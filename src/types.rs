//! Core data types for TimeCoin blockchain

#![allow(dead_code)]

use ed25519_dalek::{Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Hash256 = [u8; 32];
pub type Signature = [u8; 64];

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Masternode {
    pub address: String,
    pub wallet_address: String,
    pub collateral: u64,
    pub public_key: VerifyingKey,
    pub tier: MasternodeTier,
    pub registered_at: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

    /// Sampling weight for Avalanche consensus
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
// VERIFIABLE FINALITY PROOFS (VFP) - Per Protocol §8
// ============================================================================

/// A finality vote signed by a masternode
/// Per protocol: FinalityVote = { chain_id, txid, tx_hash_commitment, slot_index, voter_mn_id, voter_weight, signature }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalityVote {
    pub chain_id: u32,
    pub txid: Hash256,
    pub tx_hash_commitment: Hash256, // H(canonical_tx_bytes)
    pub slot_index: u64,
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
    pub validators: Vec<(String, u64)>, // (mn_id, weight)
    pub total_weight: u64,
    pub timestamp: u64,
}

impl AVSSnapshot {
    /// Create a new AVS snapshot
    pub fn new(slot_index: u64, validators: Vec<(String, u64)>) -> Self {
        let total_weight = validators.iter().map(|(_, w)| w).sum();
        Self {
            slot_index,
            validators,
            total_weight,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        }
    }

    /// Check if a validator is in this snapshot
    pub fn contains_validator(&self, mn_id: &str) -> bool {
        self.validators.iter().any(|(id, _)| id == mn_id)
    }

    /// Get validator weight if present
    pub fn get_validator_weight(&self, mn_id: &str) -> Option<u64> {
        self.validators
            .iter()
            .find(|(id, _)| id == mn_id)
            .map(|(_, w)| *w)
    }

    /// Calculate voting threshold (67% of total weight)
    pub fn voting_threshold(&self) -> u64 {
        (self.total_weight * 67) / 100
    }
}
