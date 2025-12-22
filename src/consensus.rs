use crate::block::types::Block;
use crate::network::message::NetworkMessage;
use crate::state_notifier::StateNotifier;
use crate::transaction_pool::TransactionPool;
use crate::types::*;
use crate::utxo_manager::UTXOStateManager;
use arc_swap::ArcSwap;
use dashmap::DashMap;
use ed25519_dalek::Verifier;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::RwLock;

// Resource limits to prevent DOS attacks
const MAX_MEMPOOL_TRANSACTIONS: usize = 10_000;
#[allow(dead_code)] // TODO: Implement byte-size tracking in TransactionPool
const MAX_MEMPOOL_SIZE_BYTES: usize = 300_000_000; // 300MB
const MAX_TX_SIZE: usize = 1_000_000; // 1MB
const MIN_TX_FEE: u64 = 1_000; // 0.00001 TIME minimum fee
const DUST_THRESHOLD: u64 = 546; // Minimum output value (prevents spam)

type BroadcastCallback = Arc<RwLock<Option<Arc<dyn Fn(NetworkMessage) + Send + Sync>>>>;

struct NodeIdentity {
    address: String,
    signing_key: ed25519_dalek::SigningKey,
}

#[allow(dead_code)]
pub struct ConsensusEngine {
    // Lock-free reads using ArcSwap (changes are infrequent)
    masternodes: ArcSwap<Vec<Masternode>>,
    // Set once at startup - use OnceLock
    identity: OnceLock<NodeIdentity>,
    pub utxo_manager: Arc<UTXOStateManager>,
    // Vote storage - clean up votes on finalization
    pub votes: Arc<DashMap<Hash256, Vec<Vote>>>,
    pub tx_pool: Arc<TransactionPool>,
    pub broadcast_callback: BroadcastCallback,
    pub state_notifier: Arc<StateNotifier>,
}

impl ConsensusEngine {
    pub fn new(masternodes: Vec<Masternode>, utxo_manager: Arc<UTXOStateManager>) -> Self {
        Self {
            masternodes: ArcSwap::from_pointee(masternodes),
            identity: OnceLock::new(),
            utxo_manager,
            votes: Arc::new(DashMap::new()),
            tx_pool: Arc::new(TransactionPool::new()),
            broadcast_callback: Arc::new(RwLock::new(None)),
            state_notifier: Arc::new(StateNotifier::new()),
        }
    }

    pub fn set_identity(
        &self,
        address: String,
        signing_key: ed25519_dalek::SigningKey,
    ) -> Result<(), String> {
        self.identity
            .set(NodeIdentity {
                address,
                signing_key,
            })
            .map_err(|_| "Identity already set".to_string())
    }

    pub fn update_masternodes(&self, masternodes: Vec<Masternode>) {
        self.masternodes.store(Arc::new(masternodes));
    }

    // Lock-free read of masternodes
    fn get_masternodes(&self) -> arc_swap::Guard<Arc<Vec<Masternode>>> {
        self.masternodes.load()
    }

    fn is_masternode(&self, address: &str) -> bool {
        self.masternodes
            .load()
            .iter()
            .any(|mn| mn.address == address)
    }

    #[allow(dead_code)]
    pub async fn set_broadcast_callback<F>(&self, callback: F)
    where
        F: Fn(NetworkMessage) + Send + Sync + 'static,
    {
        *self.broadcast_callback.write().await = Some(Arc::new(callback));
    }

    async fn broadcast(&self, msg: NetworkMessage) {
        if let Some(callback) = self.broadcast_callback.read().await.as_ref() {
            callback(msg);
        }
    }

