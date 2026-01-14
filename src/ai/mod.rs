pub mod adaptive_reconnection;
pub mod anomaly_detector;
pub mod consensus_health;
pub mod fork_resolver;
pub mod masternode_health;
pub mod network_optimizer;
pub mod peer_selector;
pub mod predictive_sync;
pub mod resource_manager;
pub mod transaction_analyzer;
pub mod transaction_validator;

pub use adaptive_reconnection::{AdaptiveReconnectionAI, ReconnectionAdvice, ReconnectionPriority};
pub use anomaly_detector::AnomalyDetector;
pub use consensus_health::{
    ConsensusHealthMonitor, ConsensusMetrics, HealthPrediction as ConsensusHealthPrediction,
};
pub use fork_resolver::ForkResolver;
pub use masternode_health::{HealthPrediction, MasternodeHealthAI, NetworkHealth};
pub use network_optimizer::NetworkOptimizer;
pub use peer_selector::AIPeerSelector;
pub use predictive_sync::PredictiveSync;
pub use resource_manager::{AllocationStrategy, ResourceManager};
pub use transaction_analyzer::TransactionAnalyzer;
pub use transaction_validator::AITransactionValidator;

use serde::{Deserialize, Serialize};
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
