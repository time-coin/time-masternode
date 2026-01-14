//! Consensus Health Monitor
//!
//! AI-powered monitoring of network consensus health to detect and prevent forks.
//! Learns from historical patterns to predict consensus issues before they occur.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use tracing::info;

/// Tracks consensus health metrics over time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusMetrics {
    pub timestamp: u64,
    pub height: u64,
    pub peer_agreement_ratio: f64, // 0.0-1.0: what fraction of peers agree on tip
    pub height_variance: f64,      // std dev of peer heights
    pub fork_count: u32,           // number of distinct chain tips seen
    pub response_rate: f64,        // fraction of peers responding to chain tip requests
    pub block_propagation_time: Option<u64>, // ms to reach majority of peers
}

/// Historical fork event for learning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkEvent {
    pub timestamp: u64,
    pub height: u64,
    pub duration_secs: u64, // how long until resolved
    pub resolution: ForkResolution,
    pub peer_count: usize,
    pub height_diff: u64, // max height difference during fork
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ForkResolution {
    NaturalConvergence, // Resolved on its own
    SyncTriggered,      // Resolved by sync_from_peers
    RollbackRequired,   // Required chain rollback
    ManualIntervention, // Required operator action
    Ongoing,            // Not yet resolved
}

/// Predicted consensus health state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthPrediction {
    pub health_score: f64,     // 0.0-1.0: overall health
    pub fork_probability: f64, // 0.0-1.0: likelihood of fork in next period
    pub recommended_action: RecommendedAction,
    pub confidence: f64,
    pub reasoning: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RecommendedAction {
    None,                    // Everything looks good
    IncreasePeerConnections, // Low peer count
    TriggerSync,             // Falling behind
    EnterDefensiveMode,      // High fork risk
    AlertOperator,           // Unusual situation
}

/// Main Consensus Health Monitor
pub struct ConsensusHealthMonitor {
    /// Recent metrics history (last 1000 samples)
    metrics_history: Arc<RwLock<VecDeque<ConsensusMetrics>>>,

    /// Fork event history for learning
    fork_history: Arc<RwLock<VecDeque<ForkEvent>>>,

    /// Current ongoing fork (if any)
    current_fork: Arc<RwLock<Option<ForkEvent>>>,

    /// Per-height consensus tracking
    height_consensus: Arc<RwLock<HashMap<u64, HeightConsensus>>>,

    /// Configuration
    config: ConsensusHealthConfig,
}

#[derive(Debug, Clone)]
pub struct ConsensusHealthConfig {
    pub min_samples: usize,
    pub fork_detection_threshold: f64, // agreement ratio below this = fork
    pub warning_threshold: f64,        // agreement ratio below this = warning
    pub max_height_variance: f64,      // max acceptable height std dev
    pub learning_rate: f64,
}

impl Default for ConsensusHealthConfig {
    fn default() -> Self {
        Self {
            min_samples: 10,
            fork_detection_threshold: 0.6, // <60% agreement = fork
            warning_threshold: 0.8,        // <80% agreement = warning
            max_height_variance: 5.0,      // heights shouldn't vary by more than 5
            learning_rate: 0.1,
        }
    }
}

/// Tracks consensus at a specific height
#[derive(Debug, Clone, Default)]
struct HeightConsensus {
    hash_counts: HashMap<[u8; 32], usize>,
    #[allow(dead_code)]
    first_seen: u64,
    total_reports: usize,
}