    pub async fn validate_transaction(&self, tx: &Transaction) -> Result<(), String> {
        // 1. Check transaction size limit
        let tx_size = bincode::serialize(tx)
            .map_err(|e| format!("Failed to serialize transaction: {}", e))?
            .len();

        if tx_size > MAX_TX_SIZE {
            return Err(format!(
                "Transaction too large: {} bytes (max {} bytes)",
                tx_size, MAX_TX_SIZE
            ));
        }

        // 2. Check inputs exist and are unspent
        for input in &tx.inputs {
            match self.utxo_manager.get_state(&input.previous_output) {
                Some(UTXOState::Unspent) => {}
                Some(state) => {
                    return Err(format!("UTXO not unspent: {:?}", state));
                }
                None => {
                    return Err("UTXO not found".to_string());
                }
            }
        }

        // 3. Check input values >= output values (no inflation)
        let mut input_sum = 0u64;
        for input in &tx.inputs {
            if let Some(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                input_sum += utxo.value;
            } else {
                return Err("UTXO not found".to_string());
            }
        }

        let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();

        // 4. Dust prevention - reject outputs below threshold
        for output in &tx.outputs {
            if output.value > 0 && output.value < DUST_THRESHOLD {
                return Err(format!(
                    "Dust output detected: {} satoshis (minimum {})",
                    output.value, DUST_THRESHOLD
                ));
            }
        }

        // 5. Calculate and validate fee
        let actual_fee = input_sum.saturating_sub(output_sum);

        // Require minimum absolute fee
        if actual_fee < MIN_TX_FEE {
            return Err(format!(
                "Transaction fee too low: {} satoshis (minimum {})",
                actual_fee, MIN_TX_FEE
            ));
        }

        // Also check proportional fee (0.1% of transaction amount)
        let fee_rate = 1000; // 0.1% = 1/1000
        let min_proportional_fee = output_sum / fee_rate;

        if actual_fee < min_proportional_fee {
            return Err(format!(
                "Insufficient fee: {} satoshis < {} satoshis required (0.1% of {})",
                actual_fee, min_proportional_fee, output_sum
            ));
        }

        if input_sum < output_sum {
            return Err(format!(
                "Insufficient funds: {} < {}",
                input_sum, output_sum
            ));
        }

        // ===== CRITICAL FIX #1: VERIFY SIGNATURES ON ALL INPUTS =====
        for (idx, _input) in tx.inputs.iter().enumerate() {
            self.verify_input_signature(tx, idx).await?;
        }

        tracing::info!(
            "✅ Transaction signatures verified: {} inputs, {} outputs",
            tx.inputs.len(),
            tx.outputs.len()
        );

        Ok(())
    }

    /// Create the message that should be signed for a transaction input
    /// Message format: SHA256(txid || input_index || outputs_hash)
    /// This prevents signature reuse and output tampering attacks
    fn create_signature_message(
        &self,
        tx: &Transaction,
        input_idx: usize,
    ) -> Result<Vec<u8>, String> {
        // Compute transaction hash
        let tx_hash = tx.txid();

        // Create message: txid || input_index || outputs_hash
        let mut message = Vec::new();

        // Add transaction hash (32 bytes)
        message.extend_from_slice(&tx_hash);

        // Add input index (4 bytes, little-endian)
        message.extend_from_slice(&(input_idx as u32).to_le_bytes());

        // Add hash of all outputs (prevents output amount tampering)
        let outputs_bytes = bincode::serialize(&tx.outputs)
            .map_err(|e| format!("Failed to serialize outputs: {}", e))?;
        let outputs_hash = Sha256::digest(&outputs_bytes);
        message.extend_from_slice(&outputs_hash);

        Ok(message)
    }

    /// Verify a single input's cryptographic signature
    /// Uses ed25519 signature scheme for verification
    async fn verify_input_signature(
        &self,
        tx: &Transaction,
        input_idx: usize,
    ) -> Result<(), String> {
        use ed25519_dalek::Signature;

        // Get the input
        let input = tx.inputs.get(input_idx).ok_or("Input index out of range")?;

        // Get the UTXO being spent
        let utxo = self
            .utxo_manager
            .get_utxo(&input.previous_output)
            .await
            .ok_or_else(|| format!("UTXO not found: {:?}", input.previous_output))?;

        // Extract public key from UTXO's script_pubkey
        // In ed25519 setup, script_pubkey IS the 32-byte public key
        if utxo.script_pubkey.len() != 32 {
            return Err(format!(
                "Invalid public key length: {} (expected 32)",
                utxo.script_pubkey.len()
            ));
        }

        let public_key = ed25519_dalek::VerifyingKey::from_bytes(
            &utxo.script_pubkey[0..32]
                .try_into()
                .map_err(|_| "Failed to convert public key bytes")?,
        )
        .map_err(|e| format!("Invalid public key: {}", e))?;

        // Parse signature from script_sig (must be exactly 64 bytes)
        if input.script_sig.len() != 64 {
            return Err(format!(
                "Invalid signature length: {} (expected 64 bytes)",
                input.script_sig.len()
            ));
        }

        let signature = Signature::from_bytes(
            &input.script_sig[0..64]
                .try_into()
                .map_err(|_| "Failed to convert signature bytes")?,
        );

        // Create the message that should have been signed
        let message = self.create_signature_message(tx, input_idx)?;

        // Verify signature
        public_key.verify(&message, &signature).map_err(|_| {
            format!(
                "Signature verification FAILED for input {}: signature doesn't match message",
                input_idx
            )
        })?;

        tracing::debug!("✅ Signature verified for input {}", input_idx);

        Ok(())
    }

