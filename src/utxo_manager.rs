use crate::storage::UtxoStorage;
use crate::types::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum UtxoError {
    #[error("UTXO already locked or spent")]
    AlreadyUsed,
}

#[allow(dead_code)]
pub struct UTXOStateManager {
    pub storage: Arc<dyn UtxoStorage>,
    pub utxo_states: Arc<RwLock<HashMap<OutPoint, UTXOState>>>,
}

impl UTXOStateManager {
    #[allow(dead_code)]
    pub fn new() -> Self {
        use crate::storage::InMemoryUtxoStorage;
        Self {
            storage: Arc::new(InMemoryUtxoStorage::new()),
            utxo_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn new_with_storage(storage: Arc<dyn UtxoStorage>) -> Self {
        Self {
            storage,
            utxo_states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn add_utxo(&self, utxo: UTXO) {
        let outpoint = utxo.outpoint.clone();
        let _ = self.storage.add_utxo(utxo).await;
        self.utxo_states
            .write()
            .await
            .insert(outpoint, UTXOState::Unspent);
    }

    #[allow(dead_code)]
    pub async fn lock_utxo(&self, outpoint: &OutPoint, txid: Hash256) -> Result<(), UtxoError> {
        let mut states = self.utxo_states.write().await;
        match states.get(outpoint) {
            Some(UTXOState::Unspent) => {
                states.insert(
                    outpoint.clone(),
                    UTXOState::Locked {
                        txid,
                        locked_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64,
                    },
                );
                Ok(())
            }
            _ => Err(UtxoError::AlreadyUsed),
        }
    }

    pub async fn get_state(&self, outpoint: &OutPoint) -> Option<UTXOState> {
        self.utxo_states.read().await.get(outpoint).cloned()
    }

    pub async fn update_state(&self, outpoint: &OutPoint, state: UTXOState) {
        self.utxo_states
            .write()
            .await
            .insert(outpoint.clone(), state);
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

    /// Calculate hash of entire UTXO set for state comparison
    pub async fn calculate_utxo_set_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};

        let mut utxos = self.list_all_utxos().await;
        // Sort by outpoint for deterministic ordering
        utxos.sort_by(|a, b| {
            let a_key = format!("{}:{}", hex::encode(a.outpoint.txid), a.outpoint.vout);
            let b_key = format!("{}:{}", hex::encode(b.outpoint.txid), b.outpoint.vout);
            a_key.cmp(&b_key)
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
            self.utxo_states.write().await.remove(&outpoint);
        }

        for utxo in to_add {
            self.add_utxo(utxo).await;
        }

        tracing::info!(
            "🔄 Reconciled UTXO state: removed {}, added {}",
            remove_count,
            add_count
        );
        Ok(())
    }
}
