//! Tiered masternode authority system for canonical chain selection
//!
//! This module implements a hierarchical approach to determine which chain
//! should be considered canonical during fork situations by examining which
//! tier of masternodes support each competing chain.
//!
//! Authority hierarchy (highest to lowest):
//! 1. Gold masternodes
//! 2. Silver masternodes
//! 3. Bronze masternodes
//! 4. Whitelisted Free masternodes
//! 5. Regular Free masternodes

use crate::masternode_registry::MasternodeInfo;
use crate::types::MasternodeTier;
use std::collections::HashMap;

/// Authority level for chain selection decisions
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthorityLevel {
    /// No masternode support
    None = 0,
    /// Only regular free nodes
    RegularFree = 1,
    /// Whitelisted free nodes
    WhitelistedFree = 2,
    /// Bronze tier masternodes
    Bronze = 3,
    /// Silver tier masternodes
    Silver = 4,
    /// Gold tier masternodes
    Gold = 5,
}

/// Analysis of masternode support for a chain
#[derive(Debug, Clone)]
pub struct ChainAuthorityAnalysis {
    /// Highest tier supporting this chain
    pub highest_tier: AuthorityLevel,
    /// Count of nodes at each tier
    pub gold_count: usize,
    pub silver_count: usize,
    pub bronze_count: usize,
    pub whitelisted_free_count: usize,
    pub regular_free_count: usize,
    /// Total nodes supporting this chain
    pub total_count: usize,
    /// Weighted authority score (higher tier = exponentially higher weight)
    pub authority_score: u64,
}

impl ChainAuthorityAnalysis {
    /// Create analysis from masternode list and their whitelist status
    pub fn from_masternodes(
        masternodes: &[&MasternodeInfo],
        whitelist_status: &HashMap<String, bool>,
    ) -> Self {
        let mut gold_count = 0;
        let mut silver_count = 0;
        let mut bronze_count = 0;
        let mut whitelisted_free_count = 0;
        let mut regular_free_count = 0;

        for mn in masternodes {
            let is_whitelisted = whitelist_status
                .get(&mn.masternode.address)
                .copied()
                .unwrap_or(false);

            match mn.masternode.tier {
                MasternodeTier::Gold => gold_count += 1,
                MasternodeTier::Silver => silver_count += 1,
                MasternodeTier::Bronze => bronze_count += 1,
                MasternodeTier::Free => {
                    if is_whitelisted {
                        whitelisted_free_count += 1;
                    } else {
                        regular_free_count += 1;
                    }
                }
            }
        }

        let highest_tier = if gold_count > 0 {
            AuthorityLevel::Gold
        } else if silver_count > 0 {
            AuthorityLevel::Silver
        } else if bronze_count > 0 {
            AuthorityLevel::Bronze
        } else if whitelisted_free_count > 0 {
            AuthorityLevel::WhitelistedFree
        } else if regular_free_count > 0 {
            AuthorityLevel::RegularFree
        } else {
            AuthorityLevel::None
        };

        // Calculate weighted authority score
        // Gold = 1000x, Silver = 100x, Bronze = 10x, Whitelisted = 2x, Regular = 1x
        let authority_score = (gold_count as u64 * 1000)
            + (silver_count as u64 * 100)
            + (bronze_count as u64 * 10)
            + (whitelisted_free_count as u64 * 2)
            + (regular_free_count as u64);

        let total_count =
            gold_count + silver_count + bronze_count + whitelisted_free_count + regular_free_count;

        Self {
            highest_tier,
            gold_count,
            silver_count,
            bronze_count,
            whitelisted_free_count,
            regular_free_count,
            total_count,
            authority_score,
        }
    }

    /// Compare two chains and determine which has higher authority
    /// Returns:
    ///   - Some(true) if self has higher authority
    ///   - Some(false) if other has higher authority
    ///   - None if authority is equal (use other tiebreakers)
    pub fn compare_authority(&self, other: &Self) -> Option<bool> {
        // First: Compare highest tier present
        if self.highest_tier != other.highest_tier {
            return Some(self.highest_tier > other.highest_tier);
        }

        // Second: If same highest tier, compare authority score
        if self.authority_score != other.authority_score {
            return Some(self.authority_score > other.authority_score);
        }

        // Third: If same authority score, compare total count
        if self.total_count != other.total_count {
            return Some(self.total_count > other.total_count);
        }

        // Equal authority - use other tiebreakers (chain work, hash, etc.)
        None
    }

