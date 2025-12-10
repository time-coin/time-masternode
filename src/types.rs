use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Hash256 = [u8; 32];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum NetworkType {
    Mainnet,
    Testnet,
}

impl NetworkType {
    #[allow(dead_code)]
    pub fn magic_bytes(&self) -> [u8; 4] {
        match self {
            NetworkType::Mainnet => [0xC0, 0x1D, 0x7E, 0x4D], // "COLD TIME"
            NetworkType::Testnet => [0x7E, 0x57, 0x7E, 0x4D], // "TEST TIME"
        }
    }

    #[allow(dead_code)]
    pub fn default_p2p_port(&self) -> u16 {
        match self {
            NetworkType::Mainnet => 24000,
            NetworkType::Testnet => 24100,
        }
    }

    #[allow(dead_code)]
    pub fn default_rpc_port(&self) -> u16 {
        match self {
            NetworkType::Mainnet => 24001,
            NetworkType::Testnet => 24101,
        }
    }

    #[allow(dead_code)]
    pub fn address_prefix(&self) -> &'static str {
        match self {
            NetworkType::Mainnet => "TIME1",
            NetworkType::Testnet => "TIME0",
        }
    }
}

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
    pub collateral: u64,
    pub public_key: VerifyingKey,
    pub tier: MasternodeTier,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MasternodeTier {
    Free = 0,       // Can receive rewards, cannot vote on governance
    Bronze = 1000,  // Can vote on governance, 10x rewards vs Free
    Silver = 10000, // Can vote on governance, 100x rewards vs Free
    Gold = 100000,  // Can vote on governance, 1000x rewards vs Free
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

    /// Reward weight for block reward distribution (proportional to collateral for fair APY)
    pub fn reward_weight(&self) -> u64 {
        match self {
            MasternodeTier::Free => 1,       // Minimal weight
            MasternodeTier::Bronze => 1000,  // Proportional to collateral
            MasternodeTier::Silver => 10000, // Proportional to collateral
            MasternodeTier::Gold => 100000,  // Proportional to collateral
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vote {
    pub txid: Hash256,
    pub voter: String,
    pub approve: bool,
    pub timestamp: i64,
    pub signature: Signature,
}
