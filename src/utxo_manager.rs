//! UTXO (Unspent Transaction Output) state management.
//!
//! Manages the UTXO set for tracking spendable outputs. Provides locking
//! mechanism for concurrent transaction processing.

use crate::storage::UtxoStorage;
use crate::types::*;
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

const LOCK_TIMEOUT_SECS: i64 = 30;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum UtxoError {
    #[error("UTXO not found")]
    NotFound,

    #[error("UTXO already locked by transaction {0}")]
    AlreadyLocked(String),

    #[error("UTXO already spent")]
    AlreadySpent,

    #[error("Lock expired")]
    LockExpired,

    #[error("Lock owned by different transaction")]
    LockMismatch,

    #[error("Storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),
}

pub struct UTXOStateManager {
    pub storage: Arc<dyn UtxoStorage>,
    pub utxo_states: DashMap<OutPoint, UTXOState>,
}

impl UTXOStateManager {
    pub fn new() -> Self {
        use crate::storage::InMemoryUtxoStorage;
        Self {
            storage: Arc::new(InMemoryUtxoStorage::new()),
            utxo_states: DashMap::new(),
        }
    }

    #[allow(dead_code)]
    pub fn new_with_storage(storage: Arc<dyn UtxoStorage>) -> Self {
        Self {
            storage,
            utxo_states: DashMap::new(),
        }
    }

    fn current_timestamp() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn is_lock_expired(locked_at: i64) -> bool {
        Self::current_timestamp() - locked_at > LOCK_TIMEOUT_SECS
    }

    pub async fn add_utxo(&self, utxo: UTXO) -> Result<(), UtxoError> {
        let outpoint = utxo.outpoint.clone();

        if self.utxo_states.contains_key(&outpoint) {
            return Err(UtxoError::AlreadySpent);
        }

        self.storage.add_utxo(utxo).await?;
        self.utxo_states.insert(outpoint, UTXOState::Unspent);
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        self.storage.remove_utxo(outpoint).await?;
        self.utxo_states.remove(outpoint);
        Ok(())
    }

    /// Mark a UTXO as spent (used when processing blocks)
    pub async fn spend_utxo(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        self.storage.remove_utxo(outpoint).await?;
        self.utxo_states.insert(
            outpoint.clone(),
            UTXOState::SpentFinalized {
                txid: [0u8; 32], // Block processing spend
                finalized_at: Self::current_timestamp(),
                votes: 0,
            },
        );
        Ok(())
    }

    /// Atomically lock a UTXO for a pending transaction
    pub fn lock_utxo(&self, outpoint: &OutPoint, txid: Hash256) -> Result<(), UtxoError> {
        use dashmap::mapref::entry::Entry;

        match self.utxo_states.entry(outpoint.clone()) {
            Entry::Occupied(mut entry) => match entry.get() {
                UTXOState::Unspent => {
                    entry.insert(UTXOState::Locked {
                        txid,
                        locked_at: Self::current_timestamp(),
                    });
                    tracing::debug!(
                        "🔒 Locked UTXO {:?} for tx {:?}",
                        outpoint,
                        hex::encode(txid)
                    );
                    Ok(())
                }
                UTXOState::Locked {
                    txid: existing_txid,
                    locked_at,
                } => {
                    if existing_txid == &txid {
                        return Ok(());
                    }

                    if Self::is_lock_expired(*locked_at) {
                        tracing::warn!("⏰ Expired lock on UTXO {:?}, allowing new lock", outpoint);
                        entry.insert(UTXOState::Locked {
                            txid,
                            locked_at: Self::current_timestamp(),
                        });
                        Ok(())
                    } else {
                        Err(UtxoError::AlreadyLocked(hex::encode(existing_txid)))
                    }
                }
                UTXOState::SpentPending { .. }
                | UTXOState::SpentFinalized { .. }
                | UTXOState::Confirmed { .. } => Err(UtxoError::AlreadySpent),
            },
            Entry::Vacant(entry) => {
                entry.insert(UTXOState::Locked {
                    txid,
                    locked_at: Self::current_timestamp(),
                });
                Ok(())
            }
        }
    }