    /// Format authority analysis for logging
    pub fn format_summary(&self) -> String {
        format!(
            "Authority={:?} (G:{} S:{} B:{} WF:{} RF:{} | score:{} total:{})",
            self.highest_tier,
            self.gold_count,
            self.silver_count,
            self.bronze_count,
            self.whitelisted_free_count,
            self.regular_free_count,
            self.authority_score,
            self.total_count
        )
    }
}

/// Determine canonical chain based on masternode authority
pub struct CanonicalChainSelector;

impl CanonicalChainSelector {
    /// Determine if we should switch to peer's chain based on masternode authority
    ///
    /// This is the PRIMARY decision mechanism - it overrides chain work, height, etc.
    /// when there's a clear authority difference.
    #[allow(clippy::too_many_arguments)]
    pub fn should_switch_to_peer_chain(
        our_analysis: &ChainAuthorityAnalysis,
        peer_analysis: &ChainAuthorityAnalysis,
        our_chain_work: u128,
        peer_chain_work: u128,
        our_height: u64,
        peer_height: u64,
        our_tip_hash: &[u8; 32],
        peer_tip_hash: &[u8; 32],
    ) -> (bool, String) {
        // Step 1: Compare masternode authority (HIGHEST PRIORITY)
        if let Some(peer_has_higher_authority) = peer_analysis.compare_authority(our_analysis) {
            if peer_has_higher_authority {
                return (
                    true,
                    format!(
                        "SWITCH: Peer has higher masternode authority. Ours: {} | Peer: {}",
                        our_analysis.format_summary(),
                        peer_analysis.format_summary()
                    ),
                );
            } else {
                return (
                    false,
                    format!(
                        "KEEP: Our chain has higher masternode authority. Ours: {} | Peer: {}",
                        our_analysis.format_summary(),
                        peer_analysis.format_summary()
                    ),
                );
            }
        }

        // Step 2: Equal authority - use chain work
        if peer_chain_work != our_chain_work {
            if peer_chain_work > our_chain_work {
                return (
                    true,
                    format!(
                        "SWITCH: Equal authority, peer has more chain work ({} > {})",
                        peer_chain_work, our_chain_work
                    ),
                );
            } else {
                return (
                    false,
                    format!(
                        "KEEP: Equal authority, our chain has more work ({} > {})",
                        our_chain_work, peer_chain_work
                    ),
                );
            }
        }

        // Step 3: Equal authority and work - use height
        if peer_height != our_height {
            if peer_height > our_height {
                return (
                    true,
                    format!(
                        "SWITCH: Equal authority and work, peer is longer ({} > {})",
                        peer_height, our_height
                    ),
                );
            } else {
                return (
                    false,
                    format!(
                        "KEEP: Equal authority and work, our chain is longer ({} > {})",
                        our_height, peer_height
                    ),
                );
            }
        }

        // Step 4: Equal everything - use deterministic hash tiebreaker
        if peer_tip_hash < our_tip_hash {
            (
                true,
                "SWITCH: All metrics equal, peer hash is smaller (deterministic tiebreaker)".to_string(),
            )
        } else {
            (
                false,
                "KEEP: All metrics equal, our hash is smaller or equal (deterministic tiebreaker)".to_string(),
            )
        }
    }

    /// Analyze our own chain's masternode support
    /// This looks at which masternodes are connected to us and presumably supporting our chain
    pub async fn analyze_our_chain_authority(
        masternode_registry: &crate::masternode_registry::MasternodeRegistry,
        connection_manager: Option<&crate::network::connection_manager::ConnectionManager>,
        peer_registry: Option<&crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) -> ChainAuthorityAnalysis {
        // Get all active masternodes
        let active_masternodes = masternode_registry.list_active().await;

        // Filter to only connected masternodes (they support our chain)
        let connected_masternodes: Vec<&MasternodeInfo> = if let Some(cm) = connection_manager {
            active_masternodes
                .iter()
                .filter(|mn| cm.is_connected(&mn.masternode.address))
                .collect()
        } else {
            // If no connection manager, assume all active support us
            active_masternodes.iter().collect()
        };

        // Build whitelist status map
        let mut whitelist_status = HashMap::new();
        if let Some(pr) = peer_registry {
            for mn in &connected_masternodes {
                let is_whitelisted = pr.is_whitelisted(&mn.masternode.address).await;
                whitelist_status.insert(mn.masternode.address.clone(), is_whitelisted);
            }
        }

        ChainAuthorityAnalysis::from_masternodes(&connected_masternodes, &whitelist_status)
    }

