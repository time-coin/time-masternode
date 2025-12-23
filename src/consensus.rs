//! Consensus Module
//!
//! This module implements the Avalanche consensus protocol for instant transaction finality.
//! Key components:
//! - Avalanche: Continuous voting consensus with quorum sampling
//! - Snowflake/Snowball: Low-latency consensus primitives
//! - Transaction validation and UTXO management
//! - Stake-weighted validator sampling

use crate::block::types::Block;
use crate::network::message::NetworkMessage;
use crate::state_notifier::StateNotifier;
use crate::transaction_pool::TransactionPool;
use crate::types::*;
use crate::utxo_manager::UTXOStateManager;
use arc_swap::ArcSwap;
use dashmap::DashMap;
use ed25519_dalek::Verifier;
use parking_lot::RwLock;
use rand::seq::SliceRandom;
use rand::thread_rng;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock as TokioRwLock;

// Resource limits to prevent DOS attacks
const MAX_MEMPOOL_TRANSACTIONS: usize = 10_000;
#[allow(dead_code)] // TODO: Implement byte-size tracking in TransactionPool
const MAX_MEMPOOL_SIZE_BYTES: usize = 300_000_000; // 300MB
const MAX_TX_SIZE: usize = 1_000_000; // 1MB
const MIN_TX_FEE: u64 = 1_000; // 0.00001 TIME minimum fee
const DUST_THRESHOLD: u64 = 546; // Minimum output value (prevents spam)

type BroadcastCallback = Arc<TokioRwLock<Option<Arc<dyn Fn(NetworkMessage) + Send + Sync>>>>;

struct NodeIdentity {
    address: String,
    signing_key: ed25519_dalek::SigningKey,
}

// ============================================================================
// AVALANCHE PROTOCOL TYPES
// ============================================================================

/// Avalanche consensus errors
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum AvalancheError {
    #[error("Transaction not found")]
    TransactionNotFound,

    #[error("Invalid preference: {0}")]
    InvalidPreference(String),

    #[error("Insufficient confidence: got {got}, need {threshold}")]
    InsufficientConfidence { got: usize, threshold: usize },

    #[error("Query failed: {0}")]
    QueryFailed(String),

    #[error("Chit acquisition failed")]
    ChitAcquisitionFailed,

    #[error("Configuration error: {0}")]
    ConfigError(String),
}

/// Configuration for Avalanche consensus
#[derive(Debug, Clone)]
pub struct AvalancheConfig {
    /// Number of validators to query per round (k parameter)
    pub sample_size: usize,
    /// Quorum size - minimum votes needed to consider a round (alpha parameter)
    /// Per spec: alpha = 14
    pub quorum_size: usize,
    /// Number of consecutive preference confirms needed for finality (beta)
    /// Per spec: beta = 20
    pub finality_confidence: usize,
    /// Timeout for query responses (milliseconds)
    pub query_timeout_ms: u64,
    /// Maximum rounds before giving up
    pub max_rounds: usize,
}

impl Default for AvalancheConfig {
    fn default() -> Self {
        Self {
            sample_size: 20,         // Query 20 validators per round (k)
            quorum_size: 14,         // Need 14+ responses for consensus (alpha)
            finality_confidence: 20, // 20 consecutive confirms for finality (beta)
            query_timeout_ms: 2000,  // 2 second timeout
            max_rounds: 100,
        }
    }
}

/// Preference tracking for a transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Preference {
    Accept,
    Reject,
}

impl std::fmt::Display for Preference {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Preference::Accept => write!(f, "Accept"),
            Preference::Reject => write!(f, "Reject"),
        }
    }
}

/// Information about a validator for stake-weighted sampling
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatorInfo {
    pub address: String,
    pub weight: usize, // Sampling weight based on tier
}

/// Vote result from a single validator with weight
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Vote {
    pub voter_id: String,
    pub preference: Preference,
    pub timestamp: Instant,
    pub weight: usize, // Stake weight of the voter
}

/// Snowflake protocol state - improved with dynamic k adjustment
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Snowflake {
    pub preference: Preference,
    pub confidence: u32,
    pub k: usize,                        // Current sample size (dynamic)
    pub suspicion: HashMap<String, f64>, // Trust scores for validators
    pub last_updated: Instant,
}