    /// Submit a new transaction to the network with lock-based double-spend prevention
    /// This implements the instant finality protocol:
    /// 1. ATOMICALLY lock UTXOs and validate transaction
    /// 2. Broadcast to network
    /// 3. Collect votes from masternodes
    /// 4. Finalize (2/3 quorum) or reject
    #[allow(dead_code)]
    pub async fn lock_and_validate_transaction(&self, tx: &Transaction) -> Result<(), String> {
        let txid = tx.txid();
        let now = chrono::Utc::now().timestamp();

        // CRITICAL: Attempt to lock ALL inputs BEFORE validation
        // This is atomic from the perspective of the consensus engine
        for input in &tx.inputs {
            self.utxo_manager
                .lock_utxo(&input.previous_output, txid)
                .map_err(|e| format!("UTXO double-spend prevented: {}", e))?;
        }

        // Now validate knowing inputs are locked and won't change
        self.validate_transaction(tx).await?;

        // Notify clients of locks
        for input in &tx.inputs {
            let old_state = Some(UTXOState::Unspent);
            let new_state = UTXOState::Locked {
                txid,
                locked_at: now,
            };
            self.state_notifier
                .notify_state_change(input.previous_output.clone(), old_state, new_state)
                .await;

            // Also broadcast lock state to network
            self.broadcast(NetworkMessage::UTXOStateUpdate {
                outpoint: input.previous_output.clone(),
                state: UTXOState::Locked {
                    txid,
                    locked_at: now,
                },
            })
            .await;
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

        // Step 1: Atomically lock and validate
        self.lock_and_validate_transaction(&tx).await?;

        // Calculate fee
        let mut input_sum = 0u64;
        for input in &tx.inputs {
            if let Some(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
                input_sum += utxo.value;
            }
        }
        let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();
        let fee = input_sum.saturating_sub(output_sum);

        // Step 2: Add to pending pool with fee and broadcast
        self.tx_pool.add_pending(tx.clone(), fee).await;
        self.broadcast(NetworkMessage::TransactionBroadcast(tx.clone()))
            .await;

        // Step 3: Process transaction through consensus
        self.process_transaction(tx).await?;

        Ok(txid)
    }

    pub async fn process_transaction(&self, tx: Transaction) -> Result<(), String> {
        let txid = tx.txid();
        let masternodes = self.get_masternodes();
        let n = masternodes.len() as u32;

        if n == 0 {
            return Err("No masternodes available".to_string());
        }

        // Validate locally first
        self.validate_transaction(&tx).await?;

        // Update UTXO states to SpentPending
        let now = chrono::Utc::now().timestamp();
        for input in &tx.inputs {
            let old_state = self.utxo_manager.get_state(&input.previous_output);
            let new_state = UTXOState::SpentPending {
                txid,
                votes: 0,
                total_nodes: n,
                spent_at: now,
            };
            self.utxo_manager
                .update_state(&input.previous_output, new_state.clone());

            // Notify clients of state change
            self.state_notifier
                .notify_state_change(input.previous_output.clone(), old_state, new_state.clone())
                .await;

            // Broadcast state update
            self.broadcast(NetworkMessage::UTXOStateUpdate {
                outpoint: input.previous_output.clone(),
                state: new_state,
            })
            .await;
        }

        // Add to pending pool first
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
        let fee = input_sum.saturating_sub(output_sum);

        // Check mempool limits before adding
        let pending_count = self.tx_pool.get_all_pending().await.len();
        if pending_count >= MAX_MEMPOOL_TRANSACTIONS {
            return Err(format!(
                "Mempool full: {} transactions (max {})",
                pending_count, MAX_MEMPOOL_TRANSACTIONS
            ));
        }

        // Calculate approximate transaction size for future mempool byte tracking
        let _tx_size = bincode::serialize(&tx)
            .map_err(|e| format!("Serialization error: {}", e))?
            .len();
        // TODO: Track actual mempool byte size in TransactionPool

        self.tx_pool.add_pending(tx.clone(), fee).await;

        // If we are a masternode, automatically vote
        if let Some(identity) = self.identity.get() {
            if self.is_masternode(&identity.address) {
                tracing::debug!(
                    "📝 Auto-voting on transaction {} as masternode {}",
                    hex::encode(txid),
                    identity.address
                );
                match self
                    .create_and_broadcast_vote(txid, true, &identity.signing_key)
                    .await
                {
                    Ok(_) => tracing::debug!("✅ Vote sent for {}", hex::encode(txid)),
                    Err(e) => tracing::warn!("Failed to vote: {}", e),
                }
            }
        }

        // NOTE: Actual finalization happens in check_and_finalize_transaction()
        // which is called when votes arrive via handle_transaction_vote()

        Ok(())
    }

    async fn is_masternode_check(&self, address: &str) -> bool {
        self.masternodes
            .load()
            .iter()
            .any(|mn| mn.address == address)
    }

    async fn create_and_broadcast_vote(
        &self,
        txid: Hash256,
        approve: bool,
        signing_key: &ed25519_dalek::SigningKey,
    ) -> Result<(), String> {
        use ed25519_dalek::Signer;

        let identity = self.identity.get().ok_or("Identity not set")?;
        let timestamp = chrono::Utc::now().timestamp();

        // Create vote message to sign
        let mut vote_data = Vec::new();
        vote_data.extend_from_slice(&txid);
        vote_data.extend_from_slice(identity.address.as_bytes());
        vote_data.push(if approve { 1 } else { 0 });
        vote_data.extend_from_slice(&timestamp.to_le_bytes());

        let signature = signing_key.sign(&vote_data);

        let vote = Vote {
            txid,
            voter: identity.address.clone(),
            approve,
            timestamp,
            signature,
        };

        // Broadcast vote
        self.broadcast(NetworkMessage::TransactionVote(vote)).await;

        Ok(())
    }

    /// Handle incoming vote from network
    pub async fn handle_transaction_vote(&self, vote: Vote) -> Result<(), String> {
        use ed25519_dalek::Verifier;

        let txid = vote.txid;

        // Verify voter is a masternode (no need to lock for read)
        let masternodes = self.get_masternodes();
        let masternode = masternodes
            .iter()
            .find(|mn| mn.address == vote.voter)
            .ok_or("Vote from non-masternode")?
            .clone();
        drop(masternodes);

        // Verify signature
        let mut vote_data = Vec::new();
        vote_data.extend_from_slice(&vote.txid);
        vote_data.extend_from_slice(vote.voter.as_bytes());
        vote_data.push(if vote.approve { 1 } else { 0 });
        vote_data.extend_from_slice(&vote.timestamp.to_le_bytes());

        masternode
            .public_key
            .verify(&vote_data, &vote.signature)
            .map_err(|_| "Invalid vote signature")?;

        // Store vote using DashMap (lock-free)
        let mut tx_votes = self.votes.entry(txid).or_default();

        // Check for duplicate vote from same masternode
        if tx_votes.iter().any(|v| v.voter == vote.voter) {
            return Err("Duplicate vote from same masternode".to_string());
        }

        tx_votes.push(vote.clone());
        let vote_count = tx_votes.len();
        let approval_count = tx_votes.iter().filter(|v| v.approve).count() as u32;
        let total_masternodes = self.get_masternodes().len();

        tracing::info!(
            "📊 Transaction {} has {}/{} votes ({} approvals)",
            hex::encode(txid),
            vote_count,
            total_masternodes,
            approval_count
        );

        drop(tx_votes); // Release DashMap entry reference

        // Check if we've reached quorum
        self.check_and_finalize_transaction(txid).await?;

        Ok(())
    }

    /// Check if transaction has enough votes to finalize
    async fn check_and_finalize_transaction(&self, txid: Hash256) -> Result<(), String> {
        let tx_votes = self.votes.get(&txid);

        if tx_votes.is_none() {
            return Ok(()); // No votes yet
        }

        let votes_ref = tx_votes.unwrap();
        let vote_vec = votes_ref.value();
        let n = self.get_masternodes().len() as u32;
        let quorum = (2 * n).div_ceil(3);
        let approval_count = vote_vec.iter().filter(|v| v.approve).count() as u32;
        let rejection_count = vote_vec.iter().filter(|v| !v.approve).count() as u32;

        drop(votes_ref); // Release DashMap entry reference

        // Check if we have quorum for approval
        if approval_count >= quorum {
            tracing::info!(
                "✅ Transaction {} reached approval quorum: {}/{} votes",
                hex::encode(txid),
                approval_count,
                n
            );
            self.finalize_transaction_approved(txid, approval_count)
                .await?;

            // Clean up votes for this transaction
            self.votes.remove(&txid);

            return Ok(());
        }

        // Check if rejection is certain (more than 1/3 rejections means quorum impossible)
        if rejection_count > n - quorum {
            tracing::warn!(
                "❌ Transaction {} rejected: {}/{} rejections",
                hex::encode(txid),
                rejection_count,
                n
            );
            self.finalize_transaction_rejected(txid, rejection_count)
                .await?;

            // Clean up votes for this transaction
            self.votes.remove(&txid);

            return Ok(());
        }

        // Still waiting for more votes
        Ok(())
    }

    async fn finalize_transaction_approved(&self, txid: Hash256, votes: u32) -> Result<(), String> {
        // Get the transaction from pending pool (optimized - no full clone)
        let tx = self
            .tx_pool
            .get_pending(&txid)
            .await
            .ok_or("Transaction not in pending pool")?;

        let now = chrono::Utc::now().timestamp();

        // Mark inputs as SpentFinalized with real-time notification
        for input in &tx.inputs {
            let old_state = self.utxo_manager.get_state(&input.previous_output);
            let new_state = UTXOState::SpentFinalized {
                txid,
                finalized_at: now,
                votes,
            };
            self.utxo_manager
                .update_state(&input.previous_output, new_state.clone());

            // 🔥 NOTIFY clients of instant finality!
            self.state_notifier
                .notify_state_change(input.previous_output.clone(), old_state, new_state.clone())
                .await;

            // Broadcast finalized state
            self.broadcast(NetworkMessage::UTXOStateUpdate {
                outpoint: input.previous_output.clone(),
                state: new_state,
            })
            .await;
        }

        // Create new UTXOs with notifications
        for (i, output) in tx.outputs.iter().enumerate() {
            let new_outpoint = OutPoint {
                txid,
                vout: i as u32,
            };
            let address = String::from_utf8_lossy(&output.script_pubkey).to_string();
            let utxo = UTXO {
                outpoint: new_outpoint.clone(),
                value: output.value,
                script_pubkey: output.script_pubkey.clone(),
                address,
            };

            self.utxo_manager.add_utxo(utxo.clone()).await;

            // Notify clients of new UTXO creation (finalized!)
            self.state_notifier
                .notify_state_change(new_outpoint.clone(), None, UTXOState::Unspent)
                .await;

            // Broadcast new UTXO state
            self.broadcast(NetworkMessage::UTXOStateUpdate {
                outpoint: new_outpoint,
                state: UTXOState::Unspent,
            })
            .await;
        }

        // Move to finalized pool
        self.tx_pool.finalize_transaction(txid).await;

        // Broadcast finalization
        self.broadcast(NetworkMessage::TransactionFinalized { txid, votes })
            .await;

        tracing::info!(
            "⚡ INSTANT FINALITY: Transaction {} finalized with {} votes",
            hex::encode(txid),
            votes
        );

        Ok(())
    }

    async fn finalize_transaction_rejected(
        &self,
        txid: Hash256,
        _votes: u32,
    ) -> Result<(), String> {
        // Get the transaction to unlock its UTXOs (optimized - no full clone)
        if let Some(tx) = self.tx_pool.get_pending(&txid).await {
            // Unlock UTXOs with notifications
            for input in &tx.inputs {
                let old_state = self.utxo_manager.get_state(&input.previous_output);
                let new_state = UTXOState::Unspent;

                self.utxo_manager
                    .update_state(&input.previous_output, new_state.clone());

                // Notify clients of unlock
                self.state_notifier
                    .notify_state_change(input.previous_output.clone(), old_state, new_state)
                    .await;

                // Broadcast unlock
                self.broadcast(NetworkMessage::UTXOStateUpdate {
                    outpoint: input.previous_output.clone(),
                    state: UTXOState::Unspent,
                })
                .await;
            }
        }

        let reason = "Failed to reach approval quorum".to_string();
        self.tx_pool.reject_transaction(txid, reason.clone()).await;

        // Broadcast rejection
        self.broadcast(NetworkMessage::TransactionRejected { txid, reason })
            .await;

        tracing::warn!(
            "❌ Transaction {} rejected - failed to reach quorum",
            hex::encode(txid)
        );

        Ok(())
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
    pub fn handle_utxo_state_update(&self, outpoint: OutPoint, state: UTXOState) {
        self.utxo_manager.update_state(&outpoint, state);
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

    #[allow(dead_code)]
    pub async fn get_active_masternodes(&self) -> Vec<Masternode> {
        self.get_masternodes().iter().cloned().collect()
    }

    #[allow(dead_code)]
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