    /// Analyze peer's chain authority by examining which masternodes are connected to them
    pub async fn analyze_peer_chain_authority(
        peer_supporting_masternodes: &[String], // IPs of masternodes supporting peer
        masternode_registry: &crate::masternode_registry::MasternodeRegistry,
        peer_registry: Option<&crate::network::peer_connection_registry::PeerConnectionRegistry>,
    ) -> ChainAuthorityAnalysis {
        // Get all active masternodes
        let active_masternodes = masternode_registry.list_active().await;

        // Filter to only masternodes in the supporting list
        let supporting_set: std::collections::HashSet<String> =
            peer_supporting_masternodes.iter().cloned().collect();

        let supporting_masternodes: Vec<&MasternodeInfo> = active_masternodes
            .iter()
            .filter(|mn| supporting_set.contains(&mn.masternode.address))
            .collect();

        // Build whitelist status map
        let mut whitelist_status = HashMap::new();
        if let Some(pr) = peer_registry {
            for mn in &supporting_masternodes {
                let is_whitelisted = pr.is_whitelisted(&mn.masternode.address).await;
                whitelist_status.insert(mn.masternode.address.clone(), is_whitelisted);
            }
        }

        ChainAuthorityAnalysis::from_masternodes(&supporting_masternodes, &whitelist_status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authority_hierarchy() {
        // Gold beats everything
        assert!(AuthorityLevel::Gold > AuthorityLevel::Silver);
        assert!(AuthorityLevel::Gold > AuthorityLevel::Bronze);
        assert!(AuthorityLevel::Gold > AuthorityLevel::WhitelistedFree);
        assert!(AuthorityLevel::Gold > AuthorityLevel::RegularFree);

        // Silver beats bronze and free
        assert!(AuthorityLevel::Silver > AuthorityLevel::Bronze);
        assert!(AuthorityLevel::Silver > AuthorityLevel::WhitelistedFree);

        // Bronze beats free tiers
        assert!(AuthorityLevel::Bronze > AuthorityLevel::WhitelistedFree);
        assert!(AuthorityLevel::Bronze > AuthorityLevel::RegularFree);

        // Whitelisted free beats regular free
        assert!(AuthorityLevel::WhitelistedFree > AuthorityLevel::RegularFree);
    }

    #[test]
    fn test_authority_comparison() {
        let gold_chain = ChainAuthorityAnalysis {
            highest_tier: AuthorityLevel::Gold,
            gold_count: 1,
            silver_count: 0,
            bronze_count: 0,
            whitelisted_free_count: 0,
            regular_free_count: 0,
            total_count: 1,
            authority_score: 1000,
        };

        let silver_chain = ChainAuthorityAnalysis {
            highest_tier: AuthorityLevel::Silver,
            gold_count: 0,
            silver_count: 5,
            bronze_count: 0,
            whitelisted_free_count: 0,
            regular_free_count: 0,
            total_count: 5,
            authority_score: 500,
        };

        // Gold beats silver even with fewer nodes
        assert_eq!(gold_chain.compare_authority(&silver_chain), Some(true));
        assert_eq!(silver_chain.compare_authority(&gold_chain), Some(false));
    }

    #[test]
    fn test_authority_score_tiebreaker() {
        let chain_a = ChainAuthorityAnalysis {
            highest_tier: AuthorityLevel::Bronze,
            gold_count: 0,
            silver_count: 0,
            bronze_count: 2,
            whitelisted_free_count: 0,
            regular_free_count: 0,
            total_count: 2,
            authority_score: 20, // 2 * 10
        };

        let chain_b = ChainAuthorityAnalysis {
            highest_tier: AuthorityLevel::Bronze,
            gold_count: 0,
            silver_count: 0,
            bronze_count: 1,
            whitelisted_free_count: 5,
            regular_free_count: 0,
            total_count: 6,
            authority_score: 20, // (1 * 10) + (5 * 2)
        };

        // Same highest tier, same authority score - should use total count
        let result = chain_a.compare_authority(&chain_b);
        // chain_b has more total nodes (6 vs 2)
        assert_eq!(result, Some(false));
    }
}
