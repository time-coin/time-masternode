//! Core data types for TimeCoin blockchain

#![allow(dead_code)]

use ed25519_dalek::{Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub type Hash256 = [u8; 32];
pub type Signature = [u8; 64];

// Import ValidatorInfo from consensus for AVSSnapshot
pub use crate::consensus::ValidatorInfo;

// Constants
pub const SATOSHIS_PER_TIME: u64 = 100_000_000; // 1 TIME = 10^8 satoshis

// NetworkType is defined in network_type.rs module

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct OutPoint {
    pub txid: Hash256,
    pub vout: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(clippy::upper_case_acronyms)]
pub struct UTXO {
    pub outpoint: OutPoint,
    pub value: u64,
    pub script_pubkey: Vec<u8>,
    pub address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxInput {
    pub previous_output: OutPoint,
    pub script_sig: Vec<u8>,
    pub sequence: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxOutput {
    pub value: u64,
    pub script_pubkey: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub version: u32,
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    pub lock_time: u32,
    pub timestamp: i64,
}

impl Transaction {
    pub fn txid(&self) -> Hash256 {
        // Use JSON serialization for canonical, network-compatible hashing
        let json = serde_json::to_string(self).expect("JSON serialization should succeed");
        Sha256::digest(json.as_bytes()).into()
    }

    /// Calculate transaction fee (input sum - output sum)
    pub fn fee_amount(&self) -> u64 {
        // For now, return 0 as fees require UTXO lookup
        // In a real implementation, this would be:
        // fee = input_total - output_total
        0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(clippy::upper_case_acronyms)]
pub enum UTXOState {
    Unspent,
    Locked {
        txid: Hash256,
        locked_at: i64,
    },
    SpentPending {
        txid: Hash256,
        votes: u32,
        total_nodes: u32,
        spent_at: i64,
    },
    SpentFinalized {
        txid: Hash256,
        finalized_at: i64,
        votes: u32,
    },
    Confirmed {
        txid: Hash256,
        block_height: u64,
        confirmed_at: i64,
    },
}

impl std::fmt::Display for UTXOState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UTXOState::Unspent => write!(f, "Unspent"),
            UTXOState::Locked { txid, locked_at } => {
                write!(
                    f,
                    "Locked (txid: {}, locked_at: {})",
                    hex::encode(txid),
                    locked_at
                )
            }
            UTXOState::SpentPending {
                txid,
                votes,
                total_nodes,
                spent_at,
            } => {
                write!(
                    f,
                    "SpentPending (txid: {}, votes: {}/{}, spent_at: {})",
                    hex::encode(txid),
                    votes,
                    total_nodes,
                    spent_at
                )
            }
            UTXOState::SpentFinalized {
                txid,
                finalized_at,
                votes,
            } => {
                write!(
                    f,
                    "SpentFinalized (txid: {}, finalized_at: {}, votes: {})",
                    hex::encode(txid),
                    finalized_at,
                    votes
                )
            }
            UTXOState::Confirmed {
                txid,
                block_height,
                confirmed_at,
            } => {
                write!(
                    f,
                    "Confirmed (txid: {}, block_height: {}, confirmed_at: {})",
                    hex::encode(txid),
                    block_height,
                    confirmed_at
                )
            }
        }
    }
}

// ============================================================================
// TRANSACTION STATUS - Per Protocol §7.3 and §7.6
// ============================================================================

/// Transaction status in the consensus state machine
/// Per protocol §7.3: status[X] ∈ {Seen, Voting, Finalized, Rejected, Archived}
/// Extended in §7.6 with FallbackResolution state
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransactionStatus {
    /// Transaction received, basic validation passed
    Seen,
    /// Actively voting via TimeVote consensus, accumulating signed votes
    Voting {
        confidence: u32,
        counter: u32,
        started_at: i64, // Unix timestamp in milliseconds
    },
    /// Deterministic fallback resolution in progress (Protocol §7.6)
    FallbackResolution {
        started_at: i64,
        round: u32,
        alerts_count: u32,
    },
    /// Has valid TimeProof with ≥ Q_finality weight (Protocol §8)
    Finalized { finalized_at: i64, vfp_weight: u64 },
    /// Rejected due to conflict or invalidity
    Rejected { rejected_at: i64, reason: String },
    /// Included in TimeLock Block, can be pruned
    Archived { block_height: u64, archived_at: i64 },
}

impl TransactionStatus {
    /// Check if transaction is in a terminal state
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TransactionStatus::Finalized { .. }
                | TransactionStatus::Rejected { .. }
                | TransactionStatus::Archived { .. }
        )
    }

    /// Check if transaction can still be finalized
    pub fn is_pending(&self) -> bool {
        matches!(
            self,
            TransactionStatus::Seen
                | TransactionStatus::Voting { .. }
                | TransactionStatus::FallbackResolution { .. }
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Masternode {
    pub address: String,
    pub wallet_address: String,
    pub collateral: u64,
    /// The specific UTXO locked as collateral (None for legacy masternodes)
    pub collateral_outpoint: Option<OutPoint>,
    /// Timestamp when collateral was locked
    pub locked_at: u64,
    /// Optional block height when unlock can be completed (for time-locked unlocks)
    pub unlock_height: Option<u64>,
    pub public_key: VerifyingKey,
    pub tier: MasternodeTier,
    pub registered_at: u64,
}

impl Masternode {
    /// Create a new legacy masternode without locked collateral (for migration)
    pub fn new_legacy(
        address: String,
        wallet_address: String,
        collateral: u64,
        public_key: VerifyingKey,
        tier: MasternodeTier,
        registered_at: u64,
    ) -> Self {
        Self {
            address,
            wallet_address,
            collateral,
            collateral_outpoint: None,
            locked_at: registered_at,
            unlock_height: None,
            public_key,
            tier,
            registered_at,
        }
    }

    /// Create a new masternode with locked collateral
    pub fn new_with_collateral(
        address: String,
        wallet_address: String,
        collateral: u64,
        collateral_outpoint: OutPoint,
        public_key: VerifyingKey,
        tier: MasternodeTier,
        registered_at: u64,
    ) -> Self {
        Self {
            address,
            wallet_address,
            collateral,
            collateral_outpoint: Some(collateral_outpoint),
            locked_at: registered_at,
            unlock_height: None,
            public_key,
            tier,
            registered_at,
        }
    }

    /// Check if this masternode has locked collateral
    pub fn has_locked_collateral(&self) -> bool {
        self.collateral_outpoint.is_some()
    }
}

/// Sort masternodes deterministically by address for consensus
/// This ensures all nodes compute the same leader election, merkle roots, etc.
pub fn sort_masternodes_canonical(masternodes: &mut [Masternode]) {
    masternodes.sort_by(|a, b| a.address.cmp(&b.address));
}

#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MasternodeTier {
    Free = 0,       // Can receive rewards (0.1x weight vs Bronze), cannot vote on governance
    Bronze = 1000,  // Can vote on governance, 1x baseline reward weight
    Silver = 10000, // Can vote on governance, 10x reward weight
    Gold = 100000,  // Can vote on governance, 100x reward weight
}

impl MasternodeTier {
    /// Free tier nodes cannot vote on governance proposals
    #[allow(dead_code)]
    pub fn can_vote_governance(&self) -> bool {
        !matches!(self, MasternodeTier::Free)
    }

    #[allow(dead_code)]
    pub fn collateral(&self) -> u64 {
        match self {
            MasternodeTier::Free => 0,
            MasternodeTier::Bronze => 1000,
            MasternodeTier::Silver => 10000,
            MasternodeTier::Gold => 100000,
        }
    }

    /// Reward weight for block reward distribution
    /// Free nodes get 0.1x weight compared to Bronze (100 vs 1000)
    /// But if ONLY free nodes exist, they share 100% of rewards
    pub fn reward_weight(&self) -> u64 {
        match self {
            MasternodeTier::Free => 100,     // 0.1x relative to Bronze
            MasternodeTier::Bronze => 1000,  // 1x (baseline)
            MasternodeTier::Silver => 10000, // 10x
            MasternodeTier::Gold => 100000,  // 100x
        }
    }

    #[allow(dead_code)]
    pub fn voting_power(&self) -> u64 {
        match self {
            MasternodeTier::Free => 0,    // Cannot vote
            MasternodeTier::Bronze => 1,  // 1x voting power
            MasternodeTier::Silver => 10, // 10x voting power
            MasternodeTier::Gold => 100,  // 100x voting power
        }
    }

    #[allow(dead_code)]
    pub fn min_uptime(&self) -> f64 {
        match self {
            MasternodeTier::Free => 0.85,   // 85% minimum
            MasternodeTier::Bronze => 0.90, // 90% minimum
            MasternodeTier::Silver => 0.95, // 95% minimum
            MasternodeTier::Gold => 0.98,   // 98% minimum
        }
    }

    /// Sampling weight for timevote consensus
    /// Used for stake-weighted sampling: P(sample node_i) = Weight_i / Total_Weight
    #[allow(dead_code)]
    pub fn sampling_weight(&self) -> usize {
        match self {
            MasternodeTier::Free => 1,     // 1x weight
            MasternodeTier::Bronze => 10,  // 10x weight
            MasternodeTier::Silver => 100, // 100x weight
            MasternodeTier::Gold => 1000,  // 1000x weight
        }
    }
}

// ============================================================================
// Collateral Locking
// ============================================================================

/// Information about a locked UTXO used as masternode collateral
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LockedCollateral {
    /// The UTXO being locked
    pub outpoint: OutPoint,
    /// Masternode address that locked it
    pub masternode_address: String,
    /// Block height when locked
    pub lock_height: u64,
    /// Timestamp when locked
    pub locked_at: u64,
    /// Optional unlock height (for time-locked unlocks)
    pub unlock_height: Option<u64>,
    /// Amount locked
    pub amount: u64,
}

impl LockedCollateral {
    pub fn new(
        outpoint: OutPoint,
        masternode_address: String,
        lock_height: u64,
        amount: u64,
    ) -> Self {
        Self {
            outpoint,
            masternode_address,
            lock_height,
            locked_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            unlock_height: None,
            amount,
        }
    }

    pub fn is_unlockable(&self, current_height: u64) -> bool {
        if let Some(unlock_height) = self.unlock_height {
            current_height >= unlock_height
        } else {
            false
        }
    }
}

// ============================================================================
// VERIFIABLE FINALITY PROOFS (VFP) - Per Protocol §8
// ============================================================================

/// A finality vote signed by a masternode
/// Vote decision for finality consensus
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoteDecision {
    Accept, // Transaction is valid and preferred
    Reject, // Transaction is invalid or conflicts with preferred transaction
}

/// Per protocol: FinalityVote = { chain_id, txid, tx_hash_commitment, slot_index, decision, voter_mn_id, voter_weight, signature }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalityVote {
    pub chain_id: u32,
    pub txid: Hash256,
    pub tx_hash_commitment: Hash256, // H(canonical_tx_bytes)
    pub slot_index: u64,
    pub decision: VoteDecision, // Accept or Reject (REQUIRED for equivocation prevention)
    pub voter_mn_id: String,
    pub voter_weight: u64,
    pub signature: Vec<u8>, // Ed25519 signature
}

