use crate::crypto::{ECVRFOutput, ECVRFProof};
use crate::types::{Hash256, Transaction};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub masternode_rewards: Vec<(String, u64)>,
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
    pub vrf_output: Option<ECVRFOutput>,
    pub vrf_proof: Option<ECVRFProof>,
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

        // Hash each transaction
        let mut hashes: Vec<Hash256> = self
            .transactions
            .iter()
            .map(|tx| {
                let bytes = bincode::serialize(tx).unwrap_or_default();
                Sha256::digest(&bytes).into()
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
                    // Duplicate last hash if odd number
                    hasher.update(chunk[0]);
                }
                next_level.push(hasher.finalize().into());
            }
            hashes = next_level;
        }

        hashes.into_iter().next().unwrap_or([0u8; 32])
    }
}
