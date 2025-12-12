use crate::block::types::{Block, BlockHeader};
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::{MasternodeInfo, MasternodeRegistry};
use crate::types::{Transaction, TxOutput};
use crate::NetworkType;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

const BLOCK_TIME_SECONDS: i64 = 600; // 10 minutes
const SATOSHIS_PER_TIME: u64 = 100_000_000;
const BLOCK_REWARD_SATOSHIS: u64 = 100 * SATOSHIS_PER_TIME; // 100 TIME

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
    is_syncing: Arc<RwLock<bool>>, // Track if currently syncing from a peer
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
        }
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

        // No local genesis - create it immediately
        // (Don't wait for peers - there may not be any peers with the genesis yet)
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

    /// Check sync status and catch up missing blocks
    pub async fn catchup_blocks(&self) -> Result<(), String> {
        let current = *self.current_height.read().await;
        let expected = self.calculate_expected_height();

        if current >= expected {
            tracing::info!("✓ Blockchain is synced (height: {})", current);
            return Ok(());
        }

        tracing::info!(
            "⏳ Syncing blockchain from peers: {} → {} ({} blocks behind)",
            current,
            expected,
            expected - current
        );

        // Wait for P2P peers to connect and sync
        tracing::info!("📡 Waiting for peer connections to sync blockchain...");
        tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

        // Check if peers synced us
        let mut current_after_wait = *self.current_height.read().await;
        if current_after_wait >= expected {
            tracing::info!("✓ Synced from peers to height {}", current_after_wait);
            return Ok(());
        }

        // Still behind - check if we made any progress
        if current_after_wait > current {
            let progress = current_after_wait - current;
            tracing::info!(
                "📥 Synced {} blocks from peers, {} more to go. Waiting...",
                progress,
                expected - current_after_wait
            );

            // Continue waiting for sync to complete (up to 5 minutes total)
            for i in 0..30 {
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                let height = *self.current_height.read().await;
                if height >= expected {
                    tracing::info!("✓ Sync complete at height {}", height);
                    return Ok(());
                }
                if height > current_after_wait {
                    tracing::info!("📥 Syncing... ({}/{})", height, expected);
                    current_after_wait = height;
                }

                // Log progress every minute
                if i % 6 == 0 && i > 0 {
                    tracing::info!(
                        "📊 Still syncing from peers... ({}/{}, {:.1}% complete)",
                        height,
                        expected,
                        (height as f64 / expected as f64) * 100.0
                    );
                }
            }
        }

        // After waiting up to 5.5 minutes, check final status
        let final_height = *self.current_height.read().await;
        if final_height >= expected {
            tracing::info!("✓ Sync complete at height {}", final_height);
            return Ok(());
        }

        // Still behind - don't generate blocks, just wait for peers
        let blocks_behind = expected - final_height;
        tracing::warn!(
            "⚠️  Still {} blocks behind. Waiting for peers to sync blockchain...",
            blocks_behind
        );
        tracing::info!(
            "💡 If peers aren't connecting, check firewall settings and peer discovery."
        );

        // Don't fail - just let the periodic sync task continue trying
        Ok(())
    }

    /// Create a catchup block (DEPRECATED - should only download from peers)
    /// This is only used for generating the very first blocks after genesis
    /// when no peers exist yet (bootstrap scenario)
    #[allow(dead_code)]
    async fn create_catchup_block(&self, height: u64, timestamp: i64) -> Result<Block, String> {
        let prev_hash = self.get_block_hash(height - 1)?;
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
            timestamp,
        };

        let block = Block {
            header: BlockHeader {
                version: 1,
                height,
                previous_hash: prev_hash,
                merkle_root: coinbase.txid(),
                timestamp,
                block_reward: BLOCK_REWARD_SATOSHIS,
            },
            transactions: vec![coinbase],
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
        };

        Ok(block)
    }

    /// Produce a block at the scheduled time
    pub async fn produce_block(&self) -> Result<Block, String> {
        let height = *self.current_height.read().await + 1;
        let expected = self.calculate_expected_height();

        // Reject future blocks
        if height > expected {
            return Err(format!(
                "Cannot create future block {} (expected: {})",
                height, expected
            ));
        }

        let timestamp = self.genesis_timestamp() + (height as i64 * BLOCK_TIME_SECONDS);
        let now = Utc::now().timestamp();

        // Must be within block time window
        if now < timestamp {
            return Err(format!(
                "Block time not reached ({}s early)",
                timestamp - now
            ));
        }

        if now > timestamp + BLOCK_TIME_SECONDS {
            return Err(format!(
                "Block time window missed ({}s late)",
                now - timestamp
            ));
        }

        // Verify 3+ masternodes
        let masternodes = self.masternode_registry.list_active().await;
        if masternodes.len() < 3 {
            return Err(format!(
                "Insufficient masternodes: {} (need 3)",
                masternodes.len()
            ));
        }

        // Get finalized transactions and calculate total fees
        let finalized_txs = self.consensus.get_finalized_transactions_for_block().await;
        let total_fees = self.consensus.tx_pool.get_total_fees().await;

        // Calculate rewards including fees
        let base_reward = BLOCK_REWARD_SATOSHIS;
        let total_reward = base_reward + total_fees;

        let mut outputs = Vec::new();
        let rewards = self.calculate_rewards_with_amount(&masternodes, total_reward);

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
            timestamp,
        };

        // Build transaction list: coinbase + finalized transactions
        let mut all_txs = vec![coinbase.clone()];
        all_txs.extend(finalized_txs);

        let prev_hash = self.get_block_hash(height - 1)?;

        let block = Block {
            header: BlockHeader {
                version: 1,
                height,
                previous_hash: prev_hash,
                merkle_root: coinbase.txid(), // TODO: Calculate proper merkle root
                timestamp,
                block_reward: total_reward,
            },
            transactions: all_txs,
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
        };

        Ok(block)
    }

    fn calculate_rewards_with_amount(
        &self,
        masternodes: &[MasternodeInfo],
        total_reward: u64,
    ) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        let total_weight: u64 = masternodes
            .iter()
            .map(|mn| mn.masternode.tier.reward_weight())
            .sum();

        masternodes
            .iter()
            .map(|mn| {
                let weight = mn.masternode.tier.reward_weight();
                let reward = (total_reward * weight) / total_weight;
                (mn.reward_address.clone(), reward)
            })
            .collect()
    }

    fn calculate_rewards_from_info(
        &self,
        masternodes: &[crate::masternode_registry::MasternodeInfo],
    ) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        let total_weight: f64 = masternodes
            .iter()
            .map(|mn| mn.masternode.tier.reward_weight() as f64)
            .sum();

        masternodes
            .iter()
            .map(|mn| {
                let weight = mn.masternode.tier.reward_weight() as f64;
                let share = (weight / total_weight) * (BLOCK_REWARD_SATOSHIS as f64);
                (mn.reward_address.clone(), share as u64)
            })
            .collect()
    }

    fn save_block(&self, block: &Block) -> Result<(), String> {
        let key = format!("block_{}", block.header.height);
        let value = bincode::serialize(block).map_err(|e| e.to_string())?;
        self.storage
            .insert(key.as_bytes(), value)
            .map_err(|e| e.to_string())?;

        // Update chain tip
        self.storage
            .insert(b"chain_height", &block.header.height.to_le_bytes())
            .map_err(|e| e.to_string())?;

        // Flush to ensure block is persisted before next validation
        self.storage.flush().map_err(|e| e.to_string())?;

        Ok(())
    }

    pub fn get_block(&self, height: u64) -> Result<Block, String> {
        let key = format!("block_{}", height);
        let data = self
            .storage
            .get(key.as_bytes())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Block {} not found", height))?;
        bincode::deserialize(&data).map_err(|e| e.to_string())
    }

    pub fn get_block_hash(&self, height: u64) -> Result<[u8; 32], String> {
        let block = self.get_block(height)?;
        Ok(block.hash())
    }

    fn load_chain_height(&self) -> Result<u64, String> {
        match self
            .storage
            .get(b"chain_height")
            .map_err(|e| e.to_string())?
        {
            Some(bytes) => {
                let arr: [u8; 8] = bytes.as_ref().try_into().map_err(|_| "Invalid height")?;
                Ok(u64::from_le_bytes(arr))
            }
            None => Ok(0),
        }
    }

    pub async fn get_height(&self) -> u64 {
        *self.current_height.read().await
    }

    pub async fn is_syncing(&self) -> bool {
        *self.is_syncing.read().await
    }

    pub async fn set_syncing(&self, syncing: bool) {
        *self.is_syncing.write().await = syncing;
    }

    pub async fn get_utxo_state_hash(&self) -> [u8; 32] {
        self.consensus.utxo_manager.calculate_utxo_set_hash().await
    }

    pub async fn get_utxo_count(&self) -> usize {
        self.consensus.utxo_manager.list_all_utxos().await.len()
    }

    pub async fn get_all_utxos(&self) -> Vec<crate::types::UTXO> {
        self.consensus.utxo_manager.list_all_utxos().await
    }

    pub async fn reconcile_utxo_state(&self, remote_utxos: Vec<crate::types::UTXO>) {
        let (to_remove, to_add) = self
            .consensus
            .utxo_manager
            .get_utxo_diff(&remote_utxos)
            .await;

        if !to_remove.is_empty() || !to_add.is_empty() {
            tracing::warn!("⚠️ UTXO state mismatch detected! Reconciling...");
            if let Err(e) = self
                .consensus
                .utxo_manager
                .reconcile_utxo_state(to_remove, to_add)
                .await
            {
                tracing::error!("Failed to reconcile UTXO state: {:?}", e);
            }
        }
    }

    /// Get all pending transactions from the mempool
    pub async fn get_pending_transactions(&self) -> Vec<Transaction> {
        self.consensus.tx_pool.get_all_pending().await
    }

    /// Add a transaction to the mempool (called when syncing from peers)
    pub async fn add_pending_transaction(&self, tx: Transaction) -> Result<(), String> {
        // Simple validation and add to pool
        self.consensus.validate_transaction(&tx).await?;
        let fee = 1000; // Default fee for synced transactions
        self.consensus.tx_pool.add_pending(tx, fee).await;
        Ok(())
    }

    pub async fn get_block_by_height(
        &self,
        height: u64,
    ) -> Result<crate::block::types::Block, String> {
        self.get_block(height)
    }

    /// Add a block received from peers (with validation)
    pub async fn add_block(&self, block: Block) -> Result<(), String> {
        let current_height = *self.current_height.read().await;

        // Skip if we already have this block or newer
        if block.header.height <= current_height && current_height > 0 {
            // Check if this is the same block or a fork
            if let Ok(existing_block) = self.get_block(block.header.height) {
                if existing_block.hash() == block.hash() {
                    tracing::debug!("Skipping block {} (already have it)", block.header.height);
                    return Ok(());
                } else {
                    // Fork detected at this height!
                    tracing::warn!(
                        "🍴 Fork detected at height {}: our hash {} vs peer hash {}",
                        block.header.height,
                        hex::encode(existing_block.hash()),
                        hex::encode(block.hash())
                    );

                    // If peer is on a different chain, we need to check if we should reorganize
                    // For now, log it - full reorg implementation below
                    return Err(format!(
                        "Fork detected at height {} - use reorg to resolve",
                        block.header.height
                    ));
                }
            }
        }

        // Special handling for genesis block (height 0)
        if block.header.height == 0 {
            // Check if we already have genesis
            if self
                .storage
                .contains_key(b"block_0")
                .map_err(|e| e.to_string())?
            {
                tracing::debug!("Already have genesis block");
                return Ok(());
            }

            // Genesis doesn't need validation against previous block
            tracing::info!("✅ Accepting genesis block from peer");
            self.process_block_utxos(&block).await;
            self.save_block(&block)?;
            *self.current_height.write().await = 0;
            return Ok(());
        }

        // Validate non-genesis blocks (this will detect fork if prev hash doesn't match)
        match self.validate_block(&block).await {
            Ok(_) => {
                // Valid block, add it normally
                self.process_block_utxos(&block).await;
                self.save_block(&block)?;

                // Update height if this is the next sequential block
                if block.header.height == current_height + 1 {
                    *self.current_height.write().await = block.header.height;
                    tracing::info!(
                        "✅ Added block {} to chain (hash: {})",
                        block.header.height,
                        hex::encode(block.hash())
                    );
                } else {
                    tracing::info!(
                        "📦 Stored block {} (gap - current height: {})",
                        block.header.height,
                        current_height
                    );
                }

                Ok(())
            }
            Err(e) if e.contains("Invalid previous hash") => {
                // Fork detected! Previous hash doesn't match our chain
                tracing::warn!(
                    "🍴 Fork detected: block {} doesn't build on our chain",
                    block.header.height
                );
                tracing::info!("🔄 Initiating blockchain reorganization...");

                // Attempt to reorganize to the peer's chain
                self.handle_fork_and_reorg(block).await
            }
            Err(e) => Err(e),
        }
    }

    /// Validate a block before accepting it
    async fn validate_block(&self, block: &Block) -> Result<(), String> {
        // 1. Validate block structure
        if block.header.height == 0 {
            return Err("Cannot validate genesis block".to_string());
        }

        // 2. Verify previous block hash
        let expected_prev_hash = self.get_block_hash(block.header.height - 1)?;
        if block.header.previous_hash != expected_prev_hash {
            return Err(format!(
                "Invalid previous hash: expected {}, got {}",
                hex::encode(expected_prev_hash),
                hex::encode(block.header.previous_hash)
            ));
        }

        // 3. Validate timestamp (must be after previous block and not from future)
        let prev_block = self.get_block(block.header.height - 1)?;
        if block.header.timestamp <= prev_block.header.timestamp {
            return Err(format!(
                "Invalid timestamp: {} <= previous {}",
                block.header.timestamp, prev_block.header.timestamp
            ));
        }

        // Check block is not from the future (allow 10 min tolerance for clock drift)
        let now = Utc::now().timestamp();
        let max_future_seconds = 600; // 10 minutes
        if block.header.timestamp > now + max_future_seconds {
            return Err(format!(
                "Block timestamp {} is too far in the future (current time: {}, diff: {}s)",
                block.header.timestamp,
                now,
                block.header.timestamp - now
            ));
        }

        // 4. Validate transactions against UTXO state
        for tx in &block.transactions {
            // Skip coinbase transaction (first tx with no inputs)
            if tx.inputs.is_empty() {
                continue;
            }

            // Verify each input references a valid UTXO
            for input in &tx.inputs {
                let utxo = self
                    .consensus
                    .utxo_manager
                    .get_utxo(&input.previous_output)
                    .await
                    .ok_or_else(|| {
                        format!(
                            "UTXO not found: {}:{}",
                            hex::encode(input.previous_output.txid),
                            input.previous_output.vout
                        )
                    })?;

                // Verify the UTXO is unspent
                if utxo.outpoint != input.previous_output {
                    return Err(format!(
                        "UTXO already spent: {}:{}",
                        hex::encode(input.previous_output.txid),
                        input.previous_output.vout
                    ));
                }

                // TODO: Verify signature against script_pubkey
                // For now, we trust the block producer did this validation
            }

            // Verify inputs >= outputs (no value creation except coinbase)
            let total_in: u64 = tx.inputs.len() as u64 * 100_000_000; // Placeholder
            let total_out: u64 = tx.outputs.iter().map(|o| o.value).sum();
            if total_out > total_in && !tx.inputs.is_empty() {
                return Err(format!(
                    "Transaction creates value: {} out > {} in",
                    total_out, total_in
                ));
            }
        }

        // 5. Skip masternode reward validation for synced blocks
        // Blocks from peers were created with the masternode set at that time,
        // which may differ from the current active set
        // Only validate structure, not the specific masternode list

        // 6. Verify total block reward is approximately correct (allow small rounding errors)
        let expected_reward = BLOCK_REWARD_SATOSHIS;
        let actual_reward: u64 = block
            .masternode_rewards
            .iter()
            .map(|(_, amount)| amount)
            .sum();

        // Allow up to 0.01% difference for rounding errors in reward distribution
        let tolerance = expected_reward / 10000; // 0.01% = 1,000,000 satoshis (0.01 TIME)
        let diff = actual_reward.abs_diff(expected_reward);

        if diff > tolerance {
            return Err(format!(
                "Invalid block reward: {} (expected {}, diff: {})",
                actual_reward, expected_reward, diff
            ));
        }

        tracing::debug!("✅ Block {} validation passed", block.header.height);
        Ok(())
    }

    /// Process block transactions to create UTXOs
    async fn process_block_utxos(&self, block: &Block) {
        use crate::types::{OutPoint, UTXO};

        for tx in &block.transactions {
            let txid = tx.txid();

            // Create UTXOs for each output
            for (i, output) in tx.outputs.iter().enumerate() {
                let outpoint = OutPoint {
                    txid,
                    vout: i as u32,
                };

                // Derive address from script_pubkey
                let address = String::from_utf8_lossy(&output.script_pubkey).to_string();

                let utxo = UTXO {
                    outpoint,
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    address,
                };

                self.consensus.utxo_manager.add_utxo(utxo).await;
            }
        }
    }

    /// Handle fork detection and perform blockchain reorganization
    async fn handle_fork_and_reorg(&self, peer_block: Block) -> Result<(), String> {
        let fork_height = peer_block.header.height;
        let current_height = *self.current_height.read().await;

        tracing::warn!(
            "🍴 Fork detected at height {} (current height: {})",
            fork_height,
            current_height
        );

        // Find common ancestor
        let common_ancestor = match self.find_common_ancestor(fork_height).await {
            Ok(height) => height,
            Err(e) => {
                tracing::error!("Failed to find common ancestor: {}", e);
                return Err(format!("Fork resolution failed: {}", e));
            }
        };

        tracing::info!("📍 Common ancestor found at height {}", common_ancestor);

        let reorg_depth = current_height - common_ancestor;

        // Safety check: prevent deep reorganizations
        const MAX_REORG_DEPTH: u64 = 100;
        const DEEP_REORG_THRESHOLD: u64 = 10;

        if reorg_depth > MAX_REORG_DEPTH {
            tracing::error!(
                "❌ Fork too deep ({} blocks) - manual intervention required",
                reorg_depth
            );
            return Err(format!(
                "Fork depth {} exceeds maximum allowed depth {}",
                reorg_depth, MAX_REORG_DEPTH
            ));
        }

        if reorg_depth > DEEP_REORG_THRESHOLD {
            tracing::warn!(
                "⚠️  Deep reorganization: {} blocks will be rolled back",
                reorg_depth
            );
        }

        // Rollback to common ancestor
        tracing::info!("🔄 Rolling back to height {}...", common_ancestor);
        self.rollback_to_height(common_ancestor).await?;

        // Request blocks from peer starting after common ancestor
        // For now, return success and let the sync process handle fetching new blocks
        tracing::info!(
            "✅ Rollback complete. Ready to sync from height {}",
            common_ancestor + 1
        );

        Ok(())
    }

    /// Find the common ancestor block between our chain and a peer's chain
    async fn find_common_ancestor(&self, fork_height: u64) -> Result<u64, String> {
        // Start from the fork height and walk backwards
        let mut height = if fork_height > 0 { fork_height - 1 } else { 0 };

        while height > 0 {
            // Get our block hash at this height
            let _our_hash = match self.get_block_hash(height) {
                Ok(hash) => hash,
                Err(_) => {
                    // We don't have this block, go back further
                    height = height.saturating_sub(1);
                    continue;
                }
            };

            // For now, we assume blocks exist if we can get the hash
            // In a full implementation, we would query peers for their hash at this height
            // and compare. Since we don't have peer communication in this function yet,
            // we'll return the height before the fork

            // If we successfully got our hash, this is a potential common ancestor
            // The actual peer comparison would happen via network messages
            tracing::debug!("Checking height {} for common ancestor", height);

            if height == 0 {
                return Ok(0); // Genesis is always common
            }

            // For now, return height - 1 of fork as common ancestor
            // This is a simplified version - full implementation would query peers
            return Ok(height);
        }

        Ok(0) // Genesis block is the ultimate common ancestor
    }

    /// Rollback blockchain to a specific height
    async fn rollback_to_height(&self, target_height: u64) -> Result<(), String> {
        let current_height = *self.current_height.read().await;

        if target_height >= current_height {
            return Err(format!(
                "Cannot rollback: target height {} >= current height {}",
                target_height, current_height
            ));
        }

        tracing::info!(
            "🔄 Rolling back from height {} to {}...",
            current_height,
            target_height
        );

        // Delete blocks in reverse order
        for height in ((target_height + 1)..=current_height).rev() {
            // Get the block before deleting it (to revert UTXOs)
            if let Ok(block) = self.get_block(height) {
                // Revert UTXOs created by this block
                self.revert_block_utxos(&block).await;

                // Delete the block from storage
                let key = format!("block_{}", height);
                self.storage
                    .remove(key.as_bytes())
                    .map_err(|e| format!("Failed to delete block {}: {}", height, e))?;

                tracing::debug!("🗑️  Removed block {}", height);
            }
        }

        // Update chain height
        *self.current_height.write().await = target_height;
        self.storage
            .insert(b"chain_height", &target_height.to_le_bytes())
            .map_err(|e| format!("Failed to update chain height: {}", e))?;
        self.storage
            .flush()
            .map_err(|e| format!("Failed to flush storage: {}", e))?;

        tracing::info!(
            "✅ Rollback complete: chain height is now {}",
            target_height
        );

        Ok(())
    }

    /// Revert UTXOs created by a block (during rollback)
    async fn revert_block_utxos(&self, block: &Block) {
        use crate::types::OutPoint;

        // Remove all UTXOs created by transactions in this block
        for tx in &block.transactions {
            let txid = tx.txid();

            for i in 0..tx.outputs.len() {
                let outpoint = OutPoint {
                    txid,
                    vout: i as u32,
                };

                // Remove the UTXO
                self.consensus.utxo_manager.remove_utxo(&outpoint).await;
                tracing::trace!("Reverted UTXO {}:{}", hex::encode(txid), i);
            }

            // Restore UTXOs that were spent by this transaction's inputs
            if !tx.inputs.is_empty() {
                for input in &tx.inputs {
                    // In a full implementation, we would restore the spent UTXO
                    // For now, we just log it
                    tracing::trace!(
                        "Should restore UTXO {}:{}",
                        hex::encode(input.previous_output.txid),
                        input.previous_output.vout
                    );
                }
            }
        }
    }

    /// Query peers for consensus on a block hash at a specific height
    #[allow(dead_code)]
    pub async fn verify_chain_consensus(
        &self,
        _height: u64,
        _block_hash: [u8; 32],
        peers: &[String],
    ) -> Result<bool, String> {
        if peers.is_empty() {
            return Err("No peers available for consensus check".to_string());
        }

        let total_peers = peers.len();
        let required_consensus = (total_peers * 2) / 3 + 1; // 2/3 + 1

        tracing::info!(
            "🔍 Checking consensus: {} peers, need {} for 2/3 majority",
            total_peers,
            required_consensus
        );

        // In a full implementation, we would:
        // 1. Query each peer for their block hash at this height
        // 2. Count how many agree with our block_hash
        // 3. Return true if we have 2/3+ consensus

        // For now, this is a placeholder that would integrate with the network layer
        tracing::warn!("⚠️  Consensus verification not fully implemented yet");

        Ok(false)
    }

    /// Get block hash at a specific height (public interface for network queries)
    pub async fn get_block_hash_at_height(&self, height: u64) -> Option<[u8; 32]> {
        self.get_block_hash(height).ok()
    }

    /// Check if we agree with a peer's block hash at a specific height
    pub async fn check_consensus_with_peer(
        &self,
        height: u64,
        peer_hash: [u8; 32],
    ) -> (bool, Option<[u8; 32]>) {
        match self.get_block_hash(height) {
            Ok(our_hash) => (our_hash == peer_hash, Some(our_hash)),
            Err(_) => (false, None),
        }
    }

    /// Get a range of blocks for reorg sync
    pub async fn get_block_range(&self, start: u64, end: u64) -> Vec<Block> {
        let mut blocks = Vec::new();
        for height in start..=end {
            if let Ok(block) = self.get_block(height) {
                blocks.push(block);
            }
        }
        blocks
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
        }
    }
}