impl FinalityVote {
    /// Verify the finality vote signature
    pub fn verify(&self, pubkey: &VerifyingKey) -> Result<(), Box<dyn std::error::Error>> {
        // Reconstruct the signed message
        let msg = self.signing_message();
        // Verify the signature
        pubkey.verify(
            &msg,
            &ed25519_dalek::Signature::from_slice(&self.signature)?,
        )?;
        Ok(())
    }

    /// Get the message that was signed
    fn signing_message(&self) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&self.chain_id.to_le_bytes());
        msg.extend_from_slice(&self.txid);
        msg.extend_from_slice(&self.tx_hash_commitment);
        msg.extend_from_slice(&self.slot_index.to_le_bytes());
        // CRITICAL: Include decision in signature to prevent equivocation
        msg.push(match self.decision {
            VoteDecision::Accept => 0x01,
            VoteDecision::Reject => 0x00,
        });
        msg.extend_from_slice(self.voter_mn_id.as_bytes());
        msg.extend_from_slice(&self.voter_weight.to_le_bytes());
        msg
    }
}

/// Verifiable Finality Proof for a transaction
/// Per protocol §8.2: VFP(X) = { tx, slot_index, votes[] }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiableFinality {
    pub tx: Transaction,
    pub slot_index: u64,
    pub votes: Vec<FinalityVote>,
}

