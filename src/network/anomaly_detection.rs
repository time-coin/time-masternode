use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Anomaly score thresholds
const THRESHOLD_SUSPICIOUS: f64 = 0.3;
const THRESHOLD_ANOMALOUS: f64 = 0.7;
const THRESHOLD_MALICIOUS: f64 = 0.9;

/// Time window for rate limiting (seconds)
const RATE_WINDOW_SECS: u64 = 60;

/// Maximum requests per minute (normal behavior)
const MAX_REQUESTS_PER_MINUTE: usize = 100;

/// Peer behavior metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerBehaviorMetrics {
    /// Total requests received
    pub total_requests: u64,
    /// Valid responses provided
    pub valid_responses: u64,
    /// Invalid/malicious responses
    pub invalid_responses: u64,
    /// Fork attempts detected
    pub fork_attempts: u64,
    /// Request timestamps (last N)
    pub request_times: Vec<u64>,
    /// Response validity pattern
    pub validity_pattern: Vec<bool>,
    /// Last anomaly score
    pub last_anomaly_score: f64,
    /// First seen timestamp
    pub first_seen: u64,
    /// Last activity timestamp
    pub last_activity: u64,
    /// Times this peer was flagged
    pub flag_count: u64,
}

impl Default for PeerBehaviorMetrics {
    fn default() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            total_requests: 0,
            valid_responses: 0,
            invalid_responses: 0,
            fork_attempts: 0,
            request_times: Vec::new(),
            validity_pattern: Vec::new(),
            last_anomaly_score: 0.0,
            first_seen: now,
            last_activity: now,
            flag_count: 0,
        }
    }
}

/// Anomaly detection result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyResult {
    /// Peer IP address
    pub peer_ip: String,
    /// Anomaly score (0.0 = normal, 1.0 = malicious)
    pub score: f64,
    /// Classification
    pub classification: AnomalyClassification,
    /// Recommended action
    pub action: RecommendedAction,
    /// Reasons for detection
    pub reasons: Vec<String>,
}

/// Anomaly classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnomalyClassification {
    Normal,
    Suspicious,
    Anomalous,
    Malicious,
}

/// Recommended action based on anomaly score
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecommendedAction {
    None,
    RateLimit,
    TemporaryBan,
    PermanentBlacklist,
}

/// AI-powered anomaly detection system
pub struct AnomalyDetector {
    /// Peer behavior tracking
    peer_metrics: Arc<RwLock<HashMap<String, PeerBehaviorMetrics>>>,
    /// Persistent storage
    storage: sled::Tree,
}

impl AnomalyDetector {
    /// Create new anomaly detector with persistent storage
    pub fn new(db: &sled::Db) -> Result<Self, String> {
        let storage = db
            .open_tree("anomaly_detection")
            .map_err(|e| format!("Failed to open anomaly_detection tree: {}", e))?;

        // Load historical data
        let mut peer_metrics = HashMap::new();
        for result in storage.iter() {
            match result {
                Ok((key, value)) => {
                    let peer_ip = String::from_utf8_lossy(&key).to_string();
                    if let Ok(metrics) = bincode::deserialize::<PeerBehaviorMetrics>(&value) {
                        debug!(
                            "📂 Loaded anomaly data for peer: {} (score: {:.3})",
                            peer_ip, metrics.last_anomaly_score
                        );
                        peer_metrics.insert(peer_ip, metrics);
                    }
                }
                Err(e) => warn!("Failed to load anomaly data: {}", e),
            }
        }

        info!(
            "🛡️ [AI] Loaded {} peer behavior profiles from disk",
            peer_metrics.len()
        );

        Ok(Self {
            peer_metrics: Arc::new(RwLock::new(peer_metrics)),
            storage,
        })
    }

    /// Record a request from a peer
    pub async fn record_request(&self, peer_ip: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut metrics_map = self.peer_metrics.write().await;
        let metrics = metrics_map.entry(peer_ip.to_string()).or_default();

        metrics.total_requests += 1;
        metrics.last_activity = now;

        // Track request timing (keep last 100)
        metrics.request_times.push(now);
        if metrics.request_times.len() > 100 {
            metrics.request_times.remove(0);
        }
    }

    /// Record a valid response from a peer
    pub async fn record_valid_response(&self, peer_ip: &str) {
        let mut metrics_map = self.peer_metrics.write().await;
        let metrics = metrics_map.entry(peer_ip.to_string()).or_default();

        metrics.valid_responses += 1;

        // Track validity pattern (keep last 50)
        metrics.validity_pattern.push(true);
        if metrics.validity_pattern.len() > 50 {
            metrics.validity_pattern.remove(0);
        }

        let metrics_clone = metrics.clone();
        drop(metrics_map);

        self.save_to_disk(peer_ip, &metrics_clone).await;
    }

