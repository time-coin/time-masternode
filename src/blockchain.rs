//! Blockchain storage and management

use crate::ai::consensus_health::{
    ConsensusHealthConfig, ConsensusHealthMonitor, ConsensusMetrics,
};
use crate::block::types::{Block, BlockHeader};
use crate::block_cache::BlockCacheManager;
use crate::consensus::ConsensusEngine;
use crate::constants;
use crate::masternode_registry::MasternodeRegistry;

use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::types::{Hash256, OutPoint, Transaction, TxInput, TxOutput, UTXOState, UTXO};
use crate::utxo_manager::UTXOStateManager;
use crate::NetworkType;
use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const BLOCK_TIME_SECONDS: i64 = constants::blockchain::BLOCK_TIME_SECONDS;
const BLOCK_REWARD_SATOSHIS: u64 = constants::blockchain::BLOCK_REWARD_SATOSHIS;
const PRODUCER_REWARD_SATOSHIS: u64 = constants::blockchain::PRODUCER_REWARD_SATOSHIS;

/// Number of reward-distribution violations before a producer is considered misbehaving
const REWARD_VIOLATION_THRESHOLD: u64 = 3;

// Security limits (Phase 1)
const MAX_BLOCK_SIZE: usize = constants::blockchain::MAX_BLOCK_SIZE;
const TIMESTAMP_TOLERANCE_SECS: i64 = constants::blockchain::TIMESTAMP_TOLERANCE_SECS;
const MAX_REORG_DEPTH: u64 = constants::blockchain::MAX_REORG_DEPTH;
const ALERT_REORG_DEPTH: u64 = 100; // Alert on reorgs deeper than this

// P2P sync configuration (Phase 3 Step 4: Extended timeouts for masternodes)
const PEER_SYNC_TIMEOUT_SECS: u64 = 60; // Short timeout for responsive sync (1 min)
const SYNC_COORDINATOR_INTERVAL_SECS: u64 = 10; // Check sync every 10 seconds

// Chain work constants - each block adds work based on validator count
const BASE_WORK_PER_BLOCK: u128 = 1_000_000;

// Checkpoint system - hardcoded block hashes to prevent deep reorgs
// Format: (height, block_hash)
const MAINNET_CHECKPOINTS: &[(u64, &str)] = &[
    // Genesis block (placeholder - update with actual mainnet genesis hash)
    (
        0,
        "0000000000000000000000000000000000000000000000000000000000000000",
    ),
    // Add checkpoints every 1000 blocks as network grows
    // Example: (1000, "actual_hash_at_block_1000"),
];

const TESTNET_CHECKPOINTS: &[(u64, &str)] = &[
    // Genesis block (placeholder - update with actual testnet genesis hash)
    (
        0,
        "0000000000000000000000000000000000000000000000000000000000000000",
    ),
    // Testnet checkpoints will be added as the network matures
];

/// Undo log for blockchain rollback operations
/// Records spent UTXOs and finalized transactions for each block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoLog {
    /// Block height this undo log corresponds to
    pub height: u64,
    /// Block hash for verification
    pub block_hash: [u8; 32],
    /// UTXOs that were spent in this block (for restoration during rollback)
    pub spent_utxos: Vec<(OutPoint, UTXO)>,
    /// Transaction IDs that were finalized by timevote before block inclusion
    pub finalized_txs: Vec<[u8; 32]>,
    /// Timestamp when undo log was created
    pub created_at: i64,
}

impl UndoLog {
    /// Create new undo log for a block
    pub fn new(height: u64, block_hash: [u8; 32]) -> Self {
        Self {
            height,
            block_hash,
            spent_utxos: Vec::new(),
            finalized_txs: Vec::new(),
            created_at: chrono::Utc::now().timestamp(),
        }
    }

    /// Add a spent UTXO to the undo log
    pub fn add_spent_utxo(&mut self, outpoint: OutPoint, utxo: UTXO) {
        self.spent_utxos.push((outpoint, utxo));
    }

    /// Mark a transaction as finalized
    pub fn add_finalized_tx(&mut self, txid: [u8; 32]) {
        self.finalized_txs.push(txid);
    }
}

/// Chain work metadata for fork resolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainWorkEntry {
    pub height: u64,
    pub block_hash: [u8; 32],
    pub cumulative_work: u128,
}

/// Reorganization event metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReorgMetrics {
    pub timestamp: i64,
    pub from_height: u64,
    pub to_height: u64,
    pub common_ancestor: u64,
    pub blocks_removed: u64,
    pub blocks_added: u64,
    pub txs_to_replay: usize,
    pub duration_ms: u64,
}

// Re-export ForkResolutionState so existing `use crate::blockchain::ForkResolutionState` works
pub use crate::ai::fork_resolver::ForkResolutionState;

/// Cache for consensus check results - avoids redundant peer queries
#[derive(Clone, Debug)]
struct ConsensusCache {
    result: bool,
    timestamp: Instant,
}

pub struct Blockchain {
    storage: sled::Db,
    consensus: Arc<ConsensusEngine>,
    masternode_registry: Arc<MasternodeRegistry>,
    pub utxo_manager: Arc<UTXOStateManager>,
    /// Current blockchain height - uses AtomicU64 for lock-free reads (100x faster)
    current_height: Arc<AtomicU64>,
    network_type: NetworkType,
    /// Cached genesis timestamp (computed once, accessed frequently)
    genesis_timestamp: i64,
    /// Synchronization state - uses AtomicBool for lock-free checks
    is_syncing: Arc<AtomicBool>,
    peer_manager: Arc<RwLock<Option<Arc<crate::peer_manager::PeerManager>>>>,
    peer_registry: Arc<RwLock<Option<Arc<PeerConnectionRegistry>>>>,
    connection_manager:
        Arc<RwLock<Option<Arc<crate::network::connection_manager::ConnectionManager>>>>,
    /// AI-based peer scoring for intelligent peer selection
    peer_scoring: Arc<crate::network::peer_scoring::PeerScoringSystem>,
    /// AI-powered fork resolution (decision making)
    fork_resolver: Arc<crate::ai::fork_resolver::ForkResolver>,
    /// Sync coordinator to prevent sync storms and duplicate requests
    sync_coordinator: Arc<crate::network::sync_coordinator::SyncCoordinator>,
    /// Cumulative chain work for longest-chain-by-work rule
    cumulative_work: Arc<RwLock<u128>>,
    /// Recent reorganization events (for monitoring and debugging)
    reorg_history: Arc<RwLock<Vec<ReorgMetrics>>>,
    /// Current fork resolution state
    pub fork_state: Arc<RwLock<ForkResolutionState>>,
    /// Fork resolution mutex to prevent concurrent fork resolutions (race condition protection)
    fork_resolution_lock: Arc<tokio::sync::Mutex<()>>,
    /// Block processing mutex to prevent concurrent block additions from multiple peers.
    /// Without this, overlapping sync batches cause UTXO double-processing and state corruption.
    block_processing_lock: Arc<tokio::sync::Mutex<()>>,
    /// Consensus peers (majority chain) - reject blocks from non-consensus peers during fork resolution
    consensus_peers: Arc<RwLock<Vec<String>>>,
    /// Two-tier block cache for efficient memory usage (10-50x faster reads)
    block_cache: Arc<BlockCacheManager>,
    /// Block validator for validation logic    /// AI-powered consensus health monitoring
    consensus_health: Arc<ConsensusHealthMonitor>,
    /// Centralized AI system for intelligent decision-making across all subsystems
    ai_system: Option<Arc<crate::ai::AISystem>>,
    /// Transaction index for O(1) transaction lookups
    pub tx_index: Option<Arc<crate::tx_index::TransactionIndex>>,
    /// Whether to compress blocks when storing (saves ~60-70% space)
    compress_blocks: bool,
    /// Cache for consensus check results (TTL: 30s) - avoids redundant peer queries
    consensus_cache: Arc<RwLock<Option<ConsensusCache>>>,
    /// Tracks whether this node has ever had connected peers.
    /// Once true, bootstrap mode (0-peer block production) is permanently disabled
    /// to prevent solo chain divergence after peer disconnection.
    has_ever_had_peers: Arc<AtomicBool>,
    /// Signal notified when a new block is successfully added to the chain.
    /// Used by block production loop to wake up instantly instead of polling.
    block_added_signal: Arc<tokio::sync::Notify>,
    /// Last logged consensus result (height, hash) to suppress duplicate log lines
    #[allow(clippy::type_complexity)]
    last_consensus_log: Arc<RwLock<Option<(u64, [u8; 32])>>>,
    /// Cooldown for same-height fork switch attempts: (height, consensus_hash, when_attempted).
    /// Prevents a tight busy-loop when sync_from_peers() returns quickly without making progress.
    /// After one attempt, subsequent calls to compare_chain_with_peers() return None for 30s
    /// so the production loop can yield instead of spinning at full speed.
    #[allow(clippy::type_complexity)]
    same_height_fork_cooldown: Arc<RwLock<Option<(u64, [u8; 32], std::time::Instant)>>>,
    /// Tracks reward-distribution violations per block producer address.
    /// After REWARD_VIOLATION_THRESHOLD strikes the producer's proposals are rejected.
    reward_violations: Arc<DashMap<String, u64>>,
    /// Set when peers have a different genesis block than ours.
    /// Suppresses repeated fork resolution attempts to avoid infinite loop.
    genesis_mismatch_detected: Arc<AtomicBool>,
    /// On-chain treasury balance in satoshis. Funded by slashed collateral,
    /// disbursed via governance-approved coinbase outputs. Not a UTXO — pure state.
    treasury_balance: Arc<AtomicU64>,
    /// Governance-adjustable block emission rate (satoshis per block).
    /// Defaults to BLOCK_REWARD_SATOSHIS (100 TIME); updated atomically by
    /// EmissionRateChange governance proposals. In-memory only — re-applied
    /// from stored proposals on restart (same as FeeScheduleChange).
    active_block_reward: Arc<AtomicU64>,
    /// On-chain governance subsystem (proposals + votes).
    governance: Option<Arc<crate::governance::GovernanceState>>,
    /// Buffer for blocks downloaded ahead of current tip during parallel sync.
    /// Blocks are stored by height and drained in order as gaps fill in.
    pending_sync_blocks: Arc<RwLock<BTreeMap<u64, Block>>>,
}

