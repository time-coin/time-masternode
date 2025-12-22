use crate::types::*;
use dashmap::DashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use thiserror::Error;

const MAX_POOL_SIZE: usize = 10_000;
const MAX_POOL_BYTES: usize = 300 * 1024 * 1024; // 300MB
const REJECT_CACHE_SIZE: usize = 1000;

#[derive(Clone)]
struct PoolEntry {
    tx: Transaction,
    fee: u64,
    added_at: Instant,
    size: usize,
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
        let txid = tx.txid();
        let tx_size = bincode::serialized_size(&tx).unwrap_or(0) as usize;

        // Check if already exists
        if self.pending.contains_key(&txid) || self.finalized.contains_key(&txid) {
            return Err(PoolError::AlreadyExists);
        }

        // Check if previously rejected
        if self.rejected.contains_key(&txid) {
            return Err(PoolError::PreviouslyRejected);
        }

        // Check limits
        let current_count = self.pending_count.load(Ordering::Relaxed);
        let current_bytes = self.pending_bytes.load(Ordering::Relaxed);

        if current_count >= MAX_POOL_SIZE || current_bytes + tx_size > MAX_POOL_BYTES {
            return Err(PoolError::PoolFull);
        }

        let entry = PoolEntry {
            tx,
            fee,
            added_at: Instant::now(),
            size: tx_size,
        };

        self.pending.insert(txid, entry);
        self.pending_count.fetch_add(1, Ordering::Relaxed);
        self.pending_bytes.fetch_add(tx_size, Ordering::Relaxed);

        Ok(())
    }

    /// Move transaction from pending to finalized (atomic)
    pub fn finalize_transaction(&self, txid: Hash256) -> Option<Transaction> {
        self.pending.remove(&txid).map(|(_, entry)| {
            let tx = entry.tx.clone();
            self.finalized.insert(txid, entry.clone());
            self.pending_count.fetch_sub(1, Ordering::Relaxed);
            self.pending_bytes.fetch_sub(entry.size, Ordering::Relaxed);
            tx
        })
    }

    /// Reject a transaction (atomic)
    pub fn reject_transaction(&self, txid: Hash256, reason: String) {
        if let Some((_, entry)) = self.pending.remove(&txid) {
            self.pending_count.fetch_sub(1, Ordering::Relaxed);
            self.pending_bytes.fetch_sub(entry.size, Ordering::Relaxed);
        }
        self.rejected.insert(txid, (reason, Instant::now()));
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
        self.finalized.clear();
    }

    /// Get pending transaction count (O(1))
    pub fn pending_count(&self) -> usize {
        self.pending_count.load(Ordering::Relaxed)
    }

    /// Get finalized transaction count
    pub fn finalized_count(&self) -> usize {
        self.finalized.len()
    }

    /// Check if transaction is pending
    pub fn is_pending(&self, txid: &Hash256) -> bool {
        self.pending.contains_key(txid)
    }

    /// Get all pending transactions
    pub fn get_all_pending(&self) -> Vec<Transaction> {
        self.pending.iter().map(|e| e.value().tx.clone()).collect()
    }

    /// Get a specific pending transaction by ID (O(1), no full pool clone)
    pub fn get_pending(&self, txid: &Hash256) -> Option<Transaction> {
        self.pending.get(txid).map(|e| e.tx.clone())
    }

    /// Check if transaction is finalized
    pub fn is_finalized(&self, txid: &Hash256) -> bool {
        self.finalized.contains_key(txid)
    }

    /// Get rejection reason
    pub fn get_rejection_reason(&self, txid: &Hash256) -> Option<String> {
        self.rejected.get(txid).map(|e| e.0.clone())
    }

    /// Get total fees from finalized transactions
    pub fn get_total_fees(&self) -> u64 {
        self.finalized.iter().map(|e| e.value().fee).sum()
    }

    /// Get fee for a specific transaction
    pub fn get_fee(&self, txid: &Hash256) -> Option<u64> {
        self.pending.get(txid).map(|e| e.fee)
    }

    /// Get pool metrics
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
}

#[derive(Debug, Clone)]
pub struct PoolMetrics {
    pub pending_count: usize,
    pub pending_bytes: usize,
    pub finalized_count: usize,
    pub rejected_count: usize,
    pub total_fees_pending: u64,
    pub avg_fee_rate: u64,
    pub oldest_pending_age_secs: u64,
}
