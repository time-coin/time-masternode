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
}
