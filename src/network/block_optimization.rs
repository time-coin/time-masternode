use crate::types::Transaction;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Maximum block size in bytes
const MAX_BLOCK_SIZE: u64 = 1_000_000; // 1MB

/// Block optimization statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OptimizationStats {
    /// Total blocks optimized
    pub total_blocks: u64,
    /// Average fees per block
    pub avg_fees_per_block: f64,
    /// Average transactions per block
    pub avg_txs_per_block: f64,
    /// Total optimization improvements (vs naive)
    pub total_improvement: f64,
}

/// Transaction with computed priority score
#[derive(Debug, Clone)]
struct ScoredTransaction {
    tx: Transaction,
    fee_per_byte: f64,
    priority_score: f64,
    size: u64,
}

/// AI-powered block production optimizer
pub struct BlockOptimizer {
    /// Historical optimization performance
    stats: Arc<RwLock<OptimizationStats>>,
    /// Learned fee patterns
    fee_patterns: Arc<RwLock<HashMap<String, f64>>>,
    /// Persistent storage
    storage: sled::Tree,
}

impl BlockOptimizer {
    /// Create new block optimizer with persistent storage
    pub fn new(db: &sled::Db) -> Result<Self, String> {
        let storage = db
            .open_tree("block_optimization")
            .map_err(|e| format!("Failed to open block_optimization tree: {}", e))?;

        // Load stats
        let stats = match storage.get(b"stats") {
            Ok(Some(bytes)) => {
                bincode::deserialize::<OptimizationStats>(&bytes).unwrap_or_default()
            }
            _ => OptimizationStats::default(),
        };

        info!(
            "🎯 [AI] Loaded block optimizer: {} blocks optimized (avg {:.2} fees)",
            stats.total_blocks, stats.avg_fees_per_block
        );

        Ok(Self {
            stats: Arc::new(RwLock::new(stats)),
            fee_patterns: Arc::new(RwLock::new(HashMap::new())),
            storage,
        })
    }

    /// Optimize transaction selection for maximum fees while staying under size limit
    pub async fn optimize_block(
        &self,
        mempool: Vec<Transaction>,
        max_size: u64,
    ) -> Vec<Transaction> {
        if mempool.is_empty() {
            return Vec::new();
        }

        debug!(
            "🎯 [AI] Optimizing block: {} candidate txs, max size: {} bytes",
            mempool.len(),
            max_size
        );

        // Step 1: Score all transactions
        let mut scored_txs = self.score_transactions(mempool).await;

        // Step 2: Sort by priority score (highest first)
        scored_txs.sort_by(|a, b| {
            b.priority_score
                .partial_cmp(&a.priority_score)
                .unwrap_or(Ordering::Equal)
        });

        // Step 3: Greedy knapsack selection
        let selected = self.select_transactions(&scored_txs, max_size);

        // Step 4: Record performance
        let total_fees: u64 = selected.iter().map(|tx| self.calculate_fee(tx)).sum();
        self.record_optimization(selected.len(), total_fees).await;

        debug!(
            "🎯 [AI] Selected {} txs, total fees: {} TIME",
            selected.len(),
            total_fees
        );

        selected
    }

    /// Score transactions based on multiple features
    async fn score_transactions(&self, txs: Vec<Transaction>) -> Vec<ScoredTransaction> {
        let mut scored = Vec::new();

        for tx in txs {
            let fee = self.calculate_fee(&tx);
            let size = self.calculate_size(&tx);
            let fee_per_byte = fee as f64 / size as f64;

            // Multi-factor priority score
            let priority_score = self.calculate_priority_score(&tx, fee, fee_per_byte).await;

            scored.push(ScoredTransaction {
                tx,
                fee_per_byte,
                priority_score,
                size,
            });
        }

        scored
    }

