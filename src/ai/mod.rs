pub mod adaptive_reconnection;
pub mod anomaly_detector;
pub mod attack_detector;
pub mod consensus_health;
pub mod fork_resolver;
pub mod metrics_dashboard;
pub mod network_optimizer;
pub mod peer_selector;
pub mod predictive_sync;
pub mod transaction_validator;

pub use adaptive_reconnection::{AdaptiveReconnectionAI, ReconnectionAdvice, ReconnectionPriority};
pub use anomaly_detector::AnomalyDetector;
pub use attack_detector::{
    AttackDetector, AttackPattern, AttackSeverity, AttackType, MitigationAction,
};
pub use consensus_health::{
    ConsensusHealthMonitor, ConsensusMetrics, HealthPrediction as ConsensusHealthPrediction,
};
pub use fork_resolver::ForkResolver;
pub use metrics_dashboard::AIMetricsDashboard;
pub use network_optimizer::NetworkOptimizer;
pub use peer_selector::AIPeerSelector;
pub use predictive_sync::PredictiveSync;
pub use transaction_validator::AITransactionValidator;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIConfig {
    pub enabled: bool,
    pub learning_rate: f64,
    pub min_samples: usize,
    pub anomaly_threshold: f64,
    pub prediction_window: u64,
}

impl Default for AIConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            learning_rate: 0.1,
            min_samples: 10,
            anomaly_threshold: 2.0,
            prediction_window: 300, // 5 minutes
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AIMetrics {
    pub timestamp: u64,
    pub peer_selection_accuracy: f64,
    pub anomaly_detections: usize,
    pub transaction_predictions: usize,
    pub network_optimizations: usize,
}

impl AIMetrics {
    pub fn new() -> Self {
        Self {
            timestamp: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            peer_selection_accuracy: 0.0,
            anomaly_detections: 0,
            transaction_predictions: 0,
            network_optimizations: 0,
        }
    }
}

impl Default for AIMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Centralized AI System that holds all AI modules.
///
/// This is the main entry point for all AI-driven decision making in TimeCoin.
/// Each module provides a specific intelligence capability:
/// - AnomalyDetector: Z-score statistical anomaly detection on network events
/// - AttackDetector: Detects sybil, eclipse, fork bombing, and timing attacks
/// - AdaptiveReconnectionAI: Learns optimal peer reconnection strategies
/// - AIPeerSelector: Scores and selects best peers for sync
/// - PredictiveSync: Predicts next block timing for prefetch optimization
/// - NetworkOptimizer: Analyzes connection/bandwidth metrics for suggestions
/// - AIMetricsCollector: Aggregates all AI subsystem metrics for dashboard
pub struct AISystem {
    pub anomaly_detector: Arc<anomaly_detector::AnomalyDetector>,
    pub attack_detector: Arc<attack_detector::AttackDetector>,
    pub reconnection_ai: Arc<adaptive_reconnection::AdaptiveReconnectionAI>,
    pub peer_selector: Arc<peer_selector::AIPeerSelector>,
    pub predictive_sync: Arc<predictive_sync::PredictiveSync>,
    pub network_optimizer: Arc<network_optimizer::NetworkOptimizer>,
    pub metrics_collector: Arc<metrics_dashboard::AIMetricsCollector>,
}

impl AISystem {
    /// Initialize all AI modules with shared database storage.
    pub fn new(db: Arc<sled::Db>) -> Result<Self, crate::error::AppError> {
        let anomaly_detector =
            Arc::new(anomaly_detector::AnomalyDetector::new(db.clone(), 2.0, 10)?);
        let attack_detector = Arc::new(attack_detector::AttackDetector::new(db.clone())?);
        let reconnection_ai = Arc::new(adaptive_reconnection::AdaptiveReconnectionAI::new(
            adaptive_reconnection::ReconnectionConfig::default(),
        ));
        let peer_selector = Arc::new(peer_selector::AIPeerSelector::new(db.clone(), 0.1)?);
        let predictive_sync = Arc::new(predictive_sync::PredictiveSync::new(db.clone(), 5)?);
        let network_optimizer = Arc::new(network_optimizer::NetworkOptimizer::new(db.clone(), 10)?);
        let metrics_collector = Arc::new(metrics_dashboard::AIMetricsCollector::new(1000));

        tracing::info!("🧠 AI System initialized with 7 active modules");

        Ok(Self {
            anomaly_detector,
            attack_detector,
            reconnection_ai,
            peer_selector,
            predictive_sync,
            network_optimizer,
            metrics_collector,
        })
    }