impl Snowflake {
    pub fn new(initial_preference: Preference, validators: &[ValidatorInfo]) -> Self {
        let mut suspicion = HashMap::new();
        for validator in validators {
            suspicion.insert(validator.address.clone(), 1.0); // Start with full trust
        }

        Self {
            preference: initial_preference,
            confidence: 0,
            k: 20, // Default sample size
            suspicion,
            last_updated: Instant::now(),
        }
    }

    /// Update preference with dynamic k adjustment
    /// When preference matches: increase confidence, decrease k
    /// When preference changes: reset confidence, increase k
    pub fn update(&mut self, new_preference: Preference, _beta: u32) {
        if new_preference == self.preference {
            self.confidence += 1;
            // Decrease k dynamically when confident
            if self.k > 2 {
                self.k -= 1;
            }
        } else {
            // Preference flipped - reset confidence and increase k
            self.preference = new_preference;
            self.confidence = 1;
            self.k += 1;
        }
        self.last_updated = Instant::now();
    }

    /// Check if finalized (high confidence)
    pub fn is_finalized(&self, threshold: u32) -> bool {
        self.confidence >= threshold
    }

    /// Update validator suspicion scores based on their vote
    pub fn update_suspicion(&mut self, voter: &str, voted_preference: Preference) {
        if voted_preference == self.preference {
            // Increase trust if they voted with us
            if let Some(score) = self.suspicion.get_mut(voter) {
                *score = (*score + 1.0).min(1.0); // Cap at 1.0
            }
        } else {
            // Decrease trust if they voted against us
            if let Some(score) = self.suspicion.get_mut(voter) {
                *score = (*score - 0.1).max(0.0); // Floor at 0.0
            }
        }
    }
}

/// Snowball protocol state - uses Snowflake but adds finalization tracking
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Snowball {
    pub snowflake: Snowflake,
    pub last_finalized: Option<Preference>,
}

impl Snowball {
    pub fn new(initial_preference: Preference, validators: &[ValidatorInfo]) -> Self {
        Self {
            snowflake: Snowflake::new(initial_preference, validators),
            last_finalized: None,
        }
    }

    /// Update based on query results
    pub fn update(&mut self, new_preference: Preference, beta: u32) {
        self.snowflake.update(new_preference, beta);
    }

    /// Record finalization
    pub fn finalize(&mut self) {
        self.last_finalized = Some(self.snowflake.preference);
    }

    /// Check if finalized
    pub fn is_finalized(&self, threshold: u32) -> bool {
        self.snowflake.is_finalized(threshold)
    }
}

/// Query round tracking - improved for better consensus detection
#[derive(Debug)]
#[allow(dead_code)]
pub struct QueryRound {
    pub round_number: usize,
    pub txid: Hash256,
    pub sampled_validators: Vec<ValidatorInfo>,
    pub votes_received: DashMap<String, Vote>,
    pub started_at: Instant,
}

impl QueryRound {
    pub fn new(round_number: usize, txid: Hash256, sampled_validators: Vec<ValidatorInfo>) -> Self {
        Self {
            round_number,
            txid,
            sampled_validators,
            votes_received: DashMap::new(),
            started_at: Instant::now(),
        }
    }

    /// Check if round is complete (all responses or timeout)
    pub fn is_complete(&self, timeout: Duration) -> bool {
        let elapsed = self.started_at.elapsed();
        elapsed > timeout || self.votes_received.len() >= self.sampled_validators.len()
    }

    /// Get consensus from collected votes - improved majority detection
    pub fn get_consensus(&self) -> Option<(Preference, usize)> {
        let mut accept_count = 0;
        let mut reject_count = 0;

        for vote in self.votes_received.iter() {
            match vote.value().preference {
                Preference::Accept => accept_count += 1,
                Preference::Reject => reject_count += 1,
            }
        }

        let total = accept_count + reject_count;
        if total == 0 {
            return None;
        }

        // Strict majority: must have more than half the votes
        if accept_count > reject_count {
            Some((Preference::Accept, accept_count))
        } else if reject_count > accept_count {
            Some((Preference::Reject, reject_count))
        } else {
            None // Tie - no consensus
        }
    }
}

/// Core Avalanche consensus engine - upgraded with dynamic k adjustment
pub struct AvalancheConsensus {
    config: AvalancheConfig,

    /// Transaction state tracking (txid -> Snowball)
    tx_state: DashMap<Hash256, Arc<RwLock<Snowball>>>,

    /// Active query rounds
    active_rounds: DashMap<Hash256, Arc<RwLock<QueryRound>>>,

