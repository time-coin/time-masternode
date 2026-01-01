use crate::error::AppError;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockTimingData {
    pub height: u64,
    pub timestamp: u64,
    pub block_time: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPrediction {
    pub predicted_next_block: u64,
    pub confidence: f64,
    pub recommended_prefetch: bool,
}

pub struct PredictiveSync {
    _db: Arc<Db>,
    block_history: Arc<RwLock<VecDeque<BlockTimingData>>>,
    min_samples: usize,
}

impl PredictiveSync {
    pub fn new(db: Arc<Db>, min_samples: usize) -> Result<Self, AppError> {
        let predictor = Self {
            _db: db,
            block_history: Arc::new(RwLock::new(VecDeque::with_capacity(500))),
            min_samples,
        };

        Ok(predictor)
    }

    pub fn record_block(&self, height: u64, timestamp: u64, block_time: u64) {
        let data = BlockTimingData {
            height,
            timestamp,
            block_time,
        };

        let mut history_lock = self.block_history.write().unwrap();
        history_lock.push_back(data);

        if history_lock.len() > 500 {
            history_lock.pop_front();
        }
    }

    pub fn predict_next_block(&self, current_height: u64) -> Option<SyncPrediction> {
        let history_lock = self.block_history.read().unwrap();

        if history_lock.len() < self.min_samples {
            return None;
        }

        let recent: Vec<&BlockTimingData> = history_lock.iter().rev().take(20).collect();

        let avg_block_time: f64 =
            recent.iter().map(|b| b.block_time as f64).sum::<f64>() / recent.len() as f64;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let latest_block = history_lock.back()?;
        let time_since_last = now.saturating_sub(latest_block.timestamp);

        let blocks_behind = if time_since_last > avg_block_time as u64 {
            (time_since_last as f64 / avg_block_time).floor() as u64
        } else {
            0
        };

        let predicted_height = current_height + blocks_behind;

        let variance = recent
            .iter()
            .map(|b| (b.block_time as f64 - avg_block_time).powi(2))
            .sum::<f64>()
            / recent.len() as f64;
        let std_dev = variance.sqrt();

        let confidence = if std_dev == 0.0 {
            1.0
        } else {
            (1.0 / (1.0 + std_dev / avg_block_time)).clamp(0.0, 1.0)
        };

        let recommended_prefetch = blocks_behind >= 2 && confidence > 0.7;

        Some(SyncPrediction {
            predicted_next_block: predicted_height,
            confidence,
            recommended_prefetch,
        })
    }

    pub fn should_prefetch(&self, current_height: u64) -> bool {
        self.predict_next_block(current_height)
            .map(|p| p.recommended_prefetch)
            .unwrap_or(false)
    }

    pub fn get_average_block_time(&self) -> Option<f64> {
        let history_lock = self.block_history.read().unwrap();

        if history_lock.len() < self.min_samples {
            return None;
        }

        let recent: Vec<&BlockTimingData> = history_lock.iter().rev().take(100).collect();

        let avg = recent.iter().map(|b| b.block_time as f64).sum::<f64>() / recent.len() as f64;

        Some(avg)
    }

    pub fn get_sync_health(&self) -> f64 {
        let history_lock = self.block_history.read().unwrap();

        if history_lock.len() < 2 {
            return 0.5;
        }

        let recent: Vec<&BlockTimingData> = history_lock.iter().rev().take(20).collect();

        let heights_continuous = recent.windows(2).all(|w| w[0].height == w[1].height + 1);

        if heights_continuous {
            1.0
        } else {
            let gaps = recent
                .windows(2)
                .filter(|w| w[0].height != w[1].height + 1)
                .count();
            (1.0 - (gaps as f64 / recent.len() as f64)).max(0.0)
        }
    }
}
