use crate::block::types::Block;
use crate::network::message::NetworkMessage;
use crate::transaction_pool::TransactionPool;
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
    pub tx_pool: Arc<TransactionPool>,
    pub broadcast_callback: Option<Arc<dyn Fn(NetworkMessage) + Send + Sync>>,
}

impl ConsensusEngine {
    pub fn new(masternodes: Vec<Masternode>, utxo_manager: Arc<UTXOStateManager>) -> Self {
        Self {
            masternodes,
            utxo_manager,
            votes: Arc::new(RwLock::new(HashMap::new())),
            tx_pool: Arc::new(TransactionPool::new()),
            broadcast_callback: None,
        }
    }

    #[allow(dead_code)]
    pub fn set_broadcast_callback<F>(&mut self, callback: F)
    where
        F: Fn(NetworkMessage) + Send + Sync + 'static,
    {
        self.broadcast_callback = Some(Arc::new(callback));
    }

    fn broadcast(&self, msg: NetworkMessage) {
        if let Some(callback) = &self.broadcast_callback {
            callback(msg);
        }
    }

    pub async fn validate_transaction(&self, tx: &Transaction) -> Result<(), String> {
        // Check inputs exist and are unspent
        for input in &tx.inputs {
            match self.utxo_manager.get_state(&input.previous_output).await {
                Some(UTXOState::Unspent) => {}
                Some(state) => {
                    return Err(format!("UTXO not unspent: {:?}", state));
                }
                None => {
                    return Err("UTXO not found".to_string());
                }
            }
        }

        // Check input values >= output values (no inflation)
        let mut input_sum = 0u64;
        for input in &tx.inputs {
            if let Some(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                input_sum += utxo.value;
            } else {
                return Err("UTXO not found".to_string());
            }
        }

        let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
        if input_sum < output_sum {
            return Err(format!(
                "Insufficient funds: {} < {}",
                input_sum, output_sum
            ));
        }

        Ok(())
    }

    /// Submit a new transaction to the network
    /// This implements the instant finality protocol:
    /// 1. Validate transaction
    /// 2. Lock UTXOs
    /// 3. Broadcast to network
    /// 4. Collect votes from masternodes
    /// 5. Finalize (2/3 quorum) or reject
    #[allow(dead_code)]
    pub async fn submit_transaction(&self, tx: Transaction) -> Result<Hash256, String> {
        let txid = tx.txid();

        // Step 1: Validate transaction
        self.validate_transaction(&tx).await?;

        // Step 2: Lock UTXOs
        for input in &tx.inputs {
            self.utxo_manager
                .lock_utxo(&input.previous_output, txid)
                .await
                .map_err(|e| format!("Failed to lock UTXO: {}", e))?;

            // Broadcast UTXO lock state
            self.broadcast(NetworkMessage::UTXOStateUpdate {
                outpoint: input.previous_output.clone(),
                state: UTXOState::Locked {
                    txid,
                    locked_at: chrono::Utc::now().timestamp(),
                },
            });
        }

        // Step 3: Add to pending pool and broadcast
        self.tx_pool.add_pending(tx.clone()).await;
        self.broadcast(NetworkMessage::TransactionBroadcast(tx.clone()));

        // Step 4: Process transaction through consensus
        self.process_transaction(tx).await?;

        Ok(txid)
    }

