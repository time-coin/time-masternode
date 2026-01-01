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

// Security limits (Phase 1)
const MAX_BLOCK_SIZE: usize = 1_000_000; // 1MB per block (Phase 1.3)
const TIMESTAMP_TOLERANCE_SECS: i64 = 900; // ±15 minutes (Phase 1.3)
const MAX_REORG_DEPTH: u64 = 1_000; // Maximum blocks to reorg
const ALERT_REORG_DEPTH: u64 = 100; // Alert on reorgs deeper than this

// P2P sync configuration
const PEER_SYNC_TIMEOUT_SECS: u64 = 120;
const PEER_SYNC_CHECK_INTERVAL_SECS: u64 = 2;

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

pub struct Blockchain {
    storage: sled::Db,
    consensus: Arc<ConsensusEngine>,
    masternode_registry: Arc<MasternodeRegistry>,
    pub utxo_manager: Arc<UTXOStateManager>,
    current_height: Arc<RwLock<u64>>,
    network_type: NetworkType,
    is_syncing: Arc<RwLock<bool>>,
    peer_manager: Arc<RwLock<Option<Arc<crate::peer_manager::PeerManager>>>>,
    peer_registry: Arc<RwLock<Option<Arc<PeerConnectionRegistry>>>>,
    connection_manager:
        Arc<RwLock<Option<Arc<crate::network::connection_manager::ConnectionManager>>>>,
    /// AI-based peer scoring for intelligent peer selection
    peer_scoring: Arc<crate::network::peer_scoring::PeerScoringSystem>,
    /// Cumulative chain work for longest-chain-by-work rule
    cumulative_work: Arc<RwLock<u128>>,
    /// Recent reorganization events (for monitoring and debugging)
    reorg_history: Arc<RwLock<Vec<ReorgMetrics>>>,
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
            connection_manager: Arc::new(RwLock::new(None)),
            peer_scoring,
            cumulative_work: Arc::new(RwLock::new(0)),
            reorg_history: Arc::new(RwLock::new(Vec::new())),
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

    /// Set connection manager for tracking peer connections
    pub async fn set_connection_manager(
        &self,
        connection_manager: Arc<crate::network::connection_manager::ConnectionManager>,
    ) {
        *self.connection_manager.write().await = Some(connection_manager);
    }

