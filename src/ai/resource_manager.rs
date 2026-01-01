use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// AI-powered resource manager that learns optimal resource allocation
pub struct ResourceManager {
    _db: Arc<Db>,
    metrics: Arc<RwLock<ResourceMetrics>>,
    predictions: Arc<RwLock<HashMap<String, ResourcePrediction>>>,
}

#[derive(Default)]
struct ResourceMetrics {
    cpu_history: Vec<f64>,
    memory_history: Vec<f64>,
    network_history: Vec<f64>,
    disk_history: Vec<f64>,
}

pub struct ResourcePrediction {
    pub predicted_cpu: f64,
    pub predicted_memory: f64,
    pub predicted_network: f64,
    pub predicted_disk: f64,
    pub confidence: f64,
}

impl ResourceManager {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            _db: db,
            metrics: Arc::new(RwLock::new(ResourceMetrics::default())),
            predictions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record current resource usage
    pub async fn record_usage(&self, cpu: f64, memory: f64, network: f64, disk: f64) {
        let mut metrics = self.metrics.write().await;

        metrics.cpu_history.push(cpu);
        metrics.memory_history.push(memory);
        metrics.network_history.push(network);
        metrics.disk_history.push(disk);

        // Keep only last 1000 samples
        if metrics.cpu_history.len() > 1000 {
            metrics.cpu_history.remove(0);
            metrics.memory_history.remove(0);
            metrics.network_history.remove(0);
            metrics.disk_history.remove(0);
        }
    }

    /// Predict resource needs for an operation
    pub async fn predict_needs(&self, operation: &str) -> Option<ResourcePrediction> {
        let predictions = self.predictions.read().await;
        predictions.get(operation).cloned()
    }

    /// Learn from completed operation
    pub async fn learn_from_operation(
        &self,
        operation: String,
        cpu_used: f64,
        memory_used: f64,
        network_used: f64,
        disk_used: f64,
    ) {
        let mut predictions = self.predictions.write().await;

        let prediction = predictions
            .entry(operation.clone())
            .or_insert(ResourcePrediction {
                predicted_cpu: cpu_used,
                predicted_memory: memory_used,
                predicted_network: network_used,
                predicted_disk: disk_used,
                confidence: 0.5,
            });

        // Exponential moving average
        let alpha = 0.3;
        prediction.predicted_cpu = alpha * cpu_used + (1.0 - alpha) * prediction.predicted_cpu;
        prediction.predicted_memory =
            alpha * memory_used + (1.0 - alpha) * prediction.predicted_memory;
        prediction.predicted_network =
            alpha * network_used + (1.0 - alpha) * prediction.predicted_network;
        prediction.predicted_disk = alpha * disk_used + (1.0 - alpha) * prediction.predicted_disk;

        // Increase confidence as we learn
        prediction.confidence = (prediction.confidence + 0.1).min(1.0);
    }

    /// Check if system has enough resources for operation
    pub async fn can_handle_operation(&self, operation: &str) -> bool {
        let prediction = match self.predict_needs(operation).await {
            Some(p) => p,
            None => return true, // Unknown operation, allow it
        };

        let metrics = self.metrics.read().await;

        // Check if resources are available based on recent usage
        if let (Some(&recent_cpu), Some(&recent_mem), Some(&recent_net), Some(&recent_disk)) = (
            metrics.cpu_history.last(),
            metrics.memory_history.last(),
            metrics.network_history.last(),
            metrics.disk_history.last(),
        ) {
            // Allow operation if predicted usage + current usage < 90% capacity
            recent_cpu + prediction.predicted_cpu < 0.9
                && recent_mem + prediction.predicted_memory < 0.9
                && recent_net + prediction.predicted_network < 0.9
                && recent_disk + prediction.predicted_disk < 0.9
        } else {
            true // No history yet, allow operation
        }
    }

    /// Get recommended resource allocation strategy
    pub async fn get_allocation_strategy(&self) -> AllocationStrategy {
        let metrics = self.metrics.read().await;

        // Calculate average usage over recent history
        let avg_cpu = average(&metrics.cpu_history);
        let avg_memory = average(&metrics.memory_history);
        let avg_network = average(&metrics.network_history);
        let avg_disk = average(&metrics.disk_history);

        // Determine bottleneck
        let max_usage = avg_cpu.max(avg_memory).max(avg_network).max(avg_disk);

        if max_usage < 0.3 {
            AllocationStrategy::Aggressive
        } else if max_usage < 0.6 {
            AllocationStrategy::Balanced
        } else if max_usage < 0.8 {
            AllocationStrategy::Conservative
        } else {
            AllocationStrategy::Minimal
        }
    }

    /// Optimize concurrent operations based on available resources
    pub async fn optimize_concurrency(&self) -> usize {
        let metrics = self.metrics.read().await;

        let avg_cpu = average(&metrics.cpu_history);
        let avg_memory = average(&metrics.memory_history);

        // Scale concurrency based on resource availability
        if avg_cpu < 0.4 && avg_memory < 0.4 {
            16 // High concurrency
        } else if avg_cpu < 0.6 && avg_memory < 0.6 {
            8 // Medium concurrency
        } else if avg_cpu < 0.8 && avg_memory < 0.8 {
            4 // Low concurrency
        } else {
            2 // Minimal concurrency
        }
    }
}

impl Clone for ResourcePrediction {
    fn clone(&self) -> Self {
        Self {
            predicted_cpu: self.predicted_cpu,
            predicted_memory: self.predicted_memory,
            predicted_network: self.predicted_network,
            predicted_disk: self.predicted_disk,
            confidence: self.confidence,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AllocationStrategy {
    Aggressive,   // Low resource usage, can do more
    Balanced,     // Normal operation
    Conservative, // High usage, be careful
    Minimal,      // Critical usage, only essential operations
}

fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}
