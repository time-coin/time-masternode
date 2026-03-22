use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::RwLock;

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

/// Per-event-type EMA state for O(1) mean/variance tracking.
///
/// Uses the standard online EMA update:
///   mean  ← (1−α)·mean + α·x
///   var   ← (1−α)·(var + α·(x − prev_mean)²)
///
/// This handles non-stationary distributions (e.g. gradual drift in message
/// rates as the network grows) without needing to store the full history.
#[derive(Debug, Clone)]
struct EmaState {
    mean: f64,
    variance: f64,
    count: u64,
}

impl EmaState {
    fn new(first_value: f64) -> Self {
        Self {
            mean: first_value,
            variance: 0.0,
            count: 1,
        }
    }

    /// Update with a new observation; returns (current_mean, current_std_dev).
    fn update(&mut self, value: f64, alpha: f64) -> (f64, f64) {
        let prev_mean = self.mean;
        self.mean = (1.0 - alpha) * self.mean + alpha * value;
        let delta = value - prev_mean;
        self.variance = (1.0 - alpha) * (self.variance + alpha * delta * delta);
        self.count += 1;
        (self.mean, self.variance.sqrt())
    }
}

/// Anomaly history TTL: reports older than this are silently dropped.
const ANOMALY_TTL_SECS: u64 = 86_400; // 24 hours
const MAX_ANOMALY_HISTORY: usize = 2_000;

pub struct AnomalyDetector {
    _db: Arc<Db>,
    /// Rolling event buffer (for external stats queries).
    events: Arc<RwLock<VecDeque<NetworkEvent>>>,
    /// Z-score threshold for flagging an anomaly.
    threshold: f64,
    /// Minimum samples before the EMA is considered warmed-up.
    min_samples: usize,
    /// Bounded, time-expiring anomaly history.
    anomalies: Arc<RwLock<VecDeque<AnomalyReport>>>,
    /// O(1) EMA state per event type.
    ema_states: Arc<RwLock<HashMap<String, EmaState>>>,
    /// EMA learning rate α.  Lower = smoother baseline; higher = faster adaptation.
    alpha: f64,
}

impl AnomalyDetector {
    pub fn new(db: Arc<Db>, threshold: f64, min_samples: usize) -> Result<Self, AppError> {
        Ok(Self {
            _db: db,
            events: Arc::new(RwLock::new(VecDeque::with_capacity(1000))),
            threshold,
            min_samples,
            anomalies: Arc::new(RwLock::new(VecDeque::new())),
            ema_states: Arc::new(RwLock::new(HashMap::new())),
            alpha: 0.05, // 5% weight per sample — responsive but not jumpy
        })
    }

    pub fn record_event(&self, event_type: String, value: f64) {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Maintain event ring-buffer for callers that query raw history.
        let mut events_lock = self.events.write();
        events_lock.push_back(NetworkEvent {
            timestamp,
            event_type: event_type.clone(),
            value,
        });
        if events_lock.len() > 1000 {
            events_lock.pop_front();
        }
        drop(events_lock);

        if let Some(anomaly) = self.detect_anomaly_ema(&event_type, value, timestamp) {
            let mut anomalies = self.anomalies.write();

            // Expire stale entries from the front (they are insertion-ordered).
            while anomalies
                .front()
                .is_some_and(|a| timestamp.saturating_sub(a.timestamp) > ANOMALY_TTL_SECS)
            {
                anomalies.pop_front();
            }
            if anomalies.len() >= MAX_ANOMALY_HISTORY {
                anomalies.pop_front();
            }
            anomalies.push_back(anomaly);
        }
    }

    /// O(1) EMA-based anomaly detection.  Updates the per-type EMA and returns
    /// an `AnomalyReport` when the z-score exceeds the configured threshold.
    fn detect_anomaly_ema(
        &self,
        event_type: &str,
        value: f64,
        timestamp: u64,
    ) -> Option<AnomalyReport> {
        let mut states = self.ema_states.write();
        let state = states
            .entry(event_type.to_string())
            .or_insert_with(|| EmaState::new(value));

        if state.count < self.min_samples as u64 {
            // Still warming up — update EMA silently.
            state.update(value, self.alpha);
            return None;
        }

        let (mean, std_dev) = state.update(value, self.alpha);

        if std_dev == 0.0 {
            return None;
        }

        let z_score = (value - mean) / std_dev;
        if z_score.abs() <= self.threshold {
            return None;
        }

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
            timestamp,
            event_type: event_type.to_string(),
            value,
            expected_mean: mean,
            expected_std: std_dev,
            z_score,
            severity,
        })
    }

    pub fn get_recent_anomalies(&self, limit: usize) -> Vec<AnomalyReport> {
        let anomalies = self.anomalies.read();
        let len = anomalies.len();
        if len <= limit {
            anomalies.iter().cloned().collect()
        } else {
            anomalies.iter().skip(len - limit).cloned().collect()
        }
    }

    pub fn get_anomaly_count(&self) -> usize {
        self.anomalies.read().len()
    }

    pub fn is_suspicious_activity(&self, event_type: &str, window_secs: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let anomalies = self.anomalies.read();
        let recent = anomalies
            .iter()
            .filter(|a| {
                a.event_type == event_type && now.saturating_sub(a.timestamp) <= window_secs
            })
            .count();

        recent >= 3
    }
}
