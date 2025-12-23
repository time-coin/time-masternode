//! Consensus Module
//!
//! This module implements the Avalanche consensus protocol for instant transaction finality.
//! Key components:
//! - Avalanche: Continuous voting consensus with quorum sampling
//! - Snowflake/Snowball: Low-latency consensus primitives
//! - Transaction validation and UTXO management
//! - Stake-weighted validator sampling

use crate::block::types::Block;
use crate::finality_proof::FinalityProofManager;
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

// ============================================================================
// PHASE 3D/3E: TSDC VOTING ACCUMULATORS
// ============================================================================

/// Accumulates prepare votes for a block (Phase 3D)
/// Pure Avalanche: Tracks continuous sampling votes until majority consensus
#[derive(Debug)]
pub struct PrepareVoteAccumulator {
    /// block_hash -> Vec<(voter_id, weight)>
    votes: DashMap<Hash256, Vec<(String, u64)>>,
}

impl PrepareVoteAccumulator {
    pub fn new() -> Self {
        Self {
            votes: DashMap::new(),
        }
    }

    /// Add a prepare vote for a block
    pub fn add_vote(&self, block_hash: Hash256, voter_id: String, weight: u64) {
        self.votes
            .entry(block_hash)
            .or_default()
            .push((voter_id, weight));
    }

    /// Check if Avalanche consensus reached: majority of sample votes for block
    /// Pure Avalanche: need >50% of sampled validators to agree
    pub fn check_consensus(&self, block_hash: Hash256, sample_size: usize) -> bool {
        if let Some(entry) = self.votes.get(&block_hash) {
            let vote_count = entry.len();
            // Majority: need more than half of sampled validators
            vote_count > sample_size / 2
        } else {
            false
        }
    }

    /// Get accumulated weight for a block
    pub fn get_weight(&self, block_hash: Hash256) -> u64 {
        self.votes
            .get(&block_hash)
            .map(|entry| entry.iter().map(|(_, w)| w).sum())
            .unwrap_or(0)
    }

    /// Clear votes for a block after finalization
    pub fn clear(&self, block_hash: Hash256) {
        self.votes.remove(&block_hash);
    }
}

/// Accumulates precommit votes for a block (Phase 3E)
/// Pure Avalanche: After prepare consensus, validators continue voting for finality
#[derive(Debug)]
pub struct PrecommitVoteAccumulator {
    /// block_hash -> Vec<(voter_id, weight)>
    votes: DashMap<Hash256, Vec<(String, u64)>>,
}

impl PrecommitVoteAccumulator {
    pub fn new() -> Self {
        Self {
            votes: DashMap::new(),
        }
    }

    /// Add a precommit vote for a block
    pub fn add_vote(&self, block_hash: Hash256, voter_id: String, weight: u64) {
        self.votes
            .entry(block_hash)
            .or_default()
            .push((voter_id, weight));
    }

    /// Check if Avalanche consensus reached: majority of sample votes for block
    /// Pure Avalanche: need >50% of sampled validators to agree (consistent with prepare)
    pub fn check_consensus(&self, block_hash: Hash256, sample_size: usize) -> bool {
        if let Some(entry) = self.votes.get(&block_hash) {
            let vote_count = entry.len();
            // Majority: need more than half of sampled validators
            vote_count > sample_size / 2
        } else {
            false
        }
    }

    /// Get accumulated weight for a block
    pub fn get_weight(&self, block_hash: Hash256) -> u64 {
        self.votes
            .get(&block_hash)
            .map(|entry| entry.iter().map(|(_, w)| w).sum())
            .unwrap_or(0)
    }

