/// PHASE 3 PART 2: Synchronization Coordinator
///
/// Orchestrates network synchronization with consensus validation
/// ensuring nodes achieve and maintain consensus on the correct blockchain state
use crate::blockchain::Blockchain;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::network::state_sync::StateSyncManager;
use crate::peer_manager::PeerManager;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

#[allow(dead_code)]
const SYNC_CHECK_INTERVAL_SECS: u64 = 30;
#[allow(dead_code)]
const CONSENSUS_THRESHOLD: f64 = 0.666; // 2/3 majority

/// Coordinates synchronization between network and blockchain layers
#[allow(dead_code)]
pub struct SyncCoordinator {
    blockchain: Arc<RwLock<Option<Arc<Blockchain>>>>,
    state_sync: Arc<StateSyncManager>,
    peer_manager: Arc<RwLock<Option<Arc<PeerManager>>>>,
    peer_registry: Arc<RwLock<Option<Arc<PeerConnectionRegistry>>>>,
    sync_in_progress: Arc<RwLock<bool>>,
}

impl SyncCoordinator {
    #[allow(dead_code)]
    pub fn new(state_sync: Arc<StateSyncManager>) -> Self {
        Self {
            blockchain: Arc::new(RwLock::new(None)),
            state_sync,
            peer_manager: Arc::new(RwLock::new(None)),
            peer_registry: Arc::new(RwLock::new(None)),
            sync_in_progress: Arc::new(RwLock::new(false)),
        }
    }

    /// Set blockchain reference for sync operations
    #[allow(dead_code)]
    pub async fn set_blockchain(&self, blockchain: Arc<Blockchain>) {
        *self.blockchain.write().await = Some(blockchain);
    }

    /// Set peer manager reference
    #[allow(dead_code)]
    pub async fn set_peer_manager(&self, peer_manager: Arc<PeerManager>) {
        *self.peer_manager.write().await = Some(peer_manager);
    }

    /// Set peer registry reference
    #[allow(dead_code)]
    pub async fn set_peer_registry(&self, peer_registry: Arc<PeerConnectionRegistry>) {
        *self.peer_registry.write().await = Some(peer_registry);
    }

    /// Start synchronization loop
    #[allow(dead_code)]
    pub async fn start_sync_loop(&self) {
        let coordinator = self.clone();

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(SYNC_CHECK_INTERVAL_SECS));

