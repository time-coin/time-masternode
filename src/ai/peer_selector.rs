use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;

// Use parking_lot::RwLock instead of std::sync::RwLock
// parking_lot RwLock doesn't poison on panic, making it safer for production
use parking_lot::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerPerformance {
    pub address: String,
    pub success_count: u64,
    pub failure_count: u64,
    pub avg_response_time: f64,
    pub last_success: u64,
    pub reliability_score: f64,
    pub bandwidth_score: f64,
    pub latency_score: f64,
    pub total_score: f64,
}

impl PeerPerformance {
    pub fn new(address: String) -> Self {
        Self {
            address,
            success_count: 0,
            failure_count: 0,
            avg_response_time: 0.0,
            last_success: 0,
            reliability_score: 0.5,
            bandwidth_score: 0.5,
            latency_score: 0.5,
            total_score: 0.5,
        }
    }

    pub fn calculate_scores(&mut self) {
        let total_attempts = self.success_count + self.failure_count;
        if total_attempts == 0 {
            return;
        }

        self.reliability_score = self.success_count as f64 / total_attempts as f64;

        self.latency_score = if self.avg_response_time > 0.0 {
            1.0 / (1.0 + self.avg_response_time / 1000.0)
        } else {
            0.5
        };

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let time_since_success = if self.last_success > 0 {
            now.saturating_sub(self.last_success)
        } else {
            3600
        };
        let recency_score = 1.0 / (1.0 + time_since_success as f64 / 3600.0);

        self.total_score =
            (self.reliability_score * 0.5) + (self.latency_score * 0.3) + (recency_score * 0.2);
    }
}

pub struct AIPeerSelector {
    db: Arc<Db>,
    performance: Arc<RwLock<HashMap<String, PeerPerformance>>>,
    learning_rate: f64,
}

impl AIPeerSelector {
    pub fn new(db: Arc<Db>, learning_rate: f64) -> Result<Self, AppError> {
        let selector = Self {
            db: db.clone(),
            performance: Arc::new(RwLock::new(HashMap::new())),
            learning_rate,
        };

        selector.load_from_db()?;
        Ok(selector)
    }

    fn load_from_db(&self) -> Result<(), AppError> {
        let prefix = b"ai_peer_";
        for result in self.db.scan_prefix(prefix) {
            let (key, value) = result.map_err(|e| {
                AppError::Storage(crate::error::StorageError::DatabaseOp(format!(
                    "Failed to scan AI peer data: {}",
                    e
                )))
            })?;

            let address = String::from_utf8_lossy(&key[prefix.len()..]).to_string();
            let perf: PeerPerformance = bincode::deserialize(&value).map_err(|e| {
                AppError::Storage(crate::error::StorageError::Serialization(format!(
                    "Failed to deserialize peer performance: {}",
                    e
                )))
            })?;

            self.performance.write().insert(address, perf);
        }

        Ok(())
    }

    pub fn save_to_db(&self) -> Result<(), AppError> {
        let perf_lock = self.performance.read();
        for (address, perf) in perf_lock.iter() {
            let key = format!("ai_peer_{}", address);
            let value = bincode::serialize(perf).map_err(|e| {
                AppError::Storage(crate::error::StorageError::Serialization(format!(
                    "Failed to serialize peer performance: {}",
                    e
                )))
            })?;
            self.db.insert(key.as_bytes(), value).map_err(|e| {
                AppError::Storage(crate::error::StorageError::DatabaseOp(format!(
                    "Failed to save peer performance: {}",
                    e
                )))
            })?;
        }
        self.db.flush().map_err(|e| {
            AppError::Storage(crate::error::StorageError::DatabaseOp(format!(
                "Failed to flush AI peer data: {}",
                e
            )))
        })?;
        Ok(())
    }

    pub fn record_success(&self, peer: &SocketAddr, response_time_ms: f64) {
        let address = peer.to_string();
        let mut perf_lock = self.performance.write();
        let perf = perf_lock
            .entry(address.clone())
            .or_insert_with(|| PeerPerformance::new(address));

        perf.success_count += 1;

        let new_avg = if perf.avg_response_time == 0.0 {
            response_time_ms
        } else {
            perf.avg_response_time * (1.0 - self.learning_rate)
                + response_time_ms * self.learning_rate
        };
        perf.avg_response_time = new_avg;

        perf.last_success = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        perf.calculate_scores();
    }

    pub fn record_failure(&self, peer: &SocketAddr) {
        let address = peer.to_string();
        let mut perf_lock = self.performance.write();
        let perf = perf_lock
            .entry(address.clone())
            .or_insert_with(|| PeerPerformance::new(address));

        perf.failure_count += 1;
        perf.calculate_scores();
    }

    pub fn select_best_peer(&self, candidates: &[SocketAddr]) -> Option<SocketAddr> {
        if candidates.is_empty() {
            return None;
        }

        let perf_lock = self.performance.read();
        let mut best_peer: Option<(SocketAddr, f64)> = None;

        for candidate in candidates {
            let score = perf_lock
                .get(&candidate.to_string())
                .map(|p| p.total_score)
                .unwrap_or(0.5);

            match best_peer {
                None => best_peer = Some((*candidate, score)),
                Some((_, best_score)) if score > best_score => {
                    best_peer = Some((*candidate, score))
                }
                _ => {}
            }
        }

        best_peer.map(|(peer, _)| peer)
    }

    pub fn get_peer_score(&self, peer: &SocketAddr) -> f64 {
        self.performance
            .read()
            .get(&peer.to_string())
            .map(|p| p.total_score)
            .unwrap_or(0.5)
    }

    pub fn get_top_peers(&self, limit: usize) -> Vec<(String, f64)> {
        let perf_lock = self.performance.read();
        let mut peers: Vec<_> = perf_lock
            .iter()
            .map(|(addr, perf)| (addr.clone(), perf.total_score))
            .collect();

        peers.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        peers.truncate(limit);
        peers
    }

    pub fn get_statistics(&self) -> HashMap<String, f64> {
        let perf_lock = self.performance.read();
        let total_peers = perf_lock.len() as f64;

        if total_peers == 0.0 {
            return HashMap::new();
        }

        let total_success: u64 = perf_lock.values().map(|p| p.success_count).sum();
        let total_failures: u64 = perf_lock.values().map(|p| p.failure_count).sum();
        let avg_score: f64 = perf_lock.values().map(|p| p.total_score).sum::<f64>() / total_peers;

        let mut stats = HashMap::new();
        stats.insert("total_peers".to_string(), total_peers);
        stats.insert("total_successes".to_string(), total_success as f64);
        stats.insert("total_failures".to_string(), total_failures as f64);
        stats.insert("average_score".to_string(), avg_score);

        stats
    }
}
