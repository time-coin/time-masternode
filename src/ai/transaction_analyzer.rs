use crate::error::AppError;
use crate::types::Transaction;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionPattern {
    pub hour_of_day: u8,
    pub day_of_week: u8,
    pub avg_tx_count: f64,
    pub avg_tx_size: f64,
    pub avg_fee: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionPrediction {
    pub timestamp: u64,
    pub predicted_tx_count: f64,
    pub predicted_mempool_size: f64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeRecommendation {
    pub low_priority: u64,
    pub medium_priority: u64,
    pub high_priority: u64,
}

pub struct TransactionAnalyzer {
    db: Arc<Db>,
    tx_history: Arc<RwLock<VecDeque<(u64, usize, u64)>>>,
    patterns: Arc<RwLock<HashMap<(u8, u8), TransactionPattern>>>,
    min_samples: usize,
}

impl TransactionAnalyzer {
    pub fn new(db: Arc<Db>, min_samples: usize) -> Result<Self, AppError> {
        let analyzer = Self {
            db: db.clone(),
            tx_history: Arc::new(RwLock::new(VecDeque::with_capacity(10000))),
            patterns: Arc::new(RwLock::new(HashMap::new())),
            min_samples,
        };

        analyzer.load_patterns()?;
        Ok(analyzer)
    }

    fn load_patterns(&self) -> Result<(), AppError> {
        let prefix = b"ai_tx_pattern_";
        for result in self.db.scan_prefix(prefix) {
            let (key, value) = result.map_err(|e| {
                AppError::Storage(crate::error::StorageError::DatabaseOp(format!(
                    "Failed to scan transaction patterns: {}",
                    e
                )))
            })?;

            let key_str = String::from_utf8_lossy(&key[prefix.len()..]);
            let parts: Vec<&str> = key_str.split('_').collect();
            if parts.len() == 2 {
                if let (Ok(hour), Ok(day)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
                    let pattern: TransactionPattern =
                        bincode::deserialize(&value).map_err(|e| {
                            AppError::Storage(crate::error::StorageError::Serialization(format!(
                                "Failed to deserialize pattern: {}",
                                e
                            )))
                        })?;
                    self.patterns.write().unwrap().insert((hour, day), pattern);
                }
            }
        }

        Ok(())
    }

    pub fn save_patterns(&self) -> Result<(), AppError> {
        let patterns_lock = self.patterns.read().unwrap();
        for ((hour, day), pattern) in patterns_lock.iter() {
            let key = format!("ai_tx_pattern_{}_{}", hour, day);
            let value = bincode::serialize(pattern).map_err(|e| {
                AppError::Storage(crate::error::StorageError::Serialization(format!(
                    "Failed to serialize pattern: {}",
                    e
                )))
            })?;
            self.db.insert(key.as_bytes(), value).map_err(|e| {
                AppError::Storage(crate::error::StorageError::DatabaseOp(format!(
                    "Failed to save pattern: {}",
                    e
                )))
            })?;
        }
        self.db.flush().map_err(|e| {
            AppError::Storage(crate::error::StorageError::DatabaseOp(format!(
                "Failed to flush transaction patterns: {}",
                e
            )))
        })?;
        Ok(())
    }

    pub fn record_transaction_batch(&self, tx_count: usize, total_size: u64) {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut history_lock = self.tx_history.write().unwrap();
        history_lock.push_back((now, tx_count, total_size));

        if history_lock.len() > 10000 {
            history_lock.pop_front();
        }

        drop(history_lock);

        self.update_patterns(now, tx_count, total_size);
    }

    fn update_patterns(&self, timestamp: u64, tx_count: usize, total_size: u64) {
        use chrono::{DateTime, Datelike, Timelike, Utc};

        let dt = DateTime::<Utc>::from_timestamp(timestamp as i64, 0).unwrap();
        let hour = dt.hour() as u8;
        let day = dt.weekday().number_from_monday() as u8;

        let mut patterns_lock = self.patterns.write().unwrap();
        let pattern = patterns_lock
            .entry((hour, day))
            .or_insert(TransactionPattern {
                hour_of_day: hour,
                day_of_week: day,
                avg_tx_count: 0.0,
                avg_tx_size: 0.0,
                avg_fee: 0.0,
            });

        let alpha = 0.1;
        pattern.avg_tx_count = pattern.avg_tx_count * (1.0 - alpha) + tx_count as f64 * alpha;
        pattern.avg_tx_size = pattern.avg_tx_size * (1.0 - alpha) + total_size as f64 * alpha;
    }

    pub fn predict_load(&self, lookahead_secs: u64) -> Option<TransactionPrediction> {
        use chrono::{DateTime, Datelike, Timelike, Utc};

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let future = now + lookahead_secs;

        let dt = DateTime::<Utc>::from_timestamp(future as i64, 0).unwrap();
        let hour = dt.hour() as u8;
        let day = dt.weekday().number_from_monday() as u8;

        let patterns_lock = self.patterns.read().unwrap();
        let pattern = patterns_lock.get(&(hour, day))?;

        let history_lock = self.tx_history.read().unwrap();
        let samples = history_lock
            .iter()
            .filter(|(ts, _, _)| {
                let dt2 = DateTime::<Utc>::from_timestamp(*ts as i64, 0).unwrap();
                dt2.hour() as u8 == hour && dt2.weekday().number_from_monday() as u8 == day
            })
            .count();

        if samples < self.min_samples {
            return None;
        }

        let confidence = (samples as f64 / (self.min_samples as f64 * 2.0)).min(1.0);

        Some(TransactionPrediction {
            timestamp: future,
            predicted_tx_count: pattern.avg_tx_count,
            predicted_mempool_size: pattern.avg_tx_size,
            confidence,
        })
    }

    pub fn recommend_fee(&self) -> FeeRecommendation {
        let history_lock = self.tx_history.read().unwrap();

        if history_lock.is_empty() {
            return FeeRecommendation {
                low_priority: 1,
                medium_priority: 2,
                high_priority: 5,
            };
        }

        let recent_count: usize = history_lock
            .iter()
            .rev()
            .take(10)
            .map(|(_, count, _)| count)
            .sum();

        let avg_recent = recent_count as f64 / 10.0;

        let congestion_multiplier = if avg_recent > 100.0 {
            3.0
        } else if avg_recent > 50.0 {
            2.0
        } else if avg_recent > 20.0 {
            1.5
        } else {
            1.0
        };

        FeeRecommendation {
            low_priority: (1.0 * congestion_multiplier) as u64,
            medium_priority: (2.0 * congestion_multiplier) as u64,
            high_priority: (5.0 * congestion_multiplier) as u64,
        }
    }

    pub fn analyze_transaction(&self, _tx: &Transaction) -> f64 {
        0.5
    }
}
