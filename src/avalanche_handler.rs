use crate::consensus::{AvalancheConfig, AvalancheConsensus, Preference};
use crate::masternode_registry::MasternodeRegistry;
use crate::transaction_pool::TransactionPool;
use crate::types::*;
use crate::utxo_manager::UTXOStateManager;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;

/// Errors for Avalanche transaction handler
#[derive(Error, Debug)]
pub enum AvalancheHandlerError {
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
pub struct AvalancheTransactionHandler {
    consensus: Arc<AvalancheConsensus>,
    utxo_manager: Arc<UTXOStateManager>,
    tx_pool: Arc<TransactionPool>,
    masternode_registry: Arc<MasternodeRegistry>,

    /// Channel for broadcasting consensus results
    finality_tx: mpsc::UnboundedSender<FinalityEvent>,
}

/// Event indicating a transaction has achieved finality
#[derive(Debug, Clone)]
pub struct FinalityEvent {
    pub txid: Hash256,
    pub preference: Preference,
    pub confidence: usize,
}

impl AvalancheTransactionHandler {
    pub fn new(
        config: AvalancheConfig,
        utxo_manager: Arc<UTXOStateManager>,
        tx_pool: Arc<TransactionPool>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<FinalityEvent>), AvalancheHandlerError> {
        let consensus = Arc::new(
            AvalancheConsensus::new(config)
                .map_err(|e| AvalancheHandlerError::ConsensusError(e.to_string()))?,
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

    /// Submit a pending transaction for consensus
    pub async fn submit_for_consensus(&self, txid: Hash256) -> Result<(), AvalancheHandlerError> {
        // Verify transaction exists in pool
        if !self.tx_pool.is_pending(&txid) {
            return Err(AvalancheHandlerError::InvalidTransaction(
                "Transaction not in pending pool".to_string(),
            ));
        }

        // Initiate consensus with Accept preference
        // (we believe the transaction is valid, pending validation votes)
        self.consensus.initiate_consensus(txid, Preference::Accept);

        tracing::debug!(
            "📋 Submitted TX {:?} for Avalanche consensus",
            hex::encode(txid)
        );
        Ok(())
    }

    /// Process a vote from a validator node
    pub fn record_validator_vote(
        &self,
        txid: Hash256,
        validator_address: &str,
        accepts: bool,
    ) -> Result<(), AvalancheHandlerError> {
        let preference = if accepts {
            Preference::Accept
        } else {
            Preference::Reject
        };

        self.consensus
            .submit_vote(txid, validator_address.to_string(), preference);

        tracing::debug!(
            "🗳️ Vote from {} for TX {:?}: {}",
            validator_address,
            hex::encode(txid),
            preference
        );

        Ok(())
    }

    /// Run a single consensus round for a transaction
    pub async fn run_consensus_round(&self, txid: Hash256) -> Result<(), AvalancheHandlerError> {
        self.consensus
            .execute_query_round(txid)
            .await
            .map_err(|e| AvalancheHandlerError::ConsensusError(e.to_string()))?;

        // Check for finality
        if let Some((preference, confidence, _rounds, finalized)) =
            self.consensus.get_tx_state(&txid)
        {
            if finalized {
                let event = FinalityEvent {
                    txid,
                    preference,
                    confidence,
                };

                let _ = self.finality_tx.send(event.clone());

                // Apply the result
                self.apply_finality_result(txid, preference).await?;

                tracing::info!(
                    "✅ Transaction {:?} finalized with preference: {}",
                    hex::encode(txid),
                    preference
                );
            }
        }

        Ok(())
    }

    /// Run full consensus to completion for a transaction
    pub async fn run_full_consensus(
        &self,
        txid: Hash256,
    ) -> Result<Preference, AvalancheHandlerError> {
        let preference = self
            .consensus
            .run_consensus(txid)
            .await
            .map_err(|e| AvalancheHandlerError::ConsensusError(e.to_string()))?;

        self.apply_finality_result(txid, preference).await?;

        Ok(preference)
    }

    /// Apply the finality result to the transaction pool and UTXO state
    async fn apply_finality_result(
        &self,
        txid: Hash256,
        preference: Preference,
    ) -> Result<(), AvalancheHandlerError> {
        match preference {
            Preference::Accept => {
                // Move transaction to finalized pool
                if let Some(_tx) = self.tx_pool.finalize_transaction(txid) {
                    tracing::info!("✅ Finalizing transaction {:?}", hex::encode(txid));

                    // TODO: Commit UTXO spends to blockchain
                    // This would involve updating the UTXO set and adding to blockchain
                }
                Ok(())
            }
            Preference::Reject => {
                // Remove from pending pool, mark as rejected
                tracing::warn!("❌ Rejecting transaction {:?}", hex::encode(txid));

                // Unlock any locked UTXOs
                // TODO: Call unlock_utxo for all inputs

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
pub async fn run_avalanche_consensus_loop(
    handler: Arc<AvalancheTransactionHandler>,
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

            // Run consensus rounds periodically
            _ = round_interval.tick() => {
                // Get all pending transactions
                let pending: Vec<Hash256> = handler.tx_pool
                    .get_all_pending()
                    .into_iter()
                    .map(|tx| tx.txid())
                    .collect();

                // Run a round for each pending transaction
                for txid in pending {
                    if !handler.is_finalized(&txid) {
                        if let Err(e) = handler.run_consensus_round(txid).await {
                            tracing::warn!("Consensus round failed for TX {:?}: {}", hex::encode(txid), e);
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
        let config = AvalancheConfig::default();
        let utxo_manager = Arc::new(UTXOStateManager::new());
        let tx_pool = Arc::new(TransactionPool::new());
        let _masternode_registry = Arc::new(MasternodeRegistry::new(
            Arc::new(sled::open("/tmp/test_db").unwrap()),
            crate::network_type::NetworkType::Testnet,
        ));

        // These tests are currently incomplete and ignored
    }

    #[tokio::test]
    #[ignore]
    async fn test_validator_initialization() {
        let config = AvalancheConfig::default();
        let utxo_manager = Arc::new(UTXOStateManager::new());
        let tx_pool = Arc::new(TransactionPool::new());
        let _masternode_registry = Arc::new(MasternodeRegistry::new(
            Arc::new(sled::open("/tmp/test_db").unwrap()),
            crate::network_type::NetworkType::Testnet,
        ));

        // These tests are currently incomplete and ignored
    }

    #[tokio::test]
    #[ignore]
    async fn test_submit_for_consensus() {
        let config = AvalancheConfig::default();
        let utxo_manager = Arc::new(UTXOStateManager::new());
        let tx_pool = Arc::new(TransactionPool::new());
        let _masternode_registry = Arc::new(MasternodeRegistry::new(
            Arc::new(sled::open("/tmp/test_db").unwrap()),
            crate::network_type::NetworkType::Testnet,
        ));

        // These tests are currently incomplete and ignored
    }
}
