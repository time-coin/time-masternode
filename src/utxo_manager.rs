use crate::storage::UtxoStorage;
use crate::types::*;
use dashmap::DashMap;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum UtxoError {
    #[error("UTXO already locked or spent")]
    AlreadyUsed,

    #[error("Storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),
}

#[allow(dead_code)]
pub struct UTXOStateManager {
    pub storage: Arc<dyn UtxoStorage>,
    pub utxo_states: DashMap<OutPoint, UTXOState>,
}

impl UTXOStateManager {
    #[allow(dead_code)]
    pub fn new() -> Self {
        use crate::storage::InMemoryUtxoStorage;
        Self {
            storage: Arc::new(InMemoryUtxoStorage::new()),
            utxo_states: DashMap::new(),
        }
    }

    pub fn new_with_storage(storage: Arc<dyn UtxoStorage>) -> Self {
        Self {
            storage,
            utxo_states: DashMap::new(),
        }
    }

    pub async fn add_utxo(&self, utxo: UTXO) -> Result<(), UtxoError> {
        let outpoint = utxo.outpoint.clone();
        self.storage.add_utxo(utxo).await?;
        self.utxo_states.insert(outpoint, UTXOState::Unspent);
        Ok(())
    }

    pub async fn remove_utxo(&self, outpoint: &OutPoint) -> Result<(), UtxoError> {
        self.storage.remove_utxo(outpoint).await?;
        self.utxo_states.remove(outpoint);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn lock_utxo(&self, outpoint: &OutPoint, txid: Hash256) -> Result<(), UtxoError> {
        use dashmap::mapref::entry::Entry;

        match self.utxo_states.entry(outpoint.clone()) {
            Entry::Occupied(mut entry) => {
                if matches!(entry.get(), UTXOState::Unspent) {
                    entry.insert(UTXOState::Locked {
                        txid,
                        locked_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64,
                    });
                    Ok(())
                } else {
                    Err(UtxoError::AlreadyUsed)
                }
            }
            Entry::Vacant(_) => Err(UtxoError::AlreadyUsed),
        }
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

    #[allow(dead_code)]
    pub async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO> {
        self.storage.get_utxo(outpoint).await
    }

    #[allow(dead_code)]
    pub async fn list_all_utxos(&self) -> Vec<UTXO> {
        self.storage.list_utxos().await
    }

    /// Calculate hash of entire UTXO set for state comparison - optimized with direct byte comparison
    pub async fn calculate_utxo_set_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut utxos = self.list_all_utxos().await;
        // Sort by bytes directly - no string allocations!
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

    /// Get UTXO differences between local and remote state
    /// TODO: This will be used in future proper UTXO reconciliation with multi-peer consensus
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

        // UTXOs we have but remote doesn't (we should remove)
        let to_remove: Vec<OutPoint> = local_set
            .difference(&remote_set)
            .map(|(outpoint, _)| outpoint.clone())
            .collect();

        // UTXOs remote has but we don't (we should add)
        let to_add: Vec<UTXO> = remote_utxos
            .iter()
            .filter(|u| !local_set.contains(&(u.outpoint.clone(), u.value)))
            .cloned()
            .collect();

        (to_remove, to_add)
    }

    /// Reconcile UTXO state with remote node
    /// TODO: This will be used in future proper UTXO reconciliation with multi-peer consensus
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
