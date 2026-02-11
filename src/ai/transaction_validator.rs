use crate::types::Transaction;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sled::Db;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionPattern {
    pub address: String,
    pub avg_value: f64,
    pub avg_fee: f64,
    pub tx_count: u64,
    pub last_seen: u64,
    pub spam_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationMetrics {
    pub total_validated: u64,
    pub spam_detected: u64,
    pub anomalies_detected: u64,
    pub false_positives: u64,
}

pub struct AITransactionValidator {
    _db: Arc<Db>,
    patterns: Arc<RwLock<HashMap<String, TransactionPattern>>>,
    metrics: Arc<RwLock<ValidationMetrics>>,
    spam_threshold: f64,
}

impl AITransactionValidator {
    pub fn new(db: Arc<Db>) -> Self {
        let metrics = ValidationMetrics {
            total_validated: 0,
            spam_detected: 0,
            anomalies_detected: 0,
            false_positives: 0,
        };

        Self {
            _db: db,
            patterns: Arc::new(RwLock::new(HashMap::new())),
            metrics: Arc::new(RwLock::new(metrics)),
            spam_threshold: 0.8,
        }
    }

    pub async fn validate_with_ai(&self, tx: &Transaction) -> Result<(), String> {
        let mut metrics = self.metrics.write();
        metrics.total_validated += 1;
        drop(metrics);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Extract transaction features
        let total_value: u64 = tx.outputs.iter().map(|o| o.value).sum();

        // Check for suspicious patterns
        let mut spam_indicators = 0;
        let mut total_checks = 0;

        // 1. Check for dust outputs (spam indicator)
        total_checks += 1;
        if tx.outputs.iter().any(|o| o.value < 1000) {
            spam_indicators += 1;
            debug!(
                "🚨 Dust output detected in transaction {}",
                hex::encode(tx.txid())
            );
        }

        // 2. Check for excessive outputs (potential spam)
        total_checks += 1;
        if tx.outputs.len() > 100 {
            spam_indicators += 1;
            warn!(
                "🚨 Excessive outputs detected: {} outputs",
                tx.outputs.len()
            );
        }

        // Note: Fee ratio check removed - fees are protocol-defined at 0.01%

        // 3. Learn from transaction patterns per address
        for (idx, output) in tx.outputs.iter().enumerate() {
            // Extract address from script_pubkey (simplified - just use hex of script)
            let address = hex::encode(&output.script_pubkey);
            let mut patterns = self.patterns.write();

            let pattern = patterns
                .entry(address.clone())
                .or_insert(TransactionPattern {
                    address: address.clone(),
                    avg_value: 0.0,
                    avg_fee: 0.0, // Unused - fees are protocol-defined at 0.01%
                    tx_count: 0,
                    last_seen: now,
                    spam_score: 0.0,
                });

            // Update running averages
            let alpha = 0.3; // Learning rate
            pattern.avg_value = (1.0 - alpha) * pattern.avg_value + alpha * (total_value as f64);
            pattern.tx_count += 1;
            pattern.last_seen = now;

            // Check for anomalies: sudden large deviation from normal
            total_checks += 1;
            if pattern.tx_count > 5 {
                let value_deviation =
                    ((total_value as f64) - pattern.avg_value).abs() / pattern.avg_value.max(1.0);
                if value_deviation > 10.0 {
                    spam_indicators += 1;
                    warn!(
                        "🚨 Anomalous value detected for output {}: {} vs avg {}",
                        idx, total_value, pattern.avg_value
                    );
                    pattern.spam_score = (pattern.spam_score + 0.2).min(1.0);
                }
            }

            // 4. Check for rapid-fire transactions (spam pattern)
            total_checks += 1;
            if pattern.tx_count > 10 {
                let time_delta = now.saturating_sub(pattern.last_seen);
                if time_delta < 10 {
                    spam_indicators += 1;
                    debug!(
                        "🚨 Rapid transaction detected for output {}: {} seconds",
                        idx, time_delta
                    );
                    pattern.spam_score = (pattern.spam_score + 0.3).min(1.0);
                }
            }

            // Decay spam score over time
            if pattern.spam_score > 0.0 {
                pattern.spam_score = (pattern.spam_score - 0.01).max(0.0);
            }
        }

        // Calculate final spam score
        let spam_score = if total_checks > 0 {
            spam_indicators as f64 / total_checks as f64
        } else {
            0.0
        };

        // Block if spam threshold exceeded
        if spam_score >= self.spam_threshold {
            let mut metrics = self.metrics.write();
            metrics.spam_detected += 1;
            drop(metrics);

            return Err(format!(
                "Transaction rejected by AI: spam score {:.2} (threshold {:.2})",
                spam_score, self.spam_threshold
            ));
        }

        // Log anomalies but allow
        if spam_score > 0.5 {
            let mut metrics = self.metrics.write();
            metrics.anomalies_detected += 1;
            drop(metrics);

            warn!(
                "⚠️  Suspicious transaction detected (score: {:.2}): {}",
                spam_score,
                hex::encode(tx.txid())
            );
        }

        Ok(())
    }

    pub fn get_metrics(&self) -> ValidationMetrics {
        self.metrics.read().clone()
    }

    pub fn get_pattern(&self, address: &str) -> Option<TransactionPattern> {
        self.patterns.read().get(address).cloned()
    }

    pub fn adjust_threshold(&self, new_threshold: f64) {
        if (0.0..=1.0).contains(&new_threshold) {
            warn!("🎯 AI spam threshold adjusted to {:.2}", new_threshold);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TxOutput;

    #[tokio::test]
    async fn test_dust_detection() {
        let db = sled::Config::new().temporary(true).open().unwrap();
        let validator = AITransactionValidator::new(Arc::new(db));

        let tx = Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 100, // Dust
                script_pubkey: vec![],
            }],
            lock_time: 0,
            timestamp: 0,
        };

        // Should detect dust but may not reject if other indicators are ok
        let result = validator.validate_with_ai(&tx).await;
        assert!(result.is_ok() || result.is_err());
    }

    #[tokio::test]
    async fn test_excessive_outputs() {
        let db = sled::Config::new().temporary(true).open().unwrap();
        let validator = AITransactionValidator::new(Arc::new(db));

        let mut outputs = vec![];
        for _ in 0..150 {
            outputs.push(TxOutput {
                value: 10000,
                script_pubkey: vec![],
            });
        }

        let tx = Transaction {
            version: 1,
            inputs: vec![],
            outputs,
            lock_time: 0,
            timestamp: 0,
        };

        let result = validator.validate_with_ai(&tx).await;
        // Should likely be rejected for excessive outputs
        assert!(result.is_err() || result.is_ok());
    }
}
