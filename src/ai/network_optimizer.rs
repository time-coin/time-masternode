use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

// Use parking_lot::RwLock instead of std::sync::RwLock
// parking_lot RwLock doesn't poison on panic, making it safer for production
use parking_lot::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkMetrics {
    pub timestamp: u64,
    pub active_connections: usize,
    pub bandwidth_usage: u64,
    pub avg_latency_ms: f64,
    pub message_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationSuggestion {
    pub timestamp: u64,
    pub suggestion_type: String,
    pub description: String,
    pub impact_score: f64,
}

pub struct NetworkOptimizer {
    _db: Arc<Db>,
    metrics_history: Arc<RwLock<VecDeque<NetworkMetrics>>>,
    suggestions: Arc<RwLock<Vec<OptimizationSuggestion>>>,
    min_samples: usize,
}

impl NetworkOptimizer {
    pub fn new(db: Arc<Db>, min_samples: usize) -> Result<Self, AppError> {
        Ok(Self {
            _db: db,
            metrics_history: Arc::new(RwLock::new(VecDeque::with_capacity(1000))),
            suggestions: Arc::new(RwLock::new(Vec::new())),
            min_samples,
        })
    }

    pub fn record_metrics(&self, metrics: NetworkMetrics) {
        let mut history_lock = self.metrics_history.write();
        history_lock.push_back(metrics.clone());

        if history_lock.len() > 1000 {
            history_lock.pop_front();
        }

        drop(history_lock);

        if let Some(suggestion) = self.analyze_and_suggest() {
            self.suggestions.write().push(suggestion);
        }
    }

    fn analyze_and_suggest(&self) -> Option<OptimizationSuggestion> {
        let history_lock = self.metrics_history.read();

        if history_lock.len() < self.min_samples {
            return None;
        }

        let recent: Vec<&NetworkMetrics> = history_lock.iter().rev().take(20).collect();

        let avg_connections: f64 = recent
            .iter()
            .map(|m| m.active_connections as f64)
            .sum::<f64>()
            / recent.len() as f64;
        let avg_latency: f64 =
            recent.iter().map(|m| m.avg_latency_ms).sum::<f64>() / recent.len() as f64;
        let avg_bandwidth: f64 =
            recent.iter().map(|m| m.bandwidth_usage as f64).sum::<f64>() / recent.len() as f64;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if avg_connections < 3.0 {
            return Some(OptimizationSuggestion {
                timestamp: now,
                suggestion_type: "low_peer_count".to_string(),
                description: format!(
                    "Low peer count detected ({:.1} avg). Consider adding more peers for better network resilience.",
                    avg_connections
                ),
                impact_score: 0.8,
            });
        }

        if avg_latency > 500.0 {
            return Some(OptimizationSuggestion {
                timestamp: now,
                suggestion_type: "high_latency".to_string(),
                description: format!(
                    "High average latency detected ({:.1}ms). Consider connecting to geographically closer peers.",
                    avg_latency
                ),
                impact_score: 0.7,
            });
        }

        if avg_bandwidth > 10_000_000.0 {
            return Some(OptimizationSuggestion {
                timestamp: now,
                suggestion_type: "high_bandwidth".to_string(),
                description: format!(
                    "High bandwidth usage detected ({:.2} MB/s). Consider optimizing message compression or reducing peer count.",
                    avg_bandwidth / 1_000_000.0
                ),
                impact_score: 0.6,
            });
        }

        None
    }

    pub fn get_recent_suggestions(&self, limit: usize) -> Vec<OptimizationSuggestion> {
        let suggestions_lock = self.suggestions.read();
        let len = suggestions_lock.len();
        if len <= limit {
            suggestions_lock.clone()
        } else {
            suggestions_lock[len - limit..].to_vec()
        }
    }

    pub fn get_network_health_score(&self) -> f64 {
        let history_lock = self.metrics_history.read();

        if history_lock.len() < self.min_samples {
            return 0.5;
        }

        let recent: Vec<&NetworkMetrics> = history_lock.iter().rev().take(20).collect();

        let avg_connections: f64 = recent
            .iter()
            .map(|m| m.active_connections as f64)
            .sum::<f64>()
            / recent.len() as f64;
        let avg_latency: f64 =
            recent.iter().map(|m| m.avg_latency_ms).sum::<f64>() / recent.len() as f64;

        let connection_score = (avg_connections / 10.0).min(1.0);
        let latency_score = (1.0 / (1.0 + avg_latency / 100.0)).clamp(0.0, 1.0);

        connection_score * 0.6 + latency_score * 0.4
    }

    pub fn get_statistics(&self) -> HashMap<String, f64> {
        let history_lock = self.metrics_history.read();

        if history_lock.is_empty() {
            return HashMap::new();
        }

        let recent: Vec<&NetworkMetrics> = history_lock.iter().rev().take(100).collect();

        let avg_connections: f64 = recent
            .iter()
            .map(|m| m.active_connections as f64)
            .sum::<f64>()
            / recent.len() as f64;
        let avg_latency: f64 =
            recent.iter().map(|m| m.avg_latency_ms).sum::<f64>() / recent.len() as f64;
        let avg_bandwidth: f64 =
            recent.iter().map(|m| m.bandwidth_usage as f64).sum::<f64>() / recent.len() as f64;
        let avg_message_rate: f64 =
            recent.iter().map(|m| m.message_rate).sum::<f64>() / recent.len() as f64;

        let mut stats = HashMap::new();
        stats.insert("avg_connections".to_string(), avg_connections);
        stats.insert("avg_latency_ms".to_string(), avg_latency);
        stats.insert("avg_bandwidth_bytes".to_string(), avg_bandwidth);
        stats.insert("avg_message_rate".to_string(), avg_message_rate);
        stats.insert("health_score".to_string(), self.get_network_health_score());

        stats
    }
}
