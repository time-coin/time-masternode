use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Maximum number of fee records to keep in history
const MAX_FEE_HISTORY: usize = 1000;

/// Fee estimate confidence levels
const CONFIDENCE_LOW: f64 = 0.70;
const CONFIDENCE_MEDIUM: f64 = 0.85;
const CONFIDENCE_HIGH: f64 = 0.95;

/// Fee record for a transaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeRecord {
    /// Fee paid (in smallest unit)
    pub fee: u64,
    /// Fee per byte
    pub fee_per_byte: f64,
    /// Block height when confirmed
    pub confirmed_at_height: u64,
    /// Blocks waited for confirmation
    pub blocks_waited: u64,
    /// Timestamp when added to mempool
    pub mempool_timestamp: u64,
    /// Timestamp when confirmed
    pub confirmed_timestamp: u64,
    /// Transaction size in bytes
    pub tx_size: u64,
}

/// Fee estimate with different confidence levels
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeEstimate {
    /// Low priority - 70% confidence in 10 blocks
    pub low: u64,
    /// Medium priority - 85% confidence in 3 blocks
    pub medium: u64,
    /// High priority - 95% confidence in 1 block
    pub high: u64,
    /// AI-recommended optimal fee
    pub optimal: u64,
    /// Current mempool congestion (0.0-1.0)
    pub congestion: f64,
}

/// Mempool congestion metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CongestionMetrics {
    /// Number of transactions in mempool
    tx_count: usize,
    /// Total bytes in mempool
    total_bytes: u64,
    /// Average fee per byte
    avg_fee_per_byte: f64,
    /// Timestamp
    timestamp: u64,
}

/// AI-powered transaction fee prediction system
pub struct FeePredictor {
    /// Historical fee records
    fee_history: Arc<RwLock<VecDeque<FeeRecord>>>,
    /// Congestion history
    congestion_history: Arc<RwLock<VecDeque<CongestionMetrics>>>,
    /// Persistent storage
    storage: sled::Tree,
}

impl FeePredictor {
    /// Create new fee predictor with persistent storage
    pub fn new(db: &sled::Db) -> Result<Self, String> {
        let storage = db
            .open_tree("fee_predictions")
            .map_err(|e| format!("Failed to open fee_predictions tree: {}", e))?;

        // Load historical data
        let mut fee_history = VecDeque::new();
        for result in storage.scan_prefix(b"fee_") {
            match result {
                Ok((_, value)) => {
                    if let Ok(record) = bincode::deserialize::<FeeRecord>(&value) {
                        fee_history.push_back(record);
                    }
                }
                Err(e) => warn!("Failed to load fee record: {}", e),
            }
        }

        // Keep only recent records
        while fee_history.len() > MAX_FEE_HISTORY {
            fee_history.pop_front();
        }

        info!("💰 [AI] Loaded {} fee records from disk", fee_history.len());

        Ok(Self {
            fee_history: Arc::new(RwLock::new(fee_history)),
            congestion_history: Arc::new(RwLock::new(VecDeque::new())),
            storage,
        })
    }

    /// Record a confirmed transaction's fee data
    pub async fn record_confirmed_tx(
        &self,
        fee: u64,
        tx_size: u64,
        blocks_waited: u64,
        mempool_timestamp: u64,
        confirmed_height: u64,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let record = FeeRecord {
            fee,
            fee_per_byte: fee as f64 / tx_size as f64,
            confirmed_at_height: confirmed_height,
            blocks_waited,
            mempool_timestamp,
            confirmed_timestamp: now,
            tx_size,
        };

        // Add to in-memory history
        let mut history = self.fee_history.write().await;
        history.push_back(record.clone());

        // Limit history size
        while history.len() > MAX_FEE_HISTORY {
            history.pop_front();
        }

        drop(history);

        // Save to disk (async)
        let key = format!("fee_{}", now);
        if let Ok(bytes) = bincode::serialize(&record) {
            if let Err(e) = self.storage.insert(key.as_bytes(), bytes) {
                warn!("Failed to save fee record: {}", e);
            } else {
                debug!("💾 Saved fee record: {} sat/byte", record.fee_per_byte);
            }
        }
    }

