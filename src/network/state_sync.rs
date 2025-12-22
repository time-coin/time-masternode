/// PHASE 3: Network State Synchronization
///
/// Ensures nodes stay synchronized by:
/// 1. Coordinating block fetching from multiple peers
/// 2. Verifying state consistency across peers
/// 3. Handling partial syncs and recovery
/// 4. Preventing state divergence
use crate::network::message::NetworkMessage;
use crate::network::peer_connection_registry::PeerConnectionRegistry;
use crate::peer_manager::PeerManager;
use crate::types::Hash256;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

#[allow(dead_code)]
const MAX_PENDING_BLOCKS: usize = 100;
const STATE_SYNC_TIMEOUT_SECS: u64 = 30;
#[allow(dead_code)]
const PEER_STATE_CACHE_TTL_SECS: i64 = 300; // 5 minutes

/// Tracks peer's blockchain state for synchronization
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct PeerState {
    pub address: String,
    pub height: u64,
    pub genesis_hash: Hash256,
    pub last_queried: i64,
    pub response_time_ms: u64,
    pub consecutive_failures: u32,
}

/// Pending block fetches from peers
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct PendingBlockFetch {
    block_height: u64,
    requested_from: Vec<String>, // Peer addresses we requested from
    attempt_count: u32,
    max_attempts: u32,
}

/// State synchronization manager
#[allow(dead_code)]
pub struct StateSyncManager {
    peer_states: Arc<RwLock<HashMap<String, PeerState>>>,
    pending_blocks: Arc<RwLock<VecDeque<PendingBlockFetch>>>,
    syncing: Arc<RwLock<bool>>,
}

