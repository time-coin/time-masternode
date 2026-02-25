//! TimeVote consensus protocol implementation for TimeCoin
//! Provides instant transaction finality with continuous voting
//!
//! Note: This module provides the complete timevote protocol scaffolding.

#![allow(dead_code)]

use crate::consensus::{Preference, TimeVoteConfig, TimeVoteConsensus};
use crate::masternode_registry::MasternodeRegistry;
use crate::transaction_pool::TransactionPool;
use crate::types::*;
use crate::utxo_manager::UTXOStateManager;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;

/// Errors for TimeVote consensus
#[derive(Error, Debug)]
pub enum TimeVoteError {
    #[error("Consensus error: {0}")]
    ConsensusError(String),

    #[error("UTXO error: {0}")]
    UtxoError(String),

    #[error("Invalid transaction: {0}")]
    InvalidTransaction(String),

    #[error("Network error: {0}")]
    NetworkError(String),
}

/// Bridge between TimeVote consensus and TimeCoin transaction handling
#[allow(dead_code)]
pub struct TimeVoteHandler {
    consensus: Arc<TimeVoteConsensus>,
    utxo_manager: Arc<UTXOStateManager>,
    tx_pool: Arc<TransactionPool>,
    masternode_registry: Arc<MasternodeRegistry>,
    finality_tx: mpsc::UnboundedSender<FinalityEvent>,
}

/// Event indicating a transaction has achieved finality
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FinalityEvent {
    pub txid: Hash256,
    pub preference: Preference,
    pub confidence: usize,
}

impl TimeVoteHandler {
    pub fn new(
        config: TimeVoteConfig,
        utxo_manager: Arc<UTXOStateManager>,
        tx_pool: Arc<TransactionPool>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Result<(Self, mpsc::UnboundedReceiver<FinalityEvent>), TimeVoteError> {
        let consensus = Arc::new(
            TimeVoteConsensus::new(config, masternode_registry.clone())
                .map_err(|e| TimeVoteError::ConsensusError(e.to_string()))?,
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

    /// Initialize timevote with current validators
    pub async fn initialize_validators(&self) {
        // Validators now come from masternode_registry - no need to build/store list
        tracing::info!("🏔️ TimeVote consensus initialized with validators");
    }

    /// Submit a transaction for TimeVote consensus
    pub async fn submit_transaction(&self, tx: Transaction) -> Result<Hash256, TimeVoteError> {
        let txid = tx.txid();

        // Add to mempool
        let fee = self.calculate_fee(&tx).await;
        self.tx_pool.add_pending(tx.clone(), fee).map_err(|e| {
            TimeVoteError::InvalidTransaction(format!("Failed to add to pool: {}", e))
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

        // Initiate TimeVote consensus with Accept preference
        self.consensus.initiate_consensus(txid, Preference::Accept);

        tracing::debug!(
            "📋 Submitted TX {} for TimeVote consensus",
            hex::encode(txid)
        );

        // Run consensus to completion
        self.wait_for_finalization(txid).await?;

        Ok(txid)
    }

    /// Wait for TimeVote finalization (driven by server.rs vote accumulation)
    async fn wait_for_finalization(&self, txid: Hash256) -> Result<(), TimeVoteError> {
        let timeout = Duration::from_secs(10);
        let poll_interval = Duration::from_millis(50);
        let start = std::time::Instant::now();

        loop {
            if self.consensus.is_finalized(&txid) {
                if let Some((preference, _finalized)) = self.consensus.get_tx_state(&txid) {
                    let event = FinalityEvent {
                        txid,
                        preference,
                        confidence: 0,
                    };

                    let _ = self.finality_tx.send(event);
                    self.apply_finality_result(txid, preference).await?;

                    tracing::info!(
                        "✅ Transaction {:?} finalized with preference: {}",
                        hex::encode(txid),
                        preference
                    );
                }
                return Ok(());
            }

            if start.elapsed() >= timeout {
                return Err(TimeVoteError::ConsensusError(
                    "TimeVote finalization timeout".to_string(),
                ));
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Apply the finality result to the transaction pool and UTXO state
    async fn apply_finality_result(
        &self,
        txid: Hash256,
        preference: Preference,
    ) -> Result<(), TimeVoteError> {
        match preference {
            Preference::Accept => {
                // Get TX before finalizing (PoolEntry is private)
                let tx_data = self.tx_pool.get_pending(&txid);

                // Move transaction to finalized pool
                self.tx_pool.finalize_transaction(txid);

                if self.tx_pool.is_finalized(&txid) {
                    // Only proceed if we got the TX data
                    if let Some(tx) = tx_data {
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
                    } // End if let Some(tx) = tx_data
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
                    "❌ Transaction {:?} rejected by timevote",
                    hex::encode(txid)
                );
                Ok(())
            }
        }
    }

    /// Get consensus state for a transaction
    pub fn get_consensus_state(&self, txid: &Hash256) -> Option<(Preference, bool)> {
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
                if let Ok(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                    sum += utxo.value;
                }
            }
            sum
        };
        let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
        input_sum.saturating_sub(output_sum)
    }

    /// Get metrics
    pub async fn get_metrics(&self) -> TimeVoteMetrics {
        TimeVoteMetrics {
            inner: self.consensus.get_metrics(),
            pending_transactions: self.tx_pool.pending_count(),
            active_validators: self.masternode_registry.count_active().await,
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TimeVoteMetrics {
    pub inner: crate::consensus::TimeVoteMetrics,
    pub pending_transactions: usize,
    pub active_validators: usize,
}

/// Background task runner for continuous consensus monitoring
#[allow(dead_code)]
pub async fn run_timevote_loop(
    handler: Arc<TimeVoteHandler>,
    mut finality_rx: mpsc::UnboundedReceiver<FinalityEvent>,
) {
    let mut check_interval = tokio::time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            // Process finality events
            Some(event) = finality_rx.recv() => {
                tracing::info!(
                    "🏔️ Finality achieved: TX {} - {}",
                    hex::encode(event.txid),
                    event.preference
                );
            }

            // Monitor pending transactions for finalization
            _ = check_interval.tick() => {
                let pending: Vec<Hash256> = handler.tx_pool
                    .get_all_pending()
                    .into_iter()
                    .map(|tx| tx.txid())
                    .collect();

                for txid in pending {
                    if handler.is_finalized(&txid) {
                        tracing::debug!("TX {} finalized (detected by timevote loop)", hex::encode(txid));
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