impl ConsensusHealthMonitor {
    pub fn new(config: ConsensusHealthConfig) -> Self {
        Self {
            metrics_history: Arc::new(RwLock::new(VecDeque::with_capacity(1000))),
            fork_history: Arc::new(RwLock::new(VecDeque::with_capacity(100))),
            current_fork: Arc::new(RwLock::new(None)),
            height_consensus: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// Record a consensus metrics sample
    pub fn record_metrics(&self, metrics: ConsensusMetrics) {
        let mut history = self.metrics_history.write();
        history.push_back(metrics.clone());
        if history.len() > 1000 {
            history.pop_front();
        }

        // Check for fork condition
        if metrics.peer_agreement_ratio < self.config.fork_detection_threshold {
            self.detect_fork(&metrics);
        } else {
            // Check if fork resolved
            self.check_fork_resolution(&metrics);
        }
    }

    /// Record a peer's chain tip report
    pub fn record_chain_tip(&self, height: u64, hash: [u8; 32]) {
        let mut consensus = self.height_consensus.write();
        let entry = consensus.entry(height).or_insert_with(|| HeightConsensus {
            first_seen: now_secs(),
            ..Default::default()
        });
        *entry.hash_counts.entry(hash).or_insert(0) += 1;
        entry.total_reports += 1;

        // Cleanup old heights (keep last 100)
        if consensus.len() > 100 {
            let min_height = height.saturating_sub(100);
            consensus.retain(|h, _| *h >= min_height);
        }
    }

    /// Calculate current peer agreement ratio for a height
    pub fn get_agreement_ratio(&self, height: u64) -> Option<f64> {
        let consensus = self.height_consensus.read();
        let entry = consensus.get(&height)?;

        if entry.total_reports == 0 {
            return None;
        }

        let max_agreement = entry.hash_counts.values().max().copied().unwrap_or(0);
        Some(max_agreement as f64 / entry.total_reports as f64)
    }

    /// Get the dominant hash at a height
    pub fn get_dominant_hash(&self, height: u64) -> Option<([u8; 32], usize)> {
        let consensus = self.height_consensus.read();
        let entry = consensus.get(&height)?;

        entry
            .hash_counts
            .iter()
            .max_by_key(|(_, count)| *count)
            .map(|(hash, count)| (*hash, *count))
    }

    /// Predict consensus health
    pub fn predict_health(&self) -> HealthPrediction {
        let history = self.metrics_history.read();

        if history.len() < self.config.min_samples {
            return HealthPrediction {
                health_score: 0.5,
                fork_probability: 0.0,
                recommended_action: RecommendedAction::None,
                confidence: 0.0,
                reasoning: vec!["Insufficient data for prediction".to_string()],
            };
        }

        let recent: Vec<&ConsensusMetrics> = history.iter().rev().take(20).collect();
        let mut reasoning = Vec::new();

        // Calculate average metrics
        let avg_agreement =
            recent.iter().map(|m| m.peer_agreement_ratio).sum::<f64>() / recent.len() as f64;
        let avg_variance =
            recent.iter().map(|m| m.height_variance).sum::<f64>() / recent.len() as f64;
        let avg_response =
            recent.iter().map(|m| m.response_rate).sum::<f64>() / recent.len() as f64;
        let avg_forks =
            recent.iter().map(|m| m.fork_count as f64).sum::<f64>() / recent.len() as f64;

        // Calculate trends (are things getting better or worse?)
        let trend = if recent.len() >= 10 {
            let first_half: f64 = recent[5..]
                .iter()
                .map(|m| m.peer_agreement_ratio)
                .sum::<f64>()
                / 5.0;
            let second_half: f64 = recent[..5]
                .iter()
                .map(|m| m.peer_agreement_ratio)
                .sum::<f64>()
                / 5.0;
            second_half - first_half // positive = improving
        } else {
            0.0
        };

        // Health score components
        let agreement_score = avg_agreement;
        let variance_score = (1.0 - avg_variance / 10.0).clamp(0.0, 1.0);
        let response_score = avg_response;
        let fork_score = (1.0 - avg_forks / 5.0).clamp(0.0, 1.0);
        let trend_score = (0.5 + trend).clamp(0.0, 1.0);

        // Weighted health score
        let health_score = agreement_score * 0.35
            + variance_score * 0.20
            + response_score * 0.20
            + fork_score * 0.15
            + trend_score * 0.10;

        // Fork probability based on current state and trend
        let fork_probability = if avg_agreement < self.config.warning_threshold {
            let base_prob = 1.0 - avg_agreement;
            let trend_adj = if trend < 0.0 { 0.2 } else { -0.1 };
            (base_prob + trend_adj).clamp(0.0, 1.0)
        } else {
            (0.1 * avg_forks).clamp(0.0, 0.3)
        };

        // Determine recommended action
        let recommended_action = if health_score < 0.3 {
            reasoning.push("Critical: Health score very low".to_string());
            RecommendedAction::AlertOperator
        } else if fork_probability > 0.7 {
            reasoning.push(format!(
                "High fork probability: {:.1}%",
                fork_probability * 100.0
            ));
            RecommendedAction::EnterDefensiveMode
        } else if avg_agreement < self.config.warning_threshold {
            reasoning.push(format!("Low peer agreement: {:.1}%", avg_agreement * 100.0));
            RecommendedAction::TriggerSync
        } else if avg_response < 0.5 {
            reasoning.push(format!(
                "Low peer response rate: {:.1}%",
                avg_response * 100.0
            ));
            RecommendedAction::IncreasePeerConnections
        } else {
            RecommendedAction::None
        };

        // Build reasoning
        reasoning.push(format!("Agreement: {:.1}%", avg_agreement * 100.0));
        reasoning.push(format!("Height variance: {:.1}", avg_variance));
        reasoning.push(format!("Response rate: {:.1}%", avg_response * 100.0));
        reasoning.push(format!(
            "Trend: {}",
            if trend > 0.0 {
                "improving"
            } else {
                "declining"
            }
        ));

        let confidence = (history.len() as f64 / 100.0).clamp(0.0, 1.0);

        HealthPrediction {
            health_score,
            fork_probability,
            recommended_action,
            confidence,
            reasoning,
        }
    }

    /// Check if we should trigger a sync based on health
    pub fn should_trigger_sync(&self) -> bool {
        let prediction = self.predict_health();
        matches!(
            prediction.recommended_action,
            RecommendedAction::TriggerSync | RecommendedAction::EnterDefensiveMode
        )
    }

    /// Get fork history statistics
    pub fn get_fork_stats(&self) -> ForkStats {
        let history = self.fork_history.read();

        if history.is_empty() {
            return ForkStats::default();
        }

        let total_forks = history.len();
        let resolved_forks: Vec<&ForkEvent> = history
            .iter()
            .filter(|f| f.resolution != ForkResolution::Ongoing)
            .collect();

        let avg_duration = if !resolved_forks.is_empty() {
            resolved_forks.iter().map(|f| f.duration_secs).sum::<u64>() as f64
                / resolved_forks.len() as f64
        } else {
            0.0
        };

        let natural_resolution_rate = resolved_forks
            .iter()
            .filter(|f| f.resolution == ForkResolution::NaturalConvergence)
            .count() as f64
            / resolved_forks.len().max(1) as f64;

        ForkStats {
            total_forks,
            avg_duration_secs: avg_duration,
            natural_resolution_rate,
            ongoing_fork: self.current_fork.read().is_some(),
        }
    }

    fn detect_fork(&self, metrics: &ConsensusMetrics) {
        let mut current = self.current_fork.write();

        if current.is_none() {
            info!(
                "🔀 Fork detected at height {}: {:.1}% agreement, {} distinct tips",
                metrics.height,
                metrics.peer_agreement_ratio * 100.0,
                metrics.fork_count
            );

            *current = Some(ForkEvent {
                timestamp: metrics.timestamp,
                height: metrics.height,
                duration_secs: 0,
                resolution: ForkResolution::Ongoing,
                peer_count: 0,
                height_diff: 0,
            });
        }
    }

    fn check_fork_resolution(&self, metrics: &ConsensusMetrics) {
        let mut current = self.current_fork.write();

        if let Some(fork) = current.take() {
            let duration = metrics.timestamp.saturating_sub(fork.timestamp);

            let resolved_fork = ForkEvent {
                duration_secs: duration,
                resolution: ForkResolution::NaturalConvergence,
                peer_count: metrics.fork_count as usize,
                height_diff: metrics.height.saturating_sub(fork.height),
                ..fork
            };

            info!(
                "✅ Fork resolved after {}s: height {} → {}",
                duration, fork.height, metrics.height
            );

            let mut history = self.fork_history.write();
            history.push_back(resolved_fork);
            if history.len() > 100 {
                history.pop_front();
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForkStats {
    pub total_forks: usize,
    pub avg_duration_secs: f64,
    pub natural_resolution_rate: f64,
    pub ongoing_fork: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_prediction_insufficient_data() {
        let monitor = ConsensusHealthMonitor::new(ConsensusHealthConfig::default());
        let prediction = monitor.predict_health();

        assert_eq!(prediction.confidence, 0.0);
        assert_eq!(prediction.recommended_action, RecommendedAction::None);
    }

    #[test]
    fn test_agreement_ratio() {
        let monitor = ConsensusHealthMonitor::new(ConsensusHealthConfig::default());

        let hash1 = [1u8; 32];
        let hash2 = [2u8; 32];

        // 3 peers agree on hash1, 1 peer reports hash2
        monitor.record_chain_tip(100, hash1);
        monitor.record_chain_tip(100, hash1);
        monitor.record_chain_tip(100, hash1);
        monitor.record_chain_tip(100, hash2);

        let ratio = monitor.get_agreement_ratio(100).unwrap();
        assert!((ratio - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_dominant_hash() {
        let monitor = ConsensusHealthMonitor::new(ConsensusHealthConfig::default());

        let hash1 = [1u8; 32];
        let hash2 = [2u8; 32];

        monitor.record_chain_tip(100, hash1);
        monitor.record_chain_tip(100, hash1);
        monitor.record_chain_tip(100, hash2);

        let (dominant, count) = monitor.get_dominant_hash(100).unwrap();
        assert_eq!(dominant, hash1);
        assert_eq!(count, 2);
    }

    #[test]
    fn test_fork_detection() {
        let monitor = ConsensusHealthMonitor::new(ConsensusHealthConfig::default());

        // Record metrics indicating a fork
        let metrics = ConsensusMetrics {
            timestamp: now_secs(),
            height: 100,
            peer_agreement_ratio: 0.5, // Below threshold
            height_variance: 3.0,
            fork_count: 2,
            response_rate: 0.8,
            block_propagation_time: None,
        };

        monitor.record_metrics(metrics);

        assert!(monitor.current_fork.read().is_some());
    }
}
