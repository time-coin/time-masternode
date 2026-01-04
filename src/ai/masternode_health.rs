//! AI Masternode Health Monitor
//!
//! Learns masternode heartbeat patterns and provides adaptive timeouts
//! to prevent false offline detections while maintaining network security.

use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Masternode health pattern data
#[derive(Debug, Clone)]
pub struct HealthPattern {
    /// Average heartbeat interval for this masternode
    avg_interval_secs: f64,
    /// Standard deviation of heartbeat intervals
    std_dev_secs: f64,
    /// Number of heartbeats observed
    sample_count: usize,
    /// Last heartbeat timestamp
    last_heartbeat: u64,
    /// Number of false offline detections (learned)
    false_offline_count: usize,
    /// Reliability score (0.0-1.0)
    reliability_score: f64,
}

impl Default for HealthPattern {
    fn default() -> Self {
        Self {
            avg_interval_secs: 60.0,
            std_dev_secs: 10.0,
            sample_count: 0,
            last_heartbeat: 0,
            false_offline_count: 0,
            reliability_score: 1.0,
        }
    }
}

/// Network health state
#[derive(Debug, Clone)]
pub struct NetworkHealth {
    /// Overall network congestion (0.0-1.0)
    pub congestion: f64,
    /// Average peer latency in milliseconds
    pub avg_latency_ms: f64,
    /// Recent block production success rate (0.0-1.0)
    pub production_rate: f64,
}

impl Default for NetworkHealth {
    fn default() -> Self {
        Self {
            congestion: 0.0,
            avg_latency_ms: 100.0,
            production_rate: 1.0,
        }
    }
}

/// Prediction result
#[derive(Debug, Clone)]
pub struct HealthPrediction {
    /// Recommended timeout in seconds
    pub recommended_timeout_secs: u64,
    /// Confidence in prediction (0.0-1.0)
    pub confidence: f64,
    /// Probability node is actually offline (0.0-1.0)
    pub offline_probability: f64,
    /// Reason for recommendation
    pub reason: String,
}

/// AI Masternode Health Monitor
pub struct MasternodeHealthAI {
    /// Per-masternode health patterns
    patterns: Arc<RwLock<HashMap<String, HealthPattern>>>,
    /// Persistent storage
    db: Arc<Db>,
    /// Learning rate (0.0-1.0)
    learning_rate: f64,
    /// Minimum samples before adaptive timeouts
    min_samples: usize,
    /// Base timeout for unknown nodes
    base_timeout_secs: u64,
}

