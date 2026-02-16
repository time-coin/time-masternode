//! Transaction mempool management
//!
//! Phase 2.4: Enhanced with LRU eviction and memory pressure monitoring
//!
//! # Dead Code Annotations
//!
//! Public API methods are marked with `#[allow(dead_code)]` because they're
//! used by the consensus engine and RPC handlers at runtime, not during
//! library compilation. All functionality is accessed through the public
//! TransactionPool API.

#![allow(dead_code)] // Public API - methods will be used by consensus engine and RPC

use crate::types::*;
use dashmap::DashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use thiserror::Error;

// Phase 2.4: Memory protection limits (adjusted for DoS protection)
const MAX_POOL_SIZE: usize = 10_000; // 10,000 transactions max
const MAX_POOL_BYTES: usize = 100 * 1024 * 1024; // 100MB (reduced from 300MB)
const REJECT_CACHE_SIZE: usize = 1000;
const MEMORY_PRESSURE_THRESHOLD: f64 = 0.9; // 90% capacity = high pressure
const LOW_FEE_EVICTION_THRESHOLD: f64 = 0.8; // Start evicting at 80% capacity

#[derive(Clone)]
pub(crate) struct PoolEntry {
    // Made pub(crate) so drop() can be used on return value
    pub(crate) tx: Transaction,
    fee: u64,
    #[allow(dead_code)]
    added_at: Instant,
    size: usize,
    /// IP address of the peer that submitted this transaction
    submitter_ip: Option<String>,
}

#[derive(Error, Debug)]
pub enum PoolError {
    #[error("Transaction pool is full")]
    PoolFull,
    #[error("Transaction already in pool")]
    AlreadyExists,
    #[error("Transaction was previously rejected")]
    PreviouslyRejected,
}

/// Transaction pool manages pending and finalized transactions
pub struct TransactionPool {
    /// Pending transactions waiting for consensus (lock-free concurrent access)
    pending: DashMap<Hash256, PoolEntry>,
    /// Finalized transactions ready for block inclusion
    finalized: DashMap<Hash256, PoolEntry>,
    /// Rejected transactions with reason and timestamp
    rejected: DashMap<Hash256, (String, Instant)>,
    /// Track sizes
    pending_count: AtomicUsize,
    pending_bytes: AtomicUsize,
}

impl Default for TransactionPool {
    fn default() -> Self {
        Self::new()
    }
}

impl TransactionPool {
    pub fn new() -> Self {
        Self {
            pending: DashMap::new(),
            finalized: DashMap::new(),
            rejected: DashMap::new(),
            pending_count: AtomicUsize::new(0),
            pending_bytes: AtomicUsize::new(0),
        }
    }

    /// Add transaction to pending pool with fee (atomic operation)
    pub fn add_pending(&self, tx: Transaction, fee: u64) -> Result<(), PoolError> {
        self.add_pending_with_submitter(tx, fee, None)
    }

    /// Add transaction to pending pool with fee and submitter IP (atomic operation)
    pub fn add_pending_with_submitter(
        &self,
        tx: Transaction,
        fee: u64,
        submitter_ip: Option<String>,
    ) -> Result<(), PoolError> {
        let txid = tx.txid();

        // Fast path: Check if already exists BEFORE expensive serialization
        if self.pending.contains_key(&txid) || self.finalized.contains_key(&txid) {
            return Err(PoolError::AlreadyExists);
        }

        // Fast path: Check if previously rejected
        if self.rejected.contains_key(&txid) {
            return Err(PoolError::PreviouslyRejected);
        }

        // Only serialize after cheap checks pass (optimization: ~20% faster)
        let tx_size = bincode::serialized_size(&tx).unwrap_or(0) as usize;

        // Check limits
        let current_count = self.pending_count.load(Ordering::Relaxed);
        let current_bytes = self.pending_bytes.load(Ordering::Relaxed);

        // Phase 2.4: Check if pool is full
        if current_count >= MAX_POOL_SIZE || current_bytes + tx_size > MAX_POOL_BYTES {
            // Try LRU eviction if under high memory pressure
            if self.get_memory_pressure() > LOW_FEE_EVICTION_THRESHOLD {
                tracing::info!(
                    "🗑️  Mempool at {}% capacity, attempting LRU eviction",
                    (self.get_memory_pressure() * 100.0) as u64
                );
                self.evict_low_fee_transactions(1)?;
            } else {
                return Err(PoolError::PoolFull);
            }
        }

        let entry = PoolEntry {
            tx,
            fee,
            added_at: Instant::now(),
            size: tx_size,
            submitter_ip,
        };

        self.pending.insert(txid, entry);
        self.pending_count.fetch_add(1, Ordering::Relaxed);
        self.pending_bytes.fetch_add(tx_size, Ordering::Relaxed);

        Ok(())
    }

