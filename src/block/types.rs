//! Block types for the TIME Coin blockchain.
//!
//! Includes Proof-of-Time attestations which prove masternodes were online
//! and witnessed by peers during block production.

#![allow(dead_code)] // Attestation methods are scaffolding for future integration

use crate::types::{Hash256, Transaction};
use serde::{Deserialize, Serialize};

/// Proof-of-Time attestation included in blocks
/// This proves a masternode was online and witnessed by peers
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TimeAttestation {
    /// The masternode address being attested
    pub masternode_address: String,
    /// Sequence number of the heartbeat
    pub sequence_number: u64,
    /// Timestamp when the heartbeat was created
    pub heartbeat_timestamp: i64,
    /// The masternode's public key (32 bytes, hex-encoded for serialization)
    pub masternode_pubkey: String,
    /// Signature of the heartbeat by the masternode (64 bytes, hex-encoded)
    pub heartbeat_signature: String,
    /// List of witness attestations (address, pubkey, timestamp, signature)
    pub witnesses: Vec<WitnessRecord>,
}

/// A witness record proving another node saw the heartbeat
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WitnessRecord {
    /// Address of the witnessing masternode
    pub witness_address: String,
    /// Public key of the witness (hex-encoded)
    pub witness_pubkey: String,
    /// Timestamp when the witness saw the heartbeat
    pub witness_timestamp: i64,
    /// Witness signature over the heartbeat hash (hex-encoded)
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub masternode_rewards: Vec<(String, u64)>,
    /// Proof-of-Time: attestations proving masternodes were online
    #[serde(default)]
    pub time_attestations: Vec<TimeAttestation>,
}

/// Masternode counts by tier at time of block production
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MasternodeTierCounts {
    pub free: u32,
    pub bronze: u32,
    pub silver: u32,
    pub gold: u32,
}

impl MasternodeTierCounts {
    pub fn total(&self) -> u32 {
        self.free + self.bronze + self.silver + self.gold
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub previous_hash: Hash256,
    pub merkle_root: Hash256,
    pub timestamp: i64,
    pub block_reward: u64,
    pub leader: String,
    /// Root hash of the time attestations merkle tree
    #[serde(default)]
    pub attestation_root: Hash256,
    /// Masternode counts by tier at time of block production
    #[serde(default)]
    pub masternode_tiers: MasternodeTierCounts,
}

impl Block {
    pub fn hash(&self) -> Hash256 {
        use sha2::{Digest, Sha256};
        let bytes =
            bincode::serialize(&self.header).expect("BlockHeader serialization must not fail");
        Sha256::digest(bytes).into()
    }

    /// Compute the merkle root from the block's transactions
    pub fn compute_merkle_root(&self) -> Hash256 {
        use sha2::{Digest, Sha256};

        if self.transactions.is_empty() {
            return [0u8; 32]; // Empty merkle root for no transactions
        }

        // Hash each transaction using txid() for consistency with block generation
        // Sort by txid to ensure deterministic ordering across all nodes
        let mut hashes: Vec<(Hash256, Hash256)> = self
            .transactions
            .iter()
            .map(|tx| {
                let txid = tx.txid();
                (txid, txid) // (sort_key, hash)
            })
            .collect();

        // Sort by txid to ensure deterministic merkle root
        hashes.sort_by(|a, b| a.0.cmp(&b.0));

        // Extract just the hashes for merkle tree construction
        let mut hashes: Vec<Hash256> = hashes.into_iter().map(|(_, hash)| hash).collect();

        // Build merkle tree
        while hashes.len() > 1 {
            let mut next_level = Vec::new();
            for chunk in hashes.chunks(2) {
                let mut hasher = Sha256::new();
                hasher.update(chunk[0]);
                if chunk.len() > 1 {
                    hasher.update(chunk[1]);
                } else {
                    // Duplicate last hash if odd number
                    hasher.update(chunk[0]);
                }
                next_level.push(hasher.finalize().into());
            }
            hashes = next_level;
        }

        hashes.into_iter().next().unwrap_or([0u8; 32])
    }

    /// Compute the merkle root of time attestations
    pub fn compute_attestation_root(&self) -> Hash256 {
        use sha2::{Digest, Sha256};

        if self.time_attestations.is_empty() {
            return [0u8; 32];
        }

        // Hash each attestation
        let mut hashes: Vec<Hash256> = self
            .time_attestations
            .iter()
            .map(|att| {
                let mut hasher = Sha256::new();
                hasher.update(att.masternode_address.as_bytes());
                hasher.update(att.sequence_number.to_le_bytes());
                hasher.update(att.heartbeat_timestamp.to_le_bytes());
                hasher.finalize().into()
            })
            .collect();

        // Build merkle tree
        while hashes.len() > 1 {
            let mut next_level = Vec::new();
            for chunk in hashes.chunks(2) {
                let mut hasher = Sha256::new();
                hasher.update(chunk[0]);
                if chunk.len() > 1 {
                    hasher.update(chunk[1]);
                } else {
                    hasher.update(chunk[0]);
                }
                next_level.push(hasher.finalize().into());
            }
            hashes = next_level;
        }

        hashes.into_iter().next().unwrap_or([0u8; 32])
    }

    /// Get count of masternodes with valid attestations in this block
    pub fn attested_masternode_count(&self) -> usize {
        self.time_attestations.len()
    }

    /// Check if a specific masternode has an attestation in this block
    pub fn has_attestation_for(&self, address: &str) -> bool {
        self.time_attestations
            .iter()
            .any(|a| a.masternode_address == address)
    }
}

impl TimeAttestation {
    /// Minimum witnesses required for a valid attestation
    pub const MIN_WITNESSES: usize = 2;

    /// Check if this attestation has enough witnesses
    pub fn is_valid(&self) -> bool {
        self.witnesses.len() >= Self::MIN_WITNESSES
    }

    /// Get unique witness count
    pub fn unique_witness_count(&self) -> usize {
        use std::collections::HashSet;
        self.witnesses
            .iter()
            .map(|w| &w.witness_address)
            .collect::<HashSet<_>>()
            .len()
    }
}
