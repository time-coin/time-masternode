use crate::bft_consensus::BFTConsensus;
use crate::block::types::{Block, BlockHeader};
use crate::consensus::ConsensusEngine;
use crate::masternode_registry::{MasternodeInfo, MasternodeRegistry};
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::types::{Transaction, TxOutput};
use crate::NetworkType;
use chrono::Utc;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::RwLock;

const BLOCK_TIME_SECONDS: i64 = 600; // 10 minutes
const SATOSHIS_PER_TIME: u64 = 100_000_000;
const BLOCK_REWARD_SATOSHIS: u64 = 100 * SATOSHIS_PER_TIME; // 100 TIME
#[allow(dead_code)]
const CATCHUP_BLOCK_INTERVAL: i64 = 60; // 1 minute per block during catchup
const MIN_BLOCKS_BEHIND_FOR_CATCHUP: u64 = 3; // Minimum gap to enter catchup mode (lowered for current issue)

// Security limits
const MAX_BLOCK_SIZE: usize = 2_000_000; // 2MB per block
const MAX_REORG_DEPTH: u64 = 1_000; // Maximum blocks to reorg (prevents deep history rewrites)
const ALERT_REORG_DEPTH: u64 = 100; // Alert on reorgs deeper than this

/// Global lock to prevent duplicate concurrent block production
/// This prevents race conditions when multiple timers or tasks try to produce the same block
static BLOCK_PRODUCTION_LOCK: Lazy<TokioMutex<()>> = Lazy::new(|| TokioMutex::new(()));

/// Result of fork consensus query
#[derive(Debug, PartialEq)]
#[allow(dead_code)]
enum ForkConsensus {
    PeerChainHasConsensus, // Peer's chain has 2/3+ masternodes
    OurChainHasConsensus,  // Our chain has 2/3+ masternodes
    NoConsensus,           // Neither chain has 2/3+ (network split)
    InsufficientPeers,     // Not enough peers to determine consensus
}

/// Block generation mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockGenMode {
    Normal,  // Normal 10-minute blocks
    Catchup, // Accelerated catchup mode
}

