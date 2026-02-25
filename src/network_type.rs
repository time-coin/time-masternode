//! Network type definitions for TIME Coin.
//!
//! Note: Some methods appear as "dead code" in library checks because they're
//! only used by the binary (main.rs). These include:
//! - `peer_discovery_url()` - used for network-specific API endpoint
//! - `magic_bytes()`, `address_prefix()` - used for protocol identification

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Copy, PartialEq, Eq, Hash)]
pub enum NetworkType {
    Mainnet,
    Testnet,
}

impl NetworkType {
    #[allow(dead_code)]
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

    #[allow(dead_code)]
    pub fn default_rpc_port(&self) -> u16 {
        match self {
            NetworkType::Mainnet => 24001,
            NetworkType::Testnet => 24101,
        }
    }

    #[allow(dead_code)]
    pub fn default_ws_port(&self) -> u16 {
        match self {
            NetworkType::Mainnet => 24002,
            NetworkType::Testnet => 24102,
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

    #[allow(dead_code)]
    pub fn address_prefix(&self) -> &str {
        match self {
            NetworkType::Mainnet => "TIME",
            NetworkType::Testnet => "TTIME",
        }
    }

    /// Get the peer discovery API URL for this network
    #[allow(dead_code)]
    pub fn peer_discovery_url(&self) -> &str {
        match self {
            NetworkType::Mainnet => "https://time-coin.io/api/peers",
            NetworkType::Testnet => "https://time-coin.io/api/testnet/peers",
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
