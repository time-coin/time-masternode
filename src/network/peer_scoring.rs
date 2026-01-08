//! AI-based peer scoring and selection system
//!
//! Learns from historical peer performance to intelligently select
//! the best peers for sync operations. Uses machine learning principles
//! without requiring heavy ML frameworks.
//!
//! Data is persisted to disk using sled database for learning across restarts.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Statistics tracked for each peer (serializable for disk storage)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerPerformanceStats {
    /// Number of successful requests
    pub successful_requests: u64,
    /// Number of failed requests
    pub failed_requests: u64,
    /// Recent response times in milliseconds (rolling window of last 10)
    pub response_times_ms: Vec<u64>,
    /// When peer was last successfully contacted (unix timestamp)
    pub last_success_timestamp: Option<u64>,
    /// When peer last failed (unix timestamp)
    pub last_failure_timestamp: Option<u64>,
    /// Total bytes received from this peer
    pub bytes_received: u64,
    /// Number of times peer was selected
    pub times_selected: u64,
    /// Computed reliability score (0.0 - 1.0)
    pub reliability_score: f64,
}

impl Default for PeerPerformanceStats {
    fn default() -> Self {
        Self {
            successful_requests: 0,
            failed_requests: 0,
            response_times_ms: Vec::new(),
            last_success_timestamp: None,
            last_failure_timestamp: None,
            bytes_received: 0,
            times_selected: 0,
            reliability_score: 0.5, // Start neutral
        }
    }
}

impl PeerPerformanceStats {
    /// Calculate average response time
    pub fn avg_response_time(&self) -> Duration {
        if self.response_times_ms.is_empty() {
            return Duration::from_secs(10); // Default penalty
        }
        let sum_ms: u64 = self.response_times_ms.iter().sum();
        let avg_ms = sum_ms / self.response_times_ms.len() as u64;
        Duration::from_millis(avg_ms)
    }

    /// Calculate success rate (0.0 - 1.0)
    pub fn success_rate(&self) -> f64 {
        let total = self.successful_requests + self.failed_requests;
        if total == 0 {
            return 0.5; // Neutral for new peers
        }
        self.successful_requests as f64 / total as f64
    }

    /// Calculate recency bonus (how recently was peer successful)
    pub fn recency_score(&self) -> f64 {
        match self.last_success_timestamp {
            Some(timestamp) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let age = now.saturating_sub(timestamp);
                // Exponential decay: 1.0 at 0s, 0.5 at 300s, approaches 0
                (-(age as f64) / 300.0).exp()
            }
            None => 0.0,
        }
    }

    /// Calculate failure penalty (recent failures hurt score)
    pub fn failure_penalty(&self) -> f64 {
        match self.last_failure_timestamp {
            Some(timestamp) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let age = now.saturating_sub(timestamp);
                if age < 60 {
                    // Recent failure: 0.5 penalty decaying over 60s
                    0.5 * (1.0 - age as f64 / 60.0)
                } else {
                    0.0 // No penalty after 60s
                }
            }
            None => 0.0,
        }
    }

    /// Record a successful request
    pub fn record_success(&mut self, response_time: Duration, bytes: u64) {
        self.successful_requests += 1;
        self.last_success_timestamp = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        self.bytes_received += bytes;

        // Keep rolling window of 10 most recent response times
        let response_ms = response_time.as_millis() as u64;
        self.response_times_ms.push(response_ms);
        if self.response_times_ms.len() > 10 {
            self.response_times_ms.remove(0);
        }

        // Update reliability score
        self.update_reliability_score();
    }

    /// Record a failed request
    pub fn record_failure(&mut self) {
        self.failed_requests += 1;
        self.last_failure_timestamp = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );

        // Update reliability score
        self.update_reliability_score();
    }

    /// Update the overall reliability score using ML-inspired weighted features
    fn update_reliability_score(&mut self) {
        // Feature weights (tuned through experimentation)
        const W_SUCCESS_RATE: f64 = 0.35;
        const W_RESPONSE_TIME: f64 = 0.25;
        const W_RECENCY: f64 = 0.20;
        const W_VOLUME: f64 = 0.10;
        const W_CONSISTENCY: f64 = 0.10;

        // Feature 1: Success rate (0.0 - 1.0)
        let success_score = self.success_rate();

        // Feature 2: Response time score (faster is better)
        let avg_time = self.avg_response_time().as_secs_f64();
        let response_score = if avg_time > 0.0 {
            // Ideal: <1s = 1.0, 5s = 0.5, >10s = 0.1
            (1.0 / (1.0 + avg_time / 2.0)).min(1.0)
        } else {
            0.5
        };

        // Feature 3: Recency bonus
        let recency_score = self.recency_score();

        // Feature 4: Volume bonus (peers that serve more data are valuable)
        let volume_score = if self.bytes_received > 0 {
            (self.bytes_received as f64 / 1_000_000.0).min(1.0) // Cap at 1MB
        } else {
            0.0
        };

        // Feature 5: Consistency (low variance in response times)
        let consistency_score = if self.response_times_ms.len() > 2 {
            let avg = self.avg_response_time().as_millis() as f64;
            let variance: f64 = self
                .response_times_ms
                .iter()
                .map(|t| {
                    let diff = *t as f64 - avg;
                    diff * diff
                })
                .sum::<f64>()
                / self.response_times_ms.len() as f64;

            // Low variance = high consistency score
            1.0 / (1.0 + variance.sqrt())
        } else {
            0.5
        };

        // Combine features with weights
        let base_score = W_SUCCESS_RATE * success_score
            + W_RESPONSE_TIME * response_score
            + W_RECENCY * recency_score
            + W_VOLUME * volume_score
            + W_CONSISTENCY * consistency_score;

        // Apply failure penalty
        let penalty = self.failure_penalty();
        self.reliability_score = (base_score - penalty).clamp(0.0, 1.0);
    }
}

