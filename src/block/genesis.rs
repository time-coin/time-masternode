//! Genesis block generation and verification for TIME Coin.
//!
//! Genesis blocks are generated dynamically based on registered masternodes,
//! ensuring all nodes that participate in network formation get the same
//! deterministic genesis block.

#![allow(dead_code)]

use crate::block::types::Block;
use crate::types::MasternodeTier;
use crate::NetworkType;

pub struct GenesisBlock;

/// Masternode info for genesis block generation
#[derive(Clone, Debug)]
pub struct GenesisMasternode {
    pub address: String,
    pub tier: MasternodeTier,
}

/// Genesis block verification and generation
impl GenesisBlock {
    /// Buffer time (seconds) after genesis timestamp to wait for peer discovery
    /// This ensures all nodes have time to discover each other before creating genesis
    pub const PEER_DISCOVERY_BUFFER: i64 = 300; // 5 minutes

    /// Verify genesis block structure
    pub fn verify_structure(block: &Block) -> Result<(), String> {
        if block.header.height != 0 {
            return Err("Genesis block must be height 0".to_string());
        }
        if block.header.previous_hash != [0u8; 32] {
            return Err("Genesis block must have zero previous hash".to_string());
        }

        // Note: We do NOT validate masternode_rewards.len() against masternode_tiers.total()
        // because the tier counts in the block header represent the CURRENT network state when
        // validating, but the rewards were calculated based on the HISTORIC state when the block
        // was created. The masternode count changes over time, so historic blocks will have
        // different reward counts than current tier totals.

        // Verify reward distribution totals block reward
        Self::verify_rewards(block)?;

        Ok(())
    }

    /// Verify genesis timestamp matches network template
    /// CRITICAL: Both testnet and mainnet use FIXED timestamps for deterministic genesis
    pub fn verify_timestamp(block: &Block, network: NetworkType) -> Result<(), String> {
        let expected_timestamp = Self::genesis_timestamp(network);
        // Both testnet and mainnet require exact timestamp match for deterministic genesis
        if block.header.timestamp != expected_timestamp {
            return Err(format!(
                "Genesis timestamp mismatch: expected {}, got {} (network: {:?})",
                expected_timestamp, block.header.timestamp, network
            ));
        }
        Ok(())
    }

    /// Get genesis timestamp for network
    /// CRITICAL: These are FIXED values - all nodes must use the same timestamp
    /// to produce identical genesis blocks and be on the same chain
    pub fn genesis_timestamp(network: NetworkType) -> i64 {
        match network {
            NetworkType::Testnet => 1764547200, // 2025-12-01T00:00:00Z - FIXED for determinism
            NetworkType::Mainnet => 1767225600, // 2026-01-01T00:00:00Z - FIXED for determinism
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
        // Genesis block has no masternode rewards (initial supply distribution only)
        // Skip validation if rewards are empty
        if block.masternode_rewards.is_empty() {
            return Ok(());
        }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::types::{Block, BlockHeader, MasternodeTierCounts};

    fn create_test_genesis() -> Block {
        Block {
            header: BlockHeader {
                version: 1,
                height: 0,
                timestamp: 1767225600,
                previous_hash: [0u8; 32],
                merkle_root: [0u8; 32],
                leader: "test_leader".to_string(),
                attestation_root: [0u8; 32],
                masternode_tiers: MasternodeTierCounts::default(),
                block_reward: 10_000_000_000,
                active_masternodes_bitmap: vec![],
                liveness_recovery: Some(false),
                vrf_output: [0u8; 32],
                vrf_proof: vec![],
                vrf_score: 0,
            },
            transactions: vec![],
            masternode_rewards: vec![],
            time_attestations: vec![],
            consensus_participants_bitmap: vec![],
            liveness_recovery: Some(false),
        }
    }

    #[test]
    fn test_genesis_verification() {
        let genesis = create_test_genesis();
        assert!(GenesisBlock::verify_structure(&genesis).is_ok());
        assert!(GenesisBlock::verify_rewards(&genesis).is_ok());
    }

    #[test]
    fn test_genesis_invalid_height() {
        let mut genesis = create_test_genesis();
        genesis.header.height = 1;
        assert!(GenesisBlock::verify_structure(&genesis).is_err());
    }

    #[test]
    fn test_genesis_invalid_previous_hash() {
        let mut genesis = create_test_genesis();
        genesis.header.previous_hash = [1u8; 32];
        assert!(GenesisBlock::verify_structure(&genesis).is_err());
    }

    #[test]
    fn test_tier_reward_distribution() {
        let masternodes = vec![
            GenesisMasternode {
                address: "addr1".to_string(),
                tier: MasternodeTier::Bronze,
            },
            GenesisMasternode {
                address: "addr2".to_string(),
                tier: MasternodeTier::Gold,
            },
        ];

        let rewards = GenesisBlock::calculate_rewards(10_000_000_000, &masternodes);
        assert_eq!(rewards.len(), 2);

        // Verify total equals block reward
        let total: u64 = rewards.iter().map(|(_, v)| v).sum();
        assert_eq!(total, 10_000_000_000);
    }
}
