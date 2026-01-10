//! Transaction selection for block production using tiered priority
//!
//! This module provides utilities for block producers to select transactions
//! from the mempool using the tiered masternode priority system.

#![allow(dead_code)]

use crate::masternode_registry::MasternodeRegistry;
use crate::network::connection_manager::ConnectionManager;
use crate::transaction_pool::TransactionPool;
use crate::transaction_priority::{TierDistribution, TransactionPriorityQueue};
use crate::types::Transaction;
use std::sync::Arc;

/// Transaction selector for block production
pub struct TransactionSelector {
    tx_pool: Arc<TransactionPool>,
    priority_queue: TransactionPriorityQueue,
}

impl TransactionSelector {
    pub fn new(
        tx_pool: Arc<TransactionPool>,
        masternode_registry: Arc<MasternodeRegistry>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        let priority_queue = TransactionPriorityQueue::new(masternode_registry, connection_manager);

        Self {
            tx_pool,
            priority_queue,
        }
    }

    /// Select transactions for a block, prioritizing higher-tier masternodes
    /// Returns transactions ordered by priority
    pub async fn select_for_block(
        &self,
        max_count: usize,
        max_size_bytes: usize,
    ) -> Vec<Transaction> {
        // Get all pending transactions with metadata
        let pending_with_metadata = self.tx_pool.get_all_pending_with_metadata();

        tracing::info!(
            "📊 Selecting from {} pending transactions for block",
            pending_with_metadata.len()
        );

        // Get tier distribution for monitoring
        let dist = self
            .priority_queue
            .get_tier_distribution(&pending_with_metadata)
            .await;

        self.log_tier_distribution(&dist);

        // Select using priority queue
        let selected = self
            .priority_queue
            .select_for_block(pending_with_metadata, max_count, max_size_bytes)
            .await;

        tracing::info!(
            "✅ Selected {} transactions for block ({}% of pending)",
            selected.len(),
            if self.tx_pool.pending_count() > 0 {
                (selected.len() * 100) / self.tx_pool.pending_count()
            } else {
                0
            }
        );

        selected
    }

    /// Select finalized transactions for block (already consensus-approved)
    /// This still applies priority ordering to ensure highest-priority txs are included first
    pub async fn select_finalized_for_block(
        &self,
        max_count: usize,
        max_size_bytes: usize,
    ) -> Vec<Transaction> {
        // Get finalized transactions with fees
        let finalized = self.tx_pool.get_finalized_transactions_with_fees();

        tracing::info!(
            "📊 Selecting from {} finalized transactions for block",
            finalized.len()
        );

        // Convert to the format expected by priority queue
        // Note: We don't have submitter_ip for finalized txs, so priority is fee-only
        let finalized_with_metadata: Vec<(Transaction, u64, Option<String>, std::time::Instant)> =
            finalized
                .into_iter()
                .map(|(tx, fee)| (tx, fee, None, std::time::Instant::now()))
                .collect();

        // Select using priority queue (will be mostly fee-based since no submitter info)
        let selected = self
            .priority_queue
            .select_for_block(finalized_with_metadata, max_count, max_size_bytes)
            .await;

        tracing::info!(
            "✅ Selected {} finalized transactions for block",
            selected.len()
        );

        selected
    }

    /// Log tier distribution for monitoring
    fn log_tier_distribution(&self, dist: &TierDistribution) {
        if dist.total() == 0 {
            return;
        }

        tracing::info!(
            "🎯 Transaction pool tier distribution: Gold: {}, Silver: {}, Bronze: {}, Whitelisted: {}, Regular: {}",
            dist.gold,
            dist.silver,
            dist.bronze,
            dist.whitelisted_free,
            dist.regular
        );

        // Calculate percentages for high-tier nodes
        let high_tier_count = dist.gold + dist.silver + dist.bronze;
        if high_tier_count > 0 {
            let high_tier_pct = (high_tier_count * 100) / dist.total();
            tracing::info!("⭐ High-tier node transactions: {}% of pool", high_tier_pct);
        }
    }

    /// Get statistics about current pending transactions
    pub async fn get_selection_stats(&self) -> SelectionStats {
        let pending_with_metadata = self.tx_pool.get_all_pending_with_metadata();
        let dist = self
            .priority_queue
            .get_tier_distribution(&pending_with_metadata)
            .await;

        SelectionStats {
            total_pending: self.tx_pool.pending_count(),
            total_finalized: self.tx_pool.finalized_count(),
            tier_distribution: dist,
        }
    }
}

/// Statistics about transaction selection
#[derive(Debug, Clone)]
pub struct SelectionStats {
    pub total_pending: usize,
    pub total_finalized: usize,
    pub tier_distribution: TierDistribution,
}

impl SelectionStats {
    /// Calculate the percentage of high-tier transactions
    pub fn high_tier_percentage(&self) -> f64 {
        let high_tier = self.tier_distribution.gold
            + self.tier_distribution.silver
            + self.tier_distribution.bronze;
        let total = self.tier_distribution.total();

        if total == 0 {
            0.0
        } else {
            (high_tier as f64 / total as f64) * 100.0
        }
    }

    /// Check if pool is dominated by high-tier nodes
    pub fn is_high_tier_dominated(&self) -> bool {
        self.high_tier_percentage() > 50.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_selection_stats_calculation() {
        let dist = TierDistribution {
            gold: 10,
            silver: 20,
            bronze: 30,
            whitelisted_free: 20,
            regular: 20,
        };

        let stats = SelectionStats {
            total_pending: 100,
            total_finalized: 10,
            tier_distribution: dist,
        };

        // High tier = 10 + 20 + 30 = 60 out of 100 total = 60%
        assert_eq!(stats.high_tier_percentage(), 60.0);
        assert!(stats.is_high_tier_dominated());
    }

    #[test]
    fn test_selection_stats_no_high_tier() {
        let dist = TierDistribution {
            gold: 0,
            silver: 0,
            bronze: 0,
            whitelisted_free: 50,
            regular: 50,
        };

        let stats = SelectionStats {
            total_pending: 100,
            total_finalized: 10,
            tier_distribution: dist,
        };

        assert_eq!(stats.high_tier_percentage(), 0.0);
        assert!(!stats.is_high_tier_dominated());
    }
}