/// AI-based peer scoring system with persistent storage
pub struct PeerScoringSystem {
    /// Performance stats per peer (IP address) - in-memory cache
    stats: Arc<RwLock<HashMap<String, PeerPerformanceStats>>>,
    /// Persistent storage
    storage: sled::Tree,
}

impl PeerScoringSystem {
    pub fn new(db: &sled::Db) -> Result<Self, String> {
        let storage = db
            .open_tree("peer_scores")
            .map_err(|e| format!("Failed to open peer_scores tree: {}", e))?;

        // Load existing data from disk
        let mut stats_map = HashMap::new();
        for result in storage.iter() {
            match result {
                Ok((key, value)) => {
                    let peer_ip = String::from_utf8_lossy(&key).to_string();
                    match bincode::deserialize::<PeerPerformanceStats>(&value) {
                        Ok(stats) => {
                            debug!(
                                "📂 Loaded AI scores for peer: {} (score: {:.3})",
                                peer_ip, stats.reliability_score
                            );
                            stats_map.insert(peer_ip, stats);
                        }
                        Err(e) => {
                            warn!("Failed to deserialize peer stats for {}: {}", peer_ip, e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to load peer score from disk: {}", e);
                }
            }
        }

        info!("🤖 [AI] Loaded {} peer scores from disk", stats_map.len());

        Ok(Self {
            stats: Arc::new(RwLock::new(stats_map)),
            storage,
        })
    }

    /// Save a peer's stats to disk asynchronously (non-blocking)
    /// Spawns background task to avoid holding lock during I/O
    async fn save_to_disk(&self, peer_ip: &str, stats: &PeerPerformanceStats) {
        let stats_clone = stats.clone();
        let peer_ip = peer_ip.to_string();
        let storage = self.storage.clone();

        // Spawn background task - don't block on disk I/O
        tokio::spawn(async move {
            match bincode::serialize(&stats_clone) {
                Ok(bytes) => {
                    if let Err(e) = storage.insert(peer_ip.as_bytes(), bytes) {
                        warn!("Failed to save peer stats to disk for {}: {}", peer_ip, e);
                    } else {
                        debug!("💾 Saved AI scores for peer: {}", peer_ip);
                    }
                }
                Err(e) => {
                    warn!("Failed to serialize peer stats for {}: {}", peer_ip, e);
                }
            }
        });
    }

    /// Flush all pending writes to disk
    pub async fn flush(&self) -> Result<(), String> {
        self.storage
            .flush_async()
            .await
            .map_err(|e| format!("Failed to flush peer scores: {}", e))?;
        debug!("💾 Flushed all peer scores to disk");
        Ok(())
    }

    /// Record a successful request to a peer
    pub async fn record_success(
        &self,
        peer_ip: &str,
        response_time: Duration,
        bytes_received: u64,
    ) {
        let peer_stats = {
            let mut stats = self.stats.write().await;
            let peer_stats = stats.entry(peer_ip.to_string()).or_default();
            peer_stats.record_success(response_time, bytes_received);

            debug!(
                "📊 Peer {} performance updated: success_rate={:.2}, avg_time={:.2}s, score={:.3}",
                peer_ip,
                peer_stats.success_rate(),
                peer_stats.avg_response_time().as_secs_f64(),
                peer_stats.reliability_score
            );

            peer_stats.clone()
        };

        // Save to disk (async, doesn't block)
        self.save_to_disk(peer_ip, &peer_stats).await;
    }

    /// Record a failed request to a peer
    pub async fn record_failure(&self, peer_ip: &str) {
        let peer_stats = {
            let mut stats = self.stats.write().await;
            let peer_stats = stats.entry(peer_ip.to_string()).or_default();
            peer_stats.record_failure();

            debug!(
                "📊 Peer {} failure recorded: success_rate={:.2}, score={:.3}",
                peer_ip,
                peer_stats.success_rate(),
                peer_stats.reliability_score
            );

            peer_stats.clone()
        };

        // Save to disk (async, doesn't block)
        self.save_to_disk(peer_ip, &peer_stats).await;
    }

    /// Record that a peer was selected (for exploration/exploitation balance)
    pub async fn record_selection(&self, peer_ip: &str) {
        let mut stats = self.stats.write().await;
        let peer_stats = stats.entry(peer_ip.to_string()).or_default();
        peer_stats.times_selected += 1;
    }

    /// Get the score for a specific peer
    pub async fn get_score(&self, peer_ip: &str) -> f64 {
        let stats = self.stats.read().await;
        stats
            .get(peer_ip)
            .map(|s| s.reliability_score)
            .unwrap_or(0.5) // Neutral score for unknown peers
    }

    /// Select the best peer from a list using AI scoring
    ///
    /// Uses epsilon-greedy strategy: 90% exploitation (pick best), 10% exploration (try random)
    pub async fn select_best_peer(&self, available_peers: &[String]) -> Option<String> {
        if available_peers.is_empty() {
            return None;
        }

        // Epsilon-greedy: 10% chance to explore (try random peer)
        let explore = rand::random::<f64>() < 0.1;

        if explore && available_peers.len() > 1 {
            // Exploration: try a random peer (helps discover better peers)
            let idx = rand::random::<usize>() % available_peers.len();
            let selected = available_peers[idx].clone();
            debug!("🔍 [AI] Exploring random peer: {}", selected);
            self.record_selection(&selected).await;
            return Some(selected);
        }

        // Exploitation: pick the best scoring peer
        let best_peer = {
            let stats = self.stats.read().await;
            let mut peer_scores: Vec<(String, f64)> = available_peers
                .iter()
                .map(|peer| {
                    let score = stats.get(peer).map(|s| s.reliability_score).unwrap_or(0.5); // New peers start neutral
                    (peer.clone(), score)
                })
                .collect();

            // Sort by score descending
            peer_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

            if let Some((best_peer, score)) = peer_scores.first() {
                info!(
                    "🤖 [AI] Selected best peer: {} (score: {:.3})",
                    best_peer, score
                );

                // Log top 3 for debugging
                for (i, (peer, s)) in peer_scores.iter().take(3).enumerate() {
                    debug!("  {}. {} (score: {:.3})", i + 1, peer, s);
                }

                Some(best_peer.clone())
            } else {
                None
            }
        }; // Drop read lock here

        // Now record selection with write lock (read lock is dropped)
        if let Some(peer) = &best_peer {
            self.record_selection(peer).await;
        }

        best_peer
    }

    /// Get performance stats for a peer (for debugging/monitoring)
    pub async fn get_stats(&self, peer_ip: &str) -> Option<PeerPerformanceStats> {
        let stats = self.stats.read().await;
        stats.get(peer_ip).cloned()
    }

    /// Get all peer statistics (for monitoring dashboard)
    pub async fn get_all_stats(&self) -> Vec<(String, PeerPerformanceStats)> {
        let stats = self.stats.read().await;
        stats
            .iter()
            .map(|(ip, stats)| (ip.clone(), stats.clone()))
            .collect()
    }

    /// Clear statistics for a peer (when they disconnect permanently)
    pub async fn clear_peer(&self, peer_ip: &str) {
        let mut stats = self.stats.write().await;
        stats.remove(peer_ip);

        // Also remove from disk
        if let Err(e) = self.storage.remove(peer_ip.as_bytes()) {
            warn!("Failed to remove peer {} from disk: {}", peer_ip, e);
        }

        debug!("🧹 Cleared stats for peer: {}", peer_ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_system() -> PeerScoringSystem {
        // Create temporary database for testing
        let config = sled::Config::new().temporary(true);
        let db = config.open().unwrap();
        PeerScoringSystem::new(&db).unwrap()
    }

    #[tokio::test]
    async fn test_peer_scoring_basic() {
        let system = create_test_system();

        // Record successes for peer A
        system
            .record_success("peer_a", Duration::from_millis(100), 1000)
            .await;
        system
            .record_success("peer_a", Duration::from_millis(150), 2000)
            .await;

        // Record failures for peer B
        system.record_failure("peer_b").await;

        // Peer A should score higher
        let score_a = system.get_score("peer_a").await;
        let score_b = system.get_score("peer_b").await;

        assert!(score_a > score_b);
    }

    #[tokio::test]
    async fn test_peer_selection() {
        let system = create_test_system();

        // Make peer_a clearly better
        for _ in 0..5 {
            system
                .record_success("peer_a", Duration::from_millis(100), 1000)
                .await;
        }

        // peer_b is slower
        for _ in 0..5 {
            system
                .record_success("peer_b", Duration::from_secs(2), 500)
                .await;
        }

        let peers = vec!["peer_a".to_string(), "peer_b".to_string()];

        // Should usually pick peer_a (90% exploitation)
        let mut a_count = 0;
        for _ in 0..100 {
            if let Some(selected) = system.select_best_peer(&peers).await {
                if selected == "peer_a" {
                    a_count += 1;
                }
            }
        }

        // Should pick peer_a most of the time (>70%)
        assert!(
            a_count > 70,
            "Expected peer_a selected >70 times, got {}",
            a_count
        );
    }
}