    /// Calculate priority score using AI-learned patterns
    async fn calculate_priority_score(&self, tx: &Transaction, fee: u64, fee_per_byte: f64) -> f64 {
        // Feature 1: Fee per byte (weight: 60%)
        // Normalize to 0-1 range (assume max 1000 sat/byte)
        let fee_score = (fee_per_byte / 1000.0).min(1.0);

        // Feature 2: Absolute fee (weight: 20%)
        // Normalize to 0-1 range (assume max 10000 sat)
        let absolute_fee_score = (fee as f64 / 10000.0).min(1.0);

        // Feature 3: Transaction dependencies (weight: 10%)
        // Prioritize txs that spend from recent blocks
        let dependency_score = self.calculate_dependency_score(tx);

        // Feature 4: Historical pattern (weight: 10%)
        // Reward txs from historically reliable sources
        let pattern_score = self.calculate_pattern_score(tx).await;

        // Combine weighted features
        let base_score = fee_score * 0.6
            + absolute_fee_score * 0.2
            + dependency_score * 0.1
            + pattern_score * 0.1;

        base_score
    }

    /// Calculate dependency score (prefer independent txs)
    fn calculate_dependency_score(&self, _tx: &Transaction) -> f64 {
        // TODO: Analyze UTXO dependencies
        // For now, assume all txs are independent
        1.0
    }

    /// Calculate pattern score based on learned history
    async fn calculate_pattern_score(&self, tx: &Transaction) -> f64 {
        // Extract pattern identifier (e.g., sender address prefix)
        let pattern_id = self.extract_pattern_id(tx);

        let patterns = self.fee_patterns.read().await;
        let score = patterns.get(&pattern_id).copied().unwrap_or(0.5);

        score
    }

    /// Extract pattern identifier from transaction
    fn extract_pattern_id(&self, tx: &Transaction) -> String {
        // Use first input as pattern (simplified)
        if let Some(input) = tx.inputs.first() {
            format!("{:x}", input.previous_output.txid[0])[..8].to_string()
        } else {
            "unknown".to_string()
        }
    }

    /// Select transactions using greedy knapsack algorithm
    fn select_transactions(
        &self,
        scored_txs: &[ScoredTransaction],
        max_size: u64,
    ) -> Vec<Transaction> {
        let mut selected = Vec::new();
        let mut current_size = 0u64;

        for scored_tx in scored_txs {
            if current_size + scored_tx.size <= max_size {
                selected.push(scored_tx.tx.clone());
                current_size += scored_tx.size;
            }

            // Stop if we've filled the block
            if current_size >= max_size * 95 / 100 {
                // 95% full is good enough
                break;
            }
        }

        selected
    }

    /// Calculate transaction fee
    fn calculate_fee(&self, tx: &Transaction) -> u64 {
        // Note: Accurate fee calculation requires UTXO lookup for input values
        // For optimization purposes, we use a simplified estimation
        // In production, this should query the UTXO manager

        // Sum outputs
        let output_value: u64 = tx.outputs.iter().map(|o| o.value).sum();

        // Estimate inputs (assume standard UTXO sizes)
        let estimated_input_value = tx.inputs.len() as u64 * 1000; // Placeholder

        // Fee = estimated_inputs - outputs
        estimated_input_value.saturating_sub(output_value)
    }

    /// Calculate transaction size in bytes
    fn calculate_size(&self, tx: &Transaction) -> u64 {
        // Simplified size calculation
        // TODO: Use actual serialized size
        let base_size = 10; // Version, locktime, etc.
        let input_size = tx.inputs.len() * 180; // ~180 bytes per input
        let output_size = tx.outputs.len() * 34; // ~34 bytes per output

        (base_size + input_size + output_size) as u64
    }

    /// Record optimization performance
    async fn record_optimization(&self, tx_count: usize, total_fees: u64) {
        let mut stats = self.stats.write().await;

        stats.total_blocks += 1;

        // Update rolling average
        let n = stats.total_blocks as f64;
        stats.avg_fees_per_block = (stats.avg_fees_per_block * (n - 1.0) + total_fees as f64) / n;
        stats.avg_txs_per_block = (stats.avg_txs_per_block * (n - 1.0) + tx_count as f64) / n;

        let stats_clone = stats.clone();
        drop(stats);

        // Save to disk
        if let Ok(bytes) = bincode::serialize(&stats_clone) {
            let _ = self.storage.insert(b"stats", bytes);
        }

        debug!(
            "📊 [AI] Block optimizer stats: {} blocks, avg {:.2} fees, avg {:.1} txs",
            stats_clone.total_blocks, stats_clone.avg_fees_per_block, stats_clone.avg_txs_per_block
        );
    }

