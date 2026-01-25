//! Blockchain storage and management

#![allow(dead_code)]

use crate::ai::consensus_health::{
    ConsensusHealthConfig, ConsensusHealthMonitor, ConsensusMetrics,
};
use crate::block::types::{Block, BlockHeader};
use crate::block_cache::BlockCacheManager;
use crate::blockchain_validation::BlockValidator;
use crate::consensus::ConsensusEngine;
use crate::constants;
use crate::masternode_registry::{MasternodeInfo, MasternodeRegistry};
use crate::network::fork_resolver::ForkResolver as NetworkForkResolver;
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::types::{OutPoint, Transaction, TxInput, TxOutput, UTXO};
use crate::utxo_manager::UTXOStateManager;
use crate::NetworkType;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const BLOCK_TIME_SECONDS: i64 = constants::blockchain::BLOCK_TIME_SECONDS;
const SATOSHIS_PER_TIME: u64 = constants::blockchain::SATOSHIS_PER_TIME;
const BLOCK_REWARD_SATOSHIS: u64 = constants::blockchain::BLOCK_REWARD_SATOSHIS;

// Security limits (Phase 1)
const MAX_BLOCK_SIZE: usize = constants::blockchain::MAX_BLOCK_SIZE;
const TIMESTAMP_TOLERANCE_SECS: i64 = constants::blockchain::TIMESTAMP_TOLERANCE_SECS;
const MAX_REORG_DEPTH: u64 = constants::blockchain::MAX_REORG_DEPTH;
const ALERT_REORG_DEPTH: u64 = 100; // Alert on reorgs deeper than this

// P2P sync configuration (Phase 3 Step 4: Extended timeouts for masternodes)
const PEER_SYNC_TIMEOUT_SECS: u64 = 60; // Short timeout for responsive sync (1 min)
const PEER_SYNC_CHECK_INTERVAL_SECS: u64 = 2;
const MASTERNODE_SYNC_TIMEOUT_SECS: u64 = 600; // 10 minutes for masternode sync
const SYNC_COORDINATOR_INTERVAL_SECS: u64 = 30; // Check sync every 30 seconds (reduced from 60s for faster fork detection)

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
enum ForkResolutionState {
    /// No fork detected
    None,

    /// Fork detected, need to find common ancestor
    FindingAncestor {
        fork_height: u64,
        peer_addr: String,
        check_height: u64,
        searched_back: u64,
        started_at: std::time::Instant, // NEW: Track when state started
    },

    /// Common ancestor found, need to get peer's chain
    FetchingChain {
        common_ancestor: u64,
        fork_height: u64,
        peer_addr: String,
        peer_height: u64,
        fetched_up_to: u64,
        started_at: std::time::Instant, // NEW: Track when state started
    },

    /// Have complete alternate chain, ready to reorg
    ReadyToReorg {
        common_ancestor: u64,
        alternate_blocks: Vec<Block>,
    },

    /// Performing reorganization
    Reorging {
        from_height: u64,
        to_height: u64,
        started_at: std::time::Instant, // NEW: Track when state started
    },
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
    /// AI-powered fork resolution
    fork_resolver: Arc<crate::ai::fork_resolver::ForkResolver>,
    /// Sync coordinator to prevent sync storms and duplicate requests
    sync_coordinator: Arc<crate::network::sync_coordinator::SyncCoordinator>,
    /// Cumulative chain work for longest-chain-by-work rule
    cumulative_work: Arc<RwLock<u128>>,
    /// Recent reorganization events (for monitoring and debugging)
    reorg_history: Arc<RwLock<Vec<ReorgMetrics>>>,
    /// Current fork resolution state
    fork_state: Arc<RwLock<ForkResolutionState>>,
    /// Fork resolution mutex to prevent concurrent fork resolutions (race condition protection)
    fork_resolution_lock: Arc<tokio::sync::Mutex<()>>,
    /// Two-tier block cache for efficient memory usage (10-50x faster reads)
    block_cache: Arc<BlockCacheManager>,
    /// Block validator for validation logic
    validator: BlockValidator,
    /// AI-powered consensus health monitoring
    consensus_health: Arc<ConsensusHealthMonitor>,
    /// Transaction index for O(1) transaction lookups
    pub tx_index: Option<Arc<crate::tx_index::TransactionIndex>>,
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

        // Initialize block validator
        let validator = BlockValidator::new(network_type);

        // Initialize AI consensus health monitor
        let consensus_health =
            Arc::new(ConsensusHealthMonitor::new(ConsensusHealthConfig::default()));