    /// Unlock a UTXO (rollback a failed/timed-out transaction)
    #[allow(dead_code)]
    pub fn unlock_utxo(&self, outpoint: &OutPoint, txid: &Hash256) -> Result<(), UtxoError> {
        use dashmap::mapref::entry::Entry;

        match self.utxo_states.entry(outpoint.clone()) {
            Entry::Occupied(mut entry) => match entry.get() {
                UTXOState::Locked {
                    txid: locked_txid, ..
                } => {
                    if locked_txid == txid {
                        entry.insert(UTXOState::Unspent);
                        tracing::debug!("🔓 Unlocked UTXO {:?}", outpoint);
                        Ok(())
                    } else {
                        Err(UtxoError::LockMismatch)
                    }
                }
                UTXOState::Unspent => Ok(()),
                _ => Err(UtxoError::AlreadySpent),
            },
            Entry::Vacant(_) => Err(UtxoError::NotFound),
        }
    }

    /// Commit a locked UTXO as spent (finalize transaction)
    #[allow(dead_code)]
    pub async fn commit_spend(
        &self,
        outpoint: &OutPoint,
        txid: &Hash256,
        block_height: u64,
    ) -> Result<(), UtxoError> {
        use dashmap::mapref::entry::Entry;

        match self.utxo_states.entry(outpoint.clone()) {
            Entry::Occupied(mut entry) => match entry.get() {
                UTXOState::Locked {
                    txid: locked_txid, ..
                } => {
                    if locked_txid != txid {
                        return Err(UtxoError::LockMismatch);
                    }

                    self.storage.remove_utxo(outpoint).await?;

                    entry.insert(UTXOState::Confirmed {
                        txid: *txid,
                        block_height,
                        confirmed_at: Self::current_timestamp(),
                    });

                    tracing::info!(
                        "✅ Committed UTXO spend {:?} in block {}",
                        outpoint,
                        block_height
                    );
                    Ok(())
                }
                UTXOState::Unspent => {
                    tracing::warn!("⚠️ Spending unlocked UTXO {:?}", outpoint);
                    self.storage.remove_utxo(outpoint).await?;
                    entry.insert(UTXOState::Confirmed {
                        txid: *txid,
                        block_height,
                        confirmed_at: Self::current_timestamp(),
                    });
                    Ok(())
                }
                _ => Err(UtxoError::AlreadySpent),
            },
            Entry::Vacant(_) => Err(UtxoError::NotFound),
        }
    }