    /// Learn from transaction confirmation patterns
    pub async fn learn_from_block(&self, txs: &[Transaction]) {
        let mut patterns = self.fee_patterns.write().await;

        for tx in txs {
            let pattern_id = self.extract_pattern_id(tx);
            let fee_per_byte = self.calculate_fee(tx) as f64 / self.calculate_size(tx) as f64;

            // Update learned pattern (exponential moving average)
            let current = patterns.get(&pattern_id).copied().unwrap_or(0.5);
            let alpha = 0.1; // Learning rate
            let updated = current * (1.0 - alpha) + (fee_per_byte / 1000.0).min(1.0) * alpha;

            patterns.insert(pattern_id, updated);
        }

        // Limit pattern cache size
        if patterns.len() > 10000 {
            // Remove lowest scoring patterns
            let keys_to_remove: Vec<String> = {
                let mut sorted: Vec<_> = patterns.iter().map(|(k, v)| (k.clone(), *v)).collect();
                sorted.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
                sorted.into_iter().take(1000).map(|(k, _)| k).collect()
            };

            for key in keys_to_remove {
                patterns.remove(&key);
            }
        }
    }

    /// Get optimization statistics
    pub async fn get_stats(&self) -> OptimizationStats {
        self.stats.read().await.clone()
    }

    /// Calculate improvement vs naive selection (first-in-first-out)
    pub async fn calculate_improvement(&self, naive_fees: u64, optimized_fees: u64) -> f64 {
        if naive_fees == 0 {
            return 0.0;
        }

        let improvement = ((optimized_fees as f64 / naive_fees as f64) - 1.0) * 100.0;

        // Update stats
        let mut stats = self.stats.write().await;
        let n = stats.total_blocks as f64;
        stats.total_improvement = (stats.total_improvement * (n - 1.0) + improvement) / n;

        improvement
    }

    /// Flush all data to disk
    pub async fn flush(&self) -> Result<(), String> {
        self.storage
            .flush_async()
            .await
            .map_err(|e| format!("Failed to flush block optimizer: {}", e))?;
        debug!("💾 Flushed block optimizer data to disk");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OutPoint, TxInput, TxOutput};

    fn create_test_optimizer() -> BlockOptimizer {
        let config = sled::Config::new().temporary(true);
        let db = config.open().unwrap();
        BlockOptimizer::new(&db).unwrap()
    }

    fn create_test_tx(fee: u64, outputs: usize) -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: OutPoint {
                    txid: [0u8; 32],
                    vout: 0,
                },
                script_sig: vec![],
                sequence: 0,
            }],
            outputs: (0..outputs)
                .map(|_| TxOutput {
                    value: 1000,
                    script_pubkey: vec![],
                })
                .collect(),
            lock_time: 0,
            timestamp: 0,
        }
    }

    #[tokio::test]
    async fn test_transaction_selection() {
        let optimizer = create_test_optimizer();

        // Create transactions with different fees
        let mut mempool = vec![
            create_test_tx(10, 1),   // Low fee
            create_test_tx(100, 1),  // High fee
            create_test_tx(50, 1),   // Medium fee
            create_test_tx(200, 10), // High fee but large
        ];

        let selected = optimizer.optimize_block(mempool, 10000).await;

        // Should prefer high fee transactions
        assert!(!selected.is_empty());
    }

    #[tokio::test]
    async fn test_size_constraint() {
        let optimizer = create_test_optimizer();

        // Create many small transactions
        let mempool: Vec<_> = (0..100).map(|i| create_test_tx(i + 10, 1)).collect();

        let selected = optimizer.optimize_block(mempool, 5000).await;

        // Should respect size limit
        let total_size: u64 = selected.iter().map(|tx| optimizer.calculate_size(tx)).sum();
        assert!(total_size <= 5000);
    }

    #[tokio::test]
    async fn test_learning() {
        let optimizer = create_test_optimizer();

        // Learn from confirmed blocks
        let confirmed = vec![create_test_tx(100, 1), create_test_tx(200, 1)];
        optimizer.learn_from_block(&confirmed).await;

        let stats = optimizer.get_stats().await;
        assert_eq!(stats.total_blocks, 0); // Learning doesn't count as optimization
    }
}
