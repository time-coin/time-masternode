//! Block types for the TIME Coin blockchain.
//!
//! Includes Proof-of-Time attestations which prove masternodes were online
//! and witnessed by peers during block production.

#![allow(dead_code)] // Attestation methods are scaffolding for future integration

use crate::types::{Hash256, Transaction};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

/// Custom deserializer for time_attestations to handle legacy block formats
/// Old blocks may have: Vec<TimeAttestation>, Option<Vec<TimeAttestation>>, or missing field
#[allow(deprecated)]
fn deserialize_time_attestations<'de, D>(deserializer: D) -> Result<Vec<TimeAttestation>, D::Error>
where
    D: Deserializer<'de>,
{
    // Try to deserialize as Option<Vec> first (handles both Some(vec) and None)
    let opt: Option<Vec<TimeAttestation>> = Option::deserialize(deserializer).unwrap_or(None);
    Ok(opt.unwrap_or_default())
}

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
///
/// DEPRECATED: Heartbeat system removed - kept for backward compatibility with old blocks
#[deprecated(note = "Heartbeat system removed - will be removed in protocol v2")]
#[allow(deprecated)]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TimeAttestation {
    pub masternode_address: String,
    pub sequence_number: u64,
    pub heartbeat_timestamp: i64,
    pub masternode_pubkey: String,
    pub heartbeat_signature: String,
    pub witnesses: Vec<WitnessRecord>,
}

/// DEPRECATED: Part of removed heartbeat system - kept for backward compatibility
#[deprecated(note = "Heartbeat system removed - will be removed in protocol v2")]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WitnessRecord {
    pub witness_address: String,
    pub witness_pubkey: String,
    pub witness_timestamp: i64,
    pub signature: String,
}

/// Old block header format from before active_masternodes_bitmap and liveness_recovery fields were added
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct BlockHeaderV1 {
    pub version: u32,
    pub height: u64,
    pub previous_hash: Hash256,
    pub merkle_root: Hash256,
    pub timestamp: i64,
    pub block_reward: u64,
    #[serde(default)]
    pub leader: String,
    #[serde(default)]
    pub attestation_root: Hash256,
    #[serde(default)]
    pub masternode_tiers: MasternodeTierCounts,
    #[serde(default)]
    pub vrf_proof: Vec<u8>,
    #[serde(default)]
    pub vrf_output: Hash256,
    #[serde(default)]
    pub vrf_score: u64,
}

impl From<BlockHeaderV1> for BlockHeader {
    fn from(v1: BlockHeaderV1) -> Self {
        BlockHeader {
            version: v1.version,
            height: v1.height,
            previous_hash: v1.previous_hash,
            merkle_root: v1.merkle_root,
            timestamp: v1.timestamp,
            block_reward: v1.block_reward,
            leader: v1.leader,
            attestation_root: v1.attestation_root,
            masternode_tiers: v1.masternode_tiers,
            vrf_proof: v1.vrf_proof,
            vrf_output: v1.vrf_output,
            vrf_score: v1.vrf_score,
            active_masternodes_bitmap: vec![], // Not present in old blocks
            liveness_recovery: None,           // Not present in old blocks
        }
    }
}

/// Old block format from before active_masternodes_bitmap and liveness_recovery fields were added
/// Used for deserializing legacy blocks from storage
#[allow(deprecated)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockV1 {
    pub header: BlockHeaderV1,
    pub transactions: Vec<Transaction>,
    pub masternode_rewards: Vec<(String, u64)>,
    #[serde(default)]
    pub time_attestations: Vec<TimeAttestation>,
    #[serde(default)]
    pub consensus_participants: Vec<String>,
}

impl From<BlockV1> for Block {
    fn from(v1: BlockV1) -> Self {
        Block {
            header: v1.header.into(),
            transactions: v1.transactions,
            masternode_rewards: v1.masternode_rewards,
            time_attestations: vec![], // Always empty in new blocks
            consensus_participants: v1.consensus_participants,
            consensus_participants_bitmap: vec![], // Not present in old blocks
            liveness_recovery: None,               // Not present in old blocks
        }
    }
}