impl VerifiableFinality {
    /// Validate the VFP according to protocol rules
    /// Returns the total weight of valid votes
    pub fn validate(
        &self,
        chain_id: u32,
        avs_snapshot: &[(String, u64, VerifyingKey)], // (mn_id, weight, pubkey)
    ) -> Result<u64, String> {
        // Rule 1: All signatures verify
        let mut total_weight = 0u64;
        let mut seen_voters = std::collections::HashSet::new();

        for vote in &self.votes {
            // Must match chain_id
            if vote.chain_id != chain_id {
                return Err("Chain ID mismatch".to_string());
            }

            // Must match txid
            if vote.txid != self.tx.txid() {
                return Err("Transaction ID mismatch".to_string());
            }

            // Must match tx_hash_commitment
            let commitment: Hash256 =
                Sha256::digest(bincode::serialize(&self.tx).map_err(|e| e.to_string())?)
                    .as_slice()
                    .try_into()
                    .unwrap();
            if vote.tx_hash_commitment != commitment {
                return Err("Transaction hash commitment mismatch".to_string());
            }

            // Must match slot_index
            if vote.slot_index != self.slot_index {
                return Err("Slot index mismatch".to_string());
            }

            // Voter must be distinct
            if seen_voters.contains(&vote.voter_mn_id) {
                return Err("Duplicate voter".to_string());
            }
            seen_voters.insert(vote.voter_mn_id.clone());

            // Voter must be in AVS snapshot
            let voter_info = avs_snapshot
                .iter()
                .find(|(id, _, _)| id == &vote.voter_mn_id)
                .ok_or_else(|| format!("Voter {} not in AVS snapshot", vote.voter_mn_id))?;

            // Verify signature
            voter_info
                .2
                .verify(
                    vote.signing_message().as_slice(),
                    &ed25519_dalek::Signature::from_slice(&vote.signature)
                        .map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;

            total_weight += voter_info.1;
        }

        Ok(total_weight)
    }
}

/// Active Validator Set snapshot for a slot
/// Per protocol §8.4: Captures the set of validators at each slot_index
/// Used for verifying finality votes and their weights
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AVSSnapshot {
    pub slot_index: u64,
    /// Reference to validator set (shared via Arc to avoid cloning addresses)
    #[serde(skip)]
    pub validators_ref: Option<Arc<Vec<ValidatorInfo>>>,
    /// Only used for serialization/deserialization
    pub validators: Vec<(String, u64)>, // (mn_id, weight)
    pub total_weight: u64,
    pub timestamp: u64,
}

impl AVSSnapshot {
    /// Create a new AVS snapshot with shared validator reference
    pub fn new_with_ref(slot_index: u64, validators: Arc<Vec<ValidatorInfo>>) -> Self {
        let total_weight: u64 = validators.iter().map(|v| v.weight as u64).sum();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Self {
            slot_index,
            validators_ref: Some(Arc::clone(&validators)),
            validators: Vec::new(), // Empty for runtime use
            total_weight,
            timestamp,
        }
    }

