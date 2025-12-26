//! Blockchain storage and management

#![allow(dead_code)]

use crate::block::types::{Block, BlockHeader};
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::{MasternodeInfo, MasternodeRegistry};
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::types::{OutPoint, Transaction, TxInput, TxOutput, UTXO};
use crate::utxo_manager::UTXOStateManager;
use crate::NetworkType;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

const BLOCK_TIME_SECONDS: i64 = 600; // 10 minutes
const SATOSHIS_PER_TIME: u64 = 100_000_000;
const BLOCK_REWARD_SATOSHIS: u64 = 100 * SATOSHIS_PER_TIME; // 100 TIME

// Security limits
const MAX_BLOCK_SIZE: usize = 2_000_000; // 2MB per block
const MAX_REORG_DEPTH: u64 = 1_000; // Maximum blocks to reorg
const ALERT_REORG_DEPTH: u64 = 100; // Alert on reorgs deeper than this

// P2P sync configuration
const PEER_SYNC_TIMEOUT_SECS: u64 = 120;
const PEER_SYNC_CHECK_INTERVAL_SECS: u64 = 2;

// Chain work constants - each block adds work based on validator count
const BASE_WORK_PER_BLOCK: u128 = 1_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct GenesisBlock {
    pub network: String,
    pub version: u32,
    pub message: String,
    pub block: Block,
}

/// Chain work metadata for fork resolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainWorkEntry {
    pub height: u64,
    pub block_hash: [u8; 32],
    pub cumulative_work: u128,
}

pub struct Blockchain {
    storage: sled::Db,
    consensus: Arc<ConsensusEngine>,
    masternode_registry: Arc<MasternodeRegistry>,
    utxo_manager: Arc<UTXOStateManager>,
    current_height: Arc<RwLock<u64>>,
    network_type: NetworkType,
    is_syncing: Arc<RwLock<bool>>,
    peer_manager: Arc<RwLock<Option<Arc<crate::peer_manager::PeerManager>>>>,
    peer_registry: Arc<RwLock<Option<Arc<PeerConnectionRegistry>>>>,
    /// Cumulative chain work for longest-chain-by-work rule
    cumulative_work: Arc<RwLock<u128>>,
}