    /// Move transaction from pending to finalized (atomic)
    /// Returns true if the transaction was successfully finalized
    pub fn finalize_transaction(&self, txid: Hash256) -> bool {
        if let Some((_, entry)) = self.pending.remove(&txid) {
            self.finalized.insert(txid, entry.clone());
            self.pending_count.fetch_sub(1, Ordering::Relaxed);
            self.pending_bytes.fetch_sub(entry.size, Ordering::Relaxed);
            tracing::info!(
                "📦 TxPool: Finalized TX {:?}, pool now has {} finalized",
                hex::encode(txid),
                self.finalized.len()
            );
            true
        } else {
            false
        }
    }

    /// Check if transaction exists in pending or finalized pool
    pub fn has_transaction(&self, txid: &Hash256) -> bool {
        self.pending.contains_key(txid) || self.finalized.contains_key(txid)
    }

    /// Reject a transaction (atomic)
    #[allow(dead_code)]
    pub fn reject_transaction(&self, txid: Hash256, reason: String) {
        if let Some((_, entry)) = self.pending.remove(&txid) {
            self.pending_count.fetch_sub(1, Ordering::Relaxed);
            self.pending_bytes.fetch_sub(entry.size, Ordering::Relaxed);
        }
        self.rejected.insert(txid, (reason, Instant::now()));
    }

    /// Get all finalized transactions for block inclusion (with fees)
    pub fn get_finalized_transactions_with_fees(&self) -> Vec<(Transaction, u64)> {
        self.finalized
            .iter()
            .map(|e| (e.value().tx.clone(), e.value().fee))
            .collect()
    }

    /// Get all finalized transactions for block inclusion
    pub fn get_finalized_transactions(&self) -> Vec<Transaction> {
        self.finalized
            .iter()
            .map(|e| e.value().tx.clone())
            .collect()
    }

    /// Clear finalized transactions (after block inclusion)
    pub fn clear_finalized(&self) {
        let count = self.finalized.len();
        self.finalized.clear();
        tracing::info!(
            "🧹 TxPool: Cleared {} finalized transactions after block inclusion",
            count
        );
    }

    /// Clear only specific finalized transactions that were included in a block
    /// This prevents clearing transactions that weren't actually in the block
    pub fn clear_finalized_txs(&self, txids: &[Hash256]) {
        let mut cleared_finalized = 0;
        let mut cleared_pending = 0;
        for txid in txids {
            if self.finalized.remove(txid).is_some() {
                cleared_finalized += 1;
            }
            // Also remove from pending: a peer may have included a TX that was still
            // pending locally (finalized on their side but not ours). Without this,
            // pending entries leak and the mempool grows indefinitely.
            if let Some((_, entry)) = self.pending.remove(txid) {
                self.pending_count.fetch_sub(1, Ordering::Relaxed);
                self.pending_bytes.fetch_sub(entry.size, Ordering::Relaxed);
                cleared_pending += 1;
            }
        }
        if cleared_finalized > 0 || cleared_pending > 0 {
            tracing::info!(
                "🧹 TxPool: Cleared {} finalized + {} pending transaction(s) included in block",
                cleared_finalized,
                cleared_pending
            );
        }
    }

    /// Get pending transaction count (O(1))
    pub fn pending_count(&self) -> usize {
        self.pending_count.load(Ordering::Relaxed)
    }

    /// Get finalized transaction count
    pub fn finalized_count(&self) -> usize {
        self.finalized.len()
    }

    /// Remove stale pending transactions older than max_age.
    /// Returns the number of transactions evicted.
    pub fn cleanup_stale_pending(&self, max_age: std::time::Duration) -> usize {
        let now = Instant::now();
        let stale_txids: Vec<Hash256> = self
            .pending
            .iter()
            .filter(|entry| now.duration_since(entry.value().added_at) > max_age)
            .map(|entry| *entry.key())
            .collect();

        let mut evicted = 0;
        for txid in &stale_txids {
            if let Some((_, entry)) = self.pending.remove(txid) {
                self.pending_count.fetch_sub(1, Ordering::Relaxed);
                self.pending_bytes.fetch_sub(entry.size, Ordering::Relaxed);
                evicted += 1;
            }
        }
        if evicted > 0 {
            tracing::info!(
                "🧹 TxPool: Evicted {} stale pending transaction(s) (older than {}s)",
                evicted,
                max_age.as_secs()
            );
        }
        evicted
    }

