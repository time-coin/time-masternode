/// Avalanche consensus protocol implementation for TimeCoin
/// Provides instant transaction finality without Byzantine quorum requirements
use crate::consensus::{AvalancheConfig, AvalancheConsensus, Preference};
use crate::masternode_registry::MasternodeRegistry;
use crate::transaction_pool::TransactionPool;
use crate::types::*;
use crate::utxo_manager::UTXOStateManager;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;

/// Errors for Avalanche consensus
#[derive(Error, Debug)]
pub enum AvalancheError {
    #[error("Consensus error: {0}")]
    ConsensusError(String),

    #[error("UTXO error: {0}")]
    UtxoError(String),

    #[error("Invalid transaction: {0}")]
    InvalidTransaction(String),

    #[error("Network error: {0}")]
    NetworkError(String),
}

/// Bridge between Avalanche consensus and TimeCoin transaction handling
pub struct AvalancheHandler {
    consensus: Arc<AvalancheConsensus>,
    utxo_manager: Arc<UTXOStateManager>,
    tx_pool: Arc<TransactionPool>,
    masternode_registry: Arc<MasternodeRegistry>,
    finality_tx: mpsc::UnboundedSender<FinalityEvent>,
}

/// Event indicating a transaction has achieved finality
#[derive(Debug, Clone)]
pub struct FinalityEvent {
    pub txid: Hash256,
    pub preference: Preference,
    pub confidence: usize,
}

impl AvalancheHandler {
    pub fn new(
        config: AvalancheConfig,
        utxo_manager: Arc<UTXOStateManager>,
        tx_pool: Arc<TransactionPool>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<FinalityEvent>), AvalancheError> {
        let consensus = Arc::new(
            AvalancheConsensus::new(config)
                .map_err(|e| AvalancheError::ConsensusError(e.to_string()))?,
        );

        let (finality_tx, finality_rx) = mpsc::unbounded_channel();

        Ok((
            Self {
                consensus,
                utxo_manager,
                tx_pool,
                masternode_registry,
                finality_tx,
            },
            finality_rx,
        ))
    }

    /// Initialize Avalanche with current validators
    pub async fn initialize_validators(&self) {
        let validators = self
            .masternode_registry
            .list_active()
            .await
            .into_iter()
            .map(|mn| crate::consensus::ValidatorInfo {
                address: mn.masternode.address,
                weight: mn.masternode.tier.sampling_weight(),
            })
            .collect();

        self.consensus.update_validators(validators);
        tracing::info!("🏔️ Avalanche consensus initialized with validators");
    }

    /// Submit a transaction for Avalanche consensus
    pub async fn submit_transaction(&self, tx: Transaction) -> Result<Hash256, AvalancheError> {
        let txid = tx.txid();

        // Add to mempool
        let fee = self.calculate_fee(&tx).await;
        self.tx_pool.add_pending(tx.clone(), fee).map_err(|e| {
            AvalancheError::InvalidTransaction(format!("Failed to add to pool: {}", e))
        })?;

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

        // Initiate Avalanche consensus with Accept preference
        self.consensus.initiate_consensus(txid, Preference::Accept);

        tracing::debug!(
            "📋 Submitted TX {:?} for Avalanche consensus",
            hex::encode(txid)
        );

        // Run consensus to completion
        self.run_consensus_to_completion(txid).await?;

        Ok(txid)
    }