    /// Create a new AVS snapshot (legacy method for serialization)
    pub fn new(slot_index: u64, validators: Vec<(String, u64)>) -> Self {
        let total_weight = validators.iter().map(|(_, w)| w).sum();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            slot_index,
            validators_ref: None,
            validators,
            total_weight,
            timestamp,
        }
    }

    /// Check if a validator is in this snapshot
    pub fn contains_validator(&self, mn_id: &str) -> bool {
        if let Some(ref validators) = self.validators_ref {
            validators.iter().any(|v| v.address == mn_id)
        } else {
            self.validators.iter().any(|(id, _)| id == mn_id)
        }
    }

    /// Get validator weight if present
    pub fn get_validator_weight(&self, mn_id: &str) -> Option<u64> {
        if let Some(ref validators) = self.validators_ref {
            validators
                .iter()
                .find(|v| v.address == mn_id)
                .map(|v| v.weight as u64)
        } else {
            self.validators
                .iter()
                .find(|(id, _)| id == mn_id)
                .map(|(_, w)| *w)
        }
    }

    /// Calculate voting threshold (67% of total weight)
    pub fn voting_threshold(&self) -> u64 {
        (self.total_weight * 67) / 100
    }
}

// ============================================================================
// LIVENESS FALLBACK PROTOCOL - Per Protocol §7.6
// ============================================================================

