use crate::types::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Transaction pool manages pending and finalized transactions
pub struct TransactionPool {
    /// Pending transactions waiting for consensus
    pending: Arc<RwLock<HashMap<Hash256, Transaction>>>,
    /// Finalized transactions ready for block inclusion
    finalized: Arc<RwLock<HashMap<Hash256, Transaction>>>,
    /// Rejected transactions
    rejected: Arc<RwLock<HashMap<Hash256, String>>>,
    /// Transaction fees (txid -> fee in satoshis)
    fees: Arc<RwLock<HashMap<Hash256, u64>>>,
}

impl TransactionPool {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(RwLock::new(HashMap::new())),
            finalized: Arc::new(RwLock::new(HashMap::new())),
            rejected: Arc::new(RwLock::new(HashMap::new())),
            fees: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Add transaction to pending pool with fee
    #[allow(dead_code)]
    pub async fn add_pending(&self, tx: Transaction, fee: u64) {
        let txid = tx.txid();
        self.pending.write().await.insert(txid, tx);
        self.fees.write().await.insert(txid, fee);
    }

    /// Move transaction from pending to finalized
    pub async fn finalize_transaction(&self, txid: Hash256) -> Option<Transaction> {
        let mut pending = self.pending.write().await;
        if let Some(tx) = pending.remove(&txid) {
            self.finalized.write().await.insert(txid, tx.clone());
            Some(tx)
        } else {
            None
        }
    }

    /// Reject a transaction
    pub async fn reject_transaction(&self, txid: Hash256, reason: String) {
        self.pending.write().await.remove(&txid);
        self.rejected.write().await.insert(txid, reason);
    }

    /// Get all finalized transactions for block inclusion
    pub async fn get_finalized_transactions(&self) -> Vec<Transaction> {
        self.finalized.read().await.values().cloned().collect()
    }

    /// Clear finalized transactions (after block inclusion)
    #[allow(dead_code)]
    pub async fn clear_finalized(&self) {
        self.finalized.write().await.clear();
    }

    /// Get pending transaction count
    #[allow(dead_code)]
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Get finalized transaction count
    #[allow(dead_code)]
    pub async fn finalized_count(&self) -> usize {
        self.finalized.read().await.len()
    }

    /// Check if transaction is pending
    #[allow(dead_code)]
    pub async fn is_pending(&self, txid: &Hash256) -> bool {
        self.pending.read().await.contains_key(txid)
    }

    /// Check if transaction is finalized
    #[allow(dead_code)]
    pub async fn is_finalized(&self, txid: &Hash256) -> bool {
        self.finalized.read().await.contains_key(txid)
    }

    /// Get rejection reason
    #[allow(dead_code)]
    pub async fn get_rejection_reason(&self, txid: &Hash256) -> Option<String> {
        self.rejected.read().await.get(txid).cloned()
    }

    /// Get total fees from finalized transactions
    pub async fn get_total_fees(&self) -> u64 {
        let fees = self.fees.read().await;
        let finalized = self.finalized.read().await;
        finalized
            .keys()
            .filter_map(|txid| fees.get(txid).copied())
            .sum()
    }

    /// Get fee for a specific transaction
    #[allow(dead_code)]
    pub async fn get_fee(&self, txid: &Hash256) -> Option<u64> {
        self.fees.read().await.get(txid).copied()
    }
}
