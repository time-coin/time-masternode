use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

// Use parking_lot::RwLock instead of std::sync::RwLock
use parking_lot::RwLock;

/// Centralized AI metrics and dashboard for monitoring all AI subsystems
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIMetricsDashboard {
    pub timestamp: u64,
    pub anomaly_detection: AnomalyMetrics,
    pub attack_detection: AttackMetrics,
    pub peer_selection: PeerSelectionMetrics,
    pub consensus_health: ConsensusHealthMetrics,
    pub network_optimization: NetworkOptimizationMetrics,
    pub transaction_analysis: TransactionAnalysisMetrics,
    pub overall_ai_health: f64, // 0.0 to 1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyMetrics {
    pub anomalies_detected: usize,
    pub false_positive_rate: f64,
    pub detection_accuracy: f64,
    pub avg_detection_time_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttackMetrics {
    pub attacks_detected: usize,
    pub attacks_mitigated: usize,
    pub critical_attacks: usize,
    pub attack_types: HashMap<String, usize>,
    pub mitigation_success_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSelectionMetrics {
    pub peers_scored: usize,
    pub selection_accuracy: f64,
    pub avg_peer_score: f64,
    pub bad_peers_filtered: usize,
    pub peer_diversity_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusHealthMetrics {
    pub consensus_score: f64,
    pub fork_probability: f64,
    pub network_agreement: f64,
    pub health_predictions_made: usize,
    pub prediction_accuracy: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkOptimizationMetrics {
    pub optimizations_applied: usize,
    pub bandwidth_saved_mb: f64,
    pub latency_reduction_ms: f64,
    pub sync_efficiency: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionAnalysisMetrics {
    pub transactions_analyzed: usize,
    pub patterns_detected: usize,
    pub predictions_made: usize,
    pub prediction_accuracy: f64,
}

pub struct AIMetricsCollector {
    metrics_history: Arc<RwLock<Vec<AIMetricsDashboard>>>,
    max_history: usize,
}

impl AIMetricsCollector {
    pub fn new(max_history: usize) -> Self {
        Self {
            metrics_history: Arc::new(RwLock::new(Vec::with_capacity(max_history))),
            max_history,
        }
    }

    /// Record a new metrics snapshot
    pub fn record_metrics(&self, metrics: AIMetricsDashboard) {
        let mut history = self.metrics_history.write();
        history.push(metrics);

        // Keep only recent history
        if history.len() > self.max_history {
            history.remove(0);
        }
    }

    /// Get latest metrics
    pub fn get_latest(&self) -> Option<AIMetricsDashboard> {
        self.metrics_history.read().last().cloned()
    }

    /// Get metrics over time range
    pub fn get_metrics_since(&self, since: Duration) -> Vec<AIMetricsDashboard> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let cutoff = now.saturating_sub(since.as_secs());

        self.metrics_history
            .read()
            .iter()
            .filter(|m| m.timestamp >= cutoff)
            .cloned()
            .collect()
    }

    /// Calculate aggregate statistics over time range
    pub fn get_aggregate_stats(&self, since: Duration) -> Result<AggregateAIStats, AppError> {
        let metrics = self.get_metrics_since(since);

        if metrics.is_empty() {
            return Err(AppError::Initialization("No metrics available".to_string()));
        }

        let count = metrics.len() as f64;

        Ok(AggregateAIStats {
            time_range_secs: since.as_secs(),
            samples: metrics.len(),
            avg_ai_health: metrics.iter().map(|m| m.overall_ai_health).sum::<f64>() / count,
            avg_consensus_score: metrics
                .iter()
                .map(|m| m.consensus_health.consensus_score)
                .sum::<f64>()
                / count,
            avg_peer_score: metrics
                .iter()
                .map(|m| m.peer_selection.avg_peer_score)
                .sum::<f64>()
                / count,
            total_anomalies: metrics
                .iter()
                .map(|m| m.anomaly_detection.anomalies_detected)
                .sum(),
            total_attacks: metrics
                .iter()
                .map(|m| m.attack_detection.attacks_detected)
                .sum(),
            total_attacks_mitigated: metrics
                .iter()
                .map(|m| m.attack_detection.attacks_mitigated)
                .sum(),
            avg_detection_accuracy: metrics
                .iter()
                .map(|m| m.anomaly_detection.detection_accuracy)
                .sum::<f64>()
                / count,
            avg_prediction_accuracy: metrics
                .iter()
                .map(|m| m.transaction_analysis.prediction_accuracy)
                .sum::<f64>()
                / count,
        })
    }

    /// Get performance trend (improving, stable, degrading)
    pub fn get_trend(&self, window: usize) -> Trend {
        let history = self.metrics_history.read();

        if history.len() < window {
            return Trend::Insufficient;
        }

        let recent = &history[history.len().saturating_sub(window)..];
        let first_half = &recent[..window / 2];
        let second_half = &recent[window / 2..];

        let avg_first: f64 =
            first_half.iter().map(|m| m.overall_ai_health).sum::<f64>() / first_half.len() as f64;
        let avg_second: f64 =
            second_half.iter().map(|m| m.overall_ai_health).sum::<f64>() / second_half.len() as f64;

        let diff = avg_second - avg_first;

        if diff > 0.05 {
            Trend::Improving
        } else if diff < -0.05 {
            Trend::Degrading
        } else {
            Trend::Stable
        }
    }

    /// Generate text summary report
    pub fn generate_report(&self) -> String {
        let latest = match self.get_latest() {
            Some(m) => m,
            None => return "No AI metrics available".to_string(),
        };

        let trend = self.get_trend(10);

        format!(
            r#"
╔══════════════════════════════════════════════════════════════╗
║                  AI SYSTEMS HEALTH REPORT                    ║
╚══════════════════════════════════════════════════════════════╝

Overall AI Health: {:.1}% {}
Trend: {:?}

┌─ ANOMALY DETECTION ─────────────────────────────────────────┐
│ Anomalies Detected:     {}
│ Detection Accuracy:     {:.1}%
│ Avg Detection Time:     {:.2}ms
└─────────────────────────────────────────────────────────────┘

┌─ ATTACK DETECTION ──────────────────────────────────────────┐
│ Attacks Detected:       {} ({} critical)
│ Attacks Mitigated:      {}
│ Mitigation Success:     {:.1}%
└─────────────────────────────────────────────────────────────┘

┌─ PEER SELECTION ────────────────────────────────────────────┐
│ Peers Scored:           {}
│ Selection Accuracy:     {:.1}%
│ Bad Peers Filtered:     {}
│ Peer Diversity:         {:.1}%
└─────────────────────────────────────────────────────────────┘

┌─ CONSENSUS HEALTH ──────────────────────────────────────────┐
│ Consensus Score:        {:.1}%
│ Fork Probability:       {:.1}%
│ Network Agreement:      {:.1}%
│ Prediction Accuracy:    {:.1}%
└─────────────────────────────────────────────────────────────┘

┌─ NETWORK OPTIMIZATION ──────────────────────────────────────┐
│ Optimizations Applied:  {}
│ Bandwidth Saved:        {:.2} MB
│ Latency Reduction:      {:.2} ms
│ Sync Efficiency:        {:.1}%
└─────────────────────────────────────────────────────────────┘

┌─ TRANSACTION ANALYSIS ──────────────────────────────────────┐
│ Transactions Analyzed:  {}
│ Patterns Detected:      {}
│ Predictions Made:       {}
│ Prediction Accuracy:    {:.1}%
└─────────────────────────────────────────────────────────────┘
            "#,
            latest.overall_ai_health * 100.0,
            if latest.overall_ai_health > 0.8 {
                "✅"
            } else if latest.overall_ai_health > 0.6 {
                "⚠️"
            } else {
                "❌"
            },
            trend,
            latest.anomaly_detection.anomalies_detected,
            latest.anomaly_detection.detection_accuracy * 100.0,
            latest.anomaly_detection.avg_detection_time_ms,
            latest.attack_detection.attacks_detected,
            latest.attack_detection.critical_attacks,
            latest.attack_detection.attacks_mitigated,
            latest.attack_detection.mitigation_success_rate * 100.0,
            latest.peer_selection.peers_scored,
            latest.peer_selection.selection_accuracy * 100.0,
            latest.peer_selection.bad_peers_filtered,
            latest.peer_selection.peer_diversity_score * 100.0,
            latest.consensus_health.consensus_score * 100.0,
            latest.consensus_health.fork_probability * 100.0,
            latest.consensus_health.network_agreement * 100.0,
            latest.consensus_health.prediction_accuracy * 100.0,
            latest.network_optimization.optimizations_applied,
            latest.network_optimization.bandwidth_saved_mb,
            latest.network_optimization.latency_reduction_ms,
            latest.network_optimization.sync_efficiency * 100.0,
            latest.transaction_analysis.transactions_analyzed,
            latest.transaction_analysis.patterns_detected,
            latest.transaction_analysis.predictions_made,
            latest.transaction_analysis.prediction_accuracy * 100.0,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateAIStats {
    pub time_range_secs: u64,
    pub samples: usize,
    pub avg_ai_health: f64,
    pub avg_consensus_score: f64,
    pub avg_peer_score: f64,
    pub total_anomalies: usize,
    pub total_attacks: usize,
    pub total_attacks_mitigated: usize,
    pub avg_detection_accuracy: f64,
    pub avg_prediction_accuracy: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Trend {
    Improving,
    Stable,
    Degrading,
    Insufficient,
}

impl Default for AIMetricsDashboard {
    fn default() -> Self {
        Self {
            timestamp: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            anomaly_detection: AnomalyMetrics {
                anomalies_detected: 0,
                false_positive_rate: 0.0,
                detection_accuracy: 0.0,
                avg_detection_time_ms: 0.0,
            },
            attack_detection: AttackMetrics {
                attacks_detected: 0,
                attacks_mitigated: 0,
                critical_attacks: 0,
                attack_types: HashMap::new(),
                mitigation_success_rate: 0.0,
            },
            peer_selection: PeerSelectionMetrics {
                peers_scored: 0,
                selection_accuracy: 0.0,
                avg_peer_score: 0.0,
                bad_peers_filtered: 0,
                peer_diversity_score: 0.0,
            },
            consensus_health: ConsensusHealthMetrics {
                consensus_score: 0.0,
                fork_probability: 0.0,
                network_agreement: 0.0,
                health_predictions_made: 0,
                prediction_accuracy: 0.0,
            },
            network_optimization: NetworkOptimizationMetrics {
                optimizations_applied: 0,
                bandwidth_saved_mb: 0.0,
                latency_reduction_ms: 0.0,
                sync_efficiency: 0.0,
            },
            transaction_analysis: TransactionAnalysisMetrics {
                transactions_analyzed: 0,
                patterns_detected: 0,
                predictions_made: 0,
                prediction_accuracy: 0.0,
            },
            overall_ai_health: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_collector() {
        let collector = AIMetricsCollector::new(100);

        let metrics = AIMetricsDashboard::default();
        collector.record_metrics(metrics.clone());

        let latest = collector.get_latest();
        assert!(latest.is_some());
        assert_eq!(latest.unwrap().timestamp, metrics.timestamp);
    }

    #[test]
    fn test_trend_detection() {
        let collector = AIMetricsCollector::new(100);

        // Add improving metrics
        for i in 0..10 {
            let mut metrics = AIMetricsDashboard::default();
            metrics.overall_ai_health = 0.5 + (i as f64 * 0.05);
            collector.record_metrics(metrics);
        }

        let trend = collector.get_trend(10);
        assert!(matches!(trend, Trend::Improving));
    }
}
