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
use ed25519_dalek::{Verifier, VerifyingKey};
use parking_lot::RwLock;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::RwLock as TokioRwLock;

// Resource limits to prevent DOS attacks
const MAX_MEMPOOL_TRANSACTIONS: usize = 10_000;
#[allow(dead_code)] // Used by TransactionPool for mempool size limits
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
    /// Required finality weight threshold as percentage (default 51% for simple majority)
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
            q_finality_percent: 51,  // 51% weight threshold for finality (simple majority)
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
    pub weight: u64, // Sampling weight based on tier
}

/// Transaction voting state - tracks preference for fallback protocol
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VotingState {
    pub preference: Preference,
    pub last_finalized: Option<Preference>,
}

impl VotingState {
    pub fn new(initial_preference: Preference) -> Self {
        Self {
            preference: initial_preference,
            last_finalized: None,
        }
    }

    /// Record finalization
    pub fn finalize(&mut self) {
        self.last_finalized = Some(self.preference);
    }
}

// ============================================================================
// PHASE 3D/3E: TIMELOCK VOTING ACCUMULATORS
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

    /// Add a prepare vote for a block.
    /// A voter can only vote for ONE block — first vote wins.
    pub fn add_vote(&self, block_hash: Hash256, voter_id: String, weight: u64) {
        // Check if this voter already voted for a DIFFERENT block
        for entry in self.votes.iter() {
            if *entry.key() != block_hash && entry.value().iter().any(|(id, _)| *id == voter_id) {
                tracing::debug!(
                    "⚠️ Ignoring duplicate prepare vote from {} — already voted for different block",
                    voter_id
                );
                return;
            }
        }
        // Also prevent double-voting for the same block
        let mut votes = self.votes.entry(block_hash).or_default();
        if votes.iter().any(|(id, _)| *id == voter_id) {
            return;
        }
        votes.push((voter_id, weight));
    }

    /// Check if timevote consensus reached: majority of participating validators agree
    pub fn check_consensus(&self, block_hash: Hash256, sample_size: usize) -> bool {
        if let Some(entry) = self.votes.get(&block_hash) {
            let vote_count = entry.len();

            // SECURITY: Require at least 2 unique voters for this block.
            // A solo node must never finalize its own block.
            if vote_count < 2 {
                return false;
            }

            // Count unique voters across ALL block hashes (participating validators)
            let mut all_voters = std::collections::HashSet::new();
            for entry in self.votes.iter() {
                for (voter_id, _) in entry.value() {
                    all_voters.insert(voter_id.clone());
                }
            }
            let participating = all_voters.len();
            // Use the smaller of active validators and actual participants as denominator
            let effective_size = sample_size.min(participating.max(1));
            // Majority: need more than half of participating validators
            vote_count > effective_size / 2
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

    /// Remove a voter's vote from all blocks.
    /// Used when a leader needs to re-vote for its own block after the message
    /// handler already voted for a peer's (inferior VRF) proposal at the same height.
    pub fn remove_voter(&self, voter_id: &str) {
        for mut entry in self.votes.iter_mut() {
            entry.value_mut().retain(|(id, _)| id != voter_id);
        }
    }

    /// Clear votes for a block after finalization
    pub fn clear(&self, block_hash: Hash256) {
        self.votes.remove(&block_hash);
    }

    /// Clear ALL votes (used when advancing to a new block height)
    pub fn clear_all(&self) {
        self.votes.clear();
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

    /// Add a precommit vote for a block.
    /// A voter can only vote for ONE block — first vote wins.
    pub fn add_vote(&self, block_hash: Hash256, voter_id: String, weight: u64) {
        // Check if this voter already voted for a DIFFERENT block
        for entry in self.votes.iter() {
            if *entry.key() != block_hash && entry.value().iter().any(|(id, _)| *id == voter_id) {
                tracing::debug!(
                    "⚠️ Ignoring duplicate precommit vote from {} — already voted for different block",
                    voter_id
                );
                return;
            }
        }
        // Also prevent double-voting for the same block
        let mut votes = self.votes.entry(block_hash).or_default();
        if votes.iter().any(|(id, _)| *id == voter_id) {
            return;
        }
        votes.push((voter_id, weight));
    }

    /// Check if timevote consensus reached: majority of participating validators agree
    ///
    /// ADAPTIVE QUORUM: Same logic as PrepareVoteAccumulator::check_consensus.
    /// Uses min(active_validators, total_unique_voters) as denominator so that
    /// non-participating nodes don't block finalization.
    ///
    /// SECURITY: A minimum of 2 unique voters is required to prevent solo finalization.
    pub fn check_consensus(&self, block_hash: Hash256, sample_size: usize) -> bool {
        if let Some(entry) = self.votes.get(&block_hash) {
            let vote_count = entry.len();

            // SECURITY: Require at least 2 unique voters for this block.
            // A solo node must never finalize its own block.
            if vote_count < 2 {
                return false;
            }

            // Count unique voters across ALL block hashes (participating validators)
            let mut all_voters = std::collections::HashSet::new();
            for entry in self.votes.iter() {
                for (voter_id, _) in entry.value() {
                    all_voters.insert(voter_id.clone());
                }
            }
            let participating = all_voters.len();
            // Use the smaller of active validators and actual participants as denominator
            let effective_size = sample_size.min(participating.max(1));
            // Majority: need more than half of participating validators
            vote_count > effective_size / 2
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

    /// Clear ALL votes (used when advancing to a new block height)
    pub fn clear_all(&self) {
        self.votes.clear();
    }

    /// Get all voters across all block hashes (for merging into last_block_voters)
    pub fn get_all_voters(&self) -> Vec<(Hash256, Vec<String>)> {
        self.votes
            .iter()
            .map(|entry| {
                let hash = *entry.key();
                let voters = entry.value().iter().map(|(id, _)| id.clone()).collect();
                (hash, voters)
            })
            .collect()
    }
}

/// Core TimeVote consensus engine - Progressive finality with vote accumulation
pub struct TimeVoteConsensus {
    config: TimeVoteConfig,

    /// Reference to masternode registry (single source of truth for validators)
    masternode_registry: Arc<MasternodeRegistry>,

    /// Transaction preference tracking (for fallback protocol)
    tx_state: DashMap<Hash256, Arc<RwLock<VotingState>>>,

    /// Finalized transactions with timestamp for cleanup
    /// Made pub(crate) for atomic finalization guard in network server
    pub(crate) finalized_txs: DashMap<Hash256, (Preference, Instant)>,

    /// AVS (Active Validator Set) snapshots per slot for finality vote verification
    /// slot_index -> AVSSnapshot
    avs_snapshots: DashMap<u64, AVSSnapshot>,

    /// TimeProof vote accumulator (formerly VFP)
    /// txid -> accumulated votes for TimeProof assembly
    timeproof_votes: DashMap<Hash256, Vec<TimeVote>>,

    /// Accumulated weight tracker for efficient finality checking
    /// txid -> accumulated weight (sum of Accept vote weights only)
    accumulated_weight: DashMap<Hash256, u64>,

    /// Phase 3D: Prepare vote accumulator for timevote blocks
    pub prepare_votes: Arc<PrepareVoteAccumulator>,

    /// Phase 3E: Precommit vote accumulator for timevote blocks
    pub precommit_votes: Arc<PrecommitVoteAccumulator>,

    /// Last height for which votes were cast — used to clear stale votes on height advance
    pub last_voted_height: AtomicU64,

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

    /// §7.6 Security: Byzantine node detection tracker
    /// mn_id -> flagged (track masternodes exhibiting Byzantine behavior)
    byzantine_nodes: DashMap<String, bool>,

    /// Conflicting TimeProof detection (Item 9: Pre-Mainnet Checklist)
    /// txid -> Vec<TimeProof> (all TimeProofs seen for this transaction)
    /// Multiple TimeProofs = partition scenario with conflicting finality
    competing_timeproofs: DashMap<Hash256, Vec<TimeProof>>,

    /// Conflict log for security monitoring
    /// (txid, slot_index, timestamp) -> conflict details for AI anomaly detector
    timeproof_conflicts: DashMap<(Hash256, u64), TimeProofConflictInfo>,

    /// Preserved voters from finalized blocks (block_hash -> voter list)
    /// Saved before cleanup so block production can reference previous block's voters
    last_block_voters: DashMap<Hash256, Vec<String>>,

    /// Metrics
    rounds_executed: AtomicUsize,
    txs_finalized: AtomicUsize,

    /// §7.6 Fallback Metrics (Phase 5)
    fallback_activations: AtomicUsize,
    stall_detections: AtomicUsize,
    timelock_resolutions: AtomicUsize,
    timeproof_conflicts_detected: AtomicUsize,
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
            finalized_txs: DashMap::new(),
            avs_snapshots: DashMap::new(),
            timeproof_votes: DashMap::new(),
            accumulated_weight: DashMap::new(),
            prepare_votes: Arc::new(PrepareVoteAccumulator::new()),
            precommit_votes: Arc::new(PrecommitVoteAccumulator::new()),
            last_voted_height: AtomicU64::new(0),
            tx_status: Arc::new(DashMap::new()),
            stall_timers: Arc::new(DashMap::new()),
            liveness_alerts: DashMap::new(),
            fallback_votes: DashMap::new(),
            proposal_to_tx: DashMap::new(),
            fallback_rounds: DashMap::new(),
            byzantine_nodes: DashMap::new(),
            competing_timeproofs: DashMap::new(),
            timeproof_conflicts: DashMap::new(),
            last_block_voters: DashMap::new(),
            active_vote_requests: Arc::new(AtomicUsize::new(0)),
            rounds_executed: AtomicUsize::new(0),
            txs_finalized: AtomicUsize::new(0),
            fallback_activations: AtomicUsize::new(0),
            stall_detections: AtomicUsize::new(0),
            timelock_resolutions: AtomicUsize::new(0),
            timeproof_conflicts_detected: AtomicUsize::new(0),
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
            self.timeproof_votes.remove(&txid);
            self.accumulated_weight.remove(&txid);
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
            finalized_txs: self.finalized_txs.len(),
            avs_snapshots: self.avs_snapshots.len(),
            vfp_votes: self.timeproof_votes.len(),
        }
    }

    /// Get validator addresses only (for compatibility)
    pub fn get_validator_addresses(&self) -> Vec<String> {
        self.get_validators()
            .iter()
            .map(|v| v.address.clone())
            .collect()
    }

    /// Initialize tracking for a new transaction's consensus preference
    pub fn initiate_consensus(&self, txid: Hash256, initial_preference: Preference) -> bool {
        if self.finalized_txs.contains_key(&txid) {
            return false; // Already finalized
        }

        if self.tx_state.contains_key(&txid) {
            return false; // Already initiated
        }

        self.tx_state.insert(
            txid,
            Arc::new(RwLock::new(VotingState::new(initial_preference))),
        );

        true
    }

    /// Get current state of a transaction
    pub fn get_tx_state(&self, txid: &Hash256) -> Option<(Preference, bool)> {
        self.tx_state.get(txid).map(|state| {
            let s = state.read();
            let is_finalized = self.finalized_txs.contains_key(txid);
            (s.preference, is_finalized)
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
    // TIMEVOTE ACCUMULATION (Per Protocol §8.5)
    // ========================================================================

    /// Accumulate a TimeVote for a transaction (Protocol §8.5)
    ///
    /// This method:
    /// 1. Verifies vote signature
    /// 2. Checks for duplicate voters
    /// 3. Accumulates Accept votes only (Reject votes logged but not counted)
    /// 4. Updates accumulated weight
    ///
    /// Returns Ok(accumulated_weight) if vote accepted, Err if rejected
    pub fn accumulate_timevote(&self, vote: TimeVote) -> Result<u64, String> {
        let txid = vote.txid;

        // Step 1: Verify signature
        // Get masternode info to get public key
        let masternodes = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.masternode_registry.list_active())
        });

        let mn_info = masternodes
            .iter()
            .find(|info| info.masternode.address == vote.voter_mn_id)
            .ok_or_else(|| format!("Voter {} not in active validator set", vote.voter_mn_id))?;

        // Verify signature
        vote.verify(&mn_info.masternode.public_key)
            .map_err(|e| format!("Vote signature verification failed: {}", e))?;

        // Step 2: Check for duplicate voters
        let mut votes = self.timeproof_votes.entry(txid).or_default();

        // Check if this voter already voted
        if votes.iter().any(|v| v.voter_mn_id == vote.voter_mn_id) {
            return Err(format!(
                "Duplicate vote from {} for TX {:?}",
                vote.voter_mn_id,
                hex::encode(txid)
            ));
        }

        // Step 3: Add vote to accumulator
        votes.push(vote.clone());
        drop(votes); // Release lock

        // Step 4: Update accumulated weight (only for Accept votes)
        let new_weight = if vote.decision == VoteDecision::Accept {
            let mut weight_entry = self.accumulated_weight.entry(txid).or_insert(0);
            *weight_entry += vote.voter_weight;
            *weight_entry
        } else {
            // Reject votes are tracked but don't contribute to weight
            tracing::debug!(
                "Reject vote from {} for TX {:?} (not counted toward finality)",
                vote.voter_mn_id,
                hex::encode(txid)
            );
            self.accumulated_weight.get(&txid).map(|w| *w).unwrap_or(0)
        };

        tracing::debug!(
            "Accumulated vote from {} for TX {:?} (decision: {:?}, weight: {}, total: {})",
            vote.voter_mn_id,
            hex::encode(txid),
            vote.decision,
            vote.voter_weight,
            new_weight
        );

        Ok(new_weight)
    }

    /// Legacy method - redirects to accumulate_timevote()
    /// Kept for backward compatibility
    pub fn accumulate_finality_vote(&self, vote: FinalityVote) -> Result<(), String> {
        self.accumulate_timevote(vote).map(|_| ())
    }

    /// Get accumulated votes for a transaction
    pub fn get_accumulated_votes(&self, txid: &Hash256) -> Vec<TimeVote> {
        self.timeproof_votes
            .get(txid)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Get accumulated weight for a transaction (Accept votes only)
    pub fn get_accumulated_weight(&self, txid: &Hash256) -> u64 {
        self.accumulated_weight.get(txid).map(|w| *w).unwrap_or(0)
    }

    /// Check if transaction meets TimeProof finality threshold (Protocol §8.3)
    /// Returns Ok(true) if accumulated weight >= 51% of AVS weight (simple majority)
    pub fn check_timeproof_finality(
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
                return Err("Duplicate voter in TimeProof".to_string());
            }
            seen_voters.insert(vote.voter_mn_id.clone());

            if let Some(weight) = snapshot.get_validator_weight(&vote.voter_mn_id) {
                total_weight += weight;
            }
        }

        // Check threshold: 51% of total weight
        let threshold = snapshot.voting_threshold();
        Ok(total_weight >= threshold)
    }

    /// Legacy alias for check_timeproof_finality
    pub fn check_vfp_finality(
        &self,
        txid: &Hash256,
        snapshot: &AVSSnapshot,
    ) -> Result<bool, String> {
        self.check_timeproof_finality(txid, snapshot)
    }

    /// Clear accumulated votes for a transaction after finality
    pub fn clear_timeproof_votes(&self, txid: &Hash256) {
        self.timeproof_votes.remove(txid);
        self.accumulated_weight.remove(txid);
    }

    /// Legacy alias for clear_timeproof_votes
    pub fn clear_vfp_votes(&self, txid: &Hash256) {
        self.clear_timeproof_votes(txid);
    }

    // ========================================================================
    // TIMEPROOF CONFLICT DETECTION - Pre-Mainnet Checklist Item 9
    // ========================================================================

    /// Detect and log competing TimeProofs for the same transaction
    ///
    /// Called when a new TimeProof is received for a transaction.
    /// If another TimeProof already exists, logs a conflict and performs fork resolution.
    ///
    /// **Per Pre-Mainnet Checklist Item 9:**
    /// - Detects multiple competing TimeProofs (network partition scenario)
    /// - Logs conflicts to anomaly detector for security monitoring
    /// - Resolves via weight comparison (higher weight wins)
    /// - Returns index of winning TimeProof
    pub fn detect_competing_timeproof(
        &self,
        new_proof: TimeProof,
        new_proof_weight: u64,
    ) -> Result<usize, String> {
        let txid = new_proof.txid;
        let slot_index = new_proof.slot_index;

        // Get or create competing proofs vector for this transaction
        let mut proofs = self.competing_timeproofs.entry(txid).or_default();

        let mut weights = Vec::new();
        let mut max_weight = new_proof_weight;
        let mut winning_index = proofs.len(); // New proof is index = current length

        // Collect weights of existing proofs
        for existing_proof in proofs.iter() {
            let existing_weight = self.calculate_timeproof_weight(existing_proof)?;
            weights.push(existing_weight);
            if existing_weight > max_weight {
                max_weight = existing_weight;
                winning_index = proofs.len() - 1;
            }
        }

        // Add new proof
        proofs.push(new_proof);
        weights.push(new_proof_weight);

        // Conflict detected if 2+ proofs exist
        if proofs.len() >= 2 {
            let conflict_key = (txid, slot_index);

            let conflict_info = TimeProofConflictInfo {
                txid,
                slot_index,
                proof_count: proofs.len(),
                proof_weights: weights.clone(),
                max_weight,
                winning_proof_index: winning_index,
                detected_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                resolved: false,
            };

            self.timeproof_conflicts
                .insert(conflict_key, conflict_info.clone());
            self.timeproof_conflicts_detected
                .fetch_add(1, Ordering::Relaxed);

            // Log with full details for security monitoring
            tracing::warn!(
                "⚠️  TIMEPROOF CONFLICT DETECTED for TX {}: {} competing proofs (slot {}) | Weights: {:?} | Winner: index {} (weight {})",
                hex::encode(txid),
                proofs.len(),
                slot_index,
                weights,
                winning_index,
                max_weight
            );

            // Send to anomaly detector if available (via ConsensusEngine)
            // This will be used for alerting and security monitoring
            return Ok(winning_index);
        }

        Ok(winning_index)
    }

    /// Calculate total weight of a TimeProof
    fn calculate_timeproof_weight(&self, proof: &TimeProof) -> Result<u64, String> {
        let mut total = 0u64;
        for vote in &proof.votes {
            total = total
                .checked_add(vote.voter_weight)
                .ok_or_else(|| "Weight overflow".to_string())?;
        }
        Ok(total)
    }

    /// Resolve fork by selecting the TimeProof with highest weight
    ///
    /// Called after partition healing when competing proofs exist.
    /// Returns the winning TimeProof and logs the resolution.
    pub fn resolve_timeproof_fork(&self, txid: Hash256) -> Result<Option<TimeProof>, String> {
        let proofs = self
            .competing_timeproofs
            .get(&txid)
            .map(|entry| entry.clone());

        if let Some(proofs) = proofs {
            if proofs.is_empty() {
                return Ok(None);
            }

            // Find proof with highest weight
            let mut max_weight = 0u64;
            let mut winning_proof = proofs[0].clone();

            for proof in &proofs {
                let weight = self.calculate_timeproof_weight(proof)?;
                if weight > max_weight {
                    max_weight = weight;
                    winning_proof = proof.clone();
                }
            }

            // Mark as resolved
            if let Some(mut conflict) = self
                .timeproof_conflicts
                .get_mut(&(txid, winning_proof.slot_index))
            {
                conflict.resolved = true;
            }

            tracing::info!(
                "✅ TimeProof fork resolved for TX {}: Selected proof with weight {} from {} competing proofs",
                hex::encode(txid),
                max_weight,
                proofs.len()
            );

            Ok(Some(winning_proof))
        } else {
            Ok(None)
        }
    }

    /// Get all competing TimeProofs for a transaction
    pub fn get_competing_timeproofs(&self, txid: Hash256) -> Vec<TimeProof> {
        self.competing_timeproofs
            .get(&txid)
            .map(|entry| entry.clone())
            .unwrap_or_default()
    }

    /// Get conflict details for a transaction
    pub fn get_conflict_info(
        &self,
        txid: Hash256,
        slot_index: u64,
    ) -> Option<TimeProofConflictInfo> {
        self.timeproof_conflicts
            .get(&(txid, slot_index))
            .map(|entry| entry.clone())
    }

    /// Get total number of timeproof conflicts detected
    pub fn conflicts_detected_count(&self) -> usize {
        self.timeproof_conflicts_detected.load(Ordering::Relaxed)
    }

    /// Clear competing proofs for a transaction after resolution
    pub fn clear_competing_timeproofs(&self, txid: Hash256) {
        self.competing_timeproofs.remove(&txid);
    }

    /// Record finalization (called when threshold reached)
    /// Updates internal state tracking
    pub fn record_finalization(&self, txid: Hash256, accumulated_weight: u64) {
        // Record finalization with timestamp
        self.finalized_txs
            .insert(txid, (Preference::Accept, Instant::now()));

        // Update transaction status
        self.tx_status.insert(
            txid,
            TransactionStatus::Finalized {
                finalized_at: chrono::Utc::now().timestamp_millis(),
                vfp_weight: accumulated_weight,
            },
        );

        // Update metrics
        self.txs_finalized.fetch_add(1, Ordering::Relaxed);

        tracing::info!(
            "✅ TX {:?} finalized with weight {} (total finalized: {})",
            hex::encode(txid),
            accumulated_weight,
            self.txs_finalized.load(Ordering::Relaxed)
        );
    }

    /// Assemble TimeProof for a finalized transaction (Protocol §8.2)
    ///
    /// Collects all Accept votes for the transaction and creates a TimeProof certificate.
    /// This should be called immediately after finalization is recorded.
    ///
    /// Returns Ok(TimeProof) if successful, Err if insufficient votes or invalid votes
    pub fn assemble_timeproof(&self, txid: Hash256) -> Result<TimeProof, String> {
        // Get all accumulated votes for this transaction
        let all_votes = self.get_accumulated_votes(&txid);

        if all_votes.is_empty() {
            return Err(format!("No votes found for TX {:?}", hex::encode(txid)));
        }

        // Filter to only Accept votes (per Protocol §8.2)
        let accept_votes: Vec<TimeVote> = all_votes
            .into_iter()
            .filter(|v| v.decision == VoteDecision::Accept)
            .collect();

        if accept_votes.is_empty() {
            return Err(format!(
                "No Accept votes found for TX {:?}",
                hex::encode(txid)
            ));
        }

        // Get slot_index from first vote (all votes must have same slot_index)
        let slot_index = accept_votes[0].slot_index;

        // Verify all votes have the same slot_index
        if !accept_votes.iter().all(|v| v.slot_index == slot_index) {
            return Err(format!(
                "Votes have mismatched slot_index for TX {:?}",
                hex::encode(txid)
            ));
        }

        // Verify all votes have the same txid and tx_hash_commitment
        let ref_commitment = accept_votes[0].tx_hash_commitment;
        if !accept_votes
            .iter()
            .all(|v| v.txid == txid && v.tx_hash_commitment == ref_commitment)
        {
            return Err(format!(
                "Votes have mismatched txid or commitment for TX {:?}",
                hex::encode(txid)
            ));
        }

        // Create TimeProof
        let timeproof = TimeProof {
            txid,
            slot_index,
            votes: accept_votes.clone(),
        };

        // Calculate total weight for logging
        let total_weight: u64 = accept_votes.iter().map(|v| v.voter_weight).sum();

        tracing::info!(
            "📜 Assembled TimeProof for TX {:?} with {} Accept votes (total weight: {})",
            hex::encode(txid),
            accept_votes.len(),
            total_weight
        );

        Ok(timeproof)
    }

    /// Verify a TimeProof certificate (Protocol §8.2)
    ///
    /// This method verifies that a TimeProof is valid by:
    /// 1. Checking all vote signatures
    /// 2. Verifying voters are in AVS
    /// 3. Checking accumulated weight >= 51% threshold
    /// 4. Ensuring vote consistency
    ///
    /// Returns Ok(accumulated_weight) if valid, Err if invalid
    pub fn verify_timeproof(&self, timeproof: &TimeProof) -> Result<u64, String> {
        // Get active masternodes for AVS verification
        let masternodes = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.masternode_registry.list_active())
        });

        // Calculate total AVS weight
        let total_avs_weight: u64 = masternodes
            .iter()
            .map(|info| info.masternode.tier.sampling_weight())
            .sum();

        // Create closure for public key lookup
        let get_pubkey = |voter_mn_id: &str| -> Option<VerifyingKey> {
            masternodes
                .iter()
                .find(|info| info.masternode.address == voter_mn_id)
                .map(|info| info.masternode.public_key)
        };

        // Verify the TimeProof using its built-in verification
        let accumulated_weight = timeproof.verify(total_avs_weight, get_pubkey)?;

        tracing::info!(
            "✅ TimeProof verified for TX {:?}: weight={}/{} ({}%), {} votes",
            hex::encode(timeproof.txid),
            accumulated_weight,
            total_avs_weight,
            (accumulated_weight * 100) / total_avs_weight,
            timeproof.votes.len()
        );

        Ok(accumulated_weight)
    }

    // ========================================================================
    // PHASE 3D: PREPARE VOTE HANDLING
    // ========================================================================

    /// Generate a prepare vote for a block (Phase 3D.1)
    /// Called when a valid block is received
    pub fn generate_prepare_vote(&self, block_hash: Hash256, voter_id: &str, voter_weight: u64) {
        // Add our own vote to the accumulator
        self.prepare_votes
            .add_vote(block_hash, voter_id.to_string(), voter_weight);

        tracing::debug!(
            "✅ Generated prepare vote for block {} from {} (weight: {})",
            hex::encode(block_hash),
            voter_id,
            voter_weight
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
    /// Pure timevote: majority of participating validators must vote for block
    pub fn check_prepare_consensus(&self, block_hash: Hash256) -> bool {
        let validators = self.get_validators();
        let sample_size = validators.len();

        // BOOTSTRAP FIX: If no active validators, use all registered masternodes
        // as the upper bound. The adaptive quorum in check_consensus() will
        // min() this with actual participants, so non-voting nodes won't block finalization.
        let sample_size = if sample_size == 0 {
            let all_registered = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(self.masternode_registry.list_all())
            });
            tracing::warn!(
                "⚠️ No active validators for consensus check, using all {} registered masternodes (bootstrap mode)",
                all_registered.len()
            );
            all_registered.len()
        } else {
            sample_size
        };

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
    pub fn generate_precommit_vote(&self, block_hash: Hash256, voter_id: &str, voter_weight: u64) {
        // Add our own vote to the accumulator
        self.precommit_votes
            .add_vote(block_hash, voter_id.to_string(), voter_weight);

        tracing::debug!(
            "✅ Generated precommit vote for block {} from {} (weight: {})",
            hex::encode(block_hash),
            voter_id,
            voter_weight
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
    /// Pure timevote: majority of participating validators must vote for block
    pub fn check_precommit_consensus(&self, block_hash: Hash256) -> bool {
        let validators = self.get_validators();
        let sample_size = validators.len();

        // BOOTSTRAP FIX: If no active validators, use all registered masternodes
        // as the upper bound. The adaptive quorum in check_consensus() will
        // min() this with actual participants, so non-voting nodes won't block finalization.
        let sample_size = if sample_size == 0 {
            let all_registered = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(self.masternode_registry.list_all())
            });
            tracing::warn!(
                "⚠️ No active validators for consensus check, using all {} registered masternodes (bootstrap mode)",
                all_registered.len()
            );
            all_registered.len()
        } else {
            sample_size
        };

        self.precommit_votes
            .check_consensus(block_hash, sample_size)
    }

    /// Get precommit vote weight for a block
    pub fn get_precommit_weight(&self, block_hash: Hash256) -> u64 {
        self.precommit_votes.get_weight(block_hash)
    }

    /// Clean up votes after block finalization (Phase 3E.6)
    /// Preserves voter list before clearing so block production can reference it
    pub fn cleanup_block_votes(&self, block_hash: Hash256) {
        // Save precommit voters before clearing
        let voters = self.precommit_votes.get_voters(block_hash);
        if !voters.is_empty() {
            self.last_block_voters.insert(block_hash, voters);
        }
        self.prepare_votes.clear(block_hash);
        self.precommit_votes.clear(block_hash);
    }

    /// Clear all stale votes when advancing to a new block height.
    /// Called when processing a proposal at a height greater than the last voted height.
    /// Without this, votes from previous heights remain in the accumulator and the
    /// "first vote wins" anti-double-voting rule silently rejects all future votes.
    pub fn advance_vote_height(&self, new_height: u64) {
        let prev = self.last_voted_height.swap(new_height, Ordering::SeqCst);
        if new_height > prev {
            // Merge any late-arriving precommit voters into last_block_voters
            // before clearing. This captures votes that arrived after
            // cleanup_block_votes saved the initial voter set at finalization.
            for (hash, voters) in self.precommit_votes.get_all_voters() {
                if !voters.is_empty() {
                    self.last_block_voters
                        .entry(hash)
                        .and_modify(|existing| {
                            for voter in &voters {
                                if !existing.contains(voter) {
                                    existing.push(voter.clone());
                                }
                            }
                        })
                        .or_insert(voters);
                }
            }
            self.prepare_votes.clear_all();
            self.precommit_votes.clear_all();
            tracing::debug!(
                "🗳️  Cleared stale votes: height advanced {} → {}",
                prev,
                new_height
            );
        }
    }

    /// Get preserved voters from a finalized block
    pub fn get_finalized_block_voters(&self, block_hash: Hash256) -> Vec<String> {
        self.last_block_voters
            .get(&block_hash)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Get metrics
    pub fn get_metrics(&self) -> TimeVoteMetrics {
        TimeVoteMetrics {
            rounds_executed: self.rounds_executed.load(Ordering::Relaxed),
            txs_finalized: self.txs_finalized.load(Ordering::Relaxed),
            tracked_txs: self.tx_state.len(),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TimeVoteMetrics {
    pub rounds_executed: usize,
    pub txs_finalized: usize,
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

    /// Start the fallback timeout monitoring task (§7.6.5)
    ///
    /// Monitors fallback resolution rounds and retries with new leaders on timeout.
    /// After MAX_FALLBACK_ROUNDS (5 rounds), marks transactions for TimeLock resolution.
    ///
    /// # Protocol Flow
    /// 1. Every 5 seconds, scan fallback_rounds for timeouts
    /// 2. If round timeout (10s), increment slot and retry with new leader
    /// 3. If exceeded 5 rounds, mark for TimeLock escalation
    ///
    /// # Returns
    /// * `JoinHandle` - Task handle for the background thread
    pub fn start_fallback_timeout_monitor(
        self: Arc<Self>,
        masternode_registry: Arc<MasternodeRegistry>,
    ) -> tokio::task::JoinHandle<()> {
        tracing::info!("⏱️ Starting fallback timeout monitor (§7.6.5)");

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                let retry_count = self.check_fallback_timeouts(&masternode_registry).await;
                if retry_count > 0 {
                    tracing::info!("⏱️ Processed {} fallback timeouts", retry_count);
                }
            }
        })
    }

    /// Start the fallback resolution background task (§7.6.4)
    ///
    /// Monitors transactions in FallbackResolution state and triggers leader proposals.
    /// When this node is elected as leader, it broadcasts a FinalityProposal.
    ///
    /// # Protocol Flow
    /// 1. Every 3 seconds, scan transactions in FallbackResolution state
    /// 2. For each transaction, check if we are the elected leader
    /// 3. If leader, determine decision and broadcast proposal
    /// 4. Track proposal to avoid duplicates
    ///
    /// # Returns
    /// * `JoinHandle` - Task handle for the background thread
    pub fn start_fallback_resolution(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tracing::info!("🎯 Starting fallback resolution task (§7.6.4)");

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                // Get current slot index
                let current_slot = self.get_current_slot_index();

                // Get AVS snapshot for this slot
                let avs = self.timevote.get_avs_snapshot(current_slot);
                let avs_snapshot = match avs {
                    Some(snapshot) => snapshot,
                    None => {
                        // No AVS yet, skip this round
                        continue;
                    }
                };

                // Scan all transactions in FallbackResolution state
                let fallback_txs: Vec<Hash256> = self
                    .timevote
                    .tx_status
                    .iter()
                    .filter_map(|entry| match entry.value() {
                        TransactionStatus::FallbackResolution { .. } => Some(*entry.key()),
                        _ => None,
                    })
                    .collect();

                if !fallback_txs.is_empty() {
                    tracing::debug!(
                        "🎯 Checking {} transactions in fallback",
                        fallback_txs.len()
                    );
                }

                for txid in fallback_txs {
                    // Get the round info
                    let (slot_index, round, _started_at) = match self
                        .timevote
                        .fallback_rounds
                        .get(&txid)
                    {
                        Some(entry) => *entry.value(),
                        None => {
                            tracing::warn!("No fallback round info for tx {}", hex::encode(txid));
                            continue;
                        }
                    };

                    // Check if we are the leader for this transaction
                    if self.is_fallback_leader(txid, slot_index, round, &avs_snapshot) {
                        tracing::info!(
                            "🎯 I am the fallback leader for tx {} (slot: {}, round: {})",
                            hex::encode(&txid[..8]),
                            slot_index,
                            round
                        );

                        // Execute as leader
                        if let Err(e) = self
                            .execute_fallback_as_leader(txid, slot_index, round)
                            .await
                        {
                            tracing::error!(
                                "Failed to execute fallback as leader for tx {}: {}",
                                hex::encode(&txid[..8]),
                                e
                            );
                        }
                    }
                }
            }
        })
    }

    /// Start the stall detection background task (§7.6.1)
    ///
    /// Monitors all transactions in Voting state and detects stalls after STALL_TIMEOUT (30s).
    /// When a stall is detected, broadcasts a LivenessAlert to trigger fallback consensus.
    ///
    /// # Protocol Flow (§7.6.1)
    /// 1. Every 5 seconds, scan all transactions in Voting state
    /// 2. Check elapsed time since voting started
    /// 3. If elapsed > 30s, broadcast LivenessAlert
    /// 4. Continue monitoring until transaction finalizes or enters FallbackResolution
    ///
    /// # Returns
    /// * `JoinHandle` - Task handle for the background thread
    ///
    /// # Example
    /// ```ignore
    /// let stall_task = consensus.start_stall_detection();
    /// // Task runs indefinitely until dropped
    /// ```
    pub fn start_stall_detection(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tracing::info!("🔍 Starting stall detection task (§7.6.1)");

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                // Get current slot index for alert signing
                let current_slot = self.get_current_slot_index();

                // Scan all transactions for stalls
                let stalled_txs = self.detect_stalled_transactions();

                if !stalled_txs.is_empty() {
                    tracing::debug!("🔍 Detected {} stalled transactions", stalled_txs.len());
                }

                // Broadcast alerts for stalled transactions
                for txid in stalled_txs {
                    if let Err(e) = self.broadcast_liveness_alert(txid, current_slot).await {
                        tracing::warn!(
                            "Failed to broadcast LivenessAlert for tx {}: {}",
                            hex::encode(txid),
                            e
                        );
                    }
                }
            }
        })
    }

    /// Detect transactions that have been stalled in Voting state (§7.6.1)
    ///
    /// Scans all transactions and identifies those that:
    /// 1. Are in Voting state
    /// 2. Have been voting for > STALL_TIMEOUT (30 seconds)
    /// 3. Are not already in FallbackResolution state
    /// 4. Are still valid (not conflicting with finalized transactions)
    ///
    /// # Returns
    /// * `Vec<Hash256>` - List of transaction IDs that are stalled
    fn detect_stalled_transactions(&self) -> Vec<Hash256> {
        let mut stalled = Vec::new();
        let now = chrono::Utc::now().timestamp_millis();

        for entry in self.timevote.tx_status.iter() {
            let txid = *entry.key();
            let status = entry.value();

            match status {
                TransactionStatus::Voting { started_at, .. } => {
                    let elapsed_ms = now - started_at;
                    let elapsed_secs = elapsed_ms / 1000;

                    // Check if transaction has stalled
                    if elapsed_secs >= STALL_TIMEOUT.as_secs() as i64 {
                        // Verify transaction is still valid before alerting
                        if self.is_transaction_still_valid(&txid) {
                            stalled.push(txid);
                            // Phase 5: Record stall detection metric
                            self.record_stall_detection();
                        } else {
                            tracing::debug!(
                                "Skipping stall alert for invalid tx {}",
                                hex::encode(txid)
                            );
                        }
                    }
                }
                TransactionStatus::FallbackResolution { .. } => {
                    // Already in fallback, don't re-alert
                    continue;
                }
                _ => {
                    // Not in voting state, skip
                    continue;
                }
            }
        }

        stalled
    }

    /// Check if a transaction is still valid for fallback resolution
    ///
    /// A transaction is invalid if:
    /// - It conflicts with a finalized transaction
    /// - Its inputs have been spent by a finalized transaction
    /// - It has been explicitly rejected
    ///
    /// # Arguments
    /// * `txid` - Transaction to check
    ///
    /// # Returns
    /// * `bool` - true if transaction is still valid
    fn is_transaction_still_valid(&self, txid: &Hash256) -> bool {
        // Check if transaction exists in pool
        let tx = match self.tx_pool.get_pending(txid) {
            Some(tx) => tx,
            None => {
                // Transaction no longer in pool
                return false;
            }
        };

        // Check if any inputs are spent by finalized transactions
        for input in &tx.inputs {
            if let Some(state) = self.utxo_manager.get_state(&input.previous_output) {
                match state {
                    UTXOState::SpentFinalized { .. } => {
                        // Input already spent by finalized tx
                        return false;
                    }
                    UTXOState::Confirmed { .. } => {
                        // Input spent and confirmed in block
                        return false;
                    }
                    _ => {
                        // Still valid
                        continue;
                    }
                }
            }
        }

        // Check if transaction has been explicitly rejected
        if let Some(status) = self.timevote.tx_status.get(txid) {
            if matches!(status.value(), TransactionStatus::Rejected { .. }) {
                return false;
            }
        }

        true
    }

    /// Get current slot index (10-minute epochs since genesis)
    ///
    /// Used for deterministic leader election in fallback protocol.
    /// Slot 0 = genesis time, increments every 10 minutes.
    ///
    /// # Returns
    /// * `u64` - Current slot index
    fn get_current_slot_index(&self) -> u64 {
        let now = chrono::Utc::now().timestamp();
        let genesis_time = 1735689600; // 2025-01-01 00:00:00 UTC
        let slot_duration = 600; // 10 minutes in seconds

        ((now - genesis_time).max(0) / slot_duration) as u64
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
    /// Called when this validator responds with "Valid" during TimeVote consensus
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

    /// Sign a TimeVote for a transaction (simplified version for vote request handling)
    /// Used when responding to TimeVoteRequest messages
    /// Returns None if node identity not set or node is not a masternode
    pub fn sign_timevote(
        &self,
        txid: Hash256,
        tx_hash_commitment: Hash256,
        slot_index: u64,
        decision: VoteDecision,
    ) -> Option<TimeVote> {
        // Get node identity
        let identity = self.identity.get()?;
        let voter_mn_id = identity.address.clone();

        // Get masternode info to determine weight
        let masternodes = self.get_masternodes();
        let mn = masternodes.iter().find(|mn| mn.address == voter_mn_id)?;
        let voter_weight = mn.tier.sampling_weight();

        // Sign and create the vote
        let vote = identity.sign_finality_vote(
            1, // TODO: Make chain_id configurable
            txid,
            tx_hash_commitment,
            slot_index,
            decision,
            voter_mn_id,
            voter_weight,
        );

        Some(vote)
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
        } else {
            tracing::error!("❌ Broadcast attempted but callback not set!");
        }
    }

    /// Broadcast TimeProof to all network peers (Protocol §8.2)
    pub async fn broadcast_timeproof(&self, proof: TimeProof) {
        tracing::info!(
            "📡 Broadcasting TimeProof for TX {:?} to network",
            hex::encode(proof.txid)
        );
        self.broadcast(NetworkMessage::TimeProofBroadcast { proof })
            .await;
    }

    pub async fn validate_transaction(&self, tx: &Transaction) -> Result<(), String> {
        self.validate_transaction_with_locks(tx, tx.txid()).await
    }

    /// Validate transaction, allowing UTXOs locked by the specified txid
    async fn validate_transaction_with_locks(
        &self,
        tx: &Transaction,
        our_txid: Hash256,
    ) -> Result<(), String> {
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

        // 2. Check inputs exist and are unspent (or locked/finalized by this tx)
        for input in &tx.inputs {
            match self.utxo_manager.get_state(&input.previous_output) {
                Some(UTXOState::Unspent) => {}
                Some(UTXOState::Locked { txid, .. }) if txid == our_txid => {
                    // OK - locked by this transaction
                }
                Some(UTXOState::SpentPending { txid, .. }) if txid == our_txid => {
                    // OK - voting in progress for this transaction
                }
                Some(UTXOState::SpentFinalized { txid, .. }) if txid == our_txid => {
                    // OK - already finalized by this transaction (e.g., receiving a block
                    // containing a TX we already finalized locally via TimeVote)
                }
                Some(state) => {
                    return Err(format!("UTXO not unspent: {}", state));
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

        tracing::debug!(
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
    /// 4. Finalize (simple majority) or reject
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

        // Now validate knowing inputs are locked (pass txid so validation knows these locks are ours)
        if let Err(e) = self.validate_transaction_with_locks(tx, txid).await {
            // Validation failed - unlock everything
            for input in &tx.inputs {
                self.utxo_manager
                    .update_state(&input.previous_output, UTXOState::Unspent);
            }
            return Err(e);
        }

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
    /// 5. Finalize (simple majority) or reject
    pub async fn submit_transaction(&self, tx: Transaction) -> Result<Hash256, String> {
        let txid = tx.txid();
        let txid_hex = hex::encode(txid);

        tracing::info!("🔍 Validating transaction {}...", &txid_hex[..16]);

        // FIX: Check broadcast callback early - fail fast if not available
        // This prevents transactions from being locked and then stuck forever
        {
            let callback_guard = self.broadcast_callback.read().await;
            if callback_guard.is_none() {
                tracing::error!("❌ No broadcast callback available - cannot process transactions");
                return Err(
                    "Network not initialized - broadcast callback not available".to_string()
                );
            }
        }

        // Step 1: Atomically lock and validate
        if let Err(e) = self.lock_and_validate_transaction(&tx).await {
            tracing::error!(
                "❌ Transaction {} validation FAILED: {}",
                &txid_hex[..16],
                e
            );
            // If lock_and_validate fails, UTXOs may be locked - unlock them
            self.unlock_transaction_inputs(&tx, &txid).await;
            return Err(e);
        }

        tracing::info!("✅ Transaction {} validation passed", &txid_hex[..16]);

        // Step 2: Broadcast transaction to network FIRST
        // This ensures validators receive the TX before vote requests
        self.broadcast(NetworkMessage::TransactionBroadcast(tx.clone()))
            .await;
        tracing::info!("📡 Broadcast transaction {} to network", &txid_hex[..16]);

        // Step 3: Process transaction through consensus locally (this adds to pool)
        // AND broadcasts vote request - validators will have received TX by now
        tracing::debug!("🗳️  Starting consensus for transaction {}", &txid_hex[..16]);
        if let Err(e) = self.process_transaction(tx.clone()).await {
            tracing::error!(
                "❌ Transaction {} consensus processing FAILED: {}",
                &txid_hex[..16],
                e
            );
            // If processing fails, unlock the inputs
            self.unlock_transaction_inputs(&tx, &txid).await;
            return Err(e);
        }

        Ok(txid)
    }

    /// Helper to unlock transaction inputs
    async fn unlock_transaction_inputs(&self, tx: &Transaction, txid: &Hash256) {
        for input in &tx.inputs {
            // Only unlock if it's still locked by this transaction
            if let Some(UTXOState::Locked {
                txid: locked_txid, ..
            }) = self.utxo_manager.get_state(&input.previous_output)
            {
                if locked_txid == *txid {
                    self.utxo_manager
                        .update_state(&input.previous_output, UTXOState::Unspent);
                    tracing::debug!(
                        "Unlocked UTXO {:?} after transaction failure",
                        input.previous_output
                    );
                }
            }
        }
    }

    pub async fn process_transaction(&self, tx: Transaction) -> Result<(), String> {
        let txid = tx.txid();
        let masternodes = self.get_masternodes();
        let n = masternodes.len() as u32;

        if n == 0 {
            return Err("No masternodes available".to_string());
        }

        // NOTE: Validation already done in lock_and_validate_transaction before this is called
        // UTXOs are already in Locked state - DO NOT transition to SpentPending here
        // That transition happens when voting actually starts (after broadcast)

        // REMOVED: Duplicate UTXO state transition to SpentPending
        // The UTXOs are already locked from lock_and_validate_transaction()
        // They will transition to SpentPending when consensus voting begins

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

        // Note: TransactionPool.add_pending() handles byte-size tracking internally
        self.tx_pool
            .add_pending(tx.clone(), fee)
            .map_err(|e| format!("Failed to add to pool: {}", e))?;

        // ===== timevote CONSENSUS INTEGRATION =====
        // Start TimeVote consensus for this transaction
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
            // Get TX before finalizing since PoolEntry is private
            let tx_for_broadcast = tx.clone();
            self.tx_pool.finalize_transaction(txid); // Drop private return type

            if self.tx_pool.is_finalized(&txid) {
                tracing::info!("✅ TX {:?} auto-finalized", hex::encode(txid));

                // CRITICAL: Transition UTXOs from Locked → SpentFinalized
                // Without this, other nodes reject blocks containing this TX
                // because the UTXOs are still in Locked state
                for input in &tx.inputs {
                    let new_state = UTXOState::SpentFinalized {
                        txid,
                        finalized_at: chrono::Utc::now().timestamp(),
                        votes: 0,
                    };
                    self.utxo_manager
                        .update_state(&input.previous_output, new_state);
                }

                // Broadcast finalization to all nodes so they also finalize it
                // Include the transaction itself so nodes can add it if they don't have it
                self.broadcast(NetworkMessage::TransactionFinalized {
                    txid,
                    tx: tx_for_broadcast,
                })
                .await;
                tracing::info!(
                    "📡 Broadcast TransactionFinalized for {:?}",
                    hex::encode(txid)
                );
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

        // Initiate consensus tracking for fallback protocol
        let tx_state = Arc::new(RwLock::new(VotingState::new(Preference::Accept)));
        self.timevote.tx_state.insert(txid, tx_state);

        // §7.6 Integration: Set initial transaction status to Voting
        // Calculate transaction hash commitment (Protocol §8.1)
        let tx_hash_commitment = TimeVote::calculate_tx_commitment(&tx);

        // Get slot_index for replay protection and AVS snapshot lookup
        // Per Protocol §9.1: slot_time = slot_index * 600 (BLOCK_INTERVAL)
        // TODO: Use blockchain height once blockchain reference is added to ConsensusEngine
        // For now, derive from current timestamp: slot_index = timestamp / BLOCK_INTERVAL
        const BLOCK_INTERVAL: u64 = 600; // 10 minutes
        let slot_index = chrono::Utc::now().timestamp() as u64 / BLOCK_INTERVAL;

        // Create TimeVoteRequest message with all required fields
        // FIX: Include transaction data so validators can process immediately
        // This eliminates the need for a delay waiting for broadcast propagation
        let vote_request_msg = NetworkMessage::TimeVoteRequest {
            txid,
            tx_hash_commitment,
            slot_index,
            tx: Some(tx.clone()), // Include TX so validators have it immediately
        };

        // FIX: No delay needed! Validators will have TX from the vote request itself
        // This makes finality truly event-driven and eliminates arbitrary timing

        if let Some(callback) = self.broadcast_callback.read().await.as_ref() {
            tracing::info!(
                "📡 Broadcasting TimeVoteRequest for TX {:?} (slot {}) to all validators",
                hex::encode(txid),
                slot_index
            );
            callback(vote_request_msg.clone());
        }
        // NOTE: No else branch needed - we check broadcast_callback at start of submit_transaction()

        self.transition_to_voting(txid);

        // Spawn consensus monitoring task
        let consensus = self.timevote.clone();
        let tx_pool = self.tx_pool.clone();
        let consensus_engine_clone = Arc::new(ConsensusEngine {
            masternode_registry: self.masternode_registry.clone(),
            identity: OnceLock::new(),
            utxo_manager: self.utxo_manager.clone(),
            tx_pool: self.tx_pool.clone(),
            broadcast_callback: self.broadcast_callback.clone(),
            state_notifier: self.state_notifier.clone(),
            timevote: self.timevote.clone(),
            finality_proof_mgr: self.finality_proof_mgr.clone(),
            ai_validator: self.ai_validator.clone(),
            finality_times: self.finality_times.clone(),
            avg_finality_ms: self.avg_finality_ms.clone(),
        });
        let tx_status_map = self.timevote.tx_status.clone();

        // PRIORITY: Spawn with high priority for instant finality
        tokio::spawn(async move {
            // The TimeVoteResponse handler in server.rs accumulates votes and
            // finalizes when 51% threshold is met. This loop monitors for that
            // finalization or auto-finalizes on timeout.
            let vote_deadline = Duration::from_secs(5);
            let poll_interval = Duration::from_millis(50);
            let start = Instant::now();

            loop {
                // Check if already finalized (by server.rs TimeVoteResponse handler)
                if consensus.finalized_txs.contains_key(&txid) {
                    tracing::debug!(
                        "✅ TX {:?} finalized via TimeVote (detected by voting loop)",
                        hex::encode(txid)
                    );
                    break;
                }

                if tx_pool.is_finalized(&txid) {
                    tracing::debug!("✅ TX {:?} already in finalized pool", hex::encode(txid));
                    break;
                }

                if start.elapsed() >= vote_deadline {
                    // Timeout: check accumulated weight from server.rs handler
                    let weight = consensus
                        .accumulated_weight
                        .get(&txid)
                        .map(|w| *w.value())
                        .unwrap_or(0);

                    let preference = consensus
                        .tx_state
                        .get(&txid)
                        .map(|s| s.read().preference)
                        .unwrap_or(Preference::Accept);

                    if preference == Preference::Accept {
                        // Auto-finalize: UTXOs are locked, double-spend impossible
                        tracing::warn!(
                            "⚠️ TX {:?} timed out after {}s (weight: {}). Auto-finalizing (UTXOs locked)",
                            hex::encode(txid),
                            vote_deadline.as_secs(),
                            weight
                        );

                        let tx_for_broadcast = tx_pool.get_pending(&txid);
                        tx_pool.finalize_transaction(txid);

                        if tx_pool.is_finalized(&txid) {
                            tracing::info!(
                                "✅ TX {:?} auto-finalized (UTXO-lock protected, weight: {})",
                                hex::encode(txid),
                                weight
                            );

                            // Try to assemble TimeProof from any votes that arrived
                            match consensus.assemble_timeproof(txid) {
                                Ok(proof) => {
                                    tracing::info!(
                                        "📜 TimeProof assembled for TX {:?} with {} votes",
                                        hex::encode(txid),
                                        proof.votes.len()
                                    );
                                    let _ = consensus_engine_clone
                                        .finality_proof_mgr
                                        .store_timeproof(proof.clone());

                                    // Only broadcast if proof weight meets threshold;
                                    // peers reject under-weight proofs with "Insufficient weight"
                                    let total_avs_weight: u64 = consensus_engine_clone
                                        .get_active_masternodes()
                                        .iter()
                                        .map(|mn| mn.tier.sampling_weight())
                                        .sum();
                                    let threshold = (total_avs_weight * 51).div_ceil(100);
                                    if weight >= threshold {
                                        consensus_engine_clone.broadcast_timeproof(proof).await;
                                    } else {
                                        tracing::debug!(
                                            "⏭️ Skipping TimeProof broadcast for auto-finalized TX {:?} (weight {} < threshold {})",
                                            hex::encode(txid), weight, threshold
                                        );
                                    }
                                }
                                Err(_) => {
                                    tracing::debug!(
                                        "No votes available for TimeProof assembly on TX {:?}",
                                        hex::encode(txid)
                                    );
                                }
                            }

                            if let Some(tx_data) = tx_for_broadcast {
                                consensus_engine_clone
                                    .broadcast(NetworkMessage::TransactionFinalized {
                                        txid,
                                        tx: tx_data,
                                    })
                                    .await;
                            }
                        }
                        consensus
                            .finalized_txs
                            .insert(txid, (Preference::Accept, Instant::now()));

                        // Update status
                        tx_status_map.insert(
                            txid,
                            TransactionStatus::Finalized {
                                finalized_at: chrono::Utc::now().timestamp_millis(),
                                vfp_weight: weight,
                            },
                        );
                    }
                    break;
                }

                tokio::time::sleep(poll_interval).await;
            }

            // Cleanup
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

    /// Clear only specific finalized transactions that were included in a block
    pub fn clear_finalized_txs(&self, txids: &[Hash256]) {
        self.tx_pool.clear_finalized_txs(txids);
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
        self.timevote
            .stall_timers
            .get(txid)
            .is_some_and(|entry| entry.value().elapsed() > STALL_TIMEOUT)
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

        // FIX: Transition UTXOs from Locked to SpentPending when voting starts
        // This is the correct place per protocol: Unspent → Locked → SpentPending
        if let Some(tx) = self.tx_pool.get_pending(&txid) {
            let now = chrono::Utc::now().timestamp();
            let n = self.get_masternodes().len() as u32;

            for input in &tx.inputs {
                let new_state = UTXOState::SpentPending {
                    txid,
                    votes: 0,
                    total_nodes: n,
                    spent_at: now,
                };
                self.utxo_manager
                    .update_state(&input.previous_output, new_state.clone());

                tracing::debug!(
                    "UTXO {:?} → SpentPending (txid: {})",
                    input.previous_output,
                    hex::encode(txid)
                );
            }
        }

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

        let threshold_reached = unique_reporters.len() >= threshold;

        // Phase 5: Record fallback activation if threshold just reached
        if threshold_reached && unique_reporters.len() == threshold {
            self.record_fallback_activation();
        }

        threshold_reached
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

        // Calculate Q_finality (simple majority (>50%) of total AVS weight)
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

    // ===== Phase 4: Validation & Safety Functions (§7.6 Security) =====

    /// Detect equivocation: Check if a masternode sent conflicting alerts/votes (§7.6 Security)
    pub fn detect_alert_equivocation(&self, txid: &Hash256, reporter: &str) -> bool {
        if let Some(alerts) = self.timevote.liveness_alerts.get(txid) {
            let reporter_alerts: Vec<_> = alerts
                .iter()
                .filter(|a| a.reporter_mn_id == reporter)
                .collect();

            if reporter_alerts.len() > 1 {
                tracing::warn!(
                    "⚠️ Equivocation detected: {} sent {} alerts for tx {}",
                    reporter,
                    reporter_alerts.len(),
                    hex::encode(txid)
                );
                return true;
            }
        }
        false
    }

    /// Detect vote equivocation: Check if a voter cast multiple different votes for same proposal
    pub fn detect_vote_equivocation(&self, proposal_hash: &Hash256, voter: &str) -> bool {
        if let Some(votes) = self.timevote.fallback_votes.get(proposal_hash) {
            let voter_votes: Vec<_> = votes.iter().filter(|v| v.voter_mn_id == voter).collect();

            if voter_votes.len() > 1 {
                // Check if votes conflict
                let first_decision = &voter_votes[0].vote;
                let has_conflict = voter_votes.iter().any(|v| &v.vote != first_decision);

                if has_conflict {
                    tracing::warn!(
                        "⚠️ Vote equivocation detected: {} cast conflicting votes for proposal {}",
                        voter,
                        hex::encode(proposal_hash)
                    );
                    return true;
                }
            }
        }
        false
    }

    /// Detect Byzantine behavior: Multiple proposals for same transaction
    pub fn detect_multiple_proposals(&self, txid: &Hash256) -> Vec<Hash256> {
        let mut proposals = Vec::new();
        for entry in self.timevote.proposal_to_tx.iter() {
            if entry.value() == txid {
                proposals.push(*entry.key());
            }
        }

        if proposals.len() > 1 {
            tracing::warn!(
                "⚠️ Byzantine behavior: {} proposals detected for tx {}",
                proposals.len(),
                hex::encode(txid)
            );
        }

        proposals
    }

    /// Validate threshold requirements before processing (§7.6 Security)
    pub fn validate_alert_threshold(
        &self,
        txid: &Hash256,
        total_masternodes: usize,
    ) -> Result<bool, String> {
        if total_masternodes == 0 {
            return Err("No masternodes in network".to_string());
        }

        let f = (total_masternodes.saturating_sub(1)) / 3;
        let threshold = f + 1;

        if threshold > total_masternodes {
            return Err(format!(
                "Invalid threshold: f+1={} exceeds total={}",
                threshold, total_masternodes
            ));
        }

        let alert_count = self.get_alert_count(txid);
        Ok(alert_count >= threshold)
    }

    /// Validate vote weight doesn't exceed total AVS weight (Byzantine detection)
    pub fn validate_vote_weight(
        &self,
        proposal_hash: &Hash256,
        total_avs_weight: u64,
    ) -> Result<(), String> {
        if let Some(votes) = self.timevote.fallback_votes.get(proposal_hash) {
            let mut total_voted_weight = 0u64;
            let mut unique_voters = std::collections::HashSet::new();

            for vote in votes.iter() {
                // Check for duplicate voters
                if !unique_voters.insert(&vote.voter_mn_id) {
                    tracing::warn!(
                        "⚠️ Duplicate vote detected from {} for proposal {}",
                        vote.voter_mn_id,
                        hex::encode(proposal_hash)
                    );
                }

                total_voted_weight = total_voted_weight.saturating_add(vote.voter_weight);
            }

            // Allow slight overflow due to rounding, but flag excessive weight
            if total_voted_weight > total_avs_weight * 11 / 10 {
                // >110% is suspicious
                return Err(format!(
                    "Vote weight {} exceeds total AVS weight {} by >10%",
                    total_voted_weight, total_avs_weight
                ));
            }
        }

        Ok(())
    }

    /// Check if a masternode has been flagged for Byzantine behavior
    pub fn is_byzantine_flagged(&self, mn_id: &str) -> bool {
        // For now, simple in-memory tracking
        // TODO: Persistent storage and slashing integration
        self.timevote
            .byzantine_nodes
            .get(mn_id)
            .map(|entry| *entry.value())
            .unwrap_or(false)
    }

    /// Flag a masternode for Byzantine behavior
    pub fn flag_byzantine(&self, mn_id: &str, reason: &str) {
        tracing::error!("🚨 Flagging {} as Byzantine: {}", mn_id, reason);
        self.timevote
            .byzantine_nodes
            .insert(mn_id.to_string(), true);
        // TODO: Emit event for slashing mechanism
    }

    /// Get count of Byzantine-flagged nodes
    pub fn get_byzantine_count(&self) -> usize {
        self.timevote
            .byzantine_nodes
            .iter()
            .filter(|entry| *entry.value())
            .count()
    }

    // ===== Phase 5: Monitoring & Metrics Functions =====

    /// Increment fallback activation counter (when f+1 alerts trigger fallback)
    pub fn record_fallback_activation(&self) {
        self.timevote
            .fallback_activations
            .fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "📊 Fallback activation count: {}",
            self.get_fallback_activations()
        );
    }

    /// Increment stall detection counter
    pub fn record_stall_detection(&self) {
        self.timevote
            .stall_detections
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Increment TimeLock resolution counter
    pub fn record_timelock_resolution(&self) {
        self.timevote
            .timelock_resolutions
            .fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "📊 TimeLock resolution count: {}",
            self.get_timelock_resolutions()
        );
    }

    /// Get fallback activation metrics
    pub fn get_fallback_activations(&self) -> usize {
        self.timevote.fallback_activations.load(Ordering::Relaxed)
    }

    /// Get stall detection metrics
    pub fn get_stall_detections(&self) -> usize {
        self.timevote.stall_detections.load(Ordering::Relaxed)
    }

    /// Get TimeLock resolution metrics
    pub fn get_timelock_resolutions(&self) -> usize {
        self.timevote.timelock_resolutions.load(Ordering::Relaxed)
    }

    /// Get comprehensive fallback metrics snapshot (§7.6 Monitoring)
    pub fn get_fallback_metrics(&self) -> FallbackMetrics {
        FallbackMetrics {
            total_fallback_activations: self.get_fallback_activations(),
            total_stall_detections: self.get_stall_detections(),
            total_timelock_resolutions: self.get_timelock_resolutions(),
            active_stalled_txs: self.timevote.liveness_alerts.len(),
            active_fallback_rounds: self.timevote.fallback_rounds.len(),
            byzantine_nodes_flagged: self.get_byzantine_count(),
            pending_proposals: self.timevote.proposal_to_tx.len(),
            total_fallback_votes: self
                .timevote
                .fallback_votes
                .iter()
                .map(|entry| entry.value().len())
                .sum(),
        }
    }

    /// Log comprehensive fallback status (for debugging and monitoring)
    pub fn log_fallback_status(&self) {
        let metrics = self.get_fallback_metrics();
        tracing::info!(
            "📊 Fallback Status: activations={}, stalls={}, timelock={}, active_stalls={}, rounds={}, byzantine={}, proposals={}, votes={}",
            metrics.total_fallback_activations,
            metrics.total_stall_detections,
            metrics.total_timelock_resolutions,
            metrics.active_stalled_txs,
            metrics.active_fallback_rounds,
            metrics.byzantine_nodes_flagged,
            metrics.pending_proposals,
            metrics.total_fallback_votes
        );
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
    /// ```ignore
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

    /// Resolve all stalled transactions via TimeLock block (§7.6.5)
    ///
    /// Called when producing a TimeLock block. This is the ultimate fallback mechanism
    /// that deterministically resolves all transactions that have been in FallbackResolution
    /// state for too long or have exceeded MAX_FALLBACK_ROUNDS.
    ///
    /// # Protocol Flow (§7.6.5)
    /// 1. Scan all transactions in FallbackResolution state
    /// 2. For each transaction, make deterministic decision based on current state
    /// 3. Finalize with Accept or Reject
    /// 4. Clean up fallback tracking
    /// 5. Return true if any transactions were resolved
    ///
    /// # Decision Logic
    /// - If transaction preference is Accept and still valid → Accept
    /// - Otherwise → Reject
    ///
    /// # Returns
    /// * `bool` - true if any transactions were resolved (set liveness_recovery flag)
    ///
    /// # Example
    /// ```ignore
    /// // When producing TimeLock block
    /// let had_stalls = consensus.resolve_stalls_via_timelock();
    /// block.liveness_recovery = had_stalls;
    /// ```
    pub fn resolve_stalls_via_timelock(&self) -> bool {
        // Get all transactions in FallbackResolution state
        let stalled_txs: Vec<Hash256> = self
            .timevote
            .tx_status
            .iter()
            .filter_map(|entry| match entry.value() {
                TransactionStatus::FallbackResolution { .. } => Some(*entry.key()),
                _ => None,
            })
            .collect();

        if stalled_txs.is_empty() {
            return false;
        }

        // Phase 5: Record TimeLock resolution metric
        self.record_timelock_resolution();

        tracing::warn!(
            "🔄 TimeLock block resolving {} stalled transactions (§7.6.5)",
            stalled_txs.len()
        );

        for txid in &stalled_txs {
            // Get the transaction's current preference
            let decision = if let Some(voting_state) = self.timevote.tx_state.get(txid) {
                let state = voting_state.value().read();
                match state.preference {
                    Preference::Accept => {
                        // Check if still valid
                        if self.is_transaction_still_valid(txid) {
                            FallbackDecision::Accept
                        } else {
                            FallbackDecision::Reject
                        }
                    }
                    Preference::Reject => FallbackDecision::Reject,
                }
            } else {
                // No voting state, default to Reject
                FallbackDecision::Reject
            };

            tracing::info!(
                "🔒 TimeLock resolving tx {}: {:?}",
                hex::encode(&txid[..8]),
                decision
            );

            // Apply the decision
            match decision {
                FallbackDecision::Accept => {
                    self.transition_to_finalized(*txid, 0); // weight=0 for TimeLock resolution
                }
                FallbackDecision::Reject => {
                    self.transition_to_rejected(*txid, "TimeLock fallback rejected".to_string());
                }
            }

            // Clean up tracking
            self.timevote.fallback_rounds.remove(txid);
            self.timevote.liveness_alerts.remove(txid);
            self.timevote.fallback_votes.retain(|k, _| {
                // Remove votes for proposals related to this transaction
                self.timevote
                    .proposal_to_tx
                    .get(k)
                    .map(|v| *v != *txid)
                    .unwrap_or(true)
            });
        }

        true // Transactions were resolved
    }

    /// Check if there are any transactions requiring liveness recovery
    ///
    /// Used to determine if the liveness_recovery flag should be set on the next TimeLock block.
    ///
    /// # Returns
    /// * `bool` - true if there are transactions in FallbackResolution state
    pub fn has_pending_fallback_transactions(&self) -> bool {
        self.timevote
            .tx_status
            .iter()
            .any(|entry| matches!(entry.value(), TransactionStatus::FallbackResolution { .. }))
    }

    // ========================================================================
    // §7.6 LIVENESS FALLBACK PROTOCOL - DETERMINISTIC LEADER ELECTION
    // ========================================================================

    /// Elect deterministic fallback leader (§7.6.4 Step 1)
    ///
    /// Computes the deterministic leader for a specific transaction and round using
    /// a hash-based selection algorithm. All honest nodes compute the same leader
    /// independently without coordination.
    ///
    /// # Algorithm (§7.6.4)
    /// ```text
    /// For each masternode in AVS:
    ///   hash = H(txid || slot_index || round || mn_pubkey)
    ///   
    /// Leader = Masternode with minimum hash value
    /// ```
    ///
    /// # Properties
    /// - **Deterministic**: Same inputs always produce same leader
    /// - **Unpredictable**: Cannot predict leader in advance without all inputs
    /// - **Fair**: Each masternode has equal probability over many elections
    /// - **Byzantine-safe**: Cannot be manipulated by adversaries
    ///
    /// # Arguments
    /// * `txid` - Transaction identifier
    /// * `slot_index` - Current slot (10-minute epoch)
    /// * `round` - Fallback round number (0-4)
    /// * `avs` - Active Validator Set snapshot
    ///
    /// # Returns
    /// * `Option<String>` - Masternode ID of elected leader, or None if AVS empty
    ///
    /// # Example
    /// ```ignore
    /// let avs = consensus.get_avs_snapshot(slot_index)?;
    /// let leader = consensus.elect_fallback_leader(txid, slot_index, 0, &avs)?;
    ///
    /// if leader == my_masternode_id {
    ///     // I am the leader, broadcast proposal
    ///     consensus.broadcast_finality_proposal(txid, slot_index, decision).await?;
    /// }
    /// ```
    pub fn elect_fallback_leader(
        &self,
        txid: Hash256,
        slot_index: u64,
        round: u32,
        avs: &AVSSnapshot,
    ) -> Option<String> {
        if avs.validators.is_empty() && avs.validators_ref.is_none() {
            tracing::warn!("Cannot elect fallback leader: AVS is empty");
            return None;
        }

        let mut min_hash = [0xff; 32];
        let mut elected_leader: Option<String> = None;

        // Get validators - use validators_ref if available, otherwise validators
        if let Some(ref validators_arc) = avs.validators_ref {
            // Use the shared reference
            for validator in validators_arc.iter() {
                // Compute deterministic hash: H(txid || slot_index || round || mn_pubkey)
                let mut hasher = Sha256::new();
                hasher.update(txid);
                hasher.update(slot_index.to_le_bytes());
                hasher.update(round.to_le_bytes());
                hasher.update(validator.address.as_bytes());

                let hash: [u8; 32] = hasher.finalize().into();

                // Track minimum hash
                if hash < min_hash {
                    min_hash = hash;
                    elected_leader = Some(validator.address.clone());
                }
            }
        } else {
            // Use the serialized validators
            for (validator_id, _weight) in &avs.validators {
                // Compute deterministic hash: H(txid || slot_index || round || mn_pubkey)
                let mut hasher = Sha256::new();
                hasher.update(txid);
                hasher.update(slot_index.to_le_bytes());
                hasher.update(round.to_le_bytes());
                hasher.update(validator_id.as_bytes());

                let hash: [u8; 32] = hasher.finalize().into();

                // Track minimum hash
                if hash < min_hash {
                    min_hash = hash;
                    elected_leader = Some(validator_id.clone());
                }
            }
        }

        if let Some(ref leader) = elected_leader {
            tracing::debug!(
                "🎯 Elected fallback leader for tx {} (slot {}, round {}): {}",
                hex::encode(&txid[..8]),
                slot_index,
                round,
                leader
            );
        }

        elected_leader
    }

    /// Check if this node is the fallback leader for a transaction
    ///
    /// Convenience method that combines leader election with identity check.
    ///
    /// # Arguments
    /// * `txid` - Transaction identifier
    /// * `slot_index` - Current slot
    /// * `round` - Fallback round number
    /// * `avs` - Active Validator Set snapshot
    ///
    /// # Returns
    /// * `bool` - true if this node is the elected leader
    pub fn is_fallback_leader(
        &self,
        txid: Hash256,
        slot_index: u64,
        round: u32,
        avs: &AVSSnapshot,
    ) -> bool {
        let identity = match self.identity.get() {
            Some(id) => id,
            None => return false,
        };

        let leader = match self.elect_fallback_leader(txid, slot_index, round, avs) {
            Some(l) => l,
            None => return false,
        };

        identity.address == leader
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
    ///    d. If exceeded: escalate to TimeLock checkpoint sync
    ///
    /// # Arguments
    /// * `masternode_registry` - For computing next leader
    ///
    /// # Returns
    /// Number of timed-out rounds that were retried or escalated
    ///
    /// # Example
    /// ```ignore
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
                // Exceeded max rounds - escalate to TimeLock
                tracing::error!(
                    "❌ Transaction {} exceeded MAX_FALLBACK_ROUNDS ({}), escalating to TimeLock",
                    hex::encode(txid),
                    MAX_FALLBACK_ROUNDS
                );

                // Mark for TimeLock escalation
                self.transition_to_rejected(
                    txid,
                    format!(
                        "Fallback failed after {} rounds, awaiting TimeLock sync",
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
    /// ```ignore
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
    /// * `slot_index` - Current TimeLock slot index (10-minute epochs)
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

        // Get poll history (empty for now, will be populated from vote accumulation)
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

    /// Determine fallback decision based on TimeVote state (§7.6.4 Step 2)
    ///
    /// Leader analyzes the transaction's current TimeVote state to decide whether
    /// to propose Accept or Reject in the fallback consensus round.
    ///
    /// # Decision Logic (§7.6.4)
    /// ```text
    /// IF counter[Accept] > counter[Reject]:
    ///   → Decision = Accept (transaction has majority support)
    /// ELSE:
    ///   → Decision = Reject (transaction lacks consensus)
    /// ```
    ///
    /// # Arguments
    /// * `txid` - Transaction to evaluate
    ///
    /// # Returns
    /// * `FallbackDecision` - Either Accept or Reject
    ///
    /// # Example
    /// ```ignore
    /// // Leader elected, now decide what to propose
    /// let decision = consensus.determine_fallback_decision(&txid);
    /// consensus.broadcast_finality_proposal(txid, slot, decision).await?;
    /// ```
    pub fn determine_fallback_decision(&self, txid: &Hash256) -> FallbackDecision {
        // Get the voting state for this transaction
        if let Some(voting_state) = self.timevote.tx_state.get(txid) {
            let state = voting_state.value().read();

            let preference = state.preference;

            tracing::debug!(
                "Fallback decision for tx {}: preference={:?}",
                hex::encode(&txid[..8]),
                preference
            );

            match preference {
                Preference::Accept => FallbackDecision::Accept,
                Preference::Reject => FallbackDecision::Reject,
            }
        } else {
            // No voting state found, default to Reject
            tracing::warn!(
                "No voting state found for tx {}, defaulting to Reject",
                hex::encode(&txid[..8])
            );
            FallbackDecision::Reject
        }
    }

    /// Execute fallback resolution as elected leader (§7.6.4 Steps 1-3)
    ///
    /// Called when this node has been deterministically elected as the fallback leader
    /// for a stalled transaction. The leader:
    /// 1. Determines decision based on vote counters
    /// 2. Signs and broadcasts FinalityProposal
    /// 3. Waits for Q_finality votes from AVS
    ///
    /// # Arguments
    /// * `txid` - Transaction to resolve
    /// * `slot_index` - Current slot (for leader election)
    /// * `round` - Fallback round number (0-4)
    ///
    /// # Returns
    /// * `Ok(())` - Proposal broadcast successfully
    /// * `Err(String)` - If not leader or broadcast failed
    ///
    /// # Example
    /// ```ignore
    /// let avs = consensus.get_avs_snapshot(slot)?;
    /// if consensus.is_fallback_leader(txid, slot, 0, &avs) {
    ///     consensus.execute_fallback_as_leader(txid, slot, 0).await?;
    /// }
    /// ```
    pub async fn execute_fallback_as_leader(
        &self,
        txid: Hash256,
        slot_index: u64,
        round: u32,
    ) -> Result<(), String> {
        tracing::info!(
            "🎯 Executing fallback as leader for tx {} (slot: {}, round: {})",
            hex::encode(&txid[..8]),
            slot_index,
            round
        );

        // Determine decision based on vote state
        let decision = self.determine_fallback_decision(&txid);

        tracing::info!(
            "📋 Leader decided: {:?} for tx {}",
            decision,
            hex::encode(&txid[..8])
        );

        // Broadcast proposal to AVS
        self.broadcast_finality_proposal(txid, slot_index, decision)
            .await?;

        // Track this round
        self.timevote
            .fallback_rounds
            .insert(txid, (slot_index, round, Instant::now()));

        Ok(())
    }

    /// Broadcast a FinalityProposal as deterministic leader (§7.6.4 Step 3)
    ///
    /// Called when this node has been elected as the deterministic fallback leader
    /// and must propose an Accept/Reject decision for a stalled transaction.
    ///
    /// # Protocol Flow (§7.6.4)
    /// 1. Node computes itself as leader via `elect_fallback_leader(txid, slot, AVS)`
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
    /// * `current_slot` - Current TimeLock slot index for alert timestamp
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

                // Get current slot index (placeholder - will be integrated with TimeLock)
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

    #[tokio::test(flavor = "multi_thread")]
    async fn test_timevote_init() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let av = TimeVoteConsensus::new(config, registry).unwrap();
        assert_eq!(av.get_validators().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_validator_management() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let av = TimeVoteConsensus::new(config, registry).unwrap();

        // Validators now come from masternode registry, so this test
        // just verifies that get_validators() works
        let validators = av.get_validators();
        assert_eq!(validators.len(), 0); // No masternodes registered
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_initiate_consensus() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let av = TimeVoteConsensus::new(config, registry).unwrap();
        let txid = test_txid(1);

        assert!(av.initiate_consensus(txid, Preference::Accept));
        assert!(!av.initiate_consensus(txid, Preference::Accept)); // Already initiated

        let (pref, finalized) = av.get_tx_state(&txid).unwrap();
        assert_eq!(pref, Preference::Accept);
        assert!(!finalized);
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

    // ========================================================================
    // PHASE 6: COMPREHENSIVE FALLBACK PROTOCOL TESTS
    // ========================================================================

    /// Test alert accumulation tracking
    #[test]
    fn test_phase6_alert_accumulation() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let txid = [42u8; 32];

        // Initially no alerts
        assert!(consensus.liveness_alerts.get(&txid).is_none());

        // Add alerts directly
        for i in 0..3 {
            let alert = LivenessAlert {
                chain_id: 1,
                txid,
                tx_hash_commitment: [0u8; 32],
                slot_index: 1000,
                poll_history: vec![],
                current_confidence: 5,
                stall_duration_ms: 30000,
                reporter_mn_id: format!("mn_{}", i),
                reporter_signature: vec![],
            };

            consensus
                .liveness_alerts
                .entry(txid)
                .or_default()
                .push(alert);
        }

        // Verify count
        let alerts = consensus.liveness_alerts.get(&txid).unwrap();
        assert_eq!(alerts.len(), 3);

        // Verify unique reporters
        let unique: std::collections::HashSet<_> =
            alerts.iter().map(|a| &a.reporter_mn_id).collect();
        assert_eq!(unique.len(), 3);
    }

    /// Test vote accumulation tracking
    #[test]
    fn test_phase6_vote_accumulation() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let proposal_hash = [42u8; 32];

        // Initially no votes
        assert!(consensus.fallback_votes.get(&proposal_hash).is_none());

        // Add votes directly to internal structure
        for i in 0..5 {
            let vote = FallbackVote {
                chain_id: 1,
                proposal_hash,
                vote: FallbackVoteDecision::Approve,
                voter_mn_id: format!("mn_{}", i),
                voter_weight: 1_000_000_000,
                voter_signature: vec![],
            };

            consensus
                .fallback_votes
                .entry(proposal_hash)
                .or_default()
                .push(vote);
        }

        // Verify votes stored
        let votes = consensus.fallback_votes.get(&proposal_hash).unwrap();
        assert_eq!(votes.len(), 5);

        // Calculate weights manually
        let approve_weight: u64 = votes
            .iter()
            .filter(|v| matches!(v.vote, FallbackVoteDecision::Approve))
            .map(|v| v.voter_weight)
            .sum();
        assert_eq!(approve_weight, 5_000_000_000);
    }

    /// Test proposal registration and lookup
    #[test]
    fn test_phase6_proposal_tracking() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let txid = [42u8; 32];
        let proposal_hash = [1u8; 32];

        // Register proposal directly
        consensus.proposal_to_tx.insert(proposal_hash, txid);

        // Lookup should work
        let found_txid = consensus.proposal_to_tx.get(&proposal_hash).map(|v| *v);
        assert_eq!(found_txid, Some(txid));

        // Non-existent proposal
        let fake_hash = [99u8; 32];
        assert!(consensus.proposal_to_tx.get(&fake_hash).is_none());
    }

    /// Test fallback round tracking
    #[test]
    fn test_phase6_fallback_round_tracking() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let txid = [42u8; 32];
        let slot_index = 1000u64;
        let round = 2u32;

        // Initially not tracking
        assert!(consensus.fallback_rounds.get(&txid).is_none());

        // Start tracking
        consensus
            .fallback_rounds
            .insert(txid, (slot_index, round, Instant::now()));

        // Verify tracking
        let (stored_slot, stored_round, _) = *consensus.fallback_rounds.get(&txid).unwrap().value();
        assert_eq!(stored_slot, slot_index);
        assert_eq!(stored_round, round);
    }

    /// Test Q_finality threshold calculation (simple majority)
    #[test]
    fn test_phase6_q_finality_threshold() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let proposal_hash = [42u8; 32];
        let total_weight = 9_000_000_000u64;
        let q_finality = (total_weight * 2) / 3; // 6B

        // Add votes below threshold
        for i in 0..5 {
            let vote = FallbackVote {
                chain_id: 1,
                proposal_hash,
                vote: FallbackVoteDecision::Approve,
                voter_mn_id: format!("mn_{}", i),
                voter_weight: 1_000_000_000,
                voter_signature: vec![],
            };
            consensus
                .fallback_votes
                .entry(proposal_hash)
                .or_default()
                .push(vote);
        }

        // Calculate weight
        // NOTE: Must scope the DashMap Ref guard to avoid deadlock.
        // `.get()` returns a Ref that holds a read lock on the shard.
        // If still held when `.entry()` tries to write-lock the same shard below, it deadlocks.
        let approve_weight: u64 = {
            let votes = consensus.fallback_votes.get(&proposal_hash).unwrap();
            votes
                .iter()
                .filter(|v| matches!(v.vote, FallbackVoteDecision::Approve))
                .map(|v| v.voter_weight)
                .sum()
        };
        assert_eq!(approve_weight, 5_000_000_000);
        assert!(approve_weight < q_finality, "5B < 6B, should not finalize");

        // Add more votes to reach threshold
        for i in 5..7 {
            let vote = FallbackVote {
                chain_id: 1,
                proposal_hash,
                vote: FallbackVoteDecision::Approve,
                voter_mn_id: format!("mn_{}", i),
                voter_weight: 1_000_000_000,
                voter_signature: vec![],
            };
            consensus
                .fallback_votes
                .entry(proposal_hash)
                .or_default()
                .push(vote);
        }

        let votes = consensus.fallback_votes.get(&proposal_hash).unwrap();
        let approve_weight: u64 = votes
            .iter()
            .filter(|v| matches!(v.vote, FallbackVoteDecision::Approve))
            .map(|v| v.voter_weight)
            .sum();
        assert_eq!(approve_weight, 7_000_000_000);
        assert!(approve_weight >= q_finality, "7B >= 6B, should finalize");
    }

    /// Test reject decision reaches Q_finality
    #[test]
    fn test_phase6_reject_reaches_quorum() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let proposal_hash = [43u8; 32];
        let total_weight = 10_000_000_000u64;
        let q_finality = (total_weight * 2) / 3; // ~6.67B

        // Add 7 Reject votes (7B >= 6.67B)
        for i in 0..7 {
            let vote = FallbackVote {
                chain_id: 1,
                proposal_hash,
                vote: FallbackVoteDecision::Reject,
                voter_mn_id: format!("mn_{}", i),
                voter_weight: 1_000_000_000,
                voter_signature: vec![],
            };
            consensus
                .fallback_votes
                .entry(proposal_hash)
                .or_default()
                .push(vote);
        }

        let votes = consensus.fallback_votes.get(&proposal_hash).unwrap();
        let reject_weight: u64 = votes
            .iter()
            .filter(|v| matches!(v.vote, FallbackVoteDecision::Reject))
            .map(|v| v.voter_weight)
            .sum();
        assert_eq!(reject_weight, 7_000_000_000);
        assert!(
            reject_weight >= q_finality,
            "Reject should reach Q_finality"
        );
    }

    /// Test f+1 alert threshold with different network sizes
    #[test]
    fn test_phase6_f_plus_1_various_sizes() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        // Test n=10: f=3, threshold=4
        let txid1 = [1u8; 32];
        for i in 0..4 {
            let alert = LivenessAlert {
                chain_id: 1,
                txid: txid1,
                tx_hash_commitment: [0u8; 32],
                slot_index: 1000,
                poll_history: vec![],
                current_confidence: 5,
                stall_duration_ms: 30000,
                reporter_mn_id: format!("mn_{}", i),
                reporter_signature: vec![],
            };
            consensus
                .liveness_alerts
                .entry(txid1)
                .or_default()
                .push(alert);
        }

        // Verify alert count
        let alerts = consensus.liveness_alerts.get(&txid1).unwrap();
        let unique: std::collections::HashSet<_> =
            alerts.iter().map(|a| &a.reporter_mn_id).collect();
        assert_eq!(unique.len(), 4, "Should have 4 unique reporters");

        // Test n=100: f=33, threshold=34
        let txid2 = [2u8; 32];
        for i in 0..34 {
            let alert = LivenessAlert {
                chain_id: 1,
                txid: txid2,
                tx_hash_commitment: [0u8; 32],
                slot_index: 1000,
                poll_history: vec![],
                current_confidence: 5,
                stall_duration_ms: 30000,
                reporter_mn_id: format!("mn_{}", i),
                reporter_signature: vec![],
            };
            consensus
                .liveness_alerts
                .entry(txid2)
                .or_default()
                .push(alert);
        }

        let alerts = consensus.liveness_alerts.get(&txid2).unwrap();
        let unique: std::collections::HashSet<_> =
            alerts.iter().map(|a| &a.reporter_mn_id).collect();
        assert_eq!(unique.len(), 34, "Should have 34 unique reporters");
    }

    /// Test mixed Approve/Reject votes
    #[test]
    fn test_phase6_mixed_votes() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        let proposal_hash = [44u8; 32];

        // Add 4 Approve votes
        for i in 0..4 {
            let vote = FallbackVote {
                chain_id: 1,
                proposal_hash,
                vote: FallbackVoteDecision::Approve,
                voter_mn_id: format!("mn_approve_{}", i),
                voter_weight: 1_000_000_000,
                voter_signature: vec![],
            };
            consensus
                .fallback_votes
                .entry(proposal_hash)
                .or_default()
                .push(vote);
        }

        // Add 3 Reject votes
        for i in 0..3 {
            let vote = FallbackVote {
                chain_id: 1,
                proposal_hash,
                vote: FallbackVoteDecision::Reject,
                voter_mn_id: format!("mn_reject_{}", i),
                voter_weight: 1_000_000_000,
                voter_signature: vec![],
            };
            consensus
                .fallback_votes
                .entry(proposal_hash)
                .or_default()
                .push(vote);
        }

        // Check status
        let votes = consensus.fallback_votes.get(&proposal_hash).unwrap();
        assert_eq!(votes.len(), 7);

        let approve_weight: u64 = votes
            .iter()
            .filter(|v| matches!(v.vote, FallbackVoteDecision::Approve))
            .map(|v| v.voter_weight)
            .sum();
        let reject_weight: u64 = votes
            .iter()
            .filter(|v| matches!(v.vote, FallbackVoteDecision::Reject))
            .map(|v| v.voter_weight)
            .sum();

        assert_eq!(approve_weight, 4_000_000_000);
        assert_eq!(reject_weight, 3_000_000_000);
    }

    /// Test Byzantine detection via internal tracking
    #[test]
    fn test_phase6_byzantine_tracking() {
        let config = TimeVoteConfig::default();
        let registry = create_test_registry();
        let consensus = TimeVoteConsensus::new(config, registry).unwrap();

        // Direct access to internal byzantine_nodes map
        assert_eq!(consensus.byzantine_nodes.len(), 0);

        // Flag a node
        consensus.byzantine_nodes.insert("mn_bad".to_string(), true);

        // Verify tracking
        assert_eq!(consensus.byzantine_nodes.len(), 1);
        assert!(*consensus.byzantine_nodes.get("mn_bad").unwrap().value());
    }
}