/// Poll result data for liveness evidence
/// Per protocol §7.6.2: Records individual polling rounds for stall detection
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PollResult {
    pub round: u64,
    pub votes_valid: u32,
    pub votes_invalid: u32,
    pub votes_unknown: u32,
    pub timestamp_ms: u64,
}

/// Liveness alert broadcast when transaction stalls
/// Per protocol §7.6.2: LivenessAlert message structure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LivenessAlert {
    pub chain_id: u32,
    pub txid: Hash256,
    pub tx_hash_commitment: Hash256,
    pub slot_index: u64,
    pub poll_history: Vec<PollResult>,
    pub current_confidence: u32,
    pub stall_duration_ms: u64,
    pub reporter_mn_id: String,
    pub reporter_signature: Vec<u8>, // Ed25519 signature
}

impl LivenessAlert {
    /// Get the message that should be signed
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&self.chain_id.to_le_bytes());
        msg.extend_from_slice(&self.txid);
        msg.extend_from_slice(&self.tx_hash_commitment);
        msg.extend_from_slice(&self.slot_index.to_le_bytes());
        msg.extend_from_slice(&self.current_confidence.to_le_bytes());
        msg.extend_from_slice(&self.stall_duration_ms.to_le_bytes());
        msg.extend_from_slice(self.reporter_mn_id.as_bytes());
        msg
    }

    /// Verify the alert signature
    pub fn verify(&self, pubkey: &VerifyingKey) -> Result<(), Box<dyn std::error::Error>> {
        let msg = self.signing_message();
        pubkey.verify(
            &msg,
            &ed25519_dalek::Signature::from_slice(&self.reporter_signature)?,
        )?;
        Ok(())
    }
}

/// Finality proposal from deterministic fallback leader
/// Per protocol §7.6.4 Step 3: Leader's decision on transaction
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FallbackDecision {
    Accept,
    Reject,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalityProposal {
    pub chain_id: u32,
    pub txid: Hash256,
    pub tx_hash_commitment: Hash256,
    pub slot_index: u64,
    pub decision: FallbackDecision,
    pub justification: String, // OPTIONAL: debugging info
    pub leader_mn_id: String,
    pub leader_signature: Vec<u8>, // Ed25519 signature
}

impl FinalityProposal {
    /// Hash of this proposal for voting
    pub fn proposal_hash(&self) -> Hash256 {
        let mut hasher = Sha256::new();
        hasher.update(self.chain_id.to_le_bytes());
        hasher.update(self.txid);
        hasher.update(self.tx_hash_commitment);
        hasher.update(self.slot_index.to_le_bytes());
        match self.decision {
            FallbackDecision::Accept => hasher.update([1u8]),
            FallbackDecision::Reject => hasher.update([0u8]),
        }
        hasher.update(self.leader_mn_id.as_bytes());
        hasher.finalize().into()
    }

    /// Get the message that should be signed
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&self.chain_id.to_le_bytes());
        msg.extend_from_slice(&self.txid);
        msg.extend_from_slice(&self.tx_hash_commitment);
        msg.extend_from_slice(&self.slot_index.to_le_bytes());
        match self.decision {
            FallbackDecision::Accept => msg.push(1u8),
            FallbackDecision::Reject => msg.push(0u8),
        }
        msg.extend_from_slice(self.leader_mn_id.as_bytes());
        msg
    }

    /// Verify the proposal signature
    pub fn verify(&self, pubkey: &VerifyingKey) -> Result<(), Box<dyn std::error::Error>> {
        let msg = self.signing_message();
        pubkey.verify(
            &msg,
            &ed25519_dalek::Signature::from_slice(&self.leader_signature)?,
        )?;
        Ok(())
    }
}