    /// Record an invalid/malicious response from a peer
    pub async fn record_invalid_response(&self, peer_ip: &str, reason: &str) {
        let mut metrics_map = self.peer_metrics.write().await;
        let metrics = metrics_map.entry(peer_ip.to_string()).or_default();

        metrics.invalid_responses += 1;

        // Track validity pattern
        metrics.validity_pattern.push(false);
        if metrics.validity_pattern.len() > 50 {
            metrics.validity_pattern.remove(0);
        }

        warn!(
            "⚠️ [AI] Invalid response from {}: {} (total invalid: {})",
            peer_ip, reason, metrics.invalid_responses
        );

        let metrics_clone = metrics.clone();
        drop(metrics_map);

        self.save_to_disk(peer_ip, &metrics_clone).await;
    }

    /// Record a fork attempt from a peer
    pub async fn record_fork_attempt(&self, peer_ip: &str) {
        let mut metrics_map = self.peer_metrics.write().await;
        let metrics = metrics_map.entry(peer_ip.to_string()).or_default();

        metrics.fork_attempts += 1;

        warn!(
            "🔀 [AI] Fork attempt detected from {} (total attempts: {})",
            peer_ip, metrics.fork_attempts
        );

        let metrics_clone = metrics.clone();
        drop(metrics_map);

        self.save_to_disk(peer_ip, &metrics_clone).await;
    }

    /// Analyze peer and return anomaly score
    pub async fn analyze_peer(&self, peer_ip: &str) -> AnomalyResult {
        let metrics_map = self.peer_metrics.read().await;
        let metrics = metrics_map.get(peer_ip).cloned().unwrap_or_default();
        drop(metrics_map);

        let mut reasons = Vec::new();
        let mut score = 0.0;

        // Feature 1: Response validity rate (weight: 40%)
        let total_responses = metrics.valid_responses + metrics.invalid_responses;
        if total_responses > 0 {
            let validity_rate = metrics.valid_responses as f64 / total_responses as f64;
            let validity_score = 1.0 - validity_rate; // Low validity = high anomaly
            score += validity_score * 0.4;

            if validity_rate < 0.5 {
                reasons.push(format!(
                    "Low response validity: {:.1}%",
                    validity_rate * 100.0
                ));
            }
        }

        // Feature 2: Fork attempt rate (weight: 30%)
        if metrics.fork_attempts > 0 {
            let fork_rate = metrics.fork_attempts as f64 / total_responses.max(1) as f64;
            let fork_score = fork_rate.min(1.0);
            score += fork_score * 0.3;

            reasons.push(format!("Fork attempts detected: {}", metrics.fork_attempts));
        }

        // Feature 3: Request rate (weight: 20%)
        let request_rate = self.calculate_request_rate(&metrics);
        if request_rate > MAX_REQUESTS_PER_MINUTE as f64 {
            let rate_score = ((request_rate / MAX_REQUESTS_PER_MINUTE as f64) - 1.0).min(1.0);
            score += rate_score * 0.2;

            reasons.push(format!(
                "High request rate: {:.1} req/min (normal: {})",
                request_rate, MAX_REQUESTS_PER_MINUTE
            ));
        }

        // Feature 4: Pattern consistency (weight: 10%)
        let pattern_score = self.calculate_pattern_anomaly(&metrics);
        score += pattern_score * 0.1;

        if pattern_score > 0.5 {
            reasons.push("Unusual request timing pattern".to_string());
        }

        // Clamp score to [0.0, 1.0]
        score = score.clamp(0.0, 1.0);

        // Update metrics with new score
        let mut metrics_map = self.peer_metrics.write().await;
        if let Some(m) = metrics_map.get_mut(peer_ip) {
            m.last_anomaly_score = score;
        }
        drop(metrics_map);

        // Classify and recommend action
        let (classification, action) = self.classify_score(score);

        // Update flag count if anomalous
        if classification != AnomalyClassification::Normal {
            let mut metrics_map = self.peer_metrics.write().await;
            if let Some(m) = metrics_map.get_mut(peer_ip) {
                m.flag_count += 1;
            }
        }

        debug!(
            "🛡️ [AI] Analyzed {}: score={:.3}, class={:?}, action={:?}",
            peer_ip, score, classification, action
        );

        AnomalyResult {
            peer_ip: peer_ip.to_string(),
            score,
            classification,
            action,
            reasons,
        }
    }

