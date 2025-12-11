use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Copy, PartialEq, Eq, Hash)]
pub enum NetworkType {
    Mainnet,
    Testnet,
}

impl NetworkType {
    pub fn magic_bytes(&self) -> [u8; 4] {
        match self {
            NetworkType::Mainnet => [0xC0, 0x1D, 0x7E, 0x4D], // "COLD TIME"
            NetworkType::Testnet => [0x54, 0x49, 0x4D, 0x45], // "TIME" in ASCII
        }
    }

    pub fn default_p2p_port(&self) -> u16 {
        match self {
            NetworkType::Mainnet => 24000,
            NetworkType::Testnet => 24100,
        }
    }

    pub fn default_rpc_port(&self) -> u16 {
        match self {
            NetworkType::Mainnet => 24001,
            NetworkType::Testnet => 24101,
        }
    }

    #[allow(dead_code)]
    pub fn genesis_timestamp(&self) -> i64 {
        match self {
            NetworkType::Mainnet => 1767225600, // 2026-01-01 00:00:00 UTC
            NetworkType::Testnet => 1764547200, // 2025-12-01 00:00:00 UTC
        }
    }

    #[allow(dead_code)]
    pub fn genesis_message(&self) -> &str {
        match self {
            NetworkType::Mainnet => "TIME Coin - Where Every Second Counts",
            NetworkType::Testnet => "TIME Coin Testnet - 10 Minute Blocks, Instant Finality",
        }
    }

    pub fn address_prefix(&self) -> &str {
        match self {
            NetworkType::Mainnet => "TIME",
            NetworkType::Testnet => "TTIME",
        }
    }
}

impl std::fmt::Display for NetworkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkType::Mainnet => write!(f, "Mainnet"),
            NetworkType::Testnet => write!(f, "Testnet"),
        }
    }
}