/// Vote on a fallback finality proposal
/// Per protocol §7.6.4 Step 4: AVS members vote on leader's proposal
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum FallbackVoteDecision {
    Approve,
    Reject,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FallbackVote {
    pub chain_id: u32,
    pub proposal_hash: Hash256,
    pub vote: FallbackVoteDecision,
    pub voter_mn_id: String,
    pub voter_weight: u64,
    pub voter_signature: Vec<u8>, // Ed25519 signature
}

impl FallbackVote {
    /// Get the message that should be signed
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&self.chain_id.to_le_bytes());
        msg.extend_from_slice(&self.proposal_hash);
        match self.vote {
            FallbackVoteDecision::Approve => msg.push(1u8),
            FallbackVoteDecision::Reject => msg.push(0u8),
        }
        msg.extend_from_slice(self.voter_mn_id.as_bytes());
        msg.extend_from_slice(&self.voter_weight.to_le_bytes());
        msg
    }

    /// Verify the vote signature
    pub fn verify(&self, pubkey: &VerifyingKey) -> Result<(), Box<dyn std::error::Error>> {
        let msg = self.signing_message();
        pubkey.verify(
            &msg,
            &ed25519_dalek::Signature::from_slice(&self.voter_signature)?,
        )?;
        Ok(())
    }
}

