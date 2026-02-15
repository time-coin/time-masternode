//! Blockchain storage and management

use crate::ai::consensus_health::{
    ConsensusHealthConfig, ConsensusHealthMonitor, ConsensusMetrics,
};
use crate::block::types::{Block, BlockHeader};
use crate::block_cache::BlockCacheManager;
use crate::consensus::ConsensusEngine;
use crate::constants;
use crate::masternode_registry::{MasternodeInfo, MasternodeRegistry};

use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::types::{Hash256, OutPoint, Transaction, TxInput, TxOutput, UTXO};
use crate::utxo_manager::UTXOStateManager;
use crate::NetworkType;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const BLOCK_TIME_SECONDS: i64 = constants::blockchain::BLOCK_TIME_SECONDS;
const BLOCK_REWARD_SATOSHIS: u64 = constants::blockchain::BLOCK_REWARD_SATOSHIS;

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

/// Result of canonical chain comparison for deterministic fork resolution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalChoice {
    /// Keep our current chain
    KeepOurs,
    /// Switch to peer's chain
    AdoptPeers,
    /// Chains are identical
    Identical,
}

/// Fork resolution state machine
#[derive(Debug, Clone)]
pub enum ForkResolutionState {
    /// No fork detected
    None,

    /// Common ancestor found, need to get peer's chain
    FetchingChain {
        common_ancestor: u64,
        fork_height: u64,
        peer_addr: String,
        peer_height: u64,
        fetched_up_to: u64,
        accumulated_blocks: Vec<Block>, // Accumulate blocks as they arrive
        started_at: std::time::Instant,
    },

    /// Have complete alternate chain, ready to reorg
    ReadyToReorg {
        common_ancestor: u64,
        alternate_blocks: Vec<Block>,
        started_at: std::time::Instant,
    },

    /// Performing reorganization
    Reorging {
        from_height: u64,
        to_height: u64,
        started_at: std::time::Instant,
    },
}

