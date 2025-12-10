use crate::block::types::Block;
use crate::types::*;
use crate::utxo_manager::UTXOStateManager;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[allow(dead_code)]
pub struct ConsensusEngine {
    pub masternodes: Vec<Masternode>,
    pub utxo_manager: Arc<UTXOStateManager>,
    pub votes: Arc<RwLock<HashMap<Hash256, Vec<Vote>>>>,
}

impl ConsensusEngine {
    pub fn new(masternodes: Vec<Masternode>, utxo_manager: Arc<UTXOStateManager>) -> Self {
        Self {
            masternodes,
            utxo_manager,
            votes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn validate_transaction(&self, tx: &Transaction) -> bool {
        for input in &tx.inputs {
            if self
                .utxo_manager
                .get_state(&input.previous_output)
                .await
                .is_none()
            {
                return false;
            }
        }
        true
    }

    pub async fn process_transaction(&self, tx: Transaction) -> Result<(), String> {
        let txid = tx.txid();

        for input in &tx.inputs {
            if self
                .utxo_manager
                .lock_utxo(&input.previous_output, txid)
                .await
                .is_err()
            {
                return Err("UTXO already locked".to_string());
            }
        }

        let n = self.masternodes.len() as u32;
        if n == 0 {
            return Err("No masternodes".to_string());
        }

        for input in &tx.inputs {
            self.utxo_manager
                .update_state(
                    &input.previous_output,
                    UTXOState::SpentPending {
                        txid,
                        votes: 0,
                        total_nodes: n,
                        spent_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64,
                    },
                )
                .await;
        }

        let mut approvals = 0u32;
        for _mn in &self.masternodes {
            if self.validate_transaction(&tx).await {
                approvals += 1;
            }
        }

        let quorum = (2 * n).div_ceil(3);
        if approvals >= quorum {
            for input in &tx.inputs {
                self.utxo_manager
                    .update_state(
                        &input.previous_output,
                        UTXOState::SpentFinalized {
                            txid,
                            finalized_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_secs() as i64,
                            votes: approvals,
                        },
                    )
                    .await;
            }

            for (i, output) in tx.outputs.iter().enumerate() {
                let new_outpoint = OutPoint {
                    txid,
                    vout: i as u32,
                };
                let utxo = UTXO {
                    outpoint: new_outpoint,
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    address: "recipient".to_string(),
                };
                self.utxo_manager.add_utxo(utxo).await;
            }

            println!(
                "✅ Transaction {} finalized instantly!",
                ::hex::encode(txid)
            );
            Ok(())
        } else {
            for input in &tx.inputs {
                self.utxo_manager
                    .update_state(&input.previous_output, UTXOState::Unspent)
                    .await;
            }
            Err("Failed to reach consensus".to_string())
        }
    }

    pub async fn get_finalized_transactions_for_block(&self) -> Vec<Transaction> {
        self.utxo_manager.get_finalized_transactions().await
    }

    pub async fn get_active_masternodes(&self) -> Vec<Masternode> {
        self.masternodes.clone()
    }

    pub async fn generate_deterministic_block(&self, height: u64, _timestamp: i64) -> Block {
        use crate::block::generator::DeterministicBlockGenerator;

        let finalized_txs = self.get_finalized_transactions_for_block().await;
        let masternodes = self.get_active_masternodes().await;
        let previous_hash = [0u8; 32];
        let base_reward = 100;

        DeterministicBlockGenerator::generate(
            height,
            previous_hash,
            finalized_txs,
            masternodes,
            base_reward,
        )
    }
}
