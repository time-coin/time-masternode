//! Sync Coordinator - Prevents duplicate sync requests and sync storms
//!
//! This module coordinates all blockchain synchronization requests across the network
//! to prevent multiple simultaneous sync operations to the same peer.
//!
//! ## Problem Being Solved
//! Before this coordinator:
//! - Opportunistic sync could fire 5-10 times for a single ChainTipResponse
//! - Multiple sync sources (periodic, opportunistic, fork-triggered) didn't coordinate
//! - No throttling led to "No sync progress" spam in logs
//!
//! ## Solution
//! - Track active sync operations per peer
//! - Throttle sync requests (max 1 per 60 seconds per peer)
//! - Queue sync requests when one is already active
//! - Coordinate between all sync triggers

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Minimum time between sync requests to the same peer
const SYNC_THROTTLE_DURATION: Duration = Duration::from_secs(60);

/// Maximum concurrent syncs across all peers
const MAX_CONCURRENT_SYNCS: usize = 3;

/// Represents an active or queued sync request
#[derive(Debug, Clone)]
pub struct SyncRequest {
    /// Peer IP address
    pub peer_ip: String,

    /// Starting block height
    pub start_height: u64,

    /// Ending block height
    pub end_height: u64,

    /// Source that triggered this sync
    pub source: SyncSource,

    /// When this request was created
    pub created_at: Instant,
}

/// Source that triggered a sync request
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncSource {
    /// Periodic chain comparison (blockchain.rs)
    Periodic,

    /// Opportunistic sync from ChainTipResponse
    Opportunistic,

    /// Fork resolution triggered sync
    ForkResolution,

    /// Manual/explicit sync request
    Manual,
}

impl std::fmt::Display for SyncSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncSource::Periodic => write!(f, "Periodic"),
            SyncSource::Opportunistic => write!(f, "Opportunistic"),
            SyncSource::ForkResolution => write!(f, "ForkResolution"),
            SyncSource::Manual => write!(f, "Manual"),
        }
    }
}

/// Tracks sync state for a single peer
#[derive(Debug, Clone)]
struct PeerSyncState {
    /// Currently active sync request (if any)
    active_sync: Option<SyncRequest>,

    /// Last time a sync completed for this peer
    last_sync_completed: Option<Instant>,

    /// Queued sync requests for this peer
    queued_syncs: Vec<SyncRequest>,
}

impl PeerSyncState {
    fn new() -> Self {
        Self {
            active_sync: None,
            last_sync_completed: None,
            queued_syncs: Vec::new(),
        }
    }
}

/// Centralized sync coordinator
pub struct SyncCoordinator {
    /// Per-peer sync state
    peer_states: Arc<RwLock<HashMap<String, PeerSyncState>>>,

    /// Total active syncs across all peers
    active_sync_count: Arc<RwLock<usize>>,
}

impl SyncCoordinator {
    /// Create a new sync coordinator
    pub fn new() -> Self {
        Self {
            peer_states: Arc::new(RwLock::new(HashMap::new())),
            active_sync_count: Arc::new(RwLock::new(0)),
        }
    }