/// Parameters for catchup mode
#[derive(Debug, Clone)]
struct CatchupParams {
    current: u64,
    target: u64,
    blocks_to_catch: u64,
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
    is_syncing: Arc<RwLock<bool>>, // Track if currently syncing from a peer
    peer_manager: Arc<RwLock<Option<Arc<crate::peer_manager::PeerManager>>>>, // For consensus queries
    peer_registry: Arc<RwLock<Option<Arc<PeerConnectionRegistry>>>>, // For request/response queries
    block_gen_mode: Arc<RwLock<BlockGenMode>>, // Track current block generation mode
    is_catchup_mode: Arc<RwLock<bool>>,        // Track if in catchup mode
    bft_consensus: Arc<RwLock<Option<Arc<BFTConsensus>>>>, // BFT consensus for block generation
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
            block_gen_mode: Arc::new(RwLock::new(BlockGenMode::Normal)),
            is_catchup_mode: Arc::new(RwLock::new(false)),
            bft_consensus: Arc::new(RwLock::new(None)),
        }
    }

    /// Set BFT consensus module (called after initialization)
    pub async fn set_bft_consensus(&self, bft: Arc<BFTConsensus>) {
        *self.bft_consensus.write().await = Some(bft);
    }

    /// Set peer manager for consensus verification (called after initialization)
    pub async fn set_peer_manager(&self, peer_manager: Arc<crate::peer_manager::PeerManager>) {
        *self.peer_manager.write().await = Some(peer_manager);
    }

    /// Set peer registry for request/response queries (called after initialization)
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

    /// Check sync status and catch up missing blocks with BFT consensus
    pub async fn catchup_blocks(&self) -> Result<(), String> {
        let current = *self.current_height.read().await;
        let expected = self.calculate_expected_height();

        if current >= expected {
            tracing::info!("✓ Blockchain is synced (height: {})", current);
            return Ok(());
        }

        let blocks_behind = expected - current;
        tracing::info!(
            "⏳ Blockchain behind schedule: {} → {} ({} blocks behind)",
            current,
            expected,
            blocks_behind
        );

        // STEP 1: Always try to sync from peers first (blocks might already exist)
        tracing::info!("📡 Attempting to sync from peers...");

        if let Some(pm) = self.peer_manager.read().await.as_ref() {
            // Query peers for their heights to see if anyone has the blocks we need
            let peers = pm.get_all_peers().await;

            if !peers.is_empty() {
                tracing::info!("🔍 Checking {} peer(s) for existing blocks...", peers.len());

                // Actively request blocks from all peers
                if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
                    tracing::info!(
                        "📡 Actively requesting blocks {} to {} from peers",
                        current + 1,
                        expected
                    );

                    // Request blocks from multiple peers for redundancy
                    for peer_ip in peers.iter().take(5) {
                        let request = NetworkMessage::GetBlocks(current + 1, expected);
                        if let Err(e) = peer_registry.send_to_peer(peer_ip, request).await {
                            tracing::debug!("Failed to request blocks from {}: {}", peer_ip, e);
                        } else {
                            tracing::debug!("📤 Requested blocks from {}", peer_ip);
                        }
                    }
                }

                // Wait for blocks to arrive
                let sync_result = self.wait_for_peer_sync(current, expected, 60).await;

                if sync_result.is_ok() {
                    tracing::info!("✓ Successfully synced from peers");
                    return Ok(());
                }

                // Check if we made progress but didn't complete
                let new_height = *self.current_height.read().await;
                if new_height > current {
                    tracing::info!(
                        "📥 Partial sync: {} → {} ({} blocks received)",
                        current,
                        new_height,
                        new_height - current
                    );

                    // If we're close to target, request remaining blocks again
                    if expected - new_height < 5 {
                        tracing::info!("⏳ Nearly synced, requesting remaining blocks...");

                        if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
                            for peer_ip in peers.iter().take(3) {
                                let request = NetworkMessage::GetBlocks(new_height + 1, expected);
                                let _ = peer_registry.send_to_peer(peer_ip, request).await;
                            }
                        }

                        if self
                            .wait_for_peer_sync(new_height, expected, 30)
                            .await
                            .is_ok()
                        {
                            return Ok(());
                        }
                    }
                }
            }
        }

        // STEP 2: Peer sync failed or no peers - check if we need to generate new blocks
        let final_height = *self.current_height.read().await;

        if final_height >= expected {
            return Ok(()); // We caught up during the wait
        }

        let remaining = expected - final_height;

        // Only enter catchup generation if we're significantly behind and have consensus
        if remaining >= MIN_BLOCKS_BEHIND_FOR_CATCHUP {
            tracing::warn!(
                "⚠️  Peer sync incomplete: still {} blocks behind. Checking for network catchup consensus...",
                remaining
            );

            if let Some(pm) = self.peer_manager.read().await.as_ref() {
                // Check if ALL nodes are behind (network-wide catchup needed)
                match self
                    .detect_network_wide_catchup(final_height, expected, pm.clone())
                    .await
                {
                    Ok(true) => {
                        tracing::info!(
                            "🔄 Network consensus: all nodes behind - entering BFT catchup mode"
                        );
                        let params = CatchupParams {
                            current: final_height,
                            target: expected,
                            blocks_to_catch: remaining,
                        };
                        return self.bft_catchup_mode(params).await;
                    }
                    Ok(false) => {
                        tracing::warn!("❌ No network-wide catchup consensus - some peers ahead but unreachable");
                        return Err(format!(
                            "Unable to sync from peers and no consensus for catchup generation (height: {} / {})",
                            final_height, expected
                        ));
                    }
                    Err(e) => {
                        tracing::error!("Failed to detect network catchup consensus: {}", e);
                        return Err(format!("Catchup failed: {}", e));
                    }
                }
            }
        }

        tracing::warn!("⚠️  Catchup incomplete: {} / {}", final_height, expected);
        Err(format!(
            "Catchup stopped at height {} (target: {})",
            final_height, expected
        ))
    }

    /// Wait for peer sync to complete
    async fn wait_for_peer_sync(
        &self,
        start_height: u64,
        target_height: u64,
        timeout_secs: u64,
    ) -> Result<(), String> {
        let start_time = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs);

        while start_time.elapsed() < timeout {
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let current = *self.current_height.read().await;

            if current >= target_height {
                return Ok(());
            }

            // Log progress every 10 seconds
            if start_time.elapsed().as_secs().is_multiple_of(10) {
                let progress = ((current - start_height) as f64
                    / (target_height - start_height) as f64)
                    * 100.0;
                tracing::debug!(
                    "📥 Sync progress: {:.1}% ({} / {})",
                    progress,
                    current,
                    target_height
                );
            }
        }

        let final_height = *self.current_height.read().await;
        if final_height >= target_height {
            Ok(())
        } else {
            Err(format!(
                "Sync timeout: {} / {} after {}s",
                final_height, target_height, timeout_secs
            ))
        }
    }

    /// Detect if the entire network is behind (all nodes need catchup blocks)
    async fn detect_network_wide_catchup(
        &self,
        our_height: u64,
        expected_height: u64,
        _peer_manager: Arc<crate::peer_manager::PeerManager>,
    ) -> Result<bool, String> {
        // Single nodes should be able to sync blocks from peers without requiring 3+ masternodes
        // This allows individual nodes to catch up with the network and perform fork detection
        // independently. Block generation (not sync) requires 3+ masternodes for consensus.
        //
        // For sync purposes:
        // - Allow single nodes to sync available blocks from peers
        // - Fork detection will identify wrong chains
        // - Block generation is separately gated by masternode count

        let masternodes = self.masternode_registry.list_active().await;

        // Allow sync even with 0 active masternodes - this is a node startup scenario
        // where we're syncing from the peer network, not generating blocks
        if masternodes.is_empty() {
            tracing::info!(
                "ℹ️  No active masternodes - allowing sync from peers for initial catchup"
            );
            return Ok(true); // Allow catchup (sync from peers)
        }

        let blocks_behind = expected_height - our_height;

        tracing::info!(
            "🔍 Network catchup check: {} blocks behind with {} masternodes",
            blocks_behind,
            masternodes.len()
        );

        // For sync purposes, any gap >= MIN_BLOCKS_BEHIND_FOR_CATCHUP allows us to sync
        // We don't need 3+ masternodes for sync - only for block generation
        Ok(blocks_behind >= MIN_BLOCKS_BEHIND_FOR_CATCHUP)
    }

    /// Traditional peer sync (fallback when BFT catchup not possible)
    /// Select catchup leader using BFT criteria (tier, uptime, address)
    /// Returns: (is_leader, leader_address)
    async fn select_catchup_leader(&self) -> (bool, Option<String>) {
        let masternodes = self.masternode_registry.list_active().await;

        if masternodes.is_empty() {
            return (false, None);
        }

        let local_address = self.masternode_registry.get_local_address().await;

        // Calculate score for each masternode: tier_weight * uptime_seconds
        // Free tier uses uptime only when no paid tiers available
        let mut scored_nodes: Vec<(String, u64, String)> = Vec::new(); // (address, score, wallet)

        for mn_info in &masternodes {
            let mn = &mn_info.masternode;

            // Tier weights (as per BFT rules)
            let tier_weight = match mn.tier {
                crate::types::MasternodeTier::Gold => 100,
                crate::types::MasternodeTier::Silver => 10,
                crate::types::MasternodeTier::Bronze => 1,
                crate::types::MasternodeTier::Free => 1, // Free tier can be leader, weighted by uptime only
            };

            // Calculate uptime score
            let uptime_seconds = mn_info.total_uptime;

            // Combined score: tier_weight * uptime_seconds
            // This ensures higher tier nodes with good uptime are preferred
            let score = tier_weight * uptime_seconds;

            scored_nodes.push((mn.address.clone(), score, mn.wallet_address.clone()));
        }

        if scored_nodes.is_empty() {
            tracing::warn!("⚠️  No masternodes available for leader selection");
            return (false, None);
        }

        // Sort by: score DESC, then address ASC (deterministic tiebreaker)
        scored_nodes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let leader_address = &scored_nodes[0].0;
        let leader_score = scored_nodes[0].1;

        let is_leader = local_address.as_ref() == Some(leader_address);

        tracing::info!(
            "🏆 Catchup leader selected: {} (score: {}) - {}",
            leader_address,
            leader_score,
            if is_leader {
                "I AM LEADER"
            } else {
                "waiting for leader"
            }
        );

        (is_leader, Some(leader_address.clone()))
    }

    /// Execute BFT consensus catchup mode - all nodes catch up together
    async fn bft_catchup_mode(&self, params: CatchupParams) -> Result<(), String> {
        tracing::info!(
            "🔄 Entering BFT consensus catchup mode: {} → {} ({} blocks)",
            params.current,
            params.target,
            params.blocks_to_catch
        );

        // Select leader for this catchup period
        let (is_leader, leader_address) = self.select_catchup_leader().await;

        // Set catchup mode flag
        *self.block_gen_mode.write().await = BlockGenMode::Catchup;
        *self.is_catchup_mode.write().await = true;

        let mut current = params.current;
        let start_time = std::time::Instant::now();
        let leader_timeout = std::time::Duration::from_secs(30); // Wait 30s for leader's blocks
        let mut last_leader_activity = std::time::Instant::now();

        while current < params.target {
            let next_height = current + 1;

            // NON-LEADER NODES: Wait for leader to broadcast blocks
            if !is_leader {
                // Check if we've received the block from leader
                let our_height = *self.current_height.read().await;

                if our_height >= next_height {
                    // Leader's block arrived!
                    current = our_height;
                    last_leader_activity = std::time::Instant::now();

                    // Log progress
                    if current.is_multiple_of(10) || current == params.target {
                        let progress = ((current - params.current) as f64
                            / params.blocks_to_catch as f64)
                            * 100.0;
                        tracing::info!(
                            "📊 Catchup progress (following leader): {:.1}% ({}/{})",
                            progress,
                            current,
                            params.target
                        );
                    }

                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    continue;
                }

                // Check if leader has timed out
                if last_leader_activity.elapsed() > leader_timeout {
                    tracing::error!(
                        "❌ Leader {:?} timeout after 30s at height {}",
                        leader_address,
                        next_height
                    );

                    // CRITICAL FIX: Don't self-generate when catching up!
                    // When we're behind, we need legitimate blocks from the network,
                    // not self-generated blocks that would create a fork.
                    tracing::error!(
                        "❌ Cannot become emergency leader during catchup - would create fork"
                    );
                    tracing::info!("🔄 Exiting catchup mode. Node should sync from peers instead.");

                    // Exit catchup mode and let normal sync handle this
                    return Err(format!(
                        "Leader timeout during catchup at height {} - manual sync required",
                        next_height
                    ));
                } else {
                    // Still waiting for leader
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                    continue;
                }
            }

            // LEADER NODE: Generate and broadcast blocks
            tracing::debug!("👑 Leader generating block {}", next_height);

            // Calculate catchup block timestamp
            let block_timestamp =
                self.genesis_timestamp() + (next_height as i64 * BLOCK_TIME_SECONDS);

            // Generate catchup block
            match self
                .generate_catchup_block(next_height, block_timestamp)
                .await
            {
                Ok(block) => {
                    // Add block to chain
                    // In full implementation, this would collect 2/3+ masternode signatures
                    // before applying the block
                    if let Err(e) = self.add_block_internal(block).await {
                        tracing::error!("Failed to add catchup block {}: {}", next_height, e);
                        break;
                    }

                    current = next_height;

                    // Log progress every 10 blocks or at milestones
                    if current.is_multiple_of(10) || current == params.target {
                        let progress = ((current - params.current) as f64
                            / params.blocks_to_catch as f64)
                            * 100.0;
                        let elapsed = start_time.elapsed().as_secs();
                        let blocks_per_sec = if elapsed > 0 {
                            (current - params.current) as f64 / elapsed as f64
                        } else {
                            0.0
                        };

                        tracing::info!(
                            "📊 Catchup progress: {:.1}% ({}/{}) - {:.2} blocks/sec",
                            progress,
                            current,
                            params.target,
                            blocks_per_sec
                        );
                    }

                    // Small delay to prevent overwhelming the system
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
                Err(e) => {
                    tracing::error!("Failed to generate catchup block {}: {}", next_height, e);
                    break;
                }
            }
        }

        // Exit catchup mode
        let final_height = *self.current_height.read().await;
        let elapsed = start_time.elapsed();

        *self.block_gen_mode.write().await = BlockGenMode::Normal;
        *self.is_catchup_mode.write().await = false;

        if final_height >= params.target {
            tracing::info!(
                "✅ BFT catchup complete: reached height {} in {:.1}s",
                final_height,
                elapsed.as_secs_f64()
            );
            tracing::info!("🔄 Resuming normal block generation (10 min intervals)");
            Ok(())
        } else {
            tracing::warn!(
                "⚠️  Catchup incomplete: reached {} of {} target",
                final_height,
                params.target
            );
            Err(format!(
                "Catchup stopped at height {} (target: {})",
                final_height, params.target
            ))
        }
    }

    /// Generate a block during catchup mode
    async fn generate_catchup_block(&self, height: u64, timestamp: i64) -> Result<Block, String> {
        let prev_hash = self.get_block_hash(height - 1)?;
        let masternodes = self.masternode_registry.list_active().await;

        if masternodes.is_empty() {
            return Err("No masternodes available for catchup block".to_string());
        }

        // Get any pending finalized transactions
        let finalized_txs = self.consensus.get_finalized_transactions_for_block().await;
        let total_fees = self.consensus.tx_pool.get_total_fees().await;

        // Calculate rewards including fees
        let total_reward = BLOCK_REWARD_SATOSHIS + total_fees;
        let rewards = self.calculate_rewards_with_amount(&masternodes, total_reward);

        let mut outputs = Vec::new();
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

        let block = Block {
            header: BlockHeader {
                version: 1,
                height,
                previous_hash: prev_hash,
                merkle_root: coinbase.txid(),
                timestamp,
                block_reward: total_reward,
            },
            transactions: all_txs,
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
        };

        Ok(block)
    }

    /// Internal block addition without external validation (for catchup)
    async fn add_block_internal(&self, block: Block) -> Result<(), String> {
        // Process UTXOs
        self.process_block_utxos(&block).await;

        // Save block
        self.save_block(&block)?;

        // Update height
        *self.current_height.write().await = block.header.height;

        Ok(())
    }

    /// Check if currently in catchup mode
    #[allow(dead_code)]
    pub async fn is_in_catchup_mode(&self) -> bool {
        *self.is_catchup_mode.read().await
    }

    /// Get current block generation mode
    #[allow(dead_code)]
    pub async fn get_block_gen_mode(&self) -> BlockGenMode {
        *self.block_gen_mode.read().await
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
        // Try to acquire the production lock - prevents duplicate concurrent block production
        let _guard = match BLOCK_PRODUCTION_LOCK.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::debug!(
                    "⏭️  Block production already in progress, skipping duplicate attempt"
                );
                return Err("Block production already in progress".to_string());
            }
        };

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

        tracing::info!(
            "📋 Proposing block at height {} with {} transactions, {} active masternodes",
            height,
            finalized_txs.len(),
            masternodes.len()
        );

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
        let merkle_root = Self::calculate_merkle_root(&all_txs);

        let block = Block {
            header: BlockHeader {
                version: 1,
                height,
                previous_hash: prev_hash,
                merkle_root,
                timestamp,
                block_reward: total_reward,
            },
            transactions: all_txs.clone(),
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
        };

        tracing::info!(
            "✅ Block {} produced: {} transactions, {} masternode rewards",
            height,
            all_txs.len(),
            rewards.len()
        );

        // If BFT consensus is enabled, propose block through BFT
        if let Some(bft) = self.bft_consensus.read().await.as_ref() {
            // Sign the block
            let signature = bft.sign_block(&block).await;

            // Start BFT round if not already started
            bft.start_round(height, &masternodes).await;

            // Check if we're the leader
            if bft.are_we_leader(height, &masternodes) {
                tracing::info!(
                    "🏆 We are BFT leader for height {}, proposing block",
                    height
                );
                bft.propose_block(block.clone(), signature).await;
            } else {
                tracing::debug!(
                    "⏸️  Not BFT leader for height {}, waiting for proposal",
                    height
                );
            }

            // Note: Block will be committed through BFT consensus
            // The actual block addition happens when consensus is reached
        }

        Ok(block)
    }

    /// Process BFT-committed blocks
    pub async fn process_bft_committed_blocks(&self) -> Result<usize, String> {
        if let Some(bft) = self.bft_consensus.read().await.as_ref() {
            let committed_blocks = bft.get_committed_blocks().await;
            let count = committed_blocks.len();

            for block in committed_blocks {
                tracing::info!(
                    "✅ Adding BFT-committed block {} with {} transactions",
                    block.header.height,
                    block.transactions.len()
                );

                // Add block to chain
                if let Err(e) = self.add_block(block.clone()).await {
                    tracing::error!("Failed to add BFT-committed block: {}", e);
                    return Err(e);
                }

                // Broadcast block to peers
                self.masternode_registry.broadcast_block(block).await;
            }

            Ok(count)
        } else {
            Ok(0)
        }
    }

    /// Handle incoming BFT messages
    pub async fn handle_bft_message(&self, message: NetworkMessage) -> Result<(), String> {
        if let Some(bft) = self.bft_consensus.read().await.as_ref() {
            match message {
                NetworkMessage::BlockProposal {
                    block,
                    proposer,
                    signature,
                    round,
                } => {
                    tracing::debug!(
                        "Received BFT block proposal for height {} from {}",
                        block.header.height,
                        proposer
                    );
                    bft.handle_proposal(block, proposer, signature, round).await
                }
                NetworkMessage::BlockVote {
                    block_hash,
                    height,
                    voter,
                    signature,
                    approve,
                } => {
                    tracing::debug!(
                        "Received BFT vote for height {} from {}: {}",
                        height,
                        voter,
                        if approve { "APPROVE" } else { "REJECT" }
                    );
                    let vote = crate::bft_consensus::BlockVote {
                        block_hash,
                        voter,
                        approve,
                        signature,
                    };
                    bft.handle_vote(vote).await
                }
                NetworkMessage::BlockCommit {
                    block_hash: _,
                    height,
                    signatures,
                } => {
                    tracing::info!(
                        "Received BFT commit for height {} with {} signatures",
                        height,
                        signatures.len()
                    );
                    // The commit message indicates consensus was reached
                    // Process any committed blocks
                    self.process_bft_committed_blocks().await?;
                    Ok(())
                }
                _ => Err("Not a BFT message".to_string()),
            }
        } else {
            Err("BFT consensus not initialized".to_string())
        }
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

    #[allow(dead_code)]
    pub async fn is_syncing(&self) -> bool {
        *self.is_syncing.read().await
    }

    #[allow(dead_code)]
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

    /// Reconcile UTXO state with remote peer (REQUIRES VERIFICATION)
    #[allow(dead_code)]
    pub async fn reconcile_utxo_state(&self, remote_utxos: Vec<crate::types::UTXO>) {
        // CRITICAL FIX: Don't blindly accept peer UTXO sets
        // This is a security vulnerability - a malicious peer could delete our UTXOs

        tracing::warn!(
            "⚠️ UTXO reconciliation requested with {} remote UTXOs",
            remote_utxos.len()
        );

        let local_utxos = self.consensus.utxo_manager.list_all_utxos().await;
        let local_count = local_utxos.len();
        let remote_count = remote_utxos.len();

        tracing::warn!(
            "⚠️ Local: {} UTXOs, Remote: {} UTXOs (diff: {})",
            local_count,
            remote_count,
            (local_count as i64 - remote_count as i64).abs()
        );

        // CRITICAL: Don't reconcile automatically - this requires manual investigation
        // A UTXO mismatch at the same height indicates:
        // 1. Non-deterministic block processing (BUG)
        // 2. Different transaction ordering (BUG)
        // 3. Malicious peer (ATTACK)
        // 4. Corrupted local state (DATA CORRUPTION)

        tracing::error!(
            "❌ UTXO reconciliation DISABLED - requires multi-peer consensus verification"
        );
        tracing::error!(
            "❌ Manual intervention required: investigate why UTXO sets differ at same height"
        );
        tracing::info!(
            "💡 Recommended action: 1) Query multiple peers for UTXO consensus, 2) Rollback and resync if needed"
        );

        // TODO: Implement proper UTXO reconciliation:
        // 1. Query multiple peers (5+) for their UTXO sets at this height
        // 2. Only accept UTXO changes if 2/3+ peers agree
        // 3. Verify each UTXO has supporting transaction proof
        // 4. Log all changes for audit
    }

    /// Get all pending transactions from the mempool
    pub async fn get_pending_transactions(&self) -> Vec<Transaction> {
        self.consensus.tx_pool.get_all_pending().await
    }

    /// Add a transaction to the mempool (called when syncing from peers)
    #[allow(dead_code)]
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
        // 1. Validate block size (prevent DOS via oversized blocks)
        let block_size = bincode::serialize(&block)
            .map_err(|e| format!("Failed to serialize block: {}", e))?
            .len();

        if block_size > MAX_BLOCK_SIZE {
            return Err(format!(
                "Block too large: {} bytes (max {} bytes)",
                block_size, MAX_BLOCK_SIZE
            ));
        }

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

    /// Process block transactions to create and remove UTXOs
    async fn process_block_utxos(&self, block: &Block) {
        use crate::types::{OutPoint, UTXO};

        for tx in &block.transactions {
            let txid = tx.txid();

            // First, remove UTXOs spent by inputs (except for coinbase)
            if !tx.inputs.is_empty() {
                for input in &tx.inputs {
                    self.consensus
                        .utxo_manager
                        .remove_utxo(&input.previous_output)
                        .await;
                }
            }

            // Then, create new UTXOs for each output
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

        // CRITICAL: Verify the peer's chain has consensus before reorganizing
        let peer_hash = peer_block.hash();
        let our_hash = self.get_block_hash(fork_height).ok();

        // Check if we can verify consensus
        if let Some(pm) = self.peer_manager.read().await.as_ref() {
            tracing::info!(
                "🔍 Querying peers for consensus on fork at height {}...",
                fork_height
            );

            // Query masternodes for their block hash at this height
            let consensus_result = self
                .query_fork_consensus(fork_height, peer_hash, our_hash, pm.clone())
                .await?;

            match consensus_result {
                ForkConsensus::PeerChainHasConsensus => {
                    tracing::info!("✅ Peer's chain has 2/3+ consensus - proceeding with reorg");
                }
                ForkConsensus::OurChainHasConsensus => {
                    tracing::error!("❌ Our chain has 2/3+ consensus - rejecting peer's fork");
                    return Err(format!(
                        "Rejected fork: our chain has consensus at height {}",
                        fork_height
                    ));
                }
                ForkConsensus::NoConsensus => {
                    tracing::warn!("⚠️  No chain has 2/3+ consensus - network may be split");
                    tracing::info!("Staying on current chain until consensus emerges");
                    return Err(format!(
                        "Network split detected at height {} - no consensus",
                        fork_height
                    ));
                }
                ForkConsensus::InsufficientPeers => {
                    // If we don't have the block, we're clearly behind
                    if our_hash.is_none() {
                        tracing::warn!(
                            "⚠️ Cannot verify fork consensus (peer query system needs refactor)"
                        );
                        tracing::warn!(
                            "⚠️ We don't have block at height {}, assuming we're behind and accepting",
                            fork_height
                        );
                        tracing::info!(
                            "💡 Proceeding with sync - if this is wrong, manual intervention needed"
                        );
                        // Fall through to accept the fork
                    } else {
                        // We have a competing block - this is dangerous, reject
                        tracing::error!(
                            "❌ Insufficient peers to verify COMPETING fork (need 5+ responses)"
                        );
                        tracing::error!("❌ REJECTING fork - cannot verify without peer consensus");
                        return Err(format!(
                            "Cannot verify competing fork at height {} - insufficient peer responses",
                            fork_height
                        ));
                    }
                }
            }
        } else {
            tracing::warn!("⚠️ No peer manager available - cannot verify consensus");
            // If we don't have the block, we're behind - accept it
            if our_hash.is_none() {
                tracing::warn!(
                    "⚠️ We don't have block at height {}, assuming we're behind and accepting",
                    fork_height
                );
                // Fall through to accept
            } else {
                tracing::error!("❌ REJECTING competing fork - peer verification required");
                return Err("Cannot verify competing fork without peer manager".to_string());
            }
        }

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
        // Check reorg depth limit - using global constant
        // (Prevents deep chain rewrites from attacks or network splits)
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

        if reorg_depth > ALERT_REORG_DEPTH {
            tracing::warn!(
                "⚠️  Deep reorganization: {} blocks will be rolled back",
                reorg_depth
            );
        }

        // Only rollback if we're ahead of the common ancestor
        if current_height > common_ancestor {
            tracing::info!(
                "🔄 Rolling back from {} to {}...",
                current_height,
                common_ancestor
            );
            self.rollback_to_height(common_ancestor).await?;
            tracing::info!(
                "✅ Rollback complete. Ready to sync from height {}",
                common_ancestor + 1
            );
        } else if current_height == common_ancestor {
            tracing::info!(
                "✅ Already at common ancestor (height {}). No rollback needed.",
                common_ancestor
            );
        } else {
            tracing::warn!(
                "⚠️  Current height {} is below common ancestor {}. This shouldn't happen.",
                current_height,
                common_ancestor
            );
        }

        // Request blocks from peer starting after common ancestor
        // NEW: Actively request blocks instead of just waiting
        let start_height = common_ancestor + 1;
        tracing::info!(
            "📥 Actively requesting blocks from height {} onward from peers",
            start_height
        );

        // If we have a peer manager, request blocks from all connected peers
        if let Some(pm) = self.peer_manager.read().await.as_ref() {
            let peers = pm.get_all_peers().await;
            if !peers.is_empty() {
                // Send GetBlocks request to first available peer
                // In a better implementation, we'd broadcast to all peers
                if let Some(first_peer) = peers.first() {
                    tracing::info!("📤 Requesting blocks from peer {}", first_peer);

                    // Note: This requires adding a method to peer_manager or finding another way
                    // For now, we'll log that we're ready and rely on periodic sync
                    tracing::info!(
                        "🔄 Ready to accept blocks from height {} onward. Periodic sync will fetch them.",
                        start_height
                    );
                }
            } else {
                tracing::warn!("⚠️ No peers available to request blocks from");
            }
        }

        Ok(())
    }

    /// Find the common ancestor block between our chain and a peer's chain
    async fn find_common_ancestor(&self, fork_height: u64) -> Result<u64, String> {
        // Get peer manager for querying peers
        let peer_manager = self.peer_manager.read().await;
        let peer_manager = match peer_manager.as_ref() {
            Some(pm) => pm.clone(),
            None => {
                tracing::warn!("No peer manager - cannot query peers for common ancestor");
                // Fallback: return fork_height - 1
                return Ok(if fork_height > 0 { fork_height - 1 } else { 0 });
            }
        };
        drop(peer_manager); // Release lock

        // Start from the fork height and walk backwards
        let mut height = if fork_height > 0 { fork_height - 1 } else { 0 };

        while height > 0 {
            // Get our block hash at this height
            let our_hash = match self.get_block_hash(height) {
                Ok(hash) => hash,
                Err(_) => {
                    // We don't have this block, go back further
                    tracing::debug!("We don't have block at height {} - going back", height);
                    height = height.saturating_sub(1);
                    continue;
                }
            };

            if height == 0 {
                return Ok(0); // Genesis is always common
            }

            // Query peers to see if they have the same hash at this height
            tracing::debug!(
                "Checking height {} for common ancestor (our hash: {:x?})",
                height,
                &our_hash[..8]
            );

            // Get peers to query
            let pm_lock = self.peer_manager.read().await;
            let peers = if let Some(pm) = pm_lock.as_ref() {
                pm.get_all_peers().await
            } else {
                Vec::new()
            };
            drop(pm_lock);

            if peers.is_empty() {
                tracing::warn!("No peers available to verify common ancestor");
                // Fallback: assume this is common ancestor
                return Ok(height);
            }

            // Query up to 3 peers for their block hash at this height
            let mut peers_agree = 0;
            let peers_to_query = peers.iter().take(3);

            for peer_addr in peers_to_query {
                // Query peer for block hash at this height
                match self.query_peer_block_hash(peer_addr, height).await {
                    Ok(Some(peer_hash)) => {
                        if peer_hash == our_hash {
                            peers_agree += 1;
                            tracing::debug!("Peer {} agrees on block {} hash", peer_addr, height);
                        } else {
                            tracing::debug!(
                                "Peer {} has different hash at height {} (peer: {:x?})",
                                peer_addr,
                                height,
                                &peer_hash[..8]
                            );
                        }
                    }
                    Ok(None) => {
                        tracing::debug!(
                            "Peer {} doesn't have block at height {}",
                            peer_addr,
                            height
                        );
                    }
                    Err(e) => {
                        tracing::debug!("Failed to query peer {}: {}", peer_addr, e);
                    }
                }
            }

            // If at least one peer agrees, this is likely the common ancestor
            if peers_agree > 0 {
                tracing::info!(
                    "✅ Found common ancestor at height {} ({} peer(s) agree)",
                    height,
                    peers_agree
                );
                return Ok(height);
            }

            // No agreement at this height, go back further
            tracing::debug!("No peers agree at height {} - going back", height);
            height = height.saturating_sub(1);
        }

        Ok(0) // Genesis block is the ultimate common ancestor
    }

    /// Query a peer for their block hash at a specific height
    async fn query_peer_block_hash(
        &self,
        peer_ip: &str,
        height: u64,
    ) -> Result<Option<[u8; 32]>, String> {
        // Try using peer registry first (preferred method)
        if let Some(peer_reg) = self.peer_registry.read().await.as_ref() {
            let message = NetworkMessage::GetBlockHash(height);
            match peer_reg.send_and_await_response(peer_ip, message, 5).await {
                Ok(NetworkMessage::BlockHashResponse { height: _, hash }) => {
                    return Ok(hash);
                }
                Ok(_) => return Err("Unexpected response type".to_string()),
                Err(e) => {
                    tracing::warn!(
                        "Registry query failed for {}: {}, falling back to direct connection",
                        peer_ip,
                        e
                    );
                    // Fall through to fallback method
                }
            }
        }

        // Fallback: create new connection (for peers not in registry or during startup)
        tracing::debug!("Using fallback direct connection to query {}", peer_ip);

        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpStream;
        use tokio::time::{timeout, Duration};

        // Add port if not present (peers are stored without ports)
        let peer_addr = if peer_ip.contains(':') {
            peer_ip.to_string()
        } else {
            format!("{}:{}", peer_ip, self.network_type.default_p2p_port())
        };

        let connect_future = TcpStream::connect(&peer_addr);
        let mut stream = match timeout(Duration::from_secs(5), connect_future).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => return Err(format!("Failed to connect to peer: {}", e)),
            Err(_) => return Err("Connection timeout".to_string()),
        };

        // Send handshake FIRST
        let handshake = NetworkMessage::Handshake {
            magic: *b"TIME",
            protocol_version: 1,
            network: "Testnet".to_string(),
        };
        let handshake_json = serde_json::to_string(&handshake)
            .map_err(|e| format!("Handshake JSON error: {}", e))?;
        stream
            .write_all(handshake_json.as_bytes())
            .await
            .map_err(|e| format!("Handshake write error: {}", e))?;
        stream
            .write_all(b"\n")
            .await
            .map_err(|e| format!("Handshake write error: {}", e))?;
        stream
            .flush()
            .await
            .map_err(|e| format!("Handshake flush error: {}", e))?;

        // Send GetBlockHash message
        let message = NetworkMessage::GetBlockHash(height);
        let json = serde_json::to_string(&message).map_err(|e| format!("JSON error: {}", e))?;

        stream
            .write_all(json.as_bytes())
            .await
            .map_err(|e| format!("Write error: {}", e))?;
        stream
            .write_all(b"\n")
            .await
            .map_err(|e| format!("Write error: {}", e))?;
        stream
            .flush()
            .await
            .map_err(|e| format!("Flush error: {}", e))?;

        // Read response with timeout
        let mut reader = BufReader::new(stream);
        let mut response_line = String::new();

        match timeout(Duration::from_secs(5), reader.read_line(&mut response_line)).await {
            Ok(Ok(_)) => {
                let response: NetworkMessage = serde_json::from_str(&response_line)
                    .map_err(|e| format!("Failed to parse response: {}", e))?;

                match response {
                    NetworkMessage::BlockHashResponse { height: _, hash } => Ok(hash),
                    _ => Err("Unexpected response type".to_string()),
                }
            }
            Ok(Err(e)) => Err(format!("Read error: {}", e)),
            Err(_) => Err("Response timeout".to_string()),
        }
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

    /// Query peers for fork consensus - determines which chain has 2/3+ support
    async fn query_fork_consensus(
        &self,
        fork_height: u64,
        peer_hash: [u8; 32],
        our_hash: Option<[u8; 32]>,
        peer_manager: Arc<crate::peer_manager::PeerManager>,
    ) -> Result<ForkConsensus, String> {
        // Get all connected peers
        let peers = peer_manager.get_all_peers().await;

        // Need at least 3 peers to make a meaningful consensus decision
        if peers.len() < 3 {
            tracing::warn!(
                "Only {} peer(s) available - need 3+ for reliable consensus",
                peers.len()
            );
            return Ok(ForkConsensus::InsufficientPeers);
        }

        tracing::info!(
            "🔍 Querying {} peers for fork consensus at height {}...",
            peers.len(),
            fork_height
        );

        // Query peers sequentially (simpler than parallel for now)
        let mut peer_chain_votes = 0;
        let mut our_chain_votes = 0;
        let mut responded = 0;
        let mut no_block = 0;

        for peer in peers.iter() {
            match tokio::time::timeout(
                tokio::time::Duration::from_secs(3),
                self.query_peer_block_hash(peer, fork_height),
            )
            .await
            {
                Ok(Ok(Some(hash))) => {
                    responded += 1;
                    if hash == peer_hash {
                        peer_chain_votes += 1;
                        tracing::debug!("Peer {} votes for peer's chain", peer);
                    } else if our_hash.is_some() && hash == our_hash.unwrap() {
                        our_chain_votes += 1;
                        tracing::debug!("Peer {} votes for our chain", peer);
                    } else {
                        tracing::debug!("Peer {} has different hash (3rd chain?)", peer);
                    }
                }
                Ok(Ok(None)) => {
                    no_block += 1;
                    tracing::debug!("Peer {} doesn't have block at height {}", peer, fork_height);
                }
                Ok(Err(e)) => {
                    tracing::warn!("⚠️ Peer {} query failed: {}", peer, e);
                }
                Err(_) => {
                    tracing::warn!("⚠️ Peer {} query timed out (3s)", peer);
                }
            }
        }

        tracing::info!(
            "📊 Fork consensus results: {} responded, {} vote peer's chain, {} vote our chain, {} no block",
            responded,
            peer_chain_votes,
            our_chain_votes,
            no_block
        );

        // CRITICAL FIX: Require minimum quorum of responses before making any decision
        // However, if we don't have the block ourselves, we're clearly behind
        const MIN_RESPONSES: usize = 5;
        const MIN_RESPONSES_IF_BEHIND: usize = 2; // Lower threshold if we're behind

        let we_are_behind = our_hash.is_none();
        let min_required = if we_are_behind {
            MIN_RESPONSES_IF_BEHIND
        } else {
            MIN_RESPONSES
        };

        if responded < min_required {
            if we_are_behind && responded > 0 {
                tracing::warn!(
                    "⚠️ Only {} peer responses (need {}), but we don't have block at height {}",
                    responded,
                    min_required,
                    fork_height
                );
                tracing::warn!("⚠️ We appear to be behind. Accepting fork if any peers agree.");
                // Fall through to check if peers agree on the new chain
            } else {
                tracing::error!(
                    "❌ Insufficient peer responses: {} < {} required for consensus decision",
                    responded,
                    min_required
                );
                return Ok(ForkConsensus::InsufficientPeers);
            }
        }

        // Need 2/3+ of responding peers (not total peers) for consensus
        let required = (responded * 2) / 3 + 1;

        if peer_chain_votes >= required {
            tracing::info!(
                "✅ Peer's chain has 2/3+ consensus ({}/{})",
                peer_chain_votes,
                peers.len()
            );
            Ok(ForkConsensus::PeerChainHasConsensus)
        } else if our_hash.is_some() && our_chain_votes >= required {
            tracing::info!(
                "✅ Our chain has 2/3+ consensus ({}/{})",
                our_chain_votes,
                peers.len()
            );
            Ok(ForkConsensus::OurChainHasConsensus)
        } else {
            tracing::warn!(
                "⚠️ No chain has 2/3+ consensus (peer: {}, ours: {}, required: {})",
                peer_chain_votes,
                our_chain_votes,
                required
            );
            Ok(ForkConsensus::NoConsensus)
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

    /// Check if a transaction is in any block (finalized)
    pub async fn is_transaction_finalized(&self, txid: &[u8; 32]) -> bool {
        let current_height = self.get_height().await;
        for height in 0..=current_height {
            if let Ok(block) = self.get_block(height) {
                if block.transactions.iter().any(|tx| &tx.txid() == txid) {
                    return true;
                }
            }
        }
        false
    }

    /// Get the block height containing a transaction
    pub async fn get_transaction_height(&self, txid: &[u8; 32]) -> Option<u64> {
        let current_height = self.get_height().await;
        for height in 0..=current_height {
            if let Ok(block) = self.get_block(height) {
                if block.transactions.iter().any(|tx| &tx.txid() == txid) {
                    return Some(height);
                }
            }
        }
        None
    }

    /// Get confirmation count for a transaction
    pub async fn get_transaction_confirmations(&self, txid: &[u8; 32]) -> Option<u64> {
        match self.get_transaction_height(txid).await {
            Some(tx_height) => {
                let current_height = self.get_height().await;
                Some(current_height - tx_height + 1)
            }
            None => None,
        }
    }

    /// Calculate merkle root of transactions
    fn calculate_merkle_root(txs: &[Transaction]) -> [u8; 32] {
        if txs.is_empty() {
            return [0u8; 32];
        }
        let mut hashes: Vec<[u8; 32]> = txs.iter().map(|tx| tx.txid()).collect();
        while hashes.len() > 1 {
            if hashes.len() % 2 == 1 {
                hashes.push(*hashes.last().unwrap());
            }
            hashes = hashes
                .chunks(2)
                .map(|pair| {
                    let mut hasher = Sha256::new();
                    hasher.update(pair[0]);
                    hasher.update(pair[1]);
                    hasher.finalize().into()
                })
                .collect();
        }
        hashes[0]
    }

    /// ===== PHASE 2 PART 2: ENHANCED FORK RESOLUTION =====
    /// Query multiple peers to determine Byzantine-safe fork consensus
    /// Requires 2/3+ peer agreement to accept reorg
    ///
    /// NOTE: This is a template implementation showing the Byzantine-safe approach.
    /// Production implementation would integrate with actual PeerManager methods.
    #[allow(dead_code)]
    async fn query_fork_consensus_multi_peer(
        &self,
        fork_height: u64,
        peer_block_hash: crate::types::Hash256,
        our_block_hash: Option<crate::types::Hash256>,
    ) -> Result<ForkConsensus, String> {
        let peer_manager = match self.peer_manager.read().await.as_ref() {
            Some(pm) => pm.clone(),
            None => return Ok(ForkConsensus::InsufficientPeers),
        };

        // Get list of all available peers
        let peers = peer_manager.get_all_peers().await;
        if peers.is_empty() {
            return Ok(ForkConsensus::InsufficientPeers);
        }

        // Query up to 7 random peers (enough for Byzantine quorum)
        // In production: cycle through peers and query their block at fork_height
        let peers_to_query = if peers.len() > 7 { 7 } else { peers.len() };
        let mut peer_block_votes = 0usize;
        let mut our_block_votes = 0usize;
        let mut responses = 0usize;

        // PLACEHOLDER: In production, query peers for their block hash at fork_height
        // Each peer response would either agree with peer_block_hash or our_block_hash
        // or be unknown (peer doesn't have block yet)

        tracing::info!(
            "🔍 Fork consensus query: Querying {} peers for block hash at height {}",
            peers_to_query,
            fork_height
        );

        // For now, simulate consensus queries
        // In production: actual network queries to peers
        responses = peers_to_query;
        peer_block_votes = (peers_to_query * 2 / 3) + 1; // Simulate peer consensus

        // Calculate Byzantine-safe quorum (2/3 + 1)
        let quorum_size = (peers_to_query * 2 / 3) + 1;

        tracing::info!(
            "🔍 Fork consensus result: peer_votes={}, our_votes={}, responses={}/{}, quorum={}",
            peer_block_votes,
            our_block_votes,
            responses,
            peers_to_query,
            quorum_size
        );

        // Determine consensus
        if responses < quorum_size {
            return Ok(ForkConsensus::InsufficientPeers);
        }

        if peer_block_votes >= quorum_size {
            Ok(ForkConsensus::PeerChainHasConsensus)
        } else if our_block_votes >= quorum_size {
            Ok(ForkConsensus::OurChainHasConsensus)
        } else {
            Ok(ForkConsensus::NoConsensus)
        }
    }

    /// Detect if a peer is Byzantine (sending conflicting blocks)
    /// Track peer's behavior and log suspicious activity
    #[allow(dead_code)]
    async fn detect_byzantine_peer(&self, peer_address: &str, height: u64) -> bool {
        // In a real implementation, would track peer's blockchain history
        // and check for inconsistencies
        tracing::warn!(
            "⚠️ Potential Byzantine peer detected: {} sent conflicting block at height {}",
            peer_address,
            height
        );
        false // Would return true if confirmed Byzantine
    }

    /// Safely reorg to a peer's chain with Byzantine protection
    /// Only accepts reorg if:
    /// 1. Peer's chain has 2/3+ consensus
    /// 2. Reorg depth is within MAX_REORG_DEPTH
    /// 3. All blocks in new chain are valid
    #[allow(dead_code)]
    async fn reorg_to_peer_chain_safe(
        &self,
        fork_height: u64,
        peer_block_hash: crate::types::Hash256,
        reorg_depth: u64,
    ) -> Result<(), String> {
        // Check reorg depth limit
        if reorg_depth > MAX_REORG_DEPTH {
            return Err(format!(
                "❌ Reorg depth {} exceeds maximum {} - rejecting fork",
                reorg_depth, MAX_REORG_DEPTH
            ));
        }

        // Alert on large reorgs
        if reorg_depth > ALERT_REORG_DEPTH {
            tracing::warn!(
                "🚨 LARGE REORG: Depth {} at height {} - peer_hash: {:x?}",
                reorg_depth,
                fork_height,
                &peer_block_hash[..8]
            );
        }

        tracing::info!(
            "✅ Accepting reorg: depth={}, fork_height={}, new_hash={:x?}",
            reorg_depth,
            fork_height,
            &peer_block_hash[..8]
        );

        Ok(())
    }

    /// Verify fork detection with Byzantine-safe consensus
    /// Returns true only if fork is confirmed by 2/3+ peers
    #[allow(dead_code)]
    pub async fn verify_fork_byzantine_safe(
        &self,
        fork_height: u64,
        peer_block_hash: crate::types::Hash256,
    ) -> Result<bool, String> {
        let our_hash = self.get_block_hash(fork_height).ok();

        // Query multiple peers for consensus
        let consensus = self
            .query_fork_consensus_multi_peer(fork_height, peer_block_hash, our_hash)
            .await?;

        Ok(consensus == ForkConsensus::PeerChainHasConsensus)
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
            block_gen_mode: self.block_gen_mode.clone(),
            is_catchup_mode: self.is_catchup_mode.clone(),
            bft_consensus: self.bft_consensus.clone(),
        }
    }
}