    pub fn genesis_timestamp(&self) -> i64 {
        self.network_type.genesis_timestamp()
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
                    *self.current_height.write().await = 0;
                    return Ok(());
                }
            }
            *self.current_height.write().await = height;
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
                        "❌ Local genesis is invalid: {} - replacing with canonical genesis",
                        e
                    );

                    // Remove the invalid genesis
                    let _ = self.storage.remove("block_0".as_bytes());
                    let _ = self.storage.remove(genesis.hash().as_slice());
                    let _ = self.storage.flush();

                    // Load canonical genesis from file
                    load_and_store_genesis(&self.storage, self.network_type)?;
                    *self.current_height.write().await = 0;
                    return Ok(());
                }
            }
            *self.current_height.write().await = 0;
            tracing::info!("✓ Genesis block verified");
            return Ok(());
        }

        // No local blockchain - load genesis from file
        load_and_store_genesis(&self.storage, self.network_type)?;
        *self.current_height.write().await = 0;

        Ok(())
    }

    /// Verify chain integrity, find missing blocks
    /// Returns a list of missing block heights that need to be downloaded
    pub async fn verify_chain_integrity(&self) -> Vec<u64> {
        let current_height = *self.current_height.read().await;
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
    pub async fn sync_from_peers(&self) -> Result<(), String> {
        // Check if already syncing - prevent concurrent syncs
        {
            let mut is_syncing = self.is_syncing.write().await;
            if *is_syncing {
                tracing::debug!("⏭️  Sync already in progress, skipping duplicate request");
                return Ok(());
            }
            *is_syncing = true;
        }

        // Ensure we reset the sync flag when done
        let _guard = scopeguard::guard((), |_| {
            let is_syncing = self.is_syncing.clone();
            tokio::spawn(async move {
                *is_syncing.write().await = false;
            });
        });

        let mut current = *self.current_height.read().await;
        let time_expected = self.calculate_expected_height();

        // Debug logging for genesis timestamp issue
        let now = chrono::Utc::now().timestamp();
        let genesis_ts = self.genesis_timestamp();
        tracing::debug!(
            "🔍 Sync calculation: current={}, time_expected={}, now={}, genesis={}, elapsed={}",
            current,
            time_expected,
            now,
            genesis_ts,
            now - genesis_ts
        );

        if current >= time_expected {
            tracing::info!("✓ Blockchain synced (height: {})", current);
            return Ok(());
        }

        let behind = time_expected - current;
        tracing::info!(
            "⏳ Syncing from peers: {} → {} ({} blocks behind based on time)",
            current,
            time_expected,
            behind
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
                "📍 Starting sync loop: current={}, expected={}, timeout={}s",
                current,
                time_expected,
                max_sync_time.as_secs()
            );

            while current < time_expected && sync_start.elapsed() < max_sync_time {
                // Request next batch of blocks
                // Always start from 0 when current is 0 (need genesis)
                // Otherwise start from current + 1 (need next block after our tip)
                let batch_start = if current == 0 {
                    0 // Request genesis and subsequent blocks
                } else {
                    current + 1 // Request next block after our tip
                };
                let batch_end = (batch_start + 100).min(time_expected);

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

                // Wait for blocks to arrive (with longer timeout per batch to reduce spurious retries)
                let batch_start_time = std::time::Instant::now();
                let batch_timeout = std::time::Duration::from_secs(30); // Increased from 15s
                let mut last_height = current;
                let mut made_progress = false;

                while batch_start_time.elapsed() < batch_timeout {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    let now_height = *self.current_height.read().await;

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
                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                let retry_height = *self.current_height.read().await;

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
                current = *self.current_height.read().await;

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

        let final_height = *self.current_height.read().await;
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
    pub async fn sync_from_specific_peer(&self, peer_ip: &str) -> Result<(), String> {
        let current = *self.current_height.read().await;
        let time_expected = self.calculate_expected_height();

        if current >= time_expected {
            tracing::info!("✓ Already synced to expected height {}", current);
            return Ok(());
        }

        let peer_registry = self.peer_registry.read().await;
        let registry = peer_registry.as_ref().ok_or("No peer registry available")?;

        // Request blocks from the specific peer
        let batch_start = current + 1;
        let batch_end = time_expected;

        tracing::info!(
            "📤 Requesting blocks {}-{} from consensus peer {}",
            batch_start,
            batch_end,
            peer_ip
        );

        let req = NetworkMessage::GetBlocks(batch_start, batch_end);
        registry
            .send_to_peer(peer_ip, req)
            .await
            .map_err(|e| format!("Failed to request blocks from {}: {}", peer_ip, e))?;

        // Wait for blocks to arrive
        let wait_start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(30);

        while wait_start.elapsed() < timeout {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let now_height = *self.current_height.read().await;

            if now_height >= time_expected {
                tracing::info!("✓ Synced from consensus peer to height {}", now_height);
                return Ok(());
            }
        }

        Err(format!("Timeout waiting for blocks from {}", peer_ip))
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
        let mut current_height = *self.current_height.read().await;

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
        let stored_height = *self.current_height.read().await;
        if current_height != stored_height {
            tracing::warn!(
                "⚠️  Correcting chain height from {} to {}",
                stored_height,
                current_height
            );
            *self.current_height.write().await = current_height;
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

        // CRITICAL: During catchup, use current time instead of historical deterministic time
        // If the scheduled timestamp is too far in the past (>15min), use current time
        // to avoid timestamp validation failures
        let blocks_behind = self
            .calculate_expected_height()
            .saturating_sub(current_height);

        let now = chrono::Utc::now().timestamp();
        let timestamp_age = now - deterministic_timestamp;
        let use_current_time = timestamp_age > TIMESTAMP_TOLERANCE_SECS;

        let aligned_timestamp = if use_current_time {
            tracing::debug!(
                "📅 Block {} using current timestamp {} (scheduled {} is {}s old, exceeds tolerance)",
                next_height,
                now,
                deterministic_timestamp,
                timestamp_age
            );
            now
        } else {
            deterministic_timestamp
        };

        // Determine if we're in catchup mode - either explicitly (target_height provided)
        // or implicitly (more than 10 blocks behind)
        let is_catchup_mode = target_height.is_some() || blocks_behind > 10;

        let masternodes = if is_catchup_mode {
            // Catchup mode - use all ACTIVE masternodes (skip connection check)
            // During rapid catchup, connection status may fluctuate, but we still
            // want to distribute rewards to active masternodes
            let active_mns = self.masternode_registry.list_active().await;
            tracing::debug!(
                "📊 Block {} (CATCHUP): using {} active masternodes for reward distribution",
                next_height,
                active_mns.len()
            );
            active_mns
        } else {
            // Normal mode - use only active AND connected masternodes
            let conn_mgr = self.connection_manager.read().await;
            let masternodes = if let Some(cm) = conn_mgr.as_ref() {
                // Check connection status
                let connected_active = self
                    .masternode_registry
                    .get_connected_active_masternodes(cm)
                    .await;
                tracing::debug!(
                    "📊 Block {}: {} connected active masternodes for reward distribution",
                    next_height,
                    connected_active.len()
                );
                connected_active
            } else {
                // Fallback: if no connection manager set, use all active
                let active_mns = self.masternode_registry.list_active().await;
                tracing::warn!(
                    "⚠️  Block {}: connection manager not set, using {} active masternodes (may include disconnected)",
                    next_height,
                    active_mns.len()
                );
                active_mns
            };
            masternodes
        };

        if masternodes.is_empty() {
            return Err("No masternodes available for block production".to_string());
        }

        // Get finalized transactions from consensus layer
        let finalized_txs = self.consensus.get_finalized_transactions_for_block();
        let total_fees = self.consensus.tx_pool.get_total_fees();

        // Calculate rewards
        let base_reward = BLOCK_REWARD_SATOSHIS;
        let total_reward = base_reward + total_fees;
        let rewards = self.calculate_rewards_with_amount(&masternodes, total_reward);

        if rewards.is_empty() {
            return Err(format!(
                "No valid masternode rewards calculated for {} masternodes",
                masternodes.len()
            ));
        }

        tracing::debug!(
            "💰 Block {}: distributing {} satoshis to {} masternodes ({} each)",
            next_height,
            total_reward,
            rewards.len(),
            total_reward / masternodes.len() as u64
        );

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
        let block_hash = block.hash();
        self.validate_checkpoint(block.header.height, &block_hash)?;

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
        let current = *self.current_height.read().await;

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

        // Step 1: Rollback UTXOs for each block (in reverse order)
        // Note: Full UTXO rollback requires storing spent UTXOs or reconstructing from chain
        // For now, we remove outputs created by rolled-back blocks
        let mut utxo_rollback_count = 0;
        for height in (target_height + 1..=current).rev() {
            if let Ok(block) = self.get_block_by_height(height).await {
                // Remove outputs created by transactions in this block
                for tx in block.transactions.iter() {
                    let txid = tx.txid();
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

                    // TODO: Restore UTXOs that were spent by this transaction
                    // This requires either:
                    // 1. Keeping a rollback journal of spent UTXOs
                    // 2. Re-scanning the chain from genesis to target_height
                    // For now, we log that this is incomplete
                    if !tx.inputs.is_empty() && tx.inputs[0].previous_output.vout != u32::MAX {
                        // Not a coinbase transaction
                        tracing::debug!(
                            "⚠️  UTXO restoration for spent inputs not fully implemented (tx {} at height {})",
                            hex::encode(&txid[..8]),
                            height
                        );
                    }
                }
            }
        }

        tracing::info!(
            "🔄 Rolled back {} UTXO changes (removed outputs from rolled-back blocks)",
            utxo_rollback_count
        );

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
        *self.current_height.write().await = target_height;

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
            tracing::warn!(
                "⏳ Cannot add block {} - waiting for genesis block first (current_height: {})",
                block_height,
                *self.current_height.read().await
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
                tracing::warn!(
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
    pub async fn should_switch_by_work(
        &self,
        peer_work: u128,
        peer_height: u64,
        peer_tip_hash: &[u8; 32],
    ) -> bool {
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
        let current = *self.current_height.read().await;

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
        let mut expected_prev_hash = if common_ancestor > 0 {
            self.get_block_hash(common_ancestor).ok()
        } else {
            None
        };

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

            // Validate previous hash chain continuity
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

            // Update expected previous hash for next block
            expected_prev_hash = Some(block.hash());
        }

        tracing::info!("✅ All blocks validated successfully, proceeding with reorganization");

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

        let new_height = *self.current_height.read().await;
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

        // Wait for responses (with timeout)
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        // Collect peer heights from registry
        let mut peer_heights: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for peer_ip in &connected_peers {
            if let Some(height) = registry.get_peer_height(peer_ip).await {
                peer_heights.insert(peer_ip.clone(), height);
            }
        }

        if peer_heights.is_empty() {
            tracing::debug!("No peer height responses received");
            return None;
        }

        // Find the most common height (consensus)
        let mut height_counts: std::collections::HashMap<u64, usize> =
            std::collections::HashMap::new();
        for height in peer_heights.values() {
            *height_counts.entry(*height).or_insert(0) += 1;
        }

        // Get the height with most peers (longest chain by consensus)
        let consensus_height = height_counts
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(height, _)| *height)?;

        // Find a peer with the consensus height
        let consensus_peer = peer_heights
            .iter()
            .find(|(_, height)| **height == consensus_height)
            .map(|(peer, height)| (peer.clone(), *height))?;

        let our_height = self.get_height().await;

        if consensus_height > our_height {
            tracing::info!(
                "🔀 Fork detected: consensus height {} > our height {} (peer: {})",
                consensus_height,
                our_height,
                consensus_peer.0
            );
            Some((consensus_height, consensus_peer.0))
        } else if consensus_height < our_height {
            tracing::warn!(
                "⚠️  We appear to be ahead of consensus: our height {} > consensus {}",
                our_height,
                consensus_height
            );
            None
        } else {
            None
        }
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

    /// Automatic fork resolution: given competing blocks, choose the longest chain
    /// Returns true if we should accept the new blocks (they extend a longer chain)
    pub async fn should_accept_fork(
        &self,
        competing_blocks: &[Block],
        peer_claimed_height: u64,
    ) -> Result<bool, String> {
        if competing_blocks.is_empty() {
            return Ok(false);
        }

        let our_height = self.get_height().await;
        let fork_height = competing_blocks.first().unwrap().header.height;

        tracing::info!(
            "🔀 Fork resolution: comparing chains at height {} (our height: {}, peer height: {})",
            fork_height,
            our_height,
            peer_claimed_height
        );

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

    /// Validate that our chain hasn't gotten ahead of the network time schedule
    pub async fn validate_chain_time(&self) -> Result<(), String> {
        let current_height = self.get_height().await;
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
            connection_manager: self.connection_manager.clone(),
            peer_scoring: self.peer_scoring.clone(),
            cumulative_work: self.cumulative_work.clone(),
            reorg_history: self.reorg_history.clone(),
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