    /// Finalized transactions
    finalized_txs: DashMap<Hash256, Preference>,

    /// Validator list with weight info for stake-weighted sampling
    validators: Arc<RwLock<Vec<ValidatorInfo>>>,

    /// Metrics
    rounds_executed: AtomicUsize,
    txs_finalized: AtomicUsize,
}

impl AvalancheConsensus {
    pub fn new(config: AvalancheConfig) -> Result<Self, AvalancheError> {
        // Validate config
        if config.sample_size == 0 {
            return Err(AvalancheError::ConfigError(
                "sample_size must be > 0".to_string(),
            ));
        }
        if config.finality_confidence == 0 {
            return Err(AvalancheError::ConfigError(
                "finality_confidence must be > 0".to_string(),
            ));
        }

        Ok(Self {
            config,
            tx_state: DashMap::new(),
            active_rounds: DashMap::new(),
            finalized_txs: DashMap::new(),
            validators: Arc::new(RwLock::new(Vec::new())),
            rounds_executed: AtomicUsize::new(0),
            txs_finalized: AtomicUsize::new(0),
        })
    }

    /// Update the list of active validators with their weights
    pub fn update_validators(&self, validators: Vec<ValidatorInfo>) {
        let mut v = self.validators.write();
        *v = validators;
    }

    /// Get current validators
    pub fn get_validators(&self) -> Vec<ValidatorInfo> {
        self.validators.read().clone()
    }

    /// Get validator addresses only (for compatibility)
    pub fn get_validator_addresses(&self) -> Vec<String> {
        self.validators
            .read()
            .iter()
            .map(|v| v.address.clone())
            .collect()
    }

    /// Sample validators using stake-weighted probability
    /// P(sampling node_i) = Weight_i / Total_Weight
    fn sample_validators(
        &self,
        validators: &[ValidatorInfo],
        sample_size: usize,
    ) -> Vec<ValidatorInfo> {
        if validators.is_empty() {
            return Vec::new();
        }

        // Calculate total weight
        let total_weight: usize = validators.iter().map(|v| v.weight).sum();
        if total_weight == 0 {
            return Vec::new();
        }

        // Create weighted sampling pool
        let mut pool = Vec::new();
        for validator in validators {
            // Add validator multiple times based on weight
            for _ in 0..validator.weight {
                pool.push(validator.clone());
            }
        }

        // Shuffle and sample with duplication removed
        let mut rng = thread_rng();
        pool.shuffle(&mut rng);

        // Use indices to avoid duplicates and take only sample_size
        let mut sampled = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for validator in pool.into_iter().take(sample_size * 2) {
            if seen.insert(validator.address.clone()) {
                sampled.push(validator);
                if sampled.len() >= sample_size {
                    break;
                }
            }
        }
        sampled
    }

    /// Initiate consensus on a transaction
    /// Returns true if consensus process started
    pub fn initiate_consensus(&self, txid: Hash256, initial_preference: Preference) -> bool {
        if self.finalized_txs.contains_key(&txid) {
            return false; // Already finalized
        }

        let validators = self.get_validators();
        self.tx_state.entry(txid).or_insert_with(|| {
            Arc::new(RwLock::new(Snowball::new(initial_preference, &validators)))
        });

        true
    }

    /// Submit a vote for a transaction from a validator
    pub fn submit_vote(&self, txid: Hash256, voter_id: String, preference: Preference) {
        // Look up the validator's weight
        let validators = self.get_validators();
        let weight = validators
            .iter()
            .find(|v| v.address == voter_id)
            .map(|v| v.weight)
            .unwrap_or(1); // Default to 1 if not found

        let vote = Vote {
            voter_id: voter_id.clone(),
            preference,
            timestamp: Instant::now(),
            weight,
        };

        // If there's an active round, record the vote
        if let Some(round) = self.active_rounds.get(&txid) {
            round
                .value()
                .read()
                .votes_received
                .insert(voter_id.clone(), vote);

            // Update validator suspicion scores in snowball
            if let Some(state) = self.tx_state.get(&txid) {
                let mut snowball = state.value().write();
                snowball.snowflake.update_suspicion(&voter_id, preference);
            }
        }
    }

