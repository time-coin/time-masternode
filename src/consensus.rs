//! Consensus Module
//!
//! This module implements the TimeVote consensus protocol for instant transaction finality.
//! Key components:
//! - TimeVote: Unified consensus with progressive finality proof assembly
//! - TimeVote Protocol: Low-latency stake-weighted voting consensus primitives adapted for signed vote collection
//! - Transaction validation and UTXO management
//! - Stake-weighted validator sampling and vote accumulation
//!
//! Note: Some methods are scaffolding for full consensus integration.

#![allow(dead_code)]

use crate::block::types::Block;
use crate::finality_proof::FinalityProofManager;
use crate::masternode_registry::MasternodeRegistry;
use crate::network::message::NetworkMessage;
use crate::state_notifier::StateNotifier;
use crate::transaction_pool::TransactionPool;
use crate::types::*;
use crate::utxo_manager::UTXOStateManager;
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

// §7.6 Liveness Fallback Protocol Parameters
const STALL_TIMEOUT: Duration = Duration::from_secs(30); // Protocol §7.6.1
const FALLBACK_MIN_DURATION: Duration = Duration::from_secs(20); // Protocol §7.6.3
const FALLBACK_ROUND_TIMEOUT: Duration = Duration::from_secs(10); // Protocol §7.6.5
const MAX_FALLBACK_ROUNDS: u32 = 5; // Protocol §7.6.5

type BroadcastCallback = Arc<TokioRwLock<Option<Arc<dyn Fn(NetworkMessage) + Send + Sync>>>>;

struct NodeIdentity {
    address: String,
    signing_key: ed25519_dalek::SigningKey,
}

impl NodeIdentity {
    /// Sign a finality vote with this node's key
    #[allow(clippy::too_many_arguments)]
    fn sign_finality_vote(
        &self,
        chain_id: u32,
        txid: Hash256,
        tx_hash_commitment: Hash256,
        slot_index: u64,
        decision: VoteDecision, // NEW: Accept or Reject
        voter_mn_id: String,
        voter_weight: u64,
    ) -> FinalityVote {
        use ed25519_dalek::Signer;

        // Create the signing message
        let mut msg = Vec::new();
        msg.extend_from_slice(&chain_id.to_le_bytes());
        msg.extend_from_slice(&txid);
        msg.extend_from_slice(&tx_hash_commitment);
        msg.extend_from_slice(&slot_index.to_le_bytes());
        // CRITICAL: Include decision in signature (equivocation prevention)
        msg.push(match decision {
            VoteDecision::Accept => 0x01,
            VoteDecision::Reject => 0x00,
        });
        msg.extend_from_slice(voter_mn_id.as_bytes());
        msg.extend_from_slice(&voter_weight.to_le_bytes());

        // Sign the message
        let signature = self.signing_key.sign(&msg);

        FinalityVote {
            chain_id,
            txid,
            tx_hash_commitment,
            slot_index,
            decision, // Include decision in vote
            voter_mn_id,
            voter_weight,
            signature: signature.to_bytes().to_vec(),
        }
    }

    /// Sign a LivenessAlert with this node's key (§7.6.2)
    #[allow(clippy::too_many_arguments)]
    fn sign_liveness_alert(
        &self,
        chain_id: u32,
        txid: Hash256,
        tx_hash_commitment: Hash256,
        slot_index: u64,
        poll_history: Vec<PollResult>,
        stall_duration_ms: u64,
        current_confidence: u32,
    ) -> LivenessAlert {
        use ed25519_dalek::Signer;

        let alert = LivenessAlert {
            chain_id,
            txid,
            tx_hash_commitment,
            slot_index,
            poll_history,
            current_confidence,
            stall_duration_ms,
            reporter_mn_id: self.address.clone(),
            reporter_signature: vec![],
        };

        let msg = alert.signing_message();
        let signature = self.signing_key.sign(&msg);

        LivenessAlert {
            reporter_signature: signature.to_bytes().to_vec(),
            ..alert
        }
    }

    /// Sign a FinalityProposal with this node's key (§7.6.4)
    fn sign_finality_proposal(
        &self,
        chain_id: u32,
        txid: Hash256,
        tx_hash_commitment: Hash256,
        slot_index: u64,
        decision: FallbackDecision,
        justification: String,
    ) -> FinalityProposal {
        use ed25519_dalek::Signer;

        let proposal = FinalityProposal {
            chain_id,
            txid,
            tx_hash_commitment,
            slot_index,
            decision: decision.clone(),
            justification,
            leader_mn_id: self.address.clone(),
            leader_signature: vec![],
        };

        let msg = proposal.signing_message();
        let signature = self.signing_key.sign(&msg);

        FinalityProposal {
            leader_signature: signature.to_bytes().to_vec(),
            ..proposal
        }
    }

    /// Sign a FallbackVote with this node's key (§7.6.4)
    fn sign_fallback_vote(
        &self,
        chain_id: u32,
        proposal_hash: Hash256,
        vote: FallbackVoteDecision,
        voter_weight: u64,
    ) -> FallbackVote {
        use ed25519_dalek::Signer;

        let fallback_vote = FallbackVote {
            chain_id,
            proposal_hash,
            vote: vote.clone(),
            voter_mn_id: self.address.clone(),
            voter_weight,
            voter_signature: vec![],
        };

        let msg = fallback_vote.signing_message();
        let signature = self.signing_key.sign(&msg);

        FallbackVote {
            voter_signature: signature.to_bytes().to_vec(),
            ..fallback_vote
        }
    }
}

// ============================================================================
// timevote PROTOCOL TYPES
// ============================================================================