            loop {
                interval.tick().await;

                if let Err(e) = coordinator.check_and_sync().await {
                    warn!("Sync check failed: {}", e);
                }
            }
        });

        info!("🔄 Sync coordinator started");
    }

    /// Check if sync is needed and perform synchronization
    #[allow(dead_code)]
    pub async fn check_and_sync(&self) -> Result<(), String> {
        // Prevent concurrent sync attempts
        if *self.sync_in_progress.read().await {
            return Ok(());
        }

        let blockchain = {
            let read = self.blockchain.read().await;
            read.as_ref()
                .cloned()
                .ok_or_else(|| "Blockchain not initialized".to_string())?
        };

        let peer_manager = {
            let read = self.peer_manager.read().await;
            read.as_ref()
                .cloned()
                .ok_or_else(|| "Peer manager not initialized".to_string())?
        };

        let peer_registry = {
            let read = self.peer_registry.read().await;
            read.as_ref()
                .cloned()
                .ok_or_else(|| "Peer registry not initialized".to_string())?
        };

        *self.sync_in_progress.write().await = true;

        let result = self
            .perform_sync(blockchain, peer_manager, peer_registry)
            .await;

        *self.sync_in_progress.write().await = false;

        result
    }

    /// Execute synchronization
    #[allow(dead_code)]
    async fn perform_sync(
        &self,
        blockchain: Arc<Blockchain>,
        peer_manager: Arc<PeerManager>,
        peer_registry: Arc<PeerConnectionRegistry>,
    ) -> Result<(), String> {
        self.state_sync.start_sync().await;

        // Step 1: Find best peer to sync from
        let best_peer = self
            .state_sync
            .select_best_sync_peer(&peer_manager, &peer_registry)
            .await?;

        info!("🎯 Selected peer {} for sync", best_peer);

        // Step 2: Verify network consensus on genesis (security check)
        if !self
            .verify_network_genesis_consensus(&peer_manager, &peer_registry)
            .await?
        {
            error!("❌ Network genesis consensus check failed!");
            self.state_sync.end_sync().await;
            return Err("Genesis consensus verification failed".to_string());
        }

        info!("✅ Genesis consensus verified");

        // Step 3: Request blocks from peer
        let current_height = blockchain.get_height().await;
        let expected_height = blockchain.calculate_expected_height();

        if current_height < expected_height {
            info!(
                "📥 Requesting blocks {} to {} from {}",
                current_height + 1,
                expected_height,
                best_peer
            );

            self.state_sync
                .request_blocks_redundant(
                    current_height + 1,
                    expected_height,
                    &peer_manager,
                    &peer_registry,
                )
                .await?;

            // Step 4: Wait for blocks with timeout
            self.wait_for_blocks(current_height, expected_height)
                .await?;
        }

        // Step 5: Verify state consistency across multiple peers
        if !self
            .verify_network_state_consistency(&peer_manager, &peer_registry)
            .await?
        {
            warn!("⚠️ State consistency check failed - possible fork");
            self.state_sync.end_sync().await;
            return Err("State consistency verification failed".to_string());
        }

        info!("✅ Network state consistency verified");

        self.state_sync.end_sync().await;
        info!("✅ Synchronization completed successfully");

        Ok(())
    }

    /// Verify all peers have same genesis block (security critical)
    #[allow(dead_code)]
    async fn verify_network_genesis_consensus(
        &self,
        peer_manager: &Arc<PeerManager>,
        peer_registry: &Arc<PeerConnectionRegistry>,
    ) -> Result<bool, String> {
        let peers = peer_manager.get_all_peers().await;

        if peers.is_empty() {
            return Ok(true); // No peers to verify against
        }

        let mut genesis_votes = std::collections::HashMap::new();

        for peer in peers.iter().take(5) {
            match self.state_sync.query_peer_state(peer, peer_registry).await {
                Ok(state) => {
                    *genesis_votes.entry(state.genesis_hash).or_insert(0) += 1;
                }
                Err(e) => {
                    warn!("Failed to query genesis from {}: {}", peer, e);
                }
            }
        }

        let total_votes: u32 = genesis_votes.values().sum();
        if total_votes == 0 {
            return Err("No genesis responses from peers".to_string());
        }

        // All peers should have same genesis
        let consensus = genesis_votes.values().all(|&v| v == total_votes);

        if consensus {
            info!("✅ All {} peer(s) agree on genesis", total_votes);
            Ok(true)
        } else {
            error!("❌ Genesis mismatch across peers - possible network split!");
            Ok(false)
        }
    }

    /// Verify blockchain state is consistent across peers
    #[allow(dead_code)]
    async fn verify_network_state_consistency(
        &self,
        peer_manager: &Arc<PeerManager>,
        peer_registry: &Arc<PeerConnectionRegistry>,
    ) -> Result<bool, String> {
        let peers = peer_manager.get_all_peers().await;

        if peers.is_empty() {
            return Ok(true);
        }

        let mut height_votes = std::collections::HashMap::new();

        // Query height from multiple peers
        for peer in peers.iter().take(5) {
            match self.state_sync.query_peer_state(peer, peer_registry).await {
                Ok(state) => {
                    *height_votes.entry(state.height).or_insert(0) += 1;
                }
                Err(e) => {
                    debug!("Failed to query height from {}: {}", peer, e);
                }
            }
        }

        let total_votes: u32 = height_votes.values().sum();
        if total_votes == 0 {
            return Err("No height responses from peers".to_string());
        }

        // Check if consensus height is clear (2/3+)
        let consensus_threshold = (total_votes as f64 * CONSENSUS_THRESHOLD) as u32;

        for (&height, &votes) in &height_votes {
            if votes >= consensus_threshold {
                info!(
                    "✅ Network height consensus: {} ({}/{} peers)",
                    height, votes, total_votes
                );
                return Ok(true);
            }
        }

        warn!(
            "⚠️ No clear consensus on blockchain height: {:?}",
            height_votes
        );
        Ok(false)
    }

    /// Wait for blocks to arrive from peers
    #[allow(dead_code)]
    async fn wait_for_blocks(&self, _start_height: u64, _target_height: u64) -> Result<(), String> {
        let timeout = tokio::time::Duration::from_secs(60);
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            let pending = self.state_sync.pending_block_count().await;

            if pending == 0 {
                info!("✅ All blocks received");
                return Ok(());
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        }

        Err(format!(
            "Block fetch timeout after {}s - {} blocks still pending",
            timeout.as_secs(),
            self.state_sync.pending_block_count().await
        ))
    }

    /// Force manual sync check
    #[allow(dead_code)]
    pub async fn manual_sync(&self) -> Result<(), String> {
        self.check_and_sync().await
    }

    /// Get sync status
    #[allow(dead_code)]
    pub async fn is_syncing(&self) -> bool {
        self.state_sync.is_syncing().await || *self.sync_in_progress.read().await
    }
}

impl Clone for SyncCoordinator {
    fn clone(&self) -> Self {
        Self {
            blockchain: self.blockchain.clone(),
            state_sync: self.state_sync.clone(),
            peer_manager: self.peer_manager.clone(),
            peer_registry: self.peer_registry.clone(),
            sync_in_progress: self.sync_in_progress.clone(),
        }
    }
}
