use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkEvent {
    pub timestamp: u64,
    pub event_type: String,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyReport {
    pub timestamp: u64,
    pub event_type: String,
    pub value: f64,
    pub expected_mean: f64,
    pub expected_std: f64,
    pub z_score: f64,
    pub severity: AnomalySeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AnomalySeverity {
    Low,
    Medium,
    High,
    Critical,
}

pub struct AnomalyDetector {
    _db: Arc<Db>,
    events: Arc<RwLock<VecDeque<NetworkEvent>>>,
    threshold: f64,
    min_samples: usize,
    anomalies: Arc<RwLock<Vec<AnomalyReport>>>,
}

impl AnomalyDetector {
    pub fn new(db: Arc<Db>, threshold: f64, min_samples: usize) -> Result<Self, AppError> {
        let detector = Self {
            _db: db,
            events: Arc::new(RwLock::new(VecDeque::with_capacity(1000))),
            threshold,
            min_samples,
            anomalies: Arc::new(RwLock::new(Vec::new())),
        };

        Ok(detector)
    }

    pub fn record_event(&self, event_type: String, value: f64) {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let event = NetworkEvent {
            timestamp,
            event_type: event_type.clone(),
            value,
        };

        let mut events_lock = self.events.write().unwrap();
        events_lock.push_back(event);

        if events_lock.len() > 1000 {
            events_lock.pop_front();
        }

        drop(events_lock);

        if let Some(anomaly) = self.detect_anomaly(&event_type, value) {
            self.anomalies.write().unwrap().push(anomaly);
        }
    }

    fn detect_anomaly(&self, event_type: &str, value: f64) -> Option<AnomalyReport> {
        let events_lock = self.events.read().unwrap();

        let matching: Vec<f64> = events_lock
            .iter()
            .filter(|e| e.event_type == event_type)
            .map(|e| e.value)
            .collect();

        if matching.len() < self.min_samples {
            return None;
        }

        let mean = matching.iter().sum::<f64>() / matching.len() as f64;
        let variance =
            matching.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / matching.len() as f64;
        let std_dev = variance.sqrt();

        if std_dev == 0.0 {
            return None;
        }

        let z_score = (value - mean) / std_dev;

        if z_score.abs() > self.threshold {
            let severity = if z_score.abs() > 4.0 {
                AnomalySeverity::Critical
            } else if z_score.abs() > 3.0 {
                AnomalySeverity::High
            } else if z_score.abs() > 2.5 {
                AnomalySeverity::Medium
            } else {
                AnomalySeverity::Low
            };

            Some(AnomalyReport {
                timestamp: SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                event_type: event_type.to_string(),
                value,
                expected_mean: mean,
                expected_std: std_dev,
                z_score,
                severity,
            })
        } else {
            None
        }
    }

    pub fn get_recent_anomalies(&self, limit: usize) -> Vec<AnomalyReport> {
        let anomalies_lock = self.anomalies.read().unwrap();
        let len = anomalies_lock.len();
        if len <= limit {
            anomalies_lock.clone()
        } else {
            anomalies_lock[len - limit..].to_vec()
        }
    }

    pub fn get_anomaly_count(&self) -> usize {
        self.anomalies.read().unwrap().len()
    }

    pub fn is_suspicious_activity(&self, event_type: &str, window_secs: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let anomalies_lock = self.anomalies.read().unwrap();
        let recent_anomalies = anomalies_lock
            .iter()
            .filter(|a| {
                a.event_type == event_type && now.saturating_sub(a.timestamp) <= window_secs
            })
            .count();

        recent_anomalies >= 3
    }
}