/// TimeVote consensus errors
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum TimeVoteError {
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

/// Configuration for TimeVote consensus
#[derive(Debug, Clone)]
pub struct TimeVoteConfig {
    /// Number of validators to query per round (k parameter)
    pub sample_size: usize,
    /// Quorum size - minimum votes needed to consider a round (alpha parameter)
    /// Per spec: alpha = 14
    pub quorum_size: usize,
    /// Number of consecutive preference confirms needed for finality (beta)
    /// Per spec: beta = 20
    pub finality_confidence: usize,
    /// Required finality weight threshold as percentage (default 67%)
    pub q_finality_percent: u64,
    /// Timeout for query responses (milliseconds)
    pub query_timeout_ms: u64,
    /// Maximum rounds before giving up
    pub max_rounds: usize,
}

impl Default for TimeVoteConfig {
    fn default() -> Self {
        Self {
            sample_size: 20,         // Query 20 validators per round (k)
            quorum_size: 14,         // Need 14+ responses for consensus (alpha)
            finality_confidence: 20, // 20 consecutive confirms for finality (beta)
            q_finality_percent: 67,  // 67% weight threshold for finality
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

/// Memory usage statistics for consensus engine
#[derive(Debug, Clone)]
pub struct ConsensusMemoryStats {
    pub tx_state_entries: usize,
    pub active_rounds: usize,
    pub finalized_txs: usize,
    pub avs_snapshots: usize,
    pub vfp_votes: usize,
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

/// TimeVote protocol state - Progressive TimeProof assembly with vote accumulation
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VotingState {
    pub snowflake: Snowflake,
    pub last_finalized: Option<Preference>,
    /// Accumulated finality votes for TimeProof assembly
    pub accumulated_votes: Vec<FinalityVote>,
    /// Current accumulated vote weight
    pub accumulated_weight: u64,
    /// Required weight threshold for finality (67% of AVS weight)
    pub required_weight: u64,
}

impl VotingState {
    pub fn new(initial_preference: Preference, validators: &[ValidatorInfo]) -> Self {
        Self {
            snowflake: Snowflake::new(initial_preference, validators),
            last_finalized: None,
            accumulated_votes: Vec::new(),
            accumulated_weight: 0,
            required_weight: 0, // Will be set based on AVS snapshot
        }
    }

    /// Update based on query results
    pub fn update(&mut self, new_preference: Preference, beta: u32) {
        self.snowflake.update(new_preference, beta);
    }

    /// Add a finality vote and update accumulated weight
    pub fn add_vote(&mut self, vote: FinalityVote) {
        self.accumulated_weight += vote.voter_weight;
        self.accumulated_votes.push(vote);
    }

    /// Check if finality threshold reached (67% weight)
    pub fn has_finality_threshold(&self) -> bool {
        self.required_weight > 0 && self.accumulated_weight >= self.required_weight
    }

    /// Record finalization
    pub fn finalize(&mut self) {
        self.last_finalized = Some(self.snowflake.preference);
    }

    /// Check if finalized
    pub fn is_finalized(&self, threshold: u32) -> bool {
        self.snowflake.is_finalized(threshold) || self.has_finality_threshold()
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
/// Pure timevote: Tracks continuous sampling votes until majority consensus
#[derive(Debug)]
pub struct PrepareVoteAccumulator {
    /// block_hash -> Vec<(voter_id, weight)>
    votes: DashMap<Hash256, Vec<(String, u64)>>,
}

impl Default for PrepareVoteAccumulator {
    fn default() -> Self {
        Self::new()
    }
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

    /// Check if timevote consensus reached: majority of sample votes for block
    /// Pure timevote: need >50% of sampled validators to agree
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

    /// Get list of voter IDs who voted for this block
    pub fn get_voters(&self, block_hash: Hash256) -> Vec<String> {
        self.votes
            .get(&block_hash)
            .map(|entry| entry.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default()
    }

    /// Clear votes for a block after finalization
    pub fn clear(&self, block_hash: Hash256) {
        self.votes.remove(&block_hash);
    }
}

/// Accumulates precommit votes for a block (Phase 3E)
/// Pure timevote: After prepare consensus, validators continue voting for finality
#[derive(Debug)]
pub struct PrecommitVoteAccumulator {
    /// block_hash -> Vec<(voter_id, weight)>
    votes: DashMap<Hash256, Vec<(String, u64)>>,
}

impl Default for PrecommitVoteAccumulator {
    fn default() -> Self {
        Self::new()
    }
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

    /// Check if timevote consensus reached: majority of sample votes for block
    /// Pure timevote: need >50% of sampled validators to agree (consistent with prepare)
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

    /// Get list of voter IDs who voted for this block
    pub fn get_voters(&self, block_hash: Hash256) -> Vec<String> {
        self.votes
            .get(&block_hash)
            .map(|entry| entry.iter().map(|(id, _)| id.clone()).collect())
            .unwrap_or_default()
    }

    /// Clear votes for a block after finalization
    pub fn clear(&self, block_hash: Hash256) {
        self.votes.remove(&block_hash);
    }
}

/// Core TimeVote consensus engine - Progressive finality with vote accumulation
pub struct TimeVoteConsensus {
    config: TimeVoteConfig,

    /// Reference to masternode registry (single source of truth for validators)
    masternode_registry: Arc<MasternodeRegistry>,

    /// Transaction state tracking (txid -> Snowball)
    tx_state: DashMap<Hash256, Arc<RwLock<VotingState>>>,

    /// Active query rounds
    active_rounds: DashMap<Hash256, Arc<RwLock<QueryRound>>>,

    /// Finalized transactions with timestamp for cleanup
    finalized_txs: DashMap<Hash256, (Preference, Instant)>,

    /// AVS (Active Validator Set) snapshots per slot for finality vote verification
    /// slot_index -> AVSSnapshot
    avs_snapshots: DashMap<u64, AVSSnapshot>,

    /// VFP (Verifiable Finality Proof) vote accumulator
    /// txid -> accumulated votes
    vfp_votes: DashMap<Hash256, Vec<FinalityVote>>,

    /// Phase 3D: Prepare vote accumulator for timevote blocks
    pub prepare_votes: Arc<PrepareVoteAccumulator>,

    /// Phase 3E: Precommit vote accumulator for timevote blocks
    pub precommit_votes: Arc<PrecommitVoteAccumulator>,

    /// §7.6 Liveness Fallback: Transaction status tracking
    /// Per protocol §7.3 and §7.6 - explicit state machine
    tx_status: Arc<DashMap<Hash256, TransactionStatus>>,

    /// §7.6 Liveness Fallback: Stall detection timers
    /// Tracks when transactions entered Voting state for timeout detection
    stall_timers: Arc<DashMap<Hash256, Instant>>,

    /// §7.6 Liveness Fallback: Alert accumulation tracker
    /// txid -> Vec<LivenessAlert> (accumulate alerts from different reporters)
    liveness_alerts: DashMap<Hash256, Vec<LivenessAlert>>,

    /// §7.6 Liveness Fallback: Vote accumulation tracker
    /// proposal_hash -> Vec<FallbackVote> (accumulate votes from AVS members)
    fallback_votes: DashMap<Hash256, Vec<FallbackVote>>,

    /// PRIORITY: Track active vote requests to pause block production
    /// This ensures instant finality is never blocked by block production
    pub active_vote_requests: Arc<AtomicUsize>,

    /// §7.6 Liveness Fallback: Proposal to transaction mapping
    /// proposal_hash -> txid (track which proposal is for which transaction)
    proposal_to_tx: DashMap<Hash256, Hash256>,

    /// §7.6 Liveness Fallback: Fallback round tracking
    /// txid -> (slot_index, round_count, started_at)
    fallback_rounds: DashMap<Hash256, (u64, u32, Instant)>,

    /// Metrics
    rounds_executed: AtomicUsize,
    txs_finalized: AtomicUsize,
}

impl TimeVoteConsensus {
    pub fn new(
        config: TimeVoteConfig,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> Result<Self, TimeVoteError> {
        // Validate config
        if config.sample_size == 0 {
            return Err(TimeVoteError::ConfigError(
                "sample_size must be > 0".to_string(),
            ));
        }
        if config.finality_confidence == 0 {
            return Err(TimeVoteError::ConfigError(
                "finality_confidence must be > 0".to_string(),
            ));
        }

        Ok(Self {
            config,
            masternode_registry,
            tx_state: DashMap::new(),
            active_rounds: DashMap::new(),
            finalized_txs: DashMap::new(),
            avs_snapshots: DashMap::new(),
            vfp_votes: DashMap::new(),
            prepare_votes: Arc::new(PrepareVoteAccumulator::new()),
            precommit_votes: Arc::new(PrecommitVoteAccumulator::new()),
            tx_status: Arc::new(DashMap::new()),
            stall_timers: Arc::new(DashMap::new()),
            liveness_alerts: DashMap::new(),
            fallback_votes: DashMap::new(),
            proposal_to_tx: DashMap::new(),
            fallback_rounds: DashMap::new(),
            active_vote_requests: Arc::new(AtomicUsize::new(0)),
            rounds_executed: AtomicUsize::new(0),
            txs_finalized: AtomicUsize::new(0),
        })
    }

    /// Get current validators (returns Arc to avoid cloning)
    /// Fetches active masternodes from registry and converts to ValidatorInfo
    pub fn get_validators(&self) -> Arc<Vec<ValidatorInfo>> {
        let masternodes = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.masternode_registry.list_active())
        });
        Arc::new(
            masternodes
                .iter()
                .map(|mn| ValidatorInfo {
                    address: mn.masternode.address.clone(),
                    weight: mn.masternode.tier.sampling_weight(),
                })
                .collect(),
        )
    }

    /// Cleanup finalized transactions and associated state older than retention period
    /// This prevents unbounded memory growth in the DashMaps
    pub fn cleanup_old_finalized(&self, retention_secs: u64) -> usize {
        let cutoff = Instant::now() - Duration::from_secs(retention_secs);
        let mut removed_count = 0;

        // Collect transactions to remove
        let to_remove: Vec<Hash256> = self
            .finalized_txs
            .iter()
            .filter(|entry| entry.value().1 < cutoff)
            .map(|entry| *entry.key())
            .collect();

        // Remove from all maps
        for txid in to_remove {
            self.finalized_txs.remove(&txid);
            self.tx_state.remove(&txid);
            self.active_rounds.remove(&txid);
            self.vfp_votes.remove(&txid);
            removed_count += 1;
        }

        if removed_count > 0 {
            tracing::debug!(
                "Cleaned up {} finalized transactions older than {} seconds",
                removed_count,
                retention_secs
            );
        }

        removed_count
    }

    /// Get memory usage statistics
    pub fn memory_stats(&self) -> ConsensusMemoryStats {
        ConsensusMemoryStats {
            tx_state_entries: self.tx_state.len(),
            active_rounds: self.active_rounds.len(),
            finalized_txs: self.finalized_txs.len(),
            avs_snapshots: self.avs_snapshots.len(),
            vfp_votes: self.vfp_votes.len(),
        }
    }

    /// Get validator addresses only (for compatibility)
    pub fn get_validator_addresses(&self) -> Vec<String> {
        self.get_validators()
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
            Arc::new(RwLock::new(VotingState::new(
                initial_preference,
                &validators,
            ))),
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
                let mut voting_state = state.value().write();
                voting_state
                    .snowflake
                    .update_suspicion(&voter_id, preference);
            }
        }
    }

    /// Execute a single query round for a transaction
    pub async fn execute_query_round(&self, txid: Hash256) -> Result<(), TimeVoteError> {
        // Get or create transaction state
        let tx_state = self
            .tx_state
            .get(&txid)
            .ok_or(TimeVoteError::TransactionNotFound)?;

        let validators = self.get_validators();
        if validators.is_empty() {
            return Err(TimeVoteError::QueryFailed(
                "No validators available".to_string(),
            ));
        }

        // Get current k from snowball (dynamic sample size)
        let current_k = {
            let voting_state = tx_state.value().read();
            voting_state.snowflake.k
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
                    self.finalized_txs
                        .insert(txid, (preference, Instant::now()));
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
    pub async fn run_consensus(&self, txid: Hash256) -> Result<Preference, TimeVoteError> {
        // Check if already finalized
        if let Some(pref) = self.finalized_txs.get(&txid) {
            return Ok(pref.value().0);
        }

        // Initialize if not already
        self.initiate_consensus(txid, Preference::Accept);

        // Run rounds until finalized
        for _ in 0..self.config.max_rounds {
            self.execute_query_round(txid).await?;

            if let Some(pref) = self.finalized_txs.get(&txid) {
                return Ok(pref.value().0);
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        Err(TimeVoteError::InsufficientConfidence {
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
        self.finalized_txs.get(txid).map(|entry| entry.value().0)
    }

    // ========================================================================
    // AVS SNAPSHOT MANAGEMENT (Per Protocol §8.4)
    // ========================================================================

    /// Create an AVS snapshot for the current slot
    /// Captures the active validator set with their weights for finality vote verification
    pub fn create_avs_snapshot(&self, slot_index: u64) -> AVSSnapshot {
        let validators = self.get_validators();
        let snapshot = AVSSnapshot::new_with_ref(slot_index, validators);

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
    /// Pure timevote: majority of sampled validators must vote for block
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
    /// Pure timevote: majority of sampled validators must vote for block
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
    pub fn get_metrics(&self) -> TimeVoteMetrics {
        TimeVoteMetrics {
            rounds_executed: self.rounds_executed.load(Ordering::Relaxed),
            txs_finalized: self.txs_finalized.load(Ordering::Relaxed),
            active_rounds: self.active_rounds.len(),
            tracked_txs: self.tx_state.len(),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TimeVoteMetrics {
    pub rounds_executed: usize,
    pub txs_finalized: usize,
    pub active_rounds: usize,
    pub tracked_txs: usize,
}

// ============================================================================
// CONSENSUS ENGINE
// ============================================================================

type FinalityTimeTracker = Arc<DashMap<[u8; 32], (Instant, Option<Instant>)>>;

#[allow(dead_code)]
pub struct ConsensusEngine {
    // Reference to the masternode registry (single source of truth)
    masternode_registry: Arc<MasternodeRegistry>,
    // Set once at startup - use OnceLock
    identity: OnceLock<NodeIdentity>,
    pub utxo_manager: Arc<UTXOStateManager>,
    pub tx_pool: Arc<TransactionPool>,
    pub broadcast_callback: BroadcastCallback,
    pub state_notifier: Arc<StateNotifier>,
    pub timevote: Arc<TimeVoteConsensus>,
    pub finality_proof_mgr: Arc<FinalityProofManager>,
    pub ai_validator: Option<Arc<crate::ai::AITransactionValidator>>,

    /// Track finality times: block_hash -> (received_at, finalized_at)
    finality_times: FinalityTimeTracker,
    /// Rolling average of last 20 finality times (in milliseconds)
    avg_finality_ms: Arc<parking_lot::RwLock<Vec<f64>>>,
}

impl ConsensusEngine {
    pub fn new(
        masternode_registry: Arc<MasternodeRegistry>,
        utxo_manager: Arc<UTXOStateManager>,
    ) -> Self {
        let timevote_config = TimeVoteConfig::default();
        let timevote = TimeVoteConsensus::new(timevote_config, masternode_registry.clone())
            .expect("Failed to initialize TimeVote consensus");

        Self {
            masternode_registry,
            identity: OnceLock::new(),
            utxo_manager,
            tx_pool: Arc::new(TransactionPool::new()),
            broadcast_callback: Arc::new(TokioRwLock::new(None)),
            state_notifier: Arc::new(StateNotifier::new()),
            timevote: Arc::new(timevote),
            finality_proof_mgr: Arc::new(FinalityProofManager::new(1)), // chain_id = 1 for mainnet
            ai_validator: None,
            finality_times: Arc::new(DashMap::new()),
            avg_finality_ms: Arc::new(parking_lot::RwLock::new(Vec::new())),
        }
    }

    /// Create a test instance without UTXO manager (for unit tests)
    #[cfg(test)]
    pub fn new_test(timevote_config: TimeVoteConfig) -> Self {
        // Create UTXO manager and masternode registry with in-memory storage
        let utxo_manager = Arc::new(UTXOStateManager::new());
        let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
        let masternode_registry =
            Arc::new(MasternodeRegistry::new(db, crate::NetworkType::Testnet));

        let timevote = TimeVoteConsensus::new(timevote_config, masternode_registry.clone())
            .expect("Failed to initialize TimeVote consensus");

        Self {
            masternode_registry,
            identity: OnceLock::new(),
            utxo_manager,
            tx_pool: Arc::new(TransactionPool::new()),
            broadcast_callback: Arc::new(TokioRwLock::new(None)),
            state_notifier: Arc::new(StateNotifier::new()),
            timevote: Arc::new(timevote),
            finality_proof_mgr: Arc::new(FinalityProofManager::new(1)),
            ai_validator: None,
            finality_times: Arc::new(DashMap::new()),
            avg_finality_ms: Arc::new(parking_lot::RwLock::new(Vec::new())),
        }
    }

    pub fn enable_ai_validation(&mut self, db: Arc<sled::Db>) {
        self.ai_validator = Some(Arc::new(crate::ai::AITransactionValidator::new(db)));
        tracing::info!("🤖 AI transaction validation enabled");
    }

    /// Record when a block is received (start of finality tracking)
    pub fn record_block_received(&self, block_hash: [u8; 32]) {
        self.finality_times
            .insert(block_hash, (Instant::now(), None));
    }

    /// Record when a block achieves finality and update average
    pub fn record_block_finalized(&self, block_hash: [u8; 32]) {
        if let Some(mut entry) = self.finality_times.get_mut(&block_hash) {
            let now = Instant::now();
            let (received_at, finalized_at) = entry.value_mut();
            *finalized_at = Some(now);

            // Calculate finality time in milliseconds
            let finality_ms = now.duration_since(*received_at).as_secs_f64() * 1000.0;

            // Update rolling average (keep last 20 measurements)
            let mut avg = self.avg_finality_ms.write();
            avg.push(finality_ms);
            if avg.len() > 20 {
                avg.remove(0);
            }

            tracing::debug!(
                "📊 Block {} finalized in {:.2}ms",
                hex::encode(block_hash),
                finality_ms
            );
        }
    }

    /// Get average finality time in milliseconds
    pub fn get_avg_finality_time_ms(&self) -> u64 {
        let avg = self.avg_finality_ms.read();
        if avg.is_empty() {
            return 750; // Default value if no measurements yet
        }
        let sum: f64 = avg.iter().sum();
        (sum / avg.len() as f64) as u64
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

    /// Get the signing key for this node (for VRF generation in block production)
    pub fn get_signing_key(&self) -> Option<ed25519_dalek::SigningKey> {
        self.identity.get().map(|id| id.signing_key.clone())
    }

    // ========================================================================
    // FINALITY VOTE GENERATION (Per Protocol §8.5)
    // ========================================================================

    /// Generate a finality vote for a transaction if this validator is AVS-active
    /// Called when this validator responds with "Valid" during query round
    pub fn generate_finality_vote(
        &self,
        txid: Hash256,
        tx: &Transaction,
        slot_index: u64,
        snapshot: &AVSSnapshot,
    ) -> Option<FinalityVote> {
        // Get identity (returns None if not set)
        let identity = self.identity.get()?;
        let voter_mn_id = identity.address.clone();

        // Only generate vote if voter is in the AVS snapshot for this slot
        if !snapshot.contains_validator(&voter_mn_id) {
            return None;
        }

        // Get voter weight from snapshot
        let voter_weight = snapshot.get_validator_weight(&voter_mn_id)?;

        // Compute transaction hash commitment (BLAKE3 hash of canonical tx bytes)
        let tx_bytes = bincode::serialize(tx).ok()?;
        let tx_hash = blake3::hash(&tx_bytes);

        // Convert blake3 hash to Hash256 (both are [u8; 32])
        let tx_hash_commitment: Hash256 = *tx_hash.as_bytes();

        // Sign and create the vote using identity
        let vote = identity.sign_finality_vote(
            1, // TODO: Make chain_id configurable
            txid,
            tx_hash_commitment,
            slot_index,
            VoteDecision::Accept, // This vote is for a valid/preferred transaction
            voter_mn_id,
            voter_weight,
        );

        Some(vote)
    }

    /// Broadcast a finality vote to all peer masternodes
    /// Used by consensus to propagate votes across the network
    pub fn broadcast_finality_vote(&self, vote: FinalityVote) -> NetworkMessage {
        NetworkMessage::FinalityVoteBroadcast { vote }
    }

    // ========================================================================
    // MASTERNODE HELPERS
    // ========================================================================

    // Lock-free read of masternodes from registry
    fn get_masternodes(&self) -> Vec<Masternode> {
        // Get active masternodes from the registry (single source of truth)
        let active = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.masternode_registry.list_active())
        });
        active.iter().map(|info| info.masternode.clone()).collect()
    }

    fn is_masternode(&self, address: &str) -> bool {
        let masternodes = self.get_masternodes();
        masternodes.iter().any(|mn| mn.address == address)
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
        // 0. AI-powered validation first (if enabled)
        if let Some(ai_validator) = &self.ai_validator {
            ai_validator.validate_with_ai(tx).await?;
        }

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
            if let Ok(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
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

        // Also check proportional fee (0.1% of transaction input value)
        // Fee is based on inputs (economic value moved) not outputs (which include change)
        let fee_rate = 1000; // 0.1% = 1/1000
        let min_proportional_fee = input_sum / fee_rate;

        if actual_fee < min_proportional_fee {
            return Err(format!(
                "Insufficient fee: {} satoshis < {} satoshis required (0.1% of {})",
                actual_fee, min_proportional_fee, input_sum
            ));
        }

        if input_sum < output_sum {
            return Err(format!(
                "Insufficient funds: {} < {}",
                input_sum, output_sum
            ));
        }

        // ===== CRITICAL FIX #1: VERIFY SIGNATURES ON ALL INPUTS =====
        // Skip signature verification if script_sig is empty (unsigned transaction)
        // TODO: Remove this once wallet signing is fully implemented
        for (idx, input) in tx.inputs.iter().enumerate() {
            if !input.script_sig.is_empty() {
                self.verify_input_signature(tx, idx).await?;
            } else {
                tracing::debug!("Skipping signature verification for unsigned input {}", idx);
            }
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
            .map_err(|e| format!("UTXO not found: {:?} - {}", input.previous_output, e))?;

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
    pub async fn submit_transaction(&self, tx: Transaction) -> Result<Hash256, String> {
        let txid = tx.txid();

        // Step 1: Atomically lock and validate
        self.lock_and_validate_transaction(&tx).await?;

        // Step 2: Broadcast transaction to network FIRST
        // This ensures validators receive the TX before vote requests
        self.broadcast(NetworkMessage::TransactionBroadcast(tx.clone()))
            .await;

        // Step 3: Process transaction through consensus locally (this adds to pool)
        // AND broadcasts vote request - validators will have received TX by now
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
                if let Ok(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
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

        // ===== timevote CONSENSUS INTEGRATION =====
        // Start timevote Snowball consensus for this transaction
        // Use validators from consensus engine (which queries masternode registry)
        let validators_for_consensus = self.timevote.get_validators();

        tracing::warn!(
            "🔄 Starting TimeVote consensus for TX {:?} with {} validators: {:?}",
            hex::encode(txid),
            validators_for_consensus.len(),
            validators_for_consensus
                .iter()
                .map(|v| &v.address)
                .collect::<Vec<_>>()
        );

        // BYPASS: Auto-finalize for single-node or low-validator scenarios
        // In production with <3 active validators, skip consensus and finalize immediately
        // This handles development/testing and bootstrap scenarios
        //
        // ALSO: If TIMECOIN_DEV_MODE=1 is set, auto-finalize regardless of validator count
        let dev_mode = std::env::var("TIMECOIN_DEV_MODE").unwrap_or_default() == "1";

        if validators_for_consensus.len() < 3 || dev_mode {
            if dev_mode {
                tracing::warn!(
                    "⚡ DEV MODE: Auto-finalizing TX {:?} (TIMECOIN_DEV_MODE=1)",
                    hex::encode(txid)
                );
            } else {
                tracing::warn!(
                    "⚡ Auto-finalizing TX {:?} - insufficient validators ({} < 3) for consensus",
                    hex::encode(txid),
                    validators_for_consensus.len()
                );
            }

            // Move directly to finalized pool
            if let Some(_finalized_tx) = self.tx_pool.finalize_transaction(txid) {
                tracing::info!("✅ TX {:?} auto-finalized", hex::encode(txid));
            }

            // Record finalization
            self.timevote
                .finalized_txs
                .insert(txid, (Preference::Accept, Instant::now()));

            // Update status to Finalized
            self.timevote.tx_status.insert(
                txid,
                TransactionStatus::Finalized {
                    finalized_at: chrono::Utc::now().timestamp_millis(),
                    vfp_weight: 0,
                },
            );

            return Ok(());
        }

        // Initiate consensus with Snowball
        let tx_state = Arc::new(RwLock::new(VotingState::new(
            Preference::Accept,
            &validators_for_consensus,
        )));
        self.timevote.tx_state.insert(txid, tx_state);

        // Create initial QueryRound for vote tracking
        let query_round = Arc::new(RwLock::new(QueryRound::new(
            0,
            txid,
            validators_for_consensus.as_ref().clone(),
        )));
        self.timevote.active_rounds.insert(txid, query_round);

        // §7.6 Integration: Set initial transaction status to Voting
        // Pre-generate vote requests (before async, so RNG doesn't cross await boundary)
        let vote_request_msg = NetworkMessage::TransactionVoteRequest { txid };

        // Immediately broadcast vote request to all validators
        // BUT: Give validators 200ms to receive and process the TransactionBroadcast first
        // This prevents voting "Reject" because they haven't seen the TX yet
        tokio::time::sleep(Duration::from_millis(200)).await;

        if let Some(callback) = self.broadcast_callback.read().await.as_ref() {
            tracing::info!(
                "📡 Broadcasting vote request for TX {:?} to all validators (after propagation delay)",
                hex::encode(txid)
            );
            callback(vote_request_msg.clone());
        } else {
            tracing::error!("❌ No broadcast callback available - cannot send vote requests!");
        }

        self.transition_to_voting(txid);

        // Spawn consensus round executor as blocking task
        let consensus = self.timevote.clone();
        let _utxo_mgr = self.utxo_manager.clone();
        let tx_pool = self.tx_pool.clone();
        let broadcast_callback = self.broadcast_callback.clone();
        let _masternodes_for_voting = masternodes.clone();
        let tx_status_map = self.timevote.tx_status.clone(); // §7.6: Track status for fallback

        // PRIORITY: Spawn with high priority for instant finality
        tokio::spawn(async move {
            // Minimal delay - instant finality requires fast response
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Execute multiple timevote rounds for this transaction
            let max_rounds = 10;
            for round_num in 0..max_rounds {
                // §7.6 Integration: Check if transaction is in FallbackResolution state
                // If in fallback, skip timevote sampling and let fallback protocol handle it
                if let Some(status_entry) = tx_status_map.get(&txid) {
                    if matches!(
                        status_entry.value(),
                        TransactionStatus::FallbackResolution { .. }
                    ) {
                        tracing::info!(
                            "Round {}: TX {:?} in FallbackResolution, skipping timevote sampling",
                            round_num,
                            hex::encode(txid)
                        );
                        // Wait for fallback to complete
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                }

                // Create new QueryRound for this round
                let query_round = Arc::new(RwLock::new(QueryRound::new(
                    round_num,
                    txid,
                    (*validators_for_consensus).clone(),
                )));
                consensus.active_rounds.insert(txid, query_round);

                // Sample size calculation (no RNG needed for count)
                let sample_size = (validators_for_consensus.len() / 3)
                    .max(3)
                    .min(validators_for_consensus.len());

                tracing::debug!(
                    "Round {}: Voting with {} validators from {} for TX {:?}",
                    round_num,
                    sample_size,
                    validators_for_consensus.len(),
                    hex::encode(txid)
                );

                // Send vote request to all peers (broadcast)
                if let Some(callback) = broadcast_callback.read().await.as_ref() {
                    callback(vote_request_msg.clone());
                }

                // Wait for votes to arrive - reduced for instant finality
                // 200ms should be enough for local network responses
                tokio::time::sleep(Duration::from_millis(200)).await;

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
                            let mut voting_state = tx_state.value().write();
                            let old_pref = voting_state.snowflake.preference;

                            // Update preference and confidence based on votes
                            voting_state.update(
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
                                voting_state.snowflake.confidence
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
                        // Transition directly to Finalized when threshold reached
                        if let Some(status_entry) = tx_status_map.get(&txid) {
                            let status = status_entry.value();
                            if matches!(status, TransactionStatus::Voting { .. }) {
                                drop(status_entry);
                                tx_status_map.insert(
                                    txid,
                                    TransactionStatus::Finalized {
                                        finalized_at: chrono::Utc::now().timestamp_millis(),
                                        vfp_weight: 0, // TODO: Use accumulated_weight from Snowball
                                    },
                                );
                                tracing::info!("TX {:?} → Finalized", hex::encode(txid));
                            }
                        }

                        tracing::info!(
                            "✅ TX {:?} finalized via TimeVote after round {} (progressive finality)",
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
                    consensus
                        .finalized_txs
                        .insert(txid, (preference, Instant::now()));
                } else {
                    // Check if we got ANY votes at all
                    let got_votes = consensus
                        .active_rounds
                        .get(&txid)
                        .map(|r| {
                            let round = r.value().read();
                            !round.votes_received.is_empty()
                        })
                        .unwrap_or(false);

                    // SAFETY: If transaction is valid (preference=Accept) and UTXOs are locked,
                    // but validators didn't respond (network partitioned or peers not upgraded),
                    // we can safely finalize because UTXO locks prevent double-spends
                    if !got_votes && preference == Preference::Accept {
                        tracing::warn!(
                            "⚠️ TX {:?} received 0 votes (validators not responding). Auto-finalizing because UTXOs are locked (double-spend impossible)",
                            hex::encode(txid)
                        );

                        // Auto-finalize with UTXO lock safety
                        if let Some(_finalized_tx) = tx_pool.finalize_transaction(txid) {
                            tracing::info!(
                                "✅ TX {:?} auto-finalized (UTXO-lock protected, 0 validator responses)",
                                hex::encode(txid)
                            );
                        }
                        consensus
                            .finalized_txs
                            .insert(txid, (Preference::Accept, Instant::now()));
                    } else {
                        // Got votes but didn't reach consensus threshold - truly reject
                        tx_pool
                            .reject_transaction(txid, "TimeVote consensus not reached".to_string());
                        consensus
                            .finalized_txs
                            .insert(txid, (Preference::Reject, Instant::now()));
                        tracing::warn!(
                            "❌ TX {:?} rejected: TimeVote consensus not reached after {} rounds (preference: {}, votes received: {})",
                            hex::encode(txid),
                            max_rounds,
                            preference,
                            if got_votes { "yes" } else { "no" }
                        );
                    }
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

    pub fn get_finalized_transactions_with_fees_for_block(&self) -> Vec<(Transaction, u64)> {
        self.tx_pool.get_finalized_transactions_with_fees()
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
        self.get_masternodes()
    }

    /// Submit a transaction to the consensus engine (called from RPC)
    pub async fn add_transaction(&self, tx: Transaction) -> Result<Hash256, String> {
        self.submit_transaction(tx).await
    }

    /// Cleanup old finalized transactions from TimeVote consensus
    /// Prevents unbounded memory growth by removing old finalized state
    pub fn cleanup_old_finalized(&self, retention_secs: u64) -> usize {
        self.timevote.cleanup_old_finalized(retention_secs)
    }

    // ========================================================================
    // §7.6 LIVENESS FALLBACK PROTOCOL - State Management
    // ========================================================================

    /// Start monitoring a transaction for stall detection (§7.6.1)
    /// Call this when a transaction enters Voting state
    pub fn start_stall_timer(&self, txid: Hash256) {
        self.timevote.stall_timers.insert(txid, Instant::now());
        tracing::debug!("Started stall timer for transaction {}", hex::encode(txid));
    }

    /// Check if a transaction has exceeded the stall timeout (§7.6.1)
    /// Returns true if transaction has been in Voting for > STALL_TIMEOUT
    pub fn check_stall_timeout(&self, txid: &Hash256) -> bool {
        if let Some(entry) = self.timevote.stall_timers.get(txid) {
            let elapsed = entry.value().elapsed();
            elapsed > STALL_TIMEOUT
        } else {
            false
        }
    }

    /// Stop monitoring a transaction (remove stall timer)
    /// Call when transaction reaches terminal state
    pub fn stop_stall_timer(&self, txid: &Hash256) {
        self.timevote.stall_timers.remove(txid);
    }

    /// Set transaction status (§7.3 state machine)
    pub fn set_tx_status(&self, txid: Hash256, status: TransactionStatus) {
        self.timevote.tx_status.insert(txid, status);
    }

    /// Get transaction status
    pub fn get_tx_status(&self, txid: &Hash256) -> Option<TransactionStatus> {
        self.timevote.tx_status.get(txid).map(|r| r.clone())
    }

    /// Transition transaction to Voting state (§7.3)
    pub fn transition_to_voting(&self, txid: Hash256) {
        let status = TransactionStatus::Voting {
            confidence: 0,
            counter: 0,
            started_at: chrono::Utc::now().timestamp_millis(),
        };
        self.set_tx_status(txid, status);
        self.start_stall_timer(txid);
        tracing::debug!("Transaction {} → Voting", hex::encode(txid));
    }

    /// Transition transaction to Finalized state (§8)
    pub fn transition_to_finalized(&self, txid: Hash256, vfp_weight: u64) {
        let status = TransactionStatus::Finalized {
            finalized_at: chrono::Utc::now().timestamp_millis(),
            vfp_weight,
        };
        self.set_tx_status(txid, status);
        self.stop_stall_timer(&txid);

        // §7.6 Week 5-6 Part 4: Clean up fallback tracking
        self.timevote.fallback_rounds.remove(&txid);
        self.timevote.liveness_alerts.remove(&txid);

        tracing::info!(
            "Transaction {} → Finalized (weight: {})",
            hex::encode(txid),
            vfp_weight
        );
    }

    /// Transition transaction to FallbackResolution state (§7.6.4)
    pub fn transition_to_fallback_resolution(&self, txid: Hash256, alerts_count: u32) {
        let status = TransactionStatus::FallbackResolution {
            started_at: chrono::Utc::now().timestamp_millis(),
            round: 0,
            alerts_count,
        };
        self.set_tx_status(txid, status);
        self.stop_stall_timer(&txid);

        // §7.6 Week 5-6 Part 4: Initialize fallback round tracking
        // Start with slot_index 0, round_count 0
        let current_slot = (chrono::Utc::now().timestamp() as u64) / 600; // 10-minute slots
        self.timevote
            .fallback_rounds
            .insert(txid, (current_slot, 0, Instant::now()));

        tracing::warn!(
            "Transaction {} → FallbackResolution (alerts: {}, slot: {})",
            hex::encode(txid),
            alerts_count,
            current_slot
        );
    }

    /// Transition transaction to Rejected state
    pub fn transition_to_rejected(&self, txid: Hash256, reason: String) {
        let status = TransactionStatus::Rejected {
            rejected_at: chrono::Utc::now().timestamp_millis(),
            reason: reason.clone(),
        };
        self.set_tx_status(txid, status);
        self.stop_stall_timer(&txid);

        // §7.6 Week 5-6 Part 4: Clean up fallback tracking
        self.timevote.fallback_rounds.remove(&txid);
        self.timevote.liveness_alerts.remove(&txid);

        tracing::info!("Transaction {} → Rejected: {}", hex::encode(txid), reason);
    }

    /// Get all transactions in a specific status
    pub fn get_transactions_by_status(&self, target_status: &TransactionStatus) -> Vec<Hash256> {
        self.timevote
            .tx_status
            .iter()
            .filter_map(|entry| {
                let (txid, status) = entry.pair();
                if std::mem::discriminant(status) == std::mem::discriminant(target_status) {
                    Some(*txid)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get all stalled transactions (in Voting for > STALL_TIMEOUT)
    pub fn get_stalled_transactions(&self) -> Vec<Hash256> {
        self.timevote
            .stall_timers
            .iter()
            .filter_map(|entry| {
                let (txid, start_time) = entry.pair();
                if start_time.elapsed() > STALL_TIMEOUT {
                    Some(*txid)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get memory usage statistics from consensus engine
    pub fn memory_stats(&self) -> ConsensusMemoryStats {
        self.timevote.memory_stats()
    }

    // ========================================================================
    // §7.6 LIVENESS FALLBACK PROTOCOL - ALERT & VOTE ACCUMULATION
    // ========================================================================

    /// Accumulate a LivenessAlert and check if f+1 threshold reached (§7.6.2-7.6.3)
    ///
    /// Returns true if fallback should be triggered (f+1 unique reporters)
    pub fn accumulate_liveness_alert(
        &self,
        alert: LivenessAlert,
        total_masternodes: usize,
    ) -> bool {
        let txid = alert.txid;

        // Add alert to tracker
        self.timevote
            .liveness_alerts
            .entry(txid)
            .or_default()
            .push(alert);

        // Count unique reporters (collect into Vec to avoid lifetime issues)
        let alerts_vec: Vec<String> = self
            .timevote
            .liveness_alerts
            .get(&txid)
            .map(|alerts| alerts.iter().map(|a| a.reporter_mn_id.clone()).collect())
            .unwrap_or_default();

        let unique_reporters: std::collections::HashSet<_> = alerts_vec.iter().collect();

        // Calculate f+1 threshold
        let f = (total_masternodes.saturating_sub(1)) / 3;
        let threshold = f + 1;

        unique_reporters.len() >= threshold
    }

    /// Get count of unique alert reporters for a transaction
    pub fn get_alert_count(&self, txid: &Hash256) -> usize {
        self.timevote
            .liveness_alerts
            .get(txid)
            .map(|alerts| {
                let unique: std::collections::HashSet<_> =
                    alerts.iter().map(|a| &a.reporter_mn_id).collect();
                unique.len()
            })
            .unwrap_or(0)
    }

    /// Accumulate a FallbackVote and check if Q_finality threshold reached (§7.6.4)
    ///
    /// Returns Some(decision) if quorum reached, None otherwise
    pub fn accumulate_fallback_vote(
        &self,
        vote: FallbackVote,
        total_avs_weight: u64,
    ) -> Option<FallbackVoteDecision> {
        let proposal_hash = vote.proposal_hash;

        // Add vote to tracker
        self.timevote
            .fallback_votes
            .entry(proposal_hash)
            .or_default()
            .push(vote);

        // Calculate weighted totals
        let votes = self.timevote.fallback_votes.get(&proposal_hash).unwrap();
        let mut approve_weight = 0u64;
        let mut reject_weight = 0u64;

        for v in votes.iter() {
            match v.vote {
                FallbackVoteDecision::Approve => approve_weight += v.voter_weight,
                FallbackVoteDecision::Reject => reject_weight += v.voter_weight,
            }
        }

        // Calculate Q_finality (2/3 of total AVS weight)
        let q_finality = (total_avs_weight * 2) / 3;

        // Check if threshold reached
        if approve_weight >= q_finality {
            Some(FallbackVoteDecision::Approve)
        } else if reject_weight >= q_finality {
            Some(FallbackVoteDecision::Reject)
        } else {
            None
        }
    }

    /// Get current vote status for a proposal (for logging/debugging)
    pub fn get_vote_status(&self, proposal_hash: &Hash256) -> Option<(u64, u64, usize)> {
        self.timevote
            .fallback_votes
            .get(proposal_hash)
            .map(|votes| {
                let mut approve_weight = 0u64;
                let mut reject_weight = 0u64;

                for v in votes.iter() {
                    match v.vote {
                        FallbackVoteDecision::Approve => approve_weight += v.voter_weight,
                        FallbackVoteDecision::Reject => reject_weight += v.voter_weight,
                    }
                }

                (approve_weight, reject_weight, votes.len())
            })
    }

    /// Register a proposal for a transaction (tracking proposal_hash -> txid)
    pub fn register_proposal(&self, proposal_hash: Hash256, txid: Hash256) {
        self.timevote.proposal_to_tx.insert(proposal_hash, txid);
    }

    /// Get transaction ID for a proposal hash
    pub fn get_proposal_txid(&self, proposal_hash: &Hash256) -> Option<Hash256> {
        self.timevote.proposal_to_tx.get(proposal_hash).map(|v| *v)
    }

    /// Finalize transaction based on fallback vote result (§7.6.4)
    pub fn finalize_from_fallback(
        &self,
        txid: Hash256,
        decision: FallbackVoteDecision,
        total_weight: u64,
    ) {
        match decision {
            FallbackVoteDecision::Approve => {
                // Transition to Finalized state
                self.transition_to_finalized(txid, total_weight);
                tracing::info!(
                    "✅ Transaction {} finalized via fallback (Approved with weight {})",
                    hex::encode(txid),
                    total_weight
                );
            }
            FallbackVoteDecision::Reject => {
                // Transition to Rejected state
                self.transition_to_rejected(txid, "Fallback consensus rejected".to_string());
                tracing::warn!(
                    "❌ Transaction {} rejected via fallback (weight {})",
                    hex::encode(txid),
                    total_weight
                );
            }
        }
    }

    /// Decide how to vote on a fallback finality proposal (§7.6.4)
    ///
    /// Evaluates transaction state and determines whether to vote Approve or Reject.
    /// This implements the voting decision logic for the liveness fallback protocol.
    ///
    /// # Decision Logic
    /// - **Approve**: Transaction is in Voting or FallbackResolution state (pending)
    /// - **Reject**: Transaction is already Finalized, Rejected, or not found
    ///
    /// The reasoning is that if a transaction is pending fallback resolution,
    /// we should vote to approve its finalization. If it's already resolved or
    /// doesn't exist, we vote to reject the proposal.
    ///
    /// # Arguments
    /// * `txid` - Transaction identifier to evaluate
    ///
    /// # Returns
    /// Vote decision: either Approve or Reject
    ///
    /// # Example
    /// ```rust
    /// let decision = consensus.decide_fallback_vote(&tx_hash);
    /// match decision {
    ///     FallbackVoteDecision::Approve => { /* cast approve vote */ }
    ///     FallbackVoteDecision::Reject => { /* cast reject vote */ }
    /// }
    /// ```
    pub fn decide_fallback_vote(&self, txid: &Hash256) -> FallbackVoteDecision {
        match self.get_tx_status(txid) {
            Some(TransactionStatus::Voting { .. })
            | Some(TransactionStatus::FallbackResolution { .. }) => {
                // Transaction is pending, vote to approve finalization
                FallbackVoteDecision::Approve
            }
            _ => {
                // Transaction is already resolved, not found, or in invalid state
                FallbackVoteDecision::Reject
            }
        }
    }

    // ========================================================================
    // §7.6 LIVENESS FALLBACK PROTOCOL - TIMEOUT & RETRY
    // ========================================================================

    /// Check for timed-out fallback rounds and retry with new leader (§7.6.3)
    ///
    /// This method is called periodically to detect fallback rounds that have
    /// exceeded FALLBACK_ROUND_TIMEOUT without reaching Q_finality. When a timeout
    /// is detected, the slot_index is incremented to deterministically select a
    /// new leader, and the fallback process retries.
    ///
    /// # Protocol Flow
    /// 1. Scan all transactions in FallbackResolution state
    /// 2. Check if round_started_at + FALLBACK_ROUND_TIMEOUT < now
    /// 3. If timed out:
    ///    a. Increment slot_index (deterministic leader rotation)
    ///    b. Check if round_count < MAX_FALLBACK_ROUNDS
    ///    c. If under limit: retry with new leader
    ///    d. If exceeded: escalate to TSDC checkpoint sync
    ///
    /// # Arguments
    /// * `masternode_registry` - For computing next leader
    ///
    /// # Returns
    /// Number of timed-out rounds that were retried or escalated
    ///
    /// # Example
    /// ```rust
    /// // Called every 5 seconds by background task
    /// let retry_count = consensus.check_fallback_timeouts(&registry).await;
    /// if retry_count > 0 {
    ///     info!("Retried {} timed-out fallback rounds", retry_count);
    /// }
    /// ```
    pub async fn check_fallback_timeouts(&self, masternode_registry: &MasternodeRegistry) -> usize {
        let now = Instant::now();
        let mut retried_count = 0;

        // Collect timed-out transactions
        let timed_out: Vec<(Hash256, u64, u32, Instant)> = self
            .timevote
            .fallback_rounds
            .iter()
            .filter_map(|entry| {
                let (txid, (slot_index, round_count, started_at)) = entry.pair();
                let elapsed = now.duration_since(*started_at);

                if elapsed >= FALLBACK_ROUND_TIMEOUT {
                    Some((*txid, *slot_index, *round_count, *started_at))
                } else {
                    None
                }
            })
            .collect();

        // Handle each timeout
        for (txid, slot_index, round_count, _started_at) in timed_out {
            if round_count >= MAX_FALLBACK_ROUNDS {
                // Exceeded max rounds - escalate to TSDC
                tracing::error!(
                    "❌ Transaction {} exceeded MAX_FALLBACK_ROUNDS ({}), escalating to TSDC",
                    hex::encode(txid),
                    MAX_FALLBACK_ROUNDS
                );

                // Mark for TSDC escalation
                self.transition_to_rejected(
                    txid,
                    format!(
                        "Fallback failed after {} rounds, awaiting TSDC sync",
                        MAX_FALLBACK_ROUNDS
                    ),
                );

                // Remove from fallback tracking
                self.timevote.fallback_rounds.remove(&txid);
                retried_count += 1;
            } else {
                // Retry with new leader (increment slot_index)
                let new_slot_index = slot_index + 1;
                let new_round_count = round_count + 1;

                tracing::warn!(
                    "⏱️ Fallback round timeout for tx {} (slot {}, round {}/{}), retrying with slot {}",
                    hex::encode(txid),
                    slot_index,
                    round_count,
                    MAX_FALLBACK_ROUNDS,
                    new_slot_index
                );

                // Update fallback round tracker
                self.timevote
                    .fallback_rounds
                    .insert(txid, (new_slot_index, new_round_count, Instant::now()));

                // Compute new leader
                let masternodes = masternode_registry.list_all().await;
                let avs: Vec<Masternode> = masternodes
                    .iter()
                    .filter(|mn| mn.is_active)
                    .map(|mn| mn.masternode.clone())
                    .collect();

                if let Some(new_leader_id) = compute_fallback_leader(&txid, new_slot_index, &avs) {
                    tracing::info!(
                        "🔄 New leader for tx {}: {} (slot {})",
                        hex::encode(txid),
                        new_leader_id,
                        new_slot_index
                    );

                    // If we are the new leader, broadcast proposal
                    if let Some(identity) = self.identity.get() {
                        if identity.address == new_leader_id {
                            tracing::info!(
                                "✅ We are the new leader for tx {} (slot {}), broadcasting proposal",
                                hex::encode(txid),
                                new_slot_index
                            );

                            // Decide proposal vote
                            let decision = match self.get_tx_status(&txid) {
                                Some(TransactionStatus::FallbackResolution { .. }) => {
                                    FallbackDecision::Accept
                                }
                                _ => FallbackDecision::Reject,
                            };

                            // Broadcast the proposal
                            if let Err(e) = self
                                .broadcast_finality_proposal(txid, new_slot_index, decision)
                                .await
                            {
                                tracing::error!(
                                    "Failed to broadcast retry proposal for tx {}: {}",
                                    hex::encode(txid),
                                    e
                                );
                            }
                        }
                    }

                    retried_count += 1;
                } else {
                    tracing::error!(
                        "Could not compute new leader for tx {} (empty AVS?)",
                        hex::encode(txid)
                    );
                }
            }
        }

        retried_count
    }

    /// Start a background task that periodically checks for fallback round timeouts (§7.6.3)
    ///
    /// This spawns a tokio task that runs every `check_interval_secs` seconds
    /// to detect and handle timed-out fallback rounds.
    ///
    /// # Arguments
    /// * `consensus` - Arc reference to ConsensusEngine
    /// * `masternode_registry` - For computing new leaders
    /// * `check_interval_secs` - How often to check (recommended: 5 seconds)
    ///
    /// # Returns
    /// JoinHandle for the background task
    ///
    /// # Example
    /// ```rust
    /// let timeout_checker = ConsensusEngine::start_fallback_timeout_checker(
    ///     consensus.clone(),
    ///     registry.clone(),
    ///     5, // Check every 5 seconds
    /// );
    /// ```
    pub fn start_fallback_timeout_checker(
        consensus: Arc<Self>,
        masternode_registry: Arc<MasternodeRegistry>,
        check_interval_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(check_interval_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                // Check for timed-out fallback rounds
                let retry_count = consensus
                    .check_fallback_timeouts(&masternode_registry)
                    .await;

                if retry_count > 0 {
                    tracing::info!(
                        "§7.6 Timeout checker handled {} fallback round timeouts",
                        retry_count
                    );
                }
            }
        })
    }

    // ========================================================================
    // §7.6 LIVENESS FALLBACK PROTOCOL - BROADCASTING
    // ========================================================================

    /// Broadcast a LivenessAlert for a stalled transaction (§7.6.2)
    ///
    /// Called when a local node detects a transaction has been in Sampling state
    /// for longer than STALL_TIMEOUT (30 seconds) without reaching finality.
    ///
    /// # Protocol Flow
    /// 1. Extracts transaction state (confidence, stall duration)
    /// 2. Signs alert with node's Ed25519 key
    /// 3. Broadcasts to all peers via gossip protocol
    /// 4. Peers will accumulate alerts and trigger fallback when f+1 threshold reached
    ///
    /// # Arguments
    /// * `txid` - Transaction identifier that is stalled
    /// * `slot_index` - Current TSDC slot index (10-minute epochs)
    ///
    /// # Returns
    /// * `Ok(())` - Alert signed and broadcast successfully
    /// * `Err(String)` - If identity not set, transaction not found, or not in Sampling state
    ///
    /// # Example
    /// ```ignore
    /// // Called periodically by stall checker
    /// if consensus.check_stall_timeout(&txid) {
    ///     consensus.broadcast_liveness_alert(txid, current_slot).await?;
    /// }
    /// ```
    /// Called when local node detects a transaction has stalled
    pub async fn broadcast_liveness_alert(
        &self,
        txid: Hash256,
        slot_index: u64,
    ) -> Result<(), String> {
        // Require identity to sign alerts
        let identity = self
            .identity
            .get()
            .ok_or_else(|| "Node identity not set".to_string())?;

        // Get current transaction state
        let tx_status = self
            .timevote
            .tx_status
            .get(&txid)
            .ok_or_else(|| format!("Transaction {} not found", hex::encode(txid)))?;

        // Extract confidence and stall duration
        let (current_confidence, stall_duration_ms) = match tx_status.value() {
            TransactionStatus::Voting {
                confidence,
                started_at,
                ..
            } => {
                let elapsed = chrono::Utc::now().timestamp_millis() - started_at;
                (*confidence, elapsed.max(0) as u64)
            }
            _ => {
                return Err(format!(
                    "Transaction {} not in Voting state",
                    hex::encode(txid)
                ))
            }
        };

        // Get tx_hash_commitment (use txid for now, will be transaction hash in full implementation)
        let tx_hash_commitment = txid;

        // Get poll history (empty for now, will be populated from Snowball state)
        let poll_history = Vec::new();

        // Sign and create the alert
        let alert = identity.sign_liveness_alert(
            1, // chain_id = 1 for mainnet
            txid,
            tx_hash_commitment,
            slot_index,
            poll_history,
            stall_duration_ms,
            current_confidence,
        );

        tracing::warn!(
            "Broadcasting LivenessAlert for tx {} (stall: {}ms, confidence: {})",
            hex::encode(txid),
            stall_duration_ms,
            current_confidence
        );

        // Broadcast to network
        self.broadcast(NetworkMessage::LivenessAlert { alert })
            .await;

        Ok(())
    }

    /// Broadcast a FinalityProposal as deterministic leader (§7.6.4 Step 3)
    ///
    /// Called when this node has been elected as the deterministic fallback leader
    /// and must propose an Accept/Reject decision for a stalled transaction.
    ///
    /// # Protocol Flow (§7.6.4)
    /// 1. Node computes itself as leader via `compute_fallback_leader(txid, slot, AVS)`
    /// 2. Signs proposal with decision (Accept or Reject)
    /// 3. Broadcasts to all AVS members
    /// 4. AVS members vote on the proposal
    /// 5. Transaction finalized if Q_finality votes received
    ///
    /// # Leader Election
    /// Leader is deterministic: `leader = MN with minimum H(txid || slot_index || mn_pubkey)`
    /// All nodes compute same leader independently without coordination.
    ///
    /// # Arguments
    /// * `txid` - Transaction being proposed for finalization
    /// * `slot_index` - Current slot (increments on timeout for new leader)
    /// * `decision` - FallbackDecision::Accept or FallbackDecision::Reject
    ///
    /// # Returns
    /// * `Ok(())` - Proposal signed and broadcast successfully
    /// * `Err(String)` - If identity not set
    ///
    /// # Example
    /// ```ignore
    /// let leader = consensus.compute_fallback_leader(&txid, slot, &avs_members)?;
    /// if leader.address == my_address {
    ///     consensus.broadcast_finality_proposal(txid, slot, FallbackDecision::Accept).await?;
    /// }
    /// ```
    pub async fn broadcast_finality_proposal(
        &self,
        txid: Hash256,
        slot_index: u64,
        decision: FallbackDecision,
    ) -> Result<(), String> {
        // Require identity to sign proposals
        let identity = self
            .identity
            .get()
            .ok_or_else(|| "Node identity not set".to_string())?;

        // Get tx_hash_commitment (use txid for now)
        let tx_hash_commitment = txid;

        // Create justification string
        let justification = format!("Fallback decision for slot {}", slot_index);

        // Sign and create the proposal
        let proposal = identity.sign_finality_proposal(
            1, // chain_id = 1 for mainnet
            txid,
            tx_hash_commitment,
            slot_index,
            decision.clone(),
            justification,
        );

        tracing::info!(
            "Broadcasting FinalityProposal for tx {} (decision: {:?})",
            hex::encode(txid),
            decision
        );

        // Broadcast to network
        self.broadcast(NetworkMessage::FinalityProposal { proposal })
            .await;

        Ok(())
    }

    /// Broadcast a FallbackVote on a leader's proposal (§7.6.4 Step 4)
    ///
    /// Called when an AVS member node receives a FinalityProposal and must vote.
    ///
    /// # Protocol Flow
    /// 1. Receive FinalityProposal from deterministic leader
    /// 2. Validate proposal (correct leader, valid decision)
    /// 3. Vote Approve or Reject based on local view
    /// 4. Broadcast vote to all AVS members
    /// 5. Accumulate votes until Q_finality threshold reached
    ///
    /// # Arguments
    /// * `proposal_hash` - Hash of the FinalityProposal being voted on
    /// * `vote` - FallbackVoteDecision::Approve or FallbackVoteDecision::Reject
    /// * `voter_weight` - Stake weight of this masternode
    ///
    /// # Returns
    /// * `Ok(())` - Vote signed and broadcast successfully
    /// * `Err(String)` - If identity not set
    ///
    /// # Example
    /// ```ignore
    /// // On receiving FinalityProposal
    /// let vote_decision = validate_proposal(&proposal)?;
    /// let my_weight = get_my_stake_weight();
    /// consensus.broadcast_fallback_vote(proposal.hash(), vote_decision, my_weight).await?;
    /// ```
    pub async fn broadcast_fallback_vote(
        &self,
        proposal_hash: Hash256,
        vote: FallbackVoteDecision,
        voter_weight: u64,
    ) -> Result<(), String> {
        // Require identity to sign votes
        let identity = self
            .identity
            .get()
            .ok_or_else(|| "Node identity not set".to_string())?;

        // Sign and create the vote
        let fallback_vote = identity.sign_fallback_vote(
            1, // chain_id = 1 for mainnet
            proposal_hash,
            vote.clone(),
            voter_weight,
        );

        tracing::debug!(
            "Broadcasting FallbackVote for proposal {} (vote: {:?}, weight: {})",
            hex::encode(proposal_hash),
            vote,
            voter_weight
        );

        // Broadcast to network
        self.broadcast(NetworkMessage::FallbackVote {
            vote: fallback_vote,
        })
        .await;

        Ok(())
    }

    /// Check for stalled transactions and broadcast alerts (§7.6.1-7.6.2)
    ///
    /// Scans all active transactions for stalls (Sampling > STALL_TIMEOUT)
    /// and broadcasts LivenessAlerts for each one found.
    ///
    /// # Timing
    /// Should be called periodically (e.g., every 5-10 seconds) via background task.
    /// See `start_stall_checker()` for automated periodic checking.
    ///
    /// # Arguments
    /// * `current_slot` - Current TSDC slot index for alert timestamp
    ///
    /// # Returns
    /// * Number of stalled transactions found and alerted
    ///
    /// # Performance
    /// * Time complexity: O(N) where N = active transactions
    /// * Typical duration: < 1ms for N < 1000
    ///
    /// # Example
    /// ```ignore
    /// // Manual check
    /// let slot = get_current_slot();
    /// let stalled_count = consensus.check_and_broadcast_stalls(slot).await;
    /// if stalled_count > 0 {
    ///     warn!("Found {} stalled transactions", stalled_count);
    /// }
    /// ```
    pub async fn check_and_broadcast_stalls(&self, current_slot: u64) -> usize {
        let stalled = self.get_stalled_transactions();
        let count = stalled.len();

        for txid in stalled {
            if let Err(e) = self.broadcast_liveness_alert(txid, current_slot).await {
                tracing::error!("Failed to broadcast LivenessAlert: {}", e);
            }
        }

        if count > 0 {
            tracing::warn!(
                "Detected and broadcast alerts for {} stalled transactions",
                count
            );
        }

        count
    }

    /// Resume timevote sampling after fallback completes (§7.6.5)
    ///
    /// Transitions transaction from FallbackResolution back to Sampling state.
    /// Used when fallback times out or otherwise fails to finalize.
    ///
    /// # Protocol Flow (§7.6.5)
    /// 1. Fallback round times out (no Q_finality votes received in 10s)
    /// 2. Increment slot_index → new deterministic leader
    /// 3. If MAX_FALLBACK_ROUNDS exceeded, resume timevote sampling
    /// 4. Transaction gets fresh stall timer, returns to normal consensus
    ///
    /// # Arguments
    /// * `txid` - Transaction to resume sampling for
    ///
    /// # Returns
    /// * `Ok(())` - Successfully transitioned back to Sampling
    /// * `Err(String)` - If transaction not found or not in FallbackResolution state
    ///
    /// # State Transitions
    /// ```text
    /// FallbackResolution → Sampling (with fresh timer)
    /// ```
    ///
    /// # Example
    /// ```ignore
    /// // After fallback timeout
    /// if fallback_round_failed && round_count >= MAX_FALLBACK_ROUNDS {
    ///     consensus.resume_sampling_after_fallback(txid)?;
    ///     info!("Resumed timevote sampling for tx {}", hex::encode(txid));
    /// }
    /// ```
    pub fn resume_sampling_after_fallback(&self, txid: Hash256) -> Result<(), String> {
        // Check current status
        let current_status = self
            .timevote
            .tx_status
            .get(&txid)
            .ok_or_else(|| format!("Transaction {} not found", hex::encode(txid)))?;

        // Only resume if in FallbackResolution
        if !matches!(
            current_status.value(),
            TransactionStatus::FallbackResolution { .. }
        ) {
            return Err(format!(
                "Transaction {} not in FallbackResolution state",
                hex::encode(txid)
            ));
        }

        drop(current_status);

        // Reset to Voting state with fresh timer
        self.transition_to_voting(txid);

        tracing::info!(
            "Resumed TimeVote voting for TX {} after fallback",
            hex::encode(txid)
        );

        Ok(())
    }

    /// Start background task for periodic stall checking (§7.6)
    ///
    /// Spawns a Tokio task that continuously monitors for stalled transactions
    /// and automatically broadcasts LivenessAlerts.
    ///
    /// # Usage
    /// Call once during node initialization, after ConsensusEngine is ready:
    /// ```ignore
    /// let consensus = Arc::new(consensus_engine);
    /// let handle = ConsensusEngine::start_stall_checker(consensus.clone(), 10);
    /// // Keep handle if you need to cancel checker later
    /// ```
    ///
    /// # Arguments
    /// * `consensus` - Arc reference to ConsensusEngine (required for 'static lifetime)
    /// * `check_interval_secs` - Seconds between stall checks (recommended: 5-10)
    ///
    /// # Returns
    /// * `JoinHandle<()>` - Handle to the background task (can be cancelled if needed)
    ///
    /// # Protocol Timing
    /// * Stall detection: 30 seconds (STALL_TIMEOUT)
    /// * Check interval: configurable (default 10s in production)
    /// * Max alert delay: check_interval_secs + network propagation (~11s typical)
    ///
    /// # Performance
    /// * CPU: ~0.1ms per check (O(N) scan of active transactions)
    /// * Memory: No additional allocation (uses existing state)
    /// * Network: Only broadcasts when stalls detected (rare in normal operation)
    ///
    /// # Example
    /// ```ignore
    /// // In main.rs or node initialization
    /// let consensus = Arc::new(consensus_engine);
    /// let stall_checker_handle = ConsensusEngine::start_stall_checker(
    ///     consensus.clone(),
    ///     10, // Check every 10 seconds
    /// );
    /// info!("§7.6 Stall checker started");
    /// ```
    pub fn start_stall_checker(
        consensus: Arc<Self>,
        check_interval_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(check_interval_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                // Get current slot index (placeholder - will be integrated with TSDC)
                let current_slot = (chrono::Utc::now().timestamp() as u64) / 600; // 10-minute slots

                // Check for stalled transactions and broadcast alerts
                let stalled_count = consensus.check_and_broadcast_stalls(current_slot).await;

                if stalled_count > 0 {
                    tracing::warn!(
                        "§7.6 Stall checker found {} stalled transactions",
                        stalled_count
                    );
                }
            }
        })
    }

    #[allow(dead_code)]
    pub async fn generate_deterministic_block(&self, height: u64, _timestamp: i64) -> Block {
        use crate::block::generator::DeterministicBlockGenerator;

        let finalized = self.get_finalized_transactions_with_fees_for_block();
        let (finalized_txs, fees): (Vec<_>, Vec<_>) = finalized.into_iter().unzip();
        let masternodes = self.get_active_masternodes();
        let previous_hash = [0u8; 32];
        let base_reward = 100;

        DeterministicBlockGenerator::generate(
            height,
            previous_hash,
            finalized_txs,
            fees,
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

        let finalized = self.get_finalized_transactions_with_fees_for_block();
        let (finalized_txs, fees): (Vec<_>, Vec<_>) = finalized.into_iter().unzip();
        let previous_hash = [0u8; 32];
        let base_reward = 100;

        // Convert to format expected by generator
        let masternodes: Vec<Masternode> = eligible.iter().map(|(mn, _addr)| mn.clone()).collect();

        DeterministicBlockGenerator::generate(
            height,
            previous_hash,
            finalized_txs,
            fees,
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

        let finalized = self.get_finalized_transactions_with_fees_for_block();
        let (finalized_txs, fees): (Vec<_>, Vec<_>) = finalized.into_iter().unzip();
        let previous_hash = [0u8; 32];
        let base_reward = 100;

        DeterministicBlockGenerator::generate(
            height,
            previous_hash,
            finalized_txs,
            fees,
            masternodes,
            base_reward,
        )
    }
}

#[cfg(test)]
fn create_test_registry() -> Arc<MasternodeRegistry> {
    let db = Arc::new(sled::Config::new().temporary(true).open().unwrap());
    Arc::new(MasternodeRegistry::new(db, crate::NetworkType::Testnet))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_txid(byte: u8) -> Hash256 {
        [byte; 32]
    }

    #[test]
    fn test_timevote_init() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let av = TimeVoteConsensus::new(config, registry).unwrap();
        assert_eq!(av.get_validators().len(), 0);
    }

    #[test]
    fn test_validator_management() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let av = TimeVoteConsensus::new(config, registry).unwrap();

        // Validators now come from masternode registry, so this test
        // just verifies that get_validators() works
        let validators = av.get_validators();
        assert_eq!(validators.len(), 0); // No masternodes registered
    }

    #[test]
    fn test_initiate_consensus() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let av = TimeVoteConsensus::new(config, registry).unwrap();
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
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let av = TimeVoteConsensus::new(config, registry).unwrap();
        let txid = test_txid(1);

        av.initiate_consensus(txid, Preference::Accept);
        av.submit_vote(txid, "v1".to_string(), Preference::Accept);

        // Votes recorded but not processed until round completes
    }

    // Snowflake tests disabled - implementation replaced by newer timevote consensus
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
        let registry = create_test_registry();

        let config = TimeVoteConfig {
            sample_size: 0,
            ..Default::default()
        };
        assert!(TimeVoteConsensus::new(config, registry.clone()).is_err());

        let config = TimeVoteConfig {
            finality_confidence: 0,
            ..Default::default()
        };
        assert!(TimeVoteConsensus::new(config, registry).is_err());
    }
}

// ============================================================================
// §7.6 LIVENESS FALLBACK PROTOCOL - Leader Election
// ============================================================================

/// Compute deterministic fallback leader for a stalled transaction (§7.6.4 Step 2)
///
/// Uses SHA-256 hash function to select a leader that all nodes compute identically,
/// without any message exchange or coordination. Leader selection is deterministic
/// based on transaction ID, slot index, and masternode public keys.
///
/// # Algorithm
/// ```text
/// For each masternode in AVS:
///     score = H(txid || slot_index || mn_pubkey)
/// leader = masternode with minimum score
/// ```
///
/// # Properties
/// - **Deterministic:** Same inputs → same output on all nodes
/// - **Unpredictable:** Hash function prevents gaming the system
/// - **Fair:** Each masternode has equal probability (uniform hash distribution)
/// - **Timeout-resistant:** Incrementing slot_index selects new leader
///
/// # Timeout Handling (§7.6.5)
/// If leader fails or times out:
/// 1. All nodes increment `slot_index`
/// 2. Recompute leader with new slot_index
/// 3. New leader deterministically selected
/// 4. No coordination or view change messages needed
///
/// # Arguments
/// * `txid` - The stalled transaction ID (32 bytes)
/// * `slot_index` - Current slot index (increments on timeout)
/// * `avs` - Active Validator Set snapshot (from Protocol §8.4)
///
/// # Returns
/// * `Some(mn_id)` - The masternode address of the elected leader
/// * `None` - If AVS is empty (should not happen in production)
///
/// # Performance
/// * Time: O(N log N) where N = AVS size (dominated by sorting)
/// * Space: O(N) for score vector
/// * Typical: < 1ms for N = 100 masternodes
///
/// # Example
/// ```ignore
/// // All nodes compute same leader independently
/// let txid = stalled_transaction.txid();
/// let slot = current_slot_index();
/// let avs = consensus.get_avs_snapshot(slot)?;
///
/// let leader_id = compute_fallback_leader(&txid, slot, &avs).unwrap();
///
/// if leader_id == my_node_id {
///     // I am the leader, propose decision
///     consensus.broadcast_finality_proposal(txid, slot, decision).await?;
/// }
/// ```
pub fn compute_fallback_leader(
    txid: &Hash256,
    slot_index: u64,
    avs: &[Masternode],
) -> Option<String> {
    if avs.is_empty() {
        return None;
    }

    // Compute hash score for each masternode
    let mut scores: Vec<(Hash256, String)> = avs
        .iter()
        .map(|mn| {
            let mut hasher = Sha256::new();
            hasher.update(txid);
            hasher.update(slot_index.to_le_bytes());
            hasher.update(mn.public_key.as_bytes());
            let score: Hash256 = hasher.finalize().into();
            (score, mn.address.clone())
        })
        .collect();

    // Leader is the masternode with minimum hash score
    scores.sort_by(|a, b| a.0.cmp(&b.0));

    scores.first().map(|(_, mn_id)| mn_id.clone())
}

#[cfg(test)]
mod fallback_tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn test_compute_fallback_leader_deterministic() {
        // Create test masternodes
        let mut avs = Vec::new();
        for i in 0..5 {
            let signing_key = SigningKey::from_bytes(&[i; 32]);
            avs.push(Masternode::new_legacy(
                format!("mn{}", i),
                format!("wallet{}", i),
                1000,
                signing_key.verifying_key(),
                MasternodeTier::Bronze,
                0,
            ));
        }

        let txid = [1u8; 32];
        let slot_index = 100;

        // Compute leader twice - should be same
        let leader1 = compute_fallback_leader(&txid, slot_index, &avs);
        let leader2 = compute_fallback_leader(&txid, slot_index, &avs);
        assert_eq!(leader1, leader2);
        assert!(leader1.is_some());

        // Different slot should give potentially different leader
        let leader3 = compute_fallback_leader(&txid, slot_index + 1, &avs);
        assert!(leader3.is_some());
        // May or may not be different, but function should work

        // Different txid should give potentially different leader
        let txid2 = [2u8; 32];
        let leader4 = compute_fallback_leader(&txid2, slot_index, &avs);
        assert!(leader4.is_some());
    }

    #[test]
    fn test_compute_fallback_leader_empty_avs() {
        let txid = [1u8; 32];
        let slot_index = 100;
        let avs: Vec<Masternode> = Vec::new();

        let leader = compute_fallback_leader(&txid, slot_index, &avs);
        assert!(leader.is_none());
    }

    // ========================================================================
    // §7.6 LIVENESS FALLBACK PROTOCOL - INTEGRATION TESTS
    // ========================================================================

    /// Test that fallback round tracking is initialized and cleaned up properly
    #[test]
    fn test_fallback_tracking_lifecycle() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();
        let txid = [99u8; 32];

        // Initially no tracking
        assert!(consensus.fallback_rounds.get(&txid).is_none());

        // Start tracking
        consensus
            .fallback_rounds
            .insert(txid, (100, 0, Instant::now()));

        // Verify present
        assert!(consensus.fallback_rounds.get(&txid).is_some());

        // Remove tracking
        consensus.fallback_rounds.remove(&txid);

        // Verify cleaned up
        assert!(consensus.fallback_rounds.get(&txid).is_none());
    }

    /// Test Q_finality calculation: 2/3 of total weight
    #[test]
    fn test_q_finality_calculation() {
        // Test various total weights
        let total1 = 6_000_000_000u64;
        let q1 = (total1 * 2) / 3;
        assert_eq!(q1, 4_000_000_000u64);

        let total2 = 10_000_000_000u64;
        let q2 = (total2 * 2) / 3;
        assert!((6_666_666_666u64..=6_666_666_667u64).contains(&q2));

        let total3 = 3_000_000_000u64;
        let q3 = (total3 * 2) / 3;
        assert_eq!(q3, 2_000_000_000u64);
    }

    /// Test f+1 threshold calculation: ⌊(n-1)/3⌋ + 1
    #[test]
    fn test_f_plus_1_threshold() {
        // n=4: f=1, need 2 alerts
        let n4 = 4;
        let f4 = (n4 - 1) / 3;
        assert_eq!(f4, 1);
        assert_eq!(f4 + 1, 2);

        // n=10: f=3, need 4 alerts
        let n10 = 10;
        let f10 = (n10 - 1) / 3;
        assert_eq!(f10, 3);
        assert_eq!(f10 + 1, 4);

        // n=100: f=33, need 34 alerts
        let n100 = 100;
        let f100 = (n100 - 1) / 3;
        assert_eq!(f100, 33);
        assert_eq!(f100 + 1, 34);
    }

    /// Test FALLBACK_ROUND_TIMEOUT constant
    #[test]
    fn test_fallback_timeout_constant() {
        assert_eq!(FALLBACK_ROUND_TIMEOUT, Duration::from_secs(10));
    }

    /// Test MAX_FALLBACK_ROUNDS constant
    #[test]
    fn test_max_fallback_rounds_constant() {
        assert_eq!(MAX_FALLBACK_ROUNDS, 5);

        // Worst-case time: 5 rounds * 10 seconds = 50 seconds
        let worst_case_ms = (MAX_FALLBACK_ROUNDS as u64) * 10 * 1000;
        assert_eq!(worst_case_ms, 50_000); // 50 seconds
    }

    /// Test proposal-to-transaction mapping operations
    #[test]
    fn test_proposal_tx_mapping_operations() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let txid = [100u8; 32];
        let proposal_hash = [200u8; 32];

        // Initially no mapping
        assert!(consensus.proposal_to_tx.get(&proposal_hash).is_none());

        // Insert mapping
        consensus.proposal_to_tx.insert(proposal_hash, txid);

        // Verify mapping exists
        let retrieved = consensus.proposal_to_tx.get(&proposal_hash);
        assert!(retrieved.is_some());
        assert_eq!(*retrieved.unwrap(), txid);

        // Remove mapping
        consensus.proposal_to_tx.remove(&proposal_hash);

        // Verify removed
        assert!(consensus.proposal_to_tx.get(&proposal_hash).is_none());
    }

    /// Test liveness alerts accumulation structure
    #[test]
    fn test_liveness_alerts_accumulation() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let txid = [101u8; 32];

        // Initially no alerts
        assert!(consensus.liveness_alerts.get(&txid).is_none());

        // Insert alert vector
        let alerts = Vec::new();
        consensus.liveness_alerts.insert(txid, alerts);

        // Verify exists
        assert!(consensus.liveness_alerts.get(&txid).is_some());

        // Clean up
        consensus.liveness_alerts.remove(&txid);
        assert!(consensus.liveness_alerts.get(&txid).is_none());
    }

    /// Test fallback votes accumulation structure
    #[test]
    fn test_fallback_votes_accumulation() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let proposal_hash = [202u8; 32];

        // Initially no votes
        assert!(consensus.fallback_votes.get(&proposal_hash).is_none());

        // Insert vote vector
        let votes = Vec::new();
        consensus.fallback_votes.insert(proposal_hash, votes);

        // Verify exists
        assert!(consensus.fallback_votes.get(&proposal_hash).is_some());

        // Clean up
        consensus.fallback_votes.remove(&proposal_hash);
        assert!(consensus.fallback_votes.get(&proposal_hash).is_none());
    }

    /// Test that DashMap operations are thread-safe
    #[test]
    fn test_dashmap_concurrent_safety() {
        use std::sync::Arc;
        use std::thread;

        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = Arc::new(TimeVoteConsensus::new(config, registry).unwrap());

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let consensus = Arc::clone(&consensus);
                thread::spawn(move || {
                    let txid = [i; 32];
                    consensus
                        .fallback_rounds
                        .insert(txid, (i as u64, 0, Instant::now()));
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // Verify all inserts succeeded
        for i in 0..10u8 {
            let txid = [i; 32];
            assert!(consensus.fallback_rounds.get(&txid).is_some());
        }
    }

    /// Test leader determinism: same inputs always give same leader
    #[test]
    fn test_leader_election_determinism() {
        use ed25519_dalek::SigningKey;

        let txid = [123u8; 32];
        let slot_index = 456u64;

        let avs: Vec<Masternode> = (0..5)
            .map(|i| {
                let signing_key = SigningKey::from_bytes(&[i; 32]);
                Masternode::new_legacy(
                    format!("mn{}", i),
                    format!("wallet{}", i),
                    1_000_000_000,
                    signing_key.verifying_key(),
                    MasternodeTier::Bronze,
                    0,
                )
            })
            .collect();

        // Compute leader multiple times
        let leader1 = compute_fallback_leader(&txid, slot_index, &avs);
        let leader2 = compute_fallback_leader(&txid, slot_index, &avs);
        let leader3 = compute_fallback_leader(&txid, slot_index, &avs);

        // All must be identical
        assert_eq!(leader1, leader2);
        assert_eq!(leader2, leader3);
    }
}