impl MasternodeHealthAI {
    /// Create new AI masternode health monitor
    pub fn new(db: Arc<Db>, learning_rate: f64, min_samples: usize) -> Result<Self, String> {
        // Load persisted patterns from database
        let mut loaded_patterns: HashMap<String, HealthPattern> = HashMap::new();
        let prefix = b"ai_mn_health_";

        for result in db.scan_prefix(prefix) {
            match result {
                Ok((key, value)) => {
                    let key_str = String::from_utf8_lossy(&key);
                    let addr = key_str.trim_start_matches("ai_mn_health_");

                    // Deserialize pattern
                    if let Ok(pattern_json) = String::from_utf8(value.to_vec()) {
                        if let Ok(pattern) = serde_json::from_str::<HealthPattern>(&pattern_json) {
                            loaded_patterns.insert(addr.to_string(), pattern);
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to load health pattern: {}", e);
                }
            }
        }

        if !loaded_patterns.is_empty() {
            info!(
                "📊 [AI Health] Loaded {} masternode health patterns",
                loaded_patterns.len()
            );
        }

        Ok(Self {
            patterns: Arc::new(RwLock::new(loaded_patterns)),
            db,
            learning_rate,
            min_samples,
            base_timeout_secs: 60,
        })
    }

    /// Record a heartbeat from a masternode
    pub async fn record_heartbeat(
        &self,
        masternode_addr: &str,
        timestamp: u64,
    ) -> Result<(), String> {
        let mut patterns = self.patterns.write().await;
        let pattern = patterns.entry(masternode_addr.to_string()).or_default();

        // Calculate interval since last heartbeat
        if pattern.last_heartbeat > 0 && timestamp > pattern.last_heartbeat {
            let interval = timestamp - pattern.last_heartbeat;

            // Update exponential moving average
            if pattern.sample_count == 0 {
                pattern.avg_interval_secs = interval as f64;
                pattern.std_dev_secs = 10.0; // Initial guess
            } else {
                let delta = interval as f64 - pattern.avg_interval_secs;
                pattern.avg_interval_secs += self.learning_rate * delta;

                // Update standard deviation (exponential moving)
                let dev_delta = delta.abs() - pattern.std_dev_secs;
                pattern.std_dev_secs += self.learning_rate * dev_delta;
            }

            pattern.sample_count += 1;
            debug!(
                "🧠 [AI Health] {} heartbeat: interval={}s, avg={:.1}s, std={:.1}s, samples={}",
                masternode_addr,
                interval,
                pattern.avg_interval_secs,
                pattern.std_dev_secs,
                pattern.sample_count
            );
        }

        pattern.last_heartbeat = timestamp;

        // Persist to database
        self.persist_pattern(masternode_addr, pattern).await?;

        Ok(())
    }

    /// Recommend timeout for a specific masternode
    pub async fn recommend_timeout(
        &self,
        masternode_addr: &str,
        network: &NetworkHealth,
        target_height: u64,
    ) -> HealthPrediction {
        let patterns = self.patterns.read().await;

        // Get pattern for this masternode
        let pattern = patterns.get(masternode_addr);

        // If insufficient data, use conservative defaults
        if pattern.is_none() || pattern.unwrap().sample_count < self.min_samples {
            return self.default_prediction(network, target_height);
        }

        let pattern = pattern.unwrap();

        // Calculate adaptive timeout based on learned patterns
        // Timeout = avg_interval + (confidence_multiplier * std_dev) + network_adjustment

        // Confidence multiplier (more samples = tighter bounds)
        let confidence_mult = if pattern.sample_count < 50 {
            3.0 // 3 std devs for low confidence
        } else if pattern.sample_count < 100 {
            2.5 // 2.5 std devs for medium confidence
        } else {
            2.0 // 2 std devs for high confidence
        };

        let mut timeout = pattern.avg_interval_secs + (confidence_mult * pattern.std_dev_secs);

        // Network adjustment: increase timeout if network is degraded
        let network_mult =
            1.0 + (network.congestion * 0.5) + ((network.avg_latency_ms - 100.0) / 1000.0).max(0.0);
        timeout *= network_mult;

        // Genesis adjustment: be more lenient at low heights
        if target_height <= 1 {
            timeout = timeout.max(300.0); // At least 5 minutes for genesis
        }

        // Reliability adjustment: increase timeout for historically reliable nodes
        timeout *= 1.0 + (pattern.reliability_score * 0.2);

        // Bounds checking
        timeout = timeout.max(self.base_timeout_secs as f64).min(600.0); // Max 10 minutes

        let confidence = (pattern.sample_count as f64 / 100.0).min(1.0);

        HealthPrediction {
            recommended_timeout_secs: timeout.ceil() as u64,
            confidence,
            offline_probability: self.calculate_offline_probability(pattern, network),
            reason: format!(
                "Adaptive: avg={:.1}s, std={:.1}s, samples={}, net_mult={:.2}",
                pattern.avg_interval_secs, pattern.std_dev_secs, pattern.sample_count, network_mult
            ),
        }
    }

    /// Default prediction when insufficient data
    fn default_prediction(&self, network: &NetworkHealth, target_height: u64) -> HealthPrediction {
        let mut timeout = self.base_timeout_secs as f64;

        // Network adjustment
        let network_mult = 1.0 + (network.congestion * 0.5);
        timeout *= network_mult;

        // Genesis adjustment
        if target_height <= 1 {
            timeout = timeout.max(300.0);
        }

        HealthPrediction {
            recommended_timeout_secs: timeout.ceil() as u64,
            confidence: 0.5,
            offline_probability: 0.5,
            reason: "Default: insufficient data".to_string(),
        }
    }

    /// Calculate probability that node is actually offline
    fn calculate_offline_probability(
        &self,
        pattern: &HealthPattern,
        network: &NetworkHealth,
    ) -> f64 {
        // Factors:
        // 1. Reliability score (history of false positives)
        // 2. Network health (degraded network increases false positive chance)
        // 3. Recent pattern consistency

        let false_positive_rate =
            pattern.false_offline_count as f64 / (pattern.sample_count.max(1) as f64);

        let network_factor = network.congestion * 0.3; // Network issues increase false positives

        // Base probability starts at 0.5, adjusted by factors
        let mut probability = 0.5;
        probability -= pattern.reliability_score * 0.2; // Reliable nodes less likely offline
        probability += false_positive_rate * 0.3; // History of false positives
        probability += network_factor; // Network issues

        probability.max(0.1).min(0.9) // Bounds
    }

    /// Record false offline detection (for learning)
    pub async fn record_false_offline(&self, masternode_addr: &str) -> Result<(), String> {
        let mut patterns = self.patterns.write().await;
        let pattern = patterns.entry(masternode_addr.to_string()).or_default();

        pattern.false_offline_count += 1;

        // Decrease reliability score
        pattern.reliability_score = (pattern.reliability_score - 0.05).max(0.0);

        info!(
            "📉 [AI Health] {} false offline recorded. Reliability: {:.2}, false_count: {}",
            masternode_addr, pattern.reliability_score, pattern.false_offline_count
        );

        self.persist_pattern(masternode_addr, pattern).await?;

        Ok(())
    }

    /// Record correct offline detection (for learning)
    pub async fn record_correct_offline(&self, masternode_addr: &str) -> Result<(), String> {
        let mut patterns = self.patterns.write().await;
        let pattern = patterns.entry(masternode_addr.to_string()).or_default();

        // Increase reliability score
        pattern.reliability_score = (pattern.reliability_score + 0.01).min(1.0);

        self.persist_pattern(masternode_addr, pattern).await?;

        Ok(())
    }

    /// Get statistics for all masternodes
    pub async fn get_statistics(&self) -> HashMap<String, HealthPattern> {
        self.patterns.read().await.clone()
    }

    /// Persist pattern to database
    async fn persist_pattern(&self, addr: &str, pattern: &HealthPattern) -> Result<(), String> {
        let key = format!("ai_mn_health_{}", addr);
        let value = serde_json::to_string(pattern)
            .map_err(|e| format!("Failed to serialize pattern: {}", e))?;

        self.db
            .insert(key.as_bytes(), value.as_bytes())
            .map_err(|e| format!("Failed to persist pattern: {}", e))?;

        Ok(())
    }
}

// Make HealthPattern serializable
impl serde::Serialize for HealthPattern {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("HealthPattern", 6)?;
        state.serialize_field("avg_interval_secs", &self.avg_interval_secs)?;
        state.serialize_field("std_dev_secs", &self.std_dev_secs)?;
        state.serialize_field("sample_count", &self.sample_count)?;
        state.serialize_field("last_heartbeat", &self.last_heartbeat)?;
        state.serialize_field("false_offline_count", &self.false_offline_count)?;
        state.serialize_field("reliability_score", &self.reliability_score)?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for HealthPattern {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct HealthPatternHelper {
            avg_interval_secs: f64,
            std_dev_secs: f64,
            sample_count: usize,
            last_heartbeat: u64,
            false_offline_count: usize,
            reliability_score: f64,
        }

        let helper = HealthPatternHelper::deserialize(deserializer)?;
        Ok(HealthPattern {
            avg_interval_secs: helper.avg_interval_secs,
            std_dev_secs: helper.std_dev_secs,
            sample_count: helper.sample_count,
            last_heartbeat: helper.last_heartbeat,
            false_offline_count: helper.false_offline_count,
            reliability_score: helper.reliability_score,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_learning() {
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let ai = MasternodeHealthAI::new(db, 0.1, 10).unwrap();

        // Simulate regular heartbeats
        let addr = "test_masternode";
        let mut timestamp = 1000u64;

        for _ in 0..20 {
            timestamp += 60; // 60 second intervals
            ai.record_heartbeat(addr, timestamp).await.unwrap();
        }

        // Get recommendation
        let network = NetworkHealth::default();
        let prediction = ai.recommend_timeout(addr, &network, 100).await;

        // Should be close to 60s + some buffer
        assert!(prediction.recommended_timeout_secs >= 60);
        assert!(prediction.recommended_timeout_secs <= 150);
        assert!(prediction.confidence > 0.1);
    }

    #[tokio::test]
    async fn test_adaptive_timeout() {
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let ai = MasternodeHealthAI::new(db, 0.1, 10).unwrap();

        // Simulate variable intervals (slow masternode)
        let addr = "slow_masternode";
        let mut timestamp = 1000u64;

        for _ in 0..20 {
            timestamp += 120; // 120 second intervals (slower)
            ai.record_heartbeat(addr, timestamp).await.unwrap();
        }

        let network = NetworkHealth::default();
        let prediction = ai.recommend_timeout(addr, &network, 100).await;

        // Should adapt to slower intervals
        assert!(prediction.recommended_timeout_secs > 120);
    }
}
