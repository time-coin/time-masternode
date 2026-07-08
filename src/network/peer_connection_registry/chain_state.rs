use super::types::{extract_ip, ChainTip};
use super::PeerConnectionRegistry;
use std::sync::Arc;

impl PeerConnectionRegistry {
    /// Reset fork error count for a peer (called when blocks are successfully added)
    pub fn reset_fork_errors(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip);
        if self.fork_error_counts.remove(ip_only).is_some() {
            tracing::debug!(
                "Reset fork error count for peer {} (blocks accepted)",
                ip_only
            );
        }
    }

    /// Increment fork error count and return the new count
    pub fn increment_fork_errors(&self, peer_ip: &str) -> u32 {
        let ip_only = extract_ip(peer_ip).to_string();
        let count = self
            .fork_error_counts
            .entry(ip_only)
            .and_modify(|c| *c += 1)
            .or_insert(1);
        *count
    }
    /// Set a peer's reported blockchain height
    pub async fn set_peer_height(&self, peer_ip: &str, height: u64) {
        let ip_only = extract_ip(peer_ip);
        let mut heights = self.peer_heights.write().await;
        heights.insert(ip_only.to_string(), height);
    }

    /// Get a peer's reported blockchain height
    pub async fn get_peer_height(&self, peer_ip: &str) -> Option<u64> {
        let ip_only = extract_ip(peer_ip);
        let heights = self.peer_heights.read().await;
        heights.get(ip_only).copied()
    }

    /// Set the software commit count reported by a peer during handshake
    pub async fn set_peer_commit_count(&self, peer_ip: &str, commit_count: u32) {
        let ip_only = extract_ip(peer_ip);
        let mut counts = self.peer_commit_counts.write().await;
        counts.insert(ip_only.to_string(), commit_count);
    }

    /// Get the software commit count reported by a peer during handshake
    pub async fn get_peer_commit_count(&self, peer_ip: &str) -> Option<u32> {
        let ip_only = extract_ip(peer_ip);
        let counts = self.peer_commit_counts.read().await;
        counts.get(ip_only).copied()
    }

    /// Set a peer's latest ping RTT in seconds
    pub async fn set_peer_ping_time(&self, peer_ip: &str, rtt_secs: f64) {
        let ip_only = extract_ip(peer_ip);
        let mut times = self.peer_ping_times.write().await;
        times.insert(ip_only.to_string(), rtt_secs);
    }

    /// Get a peer's latest ping RTT in seconds
    pub async fn get_peer_ping_time(&self, peer_ip: &str) -> Option<f64> {
        let ip_only = extract_ip(peer_ip);
        let times = self.peer_ping_times.read().await;
        times.get(ip_only).copied()
    }

    /// Record that a ping was sent to a peer (for centralized RTT tracking)
    pub async fn record_ping_sent(&self, peer_ip: &str, nonce: u64) {
        let ip_only = extract_ip(peer_ip).to_string();
        let mut pings = self.pending_pings.write().await;
        let entry = pings.entry(ip_only).or_default();
        entry.push((nonce, std::time::Instant::now()));
        // Keep at most 10 pending pings per peer
        if entry.len() > 10 {
            entry.remove(0);
        }
    }

    /// Record that a pong was received, compute and store RTT
    pub async fn record_pong_received(&self, peer_ip: &str, nonce: u64) {
        let ip_only = extract_ip(peer_ip).to_string();
        let now = std::time::Instant::now();
        let mut pings = self.pending_pings.write().await;
        if let Some(pending) = pings.get_mut(&ip_only) {
            if let Some(pos) = pending.iter().position(|(n, _)| *n == nonce) {
                let (_, sent_time) = pending.remove(pos);
                let rtt_secs = now.duration_since(sent_time).as_secs_f64() / 2.0;
                drop(pings); // Release lock before acquiring another
                let mut times = self.peer_ping_times.write().await;
                times.insert(ip_only, rtt_secs);
            }
        }
    }

    /// Phase 3: Update a peer's known height
    pub async fn update_peer_height(&self, peer_ip: &str, height: u64) {
        let ip_only = extract_ip(peer_ip);
        let mut heights = self.peer_heights.write().await;
        heights.insert(ip_only.to_string(), height);
    }

    /// Update a peer's chain tip (height + hash)
    /// Only updates if the new height is >= the cached height (monotonic),
    /// preventing stale ChainTipResponse from overwriting a newer forced update.
    pub async fn update_peer_chain_tip(&self, peer_ip: &str, height: u64, hash: [u8; 32]) {
        let ip_only = extract_ip(peer_ip);
        let mut tips = self.peer_chain_tips.write().await;
        if let Some(&(existing_height, _)) = tips.get(ip_only) {
            if height < existing_height {
                tracing::debug!(
                    "🔄 Ignoring stale chain tip for {} (cached: {}, received: {})",
                    ip_only,
                    existing_height,
                    height
                );
                return;
            }
        }
        tips.insert(ip_only.to_string(), (height, hash));
        drop(tips);
        // Keep a time-stamped copy that survives disconnect for up to 5 minutes.
        let mut recent = self.recent_chain_tip_cache.write().await;
        let should_update = recent
            .get(ip_only)
            .map_or(true, |(cached_h, _, _)| height >= *cached_h);
        if should_update {
            recent.insert(
                ip_only.to_string(),
                (height, hash, std::time::Instant::now()),
            );
        }
        drop(recent);
        self.chain_tip_updated.notify_waiters();
    }

    /// Get a peer's chain tip (height + hash)
    pub async fn get_peer_chain_tip(&self, peer_ip: &str) -> Option<ChainTip> {
        let ip_only = extract_ip(peer_ip);
        let tips = self.peer_chain_tips.read().await;
        tips.get(ip_only).copied()
    }

    /// Returns chain tips from peers seen in the last `max_age_secs` seconds, including
    /// recently-disconnected peers. Used by compare_chain_with_peers() to count fork evidence
    /// from peers that disconnected immediately after detecting a fork (minority-fork trap).
    pub async fn get_recent_chain_tips(&self, max_age_secs: u64) -> Vec<(String, u64, [u8; 32])> {
        let now = std::time::Instant::now();
        let cache = self.recent_chain_tip_cache.read().await;
        cache
            .iter()
            .filter(|(_, (_, _, t))| {
                now.checked_duration_since(*t)
                    .is_some_and(|age| age.as_secs() <= max_age_secs)
            })
            .map(|(ip, (h, hash, _))| (ip.clone(), *h, *hash))
            .collect()
    }

    /// Get the chain tip update signal (notified when any peer reports a new chain tip)
    pub fn chain_tip_updated_signal(&self) -> Arc<tokio::sync::Notify> {
        self.chain_tip_updated.clone()
    }

    /// Clear stale peer data when peer disconnects
    pub async fn clear_peer_data(&self, peer_ip: &str) {
        let ip_only = extract_ip(peer_ip).to_string();
        let mut heights = self.peer_heights.write().await;
        let mut tips = self.peer_chain_tips.write().await;
        let mut ping_times = self.peer_ping_times.write().await;
        let mut pings = self.pending_pings.write().await;
        heights.remove(peer_ip);
        tips.remove(peer_ip);
        ping_times.remove(peer_ip);
        pings.remove(peer_ip);
        self.peer_commit_counts.write().await.remove(peer_ip);
        // Remove genesis confirmation so reconnecting peer is re-verified
        self.genesis_confirmed_peers.write().await.remove(&ip_only);
        // Clear cooldown on disconnect so the peer gets a fresh check on reconnect
        self.clear_genesis_check_cooldown(&ip_only);
        tracing::debug!(
            "🧹 Cleared stale chain tip data for disconnected peer {}",
            peer_ip
        );
    }
}
