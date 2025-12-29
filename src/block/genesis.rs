//! Genesis block generation and verification for TIME Coin.
//!
//! Genesis blocks are dynamically generated based on active masternodes,
//! ensuring fair reward distribution from the start of the network.
//!
//! The genesis timestamp is fixed per network (from template), but
//! masternode tiers and rewards are set at runtime based on participants.

#![allow(dead_code)]

use crate::block::types::{Block, BlockHeader, MasternodeTierCounts};
use crate::types::{MasternodeTier, Transaction};
use crate::NetworkType;
use serde::{Deserialize, Serialize};
use serde_json::json;

pub struct GenesisBlock;

/// Genesis template loaded from JSON file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisTemplate {
    pub network: String,
    pub version: u32,
    pub message: String,
    pub block: GenesisBlockTemplate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisBlockTemplate {
    pub header: GenesisHeaderTemplate,
    pub transactions: Vec<serde_json::Value>,
    pub masternode_rewards: Vec<serde_json::Value>,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisHeaderTemplate {
    pub block_number: u64,
    pub timestamp: String,
    pub timestamp_unix: i64,
    pub previous_hash: String,
    pub merkle_root: String,
    pub masternode_counts: MasternodeCountsTemplate,
    pub block_reward: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasternodeCountsTemplate {
    pub free: u32,
    pub bronze: u32,
    pub silver: u32,
    pub gold: u32,
}

/// Masternode info for genesis block generation
#[derive(Clone, Debug)]
pub struct GenesisMasternode {
    pub address: String,
    pub tier: MasternodeTier,
}

/// Genesis block verification and generation
impl GenesisBlock {
    /// Minimum masternodes required to generate genesis
    pub const MIN_MASTERNODES_FOR_GENESIS: usize = 3;

    /// Buffer time (seconds) after genesis timestamp to wait for peer discovery
    /// This ensures all nodes have time to discover each other before creating genesis
    pub const PEER_DISCOVERY_BUFFER: i64 = 300; // 5 minutes

    /// Load canonical genesis block from JSON file
    /// This ensures all nodes use the exact same genesis block
    pub fn load_from_file(network: NetworkType) -> Result<Block, String> {
        let filename = match network {
            NetworkType::Testnet => "genesis.testnet.json",
            NetworkType::Mainnet => "genesis.mainnet.json",
        };

        // Try multiple locations
        let mut paths = vec![filename.to_string(), format!("./{}", filename)];

        // Add executable directory
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                paths.push(exe_dir.join(filename).to_string_lossy().to_string());
            }
        }

        // Add data directory (most likely location)
        if let Ok(data_dir) = crate::config::Config::get_data_directory(network) {
            paths.push(data_dir.join(filename).to_string_lossy().to_string());
        }

        // Add common system locations
        paths.extend([
            format!("/etc/timecoin/{}", filename),
            format!("/usr/local/share/timecoin/{}", filename),
            format!("/root/{}", filename),
        ]);

        // Add home directory
        if let Ok(home) = std::env::var("HOME") {
            paths.push(format!("{}/.timecoin/{}", home, filename));
            paths.push(format!("{}/{}", home, filename));
            // Also try network-specific subdirectory
            let network_dir = match network {
                NetworkType::Testnet => "testnet",
                NetworkType::Mainnet => "mainnet",
            };
            paths.push(format!("{}/.timecoin/{}/{}", home, network_dir, filename));
        }

        for path in &paths {
            tracing::debug!("Trying to load genesis from: {}", path);
            if let Ok(content) = std::fs::read_to_string(path) {
                match serde_json::from_str::<Block>(&content) {
                    Ok(block) => {
                        tracing::info!("✓ Loaded genesis block from {}", path);
                        tracing::info!("  Hash: {}", hex::encode(block.hash()));
                        tracing::info!("  Timestamp: {}", block.header.timestamp);
                        tracing::info!("  Masternodes: {}", block.masternode_rewards.len());
                        return Ok(block);
                    }
                    Err(e) => {
                        tracing::warn!("⚠️  Failed to parse {}: {}", path, e);
                        continue;
                    }
                }
            }
        }

        Err(format!(
            "Genesis file {} not found in any of {} locations tried",
            filename,
            paths.len()
        ))
    }

    /// Load genesis template from JSON file
    pub fn load_template(network: NetworkType) -> Result<GenesisTemplate, String> {
        let filename = match network {
            NetworkType::Testnet => "genesis.testnet.json",
            NetworkType::Mainnet => "genesis.mainnet.json",
        };

        // Try current directory first, then common locations
        let paths = [
            filename.to_string(),
            format!("./{}", filename),
            format!("/etc/timecoin/{}", filename),
            format!("~/.timecoin/{}", filename),
        ];

        for path in &paths {
            if let Ok(content) = std::fs::read_to_string(path) {
                return serde_json::from_str(&content)
                    .map_err(|e| format!("Failed to parse {}: {}", path, e));
            }
        }

        // If no file found, return default template
        Ok(Self::default_template(network))
    }

    /// Default template if JSON file not found
    fn default_template(network: NetworkType) -> GenesisTemplate {
        let (timestamp, timestamp_unix, message) = match network {
            NetworkType::Testnet => (
                "2025-12-01T00:00:00Z",
                1764547200i64,
                "TIME Coin Testnet Relaunch - December 1, 2025 - TSDC + Avalanche Consensus",
            ),
            NetworkType::Mainnet => (
                "2026-01-01T00:00:00Z",
                1767225600i64,
                "TIME Coin Mainnet Launch - January 1, 2026 - TSDC + Avalanche Consensus",
            ),
        };

        GenesisTemplate {
            network: match network {
                NetworkType::Testnet => "testnet".to_string(),
                NetworkType::Mainnet => "mainnet".to_string(),
            },
            version: 2,
            message: message.to_string(),
            block: GenesisBlockTemplate {
                header: GenesisHeaderTemplate {
                    block_number: 0,
                    timestamp: timestamp.to_string(),
                    timestamp_unix,
                    previous_hash:
                        "0000000000000000000000000000000000000000000000000000000000000000"
                            .to_string(),
                    merkle_root: "dynamic".to_string(),
                    masternode_counts: MasternodeCountsTemplate {
                        free: 0,
                        bronze: 0,
                        silver: 0,
                        gold: 0,
                    },
                    block_reward: 10_000_000_000,
                },
                transactions: vec![],
                masternode_rewards: vec![],
                hash: "dynamic".to_string(),
            },
        }
    }

    /// Verify genesis block structure
    pub fn verify_structure(block: &Block) -> Result<(), String> {
        if block.header.height != 0 {
            return Err("Genesis block must be height 0".to_string());
        }
        if block.header.previous_hash != [0u8; 32] {
            return Err("Genesis block must have zero previous hash".to_string());
        }
        if block.transactions.is_empty() {
            return Err("Genesis block must have coinbase transaction".to_string());
        }

        // Verify masternode rewards match tier counts
        let tier_counts = &block.header.masternode_tiers;
        let total_masternodes = tier_counts.total() as usize;

        if block.masternode_rewards.len() != total_masternodes {
            return Err(format!(
                "Masternode rewards count {} doesn't match tier total {}",
                block.masternode_rewards.len(),
                total_masternodes
            ));
        }

        // Verify reward distribution totals block reward
        Self::verify_rewards(block)?;

        Ok(())
    }

    /// Verify genesis timestamp matches network template
    pub fn verify_timestamp(block: &Block, network: NetworkType) -> Result<(), String> {
        let expected_timestamp = Self::genesis_timestamp(network);
        if block.header.timestamp != expected_timestamp {
            return Err(format!(
                "Genesis timestamp mismatch: expected {}, got {}",
                expected_timestamp, block.header.timestamp
            ));
        }
        Ok(())
    }

    /// Generate genesis block with active masternodes
    /// CRITICAL: masternodes MUST be pre-sorted by address for determinism
    pub fn generate_with_masternodes(
        network: NetworkType,
        masternodes: Vec<GenesisMasternode>,
        leader: &str,
    ) -> Block {
        // Validate input is sorted for determinism
        #[cfg(debug_assertions)]
        {
            for i in 1..masternodes.len() {
                assert!(
                    masternodes[i - 1].address <= masternodes[i].address,
                    "Masternodes must be sorted by address for deterministic genesis generation"
                );
            }
        }

        // Load template to get timestamp and other settings
        let template =
            Self::load_template(network).unwrap_or_else(|_| Self::default_template(network));
        let genesis_timestamp = template.block.header.timestamp_unix;
        let block_reward = template.block.header.block_reward;

        // Count masternodes by tier
        let mut tier_counts = MasternodeTierCounts::default();
        for mn in &masternodes {
            match mn.tier {
                MasternodeTier::Free => tier_counts.free += 1,
                MasternodeTier::Bronze => tier_counts.bronze += 1,
                MasternodeTier::Silver => tier_counts.silver += 1,
                MasternodeTier::Gold => tier_counts.gold += 1,
            }
        }

        // Calculate reward distribution
        let masternode_rewards = Self::calculate_rewards(block_reward, &masternodes);

        // Coinbase transaction marker (empty outputs - rewards are in masternode_rewards)
        let coinbase = Transaction {
            version: 1,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            timestamp: genesis_timestamp,
        };

        Block {
            header: BlockHeader {
                version: 2,
                height: 0,
                previous_hash: [0u8; 32],
                merkle_root: coinbase.txid(),
                timestamp: genesis_timestamp,
                block_reward,
                leader: leader.to_string(),
                attestation_root: [0u8; 32],
                masternode_tiers: tier_counts,
            },
            transactions: vec![coinbase],
            masternode_rewards,
            time_attestations: vec![],
        }
    }

    /// Get genesis timestamp for network
    pub fn genesis_timestamp(network: NetworkType) -> i64 {
        match network {
            NetworkType::Testnet => 1764547200, // 2025-12-01T00:00:00Z
            NetworkType::Mainnet => 1767225600, // 2026-01-01T00:00:00Z
        }
    }

    /// Get block reward for network
    pub fn block_reward(network: NetworkType) -> u64 {
        match network {
            NetworkType::Testnet => 10_000_000_000, // 100 TIME in satoshis
            NetworkType::Mainnet => 10_000_000_000, // 100 TIME in satoshis
        }
    }

    /// Calculate reward distribution based on masternode tiers
    /// CRITICAL: Input must be pre-sorted by address for determinism
    /// Returns rewards in the same sorted order
    pub fn calculate_rewards(
        total_reward: u64,
        masternodes: &[GenesisMasternode],
    ) -> Vec<(String, u64)> {
        if masternodes.is_empty() {
            return vec![];
        }

        // Calculate total weight using tier's reward_weight
        let total_weight: u64 = masternodes.iter().map(|mn| mn.tier.reward_weight()).sum();

        if total_weight == 0 {
            return vec![];
        }

        // Distribute rewards proportionally
        // Since input is pre-sorted, output will maintain sorted order
        let mut rewards = Vec::new();
        let mut distributed = 0u64;

        for (i, mn) in masternodes.iter().enumerate() {
            let share = if i == masternodes.len() - 1 {
                // Last masternode (alphabetically last) gets remainder to avoid rounding errors
                total_reward - distributed
            } else {
                (total_reward * mn.tier.reward_weight()) / total_weight
            };
            rewards.push((mn.address.clone(), share));
            distributed += share;
        }

        rewards
    }

    /// Verify reward distribution is correct
    pub fn verify_rewards(block: &Block) -> Result<(), String> {
        let total_reward = block.header.block_reward;
        let distributed: u64 = block.masternode_rewards.iter().map(|(_, v)| v).sum();

        if distributed != total_reward {
            return Err(format!(
                "Reward distribution {} doesn't match block reward {}",
                distributed, total_reward
            ));
        }

        Ok(())
    }

    /// Export genesis block as JSON
    #[allow(dead_code)]
    pub fn export_json(block: &Block, network: NetworkType) -> String {
        let block_hash = block.hash();
        let network_str = match network {
            NetworkType::Mainnet => "mainnet",
            NetworkType::Testnet => "testnet",
        };

        json!({
            "network": network_str,
            "version": 2,
            "message": format!(
                "TIME Coin {} Genesis Block - TSDC + Avalanche Consensus",
                if matches!(network, NetworkType::Mainnet) { "Mainnet" } else { "Testnet" }
            ),
            "block": {
                "header": {
                    "version": block.header.version,
                    "height": block.header.height,
                    "timestamp": chrono::DateTime::from_timestamp(block.header.timestamp, 0)
                        .unwrap()
                        .format("%Y-%m-%dT%H:%M:%SZ")
                        .to_string(),
                    "timestamp_unix": block.header.timestamp,
                    "previous_hash": hex::encode(block.header.previous_hash),
                    "merkle_root": hex::encode(block.header.merkle_root),
                    "block_reward": block.header.block_reward,
                    "leader": block.header.leader,
                    "attestation_root": hex::encode(block.header.attestation_root),
                    "masternode_tiers": {
                        "free": block.header.masternode_tiers.free,
                        "bronze": block.header.masternode_tiers.bronze,
                        "silver": block.header.masternode_tiers.silver,
                        "gold": block.header.masternode_tiers.gold,
                    }
                },
                "transactions": block.transactions.iter().map(|tx| {
                    json!({
                        "txid": hex::encode(tx.txid()),
                        "version": tx.version,
                        "inputs": tx.inputs,
                        "outputs": tx.outputs.iter().map(|o| {
                            json!({
                                "value": o.value,
                                "script_pubkey": hex::encode(&o.script_pubkey),
                            })
                        }).collect::<Vec<_>>(),
                        "lock_time": tx.lock_time,
                        "timestamp": tx.timestamp,
                    })
                }).collect::<Vec<_>>(),
                "masternode_rewards": block.masternode_rewards.iter().map(|(addr, amount)| {
                    json!({
                        "address": addr,
                        "amount": amount,
                    })
                }).collect::<Vec<_>>(),
                "hash": hex::encode(block_hash),
            }
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_with_masternodes() {
        let masternodes = vec![
            GenesisMasternode {
                address: "TIME0abc123".to_string(),
                tier: MasternodeTier::Free,
            },
            GenesisMasternode {
                address: "TIME0def456".to_string(),
                tier: MasternodeTier::Free,
            },
            GenesisMasternode {
                address: "TIME0ghi789".to_string(),
                tier: MasternodeTier::Free,
            },
        ];

        let genesis = GenesisBlock::generate_with_masternodes(
            NetworkType::Testnet,
            masternodes,
            "TIME0abc123",
        );

        assert_eq!(genesis.header.height, 0);
        assert_eq!(genesis.header.previous_hash, [0u8; 32]);
        assert_eq!(genesis.header.masternode_tiers.free, 3);
        assert_eq!(genesis.masternode_rewards.len(), 3);

        // Verify reward distribution
        let total: u64 = genesis.masternode_rewards.iter().map(|(_, v)| v).sum();
        assert_eq!(total, genesis.header.block_reward);
    }

    #[test]
    fn test_genesis_deterministic() {
        let masternodes = vec![GenesisMasternode {
            address: "TIME0abc123".to_string(),
            tier: MasternodeTier::Free,
        }];

        let genesis1 = GenesisBlock::generate_with_masternodes(
            NetworkType::Testnet,
            masternodes.clone(),
            "TIME0abc123",
        );
        let genesis2 = GenesisBlock::generate_with_masternodes(
            NetworkType::Testnet,
            masternodes,
            "TIME0abc123",
        );

        assert_eq!(genesis1.hash(), genesis2.hash());
    }

    #[test]
    fn test_genesis_deterministic_with_sorting() {
        // Test that sorted input produces same result regardless of initial order
        let mut masternodes1 = vec![
            GenesisMasternode {
                address: "TIME0aaa".to_string(),
                tier: MasternodeTier::Free,
            },
            GenesisMasternode {
                address: "TIME0bbb".to_string(),
                tier: MasternodeTier::Bronze,
            },
            GenesisMasternode {
                address: "TIME0ccc".to_string(),
                tier: MasternodeTier::Silver,
            },
        ];

        let mut masternodes2 = vec![
            GenesisMasternode {
                address: "TIME0ccc".to_string(),
                tier: MasternodeTier::Silver,
            },
            GenesisMasternode {
                address: "TIME0aaa".to_string(),
                tier: MasternodeTier::Free,
            },
            GenesisMasternode {
                address: "TIME0bbb".to_string(),
                tier: MasternodeTier::Bronze,
            },
        ];

        // Sort both
        masternodes1.sort_by(|a, b| a.address.cmp(&b.address));
        masternodes2.sort_by(|a, b| a.address.cmp(&b.address));

        let genesis1 =
            GenesisBlock::generate_with_masternodes(NetworkType::Testnet, masternodes1, "TIME0aaa");
        let genesis2 =
            GenesisBlock::generate_with_masternodes(NetworkType::Testnet, masternodes2, "TIME0aaa");

        // Blocks should be identical
        assert_eq!(genesis1.hash(), genesis2.hash(), "Block hashes must match");
        assert_eq!(
            genesis1.masternode_rewards, genesis2.masternode_rewards,
            "Masternode rewards must be identical"
        );

        // Verify rewards are sorted by address
        for i in 1..genesis1.masternode_rewards.len() {
            assert!(
                genesis1.masternode_rewards[i - 1].0 <= genesis1.masternode_rewards[i].0,
                "Rewards must be sorted by address"
            );
        }
    }

    #[test]
    fn test_genesis_verification() {
        let masternodes = vec![
            GenesisMasternode {
                address: "TIME0abc123".to_string(),
                tier: MasternodeTier::Free,
            },
            GenesisMasternode {
                address: "TIME0def456".to_string(),
                tier: MasternodeTier::Bronze,
            },
        ];

        let genesis = GenesisBlock::generate_with_masternodes(
            NetworkType::Testnet,
            masternodes,
            "TIME0abc123",
        );

        assert!(GenesisBlock::verify_structure(&genesis).is_ok());
        assert!(GenesisBlock::verify_rewards(&genesis).is_ok());
    }

    #[test]
    fn test_tier_reward_distribution() {
        // 1 Free (1x) + 1 Bronze (2x) = 3 total weight
        // Free gets 1/3, Bronze gets 2/3
        let mut masternodes = vec![
            GenesisMasternode {
                address: "TIME0free".to_string(),
                tier: MasternodeTier::Free,
            },
            GenesisMasternode {
                address: "TIME0bronze".to_string(),
                tier: MasternodeTier::Bronze,
            },
        ];

        // Sort for deterministic generation
        masternodes.sort_by(|a, b| a.address.cmp(&b.address));

        let genesis = GenesisBlock::generate_with_masternodes(
            NetworkType::Testnet,
            masternodes,
            "TIME0bronze",
        );

        let free_reward = genesis
            .masternode_rewards
            .iter()
            .find(|(a, _)| a == "TIME0free")
            .unwrap()
            .1;
        let bronze_reward = genesis
            .masternode_rewards
            .iter()
            .find(|(a, _)| a == "TIME0bronze")
            .unwrap()
            .1;

        // Bronze should get roughly 2x free's reward
        assert!(bronze_reward > free_reward);
        assert_eq!(free_reward + bronze_reward, genesis.header.block_reward);
    }
}