#[allow(deprecated)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub masternode_rewards: Vec<(String, u64)>,
    /// DEPRECATED: Heartbeat attestations - kept for deserializing old blocks
    /// Always empty in new blocks but field must exist for backward compatibility
    /// Uses custom deserializer to handle both Vec and Option<Vec> formats from old blocks
    #[serde(default, deserialize_with = "deserialize_time_attestations")]
    pub time_attestations: Vec<TimeAttestation>,
    /// DEPRECATED: List of masternodes that participated in consensus
    /// Use consensus_participants_bitmap instead for new blocks
    #[serde(default)]
    pub consensus_participants: Vec<String>,
    /// Compact bitmap of consensus participants (1 bit per registered masternode)
    /// Space savings: 10,000 masternodes = 1,250 bytes vs ~200KB for address list
    #[serde(default)]
    pub consensus_participants_bitmap: Vec<u8>,
    /// §7.6 Liveness Fallback: Flag indicating this block resolved stalled transactions
    #[serde(default)]
    pub liveness_recovery: Option<bool>,
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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub previous_hash: Hash256,
    pub merkle_root: Hash256,
    pub timestamp: i64,
    pub block_reward: u64,
    #[serde(default)]
    pub leader: String,
    /// Root hash of the time attestations merkle tree
    #[serde(default)]
    pub attestation_root: Hash256,
    /// Masternode counts by tier at time of block production
    #[serde(default)]
    pub masternode_tiers: MasternodeTierCounts,
    /// VRF proof generated by block leader (typically 64 bytes signature)
    #[serde(default)]
    pub vrf_proof: Vec<u8>,
    /// VRF output hash - deterministic randomness derived from proof
    #[serde(default)]
    pub vrf_output: Hash256,
    /// VRF score derived from output (for chain comparison)
    #[serde(default)]
    pub vrf_score: u64,
    /// Compact bitmap indicating which masternodes are active (1 bit per masternode)
    /// Masternodes are in deterministic order (sorted by address)
    /// Bit = 1 means active (connected or recently produced block)
    /// Bit = 0 means inactive
    /// Space: 10,000 masternodes = 1,250 bytes (vs 200KB for address list)
    #[serde(default)]
    pub active_masternodes_bitmap: Vec<u8>,
    /// §7.6 Liveness Fallback: Flag indicating this TimeLock block resolved stalled transactions
    /// Wrapped in Option for backward compatibility with pre-v6.2 blocks
    #[serde(default)]
    pub liveness_recovery: Option<bool>,
}

impl Block {
    pub fn hash(&self) -> Hash256 {
        use sha2::{Digest, Sha256};

        // Hash only the consensus-critical fields, excluding masternode_tiers
        // which is metadata that changes over time and should not affect block identity
        let mut hasher = Sha256::new();
        hasher.update(self.header.version.to_le_bytes());
        hasher.update(self.header.height.to_le_bytes());
        hasher.update(self.header.previous_hash);
        hasher.update(self.header.merkle_root);
        hasher.update(self.header.timestamp.to_le_bytes());
        hasher.update(self.header.block_reward.to_le_bytes());
        hasher.update(self.header.leader.as_bytes());
        hasher.update(self.header.attestation_root);
        // VRF fields for deterministic chain comparison
        // NOTE: vrf_proof is NOT included (it's a proof OF the output)
        hasher.update(self.header.vrf_output);
        hasher.update(self.header.vrf_score.to_le_bytes());
        // Include active masternodes bitmap (consensus-critical field)
        hasher.update(&self.header.active_masternodes_bitmap);
        // Explicitly NOT including masternode_tiers - it's metadata only

        hasher.finalize().into()
    }