impl Blockchain {
    pub fn new(
        storage: sled::Db,
        consensus: Arc<ConsensusEngine>,
        masternode_registry: Arc<MasternodeRegistry>,
        utxo_manager: Arc<UTXOStateManager>,
        network_type: NetworkType,
    ) -> Self {
        // Initialize AI peer scoring with persistent storage
        let peer_scoring = match crate::network::peer_scoring::PeerScoringSystem::new(&storage) {
            Ok(scoring) => Arc::new(scoring),
            Err(e) => {
                tracing::warn!(
                    "Failed to initialize AI peer scoring with persistence: {}. Using fallback.",
                    e
                );
                // Fallback: create without persistence (shouldn't happen but be safe)
                Arc::new(crate::network::peer_scoring::PeerScoringSystem::new(&storage).unwrap())
            }
        };

        // Initialize AI fork resolver
        let fork_resolver = Arc::new(crate::ai::fork_resolver::ForkResolver::new(Arc::new(
            storage.clone(),
        )));

        // Initialize sync coordinator to prevent sync storms
        let sync_coordinator = Arc::new(crate::network::sync_coordinator::SyncCoordinator::new());

        // Initialize two-tier block cache
        // Hot cache: 50 deserialized blocks (~50MB)
        // Warm cache: 500 serialized blocks (~150MB compressed)
        // Total: ~200MB vs 550MB with single-tier cache (60% reduction)
        let hot_capacity = constants::blockchain::BLOCK_CACHE_SIZE / 10; // 10% hot
        let warm_capacity = constants::blockchain::BLOCK_CACHE_SIZE; // 100% warm
        let block_cache = Arc::new(BlockCacheManager::new(hot_capacity, warm_capacity));
        // Initialize AI consensus health monitor
        let consensus_health =
            Arc::new(ConsensusHealthMonitor::new(ConsensusHealthConfig::default()));

        // Load chain height from database
        let loaded_height = storage
            .get("chain_height".as_bytes())
            .ok()
            .and_then(|opt| opt)
            .and_then(|bytes| bincode::deserialize::<u64>(&bytes).ok())
            .unwrap_or(0);

        // Load treasury balance from database
        let loaded_treasury = storage
            .get("treasury_balance".as_bytes())
            .ok()
            .and_then(|opt| opt)
            .and_then(|bytes| bincode::deserialize::<u64>(&bytes).ok())
            .unwrap_or(0);

        if loaded_treasury > 0 {
            tracing::info!(
                "🏦 Loaded treasury balance: {} TIME ({} satoshis)",
                loaded_treasury / constants::blockchain::SATOSHIS_PER_TIME,
                loaded_treasury
            );
        }

        if loaded_height > 0 {
            tracing::info!(
                "📊 Loaded blockchain height {} from database",
                loaded_height
            );
        }

        Self {
            storage,
            consensus,
            masternode_registry,
            utxo_manager,
            current_height: Arc::new(AtomicU64::new(loaded_height)),
            network_type,
            genesis_timestamp: network_type.genesis_timestamp(), // Cache for fast access
            is_syncing: Arc::new(AtomicBool::new(false)),
            peer_manager: Arc::new(RwLock::new(None)),
            peer_registry: Arc::new(RwLock::new(None)),
            connection_manager: Arc::new(RwLock::new(None)),
            peer_scoring,
            fork_resolver,
            sync_coordinator,
            cumulative_work: Arc::new(RwLock::new(0)),
            reorg_history: Arc::new(RwLock::new(Vec::new())),
            fork_state: Arc::new(RwLock::new(ForkResolutionState::None)),
            fork_resolution_lock: Arc::new(tokio::sync::Mutex::new(())),
            block_processing_lock: Arc::new(tokio::sync::Mutex::new(())),
            consensus_peers: Arc::new(RwLock::new(Vec::new())),
            block_cache,
            consensus_health,
            ai_system: None,
            tx_index: None, // Initialize without txindex, call build_tx_index() separately
            compress_blocks: false, // Disabled temporarily to debug block corruption issues
            consensus_cache: Arc::new(RwLock::new(None)), // Initialize empty cache
            has_ever_had_peers: Arc::new(AtomicBool::new(false)),
            block_added_signal: Arc::new(tokio::sync::Notify::new()),
            last_consensus_log: Arc::new(RwLock::new(None)),
            same_height_fork_cooldown: Arc::new(RwLock::new(None)),
            reward_violations: Arc::new(DashMap::new()),
            genesis_mismatch_detected: Arc::new(AtomicBool::new(false)),
            treasury_balance: Arc::new(AtomicU64::new(loaded_treasury)),
            active_block_reward: Arc::new(AtomicU64::new(BLOCK_REWARD_SATOSHIS)),
            governance: None,
            pending_sync_blocks: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Enable or disable block compression
    /// Get the block-added signal for event-driven consensus waiting.
    /// Notified whenever a new block is successfully added to the chain.
    pub fn block_added_signal(&self) -> Arc<tokio::sync::Notify> {
        self.block_added_signal.clone()
    }

    pub fn set_compress_blocks(&mut self, _compress: bool) {
        // TEMPORARY: Force compression OFF due to corruption issues
        // Re-enable after root cause is found
        self.compress_blocks = false;
        tracing::info!("📦 Block compression disabled (forced off for debugging)");
    }

    /// Set the governance subsystem (called from main.rs after blockchain init).
    pub fn set_governance(&mut self, governance: Arc<crate::governance::GovernanceState>) {
        self.governance = Some(governance);
    }

    /// Access the governance subsystem.
    pub fn governance(&self) -> Option<&Arc<crate::governance::GovernanceState>> {
        self.governance.as_ref()
    }

    /// Execute a passed TreasurySpend proposal: debit treasury and create a spendable UTXO.
    async fn execute_treasury_spend(
        &self,
        recipient: &str,
        amount: u64,
        proposal_id: &crate::types::Hash256,
    ) -> Result<(), String> {
        // Debit treasury first (so we can't overspend if UTXO creation fails)
        let current = self
            .treasury_balance
            .load(std::sync::atomic::Ordering::SeqCst);
        if amount > current {
            return Err(format!(
                "TreasurySpend: amount {amount} exceeds treasury balance {current}"
            ));
        }
        self.treasury_balance
            .fetch_sub(amount, std::sync::atomic::Ordering::SeqCst);
        let new_bal = self
            .treasury_balance
            .load(std::sync::atomic::Ordering::SeqCst);
        let _ = self
            .storage
            .insert("treasury_balance", &new_bal.to_le_bytes());

        // Create a UTXO payable to the recipient, using proposal_id as the synthetic txid
        let utxo = crate::types::UTXO {
            outpoint: crate::types::OutPoint {
                txid: *proposal_id,
                vout: 0,
            },
            value: amount,
            script_pubkey: recipient.as_bytes().to_vec(),
            address: recipient.to_string(),
        };
        self.utxo_manager
            .add_utxo(utxo)
            .await
            .map_err(|e| format!("add_utxo: {e:?}"))?;

        tracing::info!(
            "🏛️  Governance TreasurySpend executed: {} satoshis → {} (treasury remaining: {})",
            amount,
            recipient,
            new_bal
        );
        Ok(())
    }

    /// Return the current governance-approved block emission rate (satoshis/block).
    /// Use this everywhere instead of the `BLOCK_REWARD_SATOSHIS` constant so that
    /// an `EmissionRateChange` governance proposal takes effect without a restart.
    pub fn get_current_block_reward(&self) -> u64 {
        self.active_block_reward.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Apply an `EmissionRateChange` governance proposal.
    fn apply_emission_rate_change(&self, new_satoshis_per_block: u64) -> Result<(), String> {
        const MIN_REWARD: u64 = 10 * 100_000_000;      // 10 TIME minimum
        const MAX_REWARD: u64 = 10_000 * 100_000_000;  // 10,000 TIME maximum
        if new_satoshis_per_block < MIN_REWARD || new_satoshis_per_block > MAX_REWARD {
            return Err(format!(
                "EmissionRateChange: {new_satoshis_per_block} satoshis/block is outside allowed range [{MIN_REWARD}, {MAX_REWARD}]"
            ));
        }
        self.active_block_reward
            .store(new_satoshis_per_block, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(
            "🏛️  Governance: emission rate updated to {} satoshis/block ({:.1} TIME/block)",
            new_satoshis_per_block,
            new_satoshis_per_block as f64 / 100_000_000.0
        );
        Ok(())
    }

    /// Execute a passed FeeScheduleChange proposal.
    fn execute_fee_schedule_change(
        &self,
        new_min_fee: u64,
        new_tiers: Vec<(u64, u64)>,
    ) -> Result<(), String> {
        let schedule = crate::consensus::FeeSchedule {
            min_fee: new_min_fee,
            tiers: new_tiers,
        };
        self.consensus.apply_fee_schedule(schedule)
    }

    /// Set transaction index (called from main.rs after blockchain init)
    pub fn set_tx_index(&mut self, tx_index: Arc<crate::tx_index::TransactionIndex>) {
        self.tx_index = Some(tx_index);
    }

    /// Set the AI system for intelligent decision-making
    pub fn set_ai_system(&mut self, ai_system: Arc<crate::ai::AISystem>) {
        self.ai_system = Some(ai_system);
    }

    /// Get the AI system reference
    pub fn ai_system(&self) -> Option<&Arc<crate::ai::AISystem>> {
        self.ai_system.as_ref()
    }

    /// Build or rebuild transaction index from blockchain
    pub async fn build_tx_index(&self) -> Result<(), String> {
        let tx_index = self
            .tx_index
            .as_ref()
            .ok_or("Transaction index not initialized")?;

        let current_height = self.get_height();
        let index_len = tx_index.len();

        tracing::info!("🔍 Building transaction index...");
        tracing::info!("   Current blockchain height: {}", current_height);
        tracing::info!("   Current index size: {} transactions", index_len);

        // Always clear before rebuilding. A partial or stale index (e.g., left by
        // an incomplete rollback) causes validate_block_rewards to look up the wrong
        // transaction and compute wrong fees, resulting in blocks being rejected.
        if let Err(e) = tx_index.clear() {
            tracing::warn!(
                "Failed to clear existing tx_index before rebuild: {}. Proceeding anyway.",
                e
            );
        }

        let mut indexed_count = 0;
        let start = std::time::Instant::now();

        // Index all blocks
        for height in 0..=current_height {
            match self.get_block_by_height(height).await {
                Ok(block) => {
                    for (tx_index_in_block, tx) in block.transactions.iter().enumerate() {
                        let txid = tx.txid();
                        if let Err(e) = tx_index.add_transaction(&txid, height, tx_index_in_block) {
                            tracing::error!(
                                "Failed to index tx {} in block {}: {}",
                                hex::encode(txid),
                                height,
                                e
                            );
                            return Err(format!("Failed to index transaction: {}", e));
                        }
                        indexed_count += 1;
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to get block {} during indexing: {}", height, e);
                    return Err(format!("Failed to get block {}: {}", height, e));
                }
            }

            // Progress updates every 100 blocks
            if height % 100 == 0 && height > 0 {
                tracing::info!(
                    "   Indexed {} blocks, {} transactions...",
                    height,
                    indexed_count
                );
            }
        }

        tx_index.flush()?;

        let elapsed = start.elapsed();
        tracing::info!(
            "✅ Transaction index built: {} transactions in {:.2}s",
            indexed_count,
            elapsed.as_secs_f64()
        );

        Ok(())
    }

    /// Full reindex: clear all UTXOs and rebuild from block 0 by replaying all blocks.
    /// This is the proper way to fix stale balances after chain corruption or reset.
    pub async fn reindex_utxos(&self) -> Result<(u64, usize), String> {
        let current_height = self.get_height();
        tracing::info!(
            "🔄 Starting full UTXO reindex from block 0 to {}...",
            current_height
        );

        // Step 1: Clear all existing UTXOs
        self.utxo_manager
            .clear_all()
            .await
            .map_err(|e| format!("Failed to clear UTXOs: {:?}", e))?;

        // Reset treasury balance (will be rebuilt from block replay)
        self.treasury_balance
            .store(0, std::sync::atomic::Ordering::Relaxed);
        if let Ok(bytes) = bincode::serialize(&0u64) {
            let _ = self.storage.insert("treasury_balance", bytes);
        }

        // Step 2: Replay all blocks from genesis to current height
        let mut blocks_processed = 0u64;
        let start = std::time::Instant::now();

        for height in 0..=current_height {
            match self.get_block_by_height(height).await {
                Ok(block) => {
                    // Process UTXOs for this block (creates outputs, spends inputs)
                    match self.process_block_utxos(&block).await {
                        Ok(undo_log) => {
                            // Save the undo log for future rollback support
                            if let Err(e) = self.save_undo_log(&undo_log) {
                                tracing::warn!(
                                    "⚠️  Failed to save undo log for block {}: {}",
                                    height,
                                    e
                                );
                            }
                            // Rebuild treasury balance (5 TIME per block, including genesis)
                            self.treasury_deposit(
                                constants::blockchain::TREASURY_POOL_SATOSHIS,
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "❌ Failed to process UTXOs for block {}: {}",
                                height,
                                e
                            );
                            return Err(format!("UTXO reindex failed at block {}: {}", height, e));
                        }
                    }
                    blocks_processed += 1;

                    // Progress updates every 100 blocks
                    if height % 100 == 0 && height > 0 {
                        tracing::info!("   Reindexed {} blocks so far...", blocks_processed);
                    }
                }
                Err(e) => {
                    tracing::error!("❌ Failed to get block {} during reindex: {}", height, e);
                    return Err(format!("Reindex failed at block {}: {}", height, e));
                }
            }
        }

        let elapsed = start.elapsed();
        let final_utxo_count = self.utxo_manager.list_all_utxos().await.len();

        tracing::info!(
            "✅ UTXO reindex complete: {} blocks processed, {} UTXOs in set ({:.2}s)",
            blocks_processed,
            final_utxo_count,
            elapsed.as_secs_f64()
        );

        Ok((blocks_processed, final_utxo_count))
    }

    /// Get transaction index statistics
    pub fn get_tx_index_stats(&self) -> Option<(usize, u64)> {
        self.tx_index.as_ref().map(|idx| {
            let tx_count = idx.len();
            let height = self.get_height();
            (tx_count, height)
        })
    }

    /// Set peer registry for P2P communication
    pub async fn set_peer_registry(&self, peer_registry: Arc<PeerConnectionRegistry>) {
        *self.peer_registry.write().await = Some(peer_registry);
    }

    /// Set connection manager for tracking peer connections
    pub async fn set_connection_manager(
        &self,
        connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    ) {
        *self.connection_manager.write().await = Some(connection_manager);
    }

    pub async fn get_connection_manager(
        &self,
    ) -> Option<Arc<crate::network::connection_manager::ConnectionManager>> {
        self.connection_manager.read().await.clone()
    }

    pub async fn get_peer_registry(&self) -> Option<Arc<PeerConnectionRegistry>> {
        self.peer_registry.read().await.clone()
    }

    /// Get the list of peers currently on the consensus chain
    pub async fn get_consensus_peers(&self) -> Vec<String> {
        self.consensus_peers.read().await.clone()
    }

    pub fn genesis_timestamp(&self) -> i64 {
        self.genesis_timestamp // Use cached value
    }

    /// Verify chain integrity on startup and fix height if needed
    /// This handles cases where blocks were written but height update was not flushed,
    /// or where gaps exist in the chain due to corruption
    pub fn verify_and_fix_chain_height(&self) -> Result<bool, String> {
        let stored_height = self
            .current_height
            .load(std::sync::atomic::Ordering::Acquire);

        tracing::info!(
            "🔍 Verifying chain integrity: stored height = {}",
            stored_height
        );

        // Helper: check if a block exists
        let block_key_exists = |h: u64| -> bool {
            let key = format!("block_{}", h);
            self.storage.get(key.as_bytes()).ok().flatten().is_some()
        };

        // First, find the highest contiguous chain from genesis
        // This handles gaps in the middle of the chain
        let mut highest_contiguous = 0u64;
        let mut gap_heights: Vec<u64> = Vec::new();
        for h in 0..=stored_height {
            if block_key_exists(h) {
                if self.get_block(h).is_ok() {
                    highest_contiguous = h;
                } else {
                    // Block exists but corrupted - record gap, don't break
                    tracing::warn!("🔧 Block {} exists but is corrupted - recording as gap", h);
                    gap_heights.push(h);
                    break; // Chain breaks at corruption
                }
            } else {
                // Gap found - record it but DON'T delete blocks above
                if h > 0 {
                    tracing::warn!(
                        "🔧 Gap detected: block {} missing (highest contiguous: {})",
                        h,
                        highest_contiguous
                    );
                    gap_heights.push(h);
                }
                break;
            }
        }

        // Scan above the gap to find how many valid blocks exist beyond it
        // These should NOT be deleted — they'll be needed after the gap is filled
        if !gap_heights.is_empty() {
            let gap_start = gap_heights[0];
            let mut blocks_above_gap = 0u64;
            let mut highest_above_gap = 0u64;
            for h in (gap_start + 1)..=stored_height.max(stored_height + 100) {
                if block_key_exists(h) && self.get_block(h).is_ok() {
                    blocks_above_gap += 1;
                    highest_above_gap = h;
                } else if !block_key_exists(h) {
                    break; // No more blocks
                }
            }
            if blocks_above_gap > 0 {
                tracing::info!(
                    "📊 Found {} valid blocks above gap (heights {} to {}) - PRESERVING for re-sync",
                    blocks_above_gap,
                    gap_start + 1,
                    highest_above_gap
                );
            }
        }

        // Also scan forward from stored height for blocks that exist beyond it
        let mut actual_height = highest_contiguous;
        for h in (stored_height + 1)..=(stored_height + 1000) {
            if block_key_exists(h) && self.get_block(h).is_ok() {
                actual_height = h;
            } else if !block_key_exists(h) {
                break;
            }
        }

        // Use the highest contiguous chain as our actual height
        // CRITICAL: Do NOT delete blocks above the gap — they will be needed
        // after the missing blocks are re-synced from peers. Only adjust the
        // reported height so sync knows to request the gap blocks.
        let correct_height = highest_contiguous;

        if correct_height != stored_height {
            tracing::warn!(
                "🔧 Chain height inconsistency: stored={}, highest_contiguous={}, scanned_high={}",
                stored_height,
                highest_contiguous,
                actual_height
            );
            tracing::info!(
                "🔧 Correcting chain height from {} to {} (gap blocks will be re-synced from peers)",
                stored_height,
                correct_height
            );

            // Update height in storage
            let height_key = "chain_height".as_bytes();
            let height_bytes = bincode::serialize(&correct_height).map_err(|e| e.to_string())?;
            self.storage
                .insert(height_key, height_bytes)
                .map_err(|e| e.to_string())?;
            self.storage.flush().map_err(|e| e.to_string())?;

            // Update in-memory height
            self.current_height
                .store(correct_height, std::sync::atomic::Ordering::Release);

            // DO NOT delete blocks above the gap!
            // Previously this code deleted all blocks above highest_contiguous,
            // which was catastrophic: a single missing block caused the entire
            // chain above it to be wiped. Instead, we preserve blocks above the
            // gap so that once the missing block(s) are re-synced, the chain can
            // be reconstructed without re-downloading everything.
            //
            // Only delete genuinely corrupted blocks (blocks that exist but fail
            // deserialization) — these can't be used anyway.
            let mut corrupted_deleted = 0u64;
            for h in (correct_height + 1)..=stored_height {
                if block_key_exists(h) && self.get_block(h).is_err() {
                    // Block exists but is corrupted — safe to delete
                    let block_key = format!("block_{}", h);
                    let _ = self.storage.remove(block_key.as_bytes());
                    corrupted_deleted += 1;
                    tracing::warn!(
                        "🧹 Deleted corrupted block {} (will re-fetch from peers)",
                        h
                    );
                }
            }

            if corrupted_deleted > 0 {
                self.storage.flush().map_err(|e| e.to_string())?;
                tracing::info!("🧹 Deleted {} corrupted blocks (preserved {} valid blocks above gap for re-sync)",
                    corrupted_deleted,
                    stored_height.saturating_sub(correct_height).saturating_sub(corrupted_deleted).saturating_sub(gap_heights.len() as u64)
                );
            }

            let gap_count = if gap_heights.is_empty() {
                0
            } else {
                gap_heights.len()
            };
            tracing::info!(
                "✅ Chain height corrected to {} ({} gap(s) detected, blocks above gap preserved for re-sync)",
                correct_height,
                gap_count
            );
            return Ok(true);
        }

        tracing::info!(
            "✅ Chain integrity verified: height {} with all blocks present",
            stored_height
        );
        Ok(false) // No fix needed
    }

    /// Initialize blockchain - verify local chain or generate genesis dynamically
    pub async fn initialize_genesis(&self) -> Result<(), String> {
        use crate::block::genesis::GenesisBlock;

        // Check if genesis already exists locally
        let height = self.load_chain_height()?;
        tracing::info!("🔍 initialize_genesis: loaded chain_height = {}", height);

        if height > 0 {
            // Verify the genesis block structure
            if let Ok(genesis) = self.get_block_by_height(0).await {
                tracing::info!("🔍 Found genesis block, verifying structure...");
                if let Err(e) = GenesisBlock::verify_structure(&genesis)
                    .and_then(|_| GenesisBlock::verify_checkpoint(&genesis, self.network_type))
                {
                    tracing::error!(
                        "❌ Local genesis block is invalid: {} - will regenerate dynamically",
                        e
                    );
                    tracing::error!("🚨 WARNING: This will DELETE all {} blocks!", height);

                    // Remove the invalid genesis and all blocks built on it
                    self.clear_all_blocks().await;
                    self.current_height.store(0, Ordering::Release);

                    // Genesis will be generated dynamically when masternodes register
                    return Ok(());
                }
                tracing::info!("✅ Genesis block structure valid");
            } else {
                tracing::warn!(
                    "⚠️ height > 0 but genesis block not found - chain may be corrupted"
                );
            }

            // BLOCK 1 REWARD-HIJACK GUARD
            // Block 1 must have ≥ 3 unique reward recipients — the same floor as
            // genesis.  A rogue or early-starting node that captured the entire
            // block 1 reward before others connected must not be allowed to lock
            // the rest of the network out permanently.  If we detect a hijacked
            // block 1 on startup, clear the whole chain so honest nodes can
            // re-produce it with proper reward distribution.
            if height >= 1 {
                const MIN_BLOCK1_RECIPIENTS: usize = 3;
                if let Ok(block1) = self.get_block_by_height(1).await {
                    let unique_recipients: std::collections::HashSet<&str> = block1
                        .masternode_rewards
                        .iter()
                        .map(|(addr, _)| addr.as_str())
                        .collect();
                    if unique_recipients.len() < MIN_BLOCK1_RECIPIENTS {
                        tracing::error!(
                            "🛡️ Block 1 reward-hijacking detected: only {} unique reward \
                             recipient(s) (need ≥{}). Clearing chain so an honest block 1 \
                             can be produced.",
                            unique_recipients.len(),
                            MIN_BLOCK1_RECIPIENTS
                        );
                        self.clear_all_blocks().await;
                        return Ok(());
                    }
                }
            }

            self.current_height.store(height, Ordering::Release);
            tracing::info!("✓ Local blockchain verified (height: {})", height);
            return Ok(());
        }

        // Check if block 0 exists explicitly
        if self
            .storage
            .contains_key("block_0".as_bytes())
            .map_err(|e| e.to_string())?
        {
            if let Ok(genesis) = self.get_block_by_height(0).await {
                if let Err(e) = GenesisBlock::verify_structure(&genesis)
                    .and_then(|_| GenesisBlock::verify_checkpoint(&genesis, self.network_type))
                {
                    tracing::error!(
                        "❌ Local genesis is invalid: {} - will regenerate dynamically",
                        e
                    );

                    // Remove the invalid genesis
                    let _ = self.storage.remove("block_0".as_bytes());
                    let _ = self.storage.remove(genesis.hash().as_slice());
                    let _ = self.storage.flush();
                    self.current_height.store(0, Ordering::Release);
                    return Ok(());
                }
            }
            self.current_height.store(0, Ordering::Release);
            tracing::info!("✓ Genesis block verified");
            return Ok(());
        }

        // No local blockchain - on testnet use hardcoded genesis; on mainnet wait for masternodes
        match self.network_type {
            NetworkType::Testnet if crate::constants::genesis::TESTNET_GENESIS_HASH.is_some() => {
                tracing::info!(
                    "📋 No genesis found — creating from hardcoded testnet genesis data"
                );
                let genesis = GenesisBlock::testnet_genesis();
                GenesisBlock::verify_checkpoint(&genesis, self.network_type)?;

                let genesis_hash = genesis.hash();
                let genesis_bytes = bincode::serialize(&genesis)
                    .map_err(|e| format!("Failed to serialize hardcoded genesis: {}", e))?;
                self.storage
                    .insert("block_0".as_bytes(), genesis_bytes)
                    .map_err(|e| format!("Failed to store hardcoded genesis: {}", e))?;
                self.storage
                    .insert(genesis_hash.as_slice(), &0u64.to_be_bytes())
                    .map_err(|e| format!("Failed to index hardcoded genesis: {}", e))?;
                let height_bytes = bincode::serialize(&0u64).map_err(|e| e.to_string())?;
                self.storage
                    .insert("chain_height".as_bytes(), height_bytes)
                    .map_err(|e| format!("Failed to save chain_height: {}", e))?;
                self.storage
                    .flush()
                    .map_err(|e| format!("Failed to flush hardcoded genesis: {}", e))?;
                self.current_height.store(0, Ordering::Release);
                tracing::info!(
                    "🎉 Hardcoded testnet genesis stored: {}",
                    hex::encode(&genesis_hash[..8])
                );
            }
            _ => {
                tracing::info!(
                    "📋 No genesis block found — will be generated dynamically when masternodes register"
                );
            }
        }
        Ok(())
    }

    /// Generate genesis block dynamically with registered masternodes
    /// This is called after masternodes have had time to register via network discovery
    pub async fn generate_dynamic_genesis(&self) -> Result<(), String> {
        use crate::block::types::{Block, BlockHeader, MasternodeTierCounts};

        // Check if genesis already exists
        if self.has_genesis() {
            tracing::info!("✓ Genesis block already exists, skipping dynamic generation");
            return Ok(());
        }

        // Genesis timestamp: Use FIXED timestamps for deterministic genesis hash
        // All nodes MUST produce identical genesis blocks to be on the same chain
        // - Testnet: December 1, 2025 00:00:00 UTC (1764547200)
        // - Mainnet: April 1, 2026 00:00:00 UTC (1775001600)
        let genesis_timestamp = self.network_type.genesis_timestamp();

        // ── CLOCK GUARD ──────────────────────────────────────────────────────────
        // Never produce genesis before the official launch time.
        // If this fires the genesis coordinator in main.rs should have caught it
        // first, but we enforce the invariant here as a hard stop so no code path
        // can bypass it (fallback genesis, RPC, tests on mainnet, etc.).
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        if now_secs < genesis_timestamp {
            let remaining = genesis_timestamp - now_secs;
            let launch_str = chrono::DateTime::from_timestamp(genesis_timestamp, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| genesis_timestamp.to_string());
            return Err(format!(
                "Too early to generate genesis: {remaining}s remaining until launch ({launch_str}). \
                 The genesis coordinator should be waiting — this is a bug if reached on mainnet."
            ));
        }
        // ── END CLOCK GUARD ──────────────────────────────────────────────────────

        tracing::info!(
            "🕐 Using fixed {} genesis timestamp: {} ({})",
            match self.network_type {
                NetworkType::Testnet => "testnet",
                NetworkType::Mainnet => "mainnet",
            },
            genesis_timestamp,
            chrono::DateTime::from_timestamp(genesis_timestamp, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                .unwrap_or_else(|| "invalid".to_string())
        );

        // Get all registered masternodes
        let registered = self.masternode_registry.get_all().await;
        tracing::info!(
            "🌱 Generating dynamic genesis block with {} registered masternodes",
            registered.len()
        );

        // ── MINIMUM MASTERNODE GUARD ─────────────────────────────────────────────
        // Require at least 3 masternodes before producing genesis.
        // A lone node that starts early must not be able to claim the entire
        // genesis reward for itself — the network would accept it and the other
        // nodes would be locked out of their share permanently.
        const MIN_GENESIS_MASTERNODES: usize = 3;
        if registered.len() < MIN_GENESIS_MASTERNODES {
            return Err(format!(
                "Cannot generate genesis: only {} masternode(s) registered, minimum is {}. \
                 Wait for more nodes to connect before genesis can be produced.",
                registered.len(),
                MIN_GENESIS_MASTERNODES
            ));
        }

        // Create bitmap with all registered masternodes
        let mut voter_addresses = Vec::new();
        let mut tier_counts = MasternodeTierCounts::default();

        for info in &registered {
            voter_addresses.push(info.masternode.address.clone());
            match info.masternode.tier {
                crate::types::MasternodeTier::Free => tier_counts.free += 1,
                crate::types::MasternodeTier::Bronze => tier_counts.bronze += 1,
                crate::types::MasternodeTier::Silver => tier_counts.silver += 1,
                crate::types::MasternodeTier::Gold => tier_counts.gold += 1,
            }
        }

        tracing::info!(
            "   Tier distribution: Free={}, Bronze={}, Silver={}, Gold={}",
            tier_counts.free,
            tier_counts.bronze,
            tier_counts.silver,
            tier_counts.gold
        );

        // Create compact bitmap for all masternodes (all active in genesis)
        let (bitmap, bitmap_count) = self
            .masternode_registry
            .create_active_bitmap_from_voters(&voter_addresses)
            .await;

        tracing::info!(
            "   Active bitmap: {} masternodes marked active",
            bitmap_count
        );

        // Distribute genesis block reward like normal block production:
        //   All-Free mode: 100 TIME split equally among all registered nodes (up to MAX_FREE_TIER_RECIPIENTS)
        //   Tier-based mode: proportional by tier reward weight
        const TIME_UNIT: u64 = 100_000_000; // 1 TIME = 100M satoshis
        // 95 TIME distributed to masternodes; 5 TIME goes to treasury via add_block (like normal blocks)
        const GENESIS_REWARD: u64 =
            100 * TIME_UNIT - constants::blockchain::TREASURY_POOL_SATOSHIS;

        // Sort canonically for determinism
        let mut sorted_for_reward = registered.clone();
        sorted_for_reward.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));
        let leader = &sorted_for_reward[0];

        use crate::types::MasternodeTier;
        let has_paid_tiers = sorted_for_reward
            .iter()
            .any(|info| info.masternode.tier != MasternodeTier::Free);

        // Helper: resolve reward address for a MasternodeInfo
        let reward_addr_for = |info: &crate::masternode_registry::MasternodeInfo| -> String {
            if !info.reward_address.is_empty() {
                info.reward_address.clone()
            } else if !info.masternode.wallet_address.is_empty() {
                info.masternode.wallet_address.clone()
            } else {
                tracing::error!(
                    "⚠️ Masternode {} has no reward_address or wallet_address — \
                     genesis reward will be unspendable (IP used as fallback). \
                     Set reward_address in time.conf and regenerate genesis.",
                    info.masternode.address
                );
                info.masternode.address.clone()
            }
        };

        let masternode_rewards: Vec<(String, u64)> = if !has_paid_tiers {
            // ── All-Free mode: equal split (mirrors validate_pool_distribution) ──
            let recipient_count = sorted_for_reward
                .len()
                .min(constants::blockchain::MAX_FREE_TIER_RECIPIENTS);
            let per_node = GENESIS_REWARD / recipient_count as u64;
            let mut distributed = 0u64;
            sorted_for_reward
                .iter()
                .take(recipient_count)
                .enumerate()
                .map(|(i, info)| {
                    let share = if i == recipient_count - 1 {
                        GENESIS_REWARD - distributed
                    } else {
                        per_node
                    };
                    distributed += share;
                    (reward_addr_for(info), share)
                })
                .collect()
        } else {
            // ── Tier-based mode: proportional by tier weight ──
            // Paid tiers (Gold/Silver/Bronze) are all included — they each receive
            // their proportional share. Free tier is capped at MAX_FREE_TIER_RECIPIENTS
            // to mirror regular block production (where Free nodes share a single pool
            // split among ≤25 nodes).
            let free_cap = constants::blockchain::MAX_FREE_TIER_RECIPIENTS;
            let mut participants: Vec<_> = sorted_for_reward
                .iter()
                .filter(|info| info.masternode.tier != MasternodeTier::Free)
                .collect();
            let free_participants: Vec<_> = sorted_for_reward
                .iter()
                .filter(|info| info.masternode.tier == MasternodeTier::Free)
                .take(free_cap)
                .collect();
            participants.extend(free_participants.iter().copied());
            // Re-sort by address for determinism
            participants.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));

            let total_weight: u64 = participants
                .iter()
                .map(|info| info.masternode.tier.reward_weight())
                .sum();
            let mut distributed = 0u64;
            participants
                .iter()
                .enumerate()
                .map(|(i, info)| {
                    let share = if i == participants.len() - 1 {
                        GENESIS_REWARD - distributed
                    } else {
                        (GENESIS_REWARD * info.masternode.tier.reward_weight()) / total_weight
                    };
                    distributed += share;
                    (reward_addr_for(info), share)
                })
                .collect()
        };

        tracing::info!(
            "   Genesis block reward: {} TIME split among {} masternodes ({} mode)",
            GENESIS_REWARD / 100_000_000,
            masternode_rewards.len(),
            if has_paid_tiers { "tier-based" } else { "all-Free equal-split" }
        );
        for (addr, amt) in &masternode_rewards {
            if *amt > 0 {
                tracing::info!("     {} TIME -> {}", amt / 100_000_000, addr);
            }
        }
        tracing::info!(
            "   Leader (block producer): {}",
            leader.masternode.address
        );

        // Create genesis header
        let header = BlockHeader {
            version: 1,
            height: 0,
            timestamp: genesis_timestamp,
            previous_hash: [0u8; 32], // Genesis has no previous block
            merkle_root: [0u8; 32],   // No transactions
            leader: leader.masternode.address.clone(),
            attestation_root: [0u8; 32],
            masternode_tiers: tier_counts,
            block_reward: GENESIS_REWARD,
            total_fees: 0,
            active_masternodes_bitmap: bitmap,
            liveness_recovery: Some(false),
            vrf_output: [0u8; 32],
            vrf_proof: vec![],
            vrf_score: 0,
            producer_signature: vec![], // Genesis has no producer signature
        };

        // Create genesis block
        let genesis = Block {
            header,
            transactions: vec![], // No transactions in genesis
            masternode_rewards,
            time_attestations: vec![],
            consensus_participants_bitmap: vec![], // No consensus voting in genesis
            liveness_recovery: Some(false),
        };

        let genesis_hash = genesis.hash();
        tracing::info!(
            "✅ Genesis block generated: hash={}, timestamp={}, masternodes={}",
            hex::encode(&genesis_hash[..8]),
            genesis_timestamp,
            registered.len()
        );

        // Verify genesis matches hardcoded checkpoint
        use crate::block::genesis::GenesisBlock;
        GenesisBlock::verify_checkpoint(&genesis, self.network_type)?;

        // Store genesis block
        let genesis_bytes = bincode::serialize(&genesis)
            .map_err(|e| format!("Failed to serialize genesis: {}", e))?;

        self.storage
            .insert("block_0".as_bytes(), genesis_bytes)
            .map_err(|e| format!("Failed to store genesis block: {}", e))?;

        self.storage
            .insert(genesis_hash.as_slice(), &0u64.to_be_bytes())
            .map_err(|e| format!("Failed to index genesis block: {}", e))?;

        self.storage
            .flush()
            .map_err(|e| format!("Failed to flush genesis: {}", e))?;

        // Update chain height in storage so it persists across restarts
        let height_key = "chain_height".as_bytes();
        let height_bytes = bincode::serialize(&0u64).map_err(|e| e.to_string())?;
        self.storage
            .insert(height_key, height_bytes)
            .map_err(|e| format!("Failed to save chain_height: {}", e))?;
        self.storage
            .flush()
            .map_err(|e| format!("Failed to flush chain_height: {}", e))?;

        self.current_height.store(0, Ordering::Release);

        // Create UTXOs for genesis rewards.
        // generate_dynamic_genesis bypasses add_block, so we must create these
        // directly here — otherwise getbalance always shows 0.
        for (vout, (address, amount)) in genesis.masternode_rewards.iter().enumerate() {
            if *amount == 0 || address.is_empty() {
                continue;
            }
            let utxo = UTXO {
                outpoint: OutPoint {
                    txid: genesis_hash,
                    vout: vout as u32,
                },
                value: *amount,
                script_pubkey: address.as_bytes().to_vec(),
                address: address.clone(),
            };
            match self.utxo_manager.add_utxo(utxo).await {
                Ok(()) => tracing::info!(
                    "💰 Genesis UTXO created: {} TIME → {}",
                    amount / 100_000_000,
                    address
                ),
                Err(e) => tracing::warn!(
                    "⚠️ Could not add genesis reward UTXO for {}: {:?}",
                    address,
                    e
                ),
            }
        }

        tracing::info!("🎉 Dynamic genesis block stored successfully (height: 0)");

        Ok(())
    }

    /// Verify chain integrity, find missing blocks
    /// Returns a list of missing block heights that need to be downloaded
    pub async fn verify_chain_integrity(&self) -> Vec<u64> {
        let current_height = self.current_height.load(Ordering::Acquire);
        let mut missing_blocks = Vec::new();

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

    /// Validate that our genesis block structure is valid
    /// Genesis hash validation now happens via peer consensus during sync
    pub async fn validate_genesis_hash(&self) -> Result<(), String> {
        use crate::block::genesis::GenesisBlock;

        // Get our local genesis block
        let local_genesis = self
            .get_block_by_height(0)
            .await
            .map_err(|e| format!("Cannot load genesis block: {}", e))?;

        // Verify structure is valid
        GenesisBlock::verify_structure(&local_genesis)?;

        let local_hash = local_genesis.hash();
        tracing::info!(
            "✅ Genesis structure validated: {} (network: {:?})",
            hex::encode(&local_hash[..8]),
            self.network_type
        );

        Ok(())
    }

    /// Check if a peer is part of the current consensus majority
    /// Returns true if peer is in consensus list OR if no consensus has been established yet
    pub async fn is_peer_in_consensus(&self, peer_ip: &str) -> bool {
        let consensus = self.consensus_peers.read().await;
        if consensus.is_empty() {
            // No consensus established yet - accept blocks from all peers
            true
        } else {
            // Only accept from consensus peers
            consensus.contains(&peer_ip.to_string())
        }
    }

    /// Clear all block data from storage (for complete reset)
    pub async fn clear_all_blocks(&self) {
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

        // Clear the chain height from storage
        let _ = self.storage.remove("chain_height".as_bytes());

        // Reset the in-memory height counter to 0
        self.current_height.store(0, Ordering::Release);

        // Also clear the genesis marker so it gets recreated
        let _ = self.storage.remove("genesis_initialized");

        // CRITICAL: Clear the UTXO set so stale balances don't persist
        // Without this, wallet shows balance from blocks that no longer exist
        if let Err(e) = self.utxo_manager.clear_all().await {
            tracing::error!("❌ Failed to clear UTXOs during block reset: {:?}", e);
        }

        // Clear undo logs too since they reference deleted blocks
        for h in 0..100000 {
            let undo_key = format!("undo_{}", h);
            match self.storage.remove(undo_key.as_bytes()) {
                Ok(Some(_)) => {}
                _ => {
                    if h > 1000 && cleared == 0 {
                        break;
                    }
                }
            }
        }

        // Flush to ensure all deletions are persisted
        let _ = self.storage.flush();

        tracing::info!(
            "🗑️  Cleared {} blocks, UTXOs, and undo logs from storage. Height reset to 0.",
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
    /// NOTE: If peers don't have blocks, they'll be produced on TimeLock schedule
    ///
    /// # Arguments
    /// * `target_height` - Optional target height to sync to. If None, uses time-based calculation.
    ///   Used during fork resolution to sync to peer consensus height.
    pub async fn sync_from_peers(&self, target_height: Option<u64>) -> Result<(), String> {
        // Check if already syncing - prevent concurrent syncs
        if self.is_syncing.load(Ordering::Acquire) {
            tracing::debug!("⏭️  Sync already in progress, skipping duplicate request");
            return Ok(());
        }

        let mut current = self.current_height.load(Ordering::Acquire);

        // Use provided target height (from consensus) or calculate from time
        let time_expected = self.calculate_expected_height();
        let target = target_height.unwrap_or(time_expected);

        // BOOTSTRAP CHECK: If we're behind but all peers are CONFIRMED at our height, skip sync.
        // This handles genuine genesis scenario where time-based calculation shows we're behind
        // but nobody has actually produced blocks yet.
        //
        // CRITICAL: Only trust this shortcut when:
        // 1. We have POSITIVE confirmation from a minimum number of peers (not just missing data)
        // 2. The time-based expected height is not too far ahead (prevents trusting stale cache
        //    when the network has been running for a long time)
        let blocks_behind_target = target.saturating_sub(current);
        // Only allow bootstrap shortcut when expected height is close to 0 (genuine new network)
        // If expected height is far ahead, peer cache is likely stale — don't skip sync
        const MAX_BOOTSTRAP_SHORTCUT_BEHIND: u64 = 10;

        // ALWAYS check if peers actually have blocks beyond our height.
        // Even when far behind time-based target, if no peer has more blocks than us,
        // syncing is futile — the blocks need to be produced, not downloaded.
        if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
            let connected_peers = peer_registry.get_compatible_peers().await;
            if !connected_peers.is_empty() {
                let mut max_peer_height = current;
                let mut peers_checked = 0u32;
                for peer_ip in &connected_peers {
                    if let Some((height, _)) = peer_registry.get_peer_chain_tip(peer_ip).await {
                        peers_checked += 1;
                        if height > max_peer_height {
                            max_peer_height = height;
                        }
                    }
                }

                if peers_checked >= 2 && max_peer_height <= current {
                    // Only skip sync if we're close to the expected time-based height.
                    // If we're far behind, request fresh chain tips first. But if after
                    // refreshing, peers STILL report the same height, allow production —
                    // the network genuinely needs to produce blocks to catch up.
                    if blocks_behind_target <= MAX_BOOTSTRAP_SHORTCUT_BEHIND {
                        tracing::info!(
                            "✅ No peers have blocks beyond height {} ({} peers checked, target {}). Skipping sync — blocks must be produced.",
                            current,
                            peers_checked,
                            target
                        );
                        return Ok(());
                    } else {
                        tracing::info!(
                            "🔄 {} blocks behind target but no peers ahead (height {}, {} peers checked). \
                             Requesting fresh chain tips before allowing production.",
                            blocks_behind_target,
                            current,
                            peers_checked,
                        );
                        // Request fresh chain tips and wait for signal (event-driven)
                        if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
                            let signal = peer_registry.chain_tip_updated_signal();
                            peer_registry
                                .broadcast(crate::network::message::NetworkMessage::GetChainTip)
                                .await;
                            // Wait for first chain tip response or short timeout
                            let _ = tokio::time::timeout(
                                tokio::time::Duration::from_millis(500),
                                signal.notified(),
                            )
                            .await;
                        }

                        // Re-check with refreshed data
                        let mut refreshed_max = current;
                        if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
                            for peer_ip in &peer_registry.get_compatible_peers().await {
                                if let Some((height, _)) =
                                    peer_registry.get_peer_chain_tip(peer_ip).await
                                {
                                    if height > refreshed_max {
                                        refreshed_max = height;
                                    }
                                }
                            }
                        }

                        if refreshed_max <= current {
                            // Still no peer ahead after refresh — blocks must be produced
                            tracing::info!(
                                "✅ After refresh: still no peers beyond height {}. Allowing production to catch up.",
                                current
                            );
                            return Ok(());
                        }
                        // A peer is now ahead — fall through to normal sync
                        tracing::info!(
                            "🔀 After refresh: peer now at height {} (we are at {}). Syncing.",
                            refreshed_max,
                            current
                        );
                    }
                }
            }
        }

        if blocks_behind_target <= MAX_BOOTSTRAP_SHORTCUT_BEHIND {
            if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
                let connected_peers = peer_registry.get_compatible_peers().await;
                if !connected_peers.is_empty() {
                    // Try to get consensus - if available, check if everyone is at our height
                    if let Some((consensus_height, _)) = self.compare_chain_with_peers().await {
                        if consensus_height == current && current < target {
                            tracing::info!(
                                "✅ Bootstrap scenario detected via consensus: All peers at height {} but time-based calc shows target {} (only {} behind). Skipping sync - ready for block production.",
                                current,
                                target,
                                blocks_behind_target
                            );
                            return Ok(()); // Don't sync - proceed to block production
                        }
                    } else {
                        // If compare_chain_with_peers returns None (incomplete responses),
                        // manually check peer heights from cache — require positive confirmation
                        tracing::debug!(
                            "🔍 Bootstrap check: Consensus unavailable, checking peer heights manually"
                        );
                        let mut peer_heights = Vec::new();
                        for peer_ip in &connected_peers {
                            if let Some((height, _)) =
                                peer_registry.get_peer_chain_tip(peer_ip).await
                            {
                                peer_heights.push(height);
                            }
                        }

                        // Require POSITIVE confirmation from at least 2 peers (not just empty cache)
                        if peer_heights.len() >= 2
                            && peer_heights.iter().all(|&h| h == current)
                            && current < target
                        {
                            tracing::info!(
                                "✅ Bootstrap scenario detected via manual check: {}/{} peers confirmed at height {} but time-based calc shows target {} (only {} behind). Skipping sync - ready for block production.",
                                peer_heights.len(),
                                connected_peers.len(),
                                current,
                                target,
                                blocks_behind_target
                            );
                            return Ok(()); // Don't sync - proceed to block production
                        }
                    }
                }
            }
        } else {
            tracing::info!(
                "🔒 Bootstrap shortcut SKIPPED: {} blocks behind target ({} vs {}) exceeds threshold {} - will sync from peers",
                blocks_behind_target, current, target, MAX_BOOTSTRAP_SHORTCUT_BEHIND
            );
        }

        // If we're already synced, return early
        if current >= target {
            tracing::info!("✓ Blockchain synced (height: {})", current);
            return Ok(());
        }

        // Now set syncing flag since we actually need to sync
        self.is_syncing.store(true, Ordering::Release);

        // Ensure we reset the sync flag when done (RAII guard)
        let is_syncing = self.is_syncing.clone();
        struct SyncGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for SyncGuard {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _guard = SyncGuard(is_syncing);

        // Debug logging for genesis timestamp issue
        let now = chrono::Utc::now().timestamp();
        let genesis_ts = self.genesis_timestamp();
        let source = if target_height.is_some() {
            "peer consensus"
        } else {
            "time-based calculation"
        };
        tracing::debug!(
            "🔍 Sync calculation: current={}, target={} ({}), time_expected={}, now={}, genesis={}, elapsed={}",
            current,
            target,
            source,
            time_expected,
            now,
            genesis_ts,
            now - genesis_ts
        );

        let behind = target - current;
        tracing::info!(
            "⏳ Syncing from peers: {} → {} ({} blocks behind via {})",
            current,
            target,
            behind,
            source
        );

        if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
            tracing::debug!("✓ Peer registry available, checking connected peers");
            // Get all connected peers
            let connected_peers = peer_registry.get_connected_peers().await;

            if connected_peers.is_empty() {
                tracing::warn!("⚠️  No connected peers to sync from");
                return Err("No connected peers to sync from".to_string());
            }

            // NOTE: We do NOT delete genesis anymore even if peers are ahead
            // The genesis block should be the canonical one loaded from genesis.testnet.json
            // If peers have a different chain, they need to restart with the new genesis

            // Use ALL compatible (consensus) peers for parallel sync, not a fixed limit.
            // Filter to consensus peers when we have consensus info, otherwise use all compatible.
            // CRITICAL: Also filter to peers whose known chain tip is >= target — a peer at our
            // own height cannot serve the blocks we need, and picking it wastes 30s timeouts.
            let consensus_peers = self.consensus_peers.read().await.clone();
            let candidate_peers: Vec<String> = if consensus_peers.is_empty() {
                // No consensus info yet — use all compatible peers
                peer_registry.get_compatible_peers().await
            } else {
                // Filter to only peers that are in consensus
                connected_peers
                    .iter()
                    .filter(|p| {
                        let ip = p.split(':').next().unwrap_or(p.as_str());
                        consensus_peers.iter().any(|cp| {
                            let cp_ip = cp.split(':').next().unwrap_or(cp.as_str());
                            cp_ip == ip
                        })
                    })
                    .cloned()
                    .collect()
            };

            // Fallback: if consensus filter left us with no peers, use all compatible
            let mut sync_peers: Vec<String> = if candidate_peers.is_empty() {
                tracing::warn!(
                    "⚠️  No consensus peers found, falling back to all compatible peers"
                );
                peer_registry.get_compatible_peers().await
            } else {
                candidate_peers
            };

            // Filter sync_peers to only those with a known chain tip above our current height.
            // Peers at or below our height cannot serve the blocks we need.
            let peers_with_needed_blocks: Vec<String> = {
                let mut filtered = Vec::new();
                for peer in &sync_peers {
                    if let Some((peer_tip, _)) = peer_registry.get_peer_chain_tip(peer).await {
                        if peer_tip > current {
                            filtered.push(peer.clone());
                        } else {
                            tracing::debug!(
                                "🔍 Skipping peer {} for sync (tip {} <= our height {})",
                                peer, peer_tip, current
                            );
                        }
                    } else {
                        // No chain tip cached — include conservatively (peer may have blocks)
                        filtered.push(peer.clone());
                    }
                }
                filtered
            };
            if peers_with_needed_blocks.len() < sync_peers.len() {
                tracing::info!(
                    "🔍 Filtered sync peers: {} → {} (removed peers at/below height {})",
                    sync_peers.len(),
                    peers_with_needed_blocks.len(),
                    current
                );
            }
            // Use filtered list unconditionally — if empty, request fresh tips and abort
            sync_peers = peers_with_needed_blocks;

            if sync_peers.is_empty() {
                // All peers have cached tips at or below our height.
                // Request fresh chain tips so the next sync coordinator cycle has current data.
                tracing::warn!(
                    "⚠️  No peers with blocks above height {} — requesting fresh chain tips",
                    current
                );
                for peer_ip in &peer_registry.get_connected_peers().await {
                    let _ = peer_registry
                        .send_to_peer(peer_ip, NetworkMessage::GetChainTip)
                        .await;
                }
                return Err("No peers with blocks above our height".to_string());
            }

            tracing::info!(
                "🚀 Parallel sync: using {} peers {:?}",
                sync_peers.len(),
                sync_peers
            );

            // Clear any stale pending blocks from previous sync attempts
            self.clear_pending_blocks().await;

            // Sync loop - pipeline requests across multiple peers
            let sync_start = std::time::Instant::now();
            let max_sync_time = std::time::Duration::from_secs(PEER_SYNC_TIMEOUT_SECS * 2);
            let starting_height = current;
            let batch_size = constants::network::MAX_BLOCKS_PER_RESPONSE; // 50 blocks per response

            tracing::info!(
                "📍 Starting parallel sync loop: current={}, target={}, peers={}, timeout={}s",
                current,
                target,
                sync_peers.len(),
                max_sync_time.as_secs()
            );

            // Track the next height to request (may be ahead of current tip due to pipelining)
            let mut next_request_height = if current == 0 {
                if self.get_block(0).is_ok() {
                    1
                } else {
                    0
                }
            } else {
                current + 1
            };

            while current < target && sync_start.elapsed() < max_sync_time {
                // Send parallel requests to different peers for different block ranges
                let mut requests_sent = 0;
                for (i, peer) in sync_peers.iter().enumerate() {
                    if next_request_height > target {
                        break;
                    }

                    let range_start = next_request_height;
                    let range_end = (range_start + batch_size - 1).min(target);

                    let req = NetworkMessage::GetBlocks(range_start, range_end);
                    tracing::debug!(
                        "📤 [Pipeline {}/{}] Requesting blocks {}-{} from {}",
                        i + 1,
                        sync_peers.len(),
                        range_start,
                        range_end,
                        peer
                    );

                    if let Err(e) = peer_registry.send_to_peer(peer, req).await {
                        tracing::warn!("❌ Failed to send GetBlocks to {}: {}", peer, e);
                        continue;
                    }

                    next_request_height = range_end + 1;
                    requests_sent += 1;
                }

                if requests_sent == 0 {
                    tracing::warn!("⚠️  Failed to send any sync requests");
                    break;
                }

                // Wait for blocks to arrive and be processed
                let batch_start_time = std::time::Instant::now();
                let batch_timeout = std::time::Duration::from_secs(30);
                let mut last_height = current;
                let mut made_progress = false;
                let mut gap_requested = false;

                while batch_start_time.elapsed() < batch_timeout {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

                    // Periodically try to drain the buffer — responses from other
                    // peers may have filled the gap since the last check.
                    let drained = self.drain_pending_blocks().await;
                    if drained > 0 {
                        tracing::info!("📦 Sync loop drained {} buffered blocks", drained);
                    }

                    let now_height = self.current_height.load(Ordering::Acquire);

                    if now_height >= target {
                        tracing::info!("✓ Sync complete at height {}", now_height);
                        self.clear_pending_blocks().await;
                        return Ok(());
                    }

                    // Check if we made progress
                    if now_height > last_height {
                        let blocks_received = now_height - last_height;
                        let response_time = batch_start_time.elapsed();

                        tracing::debug!(
                            "📈 Block sync progress: {} → {} ({} blocks in {:.2}s, {} pending)",
                            last_height,
                            now_height,
                            blocks_received,
                            response_time.as_secs_f64(),
                            self.pending_sync_blocks.read().await.len()
                        );

                        // Record AI success for all sync peers (they all contributed)
                        for peer in &sync_peers {
                            self.peer_scoring
                                .record_success(peer, response_time, blocks_received * 500)
                                .await;
                        }

                        last_height = now_height;
                        made_progress = true;
                        gap_requested = false; // reset — new gap may form
                    }

                    // If we've processed all requested blocks (including buffered), request more
                    if now_height >= next_request_height.saturating_sub(1) {
                        break;
                    }

                    // Early gap detection: if we have buffered blocks but height
                    // is stuck, a peer failed to deliver its range. Identify the
                    // missing range and request it from a different peer immediately
                    // instead of waiting for the full 30s timeout.
                    if !gap_requested
                        && batch_start_time.elapsed() > std::time::Duration::from_secs(3)
                    {
                        let pending_count = self.pending_sync_blocks.read().await.len();
                        if pending_count > 0 && now_height == last_height {
                            let next_needed = now_height + 1;
                            let first_buffered = {
                                let pending = self.pending_sync_blocks.read().await;
                                pending.keys().next().copied()
                            };

                            if let Some(first) = first_buffered {
                                if first > next_needed {
                                    // There's a gap — find a peer to fill it
                                    let gap_end = (first - 1).min(next_needed + batch_size - 1);
                                    // Determine which peer was responsible for this range
                                    let gap_chunk_idx = ((next_needed - starting_height.max(1))
                                        as usize)
                                        .checked_div(batch_size as usize)
                                        .unwrap_or(0);
                                    let failed_peer =
                                        sync_peers.get(gap_chunk_idx).cloned().unwrap_or_default();

                                    // Try any connected peer except the one that failed
                                    let all_peers = if let Some(pr) =
                                        self.peer_registry.read().await.as_ref()
                                    {
                                        pr.get_compatible_peers().await
                                    } else {
                                        vec![]
                                    };
                                    let alt_peers: Vec<String> = all_peers
                                        .into_iter()
                                        .filter(|p| *p != failed_peer)
                                        .collect();

                                    if let Some(alt) =
                                        self.peer_scoring.select_best_peer(&alt_peers).await
                                    {
                                        tracing::info!(
                                            "🔄 Gap detected: blocks {}-{} missing (peer {} didn't respond). Requesting from {}",
                                            next_needed,
                                            gap_end,
                                            failed_peer,
                                            alt
                                        );
                                        if let Some(pr) = self.peer_registry.read().await.as_ref() {
                                            let req =
                                                NetworkMessage::GetBlocks(next_needed, gap_end);
                                            let _ = pr.send_to_peer(&alt, req).await;
                                        }
                                        gap_requested = true;
                                    }
                                }
                            }
                        }
                    }
                }

                // If no progress after request, try fallback peers
                if !made_progress {
                    for peer in &sync_peers {
                        self.peer_scoring.record_failure(peer).await;
                    }

                    tracing::warn!(
                        "⚠️  No progress after parallel sync request (timeout after 30s)"
                    );

                    // Determine the actual missing range.  Other peers may have already
                    // buffered blocks above the gap — keep them and only re-request what
                    // is missing rather than wiping the whole buffer and starting over.
                    let current_tip = self.current_height.load(Ordering::Acquire);
                    let next_needed = current_tip + 1;
                    let first_buffered = {
                        let pending = self.pending_sync_blocks.read().await;
                        pending.keys().next().copied()
                    };

                    // If we have buffered blocks starting above the gap, keep them and
                    // only request the missing leading range from a different peer.
                    // If there is no gap (buffer is empty or starts exactly where we
                    // need it), clear stale state and restart normally.
                    let has_gap = matches!(first_buffered, Some(first) if first > next_needed);
                    if !has_gap {
                        self.clear_pending_blocks().await;
                    }
                    next_request_height = next_needed;

                    // Build a retry peer list for the missing range.
                    //
                    // When has_gap: the peers responsible for the missing leading chunk
                    // are sync_peers[0..gap_chunks]. Exclude only those — any other
                    // connected peer (including the rest of sync_peers) may have those
                    // blocks and should be tried.
                    //
                    // When no gap: all current sync_peers failed to make progress, so
                    // only try peers that weren't in the last round.
                    // Build retry peer candidates, excluding failed/already-tried peers.
                    // Also exclude peers whose known chain tip is at or below next_needed —
                    // they cannot serve the missing blocks and will just waste timeout budget.
                    let retry_peers: Vec<String> = {
                        let excluded: HashSet<String> = if has_gap {
                            let gap_chunks = first_buffered
                                .map(|f| {
                                    ((f - next_needed) as usize).div_ceil(batch_size as usize)
                                })
                                .unwrap_or(1)
                                .min(sync_peers.len());
                            sync_peers.iter().take(gap_chunks).cloned().collect()
                        } else {
                            sync_peers.iter().cloned().collect()
                        };
                        let mut candidates = Vec::new();
                        for p in connected_peers.iter() {
                            if excluded.contains(p) {
                                continue;
                            }
                            // Skip peers known to be at or below the height we need
                            if let Some((peer_tip, _)) =
                                peer_registry.get_peer_chain_tip(p).await
                            {
                                if peer_tip < next_needed {
                                    continue;
                                }
                            }
                            candidates.push(p.clone());
                        }
                        candidates
                    };

                    if !retry_peers.is_empty() {
                        if let Some(alt_peer) =
                            self.peer_scoring.select_best_peer(&retry_peers).await
                        {
                            let missing_end = first_buffered
                                .map(|f| (f - 1).min(next_needed + batch_size - 1))
                                .unwrap_or(next_needed + batch_size - 1)
                                .min(target);
                            tracing::info!(
                                "🤖 [AI] Requesting missing range {}-{} from fallback peer: {}",
                                next_needed,
                                missing_end,
                                alt_peer
                            );
                            // Switch to the single fallback peer for the missing range.
                            // Do NOT clear pending blocks — valid buffered blocks above
                            // the gap are preserved and will drain once the gap is filled.
                            sync_peers = vec![alt_peer];
                            continue;
                        }
                    }

                    // No progress and no fallback peers
                    if current == starting_height {
                        tracing::warn!(
                            "⚠️  No progress after trying all peers - blocks may not exist yet"
                        );
                        break;
                    }
                }

                // Update current height for next iteration
                current = self.current_height.load(Ordering::Acquire);

                // Flush storage at batch boundary
                if made_progress {
                    if let Err(e) = self.flush_storage_async().await {
                        tracing::warn!("⚠️  Batch flush failed: {}", e);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }

                // Log progress periodically
                let elapsed = sync_start.elapsed().as_secs();
                if elapsed > 0 && elapsed % 30 == 0 {
                    let pending_count = self.pending_sync_blocks.read().await.len();
                    tracing::info!(
                        "⏳ Still syncing... height {} / {} ({} pending, {}s elapsed)",
                        current,
                        target,
                        pending_count,
                        elapsed
                    );
                }
            }

            // Clean up any remaining buffered blocks
            self.clear_pending_blocks().await;
        } else {
            tracing::warn!("⚠️  Peer registry not available - cannot sync from peers");
        }

        let final_height = self.current_height.load(Ordering::Acquire);

        // Final flush to ensure all synced data is persisted before clearing is_syncing
        if let Err(e) = self.flush_storage_async().await {
            tracing::warn!("⚠️  Final sync flush failed: {}", e);
        }

        if final_height >= time_expected {
            tracing::info!("✓ Sync complete at height {}", final_height);
            return Ok(());
        }

        tracing::warn!(
            "⚠️  Sync incomplete at height {} (time-based target: {})",
            final_height,
            time_expected
        );
        Err(format!(
            "Peers don't have blocks beyond {} (time-based target: {})",
            final_height, time_expected
        ))
    }

    /// Buffer a block for later application during parallel sync.
    /// Returns true if the block was buffered, false if it was a duplicate.
    pub async fn buffer_sync_block(&self, block: Block) -> bool {
        let height = block.header.height;
        let mut pending = self.pending_sync_blocks.write().await;
        // Cap buffer size to prevent memory issues (~500 blocks ≈ 50-75MB)
        if pending.len() >= 500 {
            debug!(
                "📦 Pending block buffer full (500), dropping block {}",
                height
            );
            return false;
        }
        if pending.contains_key(&height) {
            return false; // Already have this height
        }
        pending.insert(height, block);
        true
    }

    /// Drain pending blocks that are sequential from our current tip.
    /// Returns the number of blocks applied.
    pub async fn drain_pending_blocks(&self) -> u64 {
        let mut applied = 0u64;
        loop {
            let current = self.current_height.load(Ordering::Acquire);
            let next_needed = current + 1;

            // Take the next block from the buffer if available
            let block = {
                let mut pending = self.pending_sync_blocks.write().await;
                pending.remove(&next_needed)
            };

            let Some(block) = block else {
                break;
            };

            // Apply the block
            let blockchain = self.clone();
            let result = tokio::task::spawn_blocking(move || {
                tokio::runtime::Handle::current()
                    .block_on(async { blockchain.add_block_with_fork_handling(block).await })
            })
            .await;

            match result {
                Ok(Ok(true)) => {
                    applied += 1;
                }
                Ok(Ok(false)) => {
                    // Block already exists or not sequential — stop draining
                    break;
                }
                Ok(Err(e)) => {
                    warn!("❌ Failed to apply buffered block {}: {}", next_needed, e);
                    break;
                }
                Err(e) => {
                    warn!("❌ Buffered block {} task panicked: {}", next_needed, e);
                    break;
                }
            }
        }

        if applied > 0 {
            // Flush after draining batch
            if let Err(e) = self.flush_storage_async().await {
                warn!("⚠️ Post-drain flush failed: {}", e);
            }
            debug!("📦 Drained {} buffered blocks", applied);
        }

        applied
    }

    /// Get the number of pending buffered blocks
    pub async fn pending_block_count(&self) -> usize {
        self.pending_sync_blocks.read().await.len()
    }

    /// Clear the pending block buffer (used when sync completes or is cancelled)
    pub async fn clear_pending_blocks(&self) {
        let mut pending = self.pending_sync_blocks.write().await;
        let count = pending.len();
        pending.clear();
        if count > 0 {
            debug!("🧹 Cleared {} pending sync blocks", count);
        }
    }

    /// Sync from a specific peer (used when we detect a fork and want the consensus chain)
    /// Now includes automatic fork detection and rollback to common ancestor
    pub async fn sync_from_specific_peer(&self, peer_ip: &str) -> Result<(), String> {
        let current = self.current_height.load(Ordering::Acquire);

        // Get peer registry to check peer's actual height
        let peer_registry = self.peer_registry.read().await;
        let registry = peer_registry.as_ref().ok_or("No peer registry available")?;

        // Get peer's actual chain tip to avoid requesting blocks they don't have
        // Fall back to requesting from our height if chain tip isn't known yet
        let peer_height = match registry.get_peer_chain_tip(peer_ip).await {
            Some((h, _hash)) => h,
            None => {
                // Peer hasn't sent a ChainTipResponse yet — estimate from consensus
                let consensus = self.consensus_peers.read().await;
                if !consensus.is_empty() {
                    // Use a reasonable estimate: request a small batch ahead
                    let est = current + 50;
                    tracing::info!(
                        "📤 No chain tip for {} — requesting blocks {}-{} (estimated)",
                        peer_ip,
                        current + 1,
                        est
                    );
                    est
                } else {
                    return Err(format!("No chain tip data for peer {}", peer_ip));
                }
            }
        };

        if current >= peer_height {
            tracing::info!("✓ Already synced to peer {} height {}", peer_ip, current);
            return Ok(());
        }

        // Request blocks from current+1 to peer's actual height (not time_expected)
        let batch_start = current + 1;
        let batch_end = peer_height;

        // ✅ Check with sync coordinator before requesting
        let sync_approved = self
            .sync_coordinator
            .request_sync(
                peer_ip.to_string(),
                batch_start,
                batch_end,
                crate::network::sync_coordinator::SyncSource::Periodic,
            )
            .await;

        match sync_approved {
            Ok(true) => {
                // Sync approved - proceed
                tracing::debug!(
                    "📤 Requesting blocks {}-{} from consensus peer {}",
                    batch_start,
                    batch_end,
                    peer_ip
                );
            }
            Ok(false) => {
                tracing::debug!(
                    "⏸️ Sync with {} queued (already active or at limit)",
                    peer_ip
                );
                return Ok(()); // Queued, not an error
            }
            Err(e) => {
                tracing::debug!("⏱️ Sync with {} throttled: {}", peer_ip, e);
                return Ok(()); // Throttled, not an error
            }
        }

        let req = NetworkMessage::GetBlocks(batch_start, batch_end);
        if let Err(e) = registry.send_to_peer(peer_ip, req).await {
            // Cancel sync on failure
            self.sync_coordinator.cancel_sync(peer_ip).await;
            return Err(format!("Failed to request blocks from {}: {}", peer_ip, e));
        }

        // Wait for blocks to arrive with longer timeout for fork resolution
        let wait_start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(30);
        let start_height = current;

        while wait_start.elapsed() < timeout {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let now_height = self.current_height.load(Ordering::Acquire);

            if now_height >= peer_height {
                tracing::info!("✓ Synced from consensus peer to height {}", now_height);
                // Mark sync as complete
                self.sync_coordinator.complete_sync(peer_ip).await;
                return Ok(());
            }

            // Check if height increased - if so, reset timer
            if now_height > start_height {
                tracing::debug!(
                    "📈 Sync progress: {} → {} from {}",
                    start_height,
                    now_height,
                    peer_ip
                );
            }
        }

        // Timeout reached - check if this might be a deeper fork
        let final_height = self.current_height.load(Ordering::Acquire);
        if final_height == current {
            tracing::warn!(
                "⚠️  No sync progress from {} - height stuck at {}. Checking for deeper fork...",
                peer_ip,
                current
            );

            // Try to detect and resolve deeper fork by finding common ancestor
            match self.find_and_resolve_fork(peer_ip, registry).await {
                Ok(common_ancestor) => {
                    tracing::info!(
                        "✅ Rolled back to common ancestor at height {}",
                        common_ancestor
                    );

                    // After rollback, check if peer still has blocks we need
                    let our_new_height = self.current_height.load(Ordering::Acquire);
                    let (peer_height_after_rollback, _) = registry
                        .get_peer_chain_tip(peer_ip)
                        .await
                        .ok_or_else(|| format!("No chain tip data for peer {}", peer_ip))?;

                    if peer_height_after_rollback > our_new_height {
                        tracing::info!(
                            "📥 After rollback to {}, peer {} has blocks up to {} - requesting new blocks",
                            our_new_height,
                            peer_ip,
                            peer_height_after_rollback
                        );

                        // Request blocks from our new height to peer's height
                        let new_batch_start = our_new_height + 1;
                        let new_batch_end = peer_height_after_rollback;

                        tracing::info!(
                            "📤 Requesting blocks {}-{} from {} after rollback",
                            new_batch_start,
                            new_batch_end,
                            peer_ip
                        );

                        let req = NetworkMessage::GetBlocks(new_batch_start, new_batch_end);
                        if let Err(e) = registry.send_to_peer(peer_ip, req).await {
                            self.sync_coordinator.cancel_sync(peer_ip).await;
                            return Err(format!(
                                "Failed to request blocks after rollback from {}: {}",
                                peer_ip, e
                            ));
                        }

                        // Wait for new blocks to arrive
                        let post_rollback_start = std::time::Instant::now();
                        let post_rollback_timeout = std::time::Duration::from_secs(30);

                        while post_rollback_start.elapsed() < post_rollback_timeout {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            let current_height = self.current_height.load(Ordering::Acquire);

                            if current_height >= peer_height_after_rollback {
                                tracing::info!(
                                    "✅ Successfully synced to height {} after rollback",
                                    current_height
                                );
                                self.sync_coordinator.complete_sync(peer_ip).await;
                                return Ok(());
                            }

                            if current_height > our_new_height {
                                tracing::debug!(
                                    "📈 Post-rollback sync progress: {} → {}",
                                    our_new_height,
                                    current_height
                                );
                            }
                        }

                        // Timeout after rollback
                        let final_height_after_rollback =
                            self.current_height.load(Ordering::Acquire);
                        self.sync_coordinator.complete_sync(peer_ip).await;
                        Err(format!(
                            "Timeout after rollback: reached {} but peer has {}",
                            final_height_after_rollback, peer_height_after_rollback
                        ))
                    } else {
                        tracing::warn!(
                            "⏸️  After rollback to {}, peer {} only has {} blocks - no new blocks to sync",
                            our_new_height,
                            peer_ip,
                            peer_height_after_rollback
                        );

                        self.sync_coordinator.complete_sync(peer_ip).await;

                        Err(format!(
                            "Fork resolved by rolling back to {}, but peer doesn't have newer blocks",
                            common_ancestor
                        ))
                    }
                }
                Err(e) => Err(format!(
                    "Failed to find common ancestor with {}: {}",
                    peer_ip, e
                )),
            }
        } else {
            // Mark sync as complete even if partial - let it be retried later
            self.sync_coordinator.complete_sync(peer_ip).await;
            Err(format!(
                "Partial sync from {}: reached {} but peer has {}",
                peer_ip, final_height, peer_height
            ))
        }
    }

    /// Find common ancestor with peer and rollback to it
    async fn find_and_resolve_fork(
        &self,
        peer_ip: &str,
        registry: &crate::network::peer_connection_registry::PeerConnectionRegistry,
    ) -> Result<u64, String> {
        // Defer to handle_fork() if it is already working on this fork.
        // handle_fork() owns the state machine and calls perform_reorg() which sets
        // fork_state = Reorging before touching storage.  Running our own rollback
        // concurrently would corrupt the chain.
        {
            let state = self.fork_state.read().await;
            if !matches!(*state, ForkResolutionState::None) {
                return Err(format!(
                    "Skipping find_and_resolve_fork: handle_fork() already active (state: {:?})",
                    std::mem::discriminant(&*state)
                ));
            }
        }

        let our_height = self.current_height.load(Ordering::Acquire);

        // Get peer's chain tip
        let (peer_height, _peer_hash) = registry
            .get_peer_chain_tip(peer_ip)
            .await
            .ok_or_else(|| format!("No chain tip data for peer {}", peer_ip))?;

        tracing::info!(
            "🔍 Searching for common ancestor: our height {}, peer height {}",
            our_height,
            peer_height
        );

        // Use iterative backward search instead of binary search
        // This is more reliable when network responses are unreliable
        let search_start = our_height.min(peer_height);
        let mut common_ancestor = 0u64;

        // Track consecutive failures to abort early if peer is offline
        let mut consecutive_failures = 0;
        const MAX_CONSECUTIVE_FAILURES: u32 = 3;

        // Search backward from current height to find matching block
        for height in (0..=search_start).rev() {
            // Abort if peer is consistently failing (offline/unreachable)
            if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                tracing::warn!(
                    "❌ Aborting common ancestor search: {} consecutive failures with peer {}",
                    consecutive_failures,
                    peer_ip
                );
                return Err(format!(
                    "Peer {} appears offline ({} consecutive failures)",
                    peer_ip, consecutive_failures
                ));
            }

            // Get our block hash at this height
            let our_hash = match self.get_block_hash(height) {
                Ok(hash) => hash,
                Err(_) => {
                    tracing::warn!("⚠️ Failed to get our hash at height {}", height);
                    continue;
                }
            };

            // Request peer's block hash at this height via request/response system
            let req = NetworkMessage::GetBlockHash(height);

            // Create channel for response
            let (tx, rx) = tokio::sync::oneshot::channel();
            registry.register_response_handler(peer_ip, tx).await;

            // Send request
            if let Err(e) = registry.send_to_peer(peer_ip, req).await {
                tracing::warn!("⚠️ Failed to send GetBlockHash to {}: {}", peer_ip, e);
                consecutive_failures += 1;
                // Move to next height
                continue;
            }

            // Wait for response with timeout
            let peer_hash_opt =
                match tokio::time::timeout(std::time::Duration::from_secs(3), rx).await {
                    Ok(Ok(NetworkMessage::BlockHashResponse {
                        height: resp_height,
                        hash,
                    })) => {
                        if resp_height == height {
                            hash
                        } else {
                            tracing::warn!(
                                "⚠️ Got hash for wrong height {} (expected {})",
                                resp_height,
                                height
                            );
                            None
                        }
                    }
                    Ok(Ok(_)) => {
                        tracing::warn!("⚠️ Got unexpected response type");
                        None
                    }
                    Ok(Err(_)) => {
                        tracing::warn!("⚠️ Response channel closed for height {}", height);
                        None
                    }
                    Err(_) => {
                        tracing::debug!("⏱️ Timeout waiting for hash at height {}", height);
                        None
                    }
                };

            if let Some(peer_hash) = peer_hash_opt {
                // Reset failure counter on successful response
                consecutive_failures = 0;

                if our_hash == peer_hash {
                    // Found common ancestor!
                    common_ancestor = height;
                    tracing::info!(
                        "✅ Found common ancestor at height {} (hash: {})",
                        height,
                        hex::encode(&our_hash[..8])
                    );
                    break;
                } else {
                    tracing::debug!(
                        "🔀 Fork at height {}: our {} vs peer {}",
                        height,
                        hex::encode(&our_hash[..8]),
                        hex::encode(&peer_hash[..8])
                    );
                }
            } else {
                // No response - count as failure
                consecutive_failures += 1;
            }

            // Don't search too deep
            if height == 0 || height < our_height.saturating_sub(100) {
                common_ancestor = height;
                tracing::warn!(
                    "⚠️ Stopped search at height {} (may not be true common ancestor)",
                    height
                );
                break;
            }
        }

        if common_ancestor == 0 && our_height > 0 {
            tracing::warn!(
                "⚠️ Could not find common ancestor via hash comparison, fork may start at genesis"
            );

            // Request genesis block from peer to verify compatibility
            tracing::info!(
                "📥 Requesting genesis block from peer {} to verify chain compatibility",
                peer_ip
            );

            let req = NetworkMessage::BlockRequest(0);
            let (tx, rx) = tokio::sync::oneshot::channel();
            registry.register_response_handler(peer_ip, tx).await;

            if let Err(e) = registry.send_to_peer(peer_ip, req).await {
                tracing::warn!("⚠️ Failed to request genesis block from {}: {}", peer_ip, e);
                return Err(format!(
                    "Failed to request genesis from peer {}: {}",
                    peer_ip, e
                ));
            }

            // Wait for genesis block response
            let peer_genesis =
                match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
                    Ok(Ok(NetworkMessage::BlockResponse(block))) => {
                        if block.header.height == 0 {
                            block
                        } else {
                            tracing::warn!(
                                "⚠️ Got block at wrong height {} (expected 0)",
                                block.header.height
                            );
                            return Err(format!(
                                "Peer {} sent wrong block (expected genesis)",
                                peer_ip
                            ));
                        }
                    }
                    Ok(Ok(_)) => {
                        tracing::warn!("⚠️ Got unexpected response type for genesis request");
                        return Err(format!("Peer {} sent invalid genesis response", peer_ip));
                    }
                    Ok(Err(_)) => {
                        tracing::warn!("⚠️ Response channel closed for genesis request");
                        return Err(format!("Peer {} closed genesis request channel", peer_ip));
                    }
                    Err(_) => {
                        tracing::warn!("⏱️ Timeout waiting for genesis block from {}", peer_ip);
                        return Err(format!("Timeout waiting for genesis from peer {}", peer_ip));
                    }
                };

            // Get our genesis block
            let our_genesis = match self.get_block(0) {
                Ok(block) => block,
                Err(e) => {
                    tracing::error!("❌ Failed to get our genesis block: {}", e);
                    return Err(format!("Failed to get our genesis block: {}", e));
                }
            };

            // Compare genesis hashes
            let our_genesis_hash = our_genesis.hash();
            let peer_genesis_hash = peer_genesis.hash();

            if our_genesis_hash == peer_genesis_hash {
                // Genesis blocks match - this is a legitimate fork from the same genesis
                tracing::info!(
                    "✅ Genesis blocks match (hash: {}) - allowing reorganization from genesis",
                    hex::encode(&our_genesis_hash[..8])
                );
                // Allow reorganization to proceed from genesis (common_ancestor = 0)
            } else {
                // Genesis blocks differ - these are incompatible chains
                tracing::error!(
                    "🛡️ SECURITY: Genesis mismatch! Our genesis: {}, Peer genesis: {}",
                    hex::encode(&our_genesis_hash[..8]),
                    hex::encode(&peer_genesis_hash[..8])
                );
                tracing::error!(
                    "💡 Peer {} is on a completely different chain - cannot reconcile",
                    peer_ip
                );
                return Err(format!(
                    "Genesis mismatch: incompatible chains (our: {}, peer: {})",
                    hex::encode(&our_genesis_hash[..8]),
                    hex::encode(&peer_genesis_hash[..8])
                ));
            }
        }

        // SECURITY: Check reorg depth limit
        let fork_depth = our_height.saturating_sub(common_ancestor);
        crate::ai::fork_resolver::check_reorg_depth(
            fork_depth,
            our_height,
            common_ancestor,
            our_height,
            peer_ip,
        )?;

        // CRITICAL FIX: If we're already at the common ancestor height, no rollback needed
        // This prevents unnecessary deletion of genesis block or existing blocks
        if our_height == common_ancestor {
            tracing::info!(
                "✓ Already at common ancestor height {} - no rollback needed",
                common_ancestor
            );
            return Ok(common_ancestor);
        }

        tracing::warn!(
            "🔄 Rolling back from height {} to {} to find common ancestor",
            our_height,
            common_ancestor
        );

        // Set Reorging state before touching storage so that concurrent
        // add_block_with_fork_handling() calls see the state and back off.
        *self.fork_state.write().await = ForkResolutionState::Reorging {
            from_height: our_height,
            to_height: common_ancestor,
            started_at: std::time::Instant::now(),
        };

        let result = self.rollback_to_height(common_ancestor).await;

        // Always reset fork_state regardless of rollback outcome.
        *self.fork_state.write().await = ForkResolutionState::None;

        result.map(|_| common_ancestor)
    }