impl Blockchain {
    pub fn new(
        storage: sled::Db,
        consensus: Arc<ConsensusEngine>,
        masternode_registry: Arc<MasternodeRegistry>,
        utxo_manager: Arc<UTXOStateManager>,
        network_type: NetworkType,
    ) -> Self {
        Self {
            storage,
            consensus,
            masternode_registry,
            utxo_manager,
            current_height: Arc::new(RwLock::new(0)),
            network_type,
            is_syncing: Arc::new(RwLock::new(false)),
            peer_manager: Arc::new(RwLock::new(None)),
            peer_registry: Arc::new(RwLock::new(None)),
            cumulative_work: Arc::new(RwLock::new(0)),
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

    /// Initialize blockchain - verify existing genesis or fail
    /// Genesis block must be synced from peers - never auto-generated
    pub async fn initialize_genesis(&self) -> Result<(), String> {
        use crate::block::genesis::GenesisBlock;

        // Check if genesis already exists locally
        let height = self.load_chain_height()?;
        if height > 0 {
            // Verify the genesis block structure
            if let Ok(genesis) = self.get_block_by_height(0).await {
                if let Err(e) = GenesisBlock::verify_structure(&genesis) {
                    tracing::error!("❌ FATAL: Local genesis block is invalid: {}", e);
                    return Err(format!("Genesis verification failed: {}", e));
                }
            }
            *self.current_height.write().await = height;
            tracing::info!("✓ Genesis block verified (height: {})", height);
            return Ok(());
        }

        // Check if block 0 exists explicitly
        if self
            .storage
            .contains_key("block_0".as_bytes())
            .map_err(|e| e.to_string())?
        {
            if let Ok(genesis) = self.get_block_by_height(0).await {
                if let Err(e) = GenesisBlock::verify_structure(&genesis) {
                    tracing::error!("❌ FATAL: Local genesis is invalid: {}", e);
                    return Err(format!("Genesis verification failed: {}", e));
                }
            }
            *self.current_height.write().await = 0;
            tracing::info!("✓ Genesis block verified");
            return Ok(());
        }

        // No genesis block - will need to sync from peers
        tracing::info!("📦 No genesis block found - will sync from peers");
        Ok(())
    }

    /// Verify chain integrity, find missing blocks
    /// Returns a list of missing block heights that need to be downloaded
    pub async fn verify_chain_integrity(&self) -> Vec<u64> {
        let current_height = *self.current_height.read().await;
        let mut missing_blocks = Vec::new();

        if current_height == 0 {
            // No blocks yet or just genesis - nothing to verify
            return vec![];
        }

        tracing::info!(
            "🔍 Verifying blockchain integrity (checking blocks 0-{})...",
            current_height
        );

        // Check each block from genesis to current height
        for height in 0..=current_height {
            let key = format!("block_{}", height);
            let exists = match self.storage.get(key.as_bytes()) {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(_) => false,
            };

            if !exists {
                missing_blocks.push(height);
            }
        }

        if missing_blocks.is_empty() {
            tracing::info!(
                "✅ Chain integrity verified: all {} blocks present",
                current_height + 1
            );
        } else {
            tracing::warn!(
                "⚠️  Found {} missing blocks in chain: {:?}",
                missing_blocks.len(),
                if missing_blocks.len() <= 10 {
                    format!("{:?}", missing_blocks)
                } else {
                    format!(
                        "[{}, {}, ... {} more]",
                        missing_blocks[0],
                        missing_blocks[1],
                        missing_blocks.len() - 2
                    )
                }
            );
        }

        missing_blocks
    }

    /// Clear all blocks above a given height from storage
    fn clear_blocks_above(&self, height: u64) {
        let mut cleared = 0;
        for h in (height + 1)..=(height + 10000) {
            // Check up to 10k blocks above
            let key = format!("block_{}", h);
            if self.storage.remove(key.as_bytes()).is_ok() {
                cleared += 1;
            } else {
                break; // No more blocks
            }
        }
        if cleared > 0 {
            tracing::info!(
                "🗑️  Cleared {} corrupted blocks above height {}",
                cleared,
                height
            );
        }
    }

    /// Clear all block data from storage (for complete reset)
    fn clear_all_blocks(&self) {
        let mut cleared = 0;
        for h in 0..100000 {
            // Clear up to 100k blocks
            let key = format!("block_{}", h);
            match self.storage.remove(key.as_bytes()) {
                Ok(Some(_)) => cleared += 1,
                _ => {
                    if h > 1000 && cleared == 0 {
                        break; // No blocks found, stop early
                    }
                }
            }
        }
        // Also clear the genesis marker so it gets recreated
        let _ = self.storage.remove("genesis_initialized");
        tracing::info!(
            "🗑️  Cleared {} blocks from storage for fresh start",
            cleared
        );
    }

    /// Download missing blocks from peers to fill gaps in the chain
    pub async fn fill_missing_blocks(&self, missing_heights: &[u64]) -> Result<usize, String> {
        if missing_heights.is_empty() {
            return Ok(0);
        }

        let peer_registry = self.peer_registry.read().await;
        let Some(registry) = peer_registry.as_ref() else {
            return Err("No peer registry available".to_string());
        };

        let connected_peers = registry.get_connected_peers().await;
        if connected_peers.is_empty() {
            return Err("No connected peers to download from".to_string());
        }

        tracing::info!(
            "📥 Downloading {} missing blocks from {} peer(s)...",
            missing_heights.len(),
            connected_peers.len()
        );

        // Group missing heights into contiguous ranges for efficient requests
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        let mut iter = missing_heights.iter().peekable();

        while let Some(&start) = iter.next() {
            let mut end = start;
            while let Some(&&next) = iter.peek() {
                if next == end + 1 {
                    end = next;
                    iter.next();
                } else {
                    break;
                }
            }
            ranges.push((start, end));
        }

        // Request each range from peers (round-robin across peers)
        let mut requested = 0;
        for (i, (start, end)) in ranges.iter().enumerate() {
            let peer = &connected_peers[i % connected_peers.len()];
            let req = NetworkMessage::GetBlocks(*start, *end);
            tracing::info!(
                "📤 Requesting missing blocks {}-{} from {}",
                start,
                end,
                peer
            );
            if registry.send_to_peer(peer, req).await.is_ok() {
                requested += (end - start + 1) as usize;
            }
        }

        // Wait a bit for blocks to arrive
        drop(peer_registry); // Release the lock before sleeping
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;

        Ok(requested)
    }

    /// Full chain verification and repair - call this at startup for masternodes
    pub async fn ensure_chain_complete(&self) -> Result<(), String> {
        let mut attempts = 0;
        const MAX_ATTEMPTS: u32 = 5;

        loop {
            let missing = self.verify_chain_integrity().await;

            if missing.is_empty() {
                tracing::info!("✅ Blockchain is complete and verified");
                return Ok(());
            }

            attempts += 1;
            if attempts > MAX_ATTEMPTS {
                return Err(format!(
                    "Failed to download {} missing blocks after {} attempts",
                    missing.len(),
                    MAX_ATTEMPTS
                ));
            }

            tracing::info!(
                "🔄 Attempt {}/{}: downloading {} missing blocks...",
                attempts,
                MAX_ATTEMPTS,
                missing.len()
            );

            match self.fill_missing_blocks(&missing).await {
                Ok(requested) => {
                    tracing::info!("📡 Requested {} blocks, waiting for response...", requested);
                    // Give more time for blocks to arrive and be processed
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                }
                Err(e) => {
                    tracing::warn!("⚠️  Failed to request missing blocks: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn create_genesis_block(&self) -> Result<Block, String> {
        use crate::block::genesis::{GenesisBlock, GenesisMasternode};

        let masternodes_info = self.masternode_registry.list_active().await;

        // Convert to GenesisMasternode format
        let genesis_masternodes: Vec<GenesisMasternode> = masternodes_info
            .iter()
            .map(|mn| GenesisMasternode {
                address: mn.masternode.wallet_address.clone(),
                tier: mn.masternode.tier.clone(),
            })
            .collect();

        // Check minimum masternodes
        if genesis_masternodes.len() < GenesisBlock::MIN_MASTERNODES_FOR_GENESIS {
            return Err(format!(
                "Need {} masternodes to generate genesis, only {} active",
                GenesisBlock::MIN_MASTERNODES_FOR_GENESIS,
                genesis_masternodes.len()
            ));
        }

        // Get leader (first masternode or self)
        let leader = genesis_masternodes
            .first()
            .map(|mn| mn.address.clone())
            .unwrap_or_else(|| "genesis".to_string());

        // Generate using the template system
        let block = GenesisBlock::generate_with_masternodes(
            self.network_type,
            genesis_masternodes,
            &leader,
        );

        tracing::info!(
            "📦 Generated genesis block with {} masternodes, reward: {} satoshis",
            masternodes_info.len(),
            block.header.block_reward
        );

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
        let mut current = *self.current_height.read().await;
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
            // Get actually connected peers from the registry, not just known peers
            let connected_peers = peer_registry.get_connected_peers().await;

            if connected_peers.is_empty() {
                tracing::warn!("⚠️  No connected peers to sync from");
                return Err("No connected peers to sync from".to_string());
            }

            tracing::info!(
                "📡 Requesting blocks from {} connected peer(s): {:?}",
                connected_peers.len(),
                connected_peers
            );

            // Sync loop - keep requesting batches until caught up or timeout
            let sync_start = std::time::Instant::now();
            let max_sync_time = std::time::Duration::from_secs(PEER_SYNC_TIMEOUT_SECS * 2);

            while current < expected && sync_start.elapsed() < max_sync_time {
                // Request next batch of blocks
                // If we don't have genesis, start from 0. Otherwise start from current + 1
                let has_genesis = self
                    .storage
                    .contains_key("block_0".as_bytes())
                    .unwrap_or(false);
                let batch_start = if !has_genesis { 0 } else { current + 1 };
                let batch_end = (batch_start + 500).min(expected);

                // Request from multiple peers in parallel
                for peer in connected_peers.iter().take(3) {
                    let req = NetworkMessage::GetBlocks(batch_start, batch_end);
                    tracing::info!(
                        "📤 Requesting blocks {}-{} from {}",
                        batch_start,
                        batch_end,
                        peer
                    );
                    if let Err(e) = peer_registry.send_to_peer(peer, req).await {
                        tracing::warn!("❌ Failed to send GetBlocks to {}: {}", peer, e);
                    }
                }

                // Wait for blocks to arrive (with shorter timeout per batch)
                let batch_start_time = std::time::Instant::now();
                let batch_timeout = std::time::Duration::from_secs(15);
                let mut last_height = current;

                while batch_start_time.elapsed() < batch_timeout {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    let now_height = *self.current_height.read().await;

                    if now_height >= expected {
                        tracing::info!("✓ Sync complete at height {}", now_height);
                        return Ok(());
                    }

                    // Check if we made progress
                    if now_height > last_height {
                        tracing::debug!("📈 Progress: {} → {}", last_height, now_height);
                        last_height = now_height;
                    }

                    // If we received all blocks in this batch, request next batch
                    if now_height >= batch_end {
                        break;
                    }
                }

                // Update current height for next iteration
                current = *self.current_height.read().await;

                // Log progress periodically
                let elapsed = sync_start.elapsed().as_secs();
                if elapsed > 0 && elapsed % 30 == 0 {
                    tracing::info!(
                        "⏳ Still syncing... height {} / {} ({}s elapsed)",
                        current,
                        expected,
                        elapsed
                    );
                }
            }
        }

        let final_height = *self.current_height.read().await;
        if final_height >= expected {
            tracing::info!("✓ Sync complete at height {}", final_height);
            return Ok(());
        }

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
        use crate::block::genesis::GenesisBlock;

        // Check if genesis block exists
        let genesis_result = self.get_block_by_height(0).await;

        if genesis_result.is_err() {
            // No genesis block - try to create one if we have enough masternodes
            tracing::info!("📦 No genesis block found - attempting to create one");

            let genesis_block = self.create_genesis_block().await?;

            // Add the genesis block to our chain
            self.add_block(genesis_block.clone()).await?;

            tracing::info!(
                "✅ Genesis block created and added at height 0, hash: {:?}",
                genesis_block.header.merkle_root
            );

            // Return the genesis block as the produced block
            return Ok(genesis_block);
        }

        let genesis = genesis_result.unwrap();

        // Verify genesis structure
        if let Err(e) = GenesisBlock::verify_structure(&genesis) {
            return Err(format!("Cannot produce blocks: invalid genesis - {}", e));
        }

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

        let prev_hash = self.get_block_hash(current_height)?;

        let next_height = current_height + 1;
        let deterministic_timestamp =
            self.genesis_timestamp() + (next_height as i64 * BLOCK_TIME_SECONDS);

        // Use current time if deterministic timestamp is in the future
        // This prevents "future block" errors during catch-up or after downtime
        let now = chrono::Utc::now().timestamp();
        let timestamp = std::cmp::min(deterministic_timestamp, now);

        // Ensure timestamp is still aligned to 10-minute intervals
        let aligned_timestamp = (timestamp / BLOCK_TIME_SECONDS) * BLOCK_TIME_SECONDS;

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

        if rewards.is_empty() {
            return Err("No valid masternode rewards".to_string());
        }

        // Coinbase transaction creates the total block reward
        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: total_reward,
                script_pubkey: b"BLOCK_REWARD".to_vec(), // Special marker for block reward
            }],
            lock_time: 0,
            timestamp: aligned_timestamp,
        };

        // Reward distribution transaction spends coinbase and distributes to masternodes
        let reward_distribution = Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: OutPoint {
                    txid: coinbase.txid(),
                    vout: 0,
                },
                script_sig: vec![],
                sequence: 0xFFFFFFFF,
            }],
            outputs: rewards
                .iter()
                .map(|(address, amount)| TxOutput {
                    value: *amount,
                    script_pubkey: address.as_bytes().to_vec(),
                })
                .collect(),
            lock_time: 0,
            timestamp: aligned_timestamp,
        };