    pub async fn process_transaction(&self, tx: Transaction) -> Result<(), String> {
        let txid = tx.txid();
        let n = self.masternodes.len() as u32;

        if n == 0 {
            return Err("No masternodes available".to_string());
        }

        // Update UTXO states to SpentPending
        for input in &tx.inputs {
            let state = UTXOState::SpentPending {
                txid,
                votes: 0,
                total_nodes: n,
                spent_at: chrono::Utc::now().timestamp(),
            };
            self.utxo_manager
                .update_state(&input.previous_output, state.clone())
                .await;

            // Broadcast state update
            self.broadcast(NetworkMessage::UTXOStateUpdate {
                outpoint: input.previous_output.clone(),
                state,
            });
        }

        // Collect votes (simplified - in real impl, this is async network operation)
        let mut approvals = 0u32;
        for _mn in &self.masternodes {
            if self.validate_transaction(&tx).await.is_ok() {
                approvals += 1;
            }
        }

        let quorum = (2 * n).div_ceil(3);

        if approvals >= quorum {
            // Transaction approved - finalize
            for input in &tx.inputs {
                let state = UTXOState::SpentFinalized {
                    txid,
                    finalized_at: chrono::Utc::now().timestamp(),
                    votes: approvals,
                };
                self.utxo_manager
                    .update_state(&input.previous_output, state.clone())
                    .await;

                // Broadcast finalized state
                self.broadcast(NetworkMessage::UTXOStateUpdate {
                    outpoint: input.previous_output.clone(),
                    state,
                });
            }

            // Create new UTXOs and broadcast
            for (i, output) in tx.outputs.iter().enumerate() {
                let new_outpoint = OutPoint {
                    txid,
                    vout: i as u32,
                };
                let utxo = UTXO {
                    outpoint: new_outpoint.clone(),
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    address: "recipient".to_string(), // TODO: derive from script_pubkey
                };

                self.utxo_manager.add_utxo(utxo.clone()).await;

                // Broadcast new UTXO state
                self.broadcast(NetworkMessage::UTXOStateUpdate {
                    outpoint: new_outpoint,
                    state: UTXOState::Unspent,
                });
            }

            // Move to finalized pool
            self.tx_pool.finalize_transaction(txid).await;

            // Broadcast finalization
            self.broadcast(NetworkMessage::TransactionFinalized {
                txid,
                votes: approvals,
            });

            tracing::info!(
                "✅ Transaction {} finalized with {} votes",
                hex::encode(txid),
                approvals
            );
            Ok(())
        } else {
            // Transaction rejected - unlock UTXOs
            for input in &tx.inputs {
                self.utxo_manager
                    .update_state(&input.previous_output, UTXOState::Unspent)
                    .await;

                // Broadcast unlock
                self.broadcast(NetworkMessage::UTXOStateUpdate {
                    outpoint: input.previous_output.clone(),
                    state: UTXOState::Unspent,
                });
            }

            let reason = format!(
                "Failed to reach consensus: {}/{} votes (need {})",
                approvals, n, quorum
            );
            self.tx_pool.reject_transaction(txid, reason.clone()).await;

            // Broadcast rejection
            self.broadcast(NetworkMessage::TransactionRejected {
                txid,
                reason: reason.clone(),
            });

            Err(reason)
        }
    }

    /// Handle incoming transaction from network
    #[allow(dead_code)]
    pub async fn handle_network_transaction(&self, tx: Transaction) -> Result<(), String> {
        let txid = tx.txid();

        // Skip if already processed
        if self.tx_pool.is_pending(&txid).await || self.tx_pool.is_finalized(&txid).await {
            return Ok(());
        }

        // Process it
        self.submit_transaction(tx).await.map(|_| ())
    }

    /// Handle incoming UTXO state update from network
    #[allow(dead_code)]
    pub async fn handle_utxo_state_update(&self, outpoint: OutPoint, state: UTXOState) {
        self.utxo_manager.update_state(&outpoint, state).await;
    }

    pub async fn get_finalized_transactions_for_block(&self) -> Vec<Transaction> {
        self.tx_pool.get_finalized_transactions().await
    }

    #[allow(dead_code)]
    pub async fn clear_finalized_transactions(&self) {
        self.tx_pool.clear_finalized().await;
    }

    #[allow(dead_code)]
    pub async fn get_mempool_info(&self) -> (usize, usize) {
        let pending = self.tx_pool.pending_count().await;
        let finalized = self.tx_pool.finalized_count().await;
        (pending, finalized)
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

    #[allow(dead_code)]
    pub async fn generate_deterministic_block_with_eligible(
        &self,
        height: u64,
        _timestamp: i64,
        eligible: Vec<(Masternode, String)>,
    ) -> Block {
        use crate::block::generator::DeterministicBlockGenerator;

        let finalized_txs = self.get_finalized_transactions_for_block().await;
        let previous_hash = [0u8; 32];
        let base_reward = 100;

        // Convert to format expected by generator
        let masternodes: Vec<Masternode> = eligible.iter().map(|(mn, _addr)| mn.clone()).collect();

        DeterministicBlockGenerator::generate(
            height,
            previous_hash,
            finalized_txs,
            masternodes,
            base_reward,
        )
    }

    #[allow(dead_code)]
    pub async fn generate_deterministic_block_with_masternodes(
        &self,
        height: u64,
        _timestamp: i64,
        masternodes: Vec<Masternode>,
    ) -> Block {
        use crate::block::generator::DeterministicBlockGenerator;

        let finalized_txs = self.get_finalized_transactions_for_block().await;
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
