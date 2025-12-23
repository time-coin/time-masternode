use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Hash256 = [u8; 32];

// Constants
#[allow(dead_code)]
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
        let bytes = bincode::serialize(self).expect("Serialization should succeed");
        Sha256::digest(bytes).into()
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