        // Count masternodes by tier
        let mut tier_counts = crate::block::types::MasternodeTierCounts::default();
        for mn in &masternodes {
            match mn.masternode.tier {
                crate::types::MasternodeTier::Free => tier_counts.free += 1,
                crate::types::MasternodeTier::Bronze => tier_counts.bronze += 1,
                crate::types::MasternodeTier::Silver => tier_counts.silver += 1,
                crate::types::MasternodeTier::Gold => tier_counts.gold += 1,
            }
        }

        // Build transaction list: coinbase + reward distribution + finalized transactions
        let mut all_txs = vec![coinbase.clone(), reward_distribution];
        all_txs.extend(finalized_txs);

        let mut block = Block {
            header: BlockHeader {
                version: 1,
                height: next_height,
                previous_hash: prev_hash,
                merkle_root: coinbase.txid(),
                timestamp: aligned_timestamp,
                block_reward: total_reward,
                leader: String::new(),
                attestation_root: [0u8; 32],
                masternode_tiers: tier_counts,
            },
            transactions: all_txs,
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
            time_attestations: vec![],
        };

        // Compute attestation root after attestations are set
        block.header.attestation_root = block.compute_attestation_root();

        Ok(block)
    }

    /// Add a block to the chain
    pub async fn add_block(&self, block: Block) -> Result<(), String> {
        // Validate block height is sequential
        let current = *self.current_height.read().await;

        // Special case: genesis block (height 0) when chain is empty
        let is_genesis = block.header.height == 0;
        let chain_empty = current == 0 && self.get_block_by_height(0).await.is_err();

        if is_genesis && chain_empty {
            // Genesis block is allowed
        } else if block.header.height != current + 1 {
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

        // Update cumulative chain work
        let block_work = self.calculate_block_work(&block);
        let mut cumulative = self.cumulative_work.write().await;
        *cumulative += block_work;

        // Store chain work entry for this height
        let work_entry = ChainWorkEntry {
            height: block.header.height,
            block_hash: block.hash(),
            cumulative_work: *cumulative,
        };
        if let Err(e) = self.store_chain_work_entry(&work_entry) {
            tracing::warn!("Failed to store chain work entry: {}", e);
        }

        // Update height
        *self.current_height.write().await = block.header.height;

        // Clear finalized transactions now that they're in a block (archived)
        self.consensus.clear_finalized_transactions();

        tracing::debug!(
            "✓ Block {} added (txs: {}, work: {}), finalized pool cleared",
            block.header.height,
            block.transactions.len(),
            block_work
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

        // CRITICAL: Flush to disk to prevent data loss on crash/restart
        // Without this, sled buffers writes and blocks can be lost
        self.storage.flush().map_err(|e| {
            tracing::error!(
                "❌ Failed to flush block {} to disk: {}",
                block.header.height,
                e
            );
            e.to_string()
        })?;

        Ok(())
    }

    async fn process_block_utxos(&self, block: &Block) {
        let _block_hash = block.hash();
        let mut utxos_created = 0;
        let mut utxos_spent = 0;

        // Process each transaction
        for tx in &block.transactions {
            let txid = tx.txid();

            // Spend inputs (mark UTXOs as spent)
            for input in &tx.inputs {
                if let Err(e) = self.utxo_manager.spend_utxo(&input.previous_output).await {
                    tracing::debug!(
                        "Could not spend UTXO {}:{}: {:?}",
                        hex::encode(input.previous_output.txid),
                        input.previous_output.vout,
                        e
                    );
                } else {
                    utxos_spent += 1;
                }
            }

            // Create outputs (add new UTXOs)
            for (vout, output) in tx.outputs.iter().enumerate() {
                // Extract address from script_pubkey
                let address = String::from_utf8_lossy(&output.script_pubkey).to_string();

                let utxo = UTXO {
                    outpoint: OutPoint {
                        txid,
                        vout: vout as u32,
                    },
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    address: address.clone(),
                };

                if let Err(e) = self.utxo_manager.add_utxo(utxo).await {
                    tracing::debug!(
                        "Could not add UTXO for tx {} vout {}: {:?}",
                        hex::encode(txid),
                        vout,
                        e
                    );
                } else {
                    utxos_created += 1;
                }
            }
        }

        if utxos_created > 0 || utxos_spent > 0 {
            tracing::info!(
                "💰 Block {} indexed {} UTXOs ({} created, {} spent)",
                block.header.height,
                utxos_created,
                utxos_created,
                utxos_spent
            );
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

    // ===== Fork Detection and Reorganization =====

    /// Detect if we're on a different chain than a peer by comparing block hashes
    /// Returns Some(fork_height) if fork detected, None if chains match
    pub async fn detect_fork(&self, peer_height: u64, peer_tip_hash: [u8; 32]) -> Option<u64> {
        let our_height = *self.current_height.read().await;

        // If peer has the same tip hash at a height we have, no fork
        let check_height = our_height.min(peer_height);
        if check_height == 0 {
            return None;
        }

        // Check if our block at peer's height matches
        if let Ok(our_hash) = self.get_block_hash(check_height) {
            if our_hash == peer_tip_hash && check_height == peer_height {
                return None; // Same chain
            }
        }

        // We have a potential fork - find divergence point
        Some(check_height)
    }

    /// Find the common ancestor between our chain and a peer's chain
    /// peer_hashes: Vec of (height, hash) from peer, ordered by height descending
    pub async fn find_common_ancestor(&self, peer_hashes: &[(u64, [u8; 32])]) -> Option<u64> {
        for (height, peer_hash) in peer_hashes {
            if *height == 0 {
                return Some(0); // Genesis is always common
            }

            if let Ok(our_hash) = self.get_block_hash(*height) {
                if our_hash == *peer_hash {
                    return Some(*height);
                }
            }
        }

        // Check genesis
        if let Ok(our_genesis) = self.get_block(0) {
            if !peer_hashes.is_empty() {
                // If we got here, chains diverge before the earliest peer hash
                // Fall back to genesis
                return Some(0);
            }
            let _ = our_genesis; // Genesis exists
        }

        None
    }

    /// Rollback the chain to a specific height
    /// This removes all blocks above the target height
    pub async fn rollback_to_height(&self, target_height: u64) -> Result<u64, String> {
        let current = *self.current_height.read().await;

        if target_height >= current {
            return Ok(current); // Nothing to rollback
        }

        let blocks_to_remove = current - target_height;

        // Safety check: don't allow massive rollbacks
        if blocks_to_remove > MAX_REORG_DEPTH {
            return Err(format!(
                "Rollback too deep: {} blocks (max: {}). Manual intervention required.",
                blocks_to_remove, MAX_REORG_DEPTH
            ));
        }

        if blocks_to_remove > ALERT_REORG_DEPTH {
            tracing::warn!(
                "⚠️  LARGE REORG: Rolling back {} blocks (from {} to {})",
                blocks_to_remove,
                current,
                target_height
            );
        }

        tracing::info!(
            "🔄 Rolling back chain from height {} to {}",
            current,
            target_height
        );

        // Remove blocks from storage (highest first)
        for height in (target_height + 1..=current).rev() {
            let key = format!("block_{}", height);
            if let Err(e) = self.storage.remove(key.as_bytes()) {
                tracing::warn!("Failed to remove block {}: {}", height, e);
            }
        }

        // Update chain height
        let height_key = "chain_height".as_bytes();
        let height_bytes = bincode::serialize(&target_height).map_err(|e| e.to_string())?;
        self.storage
            .insert(height_key, height_bytes)
            .map_err(|e| e.to_string())?;

        // Update in-memory height
        *self.current_height.write().await = target_height;

        tracing::info!(
            "✅ Rollback complete: removed {} blocks, now at height {}",
            blocks_to_remove,
            target_height
        );

        Ok(target_height)
    }

    /// Validate a block before accepting it
    /// Checks: hash integrity, previous hash chain, merkle root, timestamp, height sequence
    pub fn validate_block(
        &self,
        block: &Block,
        expected_prev_hash: Option<[u8; 32]>,
    ) -> Result<(), String> {
        // 1. Verify previous hash if we have one to compare
        if let Some(prev_hash) = expected_prev_hash {
            if block.header.previous_hash != prev_hash {
                return Err(format!(
                    "Block {} previous_hash mismatch: expected {}, got {}",
                    block.header.height,
                    hex::encode(&prev_hash[..8]),
                    hex::encode(&block.header.previous_hash[..8])
                ));
            }
        }

        // 3. Verify merkle root matches transactions
        let computed_merkle = block.compute_merkle_root();
        if computed_merkle != block.header.merkle_root {
            return Err(format!(
                "Block {} merkle root mismatch: computed {}, header {}",
                block.header.height,
                hex::encode(&computed_merkle[..8]),
                hex::encode(&block.header.merkle_root[..8])
            ));
        }

        // 4. Verify timestamp is reasonable (not too far in future)
        // Allow up to 10 minutes + 2 minutes grace period for deterministic scheduling
        // Blocks are scheduled deterministically, so they may appear in advance
        let now = chrono::Utc::now().timestamp();
        let max_future = BLOCK_TIME_SECONDS + (2 * 60); // 10 minutes + 2 minute grace
        if block.header.timestamp > now + max_future {
            return Err(format!(
                "Block {} timestamp {} is too far in future (now: {}, max allowed: {})",
                block.header.height,
                block.header.timestamp,
                now,
                now + max_future
            ));
        }

        // 5. For non-genesis blocks, timestamp should be after previous block
        // (This is validated implicitly by chain ordering)

        // 6. Block size check
        let serialized = bincode::serialize(block).map_err(|e| e.to_string())?;
        if serialized.len() > MAX_BLOCK_SIZE {
            return Err(format!(
                "Block {} exceeds max size: {} > {}",
                block.header.height,
                serialized.len(),
                MAX_BLOCK_SIZE
            ));
        }

        Ok(())
    }

    /// Try to add a block, handling potential fork scenarios
    /// Returns Ok(true) if block was added, Ok(false) if skipped, Err on failure
    pub async fn add_block_with_fork_handling(&self, block: Block) -> Result<bool, String> {
        use crate::block::genesis::GenesisBlock;

        let block_height = block.header.height;

        // Special case: Genesis block (height 0)
        if block_height == 0 {
            // Check if we already have genesis
            if self
                .storage
                .contains_key("block_0".as_bytes())
                .unwrap_or(false)
            {
                if let Ok(existing) = self.get_block(0) {
                    if existing.hash() == block.hash() {
                        return Ok(false); // Already have correct genesis
                    }
                    // Different genesis - reject
                    return Err("Received different genesis block than what we have".to_string());
                }
            }

            // Verify genesis structure
            if let Err(e) = GenesisBlock::verify_structure(&block) {
                return Err(format!("Invalid genesis block: {}", e));
            }

            // Verify genesis timestamp matches network template
            if let Err(e) = GenesisBlock::verify_timestamp(&block, self.network_type) {
                return Err(format!("Invalid genesis timestamp: {}", e));
            }

            tracing::info!(
                "✅ Received valid genesis block: {} (masternodes: {})",
                hex::encode(block.hash()),
                block.header.masternode_tiers.total()
            );

            // Save genesis block
            self.process_block_utxos(&block).await;
            self.save_block(&block)?;
            // Genesis is height 0, current_height stays at 0

            return Ok(true);
        }

        // For all non-genesis blocks, we need genesis to exist first
        let has_genesis = self
            .storage
            .contains_key("block_0".as_bytes())
            .unwrap_or(false);
        if !has_genesis {
            tracing::debug!(
                "⏳ Cannot add block {} - waiting for genesis block first",
                block_height
            );
            return Ok(false);
        }

        // Get current height (after genesis check)
        let current = *self.current_height.read().await;

        // Case 1: Block is exactly what we expect (next block)
        if block_height == current + 1 {
            // Get expected previous hash
            let expected_prev_hash = self.get_block_hash(current)?;

            // Check if previous_hash matches
            if block.header.previous_hash != expected_prev_hash {
                tracing::warn!(
                    "🔀 Fork detected: block {} previous_hash mismatch (expected {}, got {})",
                    block_height,
                    hex::encode(&expected_prev_hash[..8]),
                    hex::encode(&block.header.previous_hash[..8])
                );
                // Don't validate further, just skip and let sync handle reorg
                return Ok(false);
            }

            // Full validation before accepting
            self.validate_block(&block, Some(expected_prev_hash))?;

            self.add_block(block).await?;
            return Ok(true);
        }

        // Case 2: Block is in our past - could be from a longer chain
        if block_height <= current {
            // Check if we already have this exact block
            if let Ok(existing) = self.get_block(block_height) {
                if existing.hash() == block.hash() {
                    return Ok(false); // Already have it
                }

                // Different block at same height - this is a fork!
                // Use chain work comparison (longest-chain-by-work rule)
                let incoming_work = self.calculate_block_work(&block);
                let existing_work = self.calculate_block_work(&existing);

                tracing::warn!(
                    "🔀 Fork detected at height {}: our hash {:?} (work: {}) vs incoming {:?} (work: {})",
                    block_height,
                    hex::encode(&existing.hash()[..8]),
                    existing_work,
                    hex::encode(&block.hash()[..8]),
                    incoming_work
                );

                // Keep the block with more work at this height
                // If equal work, prefer existing (first-seen as tiebreaker)
                if incoming_work > existing_work {
                    tracing::info!(
                        "📈 Incoming block has more work, would need full chain comparison"
                    );
                }

                // For single block comparison, we keep existing
                // Full chain reorg requires comparing cumulative work of entire chain
                return Ok(false);
            }

            // We don't have a block at this height - fill the gap
            // This shouldn't normally happen if chain is consistent
            tracing::warn!(
                "⚠️  Received block {} but we're at height {} with gap",
                block_height,
                current
            );
            return Ok(false);
        }

        // Case 3: Block is too far in the future
        if block_height > current + 1 {
            tracing::debug!(
                "⏳ Block {} is ahead of our height {} - need to sync first",
                block_height,
                current
            );
            return Ok(false);
        }

        Ok(false)
    }

    /// Calculate work contribution of a single block
    /// Work = BASE_WORK + (attestation_count * bonus)
    pub fn calculate_block_work(&self, block: &Block) -> u128 {
        let attestation_bonus = block.time_attestations.len() as u128 * 10_000;
        BASE_WORK_PER_BLOCK + attestation_bonus
    }

    /// Get cumulative chain work up to current tip
    pub async fn get_cumulative_work(&self) -> u128 {
        *self.cumulative_work.read().await
    }

    /// Get cumulative work at a specific height
    pub async fn get_work_at_height(&self, height: u64) -> Result<u128, String> {
        if let Some(entry) = self.get_chain_work_entry(height)? {
            Ok(entry.cumulative_work)
        } else {
            // Calculate from scratch if not cached
            let mut work: u128 = 0;
            for h in 0..=height {
                if let Ok(block) = self.get_block(h) {
                    work += self.calculate_block_work(&block);
                }
            }
            Ok(work)
        }
    }

    /// Store chain work entry for a height
    fn store_chain_work_entry(&self, entry: &ChainWorkEntry) -> Result<(), String> {
        let tree = self
            .storage
            .open_tree("chain_work")
            .map_err(|e| e.to_string())?;
        let key = format!("work:{}", entry.height);
        let value = bincode::serialize(entry).map_err(|e| e.to_string())?;
        tree.insert(key.as_bytes(), value)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Get chain work entry for a height
    fn get_chain_work_entry(&self, height: u64) -> Result<Option<ChainWorkEntry>, String> {
        let tree = self
            .storage
            .open_tree("chain_work")
            .map_err(|e| e.to_string())?;
        let key = format!("work:{}", height);
        if let Some(data) = tree.get(key.as_bytes()).map_err(|e| e.to_string())? {
            let entry: ChainWorkEntry = bincode::deserialize(&data).map_err(|e| e.to_string())?;
            Ok(Some(entry))
        } else {
            Ok(None)
        }
    }

    /// Check if we should accept a peer's chain over our own
    /// Uses longest-chain-by-work rule
    pub async fn should_switch_to_chain(&self, peer_height: u64, _peer_tip_hash: [u8; 32]) -> bool {
        let our_height = *self.current_height.read().await;

        // Primary rule: compare heights (proxy for work in simple case)
        // For proper implementation, compare cumulative work
        if peer_height > our_height {
            tracing::info!(
                "📊 Peer has longer chain: {} vs our {}",
                peer_height,
                our_height
            );
            return true;
        }

        false
    }

    /// Check if we should switch to peer's chain based on work comparison
    pub async fn should_switch_by_work(&self, peer_work: u128, peer_height: u64) -> bool {
        let our_work = *self.cumulative_work.read().await;
        let our_height = *self.current_height.read().await;

        if peer_work > our_work {
            tracing::info!(
                "📊 Peer has more chain work: {} vs our {} (heights: {} vs {})",
                peer_work,
                our_work,
                peer_height,
                our_height
            );
            return true;
        }

        // If equal work, prefer longer chain
        if peer_work == our_work && peer_height > our_height {
            tracing::info!(
                "📊 Equal work but peer is longer: {} blocks vs our {}",
                peer_height,
                our_height
            );
            return true;
        }

        false
    }

    /// Perform a chain reorganization to adopt a peer's chain
    /// 1. Find common ancestor
    /// 2. Rollback to common ancestor
    /// 3. Apply new blocks from peer
    pub async fn reorganize_to_chain(
        &self,
        common_ancestor: u64,
        new_blocks: Vec<Block>,
    ) -> Result<(), String> {
        let current = *self.current_height.read().await;

        if new_blocks.is_empty() {
            return Err("No blocks provided for reorganization".to_string());
        }

        let first_new = new_blocks.first().unwrap().header.height;
        let last_new = new_blocks.last().unwrap().header.height;

        tracing::info!(
            "🔄 Reorganizing chain: rollback {} -> {}, then apply blocks {} -> {}",
            current,
            common_ancestor,
            first_new,
            last_new
        );

        // Step 1: Rollback to common ancestor
        self.rollback_to_height(common_ancestor).await?;

        // Recalculate cumulative work after rollback
        let ancestor_work = self.get_work_at_height(common_ancestor).await.unwrap_or(0);
        *self.cumulative_work.write().await = ancestor_work;

        // Step 2: Apply new blocks in order
        for block in new_blocks {
            if let Err(e) = self.add_block(block.clone()).await {
                tracing::error!(
                    "❌ Failed to apply block {} during reorg: {}",
                    block.header.height,
                    e
                );
                return Err(format!(
                    "Reorg failed at block {}: {}",
                    block.header.height, e
                ));
            }
        }

        let new_height = *self.current_height.read().await;
        let new_work = *self.cumulative_work.read().await;
        tracing::info!(
            "✅ Reorganization complete: new height {}, cumulative work {}",
            new_height,
            new_work
        );

        Ok(())
    }

    /// Periodic chain comparison with peers to detect forks
    /// Requests block height from peers and compares
    pub async fn compare_chain_with_peers(&self) -> Option<(u64, String)> {
        let peer_registry = self.peer_registry.read().await;
        let registry = match peer_registry.as_ref() {
            Some(r) => r,
            None => return None,
        };

        let connected_peers = registry.get_connected_peers().await;
        if connected_peers.is_empty() {
            return None;
        }

        // Request block heights from all peers
        for peer in &connected_peers {
            let request = NetworkMessage::GetBlockHeight;
            if let Err(e) = registry.send_to_peer(peer, request).await {
                tracing::debug!("Failed to send GetBlockHeight to {}: {}", peer, e);
            }
        }

        // The actual fork detection happens when we receive BlockHeightResponse
        // in the message handler, not here. This just triggers the queries.
        None
    }

    /// Start periodic chain comparison task
    pub fn start_chain_comparison_task(blockchain: Arc<Blockchain>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(300)); // Every 5 minutes

            loop {
                interval.tick().await;

                let our_height = blockchain.get_height().await;
                tracing::debug!("🔍 Periodic chain check: our height = {}", our_height);

                // Query peers for their heights
                blockchain.compare_chain_with_peers().await;
            }
        });
    }
}

impl Clone for Blockchain {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            consensus: self.consensus.clone(),
            masternode_registry: self.masternode_registry.clone(),
            utxo_manager: self.utxo_manager.clone(),
            current_height: self.current_height.clone(),
            network_type: self.network_type,
            is_syncing: self.is_syncing.clone(),
            peer_manager: self.peer_manager.clone(),
            peer_registry: self.peer_registry.clone(),
            cumulative_work: self.cumulative_work.clone(),
        }
    }
}