    /// Request a sync operation, subject to throttling
    ///
    /// Returns:
    /// - `Ok(true)` - Sync approved and started
    /// - `Ok(false)` - Sync throttled/queued
    /// - `Err(msg)` - Sync rejected with reason
    pub async fn request_sync(
        &self,
        peer_ip: String,
        start_height: u64,
        end_height: u64,
        source: SyncSource,
    ) -> Result<bool, String> {
        let mut states = self.peer_states.write().await;
        let state = states
            .entry(peer_ip.clone())
            .or_insert_with(PeerSyncState::new);

        // Check if sync with this peer is already active
        if let Some(active) = &state.active_sync {
            // Auto-expire stale syncs that never completed (e.g., peer went offline)
            if active.created_at.elapsed() > Duration::from_secs(60) {
                warn!(
                    "⚠️  Auto-expiring stale sync with {} (started {:?} ago, heights {}-{})",
                    peer_ip,
                    active.created_at.elapsed(),
                    active.start_height,
                    active.end_height
                );
                state.active_sync = None;
                let mut active_count = self.active_sync_count.write().await;
                *active_count = active_count.saturating_sub(1);
                // Fall through to approve new sync below
            } else {
                debug!(
                    "🔄 Sync already active with {} (heights {}-{}, source: {}), queuing new request",
                    peer_ip, active.start_height, active.end_height, active.source
                );

                // Queue this request for later
                state.queued_syncs.push(SyncRequest {
                    peer_ip: peer_ip.clone(),
                    start_height,
                    end_height,
                    source,
                    created_at: Instant::now(),
                });

                return Ok(false);
            }
        }

        // Check throttling - did we sync with this peer recently?
        if let Some(last_sync) = state.last_sync_completed {
            let elapsed = last_sync.elapsed();
            if elapsed < SYNC_THROTTLE_DURATION {
                let remaining = SYNC_THROTTLE_DURATION - elapsed;
                debug!(
                    "⏱️ Throttling sync with {} - last sync was {:?} ago, need to wait {:?}",
                    peer_ip, elapsed, remaining
                );
                return Err(format!(
                    "Throttled: synced {:?} ago, wait {:?}",
                    elapsed, remaining
                ));
            }
        }

        // Check global concurrent sync limit
        let mut active_count = self.active_sync_count.write().await;
        if *active_count >= MAX_CONCURRENT_SYNCS {
            debug!(
                "⏸️ Max concurrent syncs ({}) reached, queuing sync with {}",
                MAX_CONCURRENT_SYNCS, peer_ip
            );

            state.queued_syncs.push(SyncRequest {
                peer_ip: peer_ip.clone(),
                start_height,
                end_height,
                source,
                created_at: Instant::now(),
            });

            return Ok(false);
        }

        // Approve sync!
        state.active_sync = Some(SyncRequest {
            peer_ip: peer_ip.clone(),
            start_height,
            end_height,
            source,
            created_at: Instant::now(),
        });

        *active_count += 1;

        info!(
            "✅ Sync approved with {} (heights {}-{}, source: {}) - {} active syncs",
            peer_ip, start_height, end_height, source, *active_count
        );

        Ok(true)
    }

    /// Mark a sync as completed, possibly starting queued syncs
    pub async fn complete_sync(&self, peer_ip: &str) {
        let mut states = self.peer_states.write().await;

        if let Some(state) = states.get_mut(peer_ip) {
            if let Some(active) = state.active_sync.take() {
                // Update completion time
                state.last_sync_completed = Some(Instant::now());

                // Decrement global counter
                let mut active_count = self.active_sync_count.write().await;
                *active_count = active_count.saturating_sub(1);

                info!(
                    "✅ Sync completed with {} (heights {}-{}, duration: {:?}) - {} active syncs remaining",
                    peer_ip,
                    active.start_height,
                    active.end_height,
                    active.created_at.elapsed(),
                    *active_count
                );

                // Check if there are queued syncs for this peer
                if !state.queued_syncs.is_empty() {
                    debug!(
                        "📋 Peer {} has {} queued syncs, will process after throttle period",
                        peer_ip,
                        state.queued_syncs.len()
                    );
                }
            } else {
                warn!(
                    "⚠️ complete_sync called for {} but no active sync found",
                    peer_ip
                );
            }
        }
    }

    /// Cancel an active sync (e.g., peer disconnected)
    pub async fn cancel_sync(&self, peer_ip: &str) {
        let mut states = self.peer_states.write().await;

        if let Some(state) = states.get_mut(peer_ip) {
            if let Some(active) = state.active_sync.take() {
                // Decrement global counter
                let mut active_count = self.active_sync_count.write().await;
                *active_count = active_count.saturating_sub(1);

                warn!(
                    "❌ Sync cancelled with {} (heights {}-{}, source: {})",
                    peer_ip, active.start_height, active.end_height, active.source
                );
            }

            // Clear queued syncs for disconnected peer
            state.queued_syncs.clear();
        }
    }