    /// Batch lock multiple UTXOs atomically
    #[allow(dead_code)]
    pub fn lock_utxos_atomic(
        &self,
        outpoints: &[OutPoint],
        txid: Hash256,
    ) -> Result<(), UtxoError> {
        let mut locked = Vec::with_capacity(outpoints.len());

        for outpoint in outpoints {
            match self.lock_utxo(outpoint, txid) {
                Ok(()) => locked.push(outpoint.clone()),
                Err(e) => {
                    for locked_outpoint in locked {
                        let _ = self.unlock_utxo(&locked_outpoint, &txid);
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Clean up expired locks
    #[allow(dead_code)]
    pub fn cleanup_expired_locks(&self) -> usize {
        let mut cleaned = 0;

        self.utxo_states.retain(|outpoint, state| {
            if let UTXOState::Locked { locked_at, txid } = state {
                if Self::is_lock_expired(*locked_at) {
                    tracing::info!(
                        "🧹 Cleaning expired lock on UTXO {:?} (tx {:?})",
                        outpoint,
                        hex::encode(txid)
                    );
                    *state = UTXOState::Unspent;
                    cleaned += 1;
                }
            }
            true
        });

        cleaned
    }

    pub fn get_state(&self, outpoint: &OutPoint) -> Option<UTXOState> {
        self.utxo_states.get(outpoint).map(|r| r.value().clone())
    }

    pub fn update_state(&self, outpoint: &OutPoint, state: UTXOState) {
        self.utxo_states.insert(outpoint.clone(), state);
    }

    #[allow(dead_code)]
    pub async fn get_finalized_transactions(&self) -> Vec<Transaction> {
        Vec::new()
    }

    pub async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO> {
        self.storage.get_utxo(outpoint).await
    }

    pub async fn list_all_utxos(&self) -> Vec<UTXO> {
        self.storage.list_utxos().await
    }

    #[allow(dead_code)]
    pub fn get_locked_utxos(&self) -> Vec<(OutPoint, Hash256, i64)> {
        self.utxo_states
            .iter()
            .filter_map(|entry| {
                if let UTXOState::Locked { txid, locked_at } = entry.value() {
                    Some((entry.key().clone(), *txid, *locked_at))
                } else {
                    None
                }
            })
            .collect()
    }

    #[allow(dead_code)]
    pub fn is_spendable(&self, outpoint: &OutPoint, by_txid: Option<&Hash256>) -> bool {
        match self.utxo_states.get(outpoint) {
            Some(ref state) => match state.value() {
                UTXOState::Unspent => true,
                UTXOState::Locked { txid, locked_at } => {
                    (by_txid == Some(txid)) || Self::is_lock_expired(*locked_at)
                }
                _ => false,
            },
            None => false,
        }
    }

    /// Calculate hash of entire UTXO set for state comparison
    #[allow(dead_code)]
    pub async fn calculate_utxo_set_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut utxos = self.list_all_utxos().await;
        utxos.sort_unstable_by(|a, b| {
            (&a.outpoint.txid, a.outpoint.vout).cmp(&(&b.outpoint.txid, b.outpoint.vout))
        });

        let mut hasher = Sha256::new();
        for utxo in utxos {
            hasher.update(utxo.outpoint.txid);
            hasher.update(utxo.outpoint.vout.to_le_bytes());
            hasher.update(utxo.value.to_le_bytes());
            hasher.update(&utxo.script_pubkey);
        }

        hasher.finalize().into()
    }

    #[allow(dead_code)]
    pub async fn get_utxo_diff(&self, remote_utxos: &[UTXO]) -> (Vec<OutPoint>, Vec<UTXO>) {
        let local_utxos = self.list_all_utxos().await;

        let local_set: std::collections::HashSet<_> = local_utxos
            .iter()
            .map(|u| (u.outpoint.clone(), u.value))
            .collect();

        let remote_set: std::collections::HashSet<_> = remote_utxos
            .iter()
            .map(|u| (u.outpoint.clone(), u.value))
            .collect();

        let to_remove: Vec<OutPoint> = local_set
            .difference(&remote_set)
            .map(|(outpoint, _)| outpoint.clone())
            .collect();

        let to_add: Vec<UTXO> = remote_utxos
            .iter()
            .filter(|u| !local_set.contains(&(u.outpoint.clone(), u.value)))
            .cloned()
            .collect();

        (to_remove, to_add)
    }

    #[allow(dead_code)]
    pub async fn reconcile_utxo_state(
        &self,
        to_remove: Vec<OutPoint>,
        to_add: Vec<UTXO>,
    ) -> Result<(), UtxoError> {
        let remove_count = to_remove.len();
        let add_count = to_add.len();

        for outpoint in to_remove {
            if let Err(e) = self.storage.remove_utxo(&outpoint).await {
                tracing::warn!("Failed to remove UTXO during reconciliation: {}", e);
            }
            self.utxo_states.remove(&outpoint);
        }

        for utxo in to_add {
            let _ = self.add_utxo(utxo).await;
        }

        tracing::info!(
            "🔄 Reconciled UTXO state: removed {}, added {}",
            remove_count,
            add_count
        );
        Ok(())
    }
}

impl Default for UTXOStateManager {
    fn default() -> Self {
        Self::new()
    }
}
