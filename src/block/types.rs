use crate::types::{Hash256, Transaction};
use crate::vdf::VDFProof;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub masternode_rewards: Vec<(String, u64)>,
    pub vdf_proof: VDFProof,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub previous_hash: Hash256,
    pub merkle_root: Hash256,
    pub timestamp: i64,
    pub block_reward: u64,
}

impl Block {
    pub fn hash(&self) -> Hash256 {
        use sha2::{Digest, Sha256};
        let bytes =
            bincode::serialize(&self.header).expect("BlockHeader serialization must not fail");
        Sha256::digest(bytes).into()
    }
}

#[derive(Serialize, Deserialize)]
#[allow(dead_code)]
pub struct BlockConsensusResult {
    pub success: bool,
    pub local_block: Block,
    pub matched_peers: usize,
    pub total_peers: usize,
    pub reconciled: Option<Block>,
}