    /// Record current mempool congestion
    pub async fn record_congestion(&self, tx_count: usize, total_bytes: u64, avg_fee: f64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let metrics = CongestionMetrics {
            tx_count,
            total_bytes,
            avg_fee_per_byte: avg_fee,
            timestamp: now,
        };

        let mut history = self.congestion_history.write().await;
        history.push_back(metrics);

        // Keep last 100 samples (about 16 hours at 10min blocks)
        while history.len() > 100 {
            history.pop_front();
        }
    }

    /// Predict optimal fee for target confirmation time
    pub async fn predict_fee(&self, target_blocks: u64) -> FeeEstimate {
        let history = self.fee_history.read().await;

        if history.is_empty() {
            // No data yet - use conservative defaults
            return FeeEstimate {
                low: 10,
                medium: 20,
                high: 50,
                optimal: 25,
                congestion: 0.5,
            };
        }

        // Calculate percentiles for different confirmation times
        let low_fee = self.calculate_percentile(&history, 10, CONFIDENCE_LOW);
        let medium_fee = self.calculate_percentile(&history, 3, CONFIDENCE_MEDIUM);
        let high_fee = self.calculate_percentile(&history, 1, CONFIDENCE_HIGH);

        // Calculate current congestion
        let congestion = self.calculate_congestion().await;

        // AI-recommended optimal fee based on target and congestion
        let optimal = self.calculate_optimal(&history, target_blocks, congestion);

        debug!(
            "💰 [AI] Fee prediction: low={}, medium={}, high={}, optimal={} (congestion: {:.2})",
            low_fee, medium_fee, high_fee, optimal, congestion
        );

        FeeEstimate {
            low: low_fee,
            medium: medium_fee,
            high: high_fee,
            optimal,
            congestion,
        }
    }

    /// Calculate fee at percentile for target confirmation time
    fn calculate_percentile(
        &self,
        history: &VecDeque<FeeRecord>,
        target_blocks: u64,
        confidence: f64,
    ) -> u64 {
        // Filter to transactions confirmed within target blocks
        let mut relevant_fees: Vec<u64> = history
            .iter()
            .filter(|r| r.blocks_waited <= target_blocks)
            .map(|r| r.fee)
            .collect();

        if relevant_fees.is_empty() {
            // Fall back to all fees
            relevant_fees = history.iter().map(|r| r.fee).collect();
        }

        if relevant_fees.is_empty() {
            return 10; // Default minimum
        }

        relevant_fees.sort_unstable();

        // Get percentile value
        let index =
            ((relevant_fees.len() as f64 * confidence) as usize).min(relevant_fees.len() - 1);
        relevant_fees[index]
    }

    /// Calculate current mempool congestion level (0.0 = empty, 1.0 = full)
    async fn calculate_congestion(&self) -> f64 {
        let history = self.congestion_history.read().await;

        if history.is_empty() {
            return 0.5; // Unknown, assume moderate
        }

        // Use most recent congestion data
        let recent = history.back().unwrap();

        // Normalize based on typical values
        // Assume 1000 txs or 1MB is "full"
        let tx_congestion = (recent.tx_count as f64 / 1000.0).min(1.0);
        let byte_congestion = (recent.total_bytes as f64 / 1_000_000.0).min(1.0);

        // Average the two metrics
        (tx_congestion + byte_congestion) / 2.0
    }