    /// Collect metrics from all subsystems and record a dashboard snapshot.
    pub fn collect_and_record_metrics(&self) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let attack_stats = self.attack_detector.get_statistics();
        let anomaly_count = self.anomaly_detector.get_anomaly_count();
        let peer_stats = self.peer_selector.get_statistics();
        let network_stats = self.network_optimizer.get_statistics();

        let dashboard = metrics_dashboard::AIMetricsDashboard {
            timestamp: now,
            anomaly_detection: metrics_dashboard::AnomalyMetrics {
                anomalies_detected: anomaly_count,
                false_positive_rate: 0.0,
                detection_accuracy: if anomaly_count > 0 { 0.8 } else { 0.0 },
                avg_detection_time_ms: 0.0,
            },
            attack_detection: metrics_dashboard::AttackMetrics {
                attacks_detected: attack_stats.total_attacks,
                attacks_mitigated: 0,
                critical_attacks: attack_stats.critical_count,
                attack_types: attack_stats.by_type,
                mitigation_success_rate: 0.0,
            },
            peer_selection: metrics_dashboard::PeerSelectionMetrics {
                peers_scored: *peer_stats.get("total_peers").unwrap_or(&0.0) as usize,
                selection_accuracy: 0.0,
                avg_peer_score: *peer_stats.get("average_score").unwrap_or(&0.5),
                bad_peers_filtered: 0,
                peer_diversity_score: 0.0,
            },
            consensus_health: metrics_dashboard::ConsensusHealthMetrics {
                consensus_score: 0.0,
                fork_probability: 0.0,
                network_agreement: 0.0,
                health_predictions_made: 0,
                prediction_accuracy: 0.0,
            },
            network_optimization: metrics_dashboard::NetworkOptimizationMetrics {
                optimizations_applied: 0,
                bandwidth_saved_mb: 0.0,
                latency_reduction_ms: 0.0,
                sync_efficiency: *network_stats.get("health_score").unwrap_or(&0.5),
            },
            transaction_analysis: metrics_dashboard::TransactionAnalysisMetrics {
                transactions_analyzed: 0,
                patterns_detected: 0,
                predictions_made: 0,
                prediction_accuracy: 0.0,
            },
            overall_ai_health: self.calculate_overall_health(),
        };

        self.metrics_collector.record_metrics(dashboard);
    }

    /// Generate a text report of all AI systems.
    pub fn generate_report(&self) -> String {
        self.metrics_collector.generate_report()
    }

    /// Get a brief one-line status summary for periodic logging.
    pub fn brief_status(&self) -> String {
        let attack_stats = self.attack_detector.get_statistics();
        let anomaly_count = self.anomaly_detector.get_anomaly_count();
        let reconnection_stats = self.reconnection_ai.get_stats();
        let network_health = self.network_optimizer.get_network_health_score();
        let sync_health = self.predictive_sync.get_sync_health();

        format!(
            "anomalies={}, attacks={} ({}crit), peers={} (rel={:.0}%), net_health={:.0}%, sync_health={:.0}%",
            anomaly_count,
            attack_stats.total_attacks,
            attack_stats.critical_count,
            reconnection_stats.total_peers,
            reconnection_stats.avg_reliability * 100.0,
            network_health * 100.0,
            sync_health * 100.0,
        )
    }

    fn calculate_overall_health(&self) -> f64 {
        let network_health = self.network_optimizer.get_network_health_score();
        let sync_health = self.predictive_sync.get_sync_health();
        let attack_stats = self.attack_detector.get_statistics();

        // Penalize for active critical attacks
        let attack_penalty = if attack_stats.critical_count > 0 {
            0.3
        } else if attack_stats.total_attacks > 5 {
            0.1
        } else {
            0.0
        };

        ((network_health * 0.4 + sync_health * 0.4 + 0.2) - attack_penalty).clamp(0.0, 1.0)
    }
}