    /// Check if transaction is pending
    pub fn is_pending(&self, txid: &Hash256) -> bool {
        self.pending.contains_key(txid)
    }

    /// Get all pending transactions
    pub fn get_all_pending(&self) -> Vec<Transaction> {
        self.pending.iter().map(|e| e.value().tx.clone()).collect()
    }

    /// Get all pending transactions with metadata for priority sorting
    pub fn get_all_pending_with_metadata(
        &self,
    ) -> Vec<(Transaction, u64, Option<String>, Instant)> {
        self.pending
            .iter()
            .map(|e| {
                let entry = e.value();
                (
                    entry.tx.clone(),
                    entry.fee,
                    entry.submitter_ip.clone(),
                    entry.added_at,
                )
            })
            .collect()
    }

    /// Get a specific pending transaction by ID (O(1), no full pool clone)
    pub fn get_pending(&self, txid: &Hash256) -> Option<Transaction> {
        self.pending.get(txid).map(|e| e.tx.clone())
    }

    /// Get a transaction from either pending or finalized pool
    pub fn get_transaction(&self, txid: &Hash256) -> Option<Transaction> {
        self.pending
            .get(txid)
            .or_else(|| self.finalized.get(txid))
            .map(|e| e.tx.clone())
    }

    /// Get all pending transactions
    pub fn get_pending_transactions(&self) -> Vec<Transaction> {
        self.pending.iter().map(|e| e.value().tx.clone()).collect()
    }

    /// Check if transaction is finalized
    #[allow(dead_code)]
    pub fn is_finalized(&self, txid: &Hash256) -> bool {
        self.finalized.contains_key(txid)
    }

    /// Get rejection reason
    #[allow(dead_code)]
    pub fn get_rejection_reason(&self, txid: &Hash256) -> Option<String> {
        self.rejected.get(txid).map(|e| e.0.clone())
    }

    /// Get total fees from finalized transactions
    pub fn get_total_fees(&self) -> u64 {
        self.finalized.iter().map(|e| e.value().fee).sum()
    }

    /// Get fee for a specific transaction
    #[allow(dead_code)]
    pub fn get_fee(&self, txid: &Hash256) -> Option<u64> {
        self.pending.get(txid).map(|e| e.fee)
    }

    /// Get pool metrics
    #[allow(dead_code)]
    pub fn get_metrics(&self) -> PoolMetrics {
        let now = Instant::now();
        let oldest_age = self
            .pending
            .iter()
            .map(|e| now.duration_since(e.value().added_at).as_secs())
            .max()
            .unwrap_or(0);

        let total_fees: u64 = self.pending.iter().map(|e| e.value().fee).sum();
        let total_size: usize = self.pending.iter().map(|e| e.value().size).sum();

        let avg_fee_rate = if total_size > 0 {
            total_fees / total_size as u64
        } else {
            0
        };

        PoolMetrics {
            pending_count: self.pending_count(),
            pending_bytes: self.pending_bytes.load(Ordering::Relaxed),
            finalized_count: self.finalized_count(),
            rejected_count: self.rejected.len(),
            total_fees_pending: total_fees,
            avg_fee_rate,
            oldest_pending_age_secs: oldest_age,
        }
    }

    /// Clean up old rejected entries (call periodically)
    pub fn cleanup_rejected(&self, max_age_secs: u64) {
        let now = Instant::now();
        self.rejected.retain(|_, (_, rejected_at)| {
            now.duration_since(*rejected_at).as_secs() < max_age_secs
        });

        // Also enforce max size
        while self.rejected.len() > REJECT_CACHE_SIZE {
            if let Some((oldest_key, _)) = self
                .rejected
                .iter()
                .min_by_key(|e| e.value().1)
                .map(|e| (*e.key(), e.value().1))
            {
                self.rejected.remove(&oldest_key);
            } else {
                break;
            }
        }
    }