        Self {
            storage,
            consensus,
            masternode_registry,
            utxo_manager,
            current_height: Arc::new(AtomicU64::new(0)),
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
            block_cache,
            validator,
            consensus_health,
            tx_index: None, // Initialize without txindex, call build_tx_index() separately
        }
    }

    /// Set transaction index (called from main.rs after blockchain init)
    pub fn set_tx_index(&mut self, tx_index: Arc<crate::tx_index::TransactionIndex>) {
        self.tx_index = Some(tx_index);
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

    /// Get transaction index statistics
    pub fn get_tx_index_stats(&self) -> Option<(usize, u64)> {
        self.tx_index.as_ref().map(|idx| {
            let tx_count = idx.len();
            let height = self.get_height();
            (tx_count, height)
        })
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

    pub fn genesis_timestamp(&self) -> i64 {
        self.genesis_timestamp // Use cached value
    }

    /// Initialize blockchain - load local chain or sync from network
    pub async fn initialize_genesis(&self) -> Result<(), String> {
        use crate::block::genesis::GenesisBlock;

        // Helper function to load and store canonical genesis from file
        let load_and_store_genesis =
            |storage: &sled::Db, network_type: NetworkType| -> Result<Block, String> {
                tracing::info!("📥 Loading canonical genesis from file...");
                let genesis = GenesisBlock::load_from_file(network_type)?;

                // Verify it's valid before storing
                GenesisBlock::verify_structure(&genesis)?;

                // Store the genesis block
                let genesis_bytes = bincode::serialize(&genesis)
                    .map_err(|e| format!("Failed to serialize genesis: {}", e))?;
                storage
                    .insert("block_0".as_bytes(), genesis_bytes)
                    .map_err(|e| format!("Failed to store genesis block: {}", e))?;
                storage
                    .insert(genesis.hash().as_slice(), &0u64.to_be_bytes())
                    .map_err(|e| format!("Failed to index genesis block: {}", e))?;
                storage
                    .flush()
                    .map_err(|e| format!("Failed to flush genesis: {}", e))?;

                tracing::info!("✅ Genesis block loaded and stored from file");
                tracing::info!("   Hash: {}", hex::encode(&genesis.hash()[..8]));
                tracing::info!("   Timestamp: {}", genesis.header.timestamp);
                tracing::info!("   Transactions: {}", genesis.transactions.len());

                Ok(genesis)
            };

        // Check if genesis already exists locally
        let height = self.load_chain_height()?;
        if height > 0 {
            // Verify the genesis block structure
            if let Ok(genesis) = self.get_block_by_height(0).await {
                if let Err(e) = GenesisBlock::verify_structure(&genesis) {
                    tracing::error!(
                        "❌ Local genesis block is invalid: {} - replacing with canonical genesis",
                        e
                    );

                    // Remove the invalid genesis and all blocks built on it
                    self.clear_all_blocks();

                    // Load canonical genesis from file
                    load_and_store_genesis(&self.storage, self.network_type)?;
                    self.current_height.store(0, Ordering::Release);
                    return Ok(());
                }
            }
            self.current_height.store(height, Ordering::Release);
            tracing::info!("✓ Local blockchain verified (height: {})", height);

            // CRITICAL: Validate genesis hash matches expected canonical hash
            // This prevents nodes with incompatible chains from connecting
            if let Err(e) = self.validate_genesis_hash().await {
                tracing::error!("❌ CRITICAL: Genesis hash validation failed: {}", e);
                tracing::error!("   This node's blockchain is incompatible with the network");
                tracing::error!("   Manual intervention required: clear blockchain and resync");
                return Err(format!(
                    "Genesis hash mismatch - incompatible blockchain: {}",
                    e
                ));
            }

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
                        "❌ Local genesis is invalid: {} - replacing with canonical genesis",
                        e
                    );

                    // Remove the invalid genesis
                    let _ = self.storage.remove("block_0".as_bytes());
                    let _ = self.storage.remove(genesis.hash().as_slice());
                    let _ = self.storage.flush();

                    // Load canonical genesis from file
                    load_and_store_genesis(&self.storage, self.network_type)?;
                    self.current_height.store(0, Ordering::Release);
                    return Ok(());
                }
            }
            self.current_height.store(0, Ordering::Release);
            tracing::info!("✓ Genesis block verified");
            return Ok(());
        }

        // No local blockchain - load genesis from file
        load_and_store_genesis(&self.storage, self.network_type)?;
        self.current_height.store(0, Ordering::Release);

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

    /// Validate that our genesis block hash matches the expected canonical hash
    /// This prevents nodes with incompatible blockchains from joining the network
    pub async fn validate_genesis_hash(&self) -> Result<(), String> {
        use crate::block::genesis::GenesisBlock;

        // Get our local genesis block
        let local_genesis = self
            .get_block_by_height(0)
            .await
            .map_err(|e| format!("Cannot load genesis block: {}", e))?;

        // Load canonical genesis from file to get expected hash
        let canonical_genesis = GenesisBlock::load_from_file(self.network_type)
            .map_err(|e| format!("Cannot load canonical genesis: {}", e))?;

        let local_hash = local_genesis.hash();
        let canonical_hash = canonical_genesis.hash();

        if local_hash != canonical_hash {
            return Err(format!(
                "Genesis block mismatch!\n\
                 Local genesis hash:     {}\n\
                 Canonical genesis hash: {}\n\
                 This node has an incompatible blockchain database.\n\
                 Action required: Delete blockchain data directory and resync from network.",
                hex::encode(local_hash),
                hex::encode(canonical_hash)
            ));
        }

        tracing::info!(
            "✅ Genesis hash validated: {} (network: {:?})",
            hex::encode(&local_hash[..8]),
            self.network_type
        );

        Ok(())
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
        self.is_syncing.store(true, Ordering::Release);

        // Ensure we reset the sync flag when done
        let is_syncing = self.is_syncing.clone();
        let _guard = scopeguard::guard((), |_| {
            is_syncing.store(false, Ordering::Release);
        });

        let mut current = self.current_height.load(Ordering::Acquire);

        // Use provided target height (from consensus) or calculate from time
        let time_expected = self.calculate_expected_height();
        let target = target_height.unwrap_or(time_expected);

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

        if current >= target {
            tracing::info!("✓ Blockchain synced (height: {})", current);
            return Ok(());
        }

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
                    0 // Request genesis and subsequent blocks
                } else {
                    current + 1 // Request next block after our tip
                };
                let batch_end = (batch_start + 100).min(target);

                let req = NetworkMessage::GetBlocks(batch_start, batch_end);
                tracing::info!(
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

                        tracing::info!(
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
                    let mut tried_peers = vec![sync_peer.clone()];
                    for attempt in 2..=5 {
                        // Use AI to select next best peer (excluding already tried)
                        let remaining_peers: Vec<String> = connected_peers
                            .iter()
                            .filter(|p| !tried_peers.contains(p))
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
                                tried_peers.push(alt_peer.clone());
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

                            tried_peers.push(alt_peer);
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
        let time_expected = self.calculate_expected_height();

        if current >= time_expected {
            tracing::info!("✓ Already synced to expected height {}", current);
            return Ok(());
        }

        // Request blocks from the specific peer
        let batch_start = current + 1;
        let batch_end = time_expected;

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
                tracing::info!(
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

        let peer_registry = self.peer_registry.read().await;
        let registry = peer_registry.as_ref().ok_or("No peer registry available")?;

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

            if now_height >= time_expected {
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
                    tracing::warn!(
                        "✅ Found common ancestor at height {}, but peer {} may be on wrong chain",
                        common_ancestor,
                        peer_ip
                    );

                    tracing::warn!(
                        "⏸️  Not re-syncing from same peer - letting sync coordinator pick best chain"
                    );

                    // DON'T re-sync from the same peer that caused the fork!
                    // Instead, mark this sync as failed and let the sync coordinator
                    // request blocks from OTHER peers (who may have the correct chain)
                    self.sync_coordinator.complete_sync(peer_ip).await;

                    Err(format!(
                        "Fork resolved by rolling back to {}, but need to sync from different peer",
                        common_ancestor
                    ))
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
                "Partial sync from {}: reached {} but expected {}",
                peer_ip, final_height, time_expected
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

        // Search backward from current height to find matching block
        for height in (0..=search_start).rev() {
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
            tracing::warn!("⚠️ Could not find common ancestor via hash comparison, using genesis");
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
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                // ALWAYS check for consensus fork first - this is critical for fork resolution
                // This uses fresh chain tip data we just requested
                if let Some((consensus_height, sync_peer)) = self.compare_chain_with_peers().await {
                    // Fork detected by consensus mechanism
                    info!(
                        "🔀 Sync coordinator: Fork detected via consensus at height {}, syncing from {}",
                        consensus_height,
                        sync_peer
                    );
                    if !already_syncing {
                        let blockchain_clone = Arc::clone(&self);
                        tokio::spawn(async move {
                            // CRITICAL FIX: Pass consensus height to sync, not time-based height
                            if let Err(e) = blockchain_clone
                                .sync_from_peers(Some(consensus_height))
                                .await
                            {
                                warn!("⚠️  Consensus fork sync failed: {}", e);
                            }
                        });
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

    /// Produce a block for the current TSDC slot
    pub async fn produce_block(&self) -> Result<Block, String> {
        self.produce_block_at_height(None).await
    }

    pub async fn produce_block_at_height(
        &self,
        target_height: Option<u64>,
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

        // Get previous block hash
        let mut current_height = self.current_height.load(Ordering::Acquire);

        // Note: Previously had a safeguard preventing block production when >50 behind
        // This is no longer needed because:
        // 1. TSDC leader selection ensures only ONE node produces catchup blocks
        // 2. All nodes agree on the leader deterministically
        // 3. Non-leaders wait for leader's blocks
        // This prevents forks while allowing coordinated catchup when network is behind

        let expected_height = self.calculate_expected_height();
        let blocks_behind = expected_height.saturating_sub(current_height);

        if blocks_behind > 10 {
            tracing::debug!(
                "📦 Producing catchup block: {} blocks behind (TSDC leader coordinated)",
                blocks_behind
            );
        }

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
        let stored_height = self.current_height.load(Ordering::Acquire);
        if current_height != stored_height {
            tracing::warn!(
                "⚠️  Correcting chain height from {} to {}",
                stored_height,
                current_height
            );
            self.current_height.store(current_height, Ordering::Release);
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

        // Use blockchain-based masternode eligibility for rewards (consensus-safe)
        // This ensures all nodes agree on which masternodes get rewards, preventing forks
        // Masternode eligibility is determined by gossip consensus
        let masternodes = self
            .masternode_registry
            .get_masternodes_for_rewards(self)
            .await;

        tracing::info!(
            "💰 Block {}: distributing rewards to {} active masternodes",
            next_height,
            masternodes.len()
        );

        // Log each masternode receiving rewards to diagnose inconsistent counts
        for mn in &masternodes {
            tracing::info!(
                "   → Masternode {} (tier: {:?}, weight: {})",
                mn.masternode.address,
                mn.masternode.tier,
                mn.masternode.tier.reward_weight()
            );
        }

        if masternodes.is_empty() {
            return Err("No masternodes available for block production".to_string());
        }

        // Require at least 3 active masternodes before producing blocks
        if masternodes.len() < 3 {
            return Err(format!(
                "Insufficient masternodes for block production: {} active (minimum 3 required)",
                masternodes.len()
            ));
        }

        // Get finalized transactions from consensus layer
        let finalized_txs = self.consensus.get_finalized_transactions_for_block();
        tracing::info!(
            "🔍 Block {}: Including {} finalized transactions",
            next_height,
            finalized_txs.len()
        );

        // Calculate fees from current transactions (will be added to NEXT block)
        let current_block_fees = self.consensus.tx_pool.get_total_fees();

        // Get fees from PREVIOUS block (stored during last block production)
        let previous_block_fees = self.get_pending_fees();

        // Calculate rewards: base_reward + fees_from_previous_block
        let base_reward = BLOCK_REWARD_SATOSHIS;
        let total_reward = base_reward + previous_block_fees;
        let rewards = self.calculate_rewards_with_amount(&masternodes, total_reward);

        if rewards.is_empty() {
            return Err(format!(
                "No valid masternode rewards calculated for {} masternodes",
                masternodes.len()
            ));
        }

        tracing::info!(
            "💰 Block {}: base {} + fees {} = {} satoshis total to {} masternodes",
            next_height,
            base_reward,
            previous_block_fees,
            total_reward,
            rewards.len()
        );

        // Store current block fees for NEXT block
        self.store_pending_fees(current_block_fees)?;

        if current_block_fees > 0 {
            tracing::info!(
                "💸 Block {}: collected {} satoshis in fees (will be added to block {})",
                next_height,
                current_block_fees,
                next_height + 1
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
        let mut sorted_finalized = finalized_txs;
        sorted_finalized.sort_by_key(|a| a.txid());
        all_txs.extend(sorted_finalized);

        // Calculate merkle root from ALL transactions in canonical order
        let merkle_root = crate::block::types::calculate_merkle_root(&all_txs);

        let mut block = Block {
            header: BlockHeader {
                version: 1,
                height: next_height,
                previous_hash: prev_hash,
                merkle_root,
                timestamp: aligned_timestamp,
                block_reward: total_reward,
                leader: String::new(),
                attestation_root: [0u8; 32],
                masternode_tiers: tier_counts,
                ..Default::default()
            },
            transactions: all_txs,
            masternode_rewards: rewards.iter().map(|(a, v)| (a.clone(), *v)).collect(),
            time_attestations: vec![],
            // Record masternodes that are receiving rewards - they are the active participants
            consensus_participants: rewards.iter().map(|(a, _)| a.clone()).collect(),
        };

        // Compute attestation root after attestations are set
        block.header.attestation_root = block.compute_attestation_root();

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

    /// Add a block to the chain
    pub async fn add_block(&self, block: Block) -> Result<(), String> {
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
                Err(_) => {
                    // CRITICAL: Cannot add block if we don't have the previous block
                    // This prevents accepting blocks with gaps in the chain
                    tracing::warn!(
                        "⚠️ Cannot add block {} - previous block {} not found. Sync from peers first.",
                        block.header.height,
                        block.header.height - 1
                    );
                    return Err(format!(
                        "Cannot add block {} - missing previous block {}. Chain must be continuous.",
                        block.header.height,
                        block.header.height - 1
                    ));
                }
            }
        }

        // Validate block height is sequential
        let current = self.current_height.load(Ordering::Acquire);

        // Special case: genesis block (height 0)
        let is_genesis = block.header.height == 0;

        // Allow genesis block if:
        // 1. Chain is at height 0 AND no block exists at height 0, OR
        // 2. We're at height 0 and trying to add genesis (replace placeholder)
        if is_genesis {
            if current == 0 {
                // Allow genesis at height 0
            } else {
                return Err(format!(
                    "Cannot add genesis block at height 0 when chain is at height {}",
                    current
                ));
            }
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

        // Process UTXOs and create undo log
        let undo_log = self.process_block_utxos(&block).await?;

        // Save undo log for rollback support
        self.save_undo_log(&undo_log)?;

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
        self.current_height
            .store(block.header.height, Ordering::Release);

        // Clear finalized transactions now that they're in a block (archived)
        self.consensus.clear_finalized_transactions();

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

        Ok(())
    }

    /// Get a block by height (with two-tier cache - 10-50x faster for recent blocks)
    pub fn get_block(&self, height: u64) -> Result<Block, String> {
        // Check cache first (fast path)
        if let Some(cached_block) = self.block_cache.get(height) {
            return Ok((*cached_block).clone());
        }

        // Cache miss - load from storage
        let key = format!("block_{}", height);
        let value = self
            .storage
            .get(key.as_bytes())
            .map_err(|e| e.to_string())?;

        if let Some(v) = value {
            let block: Block = bincode::deserialize(&v).map_err(|e| e.to_string())?;

            // Add to cache for future reads
            self.block_cache.put(height, block.clone());

            Ok(block)
        } else {
            Err(format!("Block {} not found", height))
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

    /// Get block cache statistics
    pub fn get_cache_stats(&self) -> crate::block_cache::CacheStats {
        self.block_cache.stats()
    }

    /// Get estimated block cache memory usage in bytes
    pub fn get_cache_memory_usage(&self) -> usize {
        self.block_cache.estimated_memory_usage()
    }

    /// Check if currently syncing (lock-free)
    #[allow(dead_code)]
    pub fn is_syncing(&self) -> bool {
        self.is_syncing.load(Ordering::Acquire)
    }

    /// Set syncing state (lock-free)
    #[allow(dead_code)]
    pub fn set_syncing(&self, syncing: bool) {
        self.is_syncing.store(syncing, Ordering::Release);
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

    // =========================================================================
    // CANONICAL CHAIN SELECTION (Fork Resolution)
    // =========================================================================

    /// Determine which of two competing chains is canonical using deterministic rules.
    ///
    /// Rules (in order of precedence):
    /// 1. Longer chain wins (most work)
    /// 2. Higher cumulative VRF score wins (when equal length)
    /// 3. Lower tip hash wins (deterministic tiebreaker when equal scores)
    ///
    /// This function MUST be deterministic - all nodes must make the same decision
    /// given the same inputs.
    pub fn choose_canonical_chain(
        our_height: u64,
        our_tip_hash: [u8; 32],
        our_cumulative_score: u128,
        peer_height: u64,
        peer_tip_hash: [u8; 32],
        peer_cumulative_score: u128,
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

        // Heights are equal - use Rule 2: Higher cumulative VRF score wins
        if peer_cumulative_score > our_cumulative_score {
            return (
                CanonicalChoice::AdoptPeers,
                format!(
                    "Equal height {}, but peer has higher VRF score: {} > {}",
                    our_height, peer_cumulative_score, our_cumulative_score
                ),
            );
        }
        if our_cumulative_score > peer_cumulative_score {
            return (
                CanonicalChoice::KeepOurs,
                format!(
                    "Equal height {}, our VRF score is higher: {} > {}",
                    our_height, our_cumulative_score, peer_cumulative_score
                ),
            );
        }

        // Scores are equal - use Rule 3: Lexicographically smaller hash wins
        // This is a deterministic tiebreaker that all nodes will agree on
        if peer_tip_hash < our_tip_hash {
            return (
                CanonicalChoice::AdoptPeers,
                format!(
                    "Equal height {} and score {}, peer has smaller tip hash",
                    our_height, our_cumulative_score
                ),
            );
        }
        if our_tip_hash < peer_tip_hash {
            return (
                CanonicalChoice::KeepOurs,
                format!(
                    "Equal height {} and score {}, our tip hash is smaller",
                    our_height, our_cumulative_score
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
    pub async fn get_transaction_confirmations(&self, _txid: &[u8; 32]) -> Option<u64> {
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

        // Optimize disk I/O: Only flush every 10 blocks instead of every block
        // Sled handles durability via write-ahead log, so this is safe
        // Reduces I/O pressure while maintaining data integrity
        if block.header.height % 10 == 0 {
            self.storage.flush().map_err(|e| {
                tracing::error!(
                    "❌ Failed to flush block {} to disk: {}",
                    block.header.height,
                    e
                );
                e.to_string()
            })?;
            tracing::debug!("💾 Flushed blocks up to height {}", block.header.height);
        }

        Ok(())
    }

    /// Store pending fees to be added to next block reward
    fn store_pending_fees(&self, fees: u64) -> Result<(), String> {
        let key = "pending_fees".as_bytes();
        let fee_bytes = bincode::serialize(&fees).map_err(|e| e.to_string())?;
        self.storage
            .insert(key, fee_bytes)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Get pending fees from previous block (to add to current block reward)
    fn get_pending_fees(&self) -> u64 {
        let key = "pending_fees".as_bytes();
        match self.storage.get(key) {
            Ok(Some(bytes)) => bincode::deserialize(&bytes).unwrap_or(0),
            _ => 0, // No pending fees (genesis or first block after restart)
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

    fn calculate_rewards_from_info(&self, masternodes: &[MasternodeInfo]) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        let per_masternode = BLOCK_REWARD_SATOSHIS / masternodes.len() as u64;
        masternodes
            .iter()
            .map(|mn| (mn.masternode.wallet_address.clone(), per_masternode))
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

        // Maximum nodes that receive rewards per block (for scalability)
        const MAX_REWARD_RECIPIENTS: usize = 10;

        // Select masternodes for rewards using deterministic rotation
        let selected_masternodes =
            self.select_reward_recipients(masternodes, MAX_REWARD_RECIPIENTS);

        if selected_masternodes.is_empty() {
            return vec![];
        }

        // Calculate total weight using tier's reward_weight
        let total_weight: u64 = selected_masternodes
            .iter()
            .map(|mn| mn.masternode.tier.reward_weight())
            .sum();

        tracing::info!(
            "💰 Reward calculation: {} of {} masternodes selected, total_reward={} satoshis ({} TIME), total_weight={}",
            selected_masternodes.len(),
            masternodes.len(),
            total_reward,
            total_reward / 100_000_000,
            total_weight
        );

        if total_weight == 0 {
            return vec![];
        }

        // Distribute rewards proportionally based on tier weights
        let mut rewards = Vec::new();
        let mut distributed = 0u64;

        for (i, mn) in selected_masternodes.iter().enumerate() {
            let share = if i == selected_masternodes.len() - 1 {
                // Last masternode gets remainder to avoid rounding errors
                total_reward - distributed
            } else {
                (total_reward * mn.masternode.tier.reward_weight()) / total_weight
            };

            tracing::info!(
                "   → {} (tier {:?}, weight {}): share={} satoshis ({} TIME)",
                mn.masternode.address,
                mn.masternode.tier,
                mn.masternode.tier.reward_weight(),
                share,
                share / 100_000_000
            );

            rewards.push((mn.masternode.wallet_address.clone(), share));
            distributed += share;
        }

        rewards
    }

    /// Select masternodes for reward distribution using deterministic rotation
    /// Returns up to max_recipients masternodes, rotating fairly based on block height
    /// Phase 3.3: Only selects masternodes with valid locked collateral
    fn select_reward_recipients(
        &self,
        masternodes: &[MasternodeInfo],
        max_recipients: usize,
    ) -> Vec<MasternodeInfo> {
        // Phase 3.3: Filter masternodes by collateral status
        // NOTE: We're lenient here to prevent network stalls. Masternodes with configured
        // but unlocked collateral are warned but still allowed to participate.
        let eligible_masternodes: Vec<MasternodeInfo> = masternodes
            .iter()
            .map(|mn| {
                // Legacy masternodes (no collateral_outpoint) are always eligible
                if mn.masternode.collateral_outpoint.is_none() {
                    return mn.clone();
                }

                // New masternodes should have locked collateral, but we allow participation
                // even if collateral isn't locked to prevent network stalls
                if let Some(collateral_outpoint) = &mn.masternode.collateral_outpoint {
                    if !self.utxo_manager.is_collateral_locked(collateral_outpoint) {
                        tracing::warn!(
                            "⚠️ Masternode {} participating without locked collateral {:?} - should lock collateral soon",
                            mn.masternode.address,
                            collateral_outpoint
                        );
                    }
                }

                mn.clone()
            })
            .collect();

        let total_nodes = eligible_masternodes.len();

        // If we have fewer than max, reward all eligible
        if total_nodes <= max_recipients {
            return eligible_masternodes;
        }

        // Deterministic selection based on block height
        // This ensures all nodes agree on who gets rewarded
        let current_height = self.get_height();

        // Sort masternodes by address to ensure consistent ordering across all nodes
        let mut sorted_masternodes = eligible_masternodes;
        sorted_masternodes.sort_by(|a, b| a.masternode.address.cmp(&b.masternode.address));

        // Calculate starting offset based on block height
        // Each block rotates by max_recipients, so every node gets a turn
        let offset = (current_height as usize * max_recipients) % total_nodes;

        // Select max_recipients nodes starting from offset, wrapping around if needed
        let mut selected = Vec::new();
        for i in 0..max_recipients {
            let idx = (offset + i) % total_nodes;
            selected.push(sorted_masternodes[idx].clone());
        }

        tracing::info!(
            "🎯 Reward rotation at height {}: selected {} nodes starting from position {} of {} total",
            current_height,
            selected.len(),
            offset,
            total_nodes
        );

        selected
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

        // CRITICAL: Validate the block_reward is correct (base reward + fees from previous block)
        // Get fees from previous block (stored during block production)
        let previous_block_fees = self.get_pending_fees();
        let expected_reward = BLOCK_REWARD_SATOSHIS + previous_block_fees;

        if block.header.block_reward != expected_reward {
            return Err(format!(
                "Block {} has incorrect block_reward: expected {} (base {} + fees {}), got {}",
                block.header.height,
                expected_reward,
                BLOCK_REWARD_SATOSHIS,
                previous_block_fees,
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

    /// Find the common ancestor between our chain and a peer's chain
    /// peer_hashes: Vec of (height, hash) from peer, ordered by height descending
    pub async fn find_common_ancestor(&self, peer_hashes: &[(u64, [u8; 32])]) -> Option<u64> {
        if peer_hashes.is_empty() {
            return None;
        }

        // First, check all the provided peer hashes
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

        // If no match found in provided hashes, keep going back through our chain
        // to find the common ancestor. Start from the lowest provided peer hash.
        let lowest_peer_height = peer_hashes.iter().map(|(h, _)| *h).min().unwrap_or(0);

        info!(
            "No common ancestor found in provided hashes, searching backwards from height {}",
            lowest_peer_height
        );

        // Walk backwards from the lowest peer height to genesis
        for height in (0..lowest_peer_height).rev() {
            if let Ok(_our_hash) = self.get_block_hash(height) {
                // Check if peer has this block (we'd need to query, but for now just check what we have)
                // Since we don't have all peer hashes, continue walking back
                // The safest fallback is genesis if we can't find a common point
                if height == 0 {
                    info!("Reached genesis block, using as common ancestor");
                    return Some(0);
                }
            }
        }

        // Genesis should always exist as the common ancestor
        if self.get_block(0).is_ok() {
            info!("Falling back to genesis as common ancestor");
            return Some(0);
        }

        None
    }

    /// Save undo log for a block
    fn save_undo_log(&self, undo_log: &UndoLog) -> Result<(), String> {
        let key = format!("undo_{}", undo_log.height);
        let data = bincode::serialize(undo_log).map_err(|e| e.to_string())?;
        self.storage
            .insert(key.as_bytes(), data)
            .map_err(|e| e.to_string())?;
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
        // TODO: Need to pass transaction pool reference to restore transactions
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
        let is_recent_block = false; // TODO: Use atomic counter for non-blocking height check

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
    /// Requests block height from peers and compares
    ///
    /// **PRIMARY FORK RESOLUTION ENTRY POINT**
    /// This is the recommended way to detect and resolve forks.
    /// It runs periodically and queries all peers for consensus.
    ///
    /// TODO(refactor): Coordinate with sync_coordinator to prevent duplicate sync requests
    /// Currently periodic fork resolution can conflict with opportunistic sync
    /// See: analysis/REFACTORING_ROADMAP.md - Phase 3, Step 3.3
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
        let connected_peers = registry.get_compatible_peers().await;
        if connected_peers.is_empty() {
            tracing::debug!("No compatible peers connected");
            return None;
        }

        tracing::debug!(
            "🔍 [LOCKED] PRIMARY FORK RESOLUTION: Periodic check with {} compatible peers",
            connected_peers.len()
        );

        tracing::info!(
            "🔍 [FORK CHECK] Querying {} connected compatible peers for chain status",
            connected_peers.len()
        );

        // Request chain tips (height + hash) from all peers
        for peer in &connected_peers {
            let request = NetworkMessage::GetChainTip;
            if let Err(e) = registry.send_to_peer(peer, request).await {
                tracing::warn!("⚠️  Failed to send GetChainTip to {}: {}", peer, e);
            } else {
                tracing::debug!("📤 Sent GetChainTip request to {}", peer);
            }
        }

        // Wait for responses (with timeout)
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Collect peer chain tips (height + hash) from registry
        let mut peer_tips: std::collections::HashMap<String, (u64, [u8; 32])> =
            std::collections::HashMap::new();
        for peer_ip in &connected_peers {
            if let Some((height, hash)) = registry.get_peer_chain_tip(peer_ip).await {
                peer_tips.insert(peer_ip.clone(), (height, hash));
                tracing::debug!("✅ Got response from {}: height {}", peer_ip, height);
            } else {
                tracing::warn!("❌ No response from {} within timeout", peer_ip);
            }
        }

        if peer_tips.is_empty() {
            tracing::warn!(
                "⚠️  No peer chain tip responses received from {} peers!",
                connected_peers.len()
            );
            return None;
        }

        // DEBUG: Log what we received from peers
        tracing::info!(
            "🔍 [DEBUG] Received chain tips from {}/{} peers:",
            peer_tips.len(),
            connected_peers.len()
        );
        for (peer_ip, (height, hash)) in &peer_tips {
            tracing::info!(
                "   Peer {}: height {} hash {}",
                peer_ip,
                height,
                hex::encode(&hash[..8])
            );
            // Record chain tip for AI consensus health monitoring
            self.consensus_health.record_chain_tip(*height, *hash);
        }

        let our_height = self.get_height();
        let our_hash = match self.get_block_hash(our_height) {
            Ok(hash) => hash,
            Err(_) => return None,
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

        // Find the best chain: MAJORITY RULES
        // A single peer on a higher height should NOT override majority at lower height
        // This prevents incompatible/forked peers from blocking consensus

        // Log all detected chains
        tracing::info!(
            "🔍 [CHAIN ANALYSIS] Detected {} different chains:",
            chain_counts.len()
        );
        for ((height, hash), peers) in &chain_counts {
            tracing::info!(
                "   📊 Chain @ height {}, hash {}: {} peers {:?}",
                height,
                hex::encode(&hash[..8]),
                peers.len(),
                peers
            );
        }

        // Find the chain with the MOST peer support
        // Only use height as tiebreaker when peer counts are equal
        let consensus_chain = chain_counts
            .iter()
            .max_by(|((h1, _), peers1), ((h2, _), peers2)| {
                // Primary: more peers wins (majority rules)
                let peer_cmp = peers1.len().cmp(&peers2.len());
                if peer_cmp != std::cmp::Ordering::Equal {
                    return peer_cmp;
                }
                // Secondary: higher height wins (at same peer count)
                h1.cmp(h2)
            })
            .map(|((height, hash), peers)| (*height, *hash, peers.clone()))?;

        let (consensus_height, consensus_hash, consensus_peers) = consensus_chain;

        tracing::info!(
            "✅ [CONSENSUS SELECTED] Height {}, hash {}, {} peers: {:?}",
            consensus_height,
            hex::encode(&consensus_hash[..8]),
            consensus_peers.len(),
            consensus_peers
        );

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
            height: consensus_height,
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
            warn!(
                "🧠 [AI] Consensus health warning: score={:.2}, fork_prob={:.2}, action={:?}",
                health.health_score, health.fork_probability, health.recommended_action
            );
            for reason in &health.reasoning {
                warn!("   Reason: {}", reason);
            }
        }

        // DEBUG: Log consensus decision
        tracing::info!(
            "🔍 [DEBUG] Consensus: height {} hash {} ({} peers agree). Our height: {} hash {}",
            consensus_height,
            hex::encode(&consensus_hash[..8]),
            consensus_peers.len(),
            our_height,
            hex::encode(&our_hash[..8])
        );

        // Case 1: Consensus chain is longer - definitely switch
        if consensus_height > our_height {
            tracing::warn!(
                "🔀 FORK RESOLUTION TRIGGERED: consensus height {} > our height {} ({} peers agree: {:?})",
                consensus_height,
                our_height,
                consensus_peers.len(),
                consensus_peers
            );
            tracing::warn!("   Will attempt to sync from peer: {}", consensus_peers[0]);
            return Some((consensus_height, consensus_peers[0].clone()));
        }

        // Case 2: Same height but different hash - fork at same height!
        if consensus_height == our_height && consensus_hash != our_hash {
            warn!(
                "🔀 Fork at same height {}: our hash {} vs consensus hash {} ({} peers)",
                consensus_height,
                hex::encode(&our_hash[..8]),
                hex::encode(&consensus_hash[..8]),
                consensus_peers.len()
            );

            // PHASE 1: Analyze masternode authority (PRIMARY DECISION)
            let _our_chain_peers = chain_counts
                .get(&(our_height, our_hash))
                .cloned()
                .unwrap_or_default();

            // Analyze our chain's masternode support
            let our_authority =
                crate::masternode_authority::CanonicalChainSelector::analyze_our_chain_authority(
                    &self.masternode_registry,
                    self.connection_manager.read().await.as_ref().map(|v| &**v),
                    self.peer_registry.read().await.as_ref().map(|v| &**v),
                )
                .await;

            // Analyze consensus chain's masternode support
            let consensus_authority =
                crate::masternode_authority::CanonicalChainSelector::analyze_peer_chain_authority(
                    &consensus_peers,
                    &self.masternode_registry,
                    self.peer_registry.read().await.as_ref().map(|v| &**v),
                )
                .await;

            info!(
                "📊 Masternode Authority Analysis:\n   Our chain: {}\n   Consensus: {}",
                our_authority.format_summary(),
                consensus_authority.format_summary()
            );

            // Determine canonical chain based on masternode authority
            let our_chain_work = *self.cumulative_work.read().await;
            let peer_chain_work = our_chain_work; // Same height = approximately equal work

            let (should_switch, reason) =
                crate::masternode_authority::CanonicalChainSelector::should_switch_to_peer_chain(
                    &our_authority,
                    &consensus_authority,
                    our_chain_work,
                    peer_chain_work,
                    our_height,
                    consensus_height,
                    &our_hash,
                    &consensus_hash,
                );

            // VRF-BASED TIEBREAKER: When masternode authority reaches hash tiebreaker,
            // use VRF scores instead for cryptographically fair selection
            let (final_should_switch, final_reason) = if reason.contains("deterministic tiebreaker")
            {
                // Calculate VRF scores for both chains
                let our_vrf_score = self.calculate_chain_vrf_score(0, our_height).await;

                // For peer chain, we estimate score based on their tip hash
                // (full VRF comparison would require requesting peer blocks)
                // Use first 16 bytes of hash as proxy for peer VRF score
                let peer_vrf_score =
                    u128::from_be_bytes(consensus_hash[0..16].try_into().unwrap_or([0u8; 16]));

                let (vrf_choice, vrf_reason) = Self::choose_canonical_chain(
                    our_height,
                    our_hash,
                    our_vrf_score,
                    consensus_height,
                    consensus_hash,
                    peer_vrf_score,
                );

                match vrf_choice {
                    CanonicalChoice::AdoptPeers => (
                        true,
                        format!(
                            "SWITCH (VRF): {} | Our VRF: {}, Peer VRF: {}",
                            vrf_reason, our_vrf_score, peer_vrf_score
                        ),
                    ),
                    CanonicalChoice::KeepOurs => (
                        false,
                        format!(
                            "KEEP (VRF): {} | Our VRF: {}, Peer VRF: {}",
                            vrf_reason, our_vrf_score, peer_vrf_score
                        ),
                    ),
                    CanonicalChoice::Identical => (should_switch, reason),
                }
            } else {
                (should_switch, reason)
            };

            warn!(
                "   Decision: {} - {}",
                if final_should_switch {
                    "SWITCH to consensus"
                } else {
                    "KEEP our chain"
                },
                final_reason
            );

            if final_should_switch {
                return Some((consensus_height, consensus_peers[0].clone()));
            } else {
                return None;
            }
        }

        // Case 3: We're ahead of consensus
        // LONGEST VALID CHAIN RULE: If we have a valid longer chain, WE are canonical
        if our_height > consensus_height {
            tracing::info!(
                "📈 We have the longest chain: height {} > consensus {} ({} peers behind)",
                our_height,
                consensus_height,
                consensus_peers.len()
            );

            // Don't roll back - we ARE the canonical chain
            // Peers will sync to us when they receive our blocks
            return None;
        }

        // Case 4: Same height, same hash - no fork
        None
    }

    /// Start periodic chain comparison task
    pub fn start_chain_comparison_task(blockchain: Arc<Blockchain>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15)); // Every 15 seconds for immediate sync

            loop {
                interval.tick().await;

                let our_height = blockchain.get_height();
                tracing::debug!("🔍 Periodic chain check: our height = {}", our_height);

                // Query peers for their heights and check for forks
                if let Some((consensus_height, consensus_peer)) =
                    blockchain.compare_chain_with_peers().await
                {
                    // Check if this is a same-height fork or we're behind
                    if consensus_height == our_height {
                        // Same height fork - need to reorg
                        tracing::warn!(
                            "🔀 Periodic fork detection: same-height fork at {}, rolling back and resyncing from {}",
                            consensus_height,
                            consensus_peer
                        );

                        // Rollback the incorrect block
                        let rollback_to = consensus_height.saturating_sub(1);
                        match blockchain.rollback_to_height(rollback_to).await {
                            Ok(_) => {
                                tracing::info!("✅ Rolled back to height {}", rollback_to);

                                // CRITICAL FIX: Request a range of earlier blocks to find common ancestor
                                // A fork at the same height likely means the fork is deeper
                                // Request from 20 blocks back to ensure we find the true common ancestor
                                if let Some(peer_registry) =
                                    blockchain.peer_registry.read().await.as_ref()
                                {
                                    let request_from = consensus_height.saturating_sub(20).max(1);

                                    // ✅ Check with sync coordinator before requesting
                                    match blockchain.sync_coordinator.request_sync(
                                        consensus_peer.clone(),
                                        request_from,
                                        consensus_height,
                                        crate::network::sync_coordinator::SyncSource::ForkResolution,
                                    ).await {
                                        Ok(true) => {
                                            let req = NetworkMessage::GetBlocks(request_from, consensus_height);
                                            if let Err(e) = peer_registry.send_to_peer(&consensus_peer, req).await {
                                                blockchain.sync_coordinator.cancel_sync(&consensus_peer).await;
                                                tracing::warn!(
                                                    "⚠️  Failed to request blocks from {}: {}",
                                                    consensus_peer,
                                                    e
                                                );
                                            } else {
                                                tracing::info!(
                                                    "📤 Requested blocks {}-{} from {} to find common ancestor",
                                                    request_from,
                                                    consensus_height,
                                                    consensus_peer
                                                );
                                            }
                                        }
                                        Ok(false) => {
                                            tracing::debug!("⏸️ Fork resolution sync queued with {}", consensus_peer);
                                        }
                                        Err(e) => {
                                            tracing::debug!("⏱️ Fork resolution sync throttled with {}: {}", consensus_peer, e);
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!("❌ Failed to rollback for fork resolution: {}", e);
                            }
                        }
                    } else if consensus_height > our_height {
                        // We're behind - normal sync
                        tracing::info!(
                            "🔀 Periodic fork detection: consensus height {} > our height {}, syncing from {}",
                            consensus_height,
                            our_height,
                            consensus_peer
                        );

                        // Trigger sync from the consensus peer
                        if let Err(e) = blockchain.sync_from_specific_peer(&consensus_peer).await {
                            tracing::warn!(
                                "⚠️  Failed to sync from consensus peer {} during periodic check: {}",
                                consensus_peer,
                                e
                            );
                        } else {
                            tracing::info!(
                                "✅ Periodic chain sync completed from {}",
                                consensus_peer
                            );
                        }
                    }
                    // Note: consensus_height < our_height case is handled by compare_chain_with_peers
                    // returning None (we don't roll back a longer valid chain)
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
        info!(
            "🔀 Fork detected at height {} from peer {}",
            fork_height, peer_addr
        );

        // Transition to FindingAncestor state
        *self.fork_state.write().await = ForkResolutionState::FindingAncestor {
            fork_height,
            peer_addr: peer_addr.clone(),
            check_height: fork_height.saturating_sub(1),
            searched_back: 0,
            started_at: std::time::Instant::now(), // NEW: Track start time
        };

        // Start the ancestor search
        self.continue_fork_resolution().await
    }

    /// Continue fork resolution state machine with timeout protection
    async fn continue_fork_resolution(&self) -> Result<(), String> {
        // Check for stale fork resolution state (timeout after 2 minutes)
        const FORK_RESOLUTION_TIMEOUT_SECS: u64 = 120;

        let state = self.fork_state.read().await.clone();

        // Check timeout for states with timestamps
        match &state {
            ForkResolutionState::FindingAncestor { started_at, .. }
            | ForkResolutionState::FetchingChain { started_at, .. }
            | ForkResolutionState::Reorging { started_at, .. } => {
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

            ForkResolutionState::FindingAncestor {
                fork_height: _,
                peer_addr,
                check_height,
                searched_back,
                started_at: _,
            } => {
                // Safety check - don't search too far back
                if searched_back > 2000 {
                    warn!(
                        "🚨 Searched back {} blocks without finding common ancestor",
                        searched_back
                    );
                    *self.fork_state.write().await = ForkResolutionState::None;
                    return Err("Deep fork >2000 blocks - chains incompatible".to_string());
                }

                // Request the block at check_height from peer
                self.request_single_block_from_peer(&peer_addr, check_height)
                    .await?;

                // State will transition when we receive the block
                Ok(())
            }

            ForkResolutionState::FetchingChain {
                common_ancestor: _,
                fork_height: _,
                peer_addr,
                peer_height,
                fetched_up_to,
                started_at: _,
            } => {
                if fetched_up_to >= peer_height {
                    // We have the complete alternate chain, ready to reorg
                    info!(
                        "✅ Fetched complete alternate chain up to height {}",
                        peer_height
                    );
                    // Transition handled when blocks arrive
                    Ok(())
                } else {
                    // Request more blocks
                    let start = fetched_up_to + 1;
                    let end = (start + 100).min(peer_height);
                    info!("📤 Requesting blocks {}-{} from peer", start, end);
                    self.request_blocks_from_peer(&peer_addr, start, end)
                        .await?;
                    Ok(())
                }
            }

            ForkResolutionState::ReadyToReorg {
                common_ancestor,
                alternate_blocks,
            } => {
                // Perform the reorganization
                self.perform_reorg(common_ancestor, alternate_blocks)
                    .await?;
                *self.fork_state.write().await = ForkResolutionState::None;
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

    /// Called when we receive a block during fork resolution
    pub async fn handle_fork_resolution_block(
        &self,
        block: Block,
        peer_addr: &str,
    ) -> Result<(), String> {
        let mut state = self.fork_state.write().await;

        match &*state {
            ForkResolutionState::FindingAncestor {
                fork_height,
                check_height,
                searched_back,
                started_at,
                ..
            } => {
                let check_height = *check_height;
                let searched_back = *searched_back;
                let fork_height = *fork_height;
                let started_at = *started_at; // Capture for use below

                // Check if this block matches our block at the same height
                if let Ok(our_block) = self.get_block(block.header.height) {
                    if our_block.hash() == block.hash() {
                        // Found common ancestor!
                        info!("✅ Found common ancestor at height {}", block.header.height);

                        // Get peer's tip height (we'll need to request it)
                        // For now, assume fork_height is close to their tip
                        let peer_height = fork_height + 10; // Estimate, will be corrected

                        *state = ForkResolutionState::FetchingChain {
                            common_ancestor: block.header.height,
                            fork_height,
                            peer_addr: peer_addr.to_string(),
                            peer_height,
                            fetched_up_to: block.header.height,
                            started_at: std::time::Instant::now(), // NEW: Track start time
                        };

                        drop(state);
                        return self.continue_fork_resolution().await;
                    }
                }

                // No match - go back further
                if check_height == 0 {
                    *state = ForkResolutionState::None;
                    return Err("No common ancestor found - chains split at genesis".to_string());
                }

                *state = ForkResolutionState::FindingAncestor {
                    fork_height,
                    peer_addr: peer_addr.to_string(),
                    check_height: check_height - 1,
                    searched_back: searched_back + 1,
                    started_at, // Preserve original start time
                };

                drop(state);
                self.continue_fork_resolution().await
            }

            _ => {
                // Not in ancestor-finding state, block will be handled normally
                Ok(())
            }
        }
    }

    /// Request a single block from a peer
    async fn request_single_block_from_peer(
        &self,
        peer_addr: &str,
        height: u64,
    ) -> Result<(), String> {
        info!(
            "📤 Requesting block at height {} from {}",
            height, peer_addr
        );

        let registry = self.peer_registry.read().await;
        if let Some(reg) = registry.as_ref() {
            let msg = NetworkMessage::GetBlocks(height, height + 1);
            reg.send_to_peer(peer_addr, msg)
                .await
                .map_err(|e| format!("Failed to request block: {}", e))?;
            Ok(())
        } else {
            Err("Peer registry not available".to_string())
        }
    }

    /// Request range of blocks from a peer
    async fn request_blocks_from_peer(
        &self,
        peer_addr: &str,
        start: u64,
        end: u64,
    ) -> Result<(), String> {
        info!("📤 Requesting blocks {}-{} from {}", start, end, peer_addr);

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
        for block in alternate_blocks {
            self.add_block(block)
                .await
                .map_err(|e| format!("Failed to add block during reorg: {}", e))?;
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
            txs_to_replay: 0, // TODO: track this
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

    /// Remove a block at specific height (helper for rollback)
    async fn remove_block_at_height(&self, height: u64) -> Result<(), String> {
        let key = format!("block:{}", height);
        self.storage
            .remove(key.as_bytes())
            .map_err(|e| format!("Failed to remove block at height {}: {}", height, e))?;

        // Also remove from hash index if we have the block
        // This is a simplified version - full implementation would also revert UTXO changes
        Ok(())
    }

    /// AI-powered fork resolution with fallback to traditional rules
    /// Returns true if we should accept the new blocks (they extend a better chain)
    ///
    /// **DEPRECATED**: This method creates duplicate fork resolution paths.
    /// Prefer using the unified fork resolution through periodic chain comparison.
    /// This method will be removed in a future version.
    ///
    /// Current issue: Multiple code paths can trigger fork resolution simultaneously:
    /// - This method (when receiving blocks)
    /// - compare_chain_with_peers() (periodic check)
    /// - Causes race conditions and conflicting decisions
    #[deprecated(
        note = "Use unified fork resolution. This creates race conditions with periodic checks."
    )]
    pub async fn should_accept_fork(
        &self,
        competing_blocks: &[Block],
        peer_claimed_height: u64,
        peer_ip: &str,
    ) -> Result<bool, String> {
        // CRITICAL: Acquire fork resolution lock to prevent concurrent fork resolutions
        // This prevents race conditions when multiple peers send competing chains simultaneously
        let _lock = self.fork_resolution_lock.lock().await;

        warn!(
            "⚠️ DEPRECATED: should_accept_fork called for peer {} (use unified resolution instead)",
            peer_ip
        );

        if competing_blocks.is_empty() {
            return Ok(false);
        }

        let our_height = self.get_height();
        let fork_height = competing_blocks.first().unwrap().header.height;

        tracing::info!(
            "🔀 [LOCKED] Fork resolution: comparing chains at height {} (our height: {}, peer height: {})",
            fork_height,
            our_height,
            peer_claimed_height
        );

        // Get chain work for both chains
        let our_chain_work = *self.cumulative_work.read().await;

        // Calculate peer's chain work (estimate based on blocks we have)
        let peer_chain_work = self
            .estimate_peer_chain_work(competing_blocks, peer_claimed_height)
            .await;

        // Gather supporting peer information
        let supporting_peers = self
            .gather_supporting_peers(our_height, peer_claimed_height)
            .await;

        // Find common ancestor
        let common_ancestor = match self.find_fork_common_ancestor(competing_blocks).await {
            Ok(ancestor) => ancestor,
            Err(e) => {
                // Peer didn't provide enough block history - reject this attempt
                warn!(
                    "Cannot determine common ancestor: {}. Rejecting peer chain.",
                    e
                );
                return Ok(false);
            }
        };

        // Get peer's tip timestamp for future-block validation
        let peer_tip_timestamp = competing_blocks.last().map(|b| b.header.timestamp);

        // Get tip hashes for deterministic tiebreaker
        let our_tip_hash = self.get_block_hash(our_height).ok();
        let peer_tip_hash = competing_blocks.last().map(|b| b.hash());

        // Get our tip timestamp
        let our_tip_timestamp = if let Ok(our_tip) = self.get_block(our_height) {
            Some(our_tip.header.timestamp)
        } else {
            None
        };

        // Check if peer is whitelisted
        let peer_is_whitelisted = if let Some(registry) = self.peer_registry.read().await.as_ref() {
            registry.is_whitelisted(peer_ip).await
        } else {
            false // Default to not whitelisted if registry not available
        };

        // Calculate fork depth
        let fork_depth = our_height.saturating_sub(common_ancestor);

        // Use fork resolver to make decision
        let resolution = self
            .fork_resolver
            .resolve_fork(crate::ai::fork_resolver::ForkResolutionParams {
                our_height,
                our_chain_work,
                peer_height: peer_claimed_height,
                peer_chain_work,
                peer_ip: peer_ip.to_string(),
                supporting_peers,
                common_ancestor,
                peer_tip_timestamp,
                our_tip_hash,
                peer_tip_hash,
                peer_is_whitelisted,
                our_tip_timestamp,
                fork_depth,
            })
            .await;

        // Simple rule: if peer has higher valid height, accept
        Ok(resolution.accept_peer_chain)
    }

    /// Early fork evaluation with minimal information
    /// Called when we detect a fork but don't have complete block data yet
    /// Returns: (should_investigate, confidence_message)
    ///
    /// **DEPRECATED**: This method makes decisions with incomplete data.
    /// It can accept/reject forks before having actual block data, leading to
    /// incorrect decisions. Use unified fork resolution instead.
    ///
    /// Issues:
    /// - Estimates peer work without seeing blocks
    /// - Can conflict with should_accept_fork() later
    /// - No tip hash for deterministic tiebreaker
    #[deprecated(
        note = "Makes decisions with incomplete data. Use unified resolution with full block data."
    )]
    pub async fn should_investigate_fork(
        &self,
        fork_height: u64,
        peer_claimed_height: u64,
        peer_ip: &str,
    ) -> (bool, String) {
        // CRITICAL: Acquire fork resolution lock to prevent concurrent fork resolutions
        let _lock = self.fork_resolution_lock.lock().await;

        warn!(
            "⚠️ DEPRECATED: should_investigate_fork called for peer {} (incomplete data)",
            peer_ip
        );

        let our_height = self.get_height();

        // If peer has significantly longer chain, investigate
        if peer_claimed_height > our_height + 10 {
            return (
                true,
                format!(
                    "Peer chain is significantly longer ({} vs {})",
                    peer_claimed_height, our_height
                ),
            );
        }

        // If fork is very recent (within last 10 blocks), investigate
        if our_height - fork_height < 10 {
            return (
                true,
                format!(
                    "Recent fork at {} (current height {})",
                    fork_height, our_height
                ),
            );
        }

        // Use AI fork resolver with minimal information
        let our_chain_work = *self.cumulative_work.read().await;

        // Estimate peer work based on claimed height
        let estimated_peer_work = self
            .estimate_peer_chain_work(&[], peer_claimed_height)
            .await;

        // Gather supporting peer information
        let supporting_peers = self
            .gather_supporting_peers(our_height, peer_claimed_height)
            .await;

        // Get tip hashes for tiebreaker (may not be available in early investigation)
        let our_tip_hash = self.get_block_hash(our_height).ok();
        let peer_tip_hash = None; // Not available during early investigation

        // Get our tip timestamp
        let our_tip_timestamp = if let Ok(our_tip) = self.get_block(our_height) {
            Some(our_tip.header.timestamp)
        } else {
            None
        };

        // Check if peer is whitelisted
        let peer_is_whitelisted = if let Some(registry) = self.peer_registry.read().await.as_ref() {
            registry.is_whitelisted(peer_ip).await
        } else {
            false // Default to not whitelisted if registry not available
        };

        // Calculate fork depth
        let common_ancestor_height = fork_height.saturating_sub(1);
        let fork_depth = our_height.saturating_sub(common_ancestor_height);

        let resolution = self
            .fork_resolver
            .resolve_fork(crate::ai::fork_resolver::ForkResolutionParams {
                our_height,
                our_chain_work,
                peer_height: peer_claimed_height,
                peer_chain_work: estimated_peer_work,
                peer_ip: peer_ip.to_string(),
                supporting_peers,
                common_ancestor: common_ancestor_height,
                peer_tip_timestamp: None, // Unknown at this stage
                our_tip_hash,
                peer_tip_hash,
                peer_is_whitelisted,
                our_tip_timestamp,
                fork_depth,
            })
            .await;

        let message = if resolution.accept_peer_chain {
            format!(
                "AI recommends investigating (confidence: {:.0}%)",
                resolution.confidence * 100.0
            )
        } else {
            format!(
                "AI recommends skipping (confidence: {:.0}%)",
                resolution.confidence * 100.0
            )
        };

        (resolution.accept_peer_chain, message)
    }

    /// Traditional fork resolution (fallback when AI confidence is low)
    async fn traditional_fork_resolution(
        &self,
        our_height: u64,
        peer_claimed_height: u64,
        competing_blocks: &[Block],
    ) -> Result<bool, String> {
        // Rule 1: Longest chain wins
        if peer_claimed_height > our_height {
            tracing::info!(
                "✅ Accepting fork: peer has longer chain ({} > {})",
                peer_claimed_height,
                our_height
            );
            return Ok(true);
        } else if peer_claimed_height < our_height {
            tracing::info!(
                "❌ Rejecting fork: our chain is longer ({} > {})",
                our_height,
                peer_claimed_height
            );
            return Ok(false);
        }

        // Rule 2: Same length - compare hashes (deterministic tiebreaker)
        if let Ok(our_tip_block) = self.get_block(our_height) {
            let peer_tip_block = competing_blocks.last().unwrap();
            let our_tip_hash = our_tip_block.hash();
            let peer_tip_hash = peer_tip_block.hash();

            // Use lexicographic comparison of hashes as tiebreaker
            if peer_tip_hash < our_tip_hash {
                tracing::info!(
                    "✅ Accepting fork: same length but peer has lower hash (tiebreaker)"
                );
                return Ok(true);
            } else {
                tracing::info!("❌ Rejecting fork: same length but our hash is lower (tiebreaker)");
                return Ok(false);
            }
        }

        Ok(false)
    }

    /// Estimate peer's chain work based on blocks we've seen
    async fn estimate_peer_chain_work(&self, blocks: &[Block], peer_height: u64) -> u128 {
        // Start with our common work up to the fork point
        let fork_point = if !blocks.is_empty() {
            blocks.first().unwrap().header.height
        } else {
            peer_height
        };

        let mut work = if fork_point > 0 {
            self.get_chain_work_at_height(fork_point - 1)
                .await
                .unwrap_or(0)
        } else {
            0
        };

        // Add work for peer's chain
        let blocks_on_peer_chain = peer_height - fork_point + 1;
        work += BASE_WORK_PER_BLOCK * blocks_on_peer_chain as u128;

        work
    }

    /// Gather information about which peers support which chain
    async fn gather_supporting_peers(
        &self,
        _our_height: u64,
        _peer_height: u64,
    ) -> Vec<(String, u64, u128)> {
        let mut supporting_peers = Vec::new();

        // Get peer information from registry
        if let Some(registry) = self.peer_registry.read().await.as_ref() {
            let peers = registry.get_connected_peers().await;
            for peer in peers {
                if let Some(height) = registry.get_peer_height(&peer).await {
                    // Estimate chain work for this peer
                    let chain_work = BASE_WORK_PER_BLOCK * height as u128;
                    supporting_peers.push((peer, height, chain_work));
                }
            }
        }

        supporting_peers
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

        // Build a map of peer's blocks for fast lookup (wrapped in Arc for closure)
        let peer_blocks = Arc::new(
            sorted_blocks
                .iter()
                .map(|b| (b.header.height, b.hash()))
                .collect::<std::collections::HashMap<u64, [u8; 32]>>(),
        );

        let peer_height = sorted_blocks.last().unwrap().header.height;
        let peer_lowest = sorted_blocks.first().unwrap().header.height;
        let our_height = self.get_height();

        info!(
            "🔍 Finding common ancestor using exponential+binary search (our: {}, peer: {}, peer blocks: {}-{})",
            our_height, peer_height, peer_lowest, peer_height
        );

        // Create network fork resolver for efficient ancestor finding
        let network_resolver = NetworkForkResolver::default();

        // Check function: returns true if peer has same block hash at height
        // Clone Arc references for the closure
        let peer_blocks_ref = Arc::clone(&peer_blocks);
        let blockchain_ref = self;

        let check_fn = move |height: u64| {
            let peer_blocks = Arc::clone(&peer_blocks_ref);
            async move {
                // Get our block hash at this height
                let our_hash = match blockchain_ref.get_block_hash(height) {
                    Ok(hash) => hash,
                    Err(_) => return Ok(false), // Can't find our block at this height
                };

                // Check if peer has a block at this height
                if let Some(peer_hash) = peer_blocks.get(&height) {
                    // Peer has block at this height - check if hashes match
                    Ok(our_hash == *peer_hash)
                } else {
                    // Peer doesn't have this height in the provided blocks.
                    // We can't make assumptions - return false to indicate we need more data.
                    // This will cause the search to either find a lower match or return 0 (genesis).
                    Ok(false)
                }
            }
        };

        // Use the efficient exponential + binary search algorithm
        let ancestor = network_resolver
            .find_common_ancestor(our_height, peer_height, check_fn)
            .await
            .map_err(|e| format!("Error in common ancestor search: {}", e))?;

        // CRITICAL FIX: If ancestor is 0 but peer_lowest is > 100,
        // the blocks slice doesn't go back far enough to find the true common ancestor.
        // Return an error to force the peer to send deeper block history.
        if ancestor == 0 && peer_lowest > 100 {
            return Err(format!(
                "Insufficient block history: peer blocks only go back to height {}, \
                but common ancestor was not found. Peer must provide blocks starting from a lower height \
                (fork likely occurred between height 0 and {}).",
                peer_lowest, peer_lowest
            ));
        }

        info!("✓ Found common ancestor at height {}", ancestor);
        Ok(ancestor)
    }

    /// Get chain work at a specific height
    async fn get_chain_work_at_height(&self, height: u64) -> Result<u128, String> {
        // For now, estimate based on height
        // In the future, this could store actual cumulative work
        Ok(BASE_WORK_PER_BLOCK * height as u128)
    }

    /// Update fork outcome for AI learning
    pub async fn update_fork_outcome(&self, fork_height: u64, was_correct: bool) {
        let outcome = if was_correct {
            crate::ai::fork_resolver::ForkOutcome::CorrectChoice
        } else {
            crate::ai::fork_resolver::ForkOutcome::WrongChoice
        };

        self.fork_resolver
            .update_fork_outcome(fork_height, outcome)
            .await;
    }

    /// Get AI fork resolver statistics
    pub async fn get_fork_resolver_stats(&self) -> crate::ai::fork_resolver::ForkResolverStats {
        self.fork_resolver.get_statistics().await
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
    /// Returns list of corrupt block heights that need resyncing
    pub async fn validate_chain_integrity(&self) -> Result<Vec<u64>, String> {
        let current_height = self.get_height();
        let mut corrupt_blocks = Vec::new();

        tracing::info!(
            "🔍 Validating blockchain integrity (0-{})...",
            current_height
        );

        // Check each block for basic integrity
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
            tracing::info!("✅ Blockchain integrity validation passed");
            Ok(Vec::new())
        } else {
            tracing::error!(
                "❌ Found {} corrupt blocks: {:?}",
                corrupt_blocks.len(),
                corrupt_blocks
            );
            // Automatically trigger self-healing
            tracing::warn!("🔧 Corrupt blocks detected - marking for deletion to trigger re-sync");
            Ok(corrupt_blocks)
        }
    }

    /// Delete corrupt blocks to trigger re-sync from peers
    pub async fn delete_corrupt_blocks(&self, corrupt_heights: &[u64]) -> Result<(), String> {
        if corrupt_heights.is_empty() {
            return Ok(());
        }

        tracing::warn!(
            "🔧 Deleting {} corrupt blocks to trigger re-sync",
            corrupt_heights.len()
        );

        for height in corrupt_heights {
            let key = format!("block_{}", height);
            if let Err(e) = self.storage.remove(key.as_bytes()) {
                tracing::warn!("Failed to delete corrupt block {}: {}", height, e);
            } else {
                tracing::info!("🗑️  Deleted corrupt block {}", height);
            }
        }

        // Update chain height to lowest deleted block - 1
        if let Some(&min_height) = corrupt_heights.iter().min() {
            if min_height > 0 {
                let new_height = min_height - 1;
                self.current_height.store(new_height, Ordering::Release);
                tracing::info!(
                    "📉 Rolled back chain height to {} (lowest corrupt block was {})",
                    new_height,
                    min_height
                );
            }
        }

        Ok(())
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
            block_cache: self.block_cache.clone(),
            validator: BlockValidator::new(self.network_type),
            consensus_health: self.consensus_health.clone(),
            tx_index: self.tx_index.clone(),
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
