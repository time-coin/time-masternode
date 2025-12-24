use crate::block::types::{Block, BlockHeader};
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::{MasternodeInfo, MasternodeRegistry};
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::types::{Transaction, TxOutput};
use crate::NetworkType;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock;

const BLOCK_TIME_SECONDS: i64 = 600; // 10 minutes
const SATOSHIS_PER_TIME: u64 = 100_000_000;
const BLOCK_REWARD_SATOSHIS: u64 = 100 * SATOSHIS_PER_TIME; // 100 TIME

// Security limits
const MAX_BLOCK_SIZE: usize = 2_000_000; // 2MB per block
#[allow(dead_code)]
const MAX_REORG_DEPTH: u64 = 1_000; // Maximum blocks to reorg
#[allow(dead_code)]
const ALERT_REORG_DEPTH: u64 = 100; // Alert on reorgs deeper than this

// P2P sync configuration
const PEER_SYNC_TIMEOUT_SECS: u64 = 120;
const PEER_SYNC_CHECK_INTERVAL_SECS: u64 = 2;

/// Global lock to prevent duplicate concurrent block production
#[allow(dead_code)]
static BLOCK_PRODUCTION_LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();

#[allow(dead_code)]
fn get_block_production_lock() -> &'static TokioMutex<()> {
    BLOCK_PRODUCTION_LOCK.get_or_init(|| TokioMutex::new(()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct GenesisBlock {
    pub network: String,
    pub version: u32,
    pub message: String,
    pub block: Block,
}

pub struct Blockchain {
    storage: sled::Db,
    consensus: Arc<ConsensusEngine>,
    masternode_registry: Arc<MasternodeRegistry>,
    current_height: Arc<RwLock<u64>>,
    network_type: NetworkType,
    is_syncing: Arc<RwLock<bool>>,
    peer_manager: Arc<RwLock<Option<Arc<crate::peer_manager::PeerManager>>>>,
    peer_registry: Arc<RwLock<Option<Arc<PeerConnectionRegistry>>>>,
}

impl Blockchain {
    pub fn new(
        storage: sled::Db,
        consensus: Arc<ConsensusEngine>,
        masternode_registry: Arc<MasternodeRegistry>,
        network_type: NetworkType,
    ) -> Self {
        Self {
            storage,
            consensus,
            masternode_registry,
            current_height: Arc::new(RwLock::new(0)),
            network_type,
            is_syncing: Arc::new(RwLock::new(false)),
            peer_manager: Arc::new(RwLock::new(None)),
            peer_registry: Arc::new(RwLock::new(None)),
        }
    }

    /// Set peer manager for block requests
    #[allow(dead_code)]
    pub async fn set_peer_manager(&self, peer_manager: Arc<crate::peer_manager::PeerManager>) {
        *self.peer_manager.write().await = Some(peer_manager);
    }

    /// Set peer registry for P2P communication
    pub async fn set_peer_registry(&self, peer_registry: Arc<PeerConnectionRegistry>) {
        *self.peer_registry.write().await = Some(peer_registry);
    }

    fn genesis_timestamp(&self) -> i64 {
        self.network_type.genesis_timestamp()
    }

    /// Initialize blockchain - load existing genesis or create it
    pub async fn initialize_genesis(&self) -> Result<(), String> {
        // Check if genesis already exists locally
        let height = self.load_chain_height()?;
        if height > 0 {
            *self.current_height.write().await = height;
            tracing::info!("✓ Genesis block already exists (height: {})", height);
            return Ok(());
        }

        // Also check if block 0 exists explicitly
        if self
            .storage
            .contains_key("block_0".as_bytes())
            .map_err(|e| e.to_string())?
        {
            *self.current_height.write().await = 0;
            tracing::info!("✓ Genesis block already exists");
            return Ok(());
        }

        // Create genesis block
        tracing::info!("📦 Creating genesis block...");
        let genesis = crate::block::genesis::GenesisBlock::for_network(self.network_type);

        // Save genesis block
        self.process_block_utxos(&genesis).await;
        self.save_block(&genesis)?;
        *self.current_height.write().await = 0;

        tracing::info!("✅ Genesis block created (height: 0)");
        Ok(())
    }

    #[allow(dead_code)]
    async fn create_genesis_block(&self) -> Result<Block, String> {
        let masternodes = self.masternode_registry.list_active().await;
        let mut outputs = Vec::new();
        let rewards = self.calculate_rewards_from_info(&masternodes);

        for (address, amount) in &rewards {
            outputs.push(TxOutput {
                value: *amount,
                script_pubkey: address.as_bytes().to_vec(),
            });
        }

        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs,
            lock_time: 0,
            timestamp: self.genesis_timestamp(),
        };

        let block = Block {
            header: BlockHeader {
                version: 1,
                height: 0,
                previous_hash: [0u8; 32],
                merkle_root: coinbase.txid(),
                timestamp: self.genesis_timestamp(),
                block_reward: BLOCK_REWARD_SATOSHIS,
                leader: String::new(),
                vrf_output: None,
                vrf_proof: None,
            },
            transactions: vec![coinbase],
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
        };

        Ok(block)
    }

    /// Calculate expected height based on time elapsed since genesis
    pub fn calculate_expected_height(&self) -> u64 {
        let now = Utc::now().timestamp();
        let genesis_timestamp = self.genesis_timestamp();
        if now < genesis_timestamp {
            return 0;
        }

        let elapsed = now - genesis_timestamp;
        (elapsed / BLOCK_TIME_SECONDS) as u64
    }

    /// Synchronize blockchain from peers
    ///
    /// New nodes joining the network can call this to download blocks from peers.
    /// Process:
    /// 1. Check if we're behind expected height
    /// 2. Request missing blocks from connected peers
    /// 3. Wait for peers to send blocks
    /// 4. Validate each block independently
    ///
    /// NOTE: If peers don't have blocks, they'll be produced on TSDC schedule
    pub async fn sync_from_peers(&self) -> Result<(), String> {
        let current = *self.current_height.read().await;
        let expected = self.calculate_expected_height();

        if current >= expected {
            tracing::info!("✓ Blockchain synced (height: {})", current);
            return Ok(());
        }

        let behind = expected - current;
        tracing::info!(
            "⏳ Syncing from peers: {} → {} ({} blocks behind)",
            current,
            expected,
            behind
        );

        if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
            // Request blocks from multiple peers
            if let Some(peers) = self.peer_manager.read().await.as_ref() {
                let peer_list = peers.get_all_peers().await;
                for peer in peer_list.iter().take(5) {
                    let req = NetworkMessage::GetBlocks(current + 1, expected);
                    let _ = peer_registry.send_to_peer(peer, req).await;
                }
            }

            // Wait for blocks to arrive
            let start = std::time::Instant::now();
            while start.elapsed().as_secs() < PEER_SYNC_TIMEOUT_SECS {
                tokio::time::sleep(std::time::Duration::from_secs(
                    PEER_SYNC_CHECK_INTERVAL_SECS,
                ))
                .await;
                let now_height = *self.current_height.read().await;
                if now_height >= expected {
                    tracing::info!("✓ Sync complete at height {}", now_height);
                    return Ok(());
                }
            }
        }

        let final_height = *self.current_height.read().await;
        tracing::warn!(
            "⚠️  Sync timeout at height {} (target: {})",
            final_height,
            expected
        );
        Err(format!(
            "Peer sync timeout (height: {} / {})",
            final_height, expected
        ))
    }

    /// Produce a block for the current TSDC slot
    pub async fn produce_block(&self) -> Result<Block, String> {
        // Get previous block hash
        let mut current_height = *self.current_height.read().await;

        // Verify the current height block actually exists
        // If not, find the actual highest block
        while current_height > 0 {
            match self.get_block(current_height) {
                Ok(_) => break, // Found a valid block
                Err(_) => {
                    tracing::warn!(
                        "⚠️  Block {} not found in storage, checking lower height",
                        current_height
                    );
                    current_height -= 1;
                }
            }
        }

        // Update stored height if we found a gap
        let stored_height = *self.current_height.read().await;
        if current_height != stored_height {
            tracing::warn!(
                "⚠️  Correcting chain height from {} to {}",
                stored_height,
                current_height
            );
            *self.current_height.write().await = current_height;
        }

        let prev_hash = if current_height == 0 {
            [0u8; 32]
        } else {
            self.get_block_hash(current_height)?
        };

        let next_height = current_height + 1;
        let timestamp = self.genesis_timestamp() + (next_height as i64 * BLOCK_TIME_SECONDS);

        // Get active masternodes
        let masternodes = self.masternode_registry.list_active().await;
        if masternodes.is_empty() {
            return Err("No active masternodes for block production".to_string());
        }

        // Get finalized transactions from consensus layer
        let finalized_txs = self.consensus.get_finalized_transactions_for_block();
        let total_fees = self.consensus.tx_pool.get_total_fees();

        // Calculate rewards
        let base_reward = BLOCK_REWARD_SATOSHIS;
        let total_reward = base_reward + total_fees;
        let rewards = self.calculate_rewards_with_amount(&masternodes, total_reward);

        let outputs = rewards
            .iter()
            .map(|(_, amount)| TxOutput {
                value: *amount,
                script_pubkey: b"masternode_reward".to_vec(),
            })
            .collect::<Vec<_>>();

        if outputs.is_empty() {
            return Err("No valid outputs for coinbase".to_string());
        }

        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs,
            lock_time: 0,
            timestamp,
        };

        // Build transaction list: coinbase + finalized transactions
        let mut all_txs = vec![coinbase.clone()];
        all_txs.extend(finalized_txs);

        let block = Block {
            header: BlockHeader {
                version: 1,
                height: next_height,
                previous_hash: prev_hash,
                merkle_root: coinbase.txid(),
                timestamp,
                block_reward: total_reward,
                leader: String::new(),
                vrf_output: None,
                vrf_proof: None,
            },
            transactions: all_txs,
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
        };

        Ok(block)
    }

    /// Add a block to the chain
    pub async fn add_block(&self, block: Block) -> Result<(), String> {
        // Validate block height is sequential
        let current = *self.current_height.read().await;
        if block.header.height != current + 1 {
            return Err(format!(
                "Block height mismatch: expected {}, got {}",
                current + 1,
                block.header.height
            ));
        }

        // Validate block size
        let serialized = bincode::serialize(&block).map_err(|e| e.to_string())?;
        if serialized.len() > MAX_BLOCK_SIZE {
            return Err(format!("Block too large: {} bytes", serialized.len()));
        }

        // Process UTXOs
        self.process_block_utxos(&block).await;

        // Save block
        self.save_block(&block)?;

        // Update height
        *self.current_height.write().await = block.header.height;

        // Clear finalized transactions now that they're in a block (archived)
        self.consensus.clear_finalized_transactions();

        tracing::debug!(
            "✓ Block {} added (txs: {}), finalized pool cleared",
            block.header.height,
            block.transactions.len()
        );

        Ok(())
    }

    /// Get a block by height
    pub fn get_block(&self, height: u64) -> Result<Block, String> {
        let key = format!("block_{}", height);
        let value = self
            .storage
            .get(key.as_bytes())
            .map_err(|e| e.to_string())?;

        if let Some(v) = value {
            bincode::deserialize(&v).map_err(|e| e.to_string())
        } else {
            Err(format!("Block {} not found", height))
        }
    }

    /// Get block hash at a height
    pub fn get_block_hash(&self, height: u64) -> Result<[u8; 32], String> {
        let block = self.get_block(height)?;
        Ok(block.hash())
    }

    /// Get current blockchain height
    pub async fn get_height(&self) -> u64 {
        *self.current_height.read().await
    }

    /// Check if currently syncing
    #[allow(dead_code)]
    pub async fn is_syncing(&self) -> bool {
        *self.is_syncing.read().await
    }

    /// Set syncing state
    #[allow(dead_code)]
    pub async fn set_syncing(&self, syncing: bool) {
        *self.is_syncing.write().await = syncing;
    }

    /// Get pending transactions (stub for compatibility)
    pub fn get_pending_transactions(&self) -> Vec<Transaction> {
        vec![]
    }

    /// Get block by height  
    pub async fn get_block_by_height(&self, height: u64) -> Result<Block, String> {
        self.get_block(height)
    }

    /// Get UTXO state hash (stub for compatibility)
    pub async fn get_utxo_state_hash(&self) -> [u8; 32] {
        [0u8; 32]
    }

    /// Get UTXO count (stub for compatibility)
    pub async fn get_utxo_count(&self) -> usize {
        0
    }

    /// Get all UTXOs (stub for compatibility)
    pub async fn get_all_utxos(&self) -> Vec<crate::types::UTXO> {
        vec![]
    }

    /// Get block hash at height
    pub async fn get_block_hash_at_height(&self, height: u64) -> Option<[u8; 32]> {
        self.get_block_hash(height).ok()
    }

    /// Check consensus with peer
    pub async fn check_consensus_with_peer(
        &self,
        _height: u64,
        _block_hash: [u8; 32],
    ) -> (bool, Option<[u8; 32]>) {
        (true, Some([0u8; 32]))
    }

    /// Get block range
    pub async fn get_block_range(&self, start: u64, end: u64) -> Vec<Block> {
        let mut blocks = vec![];
        for height in start..=end {
            if let Ok(block) = self.get_block(height) {
                blocks.push(block);
            }
        }
        blocks
    }

    /// Check if transaction is finalized (stub for compatibility)
    pub async fn is_transaction_finalized(&self, _txid: &[u8; 32]) -> bool {
        true
    }

    /// Get transaction confirmations (stub for compatibility)
    pub async fn get_transaction_confirmations(&self, _txid: &[u8; 32]) -> Option<u64> {
        Some(0)
    }

    // ===== Internal Helper Methods =====

    fn load_chain_height(&self) -> Result<u64, String> {
        let value = self
            .storage
            .get("chain_height".as_bytes())
            .map_err(|e| e.to_string())?;

        if let Some(v) = value {
            let height: u64 = bincode::deserialize(&v).map_err(|e| e.to_string())?;
            Ok(height)
        } else {
            Ok(0)
        }
    }

    fn save_block(&self, block: &Block) -> Result<(), String> {
        let key = format!("block_{}", block.header.height);
        let serialized = bincode::serialize(block).map_err(|e| e.to_string())?;
        self.storage
            .insert(key.as_bytes(), serialized)
            .map_err(|e| e.to_string())?;

        // Update chain height
        let height_key = "chain_height".as_bytes();
        let height_bytes = bincode::serialize(&block.header.height).map_err(|e| e.to_string())?;
        self.storage
            .insert(height_key, height_bytes)
            .map_err(|e| e.to_string())?;

        Ok(())
    }

    async fn process_block_utxos(&self, block: &Block) {
        for tx in &block.transactions {
            let txid = tx.txid();
            for output in &tx.outputs {
                // Process outputs as available UTXOs
                // This is a simplified version - full implementation would track spent/unspent state
                let _utxo = crate::types::UTXO {
                    outpoint: crate::types::OutPoint { txid, vout: 0 },
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    address: String::new(),
                };
                // Add to UTXO set via consensus engine
            }
        }
    }

    fn calculate_rewards_from_info(&self, masternodes: &[MasternodeInfo]) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        let per_masternode = BLOCK_REWARD_SATOSHIS / masternodes.len() as u64;
        masternodes
            .iter()
            .map(|mn| (mn.masternode.address.clone(), per_masternode))
            .collect()
    }

    fn calculate_rewards_with_amount(
        &self,
        masternodes: &[MasternodeInfo],
        total_reward: u64,
    ) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        let per_masternode = total_reward / masternodes.len() as u64;
        masternodes
            .iter()
            .map(|mn| (mn.masternode.address.clone(), per_masternode))
            .collect()
    }
}

impl Clone for Blockchain {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            consensus: self.consensus.clone(),
            masternode_registry: self.masternode_registry.clone(),
            current_height: self.current_height.clone(),
            network_type: self.network_type,
            is_syncing: self.is_syncing.clone(),
            peer_manager: self.peer_manager.clone(),
            peer_registry: self.peer_registry.clone(),
        }
    }
}
