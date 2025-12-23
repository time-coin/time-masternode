/// Avalanche-based transaction finality handler
/// Replaces BFT voting with Avalanche consensus protocol
use crate::consensus::{AvalancheConsensus, Preference};
use crate::transaction_pool::TransactionPool;
use crate::types::{Hash256, Transaction, UTXO, OutPoint, UTXOState};
use crate::utxo_manager::UTXOStateManager;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AvalancheTxHandler {
    pub avalanche: Arc<AvalancheConsensus>,
    pub tx_pool: Arc<TransactionPool>,
    pub utxo_manager: Arc<UTXOStateManager>,
}

impl AvalancheTxHandler {
    pub fn new(
        avalanche: Arc<AvalancheConsensus>,
        tx_pool: Arc<TransactionPool>,
        utxo_manager: Arc<UTXOStateManager>,
    ) -> Self {
        Self {
            avalanche,
            tx_pool,
            utxo_manager,
        }
    }

    /// Submit transaction for Avalanche consensus
    pub async fn submit_transaction(&self, tx: Transaction) -> Result<Hash256, String> {
        let txid = tx.txid();

        // Add to mempool
        let fee = self.calculate_fee(&tx).await;
        self.tx_pool
            .add_pending(tx.clone(), fee)
            .map_err(|e| format!("Failed to add to pool: {}", e))?;

        // Mark inputs as spent pending
        for input in &tx.inputs {
            let new_state = UTXOState::SpentPending {
                txid,
                votes: 0,
                total_nodes: 1,
                spent_at: chrono::Utc::now().timestamp(),
            };
            self.utxo_manager
                .update_state(&input.previous_output, new_state);
        }

        // Initiate Avalanche consensus
        self.avalanche
            .initiate_consensus(txid, Preference::Accept);

        // Run Avalanche until finalized
        self.run_avalanche_consensus(txid).await?;

        Ok(txid)
    }

    /// Run Avalanche consensus rounds until transaction is finalized
    async fn run_avalanche_consensus(&self, txid: Hash256) -> Result<(), String> {
        let max_rounds = 100;
        let mut round = 0;

        loop {
            // Execute query round
            if let Err(e) = self.avalanche.execute_query_round(txid).await {
                tracing::warn!("Query round error: {}", e);
                round += 1;
                if round >= max_rounds {
                    return Err("Avalanche consensus timeout".to_string());
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                continue;
            }

            // Check if finalized
            if self.avalanche.is_finalized(&txid) {
                tracing::info!(
                    "✅ Transaction {} finalized via Avalanche consensus",
                    hex::encode(txid)
                );
                self.finalize_transaction(txid).await?;
                return Ok(());
            }

            // Check if rejected
            if let Some(Preference::Reject) = self.avalanche.get_finalized_preference(&txid) {
                tracing::warn!("❌ Transaction {} rejected by Avalanche", hex::encode(txid));
                self.reject_transaction(txid).await?;
                return Err("Transaction rejected".to_string());
            }

            round += 1;
            if round >= max_rounds {
                return Err("Avalanche consensus timeout".to_string());
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    /// Finalize transaction after Avalanche consensus succeeds
    async fn finalize_transaction(&self, txid: Hash256) -> Result<(), String> {
        let tx = self
            .tx_pool
            .get_pending(&txid)
            .ok_or("Transaction not in pending pool")?;

        // Update inputs to Spent
        for input in &tx.inputs {
            let new_state = UTXOState::Spent {
                txid,
                finalized_at: chrono::Utc::now().timestamp(),
            };
            self.utxo_manager
                .update_state(&input.previous_output, new_state);
        }

        // Create new UTXOs from outputs
        for (idx, output) in tx.outputs.iter().enumerate() {
            let outpoint = OutPoint {
                txid,
                vout: idx as u32,
            };
            let utxo = UTXO {
                outpoint: outpoint.clone(),
                value: output.value,
                script_pubkey: output.script_pubkey.clone(),
                address: String::new(),
            };

            let _ = self.utxo_manager.add_utxo(utxo).await;

            // Mark as unspent
            self.utxo_manager
                .update_state(&outpoint, UTXOState::Unspent);
        }

        // Move to finalized pool
        self.tx_pool.finalize_transaction(txid);

        tracing::info!(
            "💾 Transaction {} finalized and available for block inclusion",
            hex::encode(txid)
        );

        Ok(())
    }

    /// Reject transaction after Avalanche consensus fails
    async fn reject_transaction(&self, txid: Hash256) -> Result<(), String> {
        let tx = self
            .tx_pool
            .get_pending(&txid)
            .ok_or("Transaction not in pending pool")?;

        // Return inputs to unspent
        for input in &tx.inputs {
            self.utxo_manager
                .update_state(&input.previous_output, UTXOState::Unspent);
        }

        // Remove from pool
        self.tx_pool.remove_pending(&txid);

        tracing::warn!(
            "❌ Transaction {} rejected by Avalanche consensus",
            hex::encode(txid)
        );

        Ok(())
    }

    /// Calculate transaction fee
    async fn calculate_fee(&self, tx: &Transaction) -> u64 {
        let input_sum: u64 = {
            let mut sum = 0u64;
            for input in &tx.inputs {
                if let Some(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                    sum += utxo.value;
                }
            }
            sum
        };
        let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
        input_sum.saturating_sub(output_sum)
    }
}