    /// Run Avalanche consensus rounds until transaction is finalized
    async fn run_consensus_to_completion(&self, txid: Hash256) -> Result<(), AvalancheError> {
        let max_rounds = 100;
        let mut round = 0;

        loop {
            // Execute query round
            if let Err(e) = self.consensus.execute_query_round(txid).await {
                tracing::warn!("Query round error: {}", e);
                round += 1;
                if round >= max_rounds {
                    return Err(AvalancheError::ConsensusError(
                        "Avalanche consensus timeout".to_string(),
                    ));
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                continue;
            }

            // Check if finalized
            if self.consensus.is_finalized(&txid) {
                // Check final preference
                if let Some((preference, confidence, _rounds, _finalized)) =
                    self.consensus.get_tx_state(&txid)
                {
                    let event = FinalityEvent {
                        txid,
                        preference,
                        confidence,
                    };

                    let _ = self.finality_tx.send(event);

                    // Apply the result
                    self.apply_finality_result(txid, preference).await?;

                    tracing::info!(
                        "✅ Transaction {:?} finalized with preference: {}",
                        hex::encode(txid),
                        preference
                    );
                }
                return Ok(());
            }

            round += 1;
            if round >= max_rounds {
                return Err(AvalancheError::ConsensusError(
                    "Avalanche consensus timeout".to_string(),
                ));
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    /// Apply the finality result to the transaction pool and UTXO state
    async fn apply_finality_result(
        &self,
        txid: Hash256,
        preference: Preference,
    ) -> Result<(), AvalancheError> {
        match preference {
            Preference::Accept => {
                // Move transaction to finalized pool
                if let Some(tx) = self.tx_pool.finalize_transaction(txid) {
                    // Update inputs to SpentFinalized
                    for input in &tx.inputs {
                        let new_state = UTXOState::SpentFinalized {
                            txid,
                            finalized_at: chrono::Utc::now().timestamp(),
                            votes: 0,
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
                        self.utxo_manager
                            .update_state(&outpoint, UTXOState::Unspent);
                    }

                    tracing::info!(
                        "💾 Transaction {:?} finalized and available for block inclusion",
                        hex::encode(txid)
                    );
                }
                Ok(())
            }
            Preference::Reject => {
                // Remove from pending pool, return inputs to unspent
                if let Some(tx) = self.tx_pool.get_pending(&txid) {
                    for input in &tx.inputs {
                        self.utxo_manager
                            .update_state(&input.previous_output, UTXOState::Unspent);
                    }
                }

                tracing::warn!(
                    "❌ Transaction {:?} rejected by Avalanche",
                    hex::encode(txid)
                );
                Ok(())
            }
        }
    }

    /// Get consensus state for a transaction
    pub fn get_consensus_state(&self, txid: &Hash256) -> Option<(Preference, usize, usize, bool)> {
        self.consensus.get_tx_state(txid)
    }

    /// Check if transaction is finalized
    pub fn is_finalized(&self, txid: &Hash256) -> bool {
        self.consensus.is_finalized(txid)
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

    /// Get metrics
    pub async fn get_metrics(&self) -> AvalancheMetrics {
        AvalancheMetrics {
            inner: self.consensus.get_metrics(),
            pending_transactions: self.tx_pool.pending_count(),
            active_validators: self.masternode_registry.count_active().await,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AvalancheMetrics {
    pub inner: crate::consensus::AvalancheMetrics,
    pub pending_transactions: usize,
    pub active_validators: usize,
}

/// Background task runner for continuous consensus rounds
pub async fn run_avalanche_loop(
    handler: Arc<AvalancheHandler>,
    mut finality_rx: mpsc::UnboundedReceiver<FinalityEvent>,
) {
    let mut round_interval = tokio::time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            // Process finality events
            Some(event) = finality_rx.recv() => {
                tracing::info!(
                    "🏔️ Finality achieved: TX {:?} - {} (confidence: {})",
                    hex::encode(event.txid),
                    event.preference,
                    event.confidence
                );
            }

            // Run consensus rounds periodically for pending transactions
            _ = round_interval.tick() => {
                let pending: Vec<Hash256> = handler.tx_pool
                    .get_all_pending()
                    .into_iter()
                    .map(|tx| tx.txid())
                    .collect();

                for txid in pending {
                    if !handler.is_finalized(&txid) {
                        if let Err(e) = handler.consensus.execute_query_round(txid).await {
                            tracing::debug!("Consensus round error for TX {:?}: {}", hex::encode(txid), e);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;

    fn test_txid(byte: u8) -> Hash256 {
        [byte; 32]
    }

    #[tokio::test]
    #[ignore]
    async fn test_handler_creation() {
        // Placeholder for future tests
    }
}