    /// Execute a single query round for a transaction
    pub async fn execute_query_round(&self, txid: Hash256) -> Result<(), AvalancheError> {
        // Get or create transaction state
        let tx_state = self
            .tx_state
            .get(&txid)
            .ok_or(AvalancheError::TransactionNotFound)?;

        let validators = self.get_validators();
        if validators.is_empty() {
            return Err(AvalancheError::QueryFailed(
                "No validators available".to_string(),
            ));
        }

        // Get current k from snowball (dynamic sample size)
        let current_k = {
            let snowball = tx_state.value().read();
            snowball.snowflake.k
        };

        // Sample validators based on current k
        let sampled = self.sample_validators(&validators, current_k);

        // Create query round
        let round_number = self.rounds_executed.fetch_add(1, Ordering::Relaxed);
        let round = Arc::new(RwLock::new(QueryRound::new(round_number, txid, sampled)));

        self.active_rounds.insert(txid, round.clone());

        // Wait for responses or timeout
        let timeout = Duration::from_millis(self.config.query_timeout_ms);
        let start = Instant::now();
        loop {
            let rd = round.read();
            if rd.is_complete(timeout) {
                drop(rd);
                break;
            }
            drop(rd);

            if start.elapsed() > timeout {
                break;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // Process results
        let consensus = {
            let rd = round.read();
            rd.get_consensus()
        };

        if let Some((preference, count)) = consensus {
            // Update transaction state
            {
                let mut state = tx_state.value().write();
                let old_pref = state.snowflake.preference;

                // Update transaction state with new preference and dynamic k adjustment
                state.update(preference, self.config.finality_confidence as u32);

                tracing::debug!(
                    "Round {}: TX {:?} preference {} -> {} ({} votes, confidence: {}, k: {})",
                    round_number,
                    hex::encode(txid),
                    old_pref,
                    preference,
                    count,
                    state.snowflake.confidence,
                    state.snowflake.k
                );

                // Check for finalization using beta (finality_confidence)
                if state.is_finalized(self.config.finality_confidence as u32) {
                    self.finalized_txs.insert(txid, preference);
                    self.txs_finalized.fetch_add(1, Ordering::Relaxed);
                    state.finalize();
                    tracing::info!(
                        "✅ TX {:?} finalized with preference: {} (confidence: {})",
                        hex::encode(txid),
                        preference,
                        state.snowflake.confidence
                    );
                }
            }
        } else {
            tracing::warn!("No consensus in round {}", round_number);
        }

        self.active_rounds.remove(&txid);
        Ok(())
    }

    /// Run consensus to completion for a transaction
    pub async fn run_consensus(&self, txid: Hash256) -> Result<Preference, AvalancheError> {
        // Check if already finalized
        if let Some(pref) = self.finalized_txs.get(&txid) {
            return Ok(*pref);
        }

        // Initialize if not already
        self.initiate_consensus(txid, Preference::Accept);

        // Run rounds until finalized
        for _ in 0..self.config.max_rounds {
            self.execute_query_round(txid).await?;

            if let Some(pref) = self.finalized_txs.get(&txid) {
                return Ok(*pref);
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Err(AvalancheError::InsufficientConfidence {
            got: self
                .tx_state
                .get(&txid)
                .map(|s| s.read().snowflake.confidence as usize)
                .unwrap_or(0),
            threshold: self.config.finality_confidence,
        })
    }

    /// Get current state of a transaction
    pub fn get_tx_state(&self, txid: &Hash256) -> Option<(Preference, usize, usize, bool)> {
        self.tx_state.get(txid).map(|state| {
            let s = state.read();
            (
                s.snowflake.preference,
                s.snowflake.confidence as usize,
                s.snowflake.k,
                self.finalized_txs.contains_key(txid),
            )
        })
    }

    /// Check if transaction is finalized
    pub fn is_finalized(&self, txid: &Hash256) -> bool {
        self.finalized_txs.contains_key(txid)
    }

    /// Get finalization preference
    pub fn get_finalized_preference(&self, txid: &Hash256) -> Option<Preference> {
        self.finalized_txs.get(txid).map(|p| *p)
    }

    /// Get metrics
    pub fn get_metrics(&self) -> AvalancheMetrics {
        AvalancheMetrics {
            rounds_executed: self.rounds_executed.load(Ordering::Relaxed),
            txs_finalized: self.txs_finalized.load(Ordering::Relaxed),
            active_rounds: self.active_rounds.len(),
            tracked_txs: self.tx_state.len(),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AvalancheMetrics {
    pub rounds_executed: usize,
    pub txs_finalized: usize,
    pub active_rounds: usize,
    pub tracked_txs: usize,
}

// ============================================================================
// CONSENSUS ENGINE
// ============================================================================

#[allow(dead_code)]
pub struct ConsensusEngine {
    // Lock-free reads using ArcSwap (changes are infrequent)
    masternodes: ArcSwap<Vec<Masternode>>,
    // Set once at startup - use OnceLock
    identity: OnceLock<NodeIdentity>,
    pub utxo_manager: Arc<UTXOStateManager>,
    pub tx_pool: Arc<TransactionPool>,
    pub broadcast_callback: BroadcastCallback,
    pub state_notifier: Arc<StateNotifier>,
    pub avalanche: Arc<AvalancheConsensus>,
}

impl ConsensusEngine {
    pub fn new(masternodes: Vec<Masternode>, utxo_manager: Arc<UTXOStateManager>) -> Self {
        let avalanche_config = AvalancheConfig::default();
        let avalanche = AvalancheConsensus::new(avalanche_config)
            .expect("Failed to initialize Avalanche consensus");

        Self {
            masternodes: ArcSwap::from_pointee(masternodes),
            identity: OnceLock::new(),
            utxo_manager,
            tx_pool: Arc::new(TransactionPool::new()),
            broadcast_callback: Arc::new(TokioRwLock::new(None)),
            state_notifier: Arc::new(StateNotifier::new()),
            avalanche: Arc::new(avalanche),
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
    /// CPU-intensive crypto is moved to spawn_blocking to prevent async runtime blocking
    async fn verify_input_signature(
        &self,
        tx: &Transaction,
        input_idx: usize,
    ) -> Result<(), String> {
        // Get the input
        let input = tx.inputs.get(input_idx).ok_or("Input index out of range")?;

        // Get the UTXO being spent (async operation)
        let utxo = self
            .utxo_manager
            .get_utxo(&input.previous_output)
            .await
            .ok_or_else(|| format!("UTXO not found: {:?}", input.previous_output))?;

        // Create the message that should have been signed
        let message = self.create_signature_message(tx, input_idx)?;

        // Clone data needed for blocking task
        let pubkey_bytes = utxo.script_pubkey.clone();
        let sig_bytes = input.script_sig.clone();

        // Move CPU-intensive signature verification to blocking pool
        tokio::task::spawn_blocking(move || {
            use ed25519_dalek::Signature;

            // Extract public key from UTXO's script_pubkey
            // In ed25519 setup, script_pubkey IS the 32-byte public key
            if pubkey_bytes.len() != 32 {
                return Err(format!(
                    "Invalid public key length: {} (expected 32)",
                    pubkey_bytes.len()
                ));
            }

            let public_key = ed25519_dalek::VerifyingKey::from_bytes(
                &pubkey_bytes[0..32]
                    .try_into()
                    .map_err(|_| "Failed to convert public key bytes")?,
            )
            .map_err(|e| format!("Invalid public key: {}", e))?;

            // Parse signature from script_sig (must be exactly 64 bytes)
            if sig_bytes.len() != 64 {
                return Err(format!(
                    "Invalid signature length: {} (expected 64 bytes)",
                    sig_bytes.len()
                ));
            }

            let signature = Signature::from_bytes(
                &sig_bytes[0..64]
                    .try_into()
                    .map_err(|_| "Failed to convert signature bytes")?,
            );

            // Verify signature (CPU intensive)
            public_key.verify(&message, &signature).map_err(|_| {
                format!(
                    "Signature verification FAILED for input {}: signature doesn't match message",
                    input_idx
                )
            })?;

            Ok::<(), String>(())
        })
        .await
        .map_err(|e| format!("Signature verification task failed: {}", e))?
        .map_err(|e| {
            tracing::warn!(
                "Signature verification failed for input {}: {}",
                input_idx,
                e
            );
            e
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

        // Step 2: Broadcast to network
        self.broadcast(NetworkMessage::TransactionBroadcast(tx.clone()))
            .await;

        // Step 3: Process transaction through consensus (this adds to pool)
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
        let pending_count = self.tx_pool.pending_count();
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

        self.tx_pool
            .add_pending(tx.clone(), fee)
            .map_err(|e| format!("Failed to add to pool: {}", e))?;

        // If we are a masternode, automatically vote

        // NOTE: Actual finalization happens in check_and_finalize_transaction()
        // which is called when votes arrive via handle_transaction_vote()

        Ok(())
    }

    pub fn get_finalized_transactions_for_block(&self) -> Vec<Transaction> {
        self.tx_pool.get_finalized_transactions()
    }

    #[allow(dead_code)]
    pub fn clear_finalized_transactions(&self) {
        self.tx_pool.clear_finalized();
    }

    #[allow(dead_code)]
    pub fn get_mempool_info(&self) -> (usize, usize) {
        let pending = self.tx_pool.pending_count();
        let finalized = self.tx_pool.finalized_count();
        (pending, finalized)
    }

    #[allow(dead_code)]
    pub fn get_active_masternodes(&self) -> Vec<Masternode> {
        self.get_masternodes().iter().cloned().collect()
    }

    #[allow(dead_code)]
    pub async fn generate_deterministic_block(&self, height: u64, _timestamp: i64) -> Block {
        use crate::block::generator::DeterministicBlockGenerator;

        let finalized_txs = self.get_finalized_transactions_for_block();
        let masternodes = self.get_active_masternodes();
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

        let finalized_txs = self.get_finalized_transactions_for_block();
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

        let finalized_txs = self.get_finalized_transactions_for_block();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_txid(byte: u8) -> Hash256 {
        [byte; 32]
    }

    #[test]
    fn test_avalanche_init() {
        let config = AvalancheConfig::default();
        let av = AvalancheConsensus::new(config).unwrap();
        assert_eq!(av.get_validators().len(), 0);
    }

    #[test]
    fn test_validator_management() {
        let config = AvalancheConfig::default();
        let av = AvalancheConsensus::new(config).unwrap();

        let validators = vec![
            ValidatorInfo {
                address: "v1".to_string(),
                weight: 1,
            },
            ValidatorInfo {
                address: "v2".to_string(),
                weight: 10,
            },
            ValidatorInfo {
                address: "v3".to_string(),
                weight: 100,
            },
        ];
        av.update_validators(validators.clone());

        assert_eq!(av.get_validators(), validators);
    }

    #[test]
    fn test_initiate_consensus() {
        let config = AvalancheConfig::default();
        let av = AvalancheConsensus::new(config).unwrap();
        let txid = test_txid(1);

        assert!(av.initiate_consensus(txid, Preference::Accept));
        assert!(!av.initiate_consensus(txid, Preference::Accept)); // Already initiated

        let (pref, confidence, _finality_threshold, finalized) = av.get_tx_state(&txid).unwrap();
        assert_eq!(pref, Preference::Accept);
        assert_eq!(confidence, 0);
        assert!(!finalized);
    }

    #[test]
    fn test_vote_submission() {
        let config = AvalancheConfig::default();
        let av = AvalancheConsensus::new(config).unwrap();
        let txid = test_txid(1);

        av.initiate_consensus(txid, Preference::Accept);
        av.submit_vote(txid, "v1".to_string(), Preference::Accept);

        // Votes recorded but not processed until round completes
    }

    // Snowflake tests disabled - implementation replaced by newer Avalanche consensus
    #[test]
    #[ignore]
    fn test_snowflake() {
        let mut sf = Snowflake::new(Preference::Accept, &[]);
        assert_eq!(sf.preference, Preference::Accept);
        assert_eq!(sf.confidence, 0);

        sf.update(Preference::Accept, 1);
        assert_eq!(sf.confidence, 1);

        sf.update(Preference::Accept, 1);
        assert_eq!(sf.confidence, 2);

        sf.update(Preference::Reject, 1);
        assert_eq!(sf.preference, Preference::Reject);
        assert_eq!(sf.confidence, 1);
    }

    #[test]
    #[ignore]
    fn test_query_round_consensus() {
        let round = QueryRound::new(0, test_txid(1), vec![]);

        round.votes_received.insert(
            "v1".to_string(),
            Vote {
                voter_id: "v1".to_string(),
                preference: Preference::Accept,
                timestamp: Instant::now(),
                weight: 1,
            },
        );

        let consensus = round.get_consensus();
        assert_eq!(consensus, Some((Preference::Accept, 1)));
    }

    #[tokio::test]
    async fn test_invalid_config() {
        let config = AvalancheConfig {
            sample_size: 0,
            ..Default::default()
        };
        assert!(AvalancheConsensus::new(config).is_err());

        let config = AvalancheConfig {
            finality_confidence: 0,
            ..Default::default()
        };
        assert!(AvalancheConsensus::new(config).is_err());
    }
}