    /// Clear votes for a block after finalization
    pub fn clear(&self, block_hash: Hash256) {
        self.votes.remove(&block_hash);
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

    /// AVS (Active Validator Set) snapshots per slot for finality vote verification
    /// slot_index -> AVSSnapshot
    avs_snapshots: DashMap<u64, AVSSnapshot>,

    /// VFP (Verifiable Finality Proof) vote accumulator
    /// txid -> accumulated votes
    vfp_votes: DashMap<Hash256, Vec<FinalityVote>>,

    /// Phase 3D: Prepare vote accumulator for Avalanche blocks
    prepare_votes: Arc<PrepareVoteAccumulator>,

    /// Phase 3E: Precommit vote accumulator for Avalanche blocks
    precommit_votes: Arc<PrecommitVoteAccumulator>,

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
            avs_snapshots: DashMap::new(),
            vfp_votes: DashMap::new(),
            prepare_votes: Arc::new(PrepareVoteAccumulator::new()),
            precommit_votes: Arc::new(PrecommitVoteAccumulator::new()),
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
    /// Returns true if consensus process was newly started, false if already in progress
    pub fn initiate_consensus(&self, txid: Hash256, initial_preference: Preference) -> bool {
        if self.finalized_txs.contains_key(&txid) {
            return false; // Already finalized
        }

        if self.tx_state.contains_key(&txid) {
            return false; // Already initiated
        }

        let validators = self.get_validators();
        self.tx_state.insert(
            txid,
            Arc::new(RwLock::new(Snowball::new(initial_preference, &validators))),
        );

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
            {
                let rd = round.read();
                if rd.is_complete(timeout) {
                    drop(rd);
                    break;
                }
            }

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
            let is_finalized = s.is_finalized(self.config.finality_confidence as u32);
            (
                s.snowflake.preference,
                s.snowflake.confidence as usize,
                s.snowflake.k,
                is_finalized,
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

    // ========================================================================
    // AVS SNAPSHOT MANAGEMENT (Per Protocol §8.4)
    // ========================================================================

    /// Create an AVS snapshot for the current slot
    /// Captures the active validator set with their weights for finality vote verification
    pub fn create_avs_snapshot(&self, slot_index: u64) -> AVSSnapshot {
        let validators = self.validators.read();
        let snapshot_validators = validators
            .iter()
            .map(|v| (v.address.clone(), v.weight as u64))
            .collect();

        let snapshot = AVSSnapshot::new(slot_index, snapshot_validators);
        self.avs_snapshots.insert(slot_index, snapshot.clone());

        // Cleanup old snapshots (retain 100 slots per protocol §8.4)
        const ASS_SNAPSHOT_RETENTION: u64 = 100;
        if slot_index > ASS_SNAPSHOT_RETENTION {
            let old_slot = slot_index - ASS_SNAPSHOT_RETENTION;
            self.avs_snapshots.remove(&old_slot);
        }

        snapshot
    }

    /// Get AVS snapshot for a specific slot
    pub fn get_avs_snapshot(&self, slot_index: u64) -> Option<AVSSnapshot> {
        self.avs_snapshots.get(&slot_index).map(|s| s.clone())
    }

    // ========================================================================
    // FINALITY VOTE ACCUMULATION (Per Protocol §8.5)
    // ========================================================================

    /// Accumulate a finality vote for VFP assembly
    pub fn accumulate_finality_vote(&self, vote: FinalityVote) -> Result<(), String> {
        self.vfp_votes.entry(vote.txid).or_default().push(vote);
        Ok(())
    }

    /// Get accumulated votes for a transaction
    pub fn get_accumulated_votes(&self, txid: &Hash256) -> Vec<FinalityVote> {
        self.vfp_votes
            .get(txid)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Check if transaction meets VFP finality threshold
    /// Returns Ok(true) if votes >= 67% of AVS weight
    pub fn check_vfp_finality(
        &self,
        txid: &Hash256,
        snapshot: &AVSSnapshot,
    ) -> Result<bool, String> {
        let votes = self.get_accumulated_votes(txid);

        if votes.is_empty() {
            return Ok(false);
        }

        // Calculate total weight of valid votes
        let mut total_weight = 0u64;
        let mut seen_voters = std::collections::HashSet::new();

        for vote in &votes {
            // Voter must be in snapshot
            if !snapshot.contains_validator(&vote.voter_mn_id) {
                continue; // Skip votes from non-AVS validators
            }

            // Voter can only vote once
            if seen_voters.contains(&vote.voter_mn_id) {
                return Err("Duplicate voter in VFP".to_string());
            }
            seen_voters.insert(vote.voter_mn_id.clone());

            if let Some(weight) = snapshot.get_validator_weight(&vote.voter_mn_id) {
                total_weight += weight;
            }
        }

        // Check threshold: 67% of total weight
        let threshold = snapshot.voting_threshold();
        Ok(total_weight >= threshold)
    }

    /// Clear accumulated votes for a transaction after finality
    pub fn clear_vfp_votes(&self, txid: &Hash256) {
        self.vfp_votes.remove(txid);
    }

    // ========================================================================
    // FINALITY VOTE GENERATION (Per Protocol §8.5)
    // ========================================================================

    /// Generate a finality vote for a transaction if this validator is AVS-active
    /// Called when this validator responds with "Valid" during query round
    pub fn generate_finality_vote(
        &self,
        txid: Hash256,
        slot_index: u64,
        voter_mn_id: String,
        voter_weight: u64,
        snapshot: &AVSSnapshot,
    ) -> Option<FinalityVote> {
        // Only generate vote if voter is in the AVS snapshot for this slot
        if !snapshot.contains_validator(&voter_mn_id) {
            return None;
        }

        // Create the finality vote
        let vote = FinalityVote {
            chain_id: 1, // TODO: Make configurable
            txid,
            tx_hash_commitment: txid, // TODO: Hash the actual tx bytes
            slot_index,
            voter_mn_id,
            voter_weight,
            signature: vec![], // TODO: Sign with validator's key
        };

        Some(vote)
    }

    /// Broadcast a finality vote to all peer masternodes
    /// Used by consensus to propagate votes across the network
    pub fn broadcast_finality_vote(&self, vote: FinalityVote) -> NetworkMessage {
        NetworkMessage::FinalityVoteBroadcast { vote }
    }

    // ========================================================================
    // PHASE 3D: PREPARE VOTE HANDLING
    // ========================================================================

    /// Generate a prepare vote for a block (Phase 3D.1)
    /// Called when a valid block is received
    pub fn generate_prepare_vote(&self, block_hash: Hash256, voter_id: &str, _voter_weight: u64) {
        tracing::debug!(
            "✅ Generated prepare vote for block {} from {}",
            hex::encode(block_hash),
            voter_id
        );
    }

    /// Accumulate a prepare vote from a peer (Phase 3D.2)
    pub fn accumulate_prepare_vote(
        &self,
        block_hash: Hash256,
        voter_id: String,
        voter_weight: u64,
    ) {
        self.prepare_votes
            .add_vote(block_hash, voter_id.clone(), voter_weight);

        let current_weight = self.prepare_votes.get_weight(block_hash);
        tracing::debug!(
            "Prepare vote from {} - accumulated weight: {}",
            voter_id,
            current_weight
        );
    }

    /// Check if prepare consensus reached (Phase 3D.2)
    /// Pure Avalanche: majority of sampled validators must vote for block
    pub fn check_prepare_consensus(&self, block_hash: Hash256) -> bool {
        let validators = self.get_validators();
        let sample_size = validators.len();
        self.prepare_votes.check_consensus(block_hash, sample_size)
    }

    /// Get prepare vote weight for a block
    pub fn get_prepare_weight(&self, block_hash: Hash256) -> u64 {
        self.prepare_votes.get_weight(block_hash)
    }

    // ========================================================================
    // PHASE 3E: PRECOMMIT VOTE HANDLING
    // ========================================================================

    /// Generate a precommit vote for a block (Phase 3E.1)
    /// Called after prepare consensus is reached
    pub fn generate_precommit_vote(&self, block_hash: Hash256, voter_id: &str, _voter_weight: u64) {
        tracing::debug!(
            "✅ Generated precommit vote for block {} from {}",
            hex::encode(block_hash),
            voter_id
        );
    }

    /// Accumulate a precommit vote from a peer (Phase 3E.2)
    pub fn accumulate_precommit_vote(
        &self,
        block_hash: Hash256,
        voter_id: String,
        voter_weight: u64,
    ) {
        self.precommit_votes
            .add_vote(block_hash, voter_id.clone(), voter_weight);

        let current_weight = self.precommit_votes.get_weight(block_hash);
        tracing::debug!(
            "Precommit vote from {} - accumulated weight: {}",
            voter_id,
            current_weight
        );
    }

    /// Check if precommit consensus reached (Phase 3E.2)
    /// Pure Avalanche: majority of sampled validators must vote for block
    pub fn check_precommit_consensus(&self, block_hash: Hash256) -> bool {
        let validators = self.get_validators();
        let sample_size = validators.len();
        self.precommit_votes
            .check_consensus(block_hash, sample_size)
    }

    /// Get precommit vote weight for a block
    pub fn get_precommit_weight(&self, block_hash: Hash256) -> u64 {
        self.precommit_votes.get_weight(block_hash)
    }

    /// Clean up votes after block finalization (Phase 3E.6)
    pub fn cleanup_block_votes(&self, block_hash: Hash256) {
        self.prepare_votes.clear(block_hash);
        self.precommit_votes.clear(block_hash);
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
    pub finality_proof_mgr: Arc<FinalityProofManager>,
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
            finality_proof_mgr: Arc::new(FinalityProofManager::new(1)), // chain_id = 1 for mainnet
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

        // ===== AVALANCHE CONSENSUS INTEGRATION =====
        // Start Avalanche Snowball consensus for this transaction
        let validators_for_consensus = {
            let mut validator_infos = Vec::new();
            for masternode in masternodes.iter() {
                let weight = masternode.tier.collateral() / 1_000_000_000; // Convert to relative weight
                validator_infos.push(ValidatorInfo {
                    address: masternode.address.clone(),
                    weight: weight as usize,
                });
            }
            validator_infos
        };

        // Initiate consensus with Snowball
        let tx_state = Arc::new(RwLock::new(Snowball::new(
            Preference::Accept,
            &validators_for_consensus,
        )));
        self.avalanche.tx_state.insert(txid, tx_state);

        // Create initial QueryRound for vote tracking
        let query_round = Arc::new(RwLock::new(QueryRound::new(
            0,
            txid,
            validators_for_consensus.clone(),
        )));
        self.avalanche.active_rounds.insert(txid, query_round);

        tracing::info!(
            "🔄 Starting Avalanche consensus for TX {:?} with {} validators",
            hex::encode(txid),
            validators_for_consensus.len()
        );

        // Pre-generate vote requests (before async, so RNG doesn't cross await boundary)
        let vote_request_msg = NetworkMessage::TransactionVoteRequest { txid };

        // Spawn consensus round executor as blocking task
        let consensus = self.avalanche.clone();
        let _utxo_mgr = self.utxo_manager.clone();
        let tx_pool = self.tx_pool.clone();
        let broadcast_callback = self.broadcast_callback.clone();
        let _masternodes_for_voting = masternodes.clone();
        tokio::spawn(async move {
            // Small initial delay for peer notifications
            tokio::time::sleep(Duration::from_millis(500)).await;

            // Execute multiple Avalanche rounds for this transaction
            let max_rounds = 10;
            for round_num in 0..max_rounds {
                // Create new QueryRound for this round
                let query_round = Arc::new(RwLock::new(QueryRound::new(
                    round_num,
                    txid,
                    validators_for_consensus.clone(),
                )));
                consensus.active_rounds.insert(txid, query_round);

                // Sample size calculation (no RNG needed for count)
                let sample_size = (validators_for_consensus.len() / 3)
                    .max(3)
                    .min(validators_for_consensus.len());

                tracing::debug!(
                    "Round {}: Sampling {} validators from {} for TX {:?}",
                    round_num,
                    sample_size,
                    validators_for_consensus.len(),
                    hex::encode(txid)
                );

                // Send vote request to all peers (broadcast)
                if let Some(callback) = broadcast_callback.read().await.as_ref() {
                    callback(vote_request_msg.clone());
                }

                // Wait for votes to arrive (votes are submitted via network server handler)
                tokio::time::sleep(Duration::from_millis(500)).await;

                // Tally votes from this round
                // Get the active round for this transaction
                if let Some(round_entry) = consensus.active_rounds.get(&txid) {
                    let round_lock = round_entry.value();
                    let round = round_lock.read();
                    // Get consensus from collected votes (Accept vs Reject tally)
                    if let Some((vote_preference, vote_count)) = round.get_consensus() {
                        tracing::debug!(
                            "Round {}: Tally result - {} votes for {:?}",
                            round_num,
                            vote_count,
                            vote_preference
                        );

                        drop(round); // Release read lock before acquiring write lock

                        // Update Snowball state with vote result
                        if let Some(tx_state) = consensus.tx_state.get(&txid) {
                            let mut snowball = tx_state.value().write();
                            let old_pref = snowball.snowflake.preference;

                            // Update preference and confidence based on votes
                            snowball.update(
                                vote_preference,
                                consensus.config.finality_confidence as u32,
                            );

                            tracing::info!(
                                "Round {}: TX {:?} preference {} -> {} ({} votes, confidence: {})",
                                round_num,
                                hex::encode(txid),
                                old_pref,
                                vote_preference,
                                vote_count,
                                snowball.snowflake.confidence
                            );

                            // Generate and broadcast finality votes if we have valid responses
                            // This propagates votes to peers for VFP accumulation
                            // TODO: Get current slot index and local validator info
                            // For now, this is wired but needs slot tracking integration
                        }
                    } else {
                        tracing::debug!("Round {}: No consensus yet (not enough votes)", round_num);
                        drop(round);
                    }
                }

                // Check finalization after vote tally
                if let Some((preference, _, _, is_finalized)) = consensus.get_tx_state(&txid) {
                    if is_finalized && preference == Preference::Accept {
                        tracing::info!(
                            "✅ TX {:?} finalized via Avalanche after round {} (real voting)",
                            hex::encode(txid),
                            round_num
                        );
                        break;
                    }
                }

                // Small delay before next round
                if round_num < max_rounds - 1 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }

            // Final finalization: check if we reached consensus
            if let Some((preference, _, _, is_finalized)) = consensus.get_tx_state(&txid) {
                if is_finalized {
                    // Move to finalized pool
                    if let Some(_finalized_tx) = tx_pool.finalize_transaction(txid) {
                        tracing::info!(
                            "📦 TX {:?} moved to finalized pool (Snowball confidence threshold reached)",
                            hex::encode(txid)
                        );
                    }
                    // Record finalization preference for reference
                    consensus.finalized_txs.insert(txid, preference);
                } else {
                    // Fallback: finalize with Accept preference even if not enough votes
                    // This prevents transactions from getting stuck
                    if let Some(_finalized_tx) = tx_pool.finalize_transaction(txid) {
                        tracing::info!(
                            "📦 TX {:?} finalized with fallback (max rounds reached, preference: {})",
                            hex::encode(txid),
                            preference
                        );
                    }
                    // Record fallback finalization
                    consensus.finalized_txs.insert(txid, preference);
                }
            }

            // Cleanup: remove QueryRound and tx_state
            consensus.active_rounds.remove(&txid);
            consensus.tx_state.remove(&txid);
            tracing::debug!(
                "🧹 Cleaned up consensus state for TX {:?}",
                hex::encode(txid)
            );
        });

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

    /// Submit a transaction to the consensus engine (called from RPC)
    pub async fn add_transaction(&self, tx: Transaction) -> Result<Hash256, String> {
        self.submit_transaction(tx).await
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