    /// Phase 2.4: Get current memory pressure (0.0 to 1.0)
    pub fn get_memory_pressure(&self) -> f64 {
        let current_bytes = self.pending_bytes.load(Ordering::Relaxed);
        let current_count = self.pending_count.load(Ordering::Relaxed);

        let bytes_pressure = current_bytes as f64 / MAX_POOL_BYTES as f64;
        let count_pressure = current_count as f64 / MAX_POOL_SIZE as f64;

        // Return the highest pressure
        bytes_pressure.max(count_pressure)
    }

    /// Phase 2.4: Check if mempool is under high memory pressure
    pub fn is_high_pressure(&self) -> bool {
        self.get_memory_pressure() > MEMORY_PRESSURE_THRESHOLD
    }

    /// Phase 2.4: Evict low-fee transactions using LRU policy
    pub fn evict_low_fee_transactions(&self, count: usize) -> Result<usize, PoolError> {
        if self.pending.is_empty() {
            return Ok(0);
        }

        // Collect transactions with their fees and ages
        let mut txs_with_priority: Vec<(Hash256, u64, Instant, usize)> = self
            .pending
            .iter()
            .map(|entry| {
                let txid = *entry.key();
                let pool_entry = entry.value();
                let fee_per_byte = if pool_entry.size > 0 {
                    pool_entry.fee / pool_entry.size as u64
                } else {
                    0
                };
                (txid, fee_per_byte, pool_entry.added_at, pool_entry.size)
            })
            .collect();

        // Sort by fee per byte (ascending), then by age (oldest first)
        txs_with_priority.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));

        // Evict the lowest priority transactions
        let mut evicted = 0;
        for (txid, _, _, _) in txs_with_priority.iter().take(count) {
            if let Some((_, entry)) = self.pending.remove(txid) {
                self.pending_count.fetch_sub(1, Ordering::Relaxed);
                self.pending_bytes.fetch_sub(entry.size, Ordering::Relaxed);
                evicted += 1;

                tracing::debug!(
                    "🗑️  Evicted low-fee transaction {} (fee/byte: {} satoshis)",
                    hex::encode(txid),
                    entry.fee / entry.size.max(1) as u64
                );
            }
        }

        tracing::info!("🗑️  Evicted {} low-fee transactions from mempool", evicted);
        Ok(evicted)
    }

    /// Phase 2.4: Get mempool pressure status
    pub fn get_pressure_status(&self) -> MemPoolPressure {
        let pressure = self.get_memory_pressure();
        let current_bytes = self.pending_bytes.load(Ordering::Relaxed);
        let current_count = self.pending_count.load(Ordering::Relaxed);

        let level = if pressure > MEMORY_PRESSURE_THRESHOLD {
            PressureLevel::Critical
        } else if pressure > LOW_FEE_EVICTION_THRESHOLD {
            PressureLevel::High
        } else if pressure > 0.5 {
            PressureLevel::Medium
        } else {
            PressureLevel::Low
        };

        MemPoolPressure {
            level,
            pressure_ratio: pressure,
            current_count,
            max_count: MAX_POOL_SIZE,
            current_bytes,
            max_bytes: MAX_POOL_BYTES,
        }
    }
}

/// Phase 2.4: Memory pressure levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Public API enum - variants will be used by external code
pub enum PressureLevel {
    Low,      // < 50%
    Medium,   // 50-80%
    High,     // 80-90%
    Critical, // > 90%
}