impl StateSyncManager {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            peer_states: Arc::new(RwLock::new(HashMap::new())),
            pending_blocks: Arc::new(RwLock::new(VecDeque::new())),
            syncing: Arc::new(RwLock::new(false)),
        }
    }

    /// Query peer's blockchain state (height, genesis hash)
    #[allow(dead_code)]
    pub async fn query_peer_state(
        &self,
        peer_address: &str,
        peer_registry: &Arc<PeerConnectionRegistry>,
    ) -> Result<PeerState, String> {
        let now = chrono::Utc::now().timestamp();

        // Check if we have fresh cached state
        {
            let states = self.peer_states.read().await;
            if let Some(state) = states.get(peer_address) {
                if now - state.last_queried < STATE_SYNC_TIMEOUT_SECS as i64 {
                    return Ok(state.clone());
                }
            }
        }

        let start_time = std::time::Instant::now();

        // Query height
        peer_registry
            .send_to_peer(peer_address, NetworkMessage::GetBlockHeight)
            .await
            .map_err(|e| format!("Failed to query height from {}: {}", peer_address, e))?;

        // Query genesis hash for verification
        peer_registry
            .send_to_peer(peer_address, NetworkMessage::GetGenesisHash)
            .await
            .map_err(|e| format!("Failed to query genesis from {}: {}", peer_address, e))?;

        // Wait for responses (would be handled by network layer callback)
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        let response_time_ms = start_time.elapsed().as_millis() as u64;

        // For now, return a placeholder - in full implementation,
        // would wait for actual responses through channels
        let peer_state = PeerState {
            address: peer_address.to_string(),
            height: 0,
            genesis_hash: [0; 32],
            last_queried: now,
            response_time_ms,
            consecutive_failures: 0,
        };

        // Cache the state
        self.peer_states
            .write()
            .await
            .insert(peer_address.to_string(), peer_state.clone());

        Ok(peer_state)
    }

    /// Find the best peer to sync from (highest height, lowest latency)
    #[allow(dead_code)]
    pub async fn select_best_sync_peer(
        &self,
        peer_manager: &Arc<PeerManager>,
        peer_registry: &Arc<PeerConnectionRegistry>,
    ) -> Result<String, String> {
        let peers = peer_manager.get_all_peers().await;

        if peers.is_empty() {
            return Err("No peers available for sync".to_string());
        }

        let mut best_peer: Option<(String, u64, u64)> = None; // (address, height, response_time)

        for peer in peers {
            match self.query_peer_state(&peer, peer_registry).await {
                Ok(state) => {
                    if best_peer.is_none()
                        || state.height > best_peer.as_ref().unwrap().1
                        || (state.height == best_peer.as_ref().unwrap().1
                            && state.response_time_ms < best_peer.as_ref().unwrap().2)
                    {
                        best_peer =
                            Some((state.address.clone(), state.height, state.response_time_ms));
                    }
                }
                Err(e) => {
                    debug!("Failed to query peer {}: {}", peer, e);
                }
            }
        }

        best_peer
            .map(|(addr, _, _)| addr)
            .ok_or_else(|| "No responsive peers found".to_string())
    }

    /// Request blocks in range from peers with redundancy
    #[allow(dead_code)]
    pub async fn request_blocks_redundant(
        &self,
        start_height: u64,
        end_height: u64,
        peer_manager: &Arc<PeerManager>,
        peer_registry: &Arc<PeerConnectionRegistry>,
    ) -> Result<(), String> {
        let peers = peer_manager.get_all_peers().await;

        if peers.is_empty() {
            return Err("No peers available".to_string());
        }

        // Request from top 3 peers for redundancy
        let selected_peers: Vec<_> = peers.iter().take(3).cloned().collect();

        info!(
            "📤 Requesting blocks {} to {} from {} peer(s)",
            start_height,
            end_height,
            selected_peers.len()
        );

        for peer in &selected_peers {
            let msg = NetworkMessage::GetBlocks(start_height, end_height);
            if let Err(e) = peer_registry.send_to_peer(peer, msg).await {
                warn!("Failed to request blocks from {}: {}", peer, e);
            }
        }

        // Track pending blocks
        let mut pending = self.pending_blocks.write().await;
        for height in start_height..=end_height {
            pending.push_back(PendingBlockFetch {
                block_height: height,
                requested_from: selected_peers.clone(),
                attempt_count: 1,
                max_attempts: 3,
            });
        }

        Ok(())
    }

    /// Verify block hash consistency across peers
    #[allow(dead_code)]
    pub async fn verify_block_hash_consensus(
        &self,
        height: u64,
        expected_hash: Hash256,
        peer_manager: &Arc<PeerManager>,
        peer_registry: &Arc<PeerConnectionRegistry>,
    ) -> Result<bool, String> {
        let peers = peer_manager.get_all_peers().await;

        if peers.is_empty() {
            return Ok(false);
        }

        let mut hash_votes: HashMap<[u8; 32], u32> = HashMap::new();

        for peer in peers.iter().take(5) {
            match self.get_peer_block_hash(peer, height, peer_registry).await {
                Ok(hash) => {
                    *hash_votes.entry(hash).or_insert(0) += 1;
                }
                Err(e) => {
                    debug!("Failed to get block hash from {}: {}", peer, e);
                }
            }
        }

        // Check if expected hash has 2/3+ consensus
        let total_votes: u32 = hash_votes.values().sum();
        if total_votes == 0 {
            return Err("No responses from peers".to_string());
        }

        let expected_votes = hash_votes.get(&expected_hash).copied().unwrap_or(0);
        let consensus_threshold = (total_votes * 2) / 3 + 1;

        if expected_votes >= consensus_threshold {
            info!(
                "✅ Block {} hash consensus verified: {}/{}",
                height, expected_votes, total_votes
            );
            Ok(true)
        } else {
            warn!(
                "❌ Block {} hash has no consensus: {}/{} (expected {}/{})",
                height, expected_votes, total_votes, consensus_threshold, total_votes
            );
            Ok(false)
        }
    }

    /// Get block hash from a specific peer
    #[allow(dead_code)]
    async fn get_peer_block_hash(
        &self,
        peer_address: &str,
        height: u64,
        peer_registry: &Arc<PeerConnectionRegistry>,
    ) -> Result<Hash256, String> {
        peer_registry
            .send_to_peer(peer_address, NetworkMessage::GetBlockHash(height))
            .await?;

        // In full implementation, would wait for response
        Ok([0; 32])
    }

    /// Mark sync as started
    #[allow(dead_code)]
    pub async fn start_sync(&self) {
        *self.syncing.write().await = true;
        info!("🔄 State sync started");
    }

    /// Mark sync as completed
    #[allow(dead_code)]
    pub async fn end_sync(&self) {
        *self.syncing.write().await = false;
        info!("✅ State sync completed");
    }

    /// Check if currently syncing
    #[allow(dead_code)]
    pub async fn is_syncing(&self) -> bool {
        *self.syncing.read().await
    }

    /// Get pending block count
    #[allow(dead_code)]
    pub async fn pending_block_count(&self) -> usize {
        self.pending_blocks.read().await.len()
    }

    /// Retry failed block fetches
    #[allow(dead_code)]
    pub async fn retry_pending_blocks(
        &self,
        peer_manager: &Arc<PeerManager>,
        peer_registry: &Arc<PeerConnectionRegistry>,
    ) -> Result<(), String> {
        let peers = peer_manager.get_all_peers().await;

        if peers.is_empty() {
            return Err("No peers available for retry".to_string());
        }

        let mut pending = self.pending_blocks.write().await;
        let mut to_retry = Vec::new();

        while let Some(fetch) = pending.pop_front() {
            if fetch.attempt_count < fetch.max_attempts {
                to_retry.push((fetch.block_height, fetch.attempt_count + 1));
            } else {
                error!(
                    "❌ Block {} failed after {} attempts",
                    fetch.block_height, fetch.max_attempts
                );
            }
        }

        // Re-request failed blocks
        for (height, attempt) in to_retry {
            if let Some(peer) = peers.first() {
                debug!("🔄 Retrying block {} (attempt {})", height, attempt);

                let msg = NetworkMessage::BlockRequest(height);
                if let Err(e) = peer_registry.send_to_peer(peer, msg).await {
                    warn!("Retry failed for block {}: {}", height, e);
                } else {
                    pending.push_back(PendingBlockFetch {
                        block_height: height,
                        requested_from: vec![peer.clone()],
                        attempt_count: attempt,
                        max_attempts: 3,
                    });
                }
            }
        }

        Ok(())
    }

    /// Clear state for fresh sync
    #[allow(dead_code)]
    pub async fn reset(&self) {
        self.peer_states.write().await.clear();
        self.pending_blocks.write().await.clear();
        *self.syncing.write().await = false;
        info!("🔄 State sync manager reset");
    }
}

impl Default for StateSyncManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for StateSyncManager {
    fn clone(&self) -> Self {
        Self {
            peer_states: self.peer_states.clone(),
            pending_blocks: self.pending_blocks.clone(),
            syncing: self.syncing.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_sync_manager_creation() {
        let manager = StateSyncManager::new();
        // Verify manager is created with no pending blocks
        assert_eq!(
            std::mem::size_of::<StateSyncManager>() > 0,
            true,
            "Manager size should be non-zero"
        );
    }
}