    /// Calculate AI-recommended optimal fee
    fn calculate_optimal(
        &self,
        history: &VecDeque<FeeRecord>,
        target_blocks: u64,
        congestion: f64,
    ) -> u64 {
        // Base fee from historical data
        let base_fee = self.calculate_percentile(history, target_blocks, 0.90);

        // Adjust for current congestion
        let congestion_multiplier = 1.0 + (congestion * 0.5); // Up to 1.5x during high congestion

        // Time urgency multiplier
        let urgency_multiplier = if target_blocks == 1 {
            1.2 // 20% premium for immediate confirmation
        } else if target_blocks <= 3 {
            1.1 // 10% premium for fast confirmation
        } else {
            1.0 // No premium for low priority
        };

        let optimal = (base_fee as f64 * congestion_multiplier * urgency_multiplier) as u64;

        // Ensure minimum fee
        optimal.max(10)
    }

    /// Get statistics about fee prediction accuracy
    pub async fn get_stats(&self) -> FeeStats {
        let history = self.fee_history.read().await;

        if history.is_empty() {
            return FeeStats::default();
        }

        let total_txs = history.len();
        let avg_fee: f64 = history.iter().map(|r| r.fee_per_byte).sum::<f64>() / total_txs as f64;
        let avg_confirmation =
            history.iter().map(|r| r.blocks_waited).sum::<u64>() / total_txs as u64;

        // Calculate how many txs confirmed in 1, 3, 10 blocks
        let fast = history.iter().filter(|r| r.blocks_waited <= 1).count();
        let medium = history.iter().filter(|r| r.blocks_waited <= 3).count();
        let slow = history.iter().filter(|r| r.blocks_waited <= 10).count();

        FeeStats {
            total_records: total_txs,
            avg_fee_per_byte: avg_fee,
            avg_blocks_to_confirm: avg_confirmation,
            fast_confirmations_pct: (fast as f64 / total_txs as f64) * 100.0,
            medium_confirmations_pct: (medium as f64 / total_txs as f64) * 100.0,
            slow_confirmations_pct: (slow as f64 / total_txs as f64) * 100.0,
        }
    }

    /// Flush all data to disk
    pub async fn flush(&self) -> Result<(), String> {
        self.storage
            .flush_async()
            .await
            .map_err(|e| format!("Failed to flush fee predictions: {}", e))?;
        debug!("💾 Flushed fee prediction data to disk");
        Ok(())
    }
}

/// Fee prediction statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeeStats {
    pub total_records: usize,
    pub avg_fee_per_byte: f64,
    pub avg_blocks_to_confirm: u64,
    pub fast_confirmations_pct: f64,
    pub medium_confirmations_pct: f64,
    pub slow_confirmations_pct: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_predictor() -> FeePredictor {
        let config = sled::Config::new().temporary(true);
        let db = config.open().unwrap();
        FeePredictor::new(&db).unwrap()
    }

    #[tokio::test]
    async fn test_fee_prediction_cold_start() {
        let predictor = create_test_predictor();

        // Should return defaults with no data
        let estimate = predictor.predict_fee(1).await;
        assert!(estimate.low > 0);
        assert!(estimate.medium >= estimate.low);
        assert!(estimate.high >= estimate.medium);
    }

    #[tokio::test]
    async fn test_fee_prediction_with_data() {
        let predictor = create_test_predictor();

        // Add some historical data
        for i in 0..100 {
            predictor
                .record_confirmed_tx(
                    100 + i, // Fee increases over time
                    250,     // Standard tx size
                    1,       // Fast confirmation
                    1000000,
                    100 + i,
                )
                .await;
        }

        let estimate = predictor.predict_fee(1).await;

        // Should have learned from data
        assert!(estimate.optimal >= 100);
        assert!(estimate.optimal <= 300);
    }

    #[tokio::test]
    async fn test_congestion_adjustment() {
        let predictor = create_test_predictor();

        // Low congestion
        predictor.record_congestion(100, 50000, 10.0).await;
        let low_congestion = predictor.calculate_congestion().await;

        // High congestion
        predictor.record_congestion(2000, 2000000, 50.0).await;
        let high_congestion = predictor.calculate_congestion().await;

        assert!(high_congestion > low_congestion);
    }
}