    /// Check if sync with a peer is currently active
    pub async fn is_sync_active(&self, peer_ip: &str) -> bool {
        let states = self.peer_states.read().await;
        states
            .get(peer_ip)
            .and_then(|s| s.active_sync.as_ref())
            .is_some()
    }

    /// Get statistics for monitoring
    pub async fn get_stats(&self) -> SyncCoordinatorStats {
        let states = self.peer_states.read().await;
        let active_count = *self.active_sync_count.read().await;

        let mut total_queued = 0;
        let mut peers_with_active_sync = 0;

        for state in states.values() {
            if state.active_sync.is_some() {
                peers_with_active_sync += 1;
            }
            total_queued += state.queued_syncs.len();
        }

        SyncCoordinatorStats {
            active_syncs: active_count,
            peers_with_active_sync,
            total_queued_syncs: total_queued,
            tracked_peers: states.len(),
        }
    }

    /// Process queued syncs (should be called periodically)
    ///
    /// This checks for queued syncs that can now be executed after
    /// throttle periods have expired
    pub async fn process_queued_syncs(&self) -> Vec<SyncRequest> {
        let mut states = self.peer_states.write().await;
        let active_count = self.active_sync_count.read().await;

        // Can't start new syncs if at limit
        if *active_count >= MAX_CONCURRENT_SYNCS {
            return Vec::new();
        }

        let mut ready_syncs = Vec::new();

        for (peer_ip, state) in states.iter_mut() {
            // Skip if already has active sync
            if state.active_sync.is_some() {
                continue;
            }

            // Check throttle
            if let Some(last_sync) = state.last_sync_completed {
                if last_sync.elapsed() < SYNC_THROTTLE_DURATION {
                    continue;
                }
            }

            // Check if there are queued syncs
            if let Some(queued) = state.queued_syncs.first().cloned() {
                // Remove from queue and mark as active
                state.queued_syncs.remove(0);
                state.active_sync = Some(queued.clone());

                info!(
                    "🔄 Processing queued sync with {} (heights {}-{}, source: {})",
                    peer_ip, queued.start_height, queued.end_height, queued.source
                );

                ready_syncs.push(queued);

                // Stop if we've reached the limit
                if ready_syncs.len() >= MAX_CONCURRENT_SYNCS - *active_count {
                    break;
                }
            }
        }

        ready_syncs
    }
}

impl Default for SyncCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about sync coordination
#[derive(Debug, Clone)]
pub struct SyncCoordinatorStats {
    pub active_syncs: usize,
    pub peers_with_active_sync: usize,
    pub total_queued_syncs: usize,
    pub tracked_peers: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sync_throttling() {
        let coordinator = SyncCoordinator::new();
        let peer = "192.168.1.1".to_string();

        // First sync should be approved
        let result = coordinator
            .request_sync(peer.clone(), 0, 100, SyncSource::Periodic)
            .await;
        assert_eq!(result, Ok(true));

        // Complete the sync
        coordinator.complete_sync(&peer).await;

        // Immediate second sync should be throttled
        let result = coordinator
            .request_sync(peer.clone(), 100, 200, SyncSource::Opportunistic)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_concurrent_sync_limit() {
        let coordinator = SyncCoordinator::new();

        // Start 3 syncs (the limit)
        for i in 0..3 {
            let peer = format!("192.168.1.{}", i + 1);
            let result = coordinator
                .request_sync(peer, 0, 100, SyncSource::Periodic)
                .await;
            assert_eq!(result, Ok(true));
        }

        // Fourth sync should be queued
        let result = coordinator
            .request_sync("192.168.1.4".to_string(), 0, 100, SyncSource::Periodic)
            .await;
        assert_eq!(result, Ok(false));
    }
}
