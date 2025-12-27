//! Block types for the TIME Coin blockchain.
//!
//! Includes Proof-of-Time attestations which prove masternodes were online
//! and witnessed by peers during block production.

#![allow(dead_code)] // Attestation methods are scaffolding for future integration

use crate::types::{Hash256, Transaction};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Build a merkle tree from a list of hashes
/// Generic merkle root calculator used for transactions, attestations, etc.
fn build_merkle_root(mut hashes: Vec<Hash256>) -> Hash256 {
    if hashes.is_empty() {
        return [0u8; 32];
    }

    while hashes.len() > 1 {
        if hashes.len() % 2 == 1 {
            hashes.push(*hashes.last().unwrap());
        }
        hashes = hashes
            .chunks(2)
            .map(|pair| {
                let mut hasher = Sha256::new();
                hasher.update(pair[0]);
                hasher.update(pair[1]);
                hasher.finalize().into()
            })
            .collect();
    }
    hashes[0]
}

/// Calculate merkle root from transactions
/// This is the canonical implementation used by both block generation and validation
pub fn calculate_merkle_root(txs: &[Transaction]) -> Hash256 {
    let hashes: Vec<Hash256> = txs.iter().map(|tx| tx.txid()).collect();
    build_merkle_root(hashes)
}

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

    /// Compute the merkle root of time attestations
    pub fn compute_attestation_root(&self) -> Hash256 {
        let hashes: Vec<Hash256> = self
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

        build_merkle_root(hashes)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TxInput, TxOutput};

    fn create_test_tx(seed: u8) -> Transaction {
        Transaction {
            version: 1,
            inputs: vec![TxInput {
                previous_output: crate::types::OutPoint {
                    txid: [seed; 32],
                    vout: 0,
                },
                script_sig: vec![seed, seed + 1, seed + 2],
                sequence: 0xFFFFFFFF,
            }],
            outputs: vec![TxOutput {
                value: seed as u64 * 100,
                script_pubkey: vec![seed + 10, seed + 11, seed + 12],
            }],
            lock_time: 0,
            timestamp: seed as i64 * 1000,
        }
    }

    /// CRITICAL TEST: Verifies that merkle roots are deterministic regardless of transaction order
    /// This is the root cause of the fork issue in production logs
    #[test]
    fn test_merkle_root_determinism_across_transaction_orders() {
        let tx1 = create_test_tx(1);
        let tx2 = create_test_tx(2);
        let tx3 = create_test_tx(3);

        // Create three blocks with same transactions in different orders
        let block1 = Block {
            header: BlockHeader {
                version: 1,
                height: 100,
                timestamp: 1000,
                previous_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                block_reward: 0,
                leader: "test".to_string(),
                attestation_root: [0u8; 32],
                masternode_tiers: Default::default(),
            },
            transactions: vec![tx1.clone(), tx2.clone(), tx3.clone()],
            masternode_rewards: vec![],
            time_attestations: vec![],
        };

        let block2 = Block {
            header: block1.header.clone(),
            transactions: vec![tx3.clone(), tx1.clone(), tx2.clone()],
            masternode_rewards: vec![],
            time_attestations: vec![],
        };

        let block3 = Block {
            header: block1.header.clone(),
            transactions: vec![tx2.clone(), tx3.clone(), tx1.clone()],
            masternode_rewards: vec![],
            time_attestations: vec![],
        };

        // Compute merkle roots
        let merkle1 = calculate_merkle_root(&block1.transactions);
        let merkle2 = calculate_merkle_root(&block2.transactions);
        let merkle3 = calculate_merkle_root(&block3.transactions);

        // CRITICAL: All merkle roots MUST be identical
        assert_eq!(
            merkle1,
            merkle2,
            "Merkle root MUST be deterministic! Order [1,2,3] vs [3,1,2]: {} vs {}",
            hex::encode(merkle1),
            hex::encode(merkle2)
        );
        assert_eq!(
            merkle1,
            merkle3,
            "Merkle root MUST be deterministic! Order [1,2,3] vs [2,3,1]: {} vs {}",
            hex::encode(merkle1),
            hex::encode(merkle3)
        );

        println!(
            "✅ Merkle root determinism verified: {}",
            hex::encode(merkle1)
        );
    }

    #[test]
    fn test_empty_block_merkle_root() {
        let block = Block {
            header: BlockHeader {
                version: 1,
                height: 100,
                timestamp: 1000,
                previous_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                block_reward: 0,
                leader: "test".to_string(),
                attestation_root: [0u8; 32],
                masternode_tiers: Default::default(),
            },
            transactions: vec![],
            masternode_rewards: vec![],
            time_attestations: vec![],
        };

        let merkle = calculate_merkle_root(&block.transactions);
        assert_eq!(
            merkle, [0u8; 32],
            "Empty block should have zero merkle root"
        );
    }

    #[test]
    fn test_single_transaction_merkle_equals_txid() {
        let tx = create_test_tx(42);
        let block = Block {
            header: BlockHeader {
                version: 1,
                height: 100,
                timestamp: 1000,
                previous_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                block_reward: 0,
                leader: "test".to_string(),
                attestation_root: [0u8; 32],
                masternode_tiers: Default::default(),
            },
            transactions: vec![tx.clone()],
            masternode_rewards: vec![],
            time_attestations: vec![],
        };

        let merkle = calculate_merkle_root(&block.transactions);
        let txid = tx.txid();

        assert_eq!(
            merkle, txid,
            "Single transaction merkle root should equal txid"
        );
    }
}