    /// Calculate request rate (requests per minute)
    fn calculate_request_rate(&self, metrics: &PeerBehaviorMetrics) -> f64 {
        if metrics.request_times.len() < 2 {
            return 0.0;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Count requests in last minute
        let recent_requests = metrics
            .request_times
            .iter()
            .filter(|&&t| now - t <= RATE_WINDOW_SECS)
            .count();

        recent_requests as f64
    }

    /// Calculate pattern anomaly score based on request timing
    fn calculate_pattern_anomaly(&self, metrics: &PeerBehaviorMetrics) -> f64 {
        if metrics.request_times.len() < 10 {
            return 0.0;
        }

        // Calculate inter-request intervals
        let mut intervals = Vec::new();
        for i in 1..metrics.request_times.len() {
            let interval = metrics.request_times[i] - metrics.request_times[i - 1];
            intervals.push(interval);
        }

        if intervals.is_empty() {
            return 0.0;
        }

        // Calculate variance - high variance = more random (normal)
        // Low variance = robotic/scripted (suspicious)
        let mean: f64 = intervals.iter().sum::<u64>() as f64 / intervals.len() as f64;
        let variance: f64 = intervals
            .iter()
            .map(|&i| {
                let diff = i as f64 - mean;
                diff * diff
            })
            .sum::<f64>()
            / intervals.len() as f64;

        // Low variance (< 1 second) = suspicious
        if variance < 1.0 {
            0.8 // Very consistent timing is suspicious
        } else if variance < 10.0 {
            0.5 // Somewhat consistent
        } else {
            0.0 // Random timing is normal
        }
    }

    /// Classify anomaly score and recommend action
    fn classify_score(&self, score: f64) -> (AnomalyClassification, RecommendedAction) {
        if score >= THRESHOLD_MALICIOUS {
            (
                AnomalyClassification::Malicious,
                RecommendedAction::PermanentBlacklist,
            )
        } else if score >= THRESHOLD_ANOMALOUS {
            (
                AnomalyClassification::Anomalous,
                RecommendedAction::TemporaryBan,
            )
        } else if score >= THRESHOLD_SUSPICIOUS {
            (
                AnomalyClassification::Suspicious,
                RecommendedAction::RateLimit,
            )
        } else {
            (AnomalyClassification::Normal, RecommendedAction::None)
        }
    }

    /// Save peer metrics to disk
    async fn save_to_disk(&self, peer_ip: &str, metrics: &PeerBehaviorMetrics) {
        if let Ok(bytes) = bincode::serialize(metrics) {
            if let Err(e) = self.storage.insert(peer_ip.as_bytes(), bytes) {
                warn!("Failed to save anomaly data for {}: {}", peer_ip, e);
            } else {
                debug!("💾 Saved anomaly data for peer: {}", peer_ip);
            }
        }
    }

    /// Get all anomaly scores
    pub async fn get_all_scores(&self) -> HashMap<String, f64> {
        let metrics_map = self.peer_metrics.read().await;
        metrics_map
            .iter()
            .map(|(ip, m)| (ip.clone(), m.last_anomaly_score))
            .collect()
    }

    /// Clear data for a peer
    pub async fn clear_peer(&self, peer_ip: &str) {
        let mut metrics_map = self.peer_metrics.write().await;
        metrics_map.remove(peer_ip);

        if let Err(e) = self.storage.remove(peer_ip.as_bytes()) {
            warn!("Failed to remove anomaly data for {}: {}", peer_ip, e);
        }

        debug!("🧹 Cleared anomaly data for peer: {}", peer_ip);
    }

    /// Flush all data to disk
    pub async fn flush(&self) -> Result<(), String> {
        self.storage
            .flush_async()
            .await
            .map_err(|e| format!("Failed to flush anomaly data: {}", e))?;
        debug!("💾 Flushed anomaly detection data to disk");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_detector() -> AnomalyDetector {
        let config = sled::Config::new().temporary(true);
        let db = config.open().unwrap();
        AnomalyDetector::new(&db).unwrap()
    }

    #[tokio::test]
    async fn test_normal_peer() {
        let detector = create_test_detector();

        // Simulate normal peer behavior
        for _ in 0..10 {
            detector.record_request("peer_a").await;
            detector.record_valid_response("peer_a").await;
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        let result = detector.analyze_peer("peer_a").await;
        assert!(result.score < THRESHOLD_SUSPICIOUS);
        assert_eq!(result.classification, AnomalyClassification::Normal);
    }

    #[tokio::test]
    async fn test_malicious_peer() {
        let detector = create_test_detector();

        // Simulate malicious peer behavior
        for _ in 0..10 {
            detector.record_request("peer_b").await;
            detector
                .record_invalid_response("peer_b", "fake blocks")
                .await;
        }

        detector.record_fork_attempt("peer_b").await;

        let result = detector.analyze_peer("peer_b").await;
        assert!(result.score >= THRESHOLD_ANOMALOUS);
        assert_ne!(result.classification, AnomalyClassification::Normal);
    }

    #[tokio::test]
    async fn test_ddos_detection() {
        let detector = create_test_detector();

        // Simulate DDoS - many rapid requests
        for _ in 0..200 {
            detector.record_request("peer_c").await;
        }

        let result = detector.analyze_peer("peer_c").await;
        assert!(result.score > 0.0);
        assert!(result
            .reasons
            .iter()
            .any(|r| r.contains("High request rate")));
    }
}