// ============================================================================
// UNIT TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// Helper to create a test signing key
    fn test_signing_key() -> SigningKey {
        SigningKey::from_bytes(&[42u8; 32])
    }

    /// Helper to create a test transaction
    fn test_transaction() -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![TxOutput {
                value: 1000,
                script_pubkey: vec![0x76, 0xa9], // OP_DUP OP_HASH160
            }],
            lock_time: 0,
            timestamp: 1234567890,
        }
    }

    #[test]
    fn test_transaction_status_terminal() {
        let status = TransactionStatus::Finalized {
            finalized_at: 1000,
            vfp_weight: 100,
        };
        assert!(status.is_terminal());
        assert!(!status.is_pending());

        let status = TransactionStatus::Rejected {
            rejected_at: 1000,
            reason: "test".to_string(),
        };
        assert!(status.is_terminal());

        let status = TransactionStatus::Archived {
            block_height: 100,
            archived_at: 1000,
        };
        assert!(status.is_terminal());
    }

    #[test]
    fn test_transaction_status_pending() {
        let status = TransactionStatus::Seen;
        assert!(status.is_pending());
        assert!(!status.is_terminal());

        let status = TransactionStatus::Voting {
            confidence: 5,
            counter: 10,
            started_at: 1000,
        };
        assert!(status.is_pending());

        let status = TransactionStatus::FallbackResolution {
            started_at: 1000,
            round: 1,
            alerts_count: 3,
        };
        assert!(status.is_pending());
    }

    #[test]
    fn test_liveness_alert_signature() {
        let signing_key = test_signing_key();
        let tx = test_transaction();
        let txid = tx.txid();
        let tx_hash: Hash256 = Sha256::digest(bincode::serialize(&tx).unwrap()).into();

        let mut alert = LivenessAlert {
            chain_id: 1,
            txid,
            tx_hash_commitment: tx_hash,
            slot_index: 100,
            poll_history: vec![PollResult {
                round: 1,
                votes_valid: 10,
                votes_invalid: 5,
                votes_unknown: 3,
                timestamp_ms: 1234567890,
            }],
            current_confidence: 5,
            stall_duration_ms: 30000,
            reporter_mn_id: "test_mn".to_string(),
            reporter_signature: Vec::new(),
        };

        // Sign the alert
        let msg = alert.signing_message();
        let signature = signing_key.sign(&msg);
        alert.reporter_signature = signature.to_bytes().to_vec();

        // Verify signature
        let verifying_key = signing_key.verifying_key();
        assert!(alert.verify(&verifying_key).is_ok());

        // Test with wrong key
        let wrong_key = SigningKey::from_bytes(&[99u8; 32]);
        assert!(alert.verify(&wrong_key.verifying_key()).is_err());
    }

    #[test]
    fn test_finality_proposal_hash() {
        let _signing_key = test_signing_key();
        let tx = test_transaction();
        let txid = tx.txid();
        let tx_hash: Hash256 = Sha256::digest(bincode::serialize(&tx).unwrap()).into();

        let proposal = FinalityProposal {
            chain_id: 1,
            txid,
            tx_hash_commitment: tx_hash,
            slot_index: 100,
            decision: FallbackDecision::Accept,
            justification: "Test".to_string(),
            leader_mn_id: "leader1".to_string(),
            leader_signature: Vec::new(),
        };

        // Proposal hash should be deterministic
        let hash1 = proposal.proposal_hash();
        let hash2 = proposal.proposal_hash();
        assert_eq!(hash1, hash2);

        // Different decision should give different hash
        let proposal2 = FinalityProposal {
            decision: FallbackDecision::Reject,
            ..proposal.clone()
        };
        assert_ne!(proposal.proposal_hash(), proposal2.proposal_hash());
    }

    #[test]
    fn test_finality_proposal_signature() {
        let signing_key = test_signing_key();
        let tx = test_transaction();
        let txid = tx.txid();
        let tx_hash: Hash256 = Sha256::digest(bincode::serialize(&tx).unwrap()).into();

        let mut proposal = FinalityProposal {
            chain_id: 1,
            txid,
            tx_hash_commitment: tx_hash,
            slot_index: 100,
            decision: FallbackDecision::Accept,
            justification: "Test".to_string(),
            leader_mn_id: "leader1".to_string(),
            leader_signature: Vec::new(),
        };

        // Sign the proposal
        let msg = proposal.signing_message();
        let signature = signing_key.sign(&msg);
        proposal.leader_signature = signature.to_bytes().to_vec();

        // Verify signature
        let verifying_key = signing_key.verifying_key();
        assert!(proposal.verify(&verifying_key).is_ok());

        // Test with wrong key
        let wrong_key = SigningKey::from_bytes(&[99u8; 32]);
        assert!(proposal.verify(&wrong_key.verifying_key()).is_err());
    }

    #[test]
    fn test_fallback_vote_signature() {
        let signing_key = test_signing_key();
        let proposal_hash = [42u8; 32];

        let mut vote = FallbackVote {
            chain_id: 1,
            proposal_hash,
            vote: FallbackVoteDecision::Approve,
            voter_mn_id: "voter1".to_string(),
            voter_weight: 1000,
            voter_signature: Vec::new(),
        };

        // Sign the vote
        let msg = vote.signing_message();
        let signature = signing_key.sign(&msg);
        vote.voter_signature = signature.to_bytes().to_vec();

        // Verify signature
        let verifying_key = signing_key.verifying_key();
        assert!(vote.verify(&verifying_key).is_ok());

        // Test with wrong key
        let wrong_key = SigningKey::from_bytes(&[99u8; 32]);
        assert!(vote.verify(&wrong_key.verifying_key()).is_err());
    }

    #[test]
    fn test_poll_result_serialization() {
        let poll = PollResult {
            round: 1,
            votes_valid: 10,
            votes_invalid: 5,
            votes_unknown: 3,
            timestamp_ms: 1234567890,
        };

        // Test serialization
        let serialized = bincode::serialize(&poll).unwrap();
        let deserialized: PollResult = bincode::deserialize(&serialized).unwrap();

        assert_eq!(poll.round, deserialized.round);
        assert_eq!(poll.votes_valid, deserialized.votes_valid);
        assert_eq!(poll.votes_invalid, deserialized.votes_invalid);
        assert_eq!(poll.votes_unknown, deserialized.votes_unknown);
        assert_eq!(poll.timestamp_ms, deserialized.timestamp_ms);
    }

    #[test]
    fn test_fallback_decision_enum() {
        // Test equality
        assert_eq!(FallbackDecision::Accept, FallbackDecision::Accept);
        assert_eq!(FallbackDecision::Reject, FallbackDecision::Reject);
        assert_ne!(FallbackDecision::Accept, FallbackDecision::Reject);

        // Test clone
        let decision = FallbackDecision::Accept;
        let cloned = decision.clone();
        assert_eq!(decision, cloned);
    }

    #[test]
    fn test_fallback_vote_decision_enum() {
        // Test equality
        assert_eq!(FallbackVoteDecision::Approve, FallbackVoteDecision::Approve);
        assert_eq!(FallbackVoteDecision::Reject, FallbackVoteDecision::Reject);
        assert_ne!(FallbackVoteDecision::Approve, FallbackVoteDecision::Reject);

        // Test clone
        let decision = FallbackVoteDecision::Approve;
        let cloned = decision.clone();
        assert_eq!(decision, cloned);
    }
}