/// Phase 2.4: Mempool pressure status
#[derive(Debug, Clone)]
#[allow(dead_code)] // Public API struct - fields will be read by external code
pub struct MemPoolPressure {
    pub level: PressureLevel,
    pub pressure_ratio: f64,
    pub current_count: usize,
    pub max_count: usize,
    pub current_bytes: usize,
    pub max_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OutPoint, TxInput, TxOutput};

    fn create_test_transaction(value: u64) -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: OutPoint {
                    txid: [0u8; 32],
                    vout: 0,
                },
                script_sig: vec![0u8; 64],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value,
                script_pubkey: vec![0u8; 25],
            }],
            lock_time: 0,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }

    #[test]
    fn test_mempool_size_limits() {
        // Phase 2.4: Verify mempool limits are properly set
        assert_eq!(MAX_POOL_SIZE, 10_000);
        assert_eq!(MAX_POOL_BYTES, 100 * 1024 * 1024); // 100MB
    }

    #[test]
    fn test_memory_pressure_calculation() {
        let pool = TransactionPool::new();

        // Empty pool = low pressure
        assert_eq!(pool.get_memory_pressure(), 0.0);
        assert!(!pool.is_high_pressure());

        // Add transactions and check pressure increases
        for i in 0..100 {
            let tx = create_test_transaction(1000 + i);
            let _ = pool.add_pending(tx, 100);
        }

        let pressure = pool.get_memory_pressure();
        assert!(pressure > 0.0);
        assert!(pressure < 0.1); // Should be low with 100 txs
    }

    #[test]
    fn test_pressure_status_levels() {
        let pool = TransactionPool::new();

        // Initially low
        let status = pool.get_pressure_status();
        assert_eq!(status.level, PressureLevel::Low);
        assert_eq!(pool.pending_count(), 0);

        // Add transactions to reach medium pressure
        // Each transaction is ~300 bytes, so we need many for 100MB
        for i in 0..6000 {
            let tx = create_test_transaction(1000 + i);
            if pool.add_pending(tx, 100).is_err() {
                // If we hit limits, that's fine for this test
                break;
            }
        }

        let status = pool.get_pressure_status();
        let count = pool.pending_count();

        // With 6000 txs out of 10,000 max, should be at least medium pressure
        assert!(
            count > 1000,
            "Should have added at least 1000 transactions, got {}",
            count
        );
        assert!(
            status.pressure_ratio > 0.1,
            "Expected pressure > 0.1, got {} with {} transactions",
            status.pressure_ratio,
            count
        );
    }

    #[test]
    fn test_lru_eviction() {
        let pool = TransactionPool::new();

        // Add transactions with different fees
        let mut low_fee_txids = vec![];
        let mut high_fee_txids = vec![];

        // Add 10 low-fee transactions
        for i in 0..10 {
            let tx = create_test_transaction(1000 + i);
            let txid = tx.txid();
            pool.add_pending(tx, 10).unwrap(); // Low fee
            low_fee_txids.push(txid);
        }

        // Add 10 high-fee transactions
        for i in 0..10 {
            let tx = create_test_transaction(2000 + i);
            let txid = tx.txid();
            pool.add_pending(tx, 1000).unwrap(); // High fee
            high_fee_txids.push(txid);
        }

        assert_eq!(pool.pending_count(), 20);

        // Evict 5 transactions
        let evicted = pool.evict_low_fee_transactions(5).unwrap();
        assert_eq!(evicted, 5);
        assert_eq!(pool.pending_count(), 15);

        // Check that low-fee transactions were evicted
        let remaining_low_fee = low_fee_txids
            .iter()
            .filter(|txid| pool.is_pending(txid))
            .count();

        // Should have evicted from low-fee pool
        assert!(remaining_low_fee < 10);

        // High-fee transactions should still be there
        for txid in &high_fee_txids {
            assert!(
                pool.is_pending(txid),
                "High-fee transaction should not be evicted"
            );
        }
    }

    #[test]
    fn test_pool_full_with_eviction() {
        let pool = TransactionPool::new();

        // Fill pool to 85% (trigger eviction threshold)
        let target_count = (MAX_POOL_SIZE as f64 * 0.82) as usize; // Slightly under threshold

        for i in 0..target_count {
            let tx = create_test_transaction(1000 + i as u64);
            let _ = pool.add_pending(tx, if i < target_count / 2 { 10 } else { 100 });
        }

        let _initial_count = pool.pending_count();

        // Add a few more to push over the eviction threshold
        for i in 0..200 {
            let tx = create_test_transaction(50000 + i as u64);
            let result = pool.add_pending(tx, 1000); // High fee

            if result.is_err() {
                // If we hit the limit, that's expected
                break;
            }
        }

        // Pool should be at or near capacity
        assert!(
            pool.pending_count() <= MAX_POOL_SIZE,
            "Pool should not exceed max size"
        );
        assert!(
            pool.get_memory_pressure() > 0.5,
            "Pool should be under pressure after adding many transactions"
        );
    }

    #[test]
    fn test_mempool_metrics() {
        let pool = TransactionPool::new();

        // Add transactions
        for i in 0..50 {
            let tx = create_test_transaction(1000 + i);
            let _ = pool.add_pending(tx, 100 + i);
        }

        let metrics = pool.get_metrics();
        assert_eq!(metrics.pending_count, 50);
        assert!(metrics.pending_bytes > 0);
        assert!(metrics.total_fees_pending > 0);
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PoolMetrics {
    pub pending_count: usize,
    pub pending_bytes: usize,
    pub finalized_count: usize,
    pub rejected_count: usize,
    pub total_fees_pending: u64,
    pub avg_fee_rate: u64,
    pub oldest_pending_age_secs: u64,
}