    /// Add VRF proof to this block using the block leader's signing key
    ///
    /// This should be called after block creation but before broadcasting.
    /// It generates a cryptographic VRF proof that proves the block leader
    /// was legitimately selected for this slot.
    ///
    /// # Arguments
    /// * `signing_key` - Block leader's ed25519 signing key
    ///
    /// # Returns
    /// Ok(()) if VRF was successfully generated and added
    pub fn add_vrf(&mut self, signing_key: &ed25519_dalek::SigningKey) -> Result<(), String> {
        use crate::block::vrf::generate_block_vrf;

        let (vrf_proof, vrf_output, vrf_score) =
            generate_block_vrf(signing_key, self.header.height, &self.header.previous_hash);

        self.header.vrf_proof = vrf_proof;
        self.header.vrf_output = vrf_output;
        self.header.vrf_score = vrf_score;

        Ok(())
    }

    /// Verify the VRF proof in this block
    ///
    /// This should be called during block validation to ensure the block leader
    /// was legitimately selected and didn't manipulate the randomness.
    ///
    /// # Arguments
    /// * `verifying_key` - Block leader's ed25519 public key
    ///
    /// # Returns
    /// Ok(()) if VRF proof is valid or block predates VRF (proof empty)
    pub fn verify_vrf(&self, verifying_key: &ed25519_dalek::VerifyingKey) -> Result<(), String> {
        use crate::block::vrf::verify_block_vrf;

        verify_block_vrf(
            verifying_key,
            self.header.height,
            &self.header.previous_hash,
            &self.header.vrf_proof,
            &self.header.vrf_output,
        )
    }

    /// Get count of masternodes with valid attestations in this block
    pub fn attested_masternode_count(&self) -> usize {
        0 // Heartbeat system removed
    }

    /// Check if a specific masternode has an attestation in this block
    pub fn has_attestation_for(&self, _address: &str) -> bool {
        false // Heartbeat system removed
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

    /// CRITICAL TEST: Verifies that canonical sorting produces deterministic merkle roots
    /// Determinism comes from ALL nodes using the same canonical order:
    /// [coinbase (empty inputs), sorted user txs by txid]
    #[test]
    fn test_merkle_root_determinism_across_transaction_orders() {
        let tx1 = create_test_tx(1);
        let tx2 = create_test_tx(2);
        let tx3 = create_test_tx(3);

        // Simulate three different orderings of the same transactions
        let order1 = vec![tx1.clone(), tx2.clone(), tx3.clone()];
        let order2 = vec![tx3.clone(), tx1.clone(), tx2.clone()];
        let order3 = vec![tx2.clone(), tx3.clone(), tx1.clone()];

        // Apply canonical sorting (coinbase first, then sorted by txid)
        fn canonical_sort(txs: &[Transaction]) -> Vec<Transaction> {
            let mut coinbase = Vec::new();
            let mut user_txs = Vec::new();
            for tx in txs {
                if tx.inputs.is_empty() {
                    coinbase.push(tx.clone());
                } else {
                    user_txs.push(tx.clone());
                }
            }
            user_txs.sort_by_key(|tx| tx.txid());
            coinbase.extend(user_txs);
            coinbase
        }

        let sorted1 = canonical_sort(&order1);
        let sorted2 = canonical_sort(&order2);
        let sorted3 = canonical_sort(&order3);

        // Compute merkle roots after canonical sorting
        let merkle1 = calculate_merkle_root(&sorted1);
        let merkle2 = calculate_merkle_root(&sorted2);
        let merkle3 = calculate_merkle_root(&sorted3);

        // CRITICAL: All merkle roots MUST be identical after canonical sorting
        assert_eq!(
            merkle1,
            merkle2,
            "Merkle root MUST be deterministic after canonical sort! {} vs {}",
            hex::encode(merkle1),
            hex::encode(merkle2)
        );
        assert_eq!(
            merkle1,
            merkle3,
            "Merkle root MUST be deterministic after canonical sort! {} vs {}",
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
                ..Default::default()
            },
            transactions: vec![],
            masternode_rewards: vec![],
            consensus_participants: vec![],
            consensus_participants_bitmap: vec![],
            liveness_recovery: Some(false),
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
                ..Default::default()
            },
            transactions: vec![tx.clone()],
            masternode_rewards: vec![],
            consensus_participants: vec![],
            consensus_participants_bitmap: vec![],
            liveness_recovery: Some(false),
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