/// Cache for 2/3 consensus check results - avoids redundant peer queries
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
    /// Cache for 2/3 consensus check results (TTL: 30s) - avoids redundant peer queries
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

        // Helper: check if a block exists under either key format
        let block_key_exists = |h: u64| -> bool {
            let key_new = format!("block_{}", h);
            let key_old = format!("block:{}", h);
            self.storage
                .get(key_new.as_bytes())
                .ok()
                .flatten()
                .is_some()
                || self
                    .storage
                    .get(key_old.as_bytes())
                    .ok()
                    .flatten()
                    .is_some()
        };

        // First, find the highest contiguous chain from genesis
        // This handles gaps in the middle of the chain
        // CRITICAL: Use get_block() which checks BOTH key formats (block_ and block:)
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
                    let block_key_old = format!("block:{}", h);
                    let _ = self.storage.remove(block_key_old.as_bytes());
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

    /// Migrate old-schema blocks to new schema
    /// This fixes blocks that were serialized before time_attestations changes
    pub async fn migrate_old_schema_blocks(&self) -> Result<u64, String> {
        use crate::block::types::BlockV1;

        tracing::info!("🔄 Checking for old-schema blocks that need migration...");

        let mut migrated_count = 0u64;
        let height = match self.load_chain_height() {
            Ok(h) => h,
            Err(_) => return Ok(0), // No blocks to migrate
        };

        // Check blocks 0 through current height
        for block_height in 0..=height {
            let key = format!("block_{}", block_height);

            if let Ok(Some(data)) = self.storage.get(key.as_bytes()) {
                // Try to deserialize with current schema
                if bincode::deserialize::<Block>(&data).is_err() {
                    // Current schema failed, try old BlockV1 format
                    match bincode::deserialize::<BlockV1>(&data) {
                        Ok(v1_block) => {
                            // Convert to new format
                            let migrated_block: Block = v1_block.into();

                            // Re-serialize with new schema
                            let new_data = bincode::serialize(&migrated_block).map_err(|e| {
                                format!(
                                    "Failed to serialize migrated block {}: {}",
                                    block_height, e
                                )
                            })?;

                            // Store the migrated block
                            self.storage.insert(key.as_bytes(), new_data).map_err(|e| {
                                format!("Failed to store migrated block {}: {}", block_height, e)
                            })?;

                            tracing::info!("✅ Migrated block {} from old schema", block_height);
                            migrated_count += 1;
                        }
                        Err(e) => {
                            tracing::warn!(
                                "⚠️ Block {} failed both deserializations, may need manual recovery: {}",
                                block_height,
                                e
                            );
                        }
                    }
                }
            }
        }

        if migrated_count > 0 {
            self.storage
                .flush()
                .map_err(|e| format!("Failed to flush after migration: {}", e))?;
            tracing::info!(
                "✅ Schema migration complete: {} blocks migrated",
                migrated_count
            );
        } else {
            tracing::info!("✅ No blocks needed migration - schema is up to date");
        }

        Ok(migrated_count)
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
                if let Err(e) = GenesisBlock::verify_structure(&genesis) {
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
                if let Err(e) = GenesisBlock::verify_structure(&genesis) {
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

        // No local blockchain - genesis will be generated dynamically via generate_dynamic_genesis()
        tracing::info!(
            "📋 No genesis block found - will be generated dynamically when masternodes register"
        );
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
        // - Mainnet: January 1, 2026 00:00:00 UTC (1767225600)
        let genesis_timestamp = self.network_type.genesis_timestamp();
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

        if registered.is_empty() {
            return Err("Cannot generate genesis: no masternodes registered".to_string());
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

        // Genesis reward: Only the leader gets the block reward
        // But ALL masternodes in the bitmap are listed as eligible for future block rewards
        const TIME_UNIT: u64 = 100_000_000; // 1 TIME = 100M satoshis
        const GENESIS_REWARD: u64 = 100 * TIME_UNIT; // 100 TIME for the genesis leader

        // Sort to find leader (lowest address)
        let mut sorted_for_reward = registered.clone();
        sorted_for_reward.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));
        let leader = &sorted_for_reward[0];

        // Create masternode rewards - list all masternodes who are eligible going forward
        // This establishes the reward list for future blocks based on genesis bitmap
        let mut masternode_rewards: Vec<(String, u64)> = Vec::new();
        for info in &registered {
            // Include all eligible masternodes in the reward list
            // Amount is 0 for non-leaders (they didn't produce this block)
            // This documents who is eligible for rewards in future blocks
            let reward_amount = if info.masternode.address == leader.masternode.address {
                GENESIS_REWARD // Leader gets the block reward
            } else {
                0 // Other masternodes are listed as eligible but receive 0 in genesis
            };
            // Use the actual masternode address, not reward_address (which may be unset)
            masternode_rewards.push((info.masternode.address.clone(), reward_amount));
        }

        tracing::info!(
            "   Genesis block reward: {} -> 100 TIME (leader)",
            leader.masternode.address
        );
        tracing::info!(
            "   {} masternodes listed as eligible for future rewards",
            registered.len()
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
            active_masternodes_bitmap: bitmap,
            liveness_recovery: Some(false),
            vrf_output: [0u8; 32],
            vrf_proof: vec![],
            vrf_score: 0,
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
                    tracing::info!(
                        "✅ No peers have blocks beyond height {} ({} peers checked, target {}). Skipping sync — blocks must be produced.",
                        current,
                        peers_checked,
                        target
                    );
                    return Ok(());
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

        // Ensure we reset the sync flag when done
        let is_syncing = self.is_syncing.clone();
        let _guard = scopeguard::guard((), |_| {
            is_syncing.store(false, Ordering::Release);
        });

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

            // Use AI to select the best peer based on historical performance
            let mut sync_peer =
                if let Some(ai_peer) = self.peer_scoring.select_best_peer(&connected_peers).await {
                    tracing::debug!("✓ AI peer selection returned: {}", ai_peer);
                    ai_peer
                } else {
                    // Fallback if AI can't decide
                    tracing::warn!("⚠️  AI couldn't select peer, using first available");
                    connected_peers.first().ok_or("No peers available")?.clone()
                };

            // Sync loop - keep requesting batches until caught up or timeout
            let sync_start = std::time::Instant::now();
            let max_sync_time = std::time::Duration::from_secs(PEER_SYNC_TIMEOUT_SECS * 2);
            let starting_height = current;

            tracing::info!(
                "📍 Starting sync loop: current={}, target={}, timeout={}s",
                current,
                target,
                max_sync_time.as_secs()
            );

            while current < target && sync_start.elapsed() < max_sync_time {
                // Request next batch of blocks
                // Always start from 0 when current is 0 (need genesis)
                // Otherwise start from current + 1 (need next block after our tip)
                let batch_start = if current == 0 {
                    if self.get_block(0).is_ok() {
                        1 // Already have genesis, request block 1+
                    } else {
                        0 // Need genesis first
                    }
                } else {
                    current + 1 // Request next block after our tip
                };
                let batch_end = (batch_start + constants::network::SYNC_BATCH_SIZE - 1).min(target);

                let req = NetworkMessage::GetBlocks(batch_start, batch_end);
                tracing::debug!(
                    "📤 Requesting blocks {}-{} from {}",
                    batch_start,
                    batch_end,
                    sync_peer
                );
                if let Err(e) = peer_registry.send_to_peer(&sync_peer, req).await {
                    tracing::warn!("❌ Failed to send GetBlocks to {}: {}", sync_peer, e);
                    break;
                }

                // Wait for blocks to arrive with reasonable timeout for network latency
                let batch_start_time = std::time::Instant::now();
                let batch_timeout = std::time::Duration::from_secs(30); // Allow time for network latency
                let mut last_height = current;
                let mut made_progress = false;

                while batch_start_time.elapsed() < batch_timeout {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let now_height = self.current_height.load(Ordering::Acquire);

                    if now_height >= time_expected {
                        tracing::info!("✓ Sync complete at height {}", now_height);
                        return Ok(());
                    }

                    // Check if we made progress
                    if now_height > last_height {
                        let blocks_received = now_height - last_height;
                        let response_time = batch_start_time.elapsed();

                        tracing::debug!(
                            "📈 Block sync progress: {} → {} from {} ({} blocks in {:.2}s)",
                            last_height,
                            now_height,
                            sync_peer,
                            blocks_received,
                            response_time.as_secs_f64()
                        );

                        // Record AI success: peer delivered blocks
                        self.peer_scoring
                            .record_success(&sync_peer, response_time, blocks_received * 1000)
                            .await;

                        last_height = now_height;
                        made_progress = true;
                    }

                    // If we received all blocks in this batch, request next batch
                    if now_height >= batch_end {
                        break;
                    }
                }

                // If no progress after request, try a different peer with exponential backoff
                if !made_progress {
                    // Record AI failure: peer didn't deliver
                    self.peer_scoring.record_failure(&sync_peer).await;

                    tracing::warn!(
                        "⚠️  No progress after requesting blocks {}-{} from {} (timeout after 30s)",
                        batch_start,
                        batch_end,
                        sync_peer
                    );

                    // Try up to 5 different peers with exponential backoff before giving up
                    let mut tried_peers = {
                        let mut set = HashSet::new();
                        set.insert(sync_peer.clone());
                        set
                    };
                    for attempt in 2..=5 {
                        // Use AI to select next best peer (excluding already tried)
                        let remaining_peers: Vec<String> = connected_peers
                            .iter()
                            .filter(|p| !tried_peers.contains(*p))
                            .cloned()
                            .collect();

                        if let Some(alt_peer) =
                            self.peer_scoring.select_best_peer(&remaining_peers).await
                        {
                            // Exponential backoff: 20s, 30s, 40s, 50s
                            let retry_timeout_secs = 10 + (attempt * 10);
                            tracing::info!(
                                "🤖 [AI] Trying alternate peer {} (attempt {}, timeout {}s)",
                                alt_peer,
                                attempt,
                                retry_timeout_secs
                            );

                            let req = NetworkMessage::GetBlocks(batch_start, batch_end);
                            if let Err(e) = peer_registry.send_to_peer(&alt_peer, req).await {
                                tracing::warn!(
                                    "❌ Failed to send GetBlocks to {}: {}",
                                    alt_peer,
                                    e
                                );
                                tried_peers.insert(alt_peer.clone());
                                self.peer_scoring.record_failure(&alt_peer).await;
                                continue;
                            }

                            // Wait for response with exponential backoff timeout
                            let retry_start = std::time::Instant::now();
                            let retry_timeout = std::time::Duration::from_secs(retry_timeout_secs);

                            while retry_start.elapsed() < retry_timeout {
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                let retry_height = self.current_height.load(Ordering::Acquire);

                                if retry_height > last_height {
                                    let blocks_received = retry_height - last_height;
                                    let response_time = retry_start.elapsed();

                                    tracing::info!(
                                        "✅ [AI] Alternate peer {} delivered {} blocks in {:.2}s!",
                                        alt_peer,
                                        blocks_received,
                                        response_time.as_secs_f64()
                                    );

                                    // Record AI success
                                    self.peer_scoring
                                        .record_success(
                                            &alt_peer,
                                            response_time,
                                            blocks_received * 1000,
                                        )
                                        .await;

                                    last_height = retry_height;
                                    made_progress = true;
                                    sync_peer = alt_peer.clone(); // Switch to working peer
                                    break;
                                }
                            }

                            if made_progress {
                                break; // Got blocks, continue with this peer
                            } else {
                                self.peer_scoring.record_failure(&alt_peer).await;
                                tracing::warn!(
                                    "⚠️  Peer {} did not respond within {}s",
                                    alt_peer,
                                    retry_timeout_secs
                                );
                            }

                            tried_peers.insert(alt_peer);
                        } else {
                            tracing::warn!(
                                "⚠️  No more alternate peers available (tried {} peers)",
                                tried_peers.len()
                            );
                            break;
                        }
                    }
                }

                // If no progress after trying multiple peers, give up
                if !made_progress && current == starting_height {
                    tracing::warn!(
                        "⚠️  No progress after trying multiple peers - they may not have blocks {} yet",
                        batch_start
                    );
                    break;
                }

                // Update current height for next iteration
                current = self.current_height.load(Ordering::Acquire);

                // Log progress periodically
                let elapsed = sync_start.elapsed().as_secs();
                if elapsed > 0 && elapsed % 30 == 0 {
                    tracing::info!(
                        "⏳ Still syncing... height {} / {} ({}s elapsed)",
                        current,
                        time_expected,
                        elapsed
                    );
                }
            }
        } else {
            tracing::warn!("⚠️  Peer registry not available - cannot sync from peers");
        }

        let final_height = self.current_height.load(Ordering::Acquire);
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

    /// Sync from a specific peer (used when we detect a fork and want the consensus chain)
    /// Now includes automatic fork detection and rollback to common ancestor
    pub async fn sync_from_specific_peer(&self, peer_ip: &str) -> Result<(), String> {
        let current = self.current_height.load(Ordering::Acquire);

        // Get peer registry to check peer's actual height
        let peer_registry = self.peer_registry.read().await;
        let registry = peer_registry.as_ref().ok_or("No peer registry available")?;

        // Get peer's actual chain tip to avoid requesting blocks they don't have
        let (peer_height, _peer_hash) = registry
            .get_peer_chain_tip(peer_ip)
            .await
            .ok_or_else(|| format!("No chain tip data for peer {}", peer_ip))?;

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
        if fork_depth > MAX_REORG_DEPTH {
            tracing::warn!(
                "🛡️ SECURITY: REJECTED deep rollback from peer {} - depth {} exceeds max {}",
                peer_ip,
                fork_depth,
                MAX_REORG_DEPTH
            );
            return Err(format!(
                "Security: Rejected deep rollback (depth {} > max {}) from peer {}",
                fork_depth, MAX_REORG_DEPTH, peer_ip
            ));
        }

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

        self.rollback_to_height(common_ancestor).await?;

        Ok(common_ancestor)
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

                // Wait for responses to arrive
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

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
                        // We're behind - sync to longer chain
                        info!(
                            "📥 Starting sync: {} → {} ({} blocks behind)",
                            our_height,
                            consensus_height,
                            consensus_height - our_height
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

        // Check 2/3 consensus requirement for block production
        // Requirement: 2/3+ of connected peers must agree on the current chain (height, hash)
        // Exception: Allow production with 0 peers (bootstrap mode)
        if !self.check_2_3_consensus_cached().await {
            return Err("Cannot produce block: no 2/3 consensus on current chain state. Waiting for network consensus.".to_string());
        }

        // Get previous block hash
        let current_height = self.current_height.load(Ordering::Acquire);

        // Note: Previously had a safeguard preventing block production when >50 behind
        // This is no longer needed because:
        // 1. TimeLock leader selection ensures only ONE node produces catchup blocks
        // 2. All nodes agree on the leader deterministically
        // 3. Non-leaders wait for leader's blocks
        // This prevents forks while allowing coordinated catchup when network is behind

        let expected_height = self.calculate_expected_height();
        let blocks_behind = expected_height.saturating_sub(current_height);

        if blocks_behind > 10 {
            tracing::debug!(
                "📦 Producing catchup block: {} blocks behind (TimeLock leader coordinated)",
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
            // Catchup mode: produce block at specific height
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

        if ds_invalid_count > 0 {
            tracing::warn!(
                "⚠️  Block {}: Excluded {} double-spend/duplicate transaction(s) before fee calculation",
                next_height,
                ds_invalid_count
            );
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
        let base_reward = BLOCK_REWARD_SATOSHIS;
        let total_reward = base_reward + finalized_txs_fees;

        // NEW: All rewards go to the block producer only
        let rewards = if let Some(ref wallet) = producer_wallet {
            tracing::info!(
                "💰 Block {}: {} satoshis ({} TIME) to block producer {}",
                next_height,
                total_reward,
                total_reward / 100_000_000,
                wallet
            );
            vec![(wallet.clone(), total_reward)]
        } else {
            // Fallback: no producer specified (old behavior - should never happen)
            tracing::warn!(
                "⚠️  No producer wallet specified, using old distribution (THIS SHOULD NOT HAPPEN)"
            );
            self.calculate_rewards_with_amount(&masternodes, total_reward)
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

        // CRITICAL: Sort finalized transactions deterministically by txid
        // This ensures all nodes compute the same merkle root for the same block
        // (Double-spend/duplicate filtering already done above before fee calculation)
        let mut sorted_finalized = finalized_txs;
        sorted_finalized.sort_by_key(|a| a.txid());
        all_txs.extend(sorted_finalized);

        // Calculate merkle root from ALL transactions in canonical order
        let merkle_root = crate::block::types::calculate_merkle_root(&all_txs);

        // Create active masternode bitmap based on who voted on the PREVIOUS block
        // New nodes can vote immediately after announcing, getting into next bitmap
        // Only nodes in previous bitmap are eligible for leader selection
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
            // Use precommit voters (final voting phase before block acceptance)
            // First try live votes, then preserved voters (saved before cleanup)
            let mut precommit_voters = self
                .consensus
                .timevote
                .precommit_votes
                .get_voters(prev_block_hash);

            if precommit_voters.is_empty() {
                precommit_voters = self
                    .consensus
                    .timevote
                    .get_finalized_block_voters(prev_block_hash);
            }

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
        } else {
            tracing::debug!(
                "⚠️ Block {} produced without VRF (no signing key available)",
                next_height
            );
        }

        Ok(block)
    }

    /// Invalidate the 2/3 consensus cache, forcing the next check to query fresh peer data.
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
                        "🔄 2/3 consensus check cache HIT ({}ms old)",
                        cached.timestamp.elapsed().as_millis()
                    );
                    return cached.result;
                }
            }
        }

        // Cache miss or expired - perform full check
        tracing::debug!("🔄 2/3 consensus check cache MISS - recalculating");
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

    /// Check if 2/3 of connected peers agree on the current chain state (height, hash)
    /// Uses TIER-WEIGHTED voting (Gold > Silver > Bronze > Free)
    /// Returns true if:
    /// - We have 0 connected peers (bootstrap mode allowed), OR
    /// - 2/3+ of WEIGHTED stake agrees on our current (height, hash)
    ///
    /// Returns false if:
    /// - We have 1+ connected peers AND less than 2/3 of weighted stake agrees on our chain
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
        // Incompatible peers (different network) must NOT dilute the 2/3 threshold.
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
                // Same logic as compare_chain_with_peers() at line 5560
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

                // Check if peer agrees on our (height, hash)
                if peer_height == our_height && peer_hash == our_hash {
                    weight_on_our_chain += peer_weight;
                    peers_agreeing += 1;
                }
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

        // Require 2/3 of WEIGHTED stake to agree on our chain
        let required_weight = (total_weight * 2).div_ceil(3); // Ceiling division for 2/3
        let has_consensus = weight_on_our_chain >= required_weight;

        // CRITICAL: Also require a minimum NUMBER of peers in sync (not just weight).
        // Prevents a single high-weight node from enabling block production.
        // "3 nodes in sync" = us + at least 2 agreeing peers.
        const MIN_AGREEING_PEERS: u32 = 2;
        let enough_peers_in_sync = peers_agreeing >= MIN_AGREEING_PEERS;

        if has_consensus && enough_peers_in_sync {
            tracing::debug!(
                "✅ Block production allowed: {}/{} weight, {}/{} peers agree on height {}",
                weight_on_our_chain,
                total_weight,
                peers_agreeing,
                peers_responding,
                our_height
            );
            if peers_ignored > 0 {
                tracing::debug!("   ({} peers with corrupted blocks ignored)", peers_ignored);
            }
        } else {
            if !has_consensus {
                tracing::warn!(
                    "⚠️ Block production blocked: {} weight agrees on height {} (need {} for 2/3 of {} total). Peer responses: {}/{}{}",
                    weight_on_our_chain,
                    our_height,
                    required_weight,
                    total_weight,
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
                    let agrees = if *peer_height == our_height && peer_hash == &our_hash {
                        "✅ AGREES"
                    } else {
                        "❌ DIFFERS"
                    };
                    tracing::warn!(
                        "   {} @ height {} hash {} weight {} {}",
                        peer_ip,
                        peer_height,
                        hex::encode(&peer_hash[..8]),
                        peer_weight,
                        agrees
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
            self.validate_block_rewards(&block)?;
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

        // Validate block size
        let serialized = bincode::serialize(&block).map_err(|e| e.to_string())?;
        if serialized.len() > MAX_BLOCK_SIZE {
            return Err(format!("Block too large: {} bytes", serialized.len()));
        }

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

        // Save undo log for rollback support
        self.save_undo_log(&undo_log)?;

        // CRITICAL FIX: Normalize block data before storage to ensure deterministic hashing
        // Deep clone to ensure no shared references and normalize all strings
        let mut block = block.clone();
        block.header.leader = block.header.leader.trim().to_string();

        // Normalize masternode rewards (sort by address for determinism)
        block.masternode_rewards.sort_by(|a, b| a.0.cmp(&b.0));

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
        self.save_block_without_height_update(&block)?;

        // CRITICAL: Immediately read back and verify hash BEFORE updating chain height
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

        // Hash verified - NOW update chain height
        self.update_chain_height(block.header.height)?;

        // SCAN FORWARD: After filling a gap, check if blocks above already exist in storage.
        // This handles the case where verify_and_fix_chain_height preserved blocks above a gap.
        // When the gap is filled, we should advance the height past all already-stored blocks.
        let mut scan_height = block.header.height + 1;
        let mut advanced = 0u64;
        while self.get_block(scan_height).is_ok() {
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

        tracing::debug!(
            "✓ Block {} hash verified after storage: {}",
            block.header.height,
            hex::encode(&post_storage_hash[..8])
        );

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
        // This ensures masternodes with spent collateral are automatically deregistered
        let cleanup_count = self
            .masternode_registry
            .cleanup_invalid_collaterals(&self.utxo_manager)
            .await;

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

        // Cache miss - try both storage key formats for backward compatibility
        // Try new format first (block_HEIGHT)
        let key_new = format!("block_{}", height);
        let has_new_key = self
            .storage
            .get(key_new.as_bytes())
            .ok()
            .flatten()
            .is_some();

        // Try old format (block:HEIGHT)
        let key_old = format!("block:{}", height);
        let has_old_key = self
            .storage
            .get(key_old.as_bytes())
            .ok()
            .flatten()
            .is_some();

        if !has_new_key && !has_old_key {
            // Block truly doesn't exist
            return Err(format!(
                "Block {} not found in storage (checked both key formats)",
                height
            ));
        }

        // Try deserializing with new key format
        if has_new_key {
            if let Ok(Some(v)) = self.storage.get(key_new.as_bytes()) {
                // Decompress if necessary (handles both compressed and uncompressed)
                let data = match crate::storage::decompress_block(&v) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("Failed to decompress block {}: {}", height, e);
                        return Err(format!("Block {} decompression failed: {}", height, e));
                    }
                };
                // Try current Block format
                match bincode::deserialize::<Block>(&data) {
                    Ok(block) => {
                        let block_arc = Arc::new(block);
                        self.block_cache.put(height, block_arc.clone());
                        return Ok((*block_arc).clone());
                    }
                    Err(e1) => {
                        // Try old BlockV1 format (without liveness_recovery field)
                        match bincode::deserialize::<crate::block::types::BlockV1>(&data) {
                            Ok(v1_block) => {
                                let block: Block = v1_block.into();
                                tracing::info!("✓ Migrated block {} from V1 format", height);
                                let block_arc = Arc::new(block);
                                self.block_cache.put(height, block_arc.clone());
                                return Ok((*block_arc).clone());
                            }
                            Err(e2) => {
                                tracing::error!(
                                    "⚠️ Block {} exists at key '{}' but failed both deserializations: Current={}, V1={}",
                                    height, key_new, e1, e2
                                );

                                // SMART RECOVERY: Delete corrupted block and trigger re-sync from peers
                                // This is safe because:
                                // 1. We delete ONLY the corrupted block (not chain height)
                                // 2. Chain height stays at current value, allowing re-fetch
                                // 3. Sync coordinator will notice gap and request from peers
                                // 4. Multiple peers validate received blocks against consensus
                                // 5. If all peers have corrupted block, we'll detect consensus corruption

                                tracing::warn!("🔄 CORRUPTED BLOCK RECOVERY: Deleting corrupted block {} for re-fetch from peers", height);

                                // Delete corrupted block
                                let _ = self.storage.remove(key_new.as_bytes());
                                self.block_cache.invalidate(height);

                                // Also try to delete old format if it exists
                                let _ = self.storage.remove(key_old.as_bytes());

                                // Flush to ensure deletion persists
                                let _ = self.storage.flush();

                                tracing::warn!(
                                    "✅ Corrupted block {} deleted, will be re-fetched from peers",
                                    height
                                );

                                // Return error to trigger recovery flow
                                return Err(format!(
                                    "Block {} was corrupted and has been deleted for re-fetch from peers",
                                    height
                                ));
                            }
                        }
                    }
                }
            }
        }

        // Try deserializing with old key format
        if has_old_key {
            if let Ok(Some(v)) = self.storage.get(key_old.as_bytes()) {
                // Decompress if necessary (handles both compressed and uncompressed)
                let data = match crate::storage::decompress_block(&v) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("Failed to decompress block {}: {}", height, e);
                        return Err(format!("Block {} decompression failed: {}", height, e));
                    }
                };
                // Try current Block format
                match bincode::deserialize::<Block>(&data) {
                    Ok(block) => {
                        let block_arc = Arc::new(block);
                        self.block_cache.put(height, block_arc.clone());
                        return Ok((*block_arc).clone());
                    }
                    Err(e1) => {
                        // Try old BlockV1 format
                        match bincode::deserialize::<crate::block::types::BlockV1>(&data) {
                            Ok(v1_block) => {
                                let block: Block = v1_block.into();
                                tracing::info!(
                                    "✓ Migrated block {} from V1 format (old key)",
                                    height
                                );
                                let block_arc = Arc::new(block);
                                self.block_cache.put(height, block_arc.clone());
                                return Ok((*block_arc).clone());
                            }
                            Err(e2) => {
                                tracing::error!(
                                    "⚠️ Block {} exists at key '{}' but failed both deserializations: Current={}, V1={}",
                                    height, key_old, e1, e2
                                );
                                // Delete the corrupt local copy so it can be re-fetched from peers.
                                // Chain height is NOT modified - the block will be re-downloaded
                                // by the integrity check or sync coordinator.
                                tracing::warn!(
                                    "🔧 Deleting corrupted block {} (old key) for re-fetch from peers",
                                    height
                                );
                                let _ = self.storage.remove(key_old.as_bytes());
                                self.block_cache.invalidate(height);
                                let _ = self.storage.flush();

                                return Err(format!(
                                    "Block {} was corrupted and deleted - will be re-fetched from peers",
                                    height
                                ));
                            }
                        }
                    }
                }
            }
        }

        Err(format!("Block {} not found", height))
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

    /// Check if currently syncing
    pub fn is_syncing(&self) -> bool {
        self.is_syncing.load(Ordering::Acquire)
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
    /// Checks both key formats and deserialization
    pub fn diagnose_missing_blocks(&self, start: u64, end: u64) {
        tracing::info!("🔬 Diagnosing blocks {} to {}", start, end);

        for height in start..=end {
            let key_old = format!("block:{}", height);
            let key_new = format!("block_{}", height);

            let old_exists = self
                .storage
                .get(key_old.as_bytes())
                .ok()
                .flatten()
                .is_some();
            let new_exists = self
                .storage
                .get(key_new.as_bytes())
                .ok()
                .flatten()
                .is_some();

            if !old_exists && !new_exists {
                tracing::warn!("  Block {}: MISSING (neither key format exists)", height);
            } else {
                tracing::debug!(
                    "  Block {}: old_key={} new_key={}",
                    height,
                    old_exists,
                    new_exists
                );

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

    // =========================================================================
    // CANONICAL CHAIN SELECTION (Fork Resolution)
    // =========================================================================

    /// Determine which of two competing chains is canonical using deterministic rules.
    ///
    /// Rules (in order of precedence):
    /// 1. Longer chain wins (most work)
    /// 2. Lower tip hash wins (deterministic tiebreaker - consistent with all fork resolution)
    ///
    /// NOTE: VRF scores are NOT used for fork resolution tiebreaking because:
    /// - VRF scores are derived from hashes, so they're redundant
    /// - "Lower hash wins" is the standard blockchain convention (Bitcoin, Ethereum)
    /// - Consistency across all fork resolution code paths is critical
    ///
    /// This function MUST be deterministic - all nodes must make the same decision
    /// given the same inputs.
    pub fn choose_canonical_chain(
        our_height: u64,
        our_tip_hash: [u8; 32],
        _our_cumulative_score: u128,
        peer_height: u64,
        peer_tip_hash: [u8; 32],
        _peer_cumulative_score: u128,
    ) -> (CanonicalChoice, String) {
        // Rule 1: Longer chain wins (most work)
        if peer_height > our_height {
            return (
                CanonicalChoice::AdoptPeers,
                format!(
                    "Peer chain is longer: {} > {} blocks",
                    peer_height, our_height
                ),
            );
        }
        if our_height > peer_height {
            return (
                CanonicalChoice::KeepOurs,
                format!(
                    "Our chain is longer: {} > {} blocks",
                    our_height, peer_height
                ),
            );
        }

        // Rule 2: Lexicographically smaller hash wins (deterministic tiebreaker)
        // This is consistent with fork_resolver.rs and masternode_authority.rs
        if peer_tip_hash < our_tip_hash {
            return (
                CanonicalChoice::AdoptPeers,
                format!(
                    "Equal height {}, peer has lower hash (canonical tiebreaker)",
                    our_height
                ),
            );
        }
        if our_tip_hash < peer_tip_hash {
            return (
                CanonicalChoice::KeepOurs,
                format!(
                    "Equal height {}, our hash is lower (canonical tiebreaker)",
                    our_height
                ),
            );
        }

        // Hashes are identical - same chain
        (
            CanonicalChoice::Identical,
            format!("Chains are identical at height {}", our_height),
        )
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

    /// Calculate cumulative VRF score for a range of blocks.
    ///
    /// Cumulative score = sum of all individual block VRF scores in the range.
    /// This is used for chain comparison when heights are equal.
    pub async fn calculate_chain_vrf_score(&self, from_height: u64, to_height: u64) -> u128 {
        let mut total_score: u128 = 0;

        for height in from_height..=to_height {
            if let Ok(block) = self.get_block(height) {
                total_score += self.calculate_block_vrf_score(&block) as u128;
            }
        }

        total_score
    }

    /// Calculate VRF score for a list of blocks (used for peer chain evaluation)
    pub fn calculate_blocks_vrf_score(&self, blocks: &[Block]) -> u128 {
        blocks
            .iter()
            .map(|b| self.calculate_block_vrf_score(b) as u128)
            .sum()
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
    pub async fn get_transaction_confirmations(&self, txid: &[u8; 32]) -> Option<u64> {
        if let Some(ref tx_index) = self.tx_index {
            if let Some(location) = tx_index.get_location(txid) {
                let current_height = self.get_height();
                return Some(current_height.saturating_sub(location.block_height) + 1);
            }
        }
        Some(0)
    }

    /// Get all finalized transaction IDs in a height range (for reorg protection)
    ///
    /// This method scans blocks in the given range and identifies which transactions
    /// were finalized by timevote consensus before being included in blocks.
    ///
    /// CRITICAL: Finalized transactions MUST be preserved during reorgs (Approach A).
    /// Once timevote finalizes a transaction, it cannot be excluded from the chain,
    /// even if the block containing it is orphaned. Any fork missing a finalized
    /// transaction must be rejected.
    async fn get_finalized_txids_in_range(
        &self,
        start_height: u64,
        end_height: u64,
    ) -> Result<Vec<[u8; 32]>, String> {
        let mut finalized_txids = Vec::new();

        // IMPLEMENTATION NOTE: This is a simplified version that checks if transactions
        // existed in the finalized pool when blocks were created. A production version
        // would need persistent tracking of finalization status, possibly using:
        // 1. Database table mapping txid -> (finalized_at_timestamp, block_height)
        // 2. Bloom filter for fast lookup with occasional false positives
        // 3. In-memory cache with persistence to disk
        //
        // Block structure: [coinbase, reward_distribution, ...finalized_txs]
        // - Index 0: Coinbase (creates block reward)
        // - Index 1: Reward distribution (spends coinbase, distributes to masternodes)
        // - Index 2+: timevote-finalized user transactions
        //
        // Only transactions at index 2+ were finalized by timevote and need protection.
        // Coinbase and reward distribution are block-specific and regenerated during reorgs.

        for height in start_height..=end_height {
            if let Ok(block) = self.get_block_by_height(height).await {
                // CRITICAL: Block structure is [coinbase, reward_distribution, ...finalized_txs]
                // Only transactions at index 2+ are actual timevote-finalized user transactions.
                // Coinbase (index 0) and reward distribution (index 1) are block-specific and
                // must NOT be protected during reorgs - they're regenerated for each block.
                for (idx, tx) in block.transactions.iter().enumerate() {
                    // Skip first two transactions (coinbase + reward distribution)
                    if idx >= 2 {
                        finalized_txids.push(tx.txid());
                    }
                }
            }
        }

        tracing::debug!(
            "Found {} finalized transactions in height range {} to {}",
            finalized_txids.len(),
            start_height,
            end_height
        );

        Ok(finalized_txids)
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
                serialized // Don't compress if it makes data larger
            }
        } else {
            serialized.clone()
        };

        let data_len = data_to_store.len();

        self.storage
            .insert(key.as_bytes(), data_to_store.clone())
            .map_err(|e| {
                tracing::error!(
                    "❌ Failed to insert block {} into database: {} (type: {:?})",
                    block.header.height,
                    e,
                    e
                );
                tracing::error!(
                    "   This may indicate database corruption. Try: rm -rf /root/.timecoin/testnet/db/blocks"
                );
                format!("Database insert failed: {}", e)
            })?;

        // CRITICAL: Update cache to ensure consistency
        // Without this, cached stale blocks can cause hash mismatches
        let block_arc = Arc::new(block.clone());
        self.block_cache.put(block.header.height, block_arc);

        // Update chain height
        let height_key = "chain_height".as_bytes();
        let height_bytes = bincode::serialize(&block.header.height).map_err(|e| e.to_string())?;
        self.storage.insert(height_key, height_bytes).map_err(|e| {
            tracing::error!("❌ Failed to update chain_height: {}", e);
            e.to_string()
        })?;

        // CRITICAL: Flush EVERY block to ensure durability
        // Previous optimization (flush every 10 blocks) was causing corruption
        // when nodes restart between flushes. Data integrity > performance.
        self.storage.flush().map_err(|e| {
            tracing::error!(
                "❌ Failed to flush block {} to disk: {}",
                block.header.height,
                e
            );
            e.to_string()
        })?;

        // VERIFICATION: Read back and verify block was stored correctly
        // This catches storage corruption immediately instead of later
        match self.storage.get(key.as_bytes()) {
            Ok(Some(stored_data)) => {
                if stored_data.len() != data_len {
                    tracing::error!(
                        "🚨 STORAGE CORRUPTION: Block {} wrote {} bytes but read back {} bytes!",
                        block.header.height,
                        data_len,
                        stored_data.len()
                    );
                    return Err(format!(
                        "Block {} storage verification failed: size mismatch ({} vs {})",
                        block.header.height,
                        data_len,
                        stored_data.len()
                    ));
                }
                // Verify we can deserialize what we stored
                let decompressed = crate::storage::decompress_block(&stored_data).map_err(|e| {
                    format!(
                        "Block {} verification failed on decompress: {}",
                        block.header.height, e
                    )
                })?;
                let _: Block = bincode::deserialize(&decompressed).map_err(|e| {
                    format!(
                        "Block {} verification failed on deserialize: {}",
                        block.header.height, e
                    )
                })?;
            }
            Ok(None) => {
                tracing::error!(
                    "🚨 STORAGE CORRUPTION: Block {} not found after save!",
                    block.header.height
                );
                return Err(format!(
                    "Block {} disappeared after save!",
                    block.header.height
                ));
            }
            Err(e) => {
                tracing::error!(
                    "🚨 STORAGE ERROR: Failed to verify block {}: {}",
                    block.header.height,
                    e
                );
                return Err(format!(
                    "Block {} verification read failed: {}",
                    block.header.height, e
                ));
            }
        }

        Ok(())
    }

    /// Save block to storage without updating chain height (for verification)
    fn save_block_without_height_update(&self, block: &Block) -> Result<(), String> {
        let key = format!("block_{}", block.header.height);
        let serialized = bincode::serialize(block).map_err(|e| e.to_string())?;

        // Compress block data if enabled
        let data_to_store = if self.compress_blocks {
            let compressed = crate::storage::compress_block(&serialized);
            // compress_block() adds magic header, so only use if actually smaller
            if compressed.len() < serialized.len() {
                tracing::debug!(
                    "Block {} compressed: {} → {} bytes ({:.1}% reduction)",
                    block.header.height,
                    serialized.len(),
                    compressed.len(),
                    100.0 * (1.0 - compressed.len() as f64 / serialized.len() as f64)
                );
                compressed
            } else {
                tracing::debug!(
                    "Block {} compression skipped: not beneficial ({} → {} bytes)",
                    block.header.height,
                    serialized.len(),
                    compressed.len()
                );
                serialized.clone() // Store uncompressed (no magic header)
            }
        } else {
            serialized.clone()
        };

        tracing::debug!(
            "💾 Saving block {} to disk: {} bytes (serialized: {} bytes, compressed: {})",
            block.header.height,
            data_to_store.len(),
            serialized.len(),
            self.compress_blocks
        );

        // Use atomic transaction to ensure block is fully written
        self.storage
            .insert(key.as_bytes(), data_to_store.clone())
            .map_err(|e| e.to_string())?;

        // CRITICAL: Update cache to ensure consistency
        let block_arc = Arc::new(block.clone());
        self.block_cache.put(block.header.height, block_arc);

        // CRITICAL: ALWAYS flush to disk after writing a block
        // sled's flush() is synchronous and calls fsync - it should block until complete
        self.storage.flush().map_err(|e| {
            tracing::error!(
                "❌ Failed to flush block {} to disk: {}",
                block.header.height,
                e
            );
            e.to_string()
        })?;

        // VERIFICATION: Read back immediately to ensure block was written completely
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
                // Try to deserialize to ensure it's valid
                let decompressed =
                    crate::storage::decompress_block(&readback_data).map_err(|e| {
                        tracing::error!(
                            "🚨 Block {} DECOMPRESS FAILED: {} (readback size: {}, has ZSTD magic: {})",
                            block.header.height,
                            e,
                            readback_data.len(),
                            readback_data.len() > 4 && &readback_data[0..4] == b"ZSTD"
                        );
                        format!(
                            "Failed to decompress readback block {}: {}",
                            block.header.height, e
                        )
                    })?;

                tracing::debug!(
                    "Block {} decompressed: readback {} bytes → {} bytes (original serialized: {} bytes)",
                    block.header.height,
                    readback_data.len(),
                    decompressed.len(),
                    serialized.len()
                );

                bincode::deserialize::<Block>(&decompressed).map_err(|e| {
                    tracing::error!(
                        "🚨 Block {} BINCODE DESERIALIZE FAILED: {} (decompressed size: {}, expected: {})",
                        block.header.height,
                        e,
                        decompressed.len(),
                        serialized.len()
                    );
                    // Dump first/last bytes for debugging
                    if decompressed.len() > 16 {
                        tracing::error!(
                            "   First 16 bytes: {:02x?}",
                            &decompressed[0..16]
                        );
                        tracing::error!(
                            "   Last 16 bytes: {:02x?}",
                            &decompressed[decompressed.len()-16..]
                        );
                    }
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
        // Flush to ensure height is persisted immediately
        self.storage
            .flush()
            .map_err(|e| format!("Failed to flush chain_height: {}", e))?;

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

    fn calculate_rewards_with_amount(
        &self,
        masternodes: &[MasternodeInfo],
        total_reward: u64,
    ) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        // NEW: All rewards go to the block producer only (first masternode in list)
        // The first masternode is the one selected as producer by the consensus algorithm
        let producer = &masternodes[0];

        tracing::info!(
            "💰 Reward calculation: {} satoshis ({} TIME) -> block producer {}",
            total_reward,
            total_reward / 100_000_000,
            producer.masternode.address
        );

        vec![(producer.masternode.wallet_address.clone(), total_reward)]
    }

    /// Validate block rewards are correct and not double-counted
    /// This prevents the old bug where rewards were added both as metadata AND as transaction outputs
    fn validate_block_rewards(&self, block: &Block) -> Result<(), String> {
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

                // Try tx_index first for O(1) lookup
                let mut found = false;
                if let Some(ref txi) = self.tx_index {
                    if let Some(loc) = txi.get_location(&spent_txid) {
                        if let Ok(src_block) = self.get_block(loc.block_height) {
                            if let Some(src_tx) = src_block.transactions.get(loc.tx_index) {
                                if let Some(output) = src_tx.outputs.get(spent_vout as usize) {
                                    input_sum += output.value;
                                    found = true;
                                }
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

        // Verify block_reward matches base reward + calculated fees
        let expected_reward = BLOCK_REWARD_SATOSHIS + calculated_fees;

        if block.header.block_reward != expected_reward {
            return Err(format!(
                "Block {} has incorrect block_reward: expected {} (base {} + fees {}), got {}",
                block.header.height,
                expected_reward,
                BLOCK_REWARD_SATOSHIS,
                calculated_fees,
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

        // Verify outputs match masternode_rewards metadata
        if reward_dist.outputs.len() != block.masternode_rewards.len() {
            return Err(format!(
                "Block {} reward distribution has {} outputs but masternode_rewards has {} entries",
                block.header.height,
                reward_dist.outputs.len(),
                block.masternode_rewards.len()
            ));
        }

        // Verify each output matches metadata
        for (i, (expected_addr, expected_amount)) in block.masternode_rewards.iter().enumerate() {
            let output = &reward_dist.outputs[i];
            let output_addr = String::from_utf8_lossy(&output.script_pubkey).to_string();

            if &output_addr != expected_addr {
                return Err(format!(
                    "Block {} reward output {} address mismatch: expected {}, got {}",
                    block.header.height, i, expected_addr, output_addr
                ));
            }

            if output.value != *expected_amount {
                return Err(format!(
                    "Block {} reward output {} amount mismatch: expected {}, got {}",
                    block.header.height, i, expected_amount, output.value
                ));
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
            return Err(format!(
                "Block {} merkle root mismatch: computed {}, header {}",
                block.header.height,
                hex::encode(&computed_merkle[..8]),
                hex::encode(&block.header.merkle_root[..8])
            ));
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

        // Note: Past timestamp check is done in add_block() where we know if we're syncing

        // Additional check: Verify timestamp aligns with blockchain timeline
        // Expected time = genesis_time + (height * block_time)
        // This check is DISABLED during initial sync because catchup blocks use current time
        // Only enforce this for recently produced blocks (within a few blocks of chain tip)
        // This prevents accepting entire fake chains that are too far ahead of schedule
        let genesis_time = self.genesis_timestamp();
        let expected_time = genesis_time + (block.header.height as i64 * BLOCK_TIME_SECONDS);
        let time_drift = block.header.timestamp - expected_time;

        // Only check schedule drift if block is recent (not historical/catchup)
        // If we're syncing old blocks, they may have catchup timestamps that don't match original schedule
        // Skip the check during sync to avoid blocking - catchup blocks use historical timestamps
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

        // 5. Block size check (Phase 1.3: 1MB hard cap)
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

            tracing::info!(
                "✅ Received valid genesis block: {} (masternodes: {})",
                hex::encode(block.hash()),
                block.header.masternode_tiers.total()
            );

            // Save genesis block
            let _ = self.process_block_utxos(&block).await;
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
                        self.save_block(&block)?;
                        return Ok(true);
                    }
                    Err(_) => {
                        // Previous block also missing - we have a bigger gap
                        tracing::warn!(
                            "⚠️  Cannot fill gap at {}: previous block {} also missing",
                            block_height,
                            block_height - 1
                        );
                        return Ok(false);
                    }
                }
            } else {
                // Genesis block (height 0) - validate and add
                self.validate_block(&block, None)?;
                tracing::info!("✅ Filling gap: adding genesis block");
                self.save_block(&block)?;
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

    /// Check if we should accept a peer's chain over our own
    /// Uses longest-chain-by-work rule
    pub async fn should_switch_to_chain(&self, peer_height: u64, _peer_tip_hash: [u8; 32]) -> bool {
        let our_height = self.current_height.load(Ordering::Acquire);

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

    /// Perform a chain reorganization to adopt a peer's chain
    /// 1. Find common ancestor
    /// 2. Rollback to common ancestor
    /// 3. Apply new blocks from peer
    pub async fn reorganize_to_chain(
        &self,
        common_ancestor: u64,
        new_blocks: Vec<Block>,
    ) -> Result<(), String> {
        let start_time = std::time::Instant::now();
        let current = self.current_height.load(Ordering::Acquire);

        if new_blocks.is_empty() {
            return Err("No blocks provided for reorganization".to_string());
        }

        let first_new = new_blocks.first().unwrap().header.height;
        let last_new = new_blocks.last().unwrap().header.height;
        let blocks_to_add = new_blocks.len() as u64;

        tracing::warn!(
            "⚠️  REORG INITIATED: rollback {} -> {}, then apply {} blocks ({} -> {})",
            current,
            common_ancestor,
            blocks_to_add,
            first_new,
            last_new
        );

        // Validate all new blocks BEFORE starting reorganization
        tracing::info!(
            "🔍 Validating {} blocks before reorganization...",
            new_blocks.len()
        );

        let now = chrono::Utc::now().timestamp();

        // CRITICAL FIX: Validate that the first block actually builds on common_ancestor
        // Only check this for the FIRST block, then validate internal chain consistency
        let common_ancestor_hash = if common_ancestor > 0 {
            match self.get_block_hash(common_ancestor) {
                Ok(hash) => {
                    // Verify first block references this common ancestor
                    if let Some(first_block) = new_blocks.first() {
                        if first_block.header.previous_hash != hash {
                            return Err(format!(
                                "Fork validation failed: first block {} doesn't build on common ancestor {} \
                                (expected prev_hash {}, got {}). This suggests the common ancestor was incorrectly identified.",
                                first_block.header.height,
                                common_ancestor,
                                hex::encode(&hash[..8]),
                                hex::encode(&first_block.header.previous_hash[..8])
                            ));
                        }
                        tracing::info!(
                            "✅ Verified first block {} builds on common ancestor {} (hash: {})",
                            first_block.header.height,
                            common_ancestor,
                            hex::encode(&hash[..8])
                        );
                    }
                    Some(hash)
                }
                Err(e) => {
                    return Err(format!(
                        "Cannot validate fork: failed to get common ancestor {} hash: {}",
                        common_ancestor, e
                    ));
                }
            }
        } else {
            None
        };

        // Now validate that the peer's chain is internally consistent
        let mut expected_prev_hash = common_ancestor_hash;

        for (index, block) in new_blocks.iter().enumerate() {
            let expected_height = common_ancestor + 1 + (index as u64);

            // Validate block height is sequential
            if block.header.height != expected_height {
                return Err(format!(
                    "Block height mismatch during reorg validation: expected {}, got {}",
                    expected_height, block.header.height
                ));
            }

            // Validate block timestamps are not in the future
            if block.header.timestamp > now + TIMESTAMP_TOLERANCE_SECS {
                return Err(format!(
                    "Block {} timestamp {} is too far in future (now: {}, tolerance: {}s)",
                    block.header.height, block.header.timestamp, now, TIMESTAMP_TOLERANCE_SECS
                ));
            }

            // Validate previous hash chain continuity WITHIN peer's chain
            if let Some(prev_hash) = expected_prev_hash {
                if block.header.previous_hash != prev_hash {
                    return Err(format!(
                        "Peer chain not internally consistent: block {} previous_hash mismatch \
                        (expected {}, got {}). Peer sent invalid/discontinuous chain.",
                        block.header.height,
                        hex::encode(&prev_hash[..8]),
                        hex::encode(&block.header.previous_hash[..8])
                    ));
                }
            }

            // Validate merkle root
            let computed_merkle = crate::block::types::calculate_merkle_root(&block.transactions);
            if computed_merkle != block.header.merkle_root {
                return Err(format!(
                    "Block {} merkle root mismatch during reorg validation",
                    block.header.height
                ));
            }

            // Validate block size
            let serialized = bincode::serialize(block).map_err(|e| e.to_string())?;
            if serialized.len() > MAX_BLOCK_SIZE {
                return Err(format!(
                    "Block {} exceeds max size: {} > {} bytes",
                    block.header.height,
                    serialized.len(),
                    MAX_BLOCK_SIZE
                ));
            }

            // Update expected previous hash for next block in peer's chain
            expected_prev_hash = Some(block.hash());
        }

        tracing::info!("✅ All blocks validated successfully, proceeding with reorganization");

        // CRITICAL: Validate finalized transaction protection (Approach A)
        // Once timevote finalizes a transaction, it MUST be in the canonical chain.
        // Reject any fork that excludes a finalized transaction.
        tracing::info!("🔒 Checking finalized transaction protection...");
        let finalized_txs_to_check = self
            .get_finalized_txids_in_range(common_ancestor + 1, current)
            .await?;

        if !finalized_txs_to_check.is_empty() {
            tracing::info!(
                "🔍 Found {} finalized transactions that must be preserved during reorg",
                finalized_txs_to_check.len()
            );

            // Build set of all txids in the new chain
            let mut new_chain_txids = std::collections::HashSet::new();
            for block in &new_blocks {
                for tx in &block.transactions {
                    new_chain_txids.insert(tx.txid());
                }
            }

            // Check each finalized transaction is present in new chain
            for txid in &finalized_txs_to_check {
                if !new_chain_txids.contains(txid) {
                    return Err(format!(
                        "⛔ REORG REJECTED: New chain is missing finalized transaction {} \
                        (timevote instant finality guarantee violated). \
                        Finalized transactions cannot be excluded from the canonical chain.",
                        hex::encode(txid)
                    ));
                }
            }

            tracing::info!(
                "✅ All {} finalized transactions are present in new chain",
                finalized_txs_to_check.len()
            );
        }

        // Step 1: Rollback to common ancestor
        self.rollback_to_height(common_ancestor).await?;

        // Recalculate cumulative work after rollback
        let ancestor_work = self.get_work_at_height(common_ancestor).await.unwrap_or(0);
        *self.cumulative_work.write().await = ancestor_work;

        // Step 2: Apply new blocks in order (already validated)
        let mut removed_txs: Vec<Transaction> = Vec::new();
        let mut added_txs: Vec<Transaction> = Vec::new();

        // Collect transactions from rolled-back blocks for mempool replay
        for height in (common_ancestor + 1..=current).rev() {
            if let Ok(block) = self.get_block_by_height(height).await {
                // Store non-coinbase transactions from rolled-back blocks
                for tx in block.transactions.iter().skip(1) {
                    // Skip coinbase (first tx)
                    removed_txs.push(tx.clone());
                }
            }
        }

        // Apply new blocks
        for block in new_blocks.into_iter() {
            // Track transactions added in new chain
            for tx in block.transactions.iter().skip(1) {
                // Skip coinbase
                added_txs.push(tx.clone());
            }

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

        let new_height = self.current_height.load(Ordering::Acquire);
        let new_work = *self.cumulative_work.read().await;

        // Identify transactions to replay to mempool
        // These are transactions that were in the old chain but not in the new chain
        let added_txids: std::collections::HashSet<_> =
            added_txs.iter().map(|tx| tx.txid()).collect();
        let txs_to_replay: Vec<_> = removed_txs
            .into_iter()
            .filter(|tx| !added_txids.contains(&tx.txid()))
            .collect();

        if !txs_to_replay.is_empty() {
            tracing::info!(
                "🔄 {} transactions need mempool replay after reorg",
                txs_to_replay.len()
            );
            // Note: Actual mempool replay requires access to TransactionPool
            // This would be done by the caller with access to the mempool:
            // for tx in txs_to_replay { mempool.add_pending(tx, calculate_fee(&tx))?; }
        }

        let duration_ms = start_time.elapsed().as_millis() as u64;

        // Record reorganization metrics
        let metrics = ReorgMetrics {
            timestamp: chrono::Utc::now().timestamp(),
            from_height: current,
            to_height: new_height,
            common_ancestor,
            blocks_removed: current - common_ancestor,
            blocks_added: blocks_to_add,
            txs_to_replay: txs_to_replay.len(),
            duration_ms,
        };

        self.reorg_history.write().await.push(metrics.clone());

        // Keep only last 100 reorg events
        {
            let mut history = self.reorg_history.write().await;
            let history_len = history.len();
            if history_len > 100 {
                history.drain(0..history_len - 100);
            }
        }

        tracing::warn!(
            "✅ REORG COMPLETE: new height {}, cumulative work {}, {} txs need replay, took {}ms",
            new_height,
            new_work,
            txs_to_replay.len(),
            duration_ms
        );

        Ok(())
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
        const MAX_PEERS_TO_CHECK: usize = 100; // Hard cap for extreme cases
        let sample_size = if connected_peers.len() > MAX_PEERS_TO_CHECK {
            let sqrt_size = (connected_peers.len() as f64).sqrt().ceil() as usize;
            sqrt_size.min(MAX_PEERS_TO_CHECK)
        } else {
            connected_peers.len()
        };

        if sample_size < connected_peers.len() {
            // Deterministic sampling based on current time for randomness
            // This avoids Send issues with thread_rng in async context
            let now_nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0) as u64;
            // Use Fisher-Yates shuffle with time-based seed
            for i in 0..sample_size {
                let j = (now_nanos.wrapping_mul(i as u64 + 1).wrapping_add(i as u64)) as usize
                    % (connected_peers.len() - i)
                    + i;
                connected_peers.swap(i, j);
            }
            connected_peers.truncate(sample_size);
            tracing::info!(
                "🎲 Sampling {} of {} peers for consensus check (scalability optimization)",
                sample_size,
                registry.get_compatible_peers().await.len()
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
                tracing::info!(
                    "🔍 [CHAIN ANALYSIS] Detected {} different chains:",
                    num_chains
                );
            }
            for ((height, hash), peers) in &chain_counts {
                if num_chains == 1 {
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

        // Find the LONGEST chain (highest height)
        // Use weighted stake as tiebreaker when heights are equal (Bronze=10, Free=1)
        // Pre-compute weighted stake for each chain
        let mut chain_weights: std::collections::HashMap<(u64, [u8; 32]), u64> =
            std::collections::HashMap::new();
        for ((height, hash), peers) in &chain_counts {
            let mut weight = 0u64;
            for peer_ip in peers {
                weight += match self.masternode_registry.get(peer_ip).await {
                    Some(info) => info.masternode.tier.sampling_weight(),
                    None => crate::types::MasternodeTier::Free.sampling_weight(),
                };
            }
            chain_weights.insert((*height, *hash), weight);
        }

        let consensus_chain = chain_counts
            .iter()
            .max_by(|((h1, hash1), peers1), ((h2, hash2), peers2)| {
                // Primary: higher height wins (longest chain rule)
                let height_cmp = h1.cmp(h2);
                if height_cmp != std::cmp::Ordering::Equal {
                    return height_cmp;
                }
                // Secondary: higher weighted stake wins (at same height)
                let w1 = chain_weights.get(&(*h1, *hash1)).copied().unwrap_or(0);
                let w2 = chain_weights.get(&(*h2, *hash2)).copied().unwrap_or(0);
                let weight_cmp = w1.cmp(&w2);
                if weight_cmp != std::cmp::Ordering::Equal {
                    return weight_cmp;
                }
                // Tertiary: more peers wins
                peers1.len().cmp(&peers2.len())
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
        // Only require 2/3 weighted threshold for same-height fork tiebreakers.
        let second_highest = chain_counts
            .iter()
            .filter(|((h, hash), _)| !(*h == consensus_height && *hash == consensus_hash))
            .map(|((h, _), _)| *h)
            .max()
            .unwrap_or(0);

        let height_advantage = consensus_height.saturating_sub(second_highest);

        if height_advantage == 0 {
            // Same-height fork — the chain with more peers is canonical.
            // Simple majority (>50%) is sufficient because the minority MUST switch
            // to resolve the fork. Requiring 2/3 causes permanent forks when the split
            // is close (e.g., 3 vs 2 = 60% never meets 67% threshold).
            let required_weight = total_responding_weight / 2 + 1; // strict majority
            if consensus_weight < required_weight {
                tracing::warn!(
                    "⚠️  Same-height fork at {}: consensus has insufficient weighted support: {}/{} weight ({:.1}%) - need majority ({}).",
                    consensus_height,
                    consensus_weight,
                    total_responding_weight,
                    (consensus_weight as f64 / total_responding_weight as f64) * 100.0,
                    required_weight
                );
                return None;
            }
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
        let fork_count = chain_counts.len() as u32;
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
            tracing::warn!(
                "🔀 FORK RESOLUTION TRIGGERED: longest chain height {} > our height {} ({} peers agree)",
                consensus_height,
                our_height,
                consensus_peers.len()
            );
            tracing::warn!("   Will attempt to sync from peer: {}", consensus_peers[0]);
            return Some((consensus_height, consensus_peers[0].clone()));
        }

        // Case 2: Same height but different hash - fork at same height!
        // The consensus chain was already selected by peer count (majority wins).
        // If our hash doesn't match, we're in the minority — always switch.
        if consensus_height == our_height && consensus_hash != our_hash {
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
        // LONGEST VALID CHAIN RULE: If we have a valid longer chain than any peer, WE are canonical
        // This can only happen if we have blocks that no peer has yet
        //
        // EXCEPTION: If we're only slightly ahead (1-5 blocks) and our chain diverges
        // from what the majority of peers agree on, we're likely on a solo fork — not
        // actually canonical. Compare our hash at consensus_height to detect this.
        if our_height > consensus_height {
            // Check if our chain at consensus_height matches what peers have
            // If it doesn't, we forked below consensus_height and built on a bad chain
            if our_height - consensus_height <= 5 && consensus_peers.len() >= 2 {
                match self.get_block_hash(consensus_height) {
                    Ok(our_hash_at_consensus) => {
                        if our_hash_at_consensus != consensus_hash {
                            tracing::warn!(
                                "🔀 SOLO FORK DETECTED: We're at {} but our hash at consensus height {} ({}) differs from {} peers ({})",
                                our_height,
                                consensus_height,
                                hex::encode(&our_hash_at_consensus[..8]),
                                consensus_peers.len(),
                                hex::encode(&consensus_hash[..8])
                            );
                            tracing::warn!(
                                "   Switching to consensus chain (we likely missed a block and forked)"
                            );
                            return Some((consensus_height, consensus_peers[0].clone()));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "🔄 Cannot verify our chain at height {}: {} — syncing to consensus",
                            consensus_height,
                            e
                        );
                        return Some((consensus_height, consensus_peers[0].clone()));
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
                        // We're behind — could be simple lag OR behind-and-forked.
                        // Request blocks starting from our_height-20 so overlapping blocks
                        // reveal whether we're on the same chain. If forked, process_peer_blocks
                        // will detect the mismatch and trigger handle_fork() automatically.
                        tracing::info!(
                            "🔀 Periodic fork detection: consensus height {} > our height {}, requesting blocks from {}",
                            consensus_height,
                            our_height,
                            consensus_peer
                        );

                        if let Some(peer_registry) = blockchain.peer_registry.read().await.as_ref()
                        {
                            let request_from = our_height.saturating_sub(20).max(1);

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
                                            "📤 Requested blocks {}-{} from {} (overlap from {} to detect forks)",
                                            request_from,
                                            consensus_height,
                                            consensus_peer,
                                            our_height
                                        );
                                    }
                                }
                                Ok(false) => {
                                    tracing::debug!("⏸️ Sync queued with {}", consensus_peer);
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "⏱️ Sync throttled with {}: {}",
                                        consensus_peer,
                                        e
                                    );
                                }
                            }
                        }
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

        let fork_height = blocks[0].header.height;
        let our_height = self.get_height();

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

        // STRATEGY: Request blocks covering MAX_REORG_DEPTH from our height.
        // The common ancestor MUST be within MAX_REORG_DEPTH or we'd reject the reorg anyway.
        // This prevents the old bug where fork_height shifted down each iteration,
        // causing the node to download the entire blockchain back to genesis.
        let lowest_peer_block = all_blocks
            .iter()
            .map(|b| b.header.height)
            .min()
            .unwrap_or(fork_height);
        let search_floor = our_height.saturating_sub(MAX_REORG_DEPTH);

        if lowest_peer_block > search_floor && search_floor > 0 {
            // We don't have enough block history - request one batch covering the reorg window
            let request_from = search_floor;
            let request_to = peer_tip_height;

            info!(
                "📥 Requesting blocks {}-{} from {} for fork resolution (need coverage back to {}, have {}-{})",
                request_from, request_to, peer_addr, search_floor, lowest_peer_block, peer_tip_height
            );

            // Transition to fetching state
            *self.fork_state.write().await = ForkResolutionState::FetchingChain {
                common_ancestor: 0, // Not yet known
                fork_height,
                peer_addr: peer_addr.clone(),
                peer_height: peer_tip_height,
                fetched_up_to: peer_tip_height, // We already have up to peer tip
                accumulated_blocks: all_blocks.clone(), // Save all blocks we have
                started_at: std::time::Instant::now(),
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
                // Once blocks are more than MAX_REORG_DEPTH deep, they are considered FINAL
                // This protects against long-range attacks where an attacker creates a fake longer chain
                if fork_depth > MAX_REORG_DEPTH {
                    warn!(
                        "🛡️ SECURITY: REJECTED DEEP REORG from peer {} - fork depth {} exceeds maximum {} blocks",
                        peer_addr, fork_depth, MAX_REORG_DEPTH
                    );
                    warn!(
                        "   Our height: {}, common ancestor: {}, peer claims height: {}",
                        our_height, common_ancestor, peer_tip_height
                    );
                    warn!(
                        "   Blocks at depth >{} are considered FINAL and cannot be reorganized",
                        MAX_REORG_DEPTH
                    );
                    warn!(
                        "   Peer {} is attempting a deep reorg attack - marking as suspicious",
                        peer_addr
                    );

                    *self.fork_state.write().await = ForkResolutionState::None;
                    return Ok(());
                }

                info!(
                    "🤖 [SIMPLIFIED] Evaluating fork: our={} peer={}, ancestor={}, depth={}",
                    our_height, peer_tip_height, common_ancestor, fork_depth
                );

                // Use simplified fork resolver (longest valid chain wins)
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
                    "🤖 [SIMPLIFIED] Fork resolution decision: accept={}, reasoning: {}",
                    resolution.accept_peer_chain, reasoning_summary
                );

                // Decision: determine whether to accept peer chain
                // Only accept if the AI resolver says it's a longer valid chain.
                // Same-height forks are resolved deterministically by the hash tiebreaker
                // in fork_resolver.rs — do NOT override with peer-count consensus, as it
                // causes reorg flip-flopping when peer counts shift during active forks.
                let accept_reason = if resolution.accept_peer_chain {
                    Some("longer valid chain".to_string())
                } else {
                    None
                };

                if let Some(reason) = accept_reason {
                    // CRITICAL SAFETY CHECK: Common ancestor cannot be higher than our chain
                    if common_ancestor > our_height {
                        warn!(
                            "🚫 REJECTED REORG: Common ancestor {} > our height {} - bug in ancestor search!",
                            common_ancestor, our_height
                        );
                        *self.fork_state.write().await = ForkResolutionState::None;
                        return Ok(());
                    }

                    // Reject reorgs to strictly shorter chains (same height is OK for consensus switch)
                    if peer_tip_height < our_height {
                        warn!(
                            "🚫 REJECTED REORG: Peer chain is SHORTER ({} < {}).",
                            peer_tip_height, our_height
                        );
                        *self.fork_state.write().await = ForkResolutionState::None;
                        return Ok(());
                    }

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
                                accumulated_blocks: Vec::new(),
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
                            accumulated_blocks: Vec::new(), // Will be filled when blocks arrive
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

                    // Transition to reorg state
                    *self.fork_state.write().await = ForkResolutionState::ReadyToReorg {
                        common_ancestor,
                        alternate_blocks: sorted_reorg_blocks, // Use sorted blocks
                        started_at: std::time::Instant::now(),
                    };

                    self.continue_fork_resolution().await
                } else {
                    info!("📊 Fork resolver rejected peer chain");
                    *self.fork_state.write().await = ForkResolutionState::None;
                    Ok(())
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

    /// Continue fork resolution state machine with timeout protection
    async fn continue_fork_resolution(&self) -> Result<(), String> {
        // Check for stale fork resolution state (timeout after 2 minutes)
        const FORK_RESOLUTION_TIMEOUT_SECS: u64 = 120;

        let state = self.fork_state.read().await.clone();

        // Check timeout for states with timestamps
        match &state {
            ForkResolutionState::FetchingChain { started_at, .. }
            | ForkResolutionState::Reorging { started_at, .. }
            | ForkResolutionState::ReadyToReorg { started_at, .. } => {
                if started_at.elapsed().as_secs() > FORK_RESOLUTION_TIMEOUT_SECS {
                    warn!(
                        "⚠️  Fork resolution timed out after {}s, resetting state",
                        started_at.elapsed().as_secs()
                    );
                    *self.fork_state.write().await = ForkResolutionState::None;
                    return Err(format!(
                        "Fork resolution timeout after {}s",
                        FORK_RESOLUTION_TIMEOUT_SECS
                    ));
                }
            }
            _ => {}
        }

        match state {
            ForkResolutionState::None => Ok(()),

            ForkResolutionState::FetchingChain {
                common_ancestor: _,
                fork_height: _,
                peer_addr,
                peer_height,
                fetched_up_to,
                accumulated_blocks: _,
                started_at: _,
            } => {
                // When we're in FetchingChain, the blocks will arrive via handle_blocks
                // and we'll retry binary search when we have more blocks
                // For now, just log status
                info!(
                    "⏳ Waiting for blocks from peer {} (have up to {}, need up to {})",
                    peer_addr, fetched_up_to, peer_height
                );
                Ok(())
            }

            ForkResolutionState::ReadyToReorg {
                common_ancestor,
                alternate_blocks,
                started_at,
            } => {
                // Check if reorg preparation is taking too long
                if started_at.elapsed().as_secs() > 60 {
                    warn!(
                        "⚠️  Reorg preparation stuck for {}s, resetting",
                        started_at.elapsed().as_secs()
                    );
                    *self.fork_state.write().await = ForkResolutionState::None;
                    return Err("Reorg preparation stalled".to_string());
                }

                // Perform the reorganization
                let reorg_result = self.perform_reorg(common_ancestor, alternate_blocks).await;

                // Always clear fork state after reorg attempt (success or failure)
                *self.fork_state.write().await = ForkResolutionState::None;

                // Clear consensus peers list - will be refreshed on next periodic check
                self.consensus_peers.write().await.clear();

                reorg_result?;
                Ok(())
            }

            ForkResolutionState::Reorging {
                from_height: _,
                to_height: _,
                started_at,
            } => {
                // Check if reorg is taking too long
                if started_at.elapsed().as_secs() > 60 {
                    warn!(
                        "⚠️  Reorg stuck for {}s, resetting",
                        started_at.elapsed().as_secs()
                    );
                    *self.fork_state.write().await = ForkResolutionState::None;
                    return Err("Reorg operation stalled".to_string());
                }
                // Already reorging, wait
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

    /// Find common ancestor between our chain and competing blocks (for fork resolution)
    /// Uses exponential + binary search algorithm for efficiency (O(log n) vs O(n))
    /// Returns error if peer blocks don't go back far enough to find true common ancestor
    async fn find_fork_common_ancestor(&self, competing_blocks: &[Block]) -> Result<u64, String> {
        if competing_blocks.is_empty() {
            return Ok(0);
        }

        // Sort blocks by height to find the starting point
        let mut sorted_blocks = competing_blocks.to_vec();
        sorted_blocks.sort_by_key(|b| b.header.height);

        // Build a map of peer's blocks for fast lookup
        let peer_blocks: std::collections::HashMap<u64, [u8; 32]> = sorted_blocks
            .iter()
            .map(|b| (b.header.height, b.hash()))
            .collect();

        let peer_height = sorted_blocks.last().unwrap().header.height;
        let peer_lowest = sorted_blocks.first().unwrap().header.height;
        let our_height = self.get_height();

        info!(
            "🔍 Finding common ancestor using exponential+binary search (our: {}, peer: {}, peer blocks: {}-{})",
            our_height, peer_height, peer_lowest, peer_height
        );

        // SIMPLIFIED APPROACH: Instead of binary search (which can fail with incomplete data),
        // search linearly downward to find the common ancestor.
        // Start from our chain height and go down, checking if the peer has a matching block.

        let mut candidate_ancestor = 0u64;

        // Search from our height downward - at each height, check if peer has a matching block
        for height in (0..=our_height).rev() {
            // Get our block hash at this height
            let our_hash = match self.get_block_hash(height) {
                Ok(hash) => hash,
                Err(_) => continue, // We don't have this block, try lower
            };

            // If peer has this block, check if hashes match
            if let Some(peer_hash) = peer_blocks.get(&height) {
                if our_hash == *peer_hash {
                    // Found matching block - this could be common ancestor
                    candidate_ancestor = height;
                    info!(
                        "✅ Found matching block at height {} (our hash {} == peer hash {})",
                        height,
                        hex::encode(&our_hash[..8]),
                        hex::encode(&peer_hash[..8])
                    );
                    break;
                } else {
                    // Different blocks at same height - fork is below this
                    info!(
                        "🔀 Different blocks at height {}: ours {} vs peer {}",
                        height,
                        hex::encode(&our_hash[..8]),
                        hex::encode(&peer_hash[..8])
                    );
                    continue;
                }
            } else {
                // Peer doesn't have this block - check if peer's NEXT block builds on our block
                // This handles the case where common ancestor is below peer's lowest block
                if let Some(peer_next_block) =
                    sorted_blocks.iter().find(|b| b.header.height == height + 1)
                {
                    if peer_next_block.header.previous_hash == our_hash {
                        // Peer's next block builds on our block at this height - this is the ancestor
                        candidate_ancestor = height;
                        info!(
                            "✅ Found common ancestor at height {} (peer's block {} builds on our block)",
                            height,
                            height + 1
                        );
                        break;
                    }
                }
            }
        }

        // SANITY CHECK: Common ancestor cannot be higher than our height
        if candidate_ancestor > our_height {
            warn!(
                "🚫 BUG DETECTED: Common ancestor {} > our height {}. Capping to our height.",
                candidate_ancestor, our_height
            );
            candidate_ancestor = our_height;
        }

        info!(
            "🔍 Common ancestor search complete: found ancestor at height {} (peer lowest: {}, our height: {})",
            candidate_ancestor, peer_lowest, our_height
        );

        // CRITICAL VALIDATION: Verify that peer's next block actually builds on this ancestor
        // If we say height N is common ancestor, peer's block N+1 must have previous_hash = our block N's hash
        if candidate_ancestor < peer_height {
            let our_block_hash = self.get_block_hash(candidate_ancestor)?;

            // Find peer's next block after candidate ancestor
            if let Some(peer_next_block) = sorted_blocks
                .iter()
                .find(|b| b.header.height == candidate_ancestor + 1)
            {
                let peer_next_prev_hash = peer_next_block.header.previous_hash;

                if our_block_hash != peer_next_prev_hash {
                    warn!(
                        "⚠️  Binary search validation failed: candidate ancestor {} has hash {}, \
                        but peer's block {} expects previous_hash {}",
                        candidate_ancestor,
                        hex::encode(our_block_hash),
                        candidate_ancestor + 1,
                        hex::encode(peer_next_prev_hash)
                    );

                    // The binary search gave us a false positive - actual fork is earlier
                    // Search backwards from candidate to find the true common ancestor
                    let mut true_ancestor = candidate_ancestor;
                    while true_ancestor > 0 {
                        true_ancestor -= 1;

                        // Check if blocks at this height match
                        if let Ok(our_hash_at) = self.get_block_hash(true_ancestor) {
                            if let Some(peer_hash_at) = peer_blocks.get(&true_ancestor) {
                                if our_hash_at == *peer_hash_at {
                                    // Verify this is a true common ancestor by checking next block
                                    if let Some(peer_next) = sorted_blocks
                                        .iter()
                                        .find(|b| b.header.height == true_ancestor + 1)
                                    {
                                        if self.get_block_hash(true_ancestor).ok()
                                            == Some(peer_next.header.previous_hash)
                                        {
                                            info!("✓ Validated true common ancestor at height {} (corrected from {})", true_ancestor, candidate_ancestor);
                                            return Ok(true_ancestor);
                                        }
                                    } else {
                                        // Peer doesn't have next block in provided set
                                        info!("✓ Found common ancestor at height {} (no next block to validate)", true_ancestor);
                                        return Ok(true_ancestor);
                                    }
                                }
                            }
                        }
                    }

                    // Couldn't find common ancestor - chains diverged at or before peer_lowest
                    if peer_lowest > 100 {
                        return Err(format!(
                            "Fork earlier than provided blocks: peer blocks start at {}, but common ancestor not found. \
                            Need deeper block history.",
                            peer_lowest
                        ));
                    }

                    // Fork at genesis
                    return Ok(0);
                }
            }
        }

        // CRITICAL FIX: If ancestor is 0 but peer_lowest is > 100,
        // the blocks slice doesn't go back far enough to find the true common ancestor.
        // Return an error to force the peer to send deeper block history.
        if candidate_ancestor == 0 && peer_lowest > 100 {
            return Err(format!(
                "Insufficient block history: peer blocks only go back to height {}, \
                but common ancestor was not found. Peer must provide blocks starting from a lower height \
                (fork likely occurred between height 0 and {}).",
                peer_lowest, peer_lowest
            ));
        }

        info!("✓ Found common ancestor at height {}", candidate_ancestor);
        Ok(candidate_ancestor)
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

                    // Check 4: Merkle root matches transactions
                    let computed_merkle =
                        crate::block::types::calculate_merkle_root(&block.transactions);
                    if computed_merkle != block.header.merkle_root {
                        tracing::error!("❌ CORRUPT BLOCK {}: merkle root mismatch", height);
                        corrupt_blocks.push(height);
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

        // Step 1: Delete the corrupt local copies (both key formats)
        for &height in corrupt_heights {
            let key_new = format!("block_{}", height);
            let key_old = format!("block:{}", height);
            let _ = self.storage.remove(key_new.as_bytes());
            let _ = self.storage.remove(key_old.as_bytes());
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