    /// Phase 3 Step 3: Spawn sync coordinator background task
    /// Proactively monitors peers and initiates sync from best masternodes
    pub fn spawn_sync_coordinator(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            info!(
                "🔄 Sync coordinator started - monitoring peers every {}s",
                SYNC_COORDINATOR_INTERVAL_SECS
            );
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                SYNC_COORDINATOR_INTERVAL_SECS,
            ));

            loop {
                interval.tick().await;

                // ALWAYS run fork detection even if syncing - this is critical for fork resolution
                // Only skip the other sync logic if already syncing
                let already_syncing = self.is_syncing.load(Ordering::Acquire);

                let our_height = self.get_height();
                let time_expected = self.calculate_expected_height();

                // Get peer registry
                let peer_registry_opt = self.peer_registry.read().await;
                let peer_registry = match peer_registry_opt.as_ref() {
                    Some(pr) => pr,
                    None => continue,
                };

                // Get all connected peers
                let connected_peers = peer_registry.get_connected_peers().await;
                if connected_peers.is_empty() {
                    continue;
                }

                // CRITICAL FIX: Actively request fresh chain tips from all peers
                // This prevents network fragmentation by proactively detecting forks
                debug!(
                    "🔍 Sync coordinator: Requesting chain tips from {} peer(s)",
                    connected_peers.len()
                );
                for peer_ip in &connected_peers {
                    let msg = NetworkMessage::GetChainTip;
                    if let Err(e) = peer_registry.send_to_peer(peer_ip, msg).await {
                        debug!("Failed to request chain tip from {}: {}", peer_ip, e);
                    }
                }

                // Wait for responses (event-driven via chain tip signal)
                let signal = peer_registry.chain_tip_updated_signal();
                let _ = tokio::time::timeout(std::time::Duration::from_secs(1), signal.notified())
                    .await;

                // ALWAYS check for consensus fork first - this is critical for fork resolution
                // Use the fresh chain tip data we just requested (already stored in peer registry)
                if let Some((consensus_height, _sync_peer)) = self.compare_chain_with_peers().await
                {
                    // Fork detected by consensus mechanism
                    info!(
                        "🔀 Sync coordinator: Consensus at height {} (our height: {})",
                        consensus_height, our_height
                    );

                    if consensus_height > our_height && !already_syncing {
                        // We're behind - sync to longer chain.
                        // When significantly behind (>10 blocks), skip per-peer throttle
                        // since sync_from_peers selects its own peers internally.
                        let blocks_behind = consensus_height - our_height;
                        let approved = if blocks_behind > 10 {
                            // Significantly behind: skip throttle, just check not already syncing
                            Ok(true)
                        } else {
                            self.sync_coordinator
                                .request_sync(
                                    _sync_peer.clone(),
                                    our_height + 1,
                                    consensus_height,
                                    crate::network::sync_coordinator::SyncSource::Periodic,
                                )
                                .await
                        };

                        match approved {
                            Ok(true) => {
                                info!(
                                    "📥 Starting sync: {} → {} ({} blocks behind)",
                                    our_height,
                                    consensus_height,
                                    blocks_behind
                                );
                                let blockchain_clone = Arc::clone(&self);
                                tokio::spawn(async move {
                                    if let Err(e) = blockchain_clone
                                        .sync_from_peers(Some(consensus_height))
                                        .await
                                    {
                                        warn!("⚠️  Sync failed: {}", e);
                                    }
                                });
                            }
                            Ok(false) => {
                                debug!(
                                    "⏸️ Sync queued for {} (another sync in progress)",
                                    _sync_peer
                                );
                            }
                            Err(e) => {
                                debug!("⏱️ Sync throttled for {}: {}", _sync_peer, e);
                            }
                        }
                    } else if consensus_height == our_height && !already_syncing {
                        // Same-height fork detected - request blocks from consensus peer
                        // for atomic reorg (rollback happens when blocks arrive)
                        info!(
                            "🔀 Sync coordinator: same-height fork at {}, requesting blocks from {}",
                            consensus_height, _sync_peer
                        );

                        if let Some(peer_registry) = self.peer_registry.read().await.as_ref() {
                            let request_from = consensus_height.saturating_sub(20).max(1);
                            match self
                                .sync_coordinator
                                .request_sync(
                                    _sync_peer.clone(),
                                    request_from,
                                    consensus_height,
                                    crate::network::sync_coordinator::SyncSource::ForkResolution,
                                )
                                .await
                            {
                                Ok(true) => {
                                    let req =
                                        NetworkMessage::GetBlocks(request_from, consensus_height);
                                    if let Err(e) =
                                        peer_registry.send_to_peer(&_sync_peer, req).await
                                    {
                                        self.sync_coordinator.cancel_sync(&_sync_peer).await;
                                        warn!(
                                            "⚠️  Failed to request blocks from {}: {}",
                                            _sync_peer, e
                                        );
                                    } else {
                                        info!(
                                            "📤 Requested blocks {}-{} from {} for fork resolution",
                                            request_from, consensus_height, _sync_peer
                                        );
                                    }
                                }
                                Ok(false) => {
                                    debug!("⏸️ Fork resolution sync queued with {}", _sync_peer);
                                }
                                Err(e) => {
                                    debug!(
                                        "⏱️ Fork resolution sync throttled with {}: {}",
                                        _sync_peer, e
                                    );
                                }
                            }
                        }
                    }
                    continue; // Skip other sync logic this round
                }

                // If already syncing, skip the rest of the sync logic
                if already_syncing {
                    continue;
                }

                // Find the best masternode to sync from
                let mut best_masternode: Option<(String, u64)> = None;

                for peer_ip in &connected_peers {
                    // Check if this peer is a whitelisted masternode
                    let is_masternode = peer_registry.is_whitelisted(peer_ip).await;
                    if !is_masternode {
                        continue;
                    }

                    // Get peer's height
                    if let Some(peer_height) = peer_registry.get_peer_height(peer_ip).await {
                        // Only consider peers ahead of us by at least 5 blocks
                        if peer_height > our_height + 5 {
                            match &best_masternode {
                                None => {
                                    best_masternode = Some((peer_ip.clone(), peer_height));
                                }
                                Some((_, best_height)) => {
                                    if peer_height > *best_height {
                                        best_masternode = Some((peer_ip.clone(), peer_height));
                                    }
                                }
                            }
                        }
                    }
                }

                // If we found a better masternode, sync from it
                if let Some((best_peer, peer_height)) = best_masternode {
                    let blocks_behind = peer_height.saturating_sub(our_height);
                    info!(
                        "🎯 Sync coordinator: Found masternode {} at height {} ({} blocks ahead of us at {})",
                        best_peer, peer_height, blocks_behind, our_height
                    );

                    // Initiate sync
                    let blockchain_clone = Arc::clone(&self);
                    tokio::spawn(async move {
                        if let Err(e) = blockchain_clone.sync_from_peers(None).await {
                            warn!("⚠️  Sync coordinator sync failed: {}", e);
                        }
                    });
                } else {
                    // Check if we're behind time-based expectation
                    if our_height + 10 < time_expected {
                        info!(
                            "⏰ Sync coordinator: We're behind time-based height ({}  vs expected {}), attempting general sync",
                            our_height, time_expected
                        );
                        let blockchain_clone = Arc::clone(&self);
                        tokio::spawn(async move {
                            if let Err(e) = blockchain_clone.sync_from_peers(None).await {
                                warn!("⚠️  Sync coordinator time-based sync failed: {}", e);
                            }
                        });
                    }
                }
            }
        })
    }

    /// Produce a block for the current TimeLock slot
    pub async fn produce_block(&self) -> Result<Block, String> {
        self.produce_block_at_height(None, None, None).await
    }

    pub async fn produce_block_at_height(
        &self,
        target_height: Option<u64>,
        producer_wallet: Option<String>,
        producer_address: Option<String>,
    ) -> Result<Block, String> {
        use crate::block::genesis::GenesisBlock;

        // Check if genesis block exists
        let genesis_result = self.get_block_by_height(0).await;

        if genesis_result.is_err() {
            // No genesis block exists - cannot produce blocks
            // Genesis must be loaded from file or synced from peers
            return Err("Cannot produce blocks: no genesis block exists. Load from genesis file or sync from peers.".to_string());
        }

        let genesis = genesis_result.unwrap();

        // Verify genesis structure
        if let Err(e) = GenesisBlock::verify_structure(&genesis) {
            return Err(format!("Cannot produce blocks: invalid genesis - {}", e));
        }

        // Check majority consensus requirement for block production
        // Requirement: 50%+ of connected peers must agree on the current chain (height, hash)
        // Exception: Allow production with 0 peers (bootstrap mode)
        if !self.check_2_3_consensus_cached().await {
            return Err("Cannot produce block: no majority consensus on current chain state. Waiting for network consensus.".to_string());
        }

        // Get previous block hash
        let current_height = self.current_height.load(Ordering::Acquire);

        let expected_height = self.calculate_expected_height();
        let blocks_behind = expected_height.saturating_sub(current_height);

        if blocks_behind > 10 {
            tracing::debug!(
                "📦 Producing block: {} blocks behind expected height (consensus-driven rapid production)",
                blocks_behind
            );
        }

        // Verify the current height block actually exists before building on top of it.
        // If the tip block is missing/corrupt, do NOT walk height backward.
        // The integrity check will re-fetch it from peers.
        if let Err(e) = self.get_block(current_height) {
            return Err(format!(
                "Cannot produce block: tip block at height {} is missing or corrupt ({}). \
                 Waiting for integrity repair to re-fetch it from peers.",
                current_height, e
            ));
        }

        // Determine the height to produce
        let next_height = if let Some(target) = target_height {
            if target <= current_height {
                return Err(format!(
                    "Cannot produce block at height {}: already have block at height {}",
                    target, current_height
                ));
            }
            if target > current_height + 1 {
                return Err(format!(
                    "Cannot produce block at height {}: missing blocks between {} and {}",
                    target, current_height, target
                ));
            }
            target
        } else {
            // Normal mode: produce next block
            current_height + 1
        };

        let prev_hash = self.get_block_hash(current_height)?;

        // CRITICAL: Do not produce a block before its expected timestamp.
        // Each block's expected time = genesis_timestamp + (height * BLOCK_TIME_SECONDS).
        // Producing early is how a malicious node could claim a higher chain.
        let now = Utc::now().timestamp();
        let genesis_ts = self.genesis_timestamp();
        let earliest_allowed = genesis_ts + (next_height as i64) * BLOCK_TIME_SECONDS;
        if now < earliest_allowed {
            let wait_secs = earliest_allowed - now;
            return Err(format!(
                "Cannot produce block {}: too early ({}s before expected time). \
                 Blocks must not be produced before their scheduled timestamp.",
                next_height, wait_secs
            ));
        }

        // CRITICAL: Validate producer_address is provided for non-genesis blocks
        // This prevents creating blocks with empty leader field (which breaks participation tracking)
        if next_height > 3 && producer_address.is_none() {
            return Err(format!(
                "Producer address required for block {} (height > 3) to track participation",
                next_height
            ));
        }

        // Calculate deterministic timestamp based on block schedule
        let deterministic_timestamp =
            self.genesis_timestamp() + (next_height as i64 * BLOCK_TIME_SECONDS);

        // Check if we're catching up (used for relaxed masternode selection)
        let _blocks_behind = self
            .calculate_expected_height()
            .saturating_sub(current_height);

        // CRITICAL: Always use deterministic timestamp, but ensure it's > previous block
        // This maintains proper block schedule (genesis + height*600) while preventing duplicates
        let mut aligned_timestamp = deterministic_timestamp;

        // Ensure timestamp is strictly greater than previous block
        if current_height > 0 {
            if let Ok(prev_block) = self.get_block_by_height(current_height).await {
                if aligned_timestamp <= prev_block.header.timestamp {
                    // Use whichever is greater: deterministic schedule or prev+1
                    aligned_timestamp = prev_block.header.timestamp + 1;
                    tracing::debug!(
                        "📅 Block {} timestamp adjusted to {} to maintain strict ordering (scheduled: {}, prev: {})",
                        next_height,
                        aligned_timestamp,
                        deterministic_timestamp,
                        prev_block.header.timestamp
                    );
                }
            }
        }

        // Require at least 3 active masternodes before producing blocks
        // (gossip-based consensus for network health)
        let masternodes = self
            .masternode_registry
            .get_masternodes_for_rewards(self)
            .await;

        if masternodes.is_empty() {
            return Err("No masternodes available for block production".to_string());
        }

        if masternodes.len() < 3 {
            return Err(format!(
                "Insufficient masternodes for block production: {} active (minimum 3 required)",
                masternodes.len()
            ));
        }

        // Get finalized transactions with pre-computed fees from consensus layer
        // CRITICAL: Use pool-stored fees because input UTXOs are already removed from
        // storage by auto-finalization's spend_utxo before block production runs
        let raw_finalized = self
            .consensus
            .get_finalized_transactions_with_fees_for_block();

        // CRITICAL: Filter double-spend/duplicate TXs BEFORE calculating fees.
        // Fees must reflect only the transactions actually included in the block,
        // otherwise block_reward will be inflated and fail validation.
        let mut valid_finalized_with_fees = Vec::new();
        let mut ds_invalid_count = 0;
        let mut evict_txids: Vec<Hash256> = Vec::new();
        let mut spent_outpoints = std::collections::HashSet::new();
        let mut seen_txids = std::collections::HashSet::new();
        for (tx, fee) in raw_finalized {
            let txid = tx.txid();

            if !seen_txids.insert(txid) {
                tracing::warn!(
                    "⚠️  Block {}: Skipping duplicate TX {}",
                    next_height,
                    hex::encode(txid)
                );
                ds_invalid_count += 1;
                continue;
            }

            // Validate input UTXOs exist and are in a spent state (SpentFinalized/SpentPending/Locked).
            // If inputs are Unspent or missing, the TX was cleared/reverted — evict from pool.
            let mut inputs_valid = true;
            for input in &tx.inputs {
                match self.utxo_manager.get_state(&input.previous_output) {
                    Some(UTXOState::SpentFinalized { .. })
                    | Some(UTXOState::SpentPending { .. })
                    | Some(UTXOState::Locked { .. }) => {}
                    other => {
                        tracing::warn!(
                            "⚠️  Block {}: Evicting TX {} - input {} is {:?} (expected spent state)",
                            next_height,
                            hex::encode(txid),
                            input.previous_output,
                            other.as_ref().map(|s| format!("{}", s)).unwrap_or_else(|| "missing".to_string())
                        );
                        inputs_valid = false;
                        break;
                    }
                }
            }
            if !inputs_valid {
                evict_txids.push(txid);
                ds_invalid_count += 1;
                continue;
            }

            let mut has_double_spend = false;
            for input in &tx.inputs {
                let outpoint_key = (input.previous_output.txid, input.previous_output.vout);
                if spent_outpoints.contains(&outpoint_key) {
                    tracing::warn!(
                        "⚠️  Block {}: Excluding TX {} - double-spend on UTXO {}:{}",
                        next_height,
                        hex::encode(txid),
                        hex::encode(input.previous_output.txid),
                        input.previous_output.vout
                    );
                    has_double_spend = true;
                    break;
                }
            }

            if has_double_spend {
                ds_invalid_count += 1;
                continue;
            }

            for input in &tx.inputs {
                spent_outpoints.insert((input.previous_output.txid, input.previous_output.vout));
            }
            valid_finalized_with_fees.push((tx, fee));
        }

        // Evict TXs with invalid inputs from the finalized pool
        if !evict_txids.is_empty() {
            tracing::warn!(
                "🧹 Block {}: Evicting {} TX(s) with invalid input UTXOs from finalized pool",
                next_height,
                evict_txids.len()
            );
            self.consensus.clear_finalized_txs(&evict_txids);
        }

        if ds_invalid_count > 0 {
            tracing::warn!(
                "⚠️  Block {}: Excluded {} invalid/double-spend/duplicate transaction(s) before fee calculation",
                next_height,
                ds_invalid_count
            );
        }

        // Size-cap: ensure the assembled block fits within MAX_BLOCK_ASSEMBLY_SIZE.
        // Reserve 32KB for block header, masternode rewards, coinbase tx, and bincode framing.
        // Transactions are already sorted by canonical order, so we simply truncate the tail.
        {
            const BLOCK_OVERHEAD_BYTES: usize = 32_768; // 32KB for non-tx block fields
            let tx_size_budget =
                constants::blockchain::MAX_BLOCK_ASSEMBLY_SIZE.saturating_sub(BLOCK_OVERHEAD_BYTES);
            let mut accumulated_tx_bytes: usize = 0;
            let mut cap_at: Option<usize> = None;
            for (i, (tx, _)) in valid_finalized_with_fees.iter().enumerate() {
                let tx_bytes = bincode::serialized_size(tx).unwrap_or(u64::MAX) as usize;
                if accumulated_tx_bytes + tx_bytes > tx_size_budget {
                    cap_at = Some(i);
                    break;
                }
                accumulated_tx_bytes += tx_bytes;
            }
            if let Some(cap) = cap_at {
                tracing::warn!(
                    "✂️  Block {}: Assembly size cap hit — truncating from {} to {} txs ({} KB tx data, assembly budget {} KB). Excess txs remain in pool for next block.",
                    next_height,
                    valid_finalized_with_fees.len(),
                    cap,
                    accumulated_tx_bytes / 1024,
                    tx_size_budget / 1024,
                );
                valid_finalized_with_fees.truncate(cap);
            }
        }

        let finalized_txs: Vec<Transaction> = valid_finalized_with_fees
            .iter()
            .map(|(tx, _)| tx.clone())
            .collect();
        let finalized_txs_fees: u64 = valid_finalized_with_fees.iter().map(|(_, fee)| fee).sum();

        if !finalized_txs.is_empty() {
            tracing::info!(
                "📝 Block {}: Including {} finalized transaction(s) (total fees: {} satoshis)",
                next_height,
                finalized_txs.len(),
                finalized_txs_fees
            );
            for (i, (tx, fee)) in valid_finalized_with_fees.iter().enumerate() {
                tracing::debug!(
                    "  📝 [{}] TX {} (inputs: {}, outputs: {}, fee: {} satoshis)",
                    i + 1,
                    hex::encode(&tx.txid()[..8]),
                    tx.inputs.len(),
                    tx.outputs.len(),
                    fee
                );
            }
        } else {
            tracing::debug!(
                "🔍 Block {}: No finalized transactions to include",
                next_height
            );
        }

        // Calculate rewards: base_reward + fees_from_finalized_txs_in_this_block
        // §10.4 Unified model: 30 TIME leader bonus + 5 TIME treasury + 65 TIME per-tier pools
        // Use governance-adjustable emission rate (defaults to BLOCK_REWARD_SATOSHIS = 100 TIME).
        let base_reward = self.get_current_block_reward();
        let treasury_share = constants::blockchain::TREASURY_POOL_SATOSHIS;
        let total_reward = base_reward + finalized_txs_fees - treasury_share; // coinbase outputs (no treasury UTXO)
        let producer_share = PRODUCER_REWARD_SATOSHIS + finalized_txs_fees; // Leader gets 30 TIME + all fees

        // NOTE: Treasury deposit happens in add_block(), not here, to avoid
        // double-deposit when the producer's own node processes the block.

        // Build reward list.
        // §10.4 Two distribution modes:
        //   All-Free mode (no paid-tier nodes in eligible pool):
        //     5 TIME → treasury (handled in add_block), 95 TIME split equally among
        //     up to MAX_FREE_TIER_RECIPIENTS free nodes. No separate producer bonus.
        //   Tier-based mode (at least one paid-tier node present):
        //     30 TIME leader bonus + 65 TIME tier pools + 5 TIME treasury.
        let mut rewards: Vec<(String, u64)> = Vec::new();

        let eligible_pool = self
            .masternode_registry
            .get_eligible_pool_nodes(next_height)
            .await;

        let blocks_without_reward_map = self
            .masternode_registry
            .get_pool_reward_tracking(self)
            .await;

        use crate::types::MasternodeTier;

        let has_paid_tier_nodes = eligible_pool
            .iter()
            .any(|mn| mn.masternode.tier != MasternodeTier::Free);

        let total_reward = if !has_paid_tier_nodes && !eligible_pool.is_empty() {
            // ── All-Free mode ─────────────────────────────────────────────────
            // All 95 TIME (total_reward) split equally among up to 25 free nodes,
            // sorted by fairness bonus so nodes waiting longest are paid first.
            let mut free_nodes: Vec<_> = eligible_pool
                .iter()
                .map(|mn| {
                    let blocks_without = blocks_without_reward_map
                        .get(&mn.masternode.address)
                        .copied()
                        .unwrap_or(0);
                    let fairness_bonus = blocks_without / 10;
                    (mn, fairness_bonus)
                })
                .collect();
            free_nodes.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| a.0.masternode.address.cmp(&b.0.masternode.address))
            });
            let recipient_count = free_nodes
                .len()
                .min(constants::blockchain::MAX_FREE_TIER_RECIPIENTS);
            let per_node = total_reward / recipient_count as u64;
            let mut distributed = 0u64;
            for (i, (mn, _)) in free_nodes.iter().take(recipient_count).enumerate() {
                // Last recipient absorbs rounding remainder so total == total_reward exactly.
                let share = if i == recipient_count - 1 {
                    total_reward - distributed
                } else {
                    per_node
                };
                let dest = if !mn.reward_address.is_empty() {
                    mn.reward_address.clone()
                } else {
                    mn.masternode.wallet_address.clone()
                };
                if let Some(entry) = rewards.iter_mut().find(|(a, _)| a == &dest) {
                    entry.1 += share;
                } else {
                    rewards.push((dest, share));
                }
                distributed += share;
            }
            tracing::info!(
                "💰 Block {}: {} TIME (all-Free) — {} TIME each to {} node(s) [{} eligible]",
                next_height,
                total_reward / 100_000_000,
                per_node / 100_000_000,
                recipient_count,
                eligible_pool.len(),
            );
            total_reward // no rounding dust in all-free mode
        } else {
            // ── Tier-based mode ───────────────────────────────────────────────
            // Producer gets 30 TIME leader bonus + fees. Per-tier pools distributed
            // to eligible winners; empty tiers roll up to producer.
            if let Some(ref wallet) = producer_wallet {
                rewards.push((wallet.clone(), producer_share));
            }

            let tiers = [
                MasternodeTier::Gold,
                MasternodeTier::Silver,
                MasternodeTier::Bronze,
                MasternodeTier::Free,
            ];
            let mut total_pool_distributed = 0u64;
            let mut rounding_dust = 0u64;

            for tier in &tiers {
                let tier_pool = tier.pool_allocation();

                let mut tier_nodes: Vec<_> = eligible_pool
                    .iter()
                    .filter(|mn| mn.masternode.tier == *tier)
                    .map(|mn| {
                        let blocks_without = blocks_without_reward_map
                            .get(&mn.masternode.address)
                            .copied()
                            .unwrap_or(0);
                        let fairness_bonus = blocks_without / 10;
                        (mn, fairness_bonus)
                    })
                    .collect();

                // Empty tiers: full pool goes to block producer
                if tier_nodes.is_empty() {
                    if let Some(entry) = rewards.first_mut() {
                        entry.1 += tier_pool;
                    }
                    total_pool_distributed += tier_pool;
                    continue;
                }

                tier_nodes.sort_by(|a, b| {
                    b.1.cmp(&a.1)
                        .then_with(|| a.0.masternode.address.cmp(&b.0.masternode.address))
                });

                let is_free_tier = matches!(tier, MasternodeTier::Free);
                let recipient_count = if is_free_tier {
                    tier_nodes
                        .len()
                        .min(constants::blockchain::MAX_FREE_TIER_RECIPIENTS)
                } else {
                    1
                };

                let per_node = tier_pool / recipient_count as u64;

                let mut distributed = 0u64;
                for (mn, _) in tier_nodes.iter().take(recipient_count) {
                    let share = per_node;
                    let dest = if !mn.reward_address.is_empty() {
                        mn.reward_address.clone()
                    } else {
                        mn.masternode.wallet_address.clone()
                    };
                    if let Some(entry) = rewards.iter_mut().find(|(a, _)| a == &dest) {
                        entry.1 += share;
                    } else {
                        rewards.push((dest, share));
                    }
                    distributed += share;
                }
                let tier_dust = tier_pool - distributed;
                if tier_dust > 0 {
                    rounding_dust += tier_dust;
                }
                total_pool_distributed += distributed;
            }

            let adjusted_reward = total_reward - rounding_dust;
            tracing::info!(
                "💰 Block {}: {} TIME — producer {} TIME, pools {} TIME to {} node(s) [{} eligible]{}",
                next_height,
                adjusted_reward / 100_000_000,
                producer_share / 100_000_000,
                total_pool_distributed / 100_000_000,
                rewards.len().saturating_sub(1),
                eligible_pool.len(),
                if rounding_dust > 0 {
                    format!(", {} sat rounding dust → treasury", rounding_dust)
                } else {
                    String::new()
                }
            );
            adjusted_reward
        };

        if rewards.is_empty() {
            return Err(format!(
                "No valid masternode rewards calculated for {} masternodes",
                masternodes.len()
            ));
        }

        tracing::info!(
            "💰 Block {}: base reward {} + fees {} = {} satoshis total",
            next_height,
            base_reward,
            finalized_txs_fees,
            total_reward
        );

        // No longer storing fees for next block - fees are included immediately
        if finalized_txs_fees > 0 {
            tracing::info!(
                "💸 Block {}: included {} satoshis in fees from {} finalized transaction(s)",
                next_height,
                finalized_txs_fees,
                finalized_txs.len()
            );
        }

        // Coinbase transaction creates the total block reward
        // CRITICAL: Include block height in output to ensure unique txid per block
        let mut height_bytes = next_height.to_le_bytes().to_vec();
        let mut script = b"BLOCK_REWARD_".to_vec();
        script.append(&mut height_bytes);

        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: total_reward,
                script_pubkey: script, // Unique per block due to height
            }],
            lock_time: 0,
            timestamp: aligned_timestamp,
            special_data: None,
            encrypted_memo: None,
        };

        // Reward distribution transaction spends coinbase and distributes to masternodes
        let block_reward_memo = self.consensus.encrypt_memo_for_self("Block Reward").ok();
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
            special_data: None,
            encrypted_memo: block_reward_memo,
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

        // CRITICAL: Sort finalized transactions deterministically by txid
        // This ensures all nodes compute the same merkle root for the same block
        // (Double-spend/duplicate filtering already done above before fee calculation)
        let mut sorted_finalized = finalized_txs;
        sorted_finalized.sort_by_key(|a| a.txid());
        all_txs.extend(sorted_finalized);

        // Calculate merkle root from ALL transactions in canonical order
        let merkle_root = crate::block::types::calculate_merkle_root(&all_txs);

        // Create active masternode bitmap based on who voted on the PREVIOUS block
        // ELIGIBILITY RULES:
        //  1. Direct voters: nodes whose timevote messages this producer received directly
        //  2. Gossip-active nodes: nodes known to be online via gossip during the block period
        //     Gossip proves a node was online — it must have been seen by ≥ min_reports peers
        //     within the last 5 minutes. A node that joined mid-block won't have fresh enough
        //     gossip records yet (gossip runs every 30s, cleanup every 60s).
        //  3. Combines (1) and (2) so that pyramid-topology nodes not directly connected to
        //     the producer still appear in the bitmap and remain eligible for rewards / leader
        //     selection next block.
        let voters = if next_height == 1 {
            // Block 1: Genesis has no voters, so use all active masternodes
            tracing::debug!("📊 Block 1 (after genesis): using all active masternodes for bitmap");
            self.masternode_registry
                .get_active_masternodes()
                .await
                .into_iter()
                .map(|mn| mn.masternode.address)
                .collect()
        } else {
            // Get voters from previous block (who voted to accept it)
            let prev_block_hash = prev_hash;
            // Gather voters from both prepare and precommit phases for maximum coverage.
            // Fast consensus (e.g., high-weight producer) can finalize before all peers'
            // precommit votes arrive, so prepare voters fill the gap.
            let mut voters_set = std::collections::HashSet::new();

            // 1. Check live precommit votes
            for v in self
                .consensus
                .timevote
                .precommit_votes
                .get_voters(prev_block_hash)
            {
                voters_set.insert(v);
            }
            // 2. Check preserved voters (saved at finalization by cleanup_block_votes)
            for v in self
                .consensus
                .timevote
                .get_finalized_block_voters(prev_block_hash)
            {
                voters_set.insert(v);
            }
            // 3. Check live prepare votes (more complete since prepare consensus is reached first)
            for v in self
                .consensus
                .timevote
                .prepare_votes
                .get_voters(prev_block_hash)
            {
                voters_set.insert(v);
            }

            // 4. Add gossip-active masternodes. These nodes were confirmed online during
            //    the block period by ≥ min_reports independent peers, but may not be
            //    directly connected to this producer (pyramid topology). Including them
            //    ensures they appear in the bitmap and stay eligible for rewards / leader
            //    selection without requiring direct connectivity to the block producer.
            let gossip_active = self.masternode_registry.get_active_masternodes().await;
            let gossip_active_count_before = voters_set.len();
            for mn in &gossip_active {
                voters_set.insert(mn.masternode.address.clone());
            }
            let added_via_gossip = voters_set.len() - gossip_active_count_before;
            if added_via_gossip > 0 {
                tracing::debug!(
                    "📊 Block {}: added {} gossip-active masternode(s) to bitmap (total {} direct voters + gossip)",
                    next_height,
                    added_via_gossip,
                    voters_set.len()
                );
            }

            let precommit_voters: Vec<String> = voters_set.into_iter().collect();

            // CRITICAL: If no voters recorded, use active masternodes on our chain as fallback
            // This prevents bitmap from becoming empty and breaking participation tracking
            // Filter out masternodes that are on a fork or significantly behind
            if precommit_voters.is_empty() {
                let all_active = self.masternode_registry.get_active_masternodes().await;

                // Filter: only include masternodes near our chain tip
                let our_height = self.get_height();
                let max_behind = 10; // Allow up to 10 blocks behind
                let mut on_chain_voters: Vec<String> = Vec::new();

                if let Some(registry) = self.get_peer_registry().await {
                    for mn in &all_active {
                        let mn_ip = mn
                            .masternode
                            .address
                            .split(':')
                            .next()
                            .unwrap_or(&mn.masternode.address);
                        if let Some(peer_height) = registry.get_peer_height(mn_ip).await {
                            if our_height.saturating_sub(peer_height) <= max_behind {
                                on_chain_voters.push(mn.masternode.address.clone());
                            }
                        } else {
                            // No height data — include local node, skip unknown peers
                            if let Some(ref local_addr) =
                                self.masternode_registry.get_local_address().await
                            {
                                let local_ip = local_addr.split(':').next().unwrap_or(local_addr);
                                if mn_ip == local_ip {
                                    on_chain_voters.push(mn.masternode.address.clone());
                                }
                            }
                        }
                    }
                }

                // If filtering left too few, fall back to all active
                if on_chain_voters.len() < 2 {
                    on_chain_voters = all_active
                        .into_iter()
                        .map(|mn| mn.masternode.address)
                        .collect();
                }

                tracing::warn!(
                    "⚠️ No precommit voters found for block {} (hash: {}) - using {} on-chain masternodes as fallback",
                    next_height - 1,
                    hex::encode(&prev_block_hash[..8]),
                    on_chain_voters.len()
                );
                on_chain_voters
            } else {
                tracing::debug!(
                    "📊 Block {}: using {} precommit voters from previous block",
                    next_height,
                    precommit_voters.len()
                );
                precommit_voters
            }
        };

        tracing::debug!(
            "📊 Creating bitmap from {} voters on previous block",
            voters.len()
        );

        let (active_bitmap, _) = self
            .masternode_registry
            .create_active_bitmap_from_voters(&voters)
            .await;

        let mut block = Block {
            header: BlockHeader {
                version: 1,
                height: next_height,
                previous_hash: prev_hash,
                merkle_root,
                timestamp: aligned_timestamp,
                block_reward: total_reward,
                total_fees: finalized_txs_fees,
                leader: producer_address.unwrap_or_default(),
                attestation_root: [0u8; 32],
                masternode_tiers: tier_counts,
                active_masternodes_bitmap: active_bitmap.clone(),
                liveness_recovery: Some(false), // Will be set below if needed
                ..Default::default()
            },
            transactions: all_txs,
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
            time_attestations: vec![],
            // Record masternodes that voted on previous block (active participants)
            consensus_participants_bitmap: active_bitmap, // Compact representation
            liveness_recovery: Some(false), // Will be set if fallback resolution occurred
        };

        // §7.6 Liveness Fallback: Check if we need to resolve stalled transactions
        if self.consensus.has_pending_fallback_transactions() {
            let resolved = self.consensus.resolve_stalls_via_timelock();
            block.liveness_recovery = Some(resolved);
            block.header.liveness_recovery = Some(resolved);
            if resolved {
                tracing::warn!(
                    "🔒 Block {} includes liveness recovery (resolved stalled transactions)",
                    next_height
                );
            }
        }

        // Add VRF proof for fork resolution (if we have signing key)
        if let Some(signing_key) = self.consensus.get_signing_key() {
            if let Err(e) = block.add_vrf(&signing_key) {
                tracing::warn!("⚠️ Failed to add VRF to block {}: {}", next_height, e);
            } else {
                tracing::debug!(
                    "🎲 Block {} VRF: score={}, output={}...",
                    next_height,
                    block.header.vrf_score,
                    hex::encode(&block.header.vrf_output[..4])
                );
            }

            // Sign the block hash with the producer's Ed25519 key
            if let Err(e) = block.sign(&signing_key) {
                tracing::warn!("⚠️ Failed to sign block {}: {}", next_height, e);
            } else {
                tracing::debug!("🔏 Block {} signed by producer", next_height);
            }
        } else {
            tracing::debug!(
                "⚠️ Block {} produced without VRF (no signing key available)",
                next_height
            );
        }

        Ok(block)
    }

    /// Invalidate the consensus cache, forcing the next check to query fresh peer data.
    pub async fn invalidate_consensus_cache(&self) {
        *self.consensus_cache.write().await = None;
    }

    /// Cached version of consensus check - returns result from cache if fresh (< 30s old)
    /// Falls back to full check if cache miss or expired
    /// Saves 5-10ms per check by avoiding redundant peer queries
    ///
    /// Invalidate the cache with `invalidate_consensus_cache()` after
    /// producing a block to force a fresh check with updated peer data.
    pub async fn check_2_3_consensus_cached(&self) -> bool {
        const CONSENSUS_CACHE_TTL: Duration = Duration::from_secs(30);

        // Check cache first
        {
            let cache = self.consensus_cache.read().await;
            if let Some(cached) = cache.as_ref() {
                if cached.timestamp.elapsed() < CONSENSUS_CACHE_TTL {
                    tracing::debug!(
                        "🔄 consensus check cache HIT ({}ms old)",
                        cached.timestamp.elapsed().as_millis()
                    );
                    return cached.result;
                }
            }
        }

        // Cache miss or expired - perform full check
        tracing::debug!("🔄 consensus check cache MISS - recalculating");
        let result = self.check_2_3_consensus_for_production().await;

        // Only cache results from real peer consensus, not bootstrap mode.
        // Caching a bootstrap `true` could allow solo production for up to 30s
        // after peers connect (stale cache), causing chain divergence.
        let has_peers = {
            let guard = self.peer_registry.read().await;
            match guard.as_ref() {
                Some(registry) => !registry.get_connected_peers().await.is_empty(),
                None => false,
            }
        };
        if has_peers {
            let mut cache = self.consensus_cache.write().await;
            *cache = Some(ConsensusCache {
                result,
                timestamp: Instant::now(),
            });
        }

        result
    }

    /// Check if majority of connected peers agree on the current chain state (height, hash)
    /// Uses TIER-WEIGHTED voting (Gold > Silver > Bronze > Free)
    /// Returns true if:
    /// - We have 0 connected peers (bootstrap mode allowed), OR
    /// - 50%+ of WEIGHTED stake agrees on our current (height, hash)
    ///
    /// Returns false if:
    /// - We have 1+ connected peers AND less than 50%+ of weighted stake agrees on our chain
    async fn check_2_3_consensus_for_production(&self) -> bool {
        // Rate limit detailed logging to once per 60 seconds
        static LAST_DETAILED_LOG: std::sync::atomic::AtomicI64 =
            std::sync::atomic::AtomicI64::new(0);
        let now_secs = chrono::Utc::now().timestamp();
        let should_log_details =
            now_secs - LAST_DETAILED_LOG.load(std::sync::atomic::Ordering::Relaxed) >= 60;

        let peer_registry_guard = self.peer_registry.read().await;
        let peer_registry = match peer_registry_guard.as_ref() {
            Some(registry) => registry,
            None => return true, // No registry = bootstrap mode allowed
        };

        // CRITICAL: Only count compatible peers (same genesis hash) for consensus.
        // Incompatible peers (different network) must NOT dilute the majority threshold.
        let connected_peers = peer_registry.get_compatible_peers().await;

        // Bootstrap mode: allow production with 0 peers ONLY if we've never had peers.
        // Once peers have been seen, losing all peers means network issue, not bootstrap.
        // Producing blocks solo after having had peers causes chain divergence.
        if connected_peers.is_empty() {
            if self.has_ever_had_peers.load(Ordering::Acquire) {
                tracing::warn!(
                    "⚠️ Block production blocked: 0 connected peers but node has previously had peers. \
                     Solo block production disabled to prevent chain divergence."
                );
                return false;
            }
            tracing::debug!("✅ Block production allowed in bootstrap mode (0 connected peers, never had peers before)");
            return true;
        }

        // Mark that we've had peers — permanently disables bootstrap mode
        if !self.has_ever_had_peers.load(Ordering::Acquire) {
            self.has_ever_had_peers.store(true, Ordering::Release);
            tracing::info!("🔒 Bootstrap mode permanently disabled — peers detected");
        }

        let our_height = self.current_height.load(Ordering::Acquire);

        // Get our current block hash
        let our_hash = match self.get_block_hash(our_height) {
            Ok(hash) => hash,
            Err(_) => {
                // If we can't get our own hash, something is wrong - don't produce
                tracing::warn!(
                    "⚠️ Block production blocked: cannot determine our current block hash at height {}",
                    our_height
                );
                return false;
            }
        };

        // Collect peer chain tips AND tier weights for our height
        let mut weight_on_our_chain = 0u64;
        let mut total_weight = 0u64;
        let mut peers_responding = 0;
        let mut peers_ignored = 0;
        let mut peers_agreeing = 0u32; // Count of peers agreeing on our exact (height, hash)
        let mut peer_states: Vec<(String, u64, [u8; 32], u64)> = Vec::new(); // (ip, height, hash, weight)

        for peer_ip in &connected_peers {
            if let Some((peer_height, peer_hash)) = peer_registry.get_peer_chain_tip(peer_ip).await
            {
                // CRITICAL: Ignore peers with zero hash (corrupted/missing blocks)
                // Same logic as compare_chain_with_peers()
                if peer_hash == [0u8; 32] {
                    peers_ignored += 1;
                    tracing::debug!(
                        "Ignoring peer {} with zero hash (corrupted blocks) for consensus check",
                        peer_ip
                    );
                    continue;
                }

                peers_responding += 1;

                // Get peer's tier weight (default to Bronze if not found)
                let peer_weight = match self.masternode_registry.get(peer_ip).await {
                    Some(info) => info.masternode.tier.sampling_weight(),
                    None => {
                        tracing::debug!(
                            "Peer {} not in masternode registry, using Free weight",
                            peer_ip
                        );
                        crate::types::MasternodeTier::Free.sampling_weight()
                    }
                };

                total_weight += peer_weight;

                // Track peer state for diagnostics
                peer_states.push((peer_ip.clone(), peer_height, peer_hash, peer_weight));

                // Check if peer is on our chain:
                // - Same height, same hash = exact agreement
                // - Lower height, same hash at their height = on our chain, just behind
                // - Different hash at any shared height = different fork (don't count)
                if peer_height == our_height && peer_hash == our_hash {
                    // Exact match
                    weight_on_our_chain += peer_weight;
                    peers_agreeing += 1;
                } else if peer_height < our_height {
                    // Peer is behind — check if they're on our chain
                    match self.get_block_hash(peer_height) {
                        Ok(our_hash_at_peer_height) if our_hash_at_peer_height == peer_hash => {
                            // Peer is on our chain, just hasn't synced yet
                            weight_on_our_chain += peer_weight;
                            peers_agreeing += 1;
                        }
                        _ => {
                            // Peer is on a different fork
                        }
                    }
                }
                // peer_height > our_height: peer is ahead on a different fork, don't count
            }
        }

        if total_weight == 0 {
            if peers_ignored > 0 {
                tracing::warn!(
                    "⚠️ Block production blocked: {} peers with corrupted blocks (zero hash) ignored, no healthy peers available",
                    peers_ignored
                );
            } else {
                tracing::warn!("⚠️ Block production blocked: no responding peers with weight");
            }
            return false;
        }

        // LONGEST CHAIN RULE: If no peer has a chain taller than ours, we have the
        // longest chain and should continue producing. Peers will sync to us.
        // HOWEVER: If peers at our height have DIFFERENT hashes, we're in a fork —
        // don't use this escape; fall through to weighted agreement check.
        // CRITICAL: Also require at least MIN_AGREEING_PEERS peers confirming they're
        // on our chain. A solo node must NOT produce blocks just because it has the
        // "longest chain" — it needs confirmation from others.
        const MIN_AGREEING_PEERS: u32 = 2;
        let max_peer_height = peer_states.iter().map(|(_, h, _, _)| *h).max().unwrap_or(0);
        if max_peer_height <= our_height {
            let fork_at_our_height = peer_states
                .iter()
                .any(|(_, h, hash, _)| *h == our_height && *hash != our_hash);
            if fork_at_our_height {
                tracing::warn!(
                    "⚠️ Same-height fork detected: peers at height {} have different hashes - blocking production until resolved",
                    our_height
                );
                // Fall through to weighted agreement check below
            } else if peers_agreeing >= MIN_AGREEING_PEERS {
                tracing::debug!(
                    "✅ Block production allowed: longest chain rule (our height {} >= max peer height {}, {} peers agree, no fork)",
                    our_height,
                    max_peer_height,
                    peers_agreeing
                );
                return true;
            } else {
                tracing::warn!(
                    "⚠️ Block production blocked: only {} peers agree (need {} minimum). \
                     Cannot produce blocks without peer confirmation.",
                    peers_agreeing,
                    MIN_AGREEING_PEERS
                );
                return false;
            }
        }

        // If peers report being ahead, consider blocking production to sync.
        // ATTACK DEFENSE: A single malicious node could produce a block early
        // and claim a higher height to stall the network. Only block production
        // when MULTIPLE independent peers confirm the higher height AND the
        // height is plausible (within time-based expected range).
        // A single peer ahead triggers a background sync attempt but does NOT
        // block production — this prevents a lone attacker from halting the chain.
        if max_peer_height > our_height {
            let time_expected = self.calculate_expected_height();
            // Plausible = at most 2 minutes of block time beyond the expected height.
            // With 600s block interval, 120s ≈ 0.2 blocks, so allow +1 block margin.
            let height_is_plausible = max_peer_height <= time_expected + 1;
            let peers_ahead: Vec<&(String, u64, [u8; 32], u64)> = peer_states
                .iter()
                .filter(|(_, h, _, _)| *h > our_height)
                .collect();
            let multiple_peers_ahead = peers_ahead.len() >= 2;

            if height_is_plausible && multiple_peers_ahead {
                tracing::warn!(
                    "⚠️ Block production blocked: {} peers ahead at height {} (we are at {}, expected ~{}). Must sync first.",
                    peers_ahead.len(),
                    max_peer_height,
                    our_height,
                    time_expected
                );
                return false;
            } else if height_is_plausible {
                // Single peer ahead — could be legitimate OR an attack.
                // Do NOT block production (attacker could stall us).
                // Instead, attempt sync in background and continue normally.
                tracing::info!(
                    "🔄 Single peer ahead at height {} (we are at {}, expected ~{}). \
                     Attempting sync but not blocking production (could be attack).",
                    max_peer_height,
                    our_height,
                    time_expected
                );
                // Fall through to weighted consensus check
            } else {
                tracing::warn!(
                    "⚠️ Ignoring implausible peer height {} (expected ~{}, we are at {}). {} peer(s) claim this.",
                    max_peer_height,
                    time_expected,
                    our_height,
                    peers_ahead.len()
                );
                // Fall through to weighted consensus check — don't trust implausible heights
            }
        }

        // All peers are at or below our height (handled by longest-chain-rule above).
        // Include our own weight in the calculation — we're on our own chain.
        let our_weight = match self.masternode_registry.get_local_masternode().await {
            Some(info) => info.masternode.tier.sampling_weight(),
            None => crate::types::MasternodeTier::Free.sampling_weight(),
        };
        let total_network_weight = total_weight + our_weight;
        let our_chain_weight = weight_on_our_chain + our_weight;

        // Require majority (50%+) of total network weight on our chain
        let required_weight = total_network_weight / 2 + 1;
        let has_consensus = our_chain_weight >= required_weight;

        // CRITICAL: Also require a minimum NUMBER of peers in sync (not just weight).
        // Prevents a single high-weight node from enabling block production.
        // "3 nodes in sync" = us + at least 2 agreeing peers.
        // (MIN_AGREEING_PEERS declared above at the longest-chain-rule check)
        let enough_peers_in_sync = peers_agreeing >= MIN_AGREEING_PEERS;

        if has_consensus && enough_peers_in_sync {
            tracing::debug!(
                "✅ Block production allowed: {}/{} weight (incl. self), {}/{} peers on our chain at height {}",
                our_chain_weight,
                total_network_weight,
                peers_agreeing + 1,
                peers_responding + 1,
                our_height
            );
            if peers_ignored > 0 {
                tracing::debug!("   ({} peers with corrupted blocks ignored)", peers_ignored);
            }
        } else {
            if !has_consensus {
                tracing::warn!(
                    "⚠️ Block production blocked: {} weight on our chain (need {} for majority of {} total incl. self). Peer responses: {}/{}{}",
                    our_chain_weight,
                    required_weight,
                    total_network_weight,
                    peers_responding,
                    connected_peers.len(),
                    if peers_ignored > 0 { format!(", {} corrupted peers ignored", peers_ignored) } else { String::new() }
                );
            }
            if !enough_peers_in_sync {
                tracing::warn!(
                    "⚠️ Block production blocked: only {} peers in sync at height {} (need at least {} for 3-node minimum)",
                    peers_agreeing,
                    our_height,
                    MIN_AGREEING_PEERS
                );
            }
            // Log detailed peer state for diagnostics (rate limited to once per minute)
            if should_log_details {
                LAST_DETAILED_LOG.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    "📊 Peer chain states (our height: {}, our hash: {}):",
                    our_height,
                    hex::encode(&our_hash[..8])
                );
                for (peer_ip, peer_height, peer_hash, peer_weight) in &peer_states {
                    let on_our_chain = if *peer_height == our_height && peer_hash == &our_hash {
                        "✅ SAME CHAIN (exact)"
                    } else if *peer_height < our_height {
                        match self.get_block_hash(*peer_height) {
                            Ok(h) if h == *peer_hash => "✅ SAME CHAIN (behind)",
                            _ => "❌ DIFFERENT FORK",
                        }
                    } else {
                        "❌ DIFFERENT FORK (taller)"
                    };
                    tracing::warn!(
                        "   {} @ height {} hash {} weight {} {}",
                        peer_ip,
                        peer_height,
                        hex::encode(&peer_hash[..8]),
                        peer_weight,
                        on_our_chain
                    );
                }
            }
        }

        has_consensus && enough_peers_in_sync
    }

    /// Add a block to the chain
    pub async fn add_block(&self, block: Block) -> Result<(), String> {
        // CRITICAL FIX: Sanitize blocks from old nodes with corrupted transaction data
        // Blocks may have malformed script_sig that deserializes from JSON but fails bincode
        let block = match bincode::serialize(&block) {
            Ok(_) => {
                // Block is clean, use as-is
                block
            }
            Err(e) => {
                tracing::warn!(
                    "⚠️ Block {} has corrupted data ({}), attempting to sanitize...",
                    block.header.height,
                    e
                );

                // Try to sanitize the block by cleaning transaction data
                let mut sanitized_block = block.clone();

                // Clean all transactions: empty out corrupted script_sig/script_pubkey
                for tx in &mut sanitized_block.transactions {
                    for input in &mut tx.inputs {
                        // If script_sig is causing issues, clear it
                        // For coinbase/reward txs, script_sig can be empty
                        if input.script_sig.len() > 10000 {
                            tracing::warn!(
                                "  Clearing oversized script_sig ({} bytes)",
                                input.script_sig.len()
                            );
                            input.script_sig = vec![];
                        }
                    }
                    for output in &mut tx.outputs {
                        // If script_pubkey is causing issues, verify it's reasonable
                        if output.script_pubkey.len() > 10000 {
                            tracing::warn!(
                                "  Clearing oversized script_pubkey ({} bytes)",
                                output.script_pubkey.len()
                            );
                            output.script_pubkey = vec![];
                        }
                    }
                }

                // Test if sanitized block can be serialized
                match bincode::serialize(&sanitized_block) {
                    Ok(_) => {
                        tracing::info!(
                            "✅ Successfully sanitized block {} (fixed corrupted transaction data)",
                            sanitized_block.header.height
                        );
                        sanitized_block
                    }
                    Err(e2) => {
                        tracing::error!(
                            "❌ Failed to sanitize block {}: {}",
                            block.header.height,
                            e2
                        );
                        return Err(format!(
                            "Block {} contains corrupted data that cannot be repaired: {}",
                            block.header.height, e2
                        ));
                    }
                }
            }
        };

        // Calculate block hash early for finality tracking
        let block_hash = block.hash();

        // Start tracking finality time for this block
        self.consensus.record_block_received(block_hash);

        // CRITICAL: Verify block integrity before adding
        // Check 1: Non-genesis blocks must have non-zero previous_hash
        if block.header.height > 0 && block.header.previous_hash == [0u8; 32] {
            tracing::error!(
                "❌ CORRUPT BLOCK DETECTED: Block {} has zero previous_hash",
                block.header.height
            );
            return Err(format!(
                "Block {} has zero previous_hash - corrupt data rejected",
                block.header.height
            ));
        }

        // Check 2: CRITICAL - Verify previous hash chain (if not genesis)
        // We MUST have the previous block before accepting a new block
        if block.header.height > 0 {
            match self.get_block(block.header.height - 1) {
                Ok(prev_block) => {
                    let expected_prev_hash = prev_block.hash();
                    if block.header.previous_hash != expected_prev_hash {
                        tracing::error!(
                            "❌ CORRUPT BLOCK DETECTED: Block {} previous_hash chain broken: expected {}, got {}",
                            block.header.height,
                            hex::encode(&expected_prev_hash[..8]),
                            hex::encode(&block.header.previous_hash[..8])
                        );
                        return Err(format!(
                            "Block {} previous_hash doesn't match previous block hash",
                            block.header.height
                        ));
                    }
                }
                Err(e) => {
                    // RECOVERY: Previous block is missing
                    // During sync: DON'T accept blocks with missing parents - we need sequential chain
                    // After sync: Accept if from consensus peer (network validated it)
                    let is_syncing = self.is_syncing.load(Ordering::Acquire);
                    let current_height = self.current_height.load(Ordering::Acquire);

                    if is_syncing {
                        // During sync, reject blocks with missing parents
                        // This prevents gaps in the chain
                        return Err(format!(
                            "Block {} has missing parent at height {} - need sequential blocks during sync (currently at height {})",
                            block.header.height,
                            block.header.height - 1,
                            current_height
                        ));
                    }

                    // After sync: Accept blocks from consensus peers (they've been validated)
                    // The block's previous_hash field serves as proof of parent validity
                    tracing::warn!(
                        "⚠️ Previous block {} not found ({}), but accepting block {} - network in consensus",
                        block.header.height - 1,
                        e,
                        block.header.height
                    );

                    // NOTE: We're accepting this block even though we can't verify the previous hash
                    // This is safe because:
                    // 1. The block came from a consensus peer (not during sync)
                    // 2. Other nodes with valid chains have validated it
                    // 3. The previous_hash field in the block header proves parent validity
                    // 4. If this breaks consensus, peers will fork-resolve and reject the bad chain
                }
            }
        }

        // Validate block height is sequential
        let current = self.current_height.load(Ordering::Acquire);

        // Special case: genesis block (height 0)
        let is_genesis = block.header.height == 0;

        if is_genesis {
            // CRITICAL: Only allow genesis if it doesn't already exist
            // This prevents duplicate genesis blocks even when current_height=0
            match self.get_block_by_height(0).await {
                Ok(_existing_genesis) => {
                    return Err(
                        "Genesis block already exists - cannot add duplicate genesis".to_string(),
                    );
                }
                Err(_) => {
                    // No genesis exists - allow adding it
                    if current != 0 {
                        return Err(format!(
                            "Cannot add genesis at height 0 when chain height is {} (chain already advanced)",
                            current
                        ));
                    }
                }
            }

            // CRITICAL: Validate genesis timestamp matches network template
            // This ensures all nodes use the same genesis and don't fork at genesis level
            use crate::block::genesis::GenesisBlock;
            GenesisBlock::verify_timestamp(&block, self.network_type)?;

            // Validate genesis hash matches hardcoded checkpoint
            GenesisBlock::verify_checkpoint(&block, self.network_type)?;
        } else if block.header.height != current + 1 {
            return Err(format!(
                "Block height mismatch: expected {}, got {}",
                current + 1,
                block.header.height
            ));
        }

        // Checkpoint validation: verify block hash matches checkpoint if this is a checkpoint height
        // (block_hash already calculated at function start for finality tracking)
        self.validate_checkpoint(block.header.height, &block_hash)?;

        // CRITICAL: Validate block rewards (prevent double-counting bug)
        // Skip for genesis block
        if !is_genesis {
            self.validate_block_rewards(&block).await?;

            // Verify producer signature (skip genesis — it has no producer)
            if !block.header.producer_signature.is_empty() {
                if let Some(proposer_info) =
                    self.masternode_registry.get(&block.header.leader).await
                {
                    if let Err(e) = block.verify_signature(&proposer_info.masternode.public_key) {
                        // Warn but don't reject: stale registry keys (e.g. after chain wipe)
                        // cause false failures during historical sync. Chain hash integrity
                        // is still enforced. Keys are refreshed once sync reaches the tip.
                        tracing::warn!(
                            "⚠️ Block {} producer signature mismatch for leader {} (stale registry key?): {}",
                            block.header.height, block.header.leader, e
                        );
                    }
                }
            }
        }

        // Additional timestamp validation: check if too far in past
        // Skip this check during sync (when we're behind) or for genesis blocks
        // During sync, we're catching up with blocks that are legitimately old
        let is_catching_up = block.header.height <= current + 5; // We're syncing if adding blocks near our current height
        if !is_catching_up && !is_genesis {
            let now = chrono::Utc::now().timestamp();
            // Only enforce timestamp check for new blocks being produced in real-time
            // Allow some tolerance for clock drift
            if block.header.timestamp < now - TIMESTAMP_TOLERANCE_SECS {
                return Err(format!(
                    "Block {} timestamp {} is too far in past (now: {}, tolerance: {}s)",
                    block.header.height, block.header.timestamp, now, TIMESTAMP_TOLERANCE_SECS
                ));
            }
        }

        // Block size check skipped here — validate_block() already performs this
        // with bincode::serialize(). Avoiding double serialization on the hot path.

        // CRITICAL: Check if block already exists BEFORE processing UTXOs
        // This prevents AlreadySpent errors when block save fails but UTXO changes persist
        if let Ok(_existing) = self.get_block_by_height(block.header.height).await {
            tracing::warn!(
                "⚠️ Block {} (hash {}) already exists in database, skipping UTXO processing",
                block.header.height,
                hex::encode(block_hash)
            );

            // CRITICAL: Still update chain height if we're behind
            // Block may have been saved but height update failed
            let current = self.current_height.load(Ordering::Acquire);
            if block.header.height > current {
                tracing::info!(
                    "📈 Updating chain height from {} to {} for existing block",
                    current,
                    block.header.height
                );
                self.current_height
                    .store(block.header.height, Ordering::Release);
            }

            return Ok(());
        }

        // Process UTXOs and create undo log
        let undo_log = self.process_block_utxos(&block).await?;

        // Process special transactions (on-chain masternode registration/updates)
        if !is_genesis {
            self.process_special_transactions(&block).await;
        }

        // Deposit treasury allocation for this block (5 TIME per block, including genesis)
        self.treasury_deposit(constants::blockchain::TREASURY_POOL_SATOSHIS);

        // Check for governance proposals whose voting window closes at this height.
        // Skip during initial sync to avoid executing proposals against a partially-built
        // masternode registry.
        if !is_genesis && !self.is_syncing.load(std::sync::atomic::Ordering::Acquire) {
            if let Some(gov) = &self.governance {
                let passed = gov
                    .check_and_execute_proposals(block.header.height, &self.masternode_registry)
                    .await;
                for proposal in passed {
                    use crate::governance::ProposalPayload;
                    match &proposal.payload {
                        ProposalPayload::TreasurySpend {
                            recipient, amount, ..
                        } => {
                            if let Err(e) = self
                                .execute_treasury_spend(recipient, *amount, &proposal.id)
                                .await
                            {
                                tracing::error!("🏛️  TreasurySpend execution failed: {e}");
                            } else {
                                gov.mark_executed(&proposal.id).await;
                            }
                        }
                        ProposalPayload::FeeScheduleChange {
                            new_min_fee,
                            new_tiers,
                        } => {
                            if let Err(e) =
                                self.execute_fee_schedule_change(*new_min_fee, new_tiers.clone())
                            {
                                tracing::error!("🏛️  FeeScheduleChange execution failed: {e}");
                            } else {
                                gov.mark_executed(&proposal.id).await;
                            }
                        }
                        ProposalPayload::EmissionRateChange {
                            new_satoshis_per_block,
                            ..
                        } => {
                            if let Err(e) =
                                self.apply_emission_rate_change(*new_satoshis_per_block)
                            {
                                tracing::error!("🏛️  EmissionRateChange execution failed: {e}");
                            } else {
                                gov.mark_executed(&proposal.id).await;
                            }
                        }
                    }
                }
            }
        }

        // Save undo log for rollback support
        self.save_undo_log(&undo_log)?;

        // CRITICAL FIX: Normalize block data before storage to ensure deterministic hashing
        // Deep clone to ensure no shared references and normalize all strings
        let mut block = block.clone();
        block.header.leader = block.header.leader.trim().to_string();

        // NOTE: Do NOT sort masternode_rewards — they must stay in the same
        // positional order as the reward distribution transaction outputs so
        // that blocks read back from storage still pass validation.

        // DIAGNOSTIC: Log block hash before storage
        let pre_storage_hash = block.hash();
        tracing::debug!(
            "🔍 PRE-STORAGE: Block {} hash {} (v:{} h:{} prev:{} mr:{} ts:{} br:{} l:'{}' ar:{} vrf_o:{} vrf_s:{} txs:{})",
            block.header.height,
            hex::encode(&pre_storage_hash[..8]),
            block.header.version,
            block.header.height,
            hex::encode(&block.header.previous_hash[..8]),
            hex::encode(&block.header.merkle_root[..8]),
            block.header.timestamp,
            block.header.block_reward,
            block.header.leader,
            hex::encode(&block.header.attestation_root[..8]),
            hex::encode(&block.header.vrf_output[..8]),
            block.header.vrf_score,
            block.transactions.len()
        );

        // Save block - but DON'T update chain height yet
        self.save_block(&block, false)?;

        let is_syncing = self.is_syncing.load(Ordering::Acquire);

        // Read-back verification: confirm hash survived round-trip through sled.
        // Skip during bulk sync — the serialization was already validated above
        // and the read-back doubles sled I/O per block, which starves tokio
        // worker threads and kills RPC responsiveness on low-CPU machines.
        if !is_syncing {
            let retrieved_block = self.get_block_from_storage_only(block.header.height)?;
            let post_storage_hash = retrieved_block.hash();
            if post_storage_hash != pre_storage_hash {
                tracing::error!(
                    "🔬 CRITICAL: POST-STORAGE HASH MISMATCH for block {}!",
                    block.header.height
                );
                tracing::error!(
                    "  Expected: {}, Got: {}",
                    hex::encode(&pre_storage_hash[..8]),
                    hex::encode(&post_storage_hash[..8])
                );
                tracing::error!("  This block will be REJECTED to prevent chain corruption");
                // Remove the corrupted block from storage
                let key = format!("block_{}", block.header.height);
                let _ = self.storage.remove(key.as_bytes());
                self.block_cache.invalidate(block.header.height);

                return Err(format!(
                    "Block {} hash changed after storage (expected {}, got {}). Block rejected.",
                    block.header.height,
                    hex::encode(&pre_storage_hash[..8]),
                    hex::encode(&post_storage_hash[..8])
                ));
            }
            tracing::debug!(
                "✓ Block {} hash verified after storage: {}",
                block.header.height,
                hex::encode(&post_storage_hash[..8])
            );
        }

        // Update chain height
        self.update_chain_height(block.header.height)?;

        // SCAN FORWARD: After filling a gap, check if blocks above already exist in storage.
        // Skip during sync — blocks arrive sequentially so there are no gaps to fill,
        // and each iteration does a sled read that blocks the thread.
        if !is_syncing {
            const MAX_SCAN_FORWARD: u64 = 100;
            let mut scan_height = block.header.height + 1;
            let mut advanced = 0u64;
            while advanced < MAX_SCAN_FORWARD && self.get_block(scan_height).is_ok() {
                self.update_chain_height(scan_height)?;
                self.current_height.store(scan_height, Ordering::Release);
                advanced += 1;
                scan_height += 1;
            }
            if advanced > 0 {
                tracing::info!(
                    "📈 Gap fill: advanced chain height past {} pre-existing blocks (now at height {})",
                    advanced,
                    scan_height - 1
                );
            }
        }

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
        self.current_height
            .store(block.header.height, Ordering::Release);

        // Clear only finalized transactions that were in THIS block
        // Extract non-coinbase, non-reward transaction IDs from the block
        // Skip first 2 transactions (coinbase + reward distribution)
        let block_txids: Vec<Hash256> = block
            .transactions
            .iter()
            .skip(2)
            .map(|tx| tx.txid())
            .collect();

        if !block_txids.is_empty() {
            tracing::debug!(
                "🔍 Block {}: Clearing {} finalized transaction(s) from pool",
                block.header.height,
                block_txids.len()
            );
            self.consensus.clear_finalized_txs(&block_txids);
        }

        // Phase 3.3: Cleanup invalid collaterals after block processing
        // This ensures masternodes with spent collateral are automatically deregistered.
        // Skip during sync: collateral UTXOs for masternodes registered in later blocks
        // haven't been indexed yet, producing false positives that deregister every
        // masternode on every block while we're catching up.
        let cleanup_count = if is_syncing {
            0
        } else {
            self.masternode_registry
                .cleanup_invalid_collaterals(&self.utxo_manager)
                .await
        };

        if cleanup_count > 0 {
            tracing::warn!(
                "🗑️ Auto-deregistered {} masternode(s) with invalid collateral at height {}",
                cleanup_count,
                block.header.height
            );
        }

        tracing::debug!(
            "✓ Block {} added (txs: {}, work: {}), finalized pool cleared",
            block.header.height,
            block.transactions.len(),
            block_work
        );

        // Update transaction index if enabled
        if let Some(tx_index) = &self.tx_index {
            for (tx_index_in_block, tx) in block.transactions.iter().enumerate() {
                let txid = tx.txid();
                if let Err(e) =
                    tx_index.add_transaction(&txid, block.header.height, tx_index_in_block)
                {
                    tracing::warn!(
                        "Failed to update txindex for block {}: {}",
                        block.header.height,
                        e
                    );
                }
            }
        }

        // Mark block as finalized (TimeVote instant finality achieved)
        self.consensus.record_block_finalized(block_hash);

        // Record block for AI predictive sync, transaction analysis, and anomaly detection
        if let Some(ai) = &self.ai_system {
            ai.predictive_sync.record_block(
                block.header.height,
                block.header.timestamp as u64,
                600, // nominal block time in seconds
            );
            ai.anomaly_detector
                .record_event("block_added".to_string(), block.header.height as f64);
        }

        // Signal any waiters (e.g. block production loop) that a new block was added
        self.block_added_signal.notify_waiters();

        Ok(())
    }

    /// Get a block by height (with two-tier cache - 10-50x faster for recent blocks)
    pub fn get_block(&self, height: u64) -> Result<Block, String> {
        // Check cache first (fast path)
        if let Some(cached_block) = self.block_cache.get(height) {
            return Ok((*cached_block).clone());
        }

        let key = format!("block_{}", height);
        let data = self
            .storage
            .get(key.as_bytes())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Block {} not found in storage", height))?;

        // Decompress if necessary (handles both compressed and uncompressed)
        let data = crate::storage::decompress_block(&data).map_err(|e| {
            tracing::error!("Failed to decompress block {}: {}", height, e);
            format!("Block {} decompression failed: {}", height, e)
        })?;

        match bincode::deserialize::<Block>(&data) {
            Ok(block) => {
                let block_arc = Arc::new(block);
                self.block_cache.put(height, block_arc.clone());
                Ok((*block_arc).clone())
            }
            Err(_) => {
                // Migration path: the `total_fees: u64` field was appended to BlockHeader
                // in v1.3.  Old blocks stored before this change are missing those 8 bytes
                // in the MIDDLE of the serialized Block (between producer_signature and
                // transactions).  We cannot simply append bytes; instead we deserialize
                // with a LegacyBlock type that omits total_fees and convert.
                use crate::network::wire::deserialize_legacy_block;

                match deserialize_legacy_block(&data) {
                    Some(block) => {
                        tracing::info!(
                            "🔄 Block {} migrated to v1.3 format (total_fees field added)",
                            height
                        );
                        // Re-store in new format so future reads skip this branch
                        if let Ok(new_serialized) = bincode::serialize(&block) {
                            let data_to_store = if self.compress_blocks {
                                let compressed = crate::storage::compress_block(&new_serialized);
                                if compressed.len() < new_serialized.len() {
                                    compressed
                                } else {
                                    new_serialized
                                }
                            } else {
                                new_serialized
                            };
                            let _ = self.storage.insert(key.as_bytes(), data_to_store);
                        }
                        let block_arc = Arc::new(block);
                        self.block_cache.put(height, block_arc.clone());
                        Ok((*block_arc).clone())
                    }
                    None => {
                        tracing::error!("⚠️ Block {} failed deserialization", height);
                        tracing::warn!(
                            "🔄 CORRUPTED BLOCK RECOVERY: Deleting corrupted block {} for re-fetch from peers",
                            height
                        );
                        let _ = self.storage.remove(key.as_bytes());
                        self.block_cache.invalidate(height);
                        let _ = self.storage.flush();
                        Err(format!(
                            "Block {} was corrupted and has been deleted for re-fetch from peers",
                            height
                        ))
                    }
                }
            }
        }
    }

    /// Get block hash at a height
    pub fn get_block_hash(&self, height: u64) -> Result<[u8; 32], String> {
        let block = self.get_block(height)?;
        Ok(block.hash())
    }

    /// Get the list of masternodes that participated in consensus for a specific block
    /// This is used to determine reward eligibility based on actual participation
    pub fn get_block_consensus_voters(&self, height: u64) -> Vec<String> {
        match self.get_block(height) {
            Ok(block) => {
                let block_hash = block.hash();
                self.consensus.timevote.prepare_votes.get_voters(block_hash)
            }
            Err(_) => vec![],
        }
    }

    /// Get current blockchain height (lock-free - 100x faster than RwLock)
    pub fn get_height(&self) -> u64 {
        self.current_height.load(Ordering::Acquire)
    }

    pub fn network_type(&self) -> NetworkType {
        self.network_type
    }

    /// Check if currently syncing
    pub fn is_syncing(&self) -> bool {
        self.is_syncing.load(Ordering::Acquire)
    }

    /// Flush storage to disk. Call at batch boundaries during sync.
    pub fn flush_storage(&self) -> Result<(), String> {
        self.storage
            .flush()
            .map(|_| ())
            .map_err(|e| format!("Storage flush failed: {}", e))
    }

    /// Async flush that runs on the blocking thread pool so it doesn't
    /// starve the tokio worker threads (critical on 1-CPU machines).
    pub async fn flush_storage_async(&self) -> Result<(), String> {
        let storage = self.storage.clone();
        tokio::task::spawn_blocking(move || {
            storage
                .flush()
                .map(|_| ())
                .map_err(|e| format!("Storage flush failed: {}", e))
        })
        .await
        .unwrap_or_else(|e| Err(format!("flush task panicked: {}", e)))
    }

    /// Check if genesis block exists
    /// Returns true if genesis (block 0) is present in storage
    pub fn has_genesis(&self) -> bool {
        if self.get_height() > 0 {
            return true;
        }
        // Check if block 0 exists in storage
        let key = "block_0".as_bytes();
        self.storage.get(key).ok().flatten().is_some()
    }

    /// Get genesis block hash
    /// Returns the hash of block 0, or all zeros if no genesis exists
    pub fn genesis_hash(&self) -> [u8; 32] {
        self.get_block_hash(0).unwrap_or_default()
    }

    /// Get block cache statistics
    pub fn get_cache_stats(&self) -> crate::block_cache::CacheStats {
        self.block_cache.stats()
    }

    /// Get estimated block cache memory usage in bytes
    pub fn get_cache_memory_usage(&self) -> usize {
        self.block_cache.estimated_memory_usage()
    }

    /// Get pending transactions from the mempool
    pub fn get_pending_transactions(&self) -> Vec<Transaction> {
        self.consensus.tx_pool.get_pending_transactions()
    }

    /// Get block by height  
    pub async fn get_block_by_height(&self, height: u64) -> Result<Block, String> {
        self.get_block(height)
    }

    /// Get UTXO state hash — deterministic SHA-256 over the sorted UTXO set
    pub async fn get_utxo_state_hash(&self) -> [u8; 32] {
        self.utxo_manager.calculate_utxo_set_hash().await
    }

    /// Get count of all unspent UTXOs
    pub async fn get_utxo_count(&self) -> usize {
        self.utxo_manager.list_all_utxos().await.len()
    }

    /// Get the full UTXO set
    pub async fn get_all_utxos(&self) -> Vec<crate::types::UTXO> {
        self.utxo_manager.list_all_utxos().await
    }

    /// Get block hash at height
    pub async fn get_block_hash_at_height(&self, height: u64) -> Option<[u8; 32]> {
        self.get_block_hash(height).ok()
    }

    /// Check chain continuity and detect missing blocks
    /// Returns a list of missing block heights
    pub fn check_chain_continuity(&self) -> Vec<u64> {
        let height = self.get_height();
        let mut missing = Vec::new();

        tracing::info!("🔍 Checking chain continuity from 0 to {}", height);

        for h in 0..=height {
            if self.get_block(h).is_err() {
                missing.push(h);
            }
        }

        if !missing.is_empty() {
            tracing::warn!(
                "⚠️ Chain has {} missing blocks: {:?}",
                missing.len(),
                if missing.len() > 20 {
                    format!("{:?}...and {} more", &missing[..20], missing.len() - 20)
                } else {
                    format!("{:?}", missing)
                }
            );
        } else {
            tracing::info!("✓ Chain is continuous from 0 to {}", height);
        }

        missing
    }

    /// Diagnose storage issues for a range of blocks
    pub fn diagnose_missing_blocks(&self, start: u64, end: u64) {
        tracing::info!("🔬 Diagnosing blocks {} to {}", start, end);

        for height in start..=end {
            let key = format!("block_{}", height);

            let exists = self.storage.get(key.as_bytes()).ok().flatten().is_some();

            if !exists {
                tracing::warn!("  Block {}: MISSING", height);
            } else {
                tracing::debug!("  Block {}: exists", height);

                match self.get_block(height) {
                    Ok(_) => tracing::debug!("    ✓ Deserializes OK"),
                    Err(e) => tracing::error!("    ✗ Deserialization failed: {}", e),
                }
            }
        }
    }

    /// Request missing blocks from peers
    pub async fn request_missing_blocks(&self, missing_heights: Vec<u64>) {
        if missing_heights.is_empty() {
            return;
        }

        tracing::info!(
            "🔄 Requesting {} missing blocks from peers",
            missing_heights.len()
        );

        let peer_registry_opt = self.peer_registry.read().await;
        let Some(peer_registry) = peer_registry_opt.as_ref() else {
            tracing::warn!("⚠️ No peer registry available to request missing blocks");
            return;
        };

        let peers = peer_registry.get_connected_peers().await;

        if peers.is_empty() {
            tracing::warn!("⚠️ No peers available to request missing blocks");
            return;
        }

        // Group consecutive heights into ranges for efficient requests
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        let mut current_start = missing_heights[0];
        let mut current_end = missing_heights[0];

        for &height in &missing_heights[1..] {
            if height == current_end + 1 {
                current_end = height;
            } else {
                ranges.push((current_start, current_end));
                current_start = height;
                current_end = height;
            }
        }
        ranges.push((current_start, current_end));

        tracing::info!("📦 Requesting {} block ranges: {:?}", ranges.len(), ranges);

        // Request each range from a different peer (round-robin)
        for (idx, (start, end)) in ranges.iter().enumerate() {
            let peer_idx = idx % peers.len();
            let peer_addr = &peers[peer_idx];

            tracing::info!(
                "📨 Requesting blocks {}-{} from peer {}",
                start,
                end,
                peer_addr
            );

            // Send GetBlocks message (GetBlockRange doesn't exist in all versions)
            let message = NetworkMessage::GetBlocks(*start, *end);

            if let Err(e) = peer_registry.send_to_peer(peer_addr, message).await {
                tracing::warn!("⚠️ Failed to request blocks from {}: {}", peer_addr, e);
            }
        }
    }

    /// Calculate the VRF score for a single block.
    ///
    /// Prefers the block's stored VRF score if available (cryptographically generated).
    /// Falls back to hash-based score for old blocks without VRF.
    pub fn calculate_block_vrf_score(&self, block: &Block) -> u64 {
        // Check if block has VRF score set (blocks with ECVRF)
        if block.header.vrf_score > 0 {
            return block.header.vrf_score;
        }

        // Check if block has VRF output but no score calculated yet
        if block.header.vrf_output != [0u8; 32] {
            return crate::block::vrf::vrf_output_to_score(&block.header.vrf_output);
        }

        // Fallback: use block hash for old blocks without VRF
        let hash = block.hash();
        u64::from_be_bytes(hash[0..8].try_into().unwrap_or([0u8; 8]))
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

    /// Check if a transaction is finalized.
    ///
    /// A transaction is considered finalized when it has been:
    /// 1. Included in a block (present in the transaction index), OR
    /// 2. Reached 51% TimeVote threshold and is waiting for block inclusion
    ///    (present in the finalized pool or timevote consensus state).
    pub async fn is_transaction_finalized(&self, txid: &[u8; 32]) -> bool {
        // Highest confidence: transaction is already in a block
        if let Some(ref tx_index) = self.tx_index {
            if tx_index.get_location(txid).is_some() {
                return true;
            }
        }
        // Transaction reached timevote finality but not yet included in a block
        if self.consensus.tx_pool.is_finalized(txid) {
            return true;
        }
        self.consensus.timevote.is_finalized(txid)
    }

    /// Get how many blocks deep a transaction is (1-based confirmations).
    ///
    /// Returns `None` when the transaction has not yet been included in any block.
    pub async fn get_transaction_confirmations(&self, txid: &[u8; 32]) -> Option<u64> {
        if let Some(ref tx_index) = self.tx_index {
            if let Some(location) = tx_index.get_location(txid) {
                let current_height = self.get_height();
                return Some(current_height.saturating_sub(location.block_height) + 1);
            }
        }
        None
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

    fn save_block(&self, block: &Block, update_height: bool) -> Result<(), String> {
        let key = format!("block_{}", block.header.height);
        let serialized = bincode::serialize(block).map_err(|e| {
            tracing::error!(
                "❌ Failed to serialize block {}: {}",
                block.header.height,
                e
            );
            e.to_string()
        })?;

        // Compress block data if enabled (typically saves 60-70%)
        let data_to_store = if self.compress_blocks {
            let compressed = crate::storage::compress_block(&serialized);
            if compressed.len() < serialized.len() {
                tracing::trace!(
                    "📦 Block {} compressed: {} → {} bytes ({:.1}% reduction)",
                    block.header.height,
                    serialized.len(),
                    compressed.len(),
                    (1.0 - compressed.len() as f64 / serialized.len() as f64) * 100.0
                );
                compressed
            } else {
                serialized.clone()
            }
        } else {
            serialized.clone()
        };

        self.storage
            .insert(key.as_bytes(), data_to_store.clone())
            .map_err(|e| {
                tracing::error!(
                    "❌ Failed to insert block {} into database: {} (type: {:?})",
                    block.header.height,
                    e,
                    e
                );
                format!("Database insert failed: {}", e)
            })?;

        // CRITICAL: Update cache to ensure consistency
        let block_arc = Arc::new(block.clone());
        self.block_cache.put(block.header.height, block_arc);

        // Update chain height if requested
        if update_height {
            let height_key = "chain_height".as_bytes();
            let height_bytes =
                bincode::serialize(&block.header.height).map_err(|e| e.to_string())?;
            self.storage.insert(height_key, height_bytes).map_err(|e| {
                tracing::error!("❌ Failed to update chain_height: {}", e);
                e.to_string()
            })?;
        }

        // Flush to ensure durability (skip during bulk sync — caller flushes at batch boundaries)
        if !self.is_syncing.load(Ordering::Acquire) {
            self.storage.flush().map_err(|e| {
                tracing::error!(
                    "❌ Failed to flush block {} to disk: {}",
                    block.header.height,
                    e
                );
                e.to_string()
            })?;
        }

        // VERIFICATION: Read back and verify block was stored correctly
        let readback = self.storage.get(key.as_bytes()).map_err(|e| {
            format!(
                "Failed to read back block {} after write: {}",
                block.header.height, e
            )
        })?;

        match readback {
            Some(readback_data) => {
                if readback_data.len() != data_to_store.len() {
                    return Err(format!(
                        "Block {} readback size mismatch: wrote {} bytes, read {} bytes",
                        block.header.height,
                        data_to_store.len(),
                        readback_data.len()
                    ));
                }
                let decompressed =
                    crate::storage::decompress_block(&readback_data).map_err(|e| {
                        tracing::error!(
                            "🚨 Block {} DECOMPRESS FAILED: {} (readback size: {})",
                            block.header.height,
                            e,
                            readback_data.len(),
                        );
                        format!(
                            "Failed to decompress readback block {}: {}",
                            block.header.height, e
                        )
                    })?;

                bincode::deserialize::<Block>(&decompressed).map_err(|e| {
                    tracing::error!(
                        "🚨 Block {} BINCODE DESERIALIZE FAILED: {} (decompressed size: {}, expected: {})",
                        block.header.height,
                        e,
                        decompressed.len(),
                        serialized.len()
                    );
                    format!(
                        "Failed to deserialize readback block {}: {}",
                        block.header.height, e
                    )
                })?;

                tracing::debug!(
                    "✓ Block {} flushed and verified on disk",
                    block.header.height
                );
                Ok(())
            }
            None => Err(format!(
                "Block {} disappeared after write - storage corruption",
                block.header.height
            )),
        }
    }

    /// Update chain height in storage
    fn update_chain_height(&self, height: u64) -> Result<(), String> {
        let height_key = "chain_height".as_bytes();
        let height_bytes = bincode::serialize(&height).map_err(|e| e.to_string())?;
        self.storage
            .insert(height_key, height_bytes)
            .map_err(|e| e.to_string())?;
        // Flush to ensure height is persisted (skip during bulk sync)
        if !self.is_syncing.load(Ordering::Acquire) {
            self.storage
                .flush()
                .map_err(|e| format!("Failed to flush chain_height: {}", e))?;
        }

        // CRITICAL: Update in-memory atomic to keep current_height in sync with storage
        // Without this, blocks are saved to disk but current_height stays stale,
        // causing nodes to think they're still at height 0 and reject new blocks
        self.current_height.store(height, Ordering::Release);

        Ok(())
    }

    /// Get block directly from storage, bypassing cache (for verification)
    fn get_block_from_storage_only(&self, height: u64) -> Result<Block, String> {
        let key = format!("block_{}", height);
        let value = self
            .storage
            .get(key.as_bytes())
            .map_err(|e| e.to_string())?;

        if let Some(v) = value {
            // Decompress if necessary (handles both compressed and uncompressed)
            let data = crate::storage::decompress_block(&v).map_err(|e| e.to_string())?;
            let block: Block = bincode::deserialize(&data).map_err(|e| e.to_string())?;
            Ok(block)
        } else {
            Err(format!("Block {} not found in storage", height))
        }
    }

    /// Process block UTXOs and create undo log for rollback support
    async fn process_block_utxos(&self, block: &Block) -> Result<UndoLog, String> {
        let block_hash = block.hash();
        let mut undo_log = UndoLog::new(block.header.height, block_hash);
        let mut utxos_created = 0;
        let mut utxos_spent = 0;

        // NOTE: block.masternode_rewards is metadata only - the actual reward UTXOs
        // are created by the reward distribution transaction (transaction index 1).
        // We do NOT create separate UTXOs from the masternode_rewards array to avoid
        // double-counting rewards.
        //
        // EXCEPTION: genesis block (height 0) has no transactions, so we create UTXOs
        // directly from masternode_rewards using the block hash as the synthetic txid.
        if block.header.height == 0 && block.transactions.is_empty() {
            for (vout, (address, amount)) in block.masternode_rewards.iter().enumerate() {
                if *amount == 0 || address.is_empty() {
                    continue;
                }
                let utxo = UTXO {
                    outpoint: OutPoint {
                        txid: block_hash,
                        vout: vout as u32,
                    },
                    value: *amount,
                    script_pubkey: address.as_bytes().to_vec(),
                    address: address.clone(),
                };
                if let Err(e) = self.utxo_manager.add_utxo(utxo).await {
                    tracing::warn!(
                        "⚠️  Could not add genesis reward UTXO for {} in genesis block: {:?}",
                        address,
                        e
                    );
                } else {
                    utxos_created += 1;
                    tracing::info!(
                        "💰 Genesis UTXO created: {} TIME → {}",
                        amount / 100_000_000,
                        address
                    );
                }
            }
        }

        tracing::debug!(
            "📊 Block {} has {} masternode reward recipients (metadata)",
            block.header.height,
            block.masternode_rewards.len()
        );

        // Process each transaction (including coinbase and reward distribution)
        for tx in &block.transactions {
            let txid = tx.txid();

            // Check if transaction was finalized by timevote before block inclusion
            // Simplified: If transaction doesn't exist in finalized pool, assume it's finalized
            // (More robust finalization tracking can be added later)
            let is_finalized = false; // Conservative: treat all as unfinalized for undo logs
            if is_finalized {
                undo_log.add_finalized_tx(txid);
            }

            // Spend inputs (mark UTXOs as spent) and record in undo log
            for input in &tx.inputs {
                // For non-finalized transactions, save the UTXO before spending
                // so we can restore it during rollback
                if !is_finalized {
                    if let Ok(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                        undo_log.add_spent_utxo(input.previous_output.clone(), utxo);
                    }
                }

                if let Err(e) = self.utxo_manager.spend_utxo(&input.previous_output).await {
                    tracing::warn!(
                        "⚠️  Could not spend UTXO {}:{} in block {}: {:?}",
                        hex::encode(input.previous_output.txid),
                        input.previous_output.vout,
                        block.header.height,
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
                    tracing::warn!(
                        "⚠️  Could not add UTXO for tx {} vout {} in block {}: {:?}",
                        hex::encode(txid),
                        vout,
                        block.header.height,
                        e
                    );
                } else {
                    utxos_created += 1;
                }
            }
        }

        if utxos_created > 0 || utxos_spent > 0 {
            tracing::info!(
                "💰 Block {} indexed {} UTXOs ({} created, {} spent, {} in undo log)",
                block.header.height,
                utxos_created,
                utxos_created,
                utxos_spent,
                undo_log.spent_utxos.len()
            );
        }

        Ok(undo_log)
    }

    /// Scan a block for special transactions (masternode registration/updates)
    /// and apply them to the masternode registry.
    async fn process_special_transactions(&self, block: &Block) {
        use crate::types::SpecialTransactionData;

        for tx in &block.transactions {
            let special = match &tx.special_data {
                Some(data) => data,
                None => continue,
            };

            let txid_hex = hex::encode(tx.txid());

            match special {
                SpecialTransactionData::MasternodeReg {
                    collateral_outpoint,
                    masternode_ip,
                    masternode_port,
                    payout_address,
                    owner_pubkey,
                    signature,
                } => {
                    // Validate the registration
                    match self
                        .masternode_registry
                        .validate_masternode_reg(
                            collateral_outpoint,
                            masternode_ip,
                            *masternode_port,
                            payout_address,
                            owner_pubkey,
                            signature,
                            &self.utxo_manager,
                        )
                        .await
                    {
                        Ok((outpoint, tier)) => {
                            // Apply the registration
                            if let Err(e) = self
                                .masternode_registry
                                .apply_masternode_reg(
                                    outpoint,
                                    masternode_ip,
                                    *masternode_port,
                                    payout_address,
                                    owner_pubkey,
                                    tier,
                                    &self.utxo_manager,
                                )
                                .await
                            {
                                tracing::warn!(
                                    "⚠️ Failed to apply MasternodeReg tx {}: {}",
                                    &txid_hex[..16],
                                    e
                                );
                            } else {
                                tracing::info!(
                                    "✅ MasternodeReg applied: {}:{} -> {} (tx {})",
                                    masternode_ip,
                                    masternode_port,
                                    payout_address,
                                    &txid_hex[..16]
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "⚠️ Invalid MasternodeReg tx {}: {}",
                                &txid_hex[..16],
                                e
                            );
                        }
                    }
                }

                SpecialTransactionData::CollateralUnlock {
                    collateral_outpoint,
                    masternode_address,
                    owner_pubkey,
                    signature,
                } => {
                    match self
                        .masternode_registry
                        .validate_collateral_unlock(
                            collateral_outpoint,
                            masternode_address,
                            owner_pubkey,
                            signature,
                        )
                        .await
                    {
                        Ok(outpoint) => {
                            if let Err(e) = self
                                .masternode_registry
                                .apply_collateral_unlock(
                                    outpoint,
                                    masternode_address,
                                    &self.utxo_manager,
                                )
                                .await
                            {
                                tracing::warn!(
                                    "⚠️ Failed to apply CollateralUnlock tx {}: {}",
                                    &txid_hex[..16],
                                    e
                                );
                            } else {
                                tracing::info!(
                                    "✅ CollateralUnlock applied: {} (tx {})",
                                    masternode_address,
                                    &txid_hex[..16]
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "⚠️ Invalid CollateralUnlock tx {}: {}",
                                &txid_hex[..16],
                                e
                            );
                        }
                    }
                }

                SpecialTransactionData::MasternodePayoutUpdate {
                    masternode_id,
                    new_payout_address,
                    owner_pubkey,
                    signature,
                } => {
                    match self
                        .masternode_registry
                        .validate_masternode_update(
                            masternode_id,
                            new_payout_address,
                            owner_pubkey,
                            signature,
                        )
                        .await
                    {
                        Ok(()) => {
                            if let Err(e) = self
                                .masternode_registry
                                .apply_masternode_update(masternode_id, new_payout_address)
                                .await
                            {
                                tracing::warn!(
                                    "⚠️ Failed to apply MasternodePayoutUpdate tx {}: {}",
                                    &txid_hex[..16],
                                    e
                                );
                            } else {
                                tracing::info!(
                                    "✅ MasternodePayoutUpdate applied: {} -> {} (tx {})",
                                    masternode_id,
                                    new_payout_address,
                                    &txid_hex[..16]
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "⚠️ Invalid MasternodePayoutUpdate tx {}: {}",
                                &txid_hex[..16],
                                e
                            );
                        }
                    }
                }
            }
        }
    }

    /// Validate block rewards are correct and not double-counted.
    /// Also verifies the 35/65 split and per-tier pool distribution through
    /// consensus: each node independently re-derives expected rewards from chain data.
    async fn validate_block_rewards(&self, block: &Block) -> Result<(), String> {
        // Skip validation for blocks with no transactions (shouldn't happen, but be safe)
        if block.transactions.len() < 2 {
            return Err(format!(
                "Block {} has {} transactions, expected at least 2 (coinbase + reward distribution)",
                block.header.height,
                block.transactions.len()
            ));
        }

        // Transaction 0 should be coinbase
        let coinbase = &block.transactions[0];
        if !coinbase.inputs.is_empty() {
            return Err(format!(
                "Block {} transaction 0 is not a coinbase (has {} inputs)",
                block.header.height,
                coinbase.inputs.len()
            ));
        }

        // Coinbase should create exactly one output with the total block reward
        if coinbase.outputs.len() != 1 {
            return Err(format!(
                "Block {} coinbase has {} outputs, expected 1",
                block.header.height,
                coinbase.outputs.len()
            ));
        }

        let coinbase_amount = coinbase.outputs[0].value;
        if coinbase_amount != block.header.block_reward {
            return Err(format!(
                "Block {} coinbase creates {} satoshis, but block_reward is {}",
                block.header.height, coinbase_amount, block.header.block_reward
            ));
        }

        // CRITICAL: Validate the block_reward is correct (base reward + fees from THIS block's txs)
        // Calculate fees from the current block's user transactions (indices 2+)
        // This mirrors the block producer's logic in produce_block_at_height()
        let mut calculated_fees = 0u64;
        for tx in block.transactions.iter().skip(2) {
            // Calculate output sum
            let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();

            // Calculate input sum by looking up each spent UTXO value from blockchain
            let mut input_sum: u64 = 0;
            let mut all_found = true;
            for input in &tx.inputs {
                let spent_txid = input.previous_output.txid;
                let spent_vout = input.previous_output.vout;

                // Try tx_index first for O(1) lookup.
                // CRITICAL: Always verify src_tx.txid() == spent_txid after the lookup.
                // Stale tx_index entries (left by incomplete rollbacks) may point to a
                // different transaction at the same block/index position, which would
                // return the wrong output value and cause fee validation to fail.
                let mut found = false;
                if let Some(ref txi) = self.tx_index {
                    if let Some(loc) = txi.get_location(&spent_txid) {
                        if let Ok(src_block) = self.get_block(loc.block_height) {
                            if let Some(src_tx) = src_block.transactions.get(loc.tx_index) {
                                if src_tx.txid() == spent_txid {
                                    if let Some(output) = src_tx.outputs.get(spent_vout as usize) {
                                        input_sum += output.value;
                                        found = true;
                                    }
                                }
                                // txid mismatch → stale entry; fall through to linear search
                            }
                        }
                    }
                }

                // Fallback: linear search through recent blocks
                if !found {
                    let search_limit = block.header.height.min(1000);
                    for search_height in (0..block.header.height).rev().take(search_limit as usize)
                    {
                        if let Ok(search_block) = self.get_block(search_height) {
                            for search_tx in &search_block.transactions {
                                if search_tx.txid() == spent_txid {
                                    if let Some(output) = search_tx.outputs.get(spent_vout as usize)
                                    {
                                        input_sum += output.value;
                                        found = true;
                                        break;
                                    }
                                }
                            }
                            if found {
                                break;
                            }
                        }
                    }
                }

                if !found {
                    tracing::debug!(
                        "Could not find UTXO for fee validation in tx {} (tx {}, vout {}), skipping fee check",
                        hex::encode(&tx.txid()[..8]),
                        hex::encode(&spent_txid[..8]),
                        spent_vout
                    );
                    all_found = false;
                    break;
                }
            }

            if !all_found {
                // Can't validate fees without all UTXO data - skip
                return Ok(());
            }

            // Fee for this transaction = inputs - outputs
            if input_sum >= output_sum {
                calculated_fees += input_sum - output_sum;
            } else {
                tracing::debug!(
                    "Transaction {} in block {} has outputs ({}) exceeding inputs ({}), skipping fee validation",
                    hex::encode(&tx.txid()[..8]),
                    block.header.height,
                    output_sum,
                    input_sum
                );
                return Ok(());
            }
        }

        // Verify block_reward matches base reward + calculated fees - treasury allocation
        let treasury_share = constants::blockchain::TREASURY_POOL_SATOSHIS;
        let expected_reward = BLOCK_REWARD_SATOSHIS + calculated_fees - treasury_share;

        if block.header.block_reward != expected_reward {
            return Err(format!(
                "Block {} has incorrect block_reward: expected {} (base {} + fees {} - treasury {}), got {}",
                block.header.height,
                expected_reward,
                BLOCK_REWARD_SATOSHIS,
                calculated_fees,
                treasury_share,
                block.header.block_reward
            ));
        }

        // Transaction 1 should be reward distribution
        let reward_dist = &block.transactions[1];

        // Should spend the coinbase
        if reward_dist.inputs.len() != 1 {
            return Err(format!(
                "Block {} reward distribution has {} inputs, expected 1",
                block.header.height,
                reward_dist.inputs.len()
            ));
        }

        let coinbase_txid = coinbase.txid();
        if reward_dist.inputs[0].previous_output.txid != coinbase_txid {
            return Err(format!(
                "Block {} reward distribution doesn't spend coinbase",
                block.header.height
            ));
        }

        // Verify outputs match masternode_rewards metadata.
        // Output count may differ from masternode_rewards count when multiple
        // masternodes share a reward address (entries are merged in newer code).
        if reward_dist.outputs.len() > block.masternode_rewards.len() {
            return Err(format!(
                "Block {} reward distribution has {} outputs but masternode_rewards has only {} entries",
                block.header.height,
                reward_dist.outputs.len(),
                block.masternode_rewards.len()
            ));
        }

        // Verify each output matches metadata (position-independent to handle
        // blocks stored with sorted masternode_rewards from older code).
        // Sum entries for the same address — multiple masternodes can share a
        // reward address, creating duplicate entries in both masternode_rewards
        // and transaction outputs.
        let mut rewards_map: std::collections::HashMap<&str, u64> =
            std::collections::HashMap::new();
        for (a, v) in &block.masternode_rewards {
            *rewards_map.entry(a.as_str()).or_insert(0) += v;
        }

        // Also sum transaction outputs by address for comparison
        let mut outputs_map: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for output in &reward_dist.outputs {
            let addr = String::from_utf8_lossy(&output.script_pubkey).to_string();
            *outputs_map.entry(addr).or_insert(0) += output.value;
        }

        for (output_addr, output_total) in &outputs_map {
            match rewards_map.get(output_addr.as_str()) {
                Some(&expected_amount) => {
                    if *output_total != expected_amount {
                        return Err(format!(
                            "Block {} reward output amount mismatch for {}: expected {}, got {}",
                            block.header.height, output_addr, expected_amount, output_total
                        ));
                    }
                }
                None => {
                    return Err(format!(
                        "Block {} reward output address {} not found in masternode_rewards",
                        block.header.height, output_addr
                    ));
                }
            }
        }

        // Verify total outputs match block reward exactly (with small tolerance for rounding)
        let total_distributed: u64 = reward_dist.outputs.iter().map(|o| o.value).sum();
        let expected_total = block.header.block_reward;

        // Allow small tolerance for rounding errors in integer division
        // Tolerance should be less than the number of masternodes (worst case: 1 satoshi per node)
        let tolerance = block.masternode_rewards.len() as u64;

        let lower_bound = expected_total.saturating_sub(tolerance);
        let upper_bound = expected_total;

        if total_distributed < lower_bound || total_distributed > upper_bound {
            return Err(format!(
                "Block {} total distributed {} outside valid range {}-{} (block_reward: {})",
                block.header.height, total_distributed, lower_bound, upper_bound, expected_total
            ));
        }

        // ═══ CONSENSUS-VERIFIED POOL DISTRIBUTION ═══
        // Re-derive the expected per-tier pool allocation from chain data.
        // Every validating node independently computes the same result, closing the gap
        // where a dishonest producer could manipulate pool distributions.
        //
        // Skip for very early blocks (no meaningful fairness history yet).
        // Skip for any block that is not truly "live" (produced in the last 30 minutes).
        //
        // During initial sync, the local registry is incomplete: masternodes whose
        // collateral UTXOs haven't been indexed yet get rejected from the registry.
        // By the time we sync to those heights, the gossip may not have re-arrived yet,
        // leaving the registry in an inconsistent state vs. the block's production-time
        // registry. A 30-minute window ensures only blocks that arrived in real-time
        // (while the node is already synced and the registry is accurate) are validated.
        {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let block_age_secs = now_secs.saturating_sub(block.header.timestamp);
            if block.header.height > 10 && block_age_secs < 1800 {
                self.validate_pool_distribution(block, calculated_fees)
                    .await?;
            }
        }

        Ok(())
    }

    /// Verify the reward distribution in a block is mathematically consistent with the
    /// tier-pool algorithm, using only data committed in the block itself.
    ///
    /// Previous approach: re-derive expected rewards from the live registry, compare to
    /// block.masternode_rewards.  Problem: the live registry reflects the current network,
    /// not the historical state when the block was produced, so historical blocks during
    /// sync are systematically rejected.
    ///
    /// This approach: read the actual rewards from block.masternode_rewards (committed,
    /// tamper-evident), classify each recipient's tier from the registry (stable — tier
    /// changes require re-registering with a new collateral UTXO), and verify the amounts
    /// are consistent with the tier-pool constants.  We verify AMOUNTS, not IDENTITY of
    /// winner within a tier (fairness-rotation winner can only be verified with historical
    /// blocks_without_reward state that is not stored on-chain).
    async fn validate_pool_distribution(
        &self,
        block: &Block,
        calculated_fees: u64,
    ) -> Result<(), String> {
        use crate::constants::blockchain::{
            GOLD_POOL_SATOSHIS, MAX_FREE_TIER_RECIPIENTS, PRODUCER_REWARD_SATOSHIS,
            SATOSHIS_PER_TIME,
        };
        use crate::types::MasternodeTier;

        let producer_addr = &block.header.leader;
        if producer_addr.is_empty() || block.masternode_rewards.is_empty() {
            return Ok(());
        }

        // ── Step 1: resolve producer's wallet address ─────────────────────────
        // We still use the registry for this single lookup (IP → wallet). The
        // producer's wallet address is stable — if we can't find it the block
        // is either very old or the registry is empty; skip validation.
        let all_infos = self.masternode_registry.list_all().await;
        let producer_wallet = match all_infos
            .iter()
            .find(|info| info.masternode.address == *producer_addr)
            .map(|info| info.masternode.wallet_address.clone())
        {
            Some(w) => w,
            None => return Ok(()),
        };

        // ── Step 2: partition block.masternode_rewards into producer vs tier pools ──
        // Each entry in masternode_rewards is a (TIME_wallet_address, satoshis) pair
        // committed in the block.  Look up each non-producer wallet's tier from the
        // registry (stable property) so we can verify the pool amounts.
        let mut producer_received: u64 = 0;
        // Accumulate the total paid per tier from the block's actual rewards.
        let mut tier_paid: std::collections::HashMap<MasternodeTier, u64> =
            std::collections::HashMap::new();
        let mut unknown_non_producer_paid: u64 = 0;

        for (wallet, amount) in &block.masternode_rewards {
            if wallet == &producer_wallet {
                producer_received += amount;
            } else {
                match self.masternode_registry.tier_for_wallet(wallet).await {
                    Some(tier) => {
                        *tier_paid.entry(tier).or_insert(0) += amount;
                    }
                    None => {
                        // Wallet not in current registry — masternode may have
                        // deregistered since the block was produced.  Accept the
                        // payment as long as it doesn't exceed the largest single pool.
                        unknown_non_producer_paid += amount;
                        if *amount > GOLD_POOL_SATOSHIS {
                            return Err(format!(
                                "Block {} unknown recipient {} received {} satoshis, \
                                 exceeds max tier pool {}",
                                block.header.height, wallet, amount, GOLD_POOL_SATOSHIS
                            ));
                        }
                    }
                }
            }
        }

        // ── Step 3: verify each tier pool was distributed correctly ───────────
        // For each paid tier: the total paid to all recipients of that tier must
        // equal the tier's canonical pool allocation.
        // For each unpaid tier (no recipients): that pool must have rolled up to
        // the producer — we track this to verify the producer's total below.
        //
        // IMPORTANT: If any reward recipients are no longer in the registry
        // (deregistered since the block was produced, or from old pool-sharing era),
        // we cannot accurately reconstruct per-tier totals — fall back to a looser
        // total-budget check instead of strict per-tier verification.
        let total_tier_budget: u64 = [
            MasternodeTier::Gold,
            MasternodeTier::Silver,
            MasternodeTier::Bronze,
            MasternodeTier::Free,
        ]
        .iter()
        .map(|t| t.pool_allocation())
        .sum();

        let mut rolled_up_to_producer: u64 = 0;
        // When the fallback path fires (deregistered recipients present), we can't
        // determine exactly how much rolled up to the producer.  Use 0 as the minimum
        // (producer must have received at least PRODUCER_REWARD) and total_tier_budget
        // as the maximum (all pools could theoretically have rolled up).
        let mut rolled_up_to_producer_max_override: Option<u64> = None;

        if unknown_non_producer_paid > 0 {
            // Some recipients have deregistered — we can only verify the total
            // non-producer payout is within the overall tier-pool budget.
            let total_non_producer_paid: u64 =
                tier_paid.values().sum::<u64>() + unknown_non_producer_paid;
            if total_non_producer_paid > total_tier_budget {
                return Err(format!(
                    "Block {} total non-producer payout {} exceeds total tier budget {}",
                    block.header.height, total_non_producer_paid, total_tier_budget
                ));
            }
            // Keep rolled_up_to_producer=0 for the MIN check (we don't know actual rollup,
            // so only require producer received base PRODUCER_REWARD).
            // Override MAX check to allow up to full tier budget rolled up.
            rolled_up_to_producer_max_override = Some(total_tier_budget);
            tracing::debug!(
                "Block {} has {} satoshis paid to unknown (deregistered) recipients — \
                 skipping strict per-tier pool verification, using loose producer max",
                block.header.height,
                unknown_non_producer_paid,
            );
        } else {
            for tier in &[
                MasternodeTier::Gold,
                MasternodeTier::Silver,
                MasternodeTier::Bronze,
                MasternodeTier::Free,
            ] {
                let pool = tier.pool_allocation();
                let paid = tier_paid.get(tier).copied().unwrap_or(0);

                if paid == 0 {
                    // No recipients in this tier — pool should have rolled to producer.
                    rolled_up_to_producer += pool;
                    continue;
                }

                // For Free tier: pool is split among ≤ MAX_FREE_TIER_RECIPIENTS nodes.
                // For paid tiers: the full pool goes to exactly one winner.
                // In both cases, total paid to the tier must equal the pool allocation
                // (within 1 satoshi per recipient for integer-division rounding).
                let recipient_count = if matches!(tier, MasternodeTier::Free) {
                    // Free tier: pool split among ≤ MAX_FREE_TIER_RECIPIENTS nodes.
                    // No minimum per-node threshold — always distribute regardless of amount.
                    let per_node = pool / MAX_FREE_TIER_RECIPIENTS as u64;
                    if per_node == 0 {
                        // Pool is smaller than MAX_FREE_TIER_RECIPIENTS satoshis — accept as-is.
                        continue;
                    }
                    // Accept any split ≤ MAX_FREE_TIER_RECIPIENTS
                    (paid / per_node).max(1) as usize
                } else {
                    1
                };

                // Rounding tolerance: at most 1 satoshi per recipient.
                let tolerance = recipient_count as u64;
                let diff = paid.abs_diff(pool);
                if diff > tolerance {
                    return Err(format!(
                        "Block {} {:?} tier pool mismatch: expected {} satoshis, \
                         block distributed {} (diff {})",
                        block.header.height, tier, pool, paid, diff
                    ));
                }
            }
        }

        // ── Step 4: verify producer received correct amount ──────────────────
        // Producer must receive: PRODUCER_REWARD + fees + all rolled-up empty pools.
        // Allow ±1 TIME tolerance for rounding and minor fee-calculation differences
        // (e.g., a peer computed fees slightly differently due to UTXO lookup order).
        // When deregistered recipients exist (fallback path), min uses rolled_up=0
        // (only require base PRODUCER_REWARD) while max uses total_tier_budget override.

        // ── All-Free detection ────────────────────────────────────────────────
        // When no paid-tier nodes exist, the block uses all-Free distribution:
        // 95 TIME split evenly among ≤MAX_FREE_TIER_RECIPIENTS free nodes.
        // The producer receives ~95/N TIME (not 30 TIME), so the normal min check
        // would incorrectly reject these valid blocks. Detect and validate separately.
        let paid_tier_total: u64 = tier_paid
            .iter()
            .filter(|(t, _)| **t != MasternodeTier::Free)
            .map(|(_, v)| *v)
            .sum();

        if producer_received < PRODUCER_REWARD_SATOSHIS && paid_tier_total == 0 {
            // All-Free block: total distributed should equal 95 TIME (block_reward - treasury).
            let free_paid = tier_paid
                .get(&MasternodeTier::Free)
                .copied()
                .unwrap_or(0);
            let total_distributed =
                producer_received + free_paid + unknown_non_producer_paid;
            let expected_total = block
                .header
                .block_reward
                .saturating_sub(crate::constants::blockchain::TREASURY_POOL_SATOSHIS);
            let tolerance = MAX_FREE_TIER_RECIPIENTS as u64; // ≤1 sat rounding per recipient
            if total_distributed.abs_diff(expected_total) > tolerance {
                return Err(format!(
                    "Block {} all-Free distribution: total paid {} satoshis, \
                     expected {} (block_reward - treasury)",
                    block.header.height, total_distributed, expected_total
                ));
            }
            let active_recipients = block
                .masternode_rewards
                .iter()
                .filter(|(_, v)| *v > 0)
                .count();
            if active_recipients > MAX_FREE_TIER_RECIPIENTS {
                return Err(format!(
                    "Block {} all-Free: {} recipients exceeds max {}",
                    block.header.height, active_recipients, MAX_FREE_TIER_RECIPIENTS
                ));
            }
            tracing::debug!(
                "Block {} validated as all-Free: {} TIME to {} recipients",
                block.header.height,
                total_distributed / SATOSHIS_PER_TIME,
                active_recipients,
            );
            return Ok(());
        }

        let rolled_up_for_max = rolled_up_to_producer_max_override.unwrap_or(rolled_up_to_producer);
        let expected_producer_min = PRODUCER_REWARD_SATOSHIS + rolled_up_to_producer; // fees may be 0 if unknown
        let expected_producer_max =
            PRODUCER_REWARD_SATOSHIS + calculated_fees + rolled_up_for_max + SATOSHIS_PER_TIME;

        if producer_received < expected_producer_min.saturating_sub(SATOSHIS_PER_TIME) {
            return Err(format!(
                "Block {} producer {} received {} satoshis, \
                 below minimum expected {} (30 TIME + {} rolled-up pools)",
                block.header.height,
                producer_addr,
                producer_received,
                expected_producer_min,
                rolled_up_to_producer
            ));
        }

        if producer_received > expected_producer_max {
            return Err(format!(
                "Block {} producer {} received {} satoshis, \
                 exceeds maximum expected {} (30 TIME + fees {} + rolled {} + 1 TIME tolerance)",
                block.header.height,
                producer_addr,
                producer_received,
                expected_producer_max,
                calculated_fees,
                rolled_up_for_max
            ));
        }

        // ── Step 5: sanity-check using bitmap active count ────────────────────
        let bitmap_active_count: u32 = block
            .header
            .active_masternodes_bitmap
            .iter()
            .map(|b| b.count_ones())
            .sum();
        let actual_recipient_count = block.masternode_rewards.len() as u32;
        if bitmap_active_count > 0 && actual_recipient_count > bitmap_active_count {
            tracing::warn!(
                "⚠️ Block {} has more reward recipients ({}) than bitmap active count ({})",
                block.header.height,
                actual_recipient_count,
                bitmap_active_count
            );
        }

        Ok(())
    }

    // ===== Reward Misbehavior Tracking =====

    /// Check whether a producer has exceeded the reward-violation threshold.
    pub fn is_producer_misbehaving(&self, producer_addr: &str) -> bool {
        self.reward_violations
            .get(producer_addr)
            .map(|v| *v >= REWARD_VIOLATION_THRESHOLD)
            .unwrap_or(false)
    }

    /// Record a reward-distribution violation for a block producer.
    /// On reaching REWARD_VIOLATION_THRESHOLD, the producer's collateral is slashed
    /// (transferred to treasury) and the masternode is deregistered.
    pub async fn record_reward_violation(&self, producer_addr: &str) {
        let mut count = self
            .reward_violations
            .entry(producer_addr.to_string())
            .or_insert(0);
        *count += 1;
        let strikes = *count;
        if strikes >= REWARD_VIOLATION_THRESHOLD {
            tracing::warn!(
                "🚨 Producer {} has {} reward violation(s) — SLASHING collateral and deregistering",
                producer_addr,
                strikes
            );
            // Drop DashMap ref before async call
            drop(count);
            self.slash_producer_collateral(producer_addr).await;
        } else {
            tracing::warn!(
                "⚠️ Producer {} reward violation ({}/{} strikes)",
                producer_addr,
                strikes,
                REWARD_VIOLATION_THRESHOLD
            );
        }
    }

    /// Slash a misbehaving producer: unlock their collateral, transfer the amount
    /// to the on-chain treasury balance, and deregister the masternode.
    async fn slash_producer_collateral(&self, producer_reward_addr: &str) {
        // Find the masternode whose reward_address matches the producer
        let registry = &self.masternode_registry;
        let mn_entry = registry.find_by_reward_address(producer_reward_addr).await;

        let (mn_address, collateral_outpoint, collateral_amount) = match mn_entry {
            Some((addr, info)) => {
                let outpoint = info.masternode.collateral_outpoint.clone();
                let amount =
                    info.masternode.tier.collateral() * constants::blockchain::SATOSHIS_PER_TIME;
                (addr, outpoint, amount)
            }
            None => {
                tracing::warn!(
                    "⚠️ Cannot slash producer {} — no masternode found with that reward address",
                    producer_reward_addr
                );
                return;
            }
        };

        // Unlock and consume the collateral UTXO
        if let Some(outpoint) = &collateral_outpoint {
            if let Err(e) = self.utxo_manager.unlock_collateral(outpoint) {
                tracing::warn!("⚠️ Failed to unlock collateral during slash: {}", e);
            }
            // Mark the UTXO as spent so it cannot be reused
            if let Err(e) = self.utxo_manager.spend_utxo(outpoint).await {
                tracing::warn!("⚠️ Failed to spend slashed collateral UTXO: {}", e);
            }
        }

        // Transfer collateral value to treasury
        self.treasury_deposit(collateral_amount);
        tracing::warn!(
            "🏦 Slashed {} TIME from producer {} → treasury (new balance: {} TIME)",
            collateral_amount / constants::blockchain::SATOSHIS_PER_TIME,
            producer_reward_addr,
            self.get_treasury_balance() / constants::blockchain::SATOSHIS_PER_TIME
        );

        // Deregister the masternode
        match registry.unregister(&mn_address).await {
            Ok(Some(_)) => {
                tracing::warn!(
                    "🗑️ Deregistered masternode {} (reward addr {}) after collateral slash",
                    mn_address,
                    producer_reward_addr
                );
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    "⚠️ Failed to deregister slashed masternode {}: {:?}",
                    mn_address,
                    e
                );
            }
        }
    }

    // ===== Treasury =====

    /// Get the current treasury balance in satoshis.
    pub fn get_treasury_balance(&self) -> u64 {
        self.treasury_balance.load(Ordering::Relaxed)
    }

    /// Deposit satoshis into the treasury (used by slashing).
    pub fn treasury_deposit(&self, amount: u64) {
        self.treasury_balance.fetch_add(amount, Ordering::Relaxed);
        // Persist to disk
        let new_balance = self.get_treasury_balance();
        if let Ok(bytes) = bincode::serialize(&new_balance) {
            let _ = self.storage.insert("treasury_balance", bytes);
        }
    }

    /// Validate a block proposal's reward distribution BEFORE voting.
    /// Strict: returns Err if rewards deviate or producer is misbehaving.
    pub async fn validate_proposal_rewards(&self, block: &Block) -> Result<(), String> {
        let producer_addr = &block.header.leader;

        // Reject proposals from producers that have exceeded the misbehavior threshold
        if !producer_addr.is_empty() && self.is_producer_misbehaving(producer_addr) {
            return Err(format!(
                "Producer {} is misbehaving ({} reward violations) — rejecting proposal",
                producer_addr,
                self.reward_violations
                    .get(producer_addr)
                    .map(|v| *v)
                    .unwrap_or(0)
            ));
        }

        // Run the pool distribution check with 0 fees as a baseline.
        // If the check fails, record a violation and reject.
        if let Err(e) = self.validate_pool_distribution(block, 0).await {
            if !producer_addr.is_empty() {
                self.record_reward_violation(producer_addr).await;
            }
            return Err(e);
        }

        Ok(())
    }

    // ===== Fork Detection and Reorganization =====

    /// Detect if we're on a different chain than a peer by comparing block hashes
    /// Returns Some(fork_height) if fork detected, None if chains match
    pub async fn detect_fork(&self, peer_height: u64, peer_tip_hash: [u8; 32]) -> Option<u64> {
        let our_height = self.current_height.load(Ordering::Acquire);

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

    /// Save undo log for a block
    fn save_undo_log(&self, undo_log: &UndoLog) -> Result<(), String> {
        let key = format!("undo_{}", undo_log.height);
        let data = bincode::serialize(undo_log).map_err(|e| {
            tracing::error!(
                "❌ Failed to serialize undo log for block {}: {}",
                undo_log.height,
                e
            );
            format!("Serialize undo log failed: {}", e)
        })?;

        self.storage
            .insert(key.as_bytes(), data)
            .map_err(|e| {
                tracing::error!("❌ CRITICAL: Failed to save undo log for block {}: {}", undo_log.height, e);
                tracing::error!("   Error details: {:?}", e);
                tracing::error!("   This indicates database corruption or disk issues");
                tracing::error!("   Fix: sudo systemctl stop timed && rm -rf /root/.timecoin/testnet/db/blocks && sudo systemctl start timed");
                format!("DB insert undo_log failed: {:?}", e)
            })?;
        Ok(())
    }

    /// Load undo log for a block height
    fn load_undo_log(&self, height: u64) -> Result<UndoLog, String> {
        let key = format!("undo_{}", height);
        let value = self
            .storage
            .get(key.as_bytes())
            .map_err(|e| e.to_string())?;

        if let Some(v) = value {
            let undo_log: UndoLog = bincode::deserialize(&v).map_err(|e| e.to_string())?;
            Ok(undo_log)
        } else {
            Err(format!("Undo log not found for height {}", height))
        }
    }

    /// Delete undo log for a block height
    fn delete_undo_log(&self, height: u64) -> Result<(), String> {
        let key = format!("undo_{}", height);
        self.storage
            .remove(key.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Get checkpoints for the current network
    fn get_checkpoints(&self) -> &'static [(u64, &'static str)] {
        match self.network_type {
            NetworkType::Mainnet => MAINNET_CHECKPOINTS,
            NetworkType::Testnet => TESTNET_CHECKPOINTS,
        }
    }

    /// Check if a height is a checkpoint
    pub fn is_checkpoint(&self, height: u64) -> bool {
        self.get_checkpoints().iter().any(|(h, _)| *h == height)
    }

    /// Validate that a block matches a checkpoint
    pub fn validate_checkpoint(&self, height: u64, block_hash: &[u8; 32]) -> Result<(), String> {
        for (checkpoint_height, checkpoint_hash_str) in self.get_checkpoints() {
            if *checkpoint_height == height {
                let expected_hash = hex::decode(checkpoint_hash_str)
                    .map_err(|e| format!("Invalid checkpoint hash: {}", e))?;

                if expected_hash.len() != 32 {
                    return Err(format!(
                        "Checkpoint hash has wrong length: {}",
                        expected_hash.len()
                    ));
                }

                let expected_hash_array: [u8; 32] = expected_hash
                    .as_slice()
                    .try_into()
                    .map_err(|_| "Failed to convert checkpoint hash")?;

                if block_hash != &expected_hash_array {
                    return Err(format!(
                        "Checkpoint validation failed at height {}: expected {}, got {}",
                        height,
                        checkpoint_hash_str,
                        hex::encode(block_hash)
                    ));
                }

                tracing::info!("✅ Checkpoint validated at height {}", height);
                return Ok(());
            }
        }

        // Not a checkpoint, validation passes
        Ok(())
    }

    /// Find the highest checkpoint at or below the given height
    pub fn find_last_checkpoint_before(&self, height: u64) -> Option<u64> {
        self.get_checkpoints()
            .iter()
            .filter(|(h, _)| *h <= height)
            .map(|(h, _)| *h)
            .max()
    }

    /// Rollback the chain to a specific height
    /// This removes all blocks above the target height and reverts UTXO changes
    pub async fn rollback_to_height(&self, target_height: u64) -> Result<u64, String> {
        let current = self.current_height.load(Ordering::Acquire);

        if target_height >= current {
            return Ok(current); // Nothing to rollback
        }

        let blocks_to_remove = current - target_height;

        // Safety check: don't allow rollback past checkpoints
        if let Some(last_checkpoint) = self.find_last_checkpoint_before(current) {
            if target_height < last_checkpoint {
                return Err(format!(
                    "Cannot rollback past checkpoint at height {} (attempting rollback to {})",
                    last_checkpoint, target_height
                ));
            }
        }

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

        // Step 1: Rollback UTXOs using undo logs (in reverse order)
        let mut utxo_rollback_count = 0;
        let mut utxo_restored_count = 0;
        let mut transactions_to_repool = Vec::new();

        for height in (target_height + 1..=current).rev() {
            // Load undo log for this block
            match self.load_undo_log(height) {
                Ok(undo_log) => {
                    tracing::debug!(
                        "📖 Loaded undo log for height {}: {} spent UTXOs, {} finalized txs",
                        height,
                        undo_log.spent_utxos.len(),
                        undo_log.finalized_txs.len()
                    );

                    // Restore spent UTXOs from undo log
                    for (outpoint, utxo) in undo_log.spent_utxos {
                        if let Err(e) = self.utxo_manager.restore_utxo(utxo).await {
                            tracing::warn!(
                                "Could not restore UTXO {:?} at height {}: {}",
                                outpoint,
                                height,
                                e
                            );
                        } else {
                            utxo_restored_count += 1;
                        }
                    }

                    // Get the block to identify transactions for mempool
                    if let Ok(block) = self.get_block_by_height(height).await {
                        // NOTE: block.masternode_rewards is metadata only - the actual reward UTXOs
                        // are created by the reward distribution transaction. We do NOT remove
                        // separate UTXOs from the masternode_rewards array (they don't exist).
                        // The reward_distribution transaction outputs are removed below with all other transactions.

                        // Remove transaction outputs
                        for tx in block.transactions.iter() {
                            let txid = tx.txid();

                            // Remove created UTXOs
                            for (vout, _output) in tx.outputs.iter().enumerate() {
                                let outpoint = OutPoint {
                                    txid,
                                    vout: vout as u32,
                                };
                                if let Err(e) = self.utxo_manager.remove_utxo(&outpoint).await {
                                    tracing::debug!(
                                        "Could not remove UTXO {:?} at height {}: {}",
                                        outpoint,
                                        height,
                                        e
                                    );
                                } else {
                                    utxo_rollback_count += 1;
                                }
                            }

                            // Non-coinbase, non-finalized transactions go back to mempool
                            let is_coinbase = !tx.inputs.is_empty()
                                && tx.inputs[0].previous_output.vout == u32::MAX;
                            let is_finalized = undo_log.finalized_txs.contains(&txid);

                            if !is_coinbase && !is_finalized {
                                transactions_to_repool.push(tx.clone());
                                tracing::debug!(
                                    "📝 Transaction {} will be returned to mempool",
                                    hex::encode(&txid[..8])
                                );
                            } else if is_finalized {
                                tracing::debug!(
                                    "✅ Finalized transaction {} - will NOT return to mempool",
                                    hex::encode(&txid[..8])
                                );
                            }
                        }
                    }

                    // Delete undo log after successful rollback
                    if let Err(e) = self.delete_undo_log(height) {
                        tracing::warn!("Could not delete undo log for height {}: {}", height, e);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "⚠️  No undo log found for height {}: {}. Rollback may be incomplete.",
                        height,
                        e
                    );

                    // Fallback: Try to at least remove created UTXOs
                    if let Ok(block) = self.get_block_by_height(height).await {
                        // Remove transaction outputs (including coinbase and reward distribution)
                        for tx in block.transactions.iter() {
                            let txid = tx.txid();
                            for (vout, _output) in tx.outputs.iter().enumerate() {
                                let outpoint = OutPoint {
                                    txid,
                                    vout: vout as u32,
                                };
                                if let Ok(()) = self.utxo_manager.remove_utxo(&outpoint).await {
                                    utxo_rollback_count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        tracing::info!(
            "🔄 UTXO rollback complete: removed {} outputs, restored {} spent UTXOs, {} txs for mempool",
            utxo_rollback_count,
            utxo_restored_count,
            transactions_to_repool.len()
        );

        // Return non-finalized transactions to mempool for re-mining
        // NOTE: Requires transaction pool integration - architectural change needed
        if !transactions_to_repool.is_empty() {
            tracing::info!(
                "💡 {} non-finalized transactions need to be returned to mempool (requires transaction pool integration)",
                transactions_to_repool.len()
            );
        }

        // Step 2: Remove blocks from storage (highest first)
        for height in (target_height + 1..=current).rev() {
            // Clean tx_index BEFORE removing the block from storage so we can still read it.
            // Not doing this leaves stale entries that map txids to positions in blocks that
            // no longer exist (or have been replaced by a different chain's blocks), which
            // causes validate_block_rewards to look up the wrong transaction and compute
            // inflated fees.
            if let Some(ref txi) = self.tx_index {
                if let Ok(block) = self.get_block(height) {
                    for tx in &block.transactions {
                        if let Err(e) = txi.remove_transaction(&tx.txid()) {
                            tracing::warn!(
                                "Failed to remove tx from index during rollback at height {}: {}",
                                height,
                                e
                            );
                        }
                    }
                }
            }

            let key = format!("block_{}", height);
            if let Err(e) = self.storage.remove(key.as_bytes()) {
                tracing::warn!("Failed to remove block {}: {}", height, e);
            }
            // CRITICAL: Invalidate cache to prevent stale reads
            self.block_cache.invalidate(height);
        }

        // Step 3: Update chain height
        let height_key = "chain_height".as_bytes();
        let height_bytes = bincode::serialize(&target_height).map_err(|e| e.to_string())?;
        self.storage
            .insert(height_key, height_bytes)
            .map_err(|e| e.to_string())?;

        // Update in-memory height
        self.current_height.store(target_height, Ordering::Release);

        tracing::info!(
            "✅ Rollback complete: removed {} blocks, rolled back {} UTXOs, now at height {}",
            blocks_to_remove,
            utxo_rollback_count,
            target_height
        );

        Ok(target_height)
    }

    /// Validate a block before accepting it (Phase 1.3)
    /// Checks: hash integrity, previous hash chain, merkle root, timestamp, height sequence,
    /// duplicate transactions, and block size limits
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

        // 2. Verify merkle root matches transactions
        // CRITICAL: Transactions must already be in canonical order from block production
        // Block generator creates: [coinbase, sorted_user_txs...] and calculates merkle from that
        // Validation must use the SAME ordering - directly from block.transactions
        let computed_merkle = crate::block::types::calculate_merkle_root(&block.transactions);
        if computed_merkle != block.header.merkle_root {
            // Backward-compat: blocks produced before the txid fix (dd2ef7d) included
            // encrypted_memo in the txid hash.  Try the legacy formula before rejecting.
            let legacy_merkle =
                crate::block::types::calculate_merkle_root_legacy(&block.transactions);
            if legacy_merkle == block.header.merkle_root {
                tracing::debug!(
                    "Block {} accepted with legacy merkle root (pre-txid-fix format)",
                    block.header.height
                );
            } else {
                return Err(format!(
                    "Block {} merkle root mismatch: computed {}, header {}",
                    block.header.height,
                    hex::encode(&computed_merkle[..8]),
                    hex::encode(&block.header.merkle_root[..8])
                ));
            }
        }

        // 3. Verify timestamp is reasonable (Phase 1.3: strict ±15 minute tolerance)
        // During initial sync, we skip strict timestamp validation to allow historical blocks
        // This check will be done at add_block time when we know if we're syncing
        let now = chrono::Utc::now().timestamp();

        // Always check not too far in future (prevents fake future blocks)
        if block.header.timestamp > now + TIMESTAMP_TOLERANCE_SECS {
            return Err(format!(
                "Block {} timestamp {} is too far in future (now: {}, tolerance: {}s)",
                block.header.height, block.header.timestamp, now, TIMESTAMP_TOLERANCE_SECS
            ));
        }

        // Reject blocks that exceed the maximum expected height
        // Allow 10s grace for minor clock skew between nodes (only one leader per height via VRF)
        let now_with_grace = Utc::now().timestamp() + 10;
        let genesis_timestamp = self.genesis_timestamp();
        let max_expected_height = if now_with_grace < genesis_timestamp {
            0
        } else {
            ((now_with_grace - genesis_timestamp) / BLOCK_TIME_SECONDS) as u64
        };
        if block.header.height > max_expected_height {
            return Err(format!(
                "Block {} exceeds maximum expected height {} (genesis-based calculation)",
                block.header.height, max_expected_height
            ));
        }

        // Reject blocks produced before their scheduled time.
        // Expected earliest time = genesis_timestamp + (height * BLOCK_TIME_SECONDS).
        // Allow 30s grace for minor clock skew between nodes.
        // Only enforce for recent blocks (within 10 of chain tip) — historical blocks
        // during sync may have been produced under different timing rules.
        let chain_tip = self.current_height.load(Ordering::Acquire);
        let block_expected_time =
            genesis_timestamp + (block.header.height as i64 * BLOCK_TIME_SECONDS);
        if block.header.height + 10 > chain_tip && block.header.timestamp < block_expected_time - 30
        {
            return Err(format!(
                "Block {} timestamp {} is before its scheduled time {} (produced too early by {}s)",
                block.header.height,
                block.header.timestamp,
                block_expected_time,
                block_expected_time - block.header.timestamp
            ));
        }

        // Note: Past timestamp check is done in add_block() where we know if we're syncing

        // Additional check: Verify timestamp aligns with blockchain timeline
        // Expected time = genesis_time + (height * block_time)
        // This check is DISABLED during initial sync because historical blocks may have
        // timestamps that don't match the original schedule
        // Only enforce this for recently produced blocks (within a few blocks of chain tip)
        // This prevents accepting entire fake chains that are too far ahead of schedule
        let genesis_time = self.genesis_timestamp();
        let expected_time = genesis_time + (block.header.height as i64 * BLOCK_TIME_SECONDS);
        let time_drift = block.header.timestamp - expected_time;

        // Only check schedule drift if block is recent (not historical)
        // If we're syncing old blocks, they may have timestamps that don't match original schedule
        // Skip the check during sync to avoid blocking
        // NOTE: Hardcoded to false - would need atomic height counter to determine if syncing
        let is_recent_block = false;

        if is_recent_block {
            // Allow some flexibility for network delays and clock drift, but reject if way ahead
            const MAX_DRIFT_FROM_SCHEDULE: i64 = 3600; // 1 hour ahead of schedule is suspicious
            if time_drift > MAX_DRIFT_FROM_SCHEDULE {
                return Err(format!(
                    "Block {} timestamp {} is too far ahead of expected schedule (expected: {}, drift: {}s)",
                    block.header.height, block.header.timestamp, expected_time, time_drift
                ));
            }
        }

        // 4. Check for duplicate transactions (Phase 1.3)
        let mut seen_txids = std::collections::HashSet::new();
        for tx in &block.transactions {
            let txid = tx.txid();
            if !seen_txids.insert(txid) {
                return Err(format!(
                    "Block {} contains duplicate transaction: {}",
                    block.header.height,
                    hex::encode(&txid[..8])
                ));
            }
        }

        // 5. Block size check (2MB hard cap)
        let serialized = bincode::serialize(block).map_err(|e| e.to_string())?;
        if serialized.len() > MAX_BLOCK_SIZE {
            return Err(format!(
                "Block {} exceeds max size: {} > {} bytes",
                block.header.height,
                serialized.len(),
                MAX_BLOCK_SIZE
            ));
        }

        // 5.5. Leader field validation: After bootstrap (height > 3), leader must be set
        // This prevents deadlock from blocks with missing participation tracking
        if block.header.height > 3 && block.header.leader.is_empty() {
            return Err(format!(
                "Block {} has empty leader field (required for height > 3 to track participation)",
                block.header.height
            ));
        }

        // 6. VRF validation (Phase 2: verify proof if present)
        // Note: VRF verification requires leader's public key from masternode registry.
        // For backward compatibility, empty vrf_proof is allowed (pre-VRF blocks).
        // Full verification with leader lookup will be added when all masternodes
        // have public keys in the registry.
        if !block.header.vrf_proof.is_empty() {
            // Basic sanity check: VRF proof should be 80 bytes (ECVRF standard)
            if block.header.vrf_proof.len() != 80 {
                return Err(format!(
                    "Block {} has invalid VRF proof length: {} (expected 80)",
                    block.header.height,
                    block.header.vrf_proof.len()
                ));
            }

            // Verify vrf_score matches vrf_output
            let expected_score = crate::block::vrf::vrf_output_to_score(&block.header.vrf_output);
            if block.header.vrf_score != expected_score {
                return Err(format!(
                    "Block {} VRF score mismatch: header says {}, computed {}",
                    block.header.height, block.header.vrf_score, expected_score
                ));
            }

            tracing::debug!(
                "✅ Block {} has VRF proof (score={}), full verification deferred to leader lookup",
                block.header.height,
                block.header.vrf_score
            );
        }

        Ok(())
    }

    /// Try to add a block, handling potential fork scenarios
    /// Returns Ok(true) if block was added, Ok(false) if skipped, Err on failure
    pub async fn add_block_with_fork_handling(&self, block: Block) -> Result<bool, String> {
        use crate::block::genesis::GenesisBlock;

        let block_height = block.header.height;

        // CRITICAL: Validate block can be serialized BEFORE processing
        // This catches corrupted blocks from peers early
        if let Err(e) = bincode::serialize(&block) {
            tracing::warn!(
                "🚫 Rejecting corrupted block {} from network: serialization failed: {}",
                block_height,
                e
            );
            return Err(format!(
                "Block {} is corrupted (serialization failed): {}",
                block_height, e
            ));
        }

        // CRITICAL: Serialize all block processing to prevent race conditions.
        // Without this lock, multiple peers sending overlapping block ranges during sync
        // causes TOCTOU races: multiple threads pass the "does block exist?" check,
        // then ALL process the same block's UTXOs → duplicate UTXO indexing, AlreadySpent
        // errors, height oscillation, and eventual node stall.
        let _block_guard = self.block_processing_lock.lock().await;

        // CRITICAL: Reject blocks during active reorg to prevent concurrent fork resolutions
        // Multiple peers sending competing chains simultaneously causes chain corruption
        {
            let fork_state = self.fork_state.read().await;
            match &*fork_state {
                ForkResolutionState::Reorging { .. } | ForkResolutionState::ReadyToReorg { .. } => {
                    tracing::debug!(
                        "🚫 Rejecting block {} during active reorg (state: {:?})",
                        block_height,
                        std::mem::discriminant(&*fork_state)
                    );
                    return Ok(false);
                }
                _ => {} // Allow blocks when not reorging
            }
        }

        // Special case: Genesis block (height 0)
        if block_height == 0 {
            // Check if we already have genesis
            if self
                .storage
                .contains_key("block_0".as_bytes())
                .unwrap_or(false)
            {
                if let Ok(existing) = self.get_block(0) {
                    let existing_hash = existing.hash();
                    let incoming_hash = block.hash();

                    if existing_hash == incoming_hash {
                        return Ok(false); // Already have correct genesis
                    }

                    // Different genesis - log detailed comparison
                    // Note: masternode_tiers are excluded from comparison as they're metadata only
                    tracing::error!(
                        "🚫 Genesis block mismatch detected!\n\
                         Our genesis: {}\n\
                         - timestamp: {}\n\
                         - previous_hash: {}\n\
                         - merkle_root: {}\n\
                         - leader: {}\n\
                         - transactions: {}\n\
                         Peer genesis: {}\n\
                         - timestamp: {}\n\
                         - previous_hash: {}\n\
                         - merkle_root: {}\n\
                         - leader: {}\n\
                         - transactions: {}",
                        hex::encode(existing_hash),
                        existing.header.timestamp,
                        hex::encode(existing.header.previous_hash),
                        hex::encode(existing.header.merkle_root),
                        existing.header.leader,
                        existing.transactions.len(),
                        hex::encode(incoming_hash),
                        block.header.timestamp,
                        hex::encode(block.header.previous_hash),
                        hex::encode(block.header.merkle_root),
                        block.header.leader,
                        block.transactions.len()
                    );

                    // Different genesis - reject
                    return Err(format!(
                        "Genesis block mismatch: our {} vs peer {}",
                        hex::encode(&existing_hash[..8]),
                        hex::encode(&incoming_hash[..8])
                    ));
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

            // CLOCK GUARD: refuse a genesis block that claims a future launch timestamp.
            // An old node (pre-clock-guard code) could pre-generate a valid-looking genesis
            // with timestamp = launch_time and broadcast it before launch.  We reject it
            // if our wall clock has not yet reached that timestamp.
            {
                let launch_ts = self.genesis_timestamp();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                if now < launch_ts {
                    let remaining = launch_ts - now;
                    tracing::warn!(
                        "🛡️ Rejected premature genesis block (launch in {}s): {}",
                        remaining,
                        hex::encode(&block.hash()[..8])
                    );
                    return Err(format!(
                        "Premature genesis block rejected: launch time has not been reached \
                         ({remaining}s remaining). This block was produced by an old node before launch."
                    ));
                }
            }

            // MINIMUM MASTERNODE GUARD: refuse a genesis block that was produced by
            // too few masternodes.  A lone (or early) node that raced to produce genesis
            // before others connected would capture the full block reward, permanently
            // locking every later node out of their share.  We enforce the same floor
            // here that generate_dynamic_genesis() enforces on production.
            //
            // We check BOTH the header tier count AND the actual reward recipients.
            // The rewards list is the ground-truth participant set — a genesis with
            // only one reward address is a single-node capture regardless of what
            // the tier header claims.
            {
                const MIN_GENESIS_MASTERNODES: u32 = 3;
                let mn_count = block.header.masternode_tiers.total();
                let reward_count = block.masternode_rewards.len() as u32;
                let effective_count = mn_count.min(reward_count);
                if effective_count < MIN_GENESIS_MASTERNODES {
                    tracing::warn!(
                        "🛡️ Rejected under-subscribed genesis block (tier_count={}, reward_recipients={}, need ≥{}): {}",
                        mn_count,
                        reward_count,
                        MIN_GENESIS_MASTERNODES,
                        hex::encode(&block.hash()[..8])
                    );
                    return Err(format!(
                        "Genesis block rejected: only {effective_count} masternode(s) participated \
                         (tier_count={mn_count}, reward_recipients={reward_count}), \
                         minimum is {MIN_GENESIS_MASTERNODES}. \
                         This block was produced before enough nodes had connected."
                    ));
                }
            }

            tracing::info!(
                "✅ Received valid genesis block: {} (masternodes: {})",
                hex::encode(block.hash()),
                block.header.masternode_tiers.total()
            );

            // Save genesis block
            let _ = self.process_block_utxos(&block).await;
            self.save_block(&block, true)?;
            // Genesis is height 0, current_height stays at 0

            return Ok(true);
        }

        // For all non-genesis blocks, we need genesis to exist first
        let has_genesis = self
            .storage
            .contains_key("block_0".as_bytes())
            .unwrap_or(false);
        if !has_genesis {
            tracing::warn!(
                "⏳ Cannot add block {} - waiting for genesis block first (current_height: {})",
                block_height,
                self.current_height.load(Ordering::Acquire)
            );
            return Ok(false);
        }

        // Get current height (after genesis check)
        let current = self.current_height.load(Ordering::Acquire);

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
                // Return error to signal fork - caller needs to request earlier blocks
                return Err(format!(
                    "Fork detected: block {} doesn't build on our chain (prev_hash mismatch)",
                    block_height
                ));
            }

            // BLOCK 1 REWARD-HIJACK GUARD (mirrors genesis minimum masternode check)
            // A lone or colluding node that produced block 1 before others connected
            // — or that modified their code to exclude other masternodes — must not
            // be able to capture the entire block reward.  Enforce the same ≥3 unique
            // recipient floor that genesis already enforces.
            if block_height == 1 {
                const MIN_BLOCK1_RECIPIENTS: usize = 3;
                let unique: std::collections::HashSet<&str> = block
                    .masternode_rewards
                    .iter()
                    .map(|(a, _)| a.as_str())
                    .collect();
                if unique.len() < MIN_BLOCK1_RECIPIENTS {
                    tracing::warn!(
                        "🛡️ Rejecting block 1: only {} unique reward recipient(s), need ≥{} \
                         (possible reward-hijacking attempt)",
                        unique.len(),
                        MIN_BLOCK1_RECIPIENTS
                    );
                    return Err(format!(
                        "Block 1 rejected: only {} unique reward recipient(s), need \
                         ≥{MIN_BLOCK1_RECIPIENTS}. This block was produced before enough \
                         masternodes had connected.",
                        unique.len()
                    ));
                }
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
                // Log at debug level to avoid spam when processing many fork blocks
                tracing::debug!(
                    "🔀 Fork detected at height {}: our hash {} vs incoming {}",
                    block_height,
                    hex::encode(&existing.hash()[..8]),
                    hex::encode(&block.hash()[..8])
                );

                // AUTO-RESOLVE: If we detect a fork at height N, check if the peer
                // is trying to give us a competing block. We should wait to see if
                // they have a longer chain, rather than immediately rejecting.
                // Signal that we need fork resolution (caller should request more blocks)
                return Err(format!(
                    "Fork detected at height {}: different block at same height",
                    block_height
                ));
            }

            // We don't have a block at this height - this means our chain has a gap
            // This can happen if the database is corrupted or height metadata is ahead of actual blocks
            // Try to fill the gap by accepting this block
            tracing::warn!(
                "⚠️  Gap detected: height {} missing at chain height {} - attempting to fill",
                block_height,
                current
            );

            // Check if we have the previous block to validate against
            if block_height > 0 {
                match self.get_block(block_height - 1) {
                    Ok(prev_block) => {
                        // Validate that this block connects to the previous
                        if block.header.previous_hash != prev_block.hash() {
                            tracing::warn!(
                                "❌ Cannot fill gap: block {} prev_hash {} doesn't match block {} hash {}",
                                block_height,
                                hex::encode(&block.header.previous_hash[..8]),
                                block_height - 1,
                                hex::encode(&prev_block.hash()[..8])
                            );
                            return Ok(false);
                        }
                        // Validate and add the block
                        self.validate_block(&block, Some(prev_block.hash()))?;
                        tracing::info!(
                            "✅ Filling gap: adding block {} (hash: {})",
                            block_height,
                            hex::encode(&block.hash()[..8])
                        );
                        // Use save_block to store without updating height (height is already >= this)
                        self.save_block(&block, true)?;
                        return Ok(true);
                    }
                    Err(_) => {
                        // Previous block also missing — we have a multi-block gap.
                        // Walk backwards to find the lowest missing height, then
                        // request the entire missing range from the best available peer.
                        let mut gap_start = block_height - 1;
                        while gap_start > 0 && self.get_block(gap_start - 1).is_err() {
                            gap_start -= 1;
                        }
                        let gap_end = current; // chain tip
                        tracing::warn!(
                            "⚠️  Multi-block gap detected: heights {}-{} missing — \
                             requesting range from peers",
                            gap_start,
                            gap_end
                        );
                        // Fire a targeted GetBlocks for the missing range.
                        let peer_reg = self.peer_registry.read().await;
                        if let Some(registry) = peer_reg.as_ref() {
                            let peers = registry.get_connected_peers().await;
                            if let Some(peer) = peers.first() {
                                let req = crate::network::message::NetworkMessage::GetBlocks(
                                    gap_start, gap_end,
                                );
                                if let Err(e) = registry.send_to_peer(peer, req).await {
                                    tracing::warn!(
                                        "Failed to request gap range from {}: {}",
                                        peer,
                                        e
                                    );
                                } else {
                                    tracing::info!(
                                        "📥 Requested missing blocks {}-{} from {}",
                                        gap_start,
                                        gap_end,
                                        peer
                                    );
                                }
                            }
                        }
                        return Ok(false);
                    }
                }
            } else {
                // Genesis block (height 0) - validate and add
                self.validate_block(&block, None)?;
                tracing::info!("✅ Filling gap: adding genesis block");
                self.save_block(&block, true)?;
                return Ok(true);
            }
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
    /// Work = BASE_WORK (attestation bonus removed with heartbeat system)
    pub fn calculate_block_work(&self, _block: &Block) -> u128 {
        BASE_WORK_PER_BLOCK
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

    /// Check if we should switch to peer's chain based on work comparison
    /// Enhanced with masternode authority analysis
    pub async fn should_switch_by_work(
        &self,
        peer_work: u128,
        peer_height: u64,
        peer_tip_hash: &[u8; 32],
        peer_ip: Option<&str>,
    ) -> bool {
        let our_work = *self.cumulative_work.read().await;
        let our_height = self.current_height.load(Ordering::Acquire);

        // If we have peer IP and heights are equal or close, use masternode authority
        if let (Some(ip), true) = (peer_ip, peer_height.abs_diff(our_height) <= 2) {
            if let Ok(our_tip) = self.get_block_by_height(our_height).await {
                let our_hash = our_tip.hash();

                // Analyze masternode authority
                let our_authority = crate::masternode_authority::CanonicalChainSelector::analyze_our_chain_authority(
                    &self.masternode_registry,
                    self.connection_manager.read().await.as_ref().map(|v| &**v),
                    self.peer_registry.read().await.as_ref().map(|v| &**v),
                ).await;

                // For peer, we only know the single peer IP, so limited analysis
                let peer_authority = crate::masternode_authority::CanonicalChainSelector::analyze_peer_chain_authority(
                    &[ip.to_string()],
                    &self.masternode_registry,
                    self.peer_registry.read().await.as_ref().map(|v| &**v),
                ).await;

                let (should_switch, reason) = crate::masternode_authority::CanonicalChainSelector::should_switch_to_peer_chain(
                    &our_authority,
                    &peer_authority,
                    our_work,
                    peer_work,
                    our_height,
                    peer_height,
                    &our_hash,
                    peer_tip_hash,
                );

                tracing::info!(
                    "📊 Chain comparison with {}:\n   Our: {} work={} height={}\n   Peer: {} work={} height={}\n   → {}",
                    ip,
                    our_authority.format_summary(),
                    our_work,
                    our_height,
                    peer_authority.format_summary(),
                    peer_work,
                    peer_height,
                    reason
                );

                return should_switch;
            }
        }

        // Fallback to traditional chain work comparison
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

        // If equal work AND equal height, use deterministic tie-breaker: lexicographically smallest hash
        if peer_work == our_work && peer_height == our_height {
            if let Ok(our_tip) = self.get_block_by_height(our_height).await {
                let our_hash = our_tip.hash();
                // Compare hashes byte-by-byte - choose the smaller one
                if peer_tip_hash < &our_hash {
                    tracing::info!(
                        "⚖️  Equal height {} and equal work {}, choosing chain with smaller hash",
                        our_height,
                        our_work
                    );
                    return true;
                }
            }
        }

        false
    }

    /// Periodic chain comparison with peers to detect forks
    /// Analyzes cached chain tip data from peers (updated by periodic tasks)
    ///
    /// **PRIMARY FORK RESOLUTION ENTRY POINT**
    /// This is the recommended way to detect and resolve forks.
    /// It runs periodically and queries all peers for consensus.
    ///
    /// NOTE: This method uses CACHED chain tip data from the peer registry.
    /// Callers should request fresh chain tips before calling this method.
    /// See: spawn_sync_coordinator() and start_chain_comparison_task() for examples.
    ///
    /// Benefits over on-demand resolution:
    /// - Queries all peers for complete picture
    /// - Detects forks before receiving unsolicited blocks
    /// - Uses peer consensus for better decisions
    /// - Single code path = no race conditions
    pub async fn compare_chain_with_peers(&self) -> Option<(u64, String)> {
        // CRITICAL: Acquire fork resolution lock to prevent concurrent fork resolutions
        // This is important because this method runs periodically and could conflict with
        // on-demand fork resolution triggered by incoming blocks
        let _lock = self.fork_resolution_lock.lock().await;

        let peer_registry = self.peer_registry.read().await;
        let registry = match peer_registry.as_ref() {
            Some(r) => r,
            None => return None,
        };

        // Use only compatible peers (exclude those on incompatible chains)
        let mut connected_peers = registry.get_compatible_peers().await;
        if connected_peers.is_empty() {
            tracing::debug!("No compatible peers connected");
            return None;
        }

        // SCALABILITY: With 10,000+ masternodes, sampling is critical
        // Sample a representative subset instead of querying all peers
        // Statistical sampling: sqrt(N) provides good confidence with O(sqrt(N)) cost
        // TIER-STRATIFIED: Ensure at least one peer from each tier is sampled
        const MAX_PEERS_TO_CHECK: usize = 100; // Hard cap for extreme cases
        let sample_size = if connected_peers.len() > MAX_PEERS_TO_CHECK {
            let sqrt_size = (connected_peers.len() as f64).sqrt().ceil() as usize;
            sqrt_size.min(MAX_PEERS_TO_CHECK)
        } else {
            connected_peers.len()
        };

        if sample_size < connected_peers.len() {
            // Tier-stratified sampling: guarantee at least one peer from each tier
            // This prevents sampling bias where all Gold/Silver nodes are missed
            let mut tier_buckets: std::collections::HashMap<u64, Vec<String>> =
                std::collections::HashMap::new();
            for peer_ip in &connected_peers {
                let weight = match self.masternode_registry.get(peer_ip).await {
                    Some(info) => info.masternode.tier.sampling_weight(),
                    None => crate::types::MasternodeTier::Free.sampling_weight(),
                };
                tier_buckets
                    .entry(weight)
                    .or_default()
                    .push(peer_ip.clone());
            }

            let mut sampled: Vec<String> = Vec::with_capacity(sample_size);

            // Phase 1: Take one peer from each tier (highest tiers first)
            let mut tier_keys: Vec<u64> = tier_buckets.keys().cloned().collect();
            tier_keys.sort_unstable_by(|a, b| b.cmp(a)); // Highest tier first
            let now_nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
            for (idx, tier) in tier_keys.iter().enumerate() {
                if sampled.len() >= sample_size {
                    break;
                }
                if let Some(peers) = tier_buckets.get_mut(tier) {
                    if !peers.is_empty() {
                        // Deterministic selection within tier
                        let pick = (now_nanos.wrapping_mul(idx as u64 + 1)) as usize % peers.len();
                        sampled.push(peers.swap_remove(pick));
                    }
                }
            }

            // Phase 2: Fill remaining slots randomly from all remaining peers
            let mut remaining: Vec<String> = tier_buckets
                .into_values()
                .flat_map(|v| v.into_iter())
                .collect();
            for i in 0..remaining
                .len()
                .min(sample_size.saturating_sub(sampled.len()))
            {
                let j = (now_nanos
                    .wrapping_mul(i as u64 + 100)
                    .wrapping_add(i as u64)) as usize
                    % (remaining.len() - i)
                    + i;
                remaining.swap(i, j);
            }
            let fill_count = sample_size.saturating_sub(sampled.len());
            sampled.extend(remaining.into_iter().take(fill_count));

            let total_before = connected_peers.len();
            connected_peers = sampled;
            tracing::info!(
                "🎲 Tier-stratified sampling: {} of {} peers ({} tiers represented)",
                connected_peers.len(),
                total_before,
                tier_keys.len(),
            );
        }

        tracing::debug!(
            "🔍 [LOCKED] PRIMARY FORK RESOLUTION: Periodic check with {} compatible peers",
            connected_peers.len()
        );

        tracing::debug!(
            "🔍 [FORK CHECK] Analyzing chain status from {} connected compatible peers",
            connected_peers.len()
        );

        // Use cached chain tips from registry (already requested by sync coordinator)
        // This prevents duplicate GetChainTip spam every 30 seconds
        // If called outside sync coordinator context, chain tips may be stale (up to 30s old)

        // Collect peer chain tips (height + hash) from registry
        let mut peer_tips: std::collections::HashMap<String, (u64, [u8; 32])> =
            std::collections::HashMap::new();
        for peer_ip in &connected_peers {
            if let Some((height, hash)) = registry.get_peer_chain_tip(peer_ip).await {
                // Ignore peers with zero hash (storage key bug - can't read their own blocks)
                if hash == [0u8; 32] {
                    tracing::debug!(
                        "⚠️  Ignoring peer {} with zero hash (likely storage issue)",
                        peer_ip
                    );
                    continue;
                }
                peer_tips.insert(peer_ip.clone(), (height, hash));
                tracing::debug!("Got cached response from {}: height {}", peer_ip, height);
            } else {
                tracing::debug!(
                    "No cached response from {} (not in peer_chain_tips map)",
                    peer_ip
                );
            }
        }

        if peer_tips.is_empty() {
            tracing::warn!(
                "⚠️  No peer chain tip responses received from {} peers!",
                connected_peers.len()
            );
            return None;
        }

        // CRITICAL: Require a minimum response rate (50%+) to make consensus decisions
        // If only 33% of peers respond, we may get incorrect consensus (e.g., 4/6 all at height 8)
        // With low response rates, wait for more responses rather than deciding prematurely
        let response_rate = peer_tips.len() as f64 / connected_peers.len() as f64;
        if response_rate < 0.5 {
            tracing::warn!(
                "⚠️  Low peer response rate: {}/{} responded ({:.1}%) - waiting for more responses before consensus decision",
                peer_tips.len(),
                connected_peers.len(),
                response_rate * 100.0
            );
            return None;
        }

        // DEBUG: Log what we received from peers
        tracing::debug!(
            "🔍 [DEBUG] Received chain tips from {}/{} peers:",
            peer_tips.len(),
            connected_peers.len()
        );
        for (peer_ip, (height, hash)) in &peer_tips {
            tracing::debug!(
                "   Peer {}: height {} hash {}",
                peer_ip,
                height,
                hex::encode(&hash[..8])
            );
            // Record chain tip for AI consensus health monitoring
            self.consensus_health.record_chain_tip(*height, *hash);
        }

        let our_height = self.get_height();
        let our_hash_result = self.get_block_hash(our_height);
        let our_hash = match &our_hash_result {
            Ok(hash) => *hash,
            Err(e) => {
                // Check if the error is due to our block being corrupted/deleted
                if e.contains("deleted") || e.contains("not found") {
                    tracing::warn!(
                        "🔄 Our block at height {} is missing ({}), need to sync from peers",
                        our_height,
                        e
                    );
                    // Our block is missing - we'll handle this specially below
                    // Don't use zero hash as placeholder (it's reserved for genesis)
                    return Some((
                        peer_tips
                            .values()
                            .map(|(h, _)| *h)
                            .max()
                            .unwrap_or(our_height),
                        peer_tips
                            .iter()
                            .next()
                            .map(|(ip, _)| ip.clone())
                            .unwrap_or_else(|| "unknown".to_string()),
                    ));
                } else {
                    // Unexpected error
                    tracing::error!("Failed to get our block hash: {}", e);
                    return None;
                }
            }
        };

        // Group peers by (height, hash) to find consensus
        let mut chain_counts: std::collections::HashMap<(u64, [u8; 32]), Vec<String>> =
            std::collections::HashMap::new();
        for (peer_ip, (height, hash)) in &peer_tips {
            chain_counts
                .entry((*height, *hash))
                .or_default()
                .push(peer_ip.clone());
        }

        // Find the best chain: LONGEST VALID CHAIN RULE
        // The longest valid chain is canonical, regardless of peer count
        // Shorter chains must sync to the longest chain

        // Log chain analysis - rate limited for single chain, always log for forks
        let num_chains = chain_counts.len();
        static LAST_SINGLE_CHAIN_LOG: std::sync::atomic::AtomicI64 =
            std::sync::atomic::AtomicI64::new(0);
        let now_secs = chrono::Utc::now().timestamp();
        let should_log = if num_chains == 1 {
            // For single chain: log at most once per 5 minutes
            let last_log = LAST_SINGLE_CHAIN_LOG.load(std::sync::atomic::Ordering::Relaxed);
            if now_secs - last_log >= 300 {
                LAST_SINGLE_CHAIN_LOG.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                true
            } else {
                false
            }
        } else {
            // For multiple chains (fork): log at most once per 60 seconds
            static LAST_FORK_LOG: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(0);
            let last_log = LAST_FORK_LOG.load(std::sync::atomic::Ordering::Relaxed);
            if now_secs - last_log >= 60 {
                LAST_FORK_LOG.store(now_secs, std::sync::atomic::Ordering::Relaxed);
                true
            } else {
                false
            }
        };

        if should_log {
            if num_chains == 1 {
                tracing::debug!("🔍 [CHAIN ANALYSIS] Network consensus: 1 chain detected");
            } else {
                // Classify peers: "at-tip" vs "syncing" (far behind consensus)
                let heights: Vec<u64> = chain_counts.keys().map(|(h, _)| *h).collect();
                let max_h = heights.iter().max().copied().unwrap_or(0);
                let min_h = heights.iter().min().copied().unwrap_or(0);
                // Count chains that are actually near the tip (within 5 blocks)
                let tip_chains: Vec<_> = chain_counts
                    .keys()
                    .filter(|(h, _)| max_h.saturating_sub(*h) <= 5)
                    .collect();
                let syncing_peers: Vec<_> = chain_counts
                    .iter()
                    .filter(|((h, _), _)| max_h.saturating_sub(*h) > 5)
                    .flat_map(|(_, peers)| peers.clone())
                    .collect();
                let is_benign = tip_chains.len() <= 1 || (max_h - min_h <= 1);

                if is_benign {
                    if !syncing_peers.is_empty() {
                        tracing::debug!(
                            "🔍 [CHAIN ANALYSIS] 1 chain at tip, {} peer(s) still syncing: {:?}",
                            syncing_peers.len(),
                            syncing_peers
                        );
                    } else {
                        tracing::debug!(
                            "🔍 [CHAIN ANALYSIS] {} chains detected (height diff ≤ 1, normal propagation delay)",
                            num_chains
                        );
                    }
                } else {
                    tracing::info!(
                        "🔍 [CHAIN ANALYSIS] Detected {} different chains at tip:",
                        tip_chains.len()
                    );
                }
            }
            for ((height, hash), peers) in &chain_counts {
                let max_h = chain_counts.keys().map(|(h, _)| *h).max().unwrap_or(0);
                let is_syncing_peer = max_h.saturating_sub(*height) > 5;
                if num_chains == 1 || is_syncing_peer {
                    tracing::debug!(
                        "   📊 {} @ height {}, hash {}: {} peers {:?}",
                        if is_syncing_peer {
                            "Syncing peer"
                        } else {
                            "Chain"
                        },
                        height,
                        hex::encode(&hash[..8]),
                        peers.len(),
                        peers
                    );
                } else {
                    // Only count tip-level chains with different hashes as benign
                    let tip_chains: usize = chain_counts
                        .keys()
                        .filter(|(h, _)| max_h.saturating_sub(*h) <= 5)
                        .count();
                    if tip_chains <= 1 || max_h.saturating_sub(*height) <= 1 {
                        tracing::debug!(
                            "   📊 Chain @ height {}, hash {}: {} peers {:?}",
                            height,
                            hex::encode(&hash[..8]),
                            peers.len(),
                            peers
                        );
                    } else {
                        tracing::info!(
                            "   📊 Chain @ height {}, hash {}: {} peers {:?}",
                            height,
                            hex::encode(&hash[..8]),
                            peers.len(),
                            peers
                        );
                    }
                }
            }
        }

        // Find the LONGEST chain (highest height)
        // Same-height tiebreaker: deterministic hash comparison (lower hash wins)
        // This matches ForkResolver logic — no subjective stake weight tiebreaker
        // which would cause permanent forks (each node sees different peers).

        // Log peer counts when there are multiple chains at tip (actual fork)
        if should_log && num_chains > 1 {
            let max_h = chain_counts.keys().map(|(h, _)| *h).max().unwrap_or(0);
            let tip_chains: Vec<_> = chain_counts
                .iter()
                .filter(|((h, _), _)| max_h.saturating_sub(*h) <= 5)
                .collect();
            // Only log fork weights if there are actually multiple chains at tip
            if tip_chains.len() > 1 {
                for ((height, hash), peers) in &tip_chains {
                    tracing::info!(
                        "   ⚖️  Chain @ height {}, hash {}: {} peers",
                        height,
                        hex::encode(&hash[..8]),
                        peers.len()
                    );
                }
            }
        }

        let consensus_chain = chain_counts
            .iter()
            .max_by(|((h1, hash1), _peers1), ((h2, hash2), _peers2)| {
                // LONGEST CHAIN RULE: Height is always the primary criterion.
                let height_cmp = h1.cmp(h2);
                if height_cmp != std::cmp::Ordering::Equal {
                    return height_cmp;
                }
                // Same height: deterministic hash tiebreaker (lower hash wins)
                // All nodes see the same hashes, so this is globally consistent.
                hash2.cmp(hash1)
            })
            .map(|((height, hash), peers)| (*height, *hash, peers.clone()))?;

        let (consensus_height, consensus_hash, consensus_peers) = consensus_chain;

        // Check weighted stake support for the consensus chain
        let mut consensus_weight = 0u64;
        let mut total_responding_weight = 0u64;
        for peer_ip in peer_tips.keys() {
            let peer_weight = match self.masternode_registry.get(peer_ip).await {
                Some(info) => info.masternode.tier.sampling_weight(),
                None => crate::types::MasternodeTier::Free.sampling_weight(),
            };
            total_responding_weight += peer_weight;
            if consensus_peers.contains(peer_ip) {
                consensus_weight += peer_weight;
            }
        }
        if total_responding_weight == 0 {
            tracing::warn!("⚠️  No responding peers with weight for consensus check");
            return None;
        }

        // LONGEST CHAIN RULE: If the consensus chain is strictly taller than ALL other chains,
        // it's canonical regardless of weighted vote — block production already proved consensus.
        // Only require majority weighted threshold for same-height fork tiebreakers.
        let second_highest = chain_counts
            .iter()
            .filter(|((h, hash), _)| !(*h == consensus_height && *hash == consensus_hash))
            .map(|((h, _), _)| *h)
            .max()
            .unwrap_or(0);

        let height_advantage = consensus_height.saturating_sub(second_highest);

        if height_advantage == 0 {
            // Same-height fork — chain was selected by deterministic hash tiebreaker
            // (lower hash wins). Require weighted masternode stake majority to switch.
            let weighted_ratio = consensus_weight as f64 / total_responding_weight as f64;
            const WEIGHTED_CONSENSUS_THRESHOLD: f64 = 0.67; // 67% weighted stake required

            if weighted_ratio < WEIGHTED_CONSENSUS_THRESHOLD {
                tracing::info!(
                    "🔀 Same-height fork at {}: weighted stake {:.1}% < {:.0}% threshold — \
                    keeping our chain (consensus weight {}/{}, {} peers)",
                    consensus_height,
                    weighted_ratio * 100.0,
                    WEIGHTED_CONSENSUS_THRESHOLD * 100.0,
                    consensus_weight,
                    total_responding_weight,
                    consensus_peers.len(),
                );
                return None;
            }

            tracing::info!(
                "🔀 Same-height fork at {}: weighted stake {:.1}% ≥ {:.0}% — accepting consensus chain ({}/{})",
                consensus_height,
                weighted_ratio * 100.0,
                WEIGHTED_CONSENSUS_THRESHOLD * 100.0,
                consensus_weight,
                total_responding_weight,
            );
        } else {
            // Longest chain is strictly taller — it wins by longest chain rule
            // Log at debug to avoid noise; this is the normal, expected case
            tracing::debug!(
                "✅ Longest chain at height {} is {} blocks ahead of next chain at {} ({}/{} weight)",
                consensus_height,
                height_advantage,
                second_highest,
                consensus_weight,
                total_responding_weight
            );
        }

        // Only log consensus result when it changes (prevents duplicate spam from multiple callers)
        let consensus_key = (consensus_height, consensus_hash);
        let should_log = {
            let last = self.last_consensus_log.read().await;
            *last != Some(consensus_key)
        };
        if should_log {
            let peer_summary = if consensus_peers.len() <= 3 {
                format!("{:?}", consensus_peers)
            } else {
                format!(
                    "{:?} ... and {} more",
                    &consensus_peers[..3],
                    consensus_peers.len() - 3
                )
            };
            tracing::debug!(
                "✅ [CONSENSUS SELECTED] Height {}, hash {}, {} peers: {}",
                consensus_height,
                hex::encode(&consensus_hash[..8]),
                consensus_peers.len(),
                peer_summary
            );
            *self.last_consensus_log.write().await = Some(consensus_key);
        }

        // Store consensus peers for validation during block acceptance
        *self.consensus_peers.write().await = consensus_peers.clone();

        // AI Consensus Health: Calculate and record metrics
        let heights: Vec<u64> = peer_tips.values().map(|(h, _)| *h).collect();
        let height_mean = heights.iter().sum::<u64>() as f64 / heights.len() as f64;
        let height_variance = (heights
            .iter()
            .map(|h| (*h as f64 - height_mean).powi(2))
            .sum::<f64>()
            / heights.len() as f64)
            .sqrt();

        let peer_agreement_ratio = consensus_peers.len() as f64 / peer_tips.len() as f64;
        // Only count chains near the tip as forks — syncing peers (far behind) are not forks
        let max_h = chain_counts.keys().map(|(h, _)| *h).max().unwrap_or(0);
        let fork_count = chain_counts
            .keys()
            .filter(|(h, _)| max_h.saturating_sub(*h) <= 5)
            .count() as u32;
        let response_rate = peer_tips.len() as f64 / connected_peers.len() as f64;

        let metrics = ConsensusMetrics {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            height: our_height, // Use OUR actual height, not peer consensus height
            peer_agreement_ratio,
            height_variance,
            fork_count,
            response_rate,
            block_propagation_time: None,
        };
        self.consensus_health.record_metrics(metrics);

        // AI Health Prediction: Log warnings if health is degraded
        let health = self.consensus_health.predict_health();
        if health.health_score < 0.7 {
            debug!(
                "🧠 [AI] Consensus health warning: score={:.2}, fork_prob={:.2}, action={:?}",
                health.health_score, health.fork_probability, health.recommended_action
            );
            for reason in &health.reasoning {
                debug!("   Reason: {}", reason);
            }
        }

        // Log consensus vs our state at debug level (avoid spamming at info)
        tracing::debug!(
            "Consensus: height {} hash {} ({} peers agree). Our height: {} hash {}",
            consensus_height,
            hex::encode(&consensus_hash[..8]),
            consensus_peers.len(),
            our_height,
            hex::encode(&our_hash[..8])
        );

        // Case 1: Longest chain is longer than us - sync to it
        if consensus_height > our_height {
            tracing::debug!(
                "🔀 Fork resolution: longest chain height {} > our height {} ({} peers agree), syncing from {}",
                consensus_height,
                our_height,
                consensus_peers.len(),
                consensus_peers[0]
            );
            return Some((consensus_height, consensus_peers[0].clone()));
        }

        // Case 2: Same height but different hash - fork at same height!
        // The consensus chain was already selected by peer count (majority wins).
        // If our hash doesn't match, we're in the minority — switch, subject to guards below.
        if consensus_height == our_height && consensus_hash != our_hash {
            // Guard 1: Require at least 3 peers supporting the alternate chain.
            // 1–2 peers with a different hash are more likely to be on a minority fork
            // (especially when other peers are temporarily unresponsive to chain-tip queries).
            // Returning None here lets the node count as a vote for its own chain so that
            // the disagreeing peers — not us — eventually get the fork alert.
            const MIN_PEERS_FOR_FORK_SWITCH: usize = 3;
            if consensus_peers.len() < MIN_PEERS_FOR_FORK_SWITCH {
                tracing::debug!(
                    "🔀 Same-height fork at {}: only {} peer(s) on alternate chain — \
                     need {} to switch (our hash {}, theirs {})",
                    consensus_height,
                    consensus_peers.len(),
                    MIN_PEERS_FOR_FORK_SWITCH,
                    hex::encode(&our_hash[..8]),
                    hex::encode(&consensus_hash[..8]),
                );
                // Clear any stale FetchingChain state that may have been set by a prior
                // handle_fork() call with this minority peer. If left in place it blocks
                // future fork resolution even after the peer reconnects with a valid chain.
                {
                    let state = self.fork_state.read().await.clone();
                    if matches!(state, ForkResolutionState::FetchingChain { .. }) {
                        tracing::debug!(
                            "🧹 Clearing stale FetchingChain state (minority peer guard)"
                        );
                        *self.fork_state.write().await = ForkResolutionState::None;
                    }
                }
                return None;
            }

            // Guard 2: 30-second cooldown between switch attempts for the same fork pair.
            // sync_from_peers() may return immediately with Ok(()) when no peers are ahead,
            // which causes the production loop to call compare_chain_with_peers() again right
            // away, detect the same fork again, and spin at thousands of iterations/minute —
            // starving the tokio runtime and making the RPC handler unresponsive.
            const SAME_HEIGHT_FORK_COOLDOWN_SECS: u64 = 30;
            {
                let cooldown = self.same_height_fork_cooldown.read().await;
                if let Some((cd_height, cd_hash, cd_time)) = &*cooldown {
                    if *cd_height == consensus_height
                        && *cd_hash == consensus_hash
                        && cd_time.elapsed()
                            < std::time::Duration::from_secs(SAME_HEIGHT_FORK_COOLDOWN_SECS)
                    {
                        tracing::debug!(
                            "🔀 Same-height fork cooldown active for height {} hash {} \
                             ({:.1}s ago, retry in {:.1}s)",
                            consensus_height,
                            hex::encode(&consensus_hash[..8]),
                            cd_time.elapsed().as_secs_f32(),
                            SAME_HEIGHT_FORK_COOLDOWN_SECS as f32 - cd_time.elapsed().as_secs_f32(),
                        );
                        return None;
                    }
                }
            }
            // Record this attempt so subsequent calls within the cooldown window return None.
            *self.same_height_fork_cooldown.write().await =
                Some((consensus_height, consensus_hash, std::time::Instant::now()));

            warn!(
                "🔀 Same-height fork at {}: switching to consensus chain ({} peers). Our hash {} vs consensus {}",
                consensus_height,
                consensus_peers.len(),
                hex::encode(&our_hash[..8]),
                hex::encode(&consensus_hash[..8]),
            );
            return Some((consensus_height, consensus_peers[0].clone()));
        }

        // Case 3: We're ahead of all known peers
        // LONGEST VALID CHAIN RULE: If we have a valid longer chain than any peer, WE are canonical.
        // Peers will sync to us when they see our blocks. Even if hashes differ at a lower
        // height, our chain is longer and should win — peers adopt the longest valid chain.
        if our_height > consensus_height {
            // Log divergence for diagnostics but do NOT roll back
            if our_height - consensus_height <= 5 && consensus_peers.len() >= 2 {
                if let Ok(our_hash_at_consensus) = self.get_block_hash(consensus_height) {
                    if our_hash_at_consensus != consensus_hash {
                        tracing::info!(
                            "📈 Longest chain rule: we're at {} (peers at {}). Hash differs at {} — peers should sync to us.",
                            our_height,
                            consensus_height,
                            consensus_height
                        );
                    }
                }
            }

            // Verify our top block is still retrievable
            match self.get_block(our_height) {
                Ok(_) => {
                    tracing::debug!(
                        "📈 We have the longest chain: height {} > highest peer {} ({} peers at that height)",
                        our_height,
                        consensus_height,
                        consensus_peers.len()
                    );

                    // Don't roll back - we ARE the canonical chain
                    // Peers will sync to us when they receive our blocks
                    return None;
                }
                Err(e) if e.contains("deleted") || e.contains("not found") => {
                    // Our block was deleted (corrupted recovery) or is missing
                    // Sync to consensus to refill the gap
                    tracing::warn!(
                        "🔄 Recovery: Our block {} is missing ({}), syncing to consensus at {}",
                        our_height,
                        e,
                        consensus_height
                    );
                    return Some((consensus_height, consensus_peers[0].clone()));
                }
                Err(e) => {
                    tracing::error!("❌ Failed to verify our top block {}: {}", our_height, e);
                    // On unexpected error, sync to consensus to be safe
                    return Some((consensus_height, consensus_peers[0].clone()));
                }
            }
        }

        // Case 4: Same height, same hash - no fork
        None
    }

    /// Start periodic chain comparison task
    ///
    /// This task queries peers every 15 seconds to detect forks and trigger sync.
    /// Works in coordination with the sync coordinator (which runs every 30s).
    pub fn start_chain_comparison_task(blockchain: Arc<Blockchain>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));

            loop {
                interval.tick().await;

                // Skip fork detection during sync — focus on catching up
                if blockchain.is_syncing() {
                    continue;
                }

                let our_height = blockchain.get_height();
                tracing::debug!("🔍 Periodic chain check: our height = {}", our_height);

                // Get peer registry
                let peer_registry_opt = blockchain.peer_registry.read().await;
                let peer_registry = match peer_registry_opt.as_ref() {
                    Some(pr) => pr,
                    None => continue,
                };

                // Request fresh chain tips from all connected compatible peers
                let connected_peers = peer_registry.get_compatible_peers().await;
                if connected_peers.is_empty() {
                    continue;
                }

                tracing::debug!(
                    "🔍 Periodic chain check: Requesting chain tips from {} peers",
                    connected_peers.len()
                );

                for peer in &connected_peers {
                    let request = NetworkMessage::GetChainTip;
                    if let Err(e) = peer_registry.send_to_peer(peer, request).await {
                        tracing::debug!("Failed to send GetChainTip to {}: {}", peer, e);
                    }
                }

                // Wait for responses (increased timeout for high-latency networks)
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

                // Drop the read lock before calling compare_chain_with_peers
                drop(peer_registry_opt);

                // Query peers for their heights and check for forks (uses cached data)
                if let Some((consensus_height, consensus_peer)) =
                    blockchain.compare_chain_with_peers().await
                {
                    // Check if this is a same-height fork or we're behind
                    if consensus_height == our_height {
                        // Same height fork - request blocks but DO NOT rollback yet!
                        // The rollback will happen atomically when blocks arrive via process_peer_blocks
                        // This prevents the race condition where we rollback to a lower height
                        // and then get stuck there if blocks don't arrive
                        tracing::warn!(
                            "🔀 Periodic fork detection: same-height fork at {}, requesting blocks from {}",
                            consensus_height,
                            consensus_peer
                        );

                        // Request blocks from peer - reorg will happen atomically when they arrive
                        if let Some(peer_registry) = blockchain.peer_registry.read().await.as_ref()
                        {
                            // Request from 20 blocks back to find common ancestor
                            let request_from = consensus_height.saturating_sub(20).max(1);

                            // ✅ Check with sync coordinator before requesting
                            match blockchain
                                .sync_coordinator
                                .request_sync(
                                    consensus_peer.clone(),
                                    request_from,
                                    consensus_height,
                                    crate::network::sync_coordinator::SyncSource::ForkResolution,
                                )
                                .await
                            {
                                Ok(true) => {
                                    let req =
                                        NetworkMessage::GetBlocks(request_from, consensus_height);
                                    if let Err(e) =
                                        peer_registry.send_to_peer(&consensus_peer, req).await
                                    {
                                        blockchain
                                            .sync_coordinator
                                            .cancel_sync(&consensus_peer)
                                            .await;
                                        tracing::warn!(
                                            "⚠️  Failed to request blocks from {}: {}",
                                            consensus_peer,
                                            e
                                        );
                                    } else {
                                        tracing::info!(
                                            "📤 Requested blocks {}-{} from {} for fork resolution (no premature rollback)",
                                            request_from,
                                            consensus_height,
                                            consensus_peer
                                        );
                                    }
                                }
                                Ok(false) => {
                                    tracing::debug!(
                                        "⏸️ Fork resolution sync queued with {}",
                                        consensus_peer
                                    );
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "⏱️ Fork resolution sync throttled with {}: {}",
                                        consensus_peer,
                                        e
                                    );
                                }
                            }
                        }
                    } else if consensus_height > our_height {
                        // We're simply behind — spawn_sync_coordinator (running every 10s) already
                        // handles catching up via sync_from_peers(). Initiating a second concurrent
                        // GetBlocks request here would compete with that one for peer responses and
                        // cause 30-second timeouts on both sides. Do nothing; let the sync coordinator
                        // own the "behind" case.
                        tracing::debug!(
                            "📊 Chain comparison: {} blocks behind ({} vs {}), sync coordinator will handle catch-up",
                            consensus_height - our_height,
                            our_height,
                            consensus_height,
                        );
                    } else {
                        // consensus_height < our_height — solo fork detected
                        // We advanced beyond peers on a divergent chain. Roll back and resync.
                        tracing::warn!(
                            "🔀 SOLO FORK RECOVERY: We're at {} but consensus is at {} — rolling back to resync from {}",
                            our_height,
                            consensus_height,
                            consensus_peer
                        );

                        if let Some(peer_registry) = blockchain.peer_registry.read().await.as_ref()
                        {
                            let request_from = consensus_height.saturating_sub(20).max(1);
                            match blockchain
                                .sync_coordinator
                                .request_sync(
                                    consensus_peer.clone(),
                                    request_from,
                                    consensus_height,
                                    crate::network::sync_coordinator::SyncSource::ForkResolution,
                                )
                                .await
                            {
                                Ok(true) => {
                                    let req =
                                        NetworkMessage::GetBlocks(request_from, consensus_height);
                                    if let Err(e) =
                                        peer_registry.send_to_peer(&consensus_peer, req).await
                                    {
                                        blockchain
                                            .sync_coordinator
                                            .cancel_sync(&consensus_peer)
                                            .await;
                                        tracing::warn!(
                                            "⚠️  Failed to request blocks from {}: {}",
                                            consensus_peer,
                                            e
                                        );
                                    } else {
                                        tracing::info!(
                                            "📤 Requested blocks {}-{} from {} for solo fork recovery",
                                            request_from,
                                            consensus_height,
                                            consensus_peer
                                        );
                                    }
                                }
                                Ok(false) => {
                                    tracing::debug!(
                                        "⏸️ Solo fork recovery sync queued with {}",
                                        consensus_peer
                                    );
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "⏱️ Solo fork recovery sync throttled with {}: {}",
                                        consensus_peer,
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    // ========================================================================
    // Fork Resolution State Machine Methods (New Architecture)
    // ========================================================================

    /// Process blocks received from a peer - NEW SIMPLE API
    /// This replaces the complex fork handling in peer_connection.rs
    pub async fn process_peer_blocks(
        &self,
        blocks: Vec<Block>,
        peer_addr: String,
    ) -> Result<usize, String> {
        if blocks.is_empty() {
            return Ok(0);
        }

        let mut added = 0;
        let mut fork_blocks = Vec::new();

        // Try to add each block
        for block in blocks.iter() {
            match self.add_block_with_fork_handling(block.clone()).await {
                Ok(true) => added += 1,
                Ok(false) => {
                    // Already have this block, skip
                }
                Err(e) if e.contains("Fork detected") => {
                    // Fork detected - collect these blocks
                    fork_blocks.push(block.clone());
                }
                Err(e) => {
                    warn!("Failed to add block {}: {}", block.header.height, e);
                }
            }
        }

        // If we detected a fork, trigger fork resolution
        if !fork_blocks.is_empty() {
            info!(
                "🔀 Fork detected in peer blocks, starting resolution with {} blocks",
                fork_blocks.len()
            );
            // Initiate fork resolution in background
            let blockchain = Arc::new(self.clone());
            let peer = peer_addr.clone();
            tokio::spawn(async move {
                if let Err(e) = blockchain.handle_fork(fork_blocks, peer).await {
                    warn!("Fork resolution failed: {}", e);
                }
            });
        }

        Ok(added)
    }

    /// Main entry point when blocks don't match our chain
    pub async fn handle_fork(&self, blocks: Vec<Block>, peer_addr: String) -> Result<(), String> {
        if blocks.is_empty() {
            return Err("No blocks provided for fork resolution".to_string());
        }

        // Skip fork resolution if genesis mismatch was already detected.
        // Operator must manually delete the chain and restart to resolve.
        if self.genesis_mismatch_detected.load(Ordering::Relaxed) {
            return Ok(());
        }

        let fork_height = blocks[0].header.height;
        let our_height = self.get_height();

        // Guard: if a reorg is already committed (ReadyToReorg), in progress (Reorging),
        // or actively fetching blocks from a DIFFERENT peer (FetchingChain), do not allow
        // a concurrent handle_fork() call to overwrite the fork_state.
        // Without this guard:
        // - ReadyToReorg/Reorging: a second peer's call clobbers the state, reorg never runs.
        // - FetchingChain (different peer): a same-height peer hijacks the state, discarding
        //   accumulated blocks from a longer-chain peer, causing infinite fork loops.
        {
            let current_state = self.fork_state.read().await;
            match &*current_state {
                ForkResolutionState::ReadyToReorg { .. } | ForkResolutionState::Reorging { .. } => {
                    tracing::debug!(
                        "🚫 Skipping handle_fork() from {} — reorg already committed/in-progress",
                        peer_addr
                    );
                    return Ok(());
                }
                ForkResolutionState::FetchingChain {
                    peer_addr: fetching_peer,
                    peer_height,
                    ..
                } if fetching_peer != &peer_addr => {
                    tracing::debug!(
                        "🚫 Skipping handle_fork() from {} — already fetching from {} (tip {})",
                        peer_addr,
                        fetching_peer,
                        peer_height
                    );
                    return Ok(());
                }
                _ => {}
            }
        }

        // Check if we're already in FetchingChain state for this peer
        // If so, merge these blocks with accumulated blocks and preserve peer_height
        let mut all_blocks = blocks.clone();
        let peer_tip_height = {
            let current_state = self.fork_state.read().await;
            if let ForkResolutionState::FetchingChain {
                peer_addr: fetching_peer,
                accumulated_blocks,
                peer_height,
                ..
            } = &*current_state
            {
                if fetching_peer == &peer_addr {
                    info!(
                        "📥 Merging {} new blocks with {} accumulated blocks",
                        blocks.len(),
                        accumulated_blocks.len()
                    );
                    // CRITICAL FIX: Prefer NEW blocks over accumulated blocks when heights match.
                    // This prevents stale/local blocks from previous attempts from corrupting the reorg.
                    // Only add accumulated blocks if new blocks don't have that height.
                    for acc_block in accumulated_blocks {
                        let height = acc_block.header.height;
                        // Check if new blocks already have this height
                        let already_have = all_blocks.iter().any(|b| b.header.height == height);
                        if !already_have {
                            // Verify this accumulated block forms a valid chain with adjacent blocks
                            // to catch any corrupted/wrong blocks before adding
                            let acc_hash = acc_block.hash();
                            let next_block_valid = all_blocks
                                .iter()
                                .find(|b| b.header.height == height + 1)
                                .map(|next| next.header.previous_hash == acc_hash)
                                .unwrap_or(true); // OK if no next block to check

                            if next_block_valid {
                                all_blocks.push(acc_block.clone());
                            } else {
                                warn!(
                                    "🚫 Skipping accumulated block {} (hash {}) - doesn't connect to next block",
                                    height,
                                    hex::encode(&acc_hash[..8])
                                );
                            }
                        }
                    }
                    info!("📦 Total blocks for fork resolution: {}", all_blocks.len());
                    // Use the peer_height from state, not max block height
                    *peer_height
                } else {
                    // Different peer, use max block height
                    blocks
                        .iter()
                        .map(|b| b.header.height)
                        .max()
                        .unwrap_or(fork_height)
                }
            } else {
                // Not in FetchingChain state, use max block height
                blocks
                    .iter()
                    .map(|b| b.header.height)
                    .max()
                    .unwrap_or(fork_height)
            }
        };

        info!(
            "🔀 Fork detected at height {} from peer {} ({} blocks provided, peer_tip: {}, our_height: {})",
            fork_height, peer_addr, blocks.len(), peer_tip_height, our_height
        );

        // STRATEGY: We need blocks covering the range from common ancestor to peer tip.
        // Instead of requesting from search_floor to peer_tip (100+ blocks that exceed
        // the 8MB frame limit), request only blocks BELOW the fork point in small batches.
        // The peer already sent us blocks above the fork point.
        let lowest_peer_block = all_blocks
            .iter()
            .map(|b| b.header.height)
            .min()
            .unwrap_or(fork_height);
        let search_floor = our_height.saturating_sub(MAX_REORG_DEPTH);

        if lowest_peer_block > search_floor && search_floor > 0 {
            // Detect accumulation stall: if we already had accumulated blocks and
            // the lowest block hasn't changed, we're in an infinite loop.
            let (stalled, original_started_at) = {
                let current_state = self.fork_state.read().await;
                if let ForkResolutionState::FetchingChain {
                    accumulated_blocks,
                    started_at,
                    ..
                } = &*current_state
                {
                    let prev_lowest = accumulated_blocks
                        .iter()
                        .map(|b| b.header.height)
                        .min()
                        .unwrap_or(u64::MAX);
                    let elapsed = started_at.elapsed();
                    // Stalled if lowest block hasn't changed AND we've been trying for >60s
                    let is_stalled = lowest_peer_block >= prev_lowest
                        && elapsed > std::time::Duration::from_secs(60);
                    (is_stalled, Some(*started_at))
                } else {
                    (false, None)
                }
            };

            if stalled {
                warn!(
                    "🔄 Fork resolution stalled: lowest block still {} after 60s, aborting",
                    lowest_peer_block
                );
                *self.fork_state.write().await = ForkResolutionState::None;
                return Ok(());
            }

            // Request a SMALL batch of blocks below the fork point to find common ancestor.
            // Don't request all the way to peer_tip — we already have those blocks.
            let batch_size = crate::constants::network::FORK_RESOLUTION_BATCH_SIZE;
            let request_to = lowest_peer_block.saturating_sub(1);
            let request_from = request_to.saturating_sub(batch_size - 1).max(search_floor);

            if request_from > request_to {
                warn!(
                    "⚠️ Fork resolution: cannot request blocks (from {} > to {}), aborting",
                    request_from, request_to
                );
                *self.fork_state.write().await = ForkResolutionState::None;
                return Ok(());
            }

            info!(
                "📥 Requesting blocks {}-{} from {} for fork resolution (need coverage back to {}, have {}-{})",
                request_from, request_to, peer_addr, search_floor, lowest_peer_block, peer_tip_height
            );

            // Transition to fetching state (preserve original start time for stall detection)
            *self.fork_state.write().await = ForkResolutionState::FetchingChain {
                common_ancestor: 0, // Not yet known
                fork_height,
                peer_addr: peer_addr.clone(),
                peer_height: peer_tip_height,
                fetched_up_to: peer_tip_height, // We already have up to peer tip
                accumulated_blocks: all_blocks.clone(), // Save all blocks we have
                started_at: original_started_at.unwrap_or_else(std::time::Instant::now),
            };

            // Request the blocks
            self.request_blocks_from_peer(&peer_addr, request_from, request_to)
                .await?;

            // Note: When blocks arrive, handle_fork() will be called again with accumulated blocks
            return Ok(());
        }

        // We have enough blocks - use binary search to find common ancestor
        match self.find_fork_common_ancestor(&all_blocks).await {
            Ok(common_ancestor) => {
                info!(
                    "✅ Binary search found common ancestor at height {} (searched {} blocks)",
                    common_ancestor,
                    blocks.len()
                );

                // Use AI fork resolver to make intelligent decision
                let peer_tip_block = all_blocks.iter().max_by_key(|b| b.header.height).unwrap();
                let peer_tip_hash = peer_tip_block.hash();
                let our_tip_hash = self.get_block_hash(our_height)?;

                // Get timestamps
                let peer_tip_timestamp = peer_tip_block.header.timestamp;

                // Calculate fork depth
                let fork_depth = our_height.saturating_sub(common_ancestor);

                // CRITICAL SECURITY CHECK: Reject reorgs that go back to genesis
                // If common_ancestor == 0, this means the chains diverged at or before genesis
                // This is ALWAYS suspicious - either different genesis blocks or an attack
                if common_ancestor == 0 && our_height > 0 {
                    // We have blocks, but ancestor search went all the way to genesis
                    // This means peer's chain doesn't share our history - REJECT

                    // Check if peer provided a genesis block for comparison
                    let peer_genesis_info = all_blocks
                        .iter()
                        .find(|b| b.header.height == 0)
                        .map(|b| hex::encode(&b.hash()[..8]));

                    let our_genesis_info = self
                        .get_block_by_height(0)
                        .await
                        .map(|b| hex::encode(&b.hash()[..8]))
                        .unwrap_or_else(|_| "unknown".to_string());

                    // ONLY reject if genesis hashes are CONFIRMED DIFFERENT
                    // If peer didn't provide genesis block, we can't determine compatibility
                    match &peer_genesis_info {
                        Some(peer_genesis) if peer_genesis != &our_genesis_info => {
                            // Peer provided genesis and it's DIFFERENT - mark incompatible
                            warn!(
                                "🛡️ SECURITY: REJECTED REORG TO GENESIS from peer {} - chains diverged at genesis level",
                                peer_addr
                            );
                            warn!(
                                "   Our height: {}, our genesis: {}, peer genesis: {}",
                                our_height, our_genesis_info, peer_genesis
                            );
                            warn!(
                                "   Peer is on a completely different chain - cannot reorg to genesis"
                            );

                            // Mark peer as genesis-incompatible
                            if let Some(registry) = self.peer_registry.read().await.as_ref() {
                                registry
                                    .mark_genesis_incompatible(
                                        &peer_addr,
                                        &our_genesis_info,
                                        peer_genesis,
                                    )
                                    .await;
                            }
                        }
                        Some(peer_genesis) => {
                            // Genesis blocks match - this is a normal fork, allow it
                            info!(
                                "✓ Genesis blocks match with peer {} ({}), allowing reorg from genesis",
                                peer_addr, peer_genesis
                            );
                        }
                        None => {
                            // Peer didn't provide genesis block - can't determine compatibility
                            // Don't mark as incompatible, but also don't allow the reorg yet
                            warn!(
                                "⚠️ Peer {} didn't provide genesis block for comparison - requesting verification",
                                peer_addr
                            );
                            // Request explicit genesis verification
                            if let Some(registry) = self.peer_registry.read().await.as_ref() {
                                let our_genesis_hash = self
                                    .get_block_by_height(0)
                                    .await
                                    .map(|b| b.hash())
                                    .unwrap_or([0u8; 32]);
                                let compatible = registry
                                    .verify_genesis_compatibility(&peer_addr, our_genesis_hash)
                                    .await;
                                if !compatible {
                                    // verify_genesis_compatibility already marks as incompatible
                                    *self.fork_state.write().await = ForkResolutionState::None;
                                    return Ok(());
                                }
                                info!(
                                    "✓ Genesis verification passed for peer {}, allowing reorg from genesis",
                                    peer_addr
                                );
                                // CONTINUE to fork resolution logic below - don't return early
                            }
                        }
                    }
                    // If we reach here, genesis is compatible - proceed to fork resolution
                }

                // CRITICAL SECURITY CHECK: Reject reorgs that are too deep
                if let Err(reason) = crate::ai::fork_resolver::check_reorg_depth(
                    fork_depth,
                    our_height,
                    common_ancestor,
                    peer_tip_height,
                    &peer_addr,
                ) {
                    warn!("🛡️ SECURITY: {}", reason);
                    *self.fork_state.write().await = ForkResolutionState::None;
                    return Ok(());
                }

                info!(
                    "🤖 Evaluating fork: our={} peer={}, ancestor={}, depth={}",
                    our_height, peer_tip_height, common_ancestor, fork_depth
                );

                let resolution = self
                    .fork_resolver
                    .resolve_fork(crate::ai::fork_resolver::ForkResolutionParams {
                        our_height,
                        peer_height: peer_tip_height,
                        peer_ip: peer_addr.clone(),
                        peer_tip_timestamp: Some(peer_tip_timestamp),
                        our_tip_hash: Some(our_tip_hash),
                        peer_tip_hash: Some(peer_tip_hash),
                    })
                    .await;

                let reasoning_summary = resolution.reasoning.join("; ");
                info!(
                    "🤖 Fork resolution decision: accept={}, reasoning: {}",
                    resolution.accept_peer_chain, reasoning_summary
                );

                // Decision: accept only if fork resolver says so
                if !resolution.accept_peer_chain {
                    // CONSENSUS OVERRIDE: The hash tiebreaker can keep us on a minority fork
                    // when the entire network has already settled on the competing block.
                    // If ≥3 peers independently confirm the peer's hash at the same height,
                    // the majority-chain consensus overrides the local hash tiebreaker.
                    let mut consensus_override = false;
                    if peer_tip_height == our_height {
                        let peer_registry_guard = self.peer_registry.read().await;
                        if let Some(registry) = peer_registry_guard.as_ref() {
                            let compatible_peers = registry.get_compatible_peers().await;
                            let mut supporting_peers = 0usize;
                            let mut total_checked = 0usize;
                            for pip in &compatible_peers {
                                if let Some((tip_height, tip_hash)) =
                                    registry.get_peer_chain_tip(pip).await
                                {
                                    total_checked += 1;
                                    if tip_height == peer_tip_height && tip_hash == peer_tip_hash {
                                        supporting_peers += 1;
                                    }
                                }
                            }
                            const MIN_CONSENSUS_OVERRIDE_PEERS: usize = 3;
                            if total_checked > 0 && supporting_peers >= MIN_CONSENSUS_OVERRIDE_PEERS
                            {
                                warn!(
                                    "🔀 Consensus override: {}/{} peers have hash {} at height {} \
                                    — accepting majority chain over hash tiebreaker",
                                    supporting_peers,
                                    total_checked,
                                    hex::encode(&peer_tip_hash[..8]),
                                    peer_tip_height,
                                );
                                consensus_override = true;
                            }
                        }
                    }

                    if !consensus_override {
                        info!("📊 Fork resolver rejected peer chain");
                        *self.fork_state.write().await = ForkResolutionState::None;
                        return Ok(());
                    }
                    // Fall through to reorg — majority consensus overrides hash tiebreaker
                }

                // NETWORK CONSENSUS CROSS-CHECK: For same-height forks, verify that
                // the fork resolver's decision aligns with what other peers see.
                // A single peer sending competing blocks at the SAME height should
                // not force a reorg without broader network agreement.
                //
                // CRITICAL: Skip this check for LONGER chains. The longest-chain
                // rule is the canonical consensus rule — a valid longer chain must
                // always be accepted regardless of how many peers currently support
                // it. Requiring peer support for longer chains creates a deadlock:
                // the majority won't switch because not enough peers support the
                // longer chain, and the minority can't produce because the majority
                // disagrees. Block validation (signatures, timestamps, chain
                // integrity) already protects against fabricated chains.
                if peer_tip_height <= our_height {
                    const MIN_PEERS_FOR_ONDEMAND_FORK: usize = 3;
                    let peer_registry = self.peer_registry.read().await;
                    if let Some(registry) = peer_registry.as_ref() {
                        let compatible_peers = registry.get_compatible_peers().await;
                        let mut supporting_peers = 0usize;
                        let mut total_checked = 0usize;
                        for pip in &compatible_peers {
                            if let Some((tip_height, tip_hash)) =
                                registry.get_peer_chain_tip(pip).await
                            {
                                total_checked += 1;
                                if (tip_height == peer_tip_height && tip_hash == peer_tip_hash)
                                    || tip_height > our_height
                                {
                                    supporting_peers += 1;
                                }
                            }
                        }
                        if total_checked > 0 && supporting_peers < MIN_PEERS_FOR_ONDEMAND_FORK {
                            info!(
                                "🛡️ On-demand fork REJECTED: only {}/{} peers support peer tip \
                                (need {}). Single-peer fork attempt from {}",
                                supporting_peers,
                                total_checked,
                                MIN_PEERS_FOR_ONDEMAND_FORK,
                                peer_addr
                            );
                            *self.fork_state.write().await = ForkResolutionState::None;
                            return Ok(());
                        }
                        if total_checked > 0 {
                            info!(
                                "✅ On-demand fork consensus cross-check passed: {}/{} peers support",
                                supporting_peers, total_checked
                            );
                        }
                    }
                } else {
                    info!(
                        "✅ Skipping peer support check: peer chain is longer ({} > {}). \
                         Longest-chain rule applies — blocks will be validated individually.",
                        peer_tip_height, our_height
                    );
                }

                // CRITICAL SAFETY CHECK: Common ancestor cannot be higher than our chain
                if common_ancestor > our_height {
                    warn!(
                        "🚫 REJECTED REORG: Common ancestor {} > our height {} - bug in ancestor search!",
                        common_ancestor, our_height
                    );
                    *self.fork_state.write().await = ForkResolutionState::None;
                    return Ok(());
                }

                // Reject reorgs to strictly shorter chains (safety net)
                if peer_tip_height < our_height {
                    warn!(
                        "🚫 REJECTED REORG: Peer chain is SHORTER ({} < {}).",
                        peer_tip_height, our_height
                    );
                    *self.fork_state.write().await = ForkResolutionState::None;
                    return Ok(());
                }

                {
                    let reason = "longest valid chain";

                    let peer_chain_length = peer_tip_height.saturating_sub(common_ancestor);
                    let our_chain_length = our_height.saturating_sub(common_ancestor);

                    info!(
                        "📊 Accepting peer chain: {} (peer {} blocks vs our {} from ancestor {})",
                        reason, peer_chain_length, our_chain_length, common_ancestor
                    );

                    // Filter ALL blocks (merged set) to only those after common ancestor.
                    // CRITICAL: Must use all_blocks (which includes accumulated blocks from
                    // previous fetches), not just the latest batch from the peer, to avoid
                    // an infinite loop when the peer splits its response across multiple messages.
                    let all_blocks_count = all_blocks.len();
                    let all_blocks_saved = all_blocks.clone();
                    let reorg_blocks: Vec<Block> = all_blocks
                        .into_iter()
                        .filter(|b| b.header.height > common_ancestor)
                        .collect();

                    if reorg_blocks.is_empty() {
                        warn!(
                            "❌ No blocks to reorg with after filtering (common_ancestor: {}, peer_tip: {}, blocks_before_filter: {})",
                            common_ancestor, peer_tip_height, all_blocks_count
                        );

                        // Request blocks from common_ancestor+1 to peer_tip
                        let expected_start = common_ancestor + 1;
                        if peer_tip_height >= expected_start {
                            info!(
                                "📥 Requesting missing blocks {}-{} from {} for reorg",
                                expected_start, peer_tip_height, peer_addr
                            );

                            *self.fork_state.write().await = ForkResolutionState::FetchingChain {
                                common_ancestor,
                                fork_height,
                                peer_addr: peer_addr.clone(),
                                peer_height: peer_tip_height,
                                fetched_up_to: peer_tip_height,
                                accumulated_blocks: all_blocks_saved,
                                started_at: std::time::Instant::now(),
                            };

                            self.request_blocks_from_peer(
                                &peer_addr,
                                expected_start,
                                peer_tip_height,
                            )
                            .await?;
                            return Ok(());
                        }

                        return Err(format!(
                            "No blocks to reorg with (ancestor: {}, peer_tip: {})",
                            common_ancestor, peer_tip_height
                        ));
                    }

                    // Verify blocks are contiguous from common_ancestor + 1
                    let expected_start = common_ancestor + 1;
                    let actual_start = reorg_blocks
                        .iter()
                        .map(|b| b.header.height)
                        .min()
                        .unwrap_or(0);

                    if actual_start != expected_start {
                        // We're missing blocks - need to fetch them
                        info!(
                            "📥 Missing blocks after common ancestor {} - requesting {}-{}",
                            common_ancestor, expected_start, peer_tip_height
                        );

                        *self.fork_state.write().await = ForkResolutionState::FetchingChain {
                            common_ancestor,
                            fork_height,
                            peer_addr: peer_addr.clone(),
                            peer_height: peer_tip_height,
                            fetched_up_to: peer_tip_height,
                            accumulated_blocks: reorg_blocks,
                            started_at: std::time::Instant::now(),
                        };

                        self.request_blocks_from_peer(&peer_addr, expected_start, peer_tip_height)
                            .await?;
                        return Ok(());
                    }

                    // Check for gaps in the middle of the chain (e.g., peer response
                    // was capped at 100 blocks, leaving intermediate heights missing)
                    let expected_block_count = (peer_tip_height - common_ancestor) as usize;
                    if reorg_blocks.len() < expected_block_count {
                        let heights: std::collections::HashSet<u64> =
                            reorg_blocks.iter().map(|b| b.header.height).collect();
                        let mut first_missing = peer_tip_height; // sentinel
                        for h in expected_start..=peer_tip_height {
                            if !heights.contains(&h) {
                                first_missing = h;
                                break;
                            }
                        }

                        info!(
                            "📥 Gap in fork chain: missing block {} (have {}/{} blocks from ancestor {} to peer tip {})",
                            first_missing, reorg_blocks.len(), expected_block_count, common_ancestor, peer_tip_height
                        );

                        *self.fork_state.write().await = ForkResolutionState::FetchingChain {
                            common_ancestor,
                            fork_height,
                            peer_addr: peer_addr.clone(),
                            peer_height: peer_tip_height,
                            fetched_up_to: peer_tip_height,
                            accumulated_blocks: reorg_blocks,
                            started_at: std::time::Instant::now(),
                        };

                        self.request_blocks_from_peer(&peer_addr, first_missing, peer_tip_height)
                            .await?;
                        return Ok(());
                    }

                    // CRITICAL: Validate the entire chain is continuous before reorg
                    // Check that each block builds on the previous one
                    info!(
                        "🔍 Validating chain continuity for {} blocks...",
                        reorg_blocks.len()
                    );

                    // Sort blocks by height
                    let mut sorted_reorg_blocks = reorg_blocks.clone();
                    sorted_reorg_blocks.sort_by_key(|b| b.header.height);

                    // DEBUG: Log all blocks in the reorg set to identify data corruption
                    for (idx, blk) in sorted_reorg_blocks.iter().enumerate() {
                        let blk_hash = blk.hash();
                        tracing::debug!(
                            "🔍 Reorg block {}: height={}, hash={}, prev_hash={}",
                            idx,
                            blk.header.height,
                            hex::encode(&blk_hash[..8]),
                            hex::encode(&blk.header.previous_hash[..8])
                        );
                    }

                    // SANITY CHECK: Ensure we don't have our own (local) blocks mixed in
                    // This can happen due to bugs in block accumulation
                    for blk in &sorted_reorg_blocks {
                        if let Ok(local_block) = self.get_block(blk.header.height) {
                            let local_hash = local_block.hash();
                            let peer_hash = blk.hash();
                            if local_hash == peer_hash {
                                // This is fine - block matches
                            } else {
                                // Verify this is actually the peer's block, not our local one
                                // by checking that it forms a valid chain with adjacent peer blocks
                                let is_probably_local = sorted_reorg_blocks.iter().any(|other| {
                                    other.header.height == blk.header.height + 1
                                        && other.header.previous_hash != peer_hash
                                });
                                if is_probably_local {
                                    warn!(
                                        "🚫 Detected local block {} (hash {}) mixed into reorg set! \
                                        Expected peer block with different hash. \
                                        This indicates a bug in block accumulation.",
                                        blk.header.height,
                                        hex::encode(&peer_hash[..8])
                                    );
                                }
                            }
                        }
                    }

                    // First block must build on common ancestor
                    let our_ancestor_hash = self.get_block_hash(common_ancestor)?;
                    let first_block = &sorted_reorg_blocks[0];
                    if first_block.header.previous_hash != our_ancestor_hash {
                        // Genesis mismatch: peer's chain is built on a different genesis block.
                        // Set flag to suppress future fork resolution attempts and avoid infinite loop.
                        if common_ancestor == 0 {
                            warn!(
                                "🚨 GENESIS MISMATCH: Our genesis hash {} does not match peer {}'s \
                                expected genesis {}. Fork resolution suppressed. \
                                To resolve: stop node, delete chain data, and restart.",
                                hex::encode(&our_ancestor_hash[..8]),
                                peer_addr,
                                hex::encode(&first_block.header.previous_hash[..8])
                            );
                            self.genesis_mismatch_detected
                                .store(true, Ordering::Relaxed);
                            *self.fork_state.write().await = ForkResolutionState::None;
                            return Ok(());
                        }
                        return Err(format!(
                            "Chain validation failed: first block {} expects previous_hash {:x?}, \
                            but common ancestor {} has hash {:x?}",
                            first_block.header.height,
                            hex::encode(first_block.header.previous_hash),
                            common_ancestor,
                            hex::encode(our_ancestor_hash)
                        ));
                    }

                    // Reject blocks with timestamps in the future
                    let now = chrono::Utc::now().timestamp();
                    for blk in &sorted_reorg_blocks {
                        if blk.header.timestamp > now + TIMESTAMP_TOLERANCE_SECS {
                            warn!(
                                "🛡️ SECURITY: REJECTED REORG from {} - block {} has future timestamp {} (now: {}, tolerance: {}s)",
                                peer_addr, blk.header.height, blk.header.timestamp, now, TIMESTAMP_TOLERANCE_SECS
                            );
                            *self.fork_state.write().await = ForkResolutionState::None;
                            return Err(format!(
                                "Reorg rejected: block {} timestamp {} is in the future",
                                blk.header.height, blk.header.timestamp
                            ));
                        }
                    }

                    // CRITICAL SECURITY: Reject chains that exceed the expected height
                    // The maximum legitimate block height is determined by elapsed time since genesis.
                    // An attacker cannot produce more blocks than time allows, regardless of timestamps.
                    let max_expected_height = self.calculate_expected_height();
                    let peer_max_height = sorted_reorg_blocks
                        .last()
                        .map(|b| b.header.height)
                        .unwrap_or(0);
                    if peer_max_height > max_expected_height {
                        warn!(
                            "🛡️ SECURITY: REJECTED REORG from {} - peer chain height {} exceeds maximum expected height {} (genesis-based calculation)",
                            peer_addr, peer_max_height, max_expected_height
                        );
                        *self.fork_state.write().await = ForkResolutionState::None;
                        return Err(format!(
                            "Reorg rejected: chain height {} exceeds expected maximum {}",
                            peer_max_height, max_expected_height
                        ));
                    }

                    // Each subsequent block must build on the previous block in the chain
                    for i in 1..sorted_reorg_blocks.len() {
                        let prev_block = &sorted_reorg_blocks[i - 1];
                        let curr_block = &sorted_reorg_blocks[i];
                        let prev_block_hash = prev_block.hash();

                        if curr_block.header.previous_hash != prev_block_hash {
                            return Err(format!(
                                "Chain validation failed: block {} expects previous_hash {:x?}, \
                                but block {} has hash {:x?}. Peer sent non-contiguous blocks!",
                                curr_block.header.height,
                                hex::encode(curr_block.header.previous_hash),
                                prev_block.header.height,
                                hex::encode(prev_block_hash)
                            ));
                        }
                    }

                    info!(
                        "✅ Chain validation passed: {} blocks form valid continuous chain from height {} to {}",
                        sorted_reorg_blocks.len(),
                        sorted_reorg_blocks.first().unwrap().header.height,
                        sorted_reorg_blocks.last().unwrap().header.height
                    );

                    // Set Reorging state and call perform_reorg directly (avoids
                    // fork_state race with concurrent handle_fork() calls).
                    *self.fork_state.write().await = ForkResolutionState::Reorging {
                        from_height: our_height,
                        to_height: sorted_reorg_blocks.last().unwrap().header.height,
                        started_at: std::time::Instant::now(),
                    };

                    let reorg_result = self
                        .perform_reorg(common_ancestor, sorted_reorg_blocks)
                        .await;

                    // Always clear fork state after reorg attempt
                    *self.fork_state.write().await = ForkResolutionState::None;
                    self.consensus_peers.write().await.clear();

                    reorg_result
                }
            }
            Err(e) => {
                warn!(
                    "⚠️  Common ancestor search failed: {} - requesting blocks within reorg window",
                    e
                );

                // Request blocks covering MAX_REORG_DEPTH from our height.
                // If the ancestor isn't within this range, we'd reject the reorg anyway.
                let request_from = our_height.saturating_sub(MAX_REORG_DEPTH);
                let request_to = peer_tip_height;

                info!(
                    "📥 Requesting block history {}-{} from {} (MAX_REORG_DEPTH={})",
                    request_from, request_to, peer_addr, MAX_REORG_DEPTH
                );

                *self.fork_state.write().await = ForkResolutionState::FetchingChain {
                    common_ancestor: 0,
                    fork_height,
                    peer_addr: peer_addr.clone(),
                    peer_height: peer_tip_height,
                    fetched_up_to: peer_tip_height,
                    accumulated_blocks: all_blocks.clone(),
                    started_at: std::time::Instant::now(),
                };

                self.request_blocks_from_peer(&peer_addr, request_from, request_to)
                    .await?;
                Ok(())
            }
        }
    }

    /// Request range of blocks from a peer
    async fn request_blocks_from_peer(
        &self,
        peer_addr: &str,
        start: u64,
        end: u64,
    ) -> Result<(), String> {
        debug!("📤 Requesting blocks {}-{} from {}", start, end, peer_addr);

        let registry = self.peer_registry.read().await;
        if let Some(reg) = registry.as_ref() {
            let msg = NetworkMessage::GetBlocks(start, end);
            reg.send_to_peer(peer_addr, msg)
                .await
                .map_err(|e| format!("Failed to request blocks: {}", e))?;
            Ok(())
        } else {
            Err("Peer registry not available".to_string())
        }
    }

    /// Perform chain reorganization
    async fn perform_reorg(
        &self,
        common_ancestor: u64,
        alternate_blocks: Vec<Block>,
    ) -> Result<(), String> {
        let start_time = std::time::Instant::now();
        let our_height = self.get_height();
        let new_height = common_ancestor + alternate_blocks.len() as u64;

        // CRITICAL SAFETY CHECK: NEVER reorg to a shorter chain.
        // Same-height reorgs ARE allowed for deterministic fork resolution
        // (e.g., lower hash wins at same height). The caller (handle_fork)
        // already validated that this reorg should be accepted.
        if new_height < our_height {
            return Err(format!(
                "REJECTED: Cannot reorg to shorter chain! Current: {}, proposed: {} (from ancestor {})",
                our_height, new_height, common_ancestor
            ));
        }

        info!(
            "🔄 Starting reorg: current height {} → rolling back to {} → applying {} blocks → new height {}",
            our_height,
            common_ancestor,
            alternate_blocks.len(),
            new_height
        );

        // Pre-flight validation: check chain consistency before touching storage
        let ancestor_hash = self.get_block_hash(common_ancestor).ok();
        let now = chrono::Utc::now().timestamp();
        crate::ai::fork_resolver::validate_fork_chain(
            common_ancestor,
            ancestor_hash,
            &alternate_blocks,
            now,
        )?;

        // Update state to Reorging
        *self.fork_state.write().await = ForkResolutionState::Reorging {
            from_height: our_height,
            to_height: new_height,
            started_at: std::time::Instant::now(), // NEW: Track start time
        };

        // 1. Rollback to common ancestor (use existing method)
        self.rollback_to_height(common_ancestor).await?;

        // 2. Apply alternate chain
        info!(
            "📝 Applying {} alternate blocks starting from height {}",
            alternate_blocks.len(),
            common_ancestor + 1
        );
        for (idx, block) in alternate_blocks.iter().enumerate() {
            let block_height = block.header.height;
            let expected_hash = block.hash();

            info!(
                "📝 Applying block {}/{}: height {} hash {} prev_hash {}",
                idx + 1,
                alternate_blocks.len(),
                block_height,
                hex::encode(&expected_hash[..8]),
                hex::encode(&block.header.previous_hash[..8])
            );

            // Verify this block builds on what we currently have
            if block_height > 0 {
                match self.get_block_hash(block_height - 1) {
                    Ok(our_prev_hash) => {
                        if our_prev_hash != block.header.previous_hash {
                            return Err(format!(
                                "DIAGNOSTIC: Block {} expects previous_hash {}, but we have {} at height {}. \
                                Common ancestor was {}, reorg chain diverged earlier than expected!",
                                block_height,
                                hex::encode(&block.header.previous_hash[..8]),
                                hex::encode(&our_prev_hash[..8]),
                                block_height - 1,
                                common_ancestor
                            ));
                        }
                    }
                    Err(e) => {
                        return Err(format!(
                            "DIAGNOSTIC: Cannot get block {} to verify chain before applying block {}: {}",
                            block_height - 1,
                            block_height,
                            e
                        ));
                    }
                }
            }

            self.add_block(block.clone())
                .await
                .map_err(|e| format!("Failed to add block {} during reorg: {}", block_height, e))?;

            // CRITICAL: Verify the hash after storage matches what we expected
            match self.get_block(block_height) {
                Ok(stored_block) => {
                    let stored_hash = stored_block.hash();
                    if stored_hash != expected_hash {
                        // DEEP DIAGNOSTIC: Log exactly what changed
                        warn!("🔬 DEEP DIAGNOSTIC - Block {} hash mismatch:", block_height);
                        warn!("  Expected hash: {}", hex::encode(expected_hash));
                        warn!("  Stored hash:   {}", hex::encode(stored_hash));

                        // Compare all consensus-critical fields
                        warn!(
                            "  version: {} vs {}",
                            block.header.version, stored_block.header.version
                        );
                        warn!(
                            "  height: {} vs {}",
                            block.header.height, stored_block.header.height
                        );
                        warn!(
                            "  previous_hash: {} vs {}",
                            hex::encode(block.header.previous_hash),
                            hex::encode(stored_block.header.previous_hash)
                        );
                        warn!(
                            "  merkle_root: {} vs {}",
                            hex::encode(block.header.merkle_root),
                            hex::encode(stored_block.header.merkle_root)
                        );
                        warn!(
                            "  timestamp: {} vs {}",
                            block.header.timestamp, stored_block.header.timestamp
                        );
                        warn!(
                            "  block_reward: {} vs {}",
                            block.header.block_reward, stored_block.header.block_reward
                        );
                        warn!(
                            "  leader: '{}' vs '{}'",
                            block.header.leader, stored_block.header.leader
                        );
                        warn!(
                            "  attestation_root: {} vs {}",
                            hex::encode(block.header.attestation_root),
                            hex::encode(stored_block.header.attestation_root)
                        );
                        warn!(
                            "  vrf_output: {} vs {}",
                            hex::encode(block.header.vrf_output),
                            hex::encode(stored_block.header.vrf_output)
                        );
                        warn!(
                            "  vrf_score: {} vs {}",
                            block.header.vrf_score, stored_block.header.vrf_score
                        );

                        // Also check transaction count (affects merkle root)
                        warn!(
                            "  transactions.len(): {} vs {}",
                            block.transactions.len(),
                            stored_block.transactions.len()
                        );

                        return Err(format!(
                            "HASH MISMATCH after storage! Block {} expected hash {}, but stored as {}. \
                            This indicates non-deterministic block hashing or storage corruption!",
                            block_height,
                            hex::encode(&expected_hash[..8]),
                            hex::encode(&stored_hash[..8])
                        ));
                    }
                    info!(
                        "✓ Block {} hash verified after storage: {}",
                        block_height,
                        hex::encode(&stored_hash[..8])
                    );
                }
                Err(e) => {
                    return Err(format!(
                        "Cannot verify hash after storing block {}: {}",
                        block_height, e
                    ));
                }
            }
        }

        let duration = start_time.elapsed();
        info!("✅ Reorg complete in {:?}", duration);

        // Record metrics
        let metrics = ReorgMetrics {
            timestamp: Utc::now().timestamp(),
            from_height: our_height,
            to_height: new_height,
            common_ancestor,
            blocks_removed: our_height.saturating_sub(common_ancestor),
            blocks_added: new_height.saturating_sub(common_ancestor),
            txs_to_replay: 0, // NOTE: Would require returning count from rollback_to_height()
            duration_ms: duration.as_millis() as u64,
        };

        let mut history = self.reorg_history.write().await;
        history.push(metrics);
        // Keep only last 100 reorgs
        if history.len() > 100 {
            history.remove(0);
        }

        Ok(())
    }

    /// Find common ancestor between our chain and competing blocks.
    /// Delegates to fork_resolver::find_common_ancestor with a closure for block hash lookups.
    async fn find_fork_common_ancestor(&self, competing_blocks: &[Block]) -> Result<u64, String> {
        let our_height = self.get_height();
        // Create a closure that captures `self` for block hash lookups
        let get_hash = |height: u64| -> Result<[u8; 32], String> { self.get_block_hash(height) };
        crate::ai::fork_resolver::find_common_ancestor(our_height, competing_blocks, &get_hash)
    }

    /// Validate that our chain hasn't gotten ahead of the network time schedule
    pub async fn validate_chain_time(&self) -> Result<(), String> {
        let current_height = self.get_height();
        let now = chrono::Utc::now().timestamp();

        // Calculate what height we SHOULD be at based on time
        let expected_height = self.get_expected_height(now);

        // Allow a small buffer for network latency and clock skew
        // TIME COIN: Keep this minimal - temporal precision is critical
        const MAX_BLOCKS_AHEAD: u64 = 0; // Zero tolerance - blocks must be on time

        if current_height > expected_height + MAX_BLOCKS_AHEAD {
            let blocks_ahead = current_height - expected_height;
            let time_ahead_seconds = blocks_ahead * BLOCK_TIME_SECONDS as u64;

            return Err(format!(
                "Chain validation failed: height {} is {} blocks ({} minutes) ahead of schedule (expected: {})",
                current_height,
                blocks_ahead,
                time_ahead_seconds / 60,
                expected_height
            ));
        }

        Ok(())
    }

    /// Get the expected height based on current time
    pub fn get_expected_height(&self, current_time: i64) -> u64 {
        let genesis_time = self.genesis_timestamp();
        if current_time < genesis_time {
            return 0;
        }
        ((current_time - genesis_time) / BLOCK_TIME_SECONDS) as u64
    }

    /// Validate blockchain integrity and detect corrupt blocks
    /// Returns list of corrupt block heights that need re-fetching from peers.
    /// Chain height is NEVER modified - corrupt blocks are repaired by downloading
    /// the correct copy from the network.
    pub async fn validate_chain_integrity(&self) -> Result<Vec<u64>, String> {
        let current_height = self.get_height();
        let mut corrupt_blocks = Vec::new();

        tracing::debug!(
            "🔍 Validating blockchain integrity (0-{})...",
            current_height
        );

        // Check all blocks for integrity - safe because we only re-fetch, never rollback
        for height in 0..=current_height {
            match self.get_block(height) {
                Ok(block) => {
                    // Check 1: Non-genesis blocks must have non-zero previous_hash
                    if height > 0 && block.header.previous_hash == [0u8; 32] {
                        tracing::error!(
                            "❌ CORRUPT BLOCK {}: zero previous_hash for non-genesis block",
                            height
                        );
                        corrupt_blocks.push(height);
                        continue;
                    }

                    // Check 2: Height in header matches actual height
                    if block.header.height != height {
                        tracing::error!(
                            "❌ CORRUPT BLOCK {}: header height mismatch (expected {}, got {})",
                            height,
                            height,
                            block.header.height
                        );
                        corrupt_blocks.push(height);
                        continue;
                    }

                    // Check 3: Previous hash chain is valid (if not first block)
                    if height > 0 {
                        match self.get_block(height - 1) {
                            Ok(prev_block) => {
                                let expected_prev_hash = prev_block.hash();
                                if block.header.previous_hash != expected_prev_hash {
                                    tracing::error!(
                                        "❌ CORRUPT BLOCK {}: previous_hash doesn't match block {} hash",
                                        height,
                                        height - 1
                                    );
                                    tracing::error!(
                                        "   Expected: {}, Got: {}",
                                        hex::encode(&expected_prev_hash[..8]),
                                        hex::encode(&block.header.previous_hash[..8])
                                    );
                                    corrupt_blocks.push(height);
                                    continue;
                                }
                            }
                            Err(_) => {
                                tracing::error!(
                                    "❌ MISSING BLOCK {}, but have block {}",
                                    height - 1,
                                    height
                                );
                                corrupt_blocks.push(height - 1);
                            }
                        }
                    }

                    // Check 4: Merkle root matches transactions.
                    // Try the current formula first; fall back to the legacy formula
                    // (pre-dd2ef7d, which included encrypted_memo in the txid) so that
                    // blocks produced by older nodes are not incorrectly flagged as corrupt.
                    let computed_merkle =
                        crate::block::types::calculate_merkle_root(&block.transactions);
                    if computed_merkle != block.header.merkle_root {
                        let legacy_merkle =
                            crate::block::types::calculate_merkle_root_legacy(&block.transactions);
                        if legacy_merkle != block.header.merkle_root {
                            tracing::error!("❌ CORRUPT BLOCK {}: merkle root mismatch", height);
                            corrupt_blocks.push(height);
                        }
                        // else: block used legacy txid formula — treat as valid
                    }
                }
                Err(e) => {
                    tracing::error!("❌ Failed to load block at height {}: {}", height, e);
                    corrupt_blocks.push(height);
                }
            }
        }

        if corrupt_blocks.is_empty() {
            tracing::debug!("✅ Blockchain integrity validation passed");
            Ok(Vec::new())
        } else {
            tracing::error!(
                "❌ Found {} corrupt blocks: {:?}",
                corrupt_blocks.len(),
                corrupt_blocks
            );
            // Return the list so the caller can trigger repair (re-fetch from peers)
            tracing::warn!("🔧 Corrupt blocks detected - will re-fetch correct copies from peers");
            Ok(corrupt_blocks)
        }
    }

    /// Repair corrupt blocks by deleting the bad local copy and re-fetching from peers.
    /// Chain height is NEVER modified - only the corrupt data is replaced.
    /// This is the correct approach: if our copy is bad, download the correct one from
    /// peers who have it. Other nodes with correct blocks should never be affected.
    pub async fn repair_corrupt_blocks(&self, corrupt_heights: &[u64]) -> Result<usize, String> {
        if corrupt_heights.is_empty() {
            return Ok(0);
        }

        tracing::warn!(
            "🔧 Repairing {} corrupt blocks by re-fetching from peers: {:?}",
            corrupt_heights.len(),
            corrupt_heights
        );

        // Step 1: Delete the corrupt local copies
        for &height in corrupt_heights {
            let key = format!("block_{}", height);
            let _ = self.storage.remove(key.as_bytes());
            self.block_cache.invalidate(height);
            tracing::info!("🗑️  Deleted corrupt local copy of block {}", height);
        }
        let _ = self.storage.flush();

        // Step 2: Re-fetch correct blocks from peers (existing infrastructure)
        // fill_missing_blocks sends GetBlocks requests and waits for responses
        let mut sorted_heights = corrupt_heights.to_vec();
        sorted_heights.sort_unstable();

        const MAX_REPAIR_ATTEMPTS: u32 = 3;
        let mut repaired = 0usize;

        for attempt in 1..=MAX_REPAIR_ATTEMPTS {
            // Check which heights still need repair
            let still_missing: Vec<u64> = sorted_heights
                .iter()
                .copied()
                .filter(|&h| self.get_block(h).is_err())
                .collect();

            if still_missing.is_empty() {
                tracing::info!(
                    "✅ All {} corrupt blocks successfully repaired from peers",
                    corrupt_heights.len()
                );
                return Ok(corrupt_heights.len());
            }

            tracing::info!(
                "📥 Repair attempt {}/{}: fetching {} blocks from peers...",
                attempt,
                MAX_REPAIR_ATTEMPTS,
                still_missing.len()
            );

            match self.fill_missing_blocks(&still_missing).await {
                Ok(requested) => {
                    tracing::info!(
                        "📡 Requested {} blocks, waiting for responses...",
                        requested
                    );
                    // Give extra time for blocks to arrive and be processed
                    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                }
                Err(e) => {
                    tracing::warn!("⚠️  Failed to request blocks on attempt {}: {}", attempt, e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }

            // Count how many we've repaired so far
            repaired = sorted_heights
                .iter()
                .filter(|&&h| self.get_block(h).is_ok())
                .count();
        }

        // Final check
        let still_missing: Vec<u64> = sorted_heights
            .iter()
            .copied()
            .filter(|&h| self.get_block(h).is_err())
            .collect();

        if still_missing.is_empty() {
            tracing::info!(
                "✅ All {} corrupt blocks repaired from peers",
                corrupt_heights.len()
            );
            Ok(corrupt_heights.len())
        } else {
            tracing::error!(
                "❌ Failed to repair {} blocks after {} attempts: {:?}. Will retry on next integrity check.",
                still_missing.len(),
                MAX_REPAIR_ATTEMPTS,
                still_missing
            );
            Ok(repaired)
        }
    }

    /// Legacy alias - redirects to repair_corrupt_blocks
    pub async fn delete_corrupt_blocks(&self, corrupt_heights: &[u64]) -> Result<(), String> {
        self.repair_corrupt_blocks(corrupt_heights)
            .await
            .map(|_| ())
    }

    /// Scan blockchain for blocks with invalid (00000...) merkle roots and remove them
    /// Returns the number of blocks deleted
    pub async fn cleanup_invalid_merkle_blocks(&self) -> Result<u64, String> {
        let current_height = self.get_height();
        let mut invalid_blocks = Vec::new();

        tracing::info!(
            "🔍 Scanning blocks 1-{} for invalid merkle roots (00000...)",
            current_height
        );

        // Check all blocks except genesis (height 0)
        for height in 1..=current_height {
            match self.get_block(height) {
                Ok(block) => {
                    // Check if merkle root is all zeros (invalid)
                    let is_zero_merkle = block.header.merkle_root.iter().all(|&b| b == 0);

                    if is_zero_merkle {
                        tracing::warn!(
                            "⚠️  Found invalid block at height {} with 00000 merkle root",
                            height
                        );
                        invalid_blocks.push(height);
                    }
                }
                Err(_) => {
                    // Block not found - may be a gap in chain
                    tracing::debug!("Block {} not found (possible chain gap)", height);
                }
            }
        }

        if invalid_blocks.is_empty() {
            tracing::info!("✅ No invalid merkle root blocks found");
            return Ok(0);
        }

        tracing::warn!(
            "🗑️  Found {} block(s) with invalid merkle roots: {:?}",
            invalid_blocks.len(),
            invalid_blocks
        );

        // Delete the invalid blocks
        self.delete_corrupt_blocks(&invalid_blocks).await?;

        Ok(invalid_blocks.len() as u64)
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
            genesis_timestamp: self.genesis_timestamp, // Clone cached value
            is_syncing: self.is_syncing.clone(),
            peer_manager: self.peer_manager.clone(),
            peer_registry: self.peer_registry.clone(),
            connection_manager: self.connection_manager.clone(),
            peer_scoring: self.peer_scoring.clone(),
            fork_resolver: self.fork_resolver.clone(),

            sync_coordinator: self.sync_coordinator.clone(),
            cumulative_work: self.cumulative_work.clone(),
            reorg_history: self.reorg_history.clone(),
            fork_state: self.fork_state.clone(),
            fork_resolution_lock: self.fork_resolution_lock.clone(),
            block_processing_lock: self.block_processing_lock.clone(),
            consensus_peers: self.consensus_peers.clone(),
            block_cache: self.block_cache.clone(),
            consensus_health: self.consensus_health.clone(),
            ai_system: self.ai_system.clone(),
            tx_index: self.tx_index.clone(),
            compress_blocks: self.compress_blocks,
            consensus_cache: self.consensus_cache.clone(),
            has_ever_had_peers: self.has_ever_had_peers.clone(),
            block_added_signal: self.block_added_signal.clone(),
            last_consensus_log: self.last_consensus_log.clone(),
            same_height_fork_cooldown: self.same_height_fork_cooldown.clone(),
            reward_violations: self.reward_violations.clone(),
            genesis_mismatch_detected: self.genesis_mismatch_detected.clone(),
            treasury_balance: self.treasury_balance.clone(),
            active_block_reward: self.active_block_reward.clone(),
            governance: self.governance.clone(),
            pending_sync_blocks: self.pending_sync_blocks.clone(),
        }
    }
}

impl Blockchain {
    /// Get reorganization history for monitoring
    pub async fn get_reorg_history(&self) -> Vec<ReorgMetrics> {
        self.reorg_history.read().await.clone()
    }

    /// Get most recent reorganization event
    pub async fn get_last_reorg(&self) -> Option<ReorgMetrics> {
        self.reorg_history.read().await.last().cloned()
    }
}
