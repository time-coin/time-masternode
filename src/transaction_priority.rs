//! Transaction priority system based on tiered masternode rankings
//!
//! This module implements a transaction priority queue that gives preference
//! to transactions submitted by higher-tier masternodes while maintaining
//! fair access for all participants.
//!
//! Priority hierarchy:
//! 1. Gold tier masternodes - Highest priority
//! 2. Silver tier masternodes - High priority
//! 3. Bronze tier masternodes - Medium priority
//! 4. Whitelisted Free tier masternodes - Low priority
//! 5. Regular Free tier masternodes - Base priority
//! 6. Non-masternode transactions - Lowest priority (fee-based only)
//!
//! The system balances:
//! - Rewarding high-tier masternode operators
//! - Maintaining network fairness
//! - Preventing abuse and spam
//! - Ensuring fee market efficiency

#![allow(dead_code)]

use crate::masternode_registry::MasternodeRegistry;
use crate::network::connection_manager::ConnectionManager;
use crate::types::{MasternodeTier, Transaction};
use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Instant;

/// Transaction priority score
/// Higher scores = higher priority
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriorityScore {
    /// Tier-based priority (0-4)
    tier_score: u32,
    /// Fee per byte in satoshis
    fee_per_byte: u64,
    /// Transaction age (for tie-breaking)
    age_secs: u64,
}

impl PriorityScore {
    /// Calculate priority score from tier and fee
    pub fn calculate(
        tier: Option<MasternodeTier>,
        is_whitelisted: bool,
        fee: u64,
        tx_size: usize,
        age: u64,
    ) -> Self {
        // Tier scoring:
        // Gold = 4, Silver = 3, Bronze = 2, Whitelisted Free = 1, Free = 0, Non-masternode = 0
        let tier_score = match tier {
            Some(MasternodeTier::Gold) => 4,
            Some(MasternodeTier::Silver) => 3,
            Some(MasternodeTier::Bronze) => 2,
            Some(MasternodeTier::Free) if is_whitelisted => 1,
            _ => 0,
        };

        let fee_per_byte = if tx_size > 0 { fee / tx_size as u64 } else { 0 };

        Self {
            tier_score,
            fee_per_byte,
            age_secs: age,
        }
    }

    /// Get composite priority value for comparison
    /// Format: (tier_score * 1e12) + (fee_per_byte * 1e6) + age_secs
    /// This ensures tier dominates, then fee, then age
    pub fn composite_value(&self) -> u128 {
        (self.tier_score as u128 * 1_000_000_000_000)
            + (self.fee_per_byte as u128 * 1_000_000)
            + (self.age_secs as u128)
    }
}

impl Eq for PriorityScore {}

impl PartialOrd for PriorityScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityScore {
    fn cmp(&self, other: &Self) -> Ordering {
        self.composite_value().cmp(&other.composite_value())
    }
}

/// Transaction with priority metadata
#[derive(Debug, Clone)]
pub struct PrioritizedTransaction {
    pub tx: Transaction,
    pub fee: u64,
    pub priority: PriorityScore,
    pub added_at: Instant,
    pub submitter_ip: Option<String>,
}

impl PartialEq for PrioritizedTransaction {
    fn eq(&self, other: &Self) -> bool {
        self.tx.txid() == other.tx.txid()
    }
}

impl Eq for PrioritizedTransaction {}

impl PartialOrd for PrioritizedTransaction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedTransaction {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first (reverse order for max-heap)
        self.priority.cmp(&other.priority)
    }
}

/// Priority-based transaction selection system
pub struct TransactionPriorityQueue {
    masternode_registry: Arc<MasternodeRegistry>,
    connection_manager: Arc<ConnectionManager>,
}

impl TransactionPriorityQueue {
    pub fn new(
        masternode_registry: Arc<MasternodeRegistry>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            masternode_registry,
            connection_manager,
        }
    }

    /// Determine the tier and whitelist status of a transaction submitter
    pub async fn get_submitter_tier(&self, ip: &str) -> (Option<MasternodeTier>, bool) {
        // Get all active masternodes
        let masternodes = self.masternode_registry.get_active_masternodes().await;

        // Find masternode matching this IP
        let masternode_info = masternodes.iter().find(|mn| {
            let mn_ip = mn
                .masternode
                .address
                .split(':')
                .next()
                .unwrap_or(&mn.masternode.address);
            mn_ip == ip
        });

        match masternode_info {
            Some(info) => {
                let is_whitelisted = self.connection_manager.is_whitelisted(ip);
                (Some(info.masternode.tier), is_whitelisted)
            }
            None => {
                // Not a masternode, check if whitelisted anyway
                let is_whitelisted = self.connection_manager.is_whitelisted(ip);
                (None, is_whitelisted)
            }
        }
    }

    /// Determine the highest tier of submitter from a peer IP address string
    pub async fn get_highest_tier_from_ip(&self, peer_ip: &str) -> Option<MasternodeTier> {
        // Parse IP address from "ip:port" format
        let ip_only = peer_ip.split(':').next().unwrap_or(peer_ip);

        // Get tier for this IP
        let (tier, _) = self.get_submitter_tier(ip_only).await;
        tier
    }

    /// Calculate priority score for a transaction
    pub async fn calculate_priority(
        &self,
        tx: &Transaction,
        fee: u64,
        submitter_ip: Option<&str>,
        added_at: Instant,
    ) -> PriorityScore {
        let tx_size = bincode::serialized_size(tx).unwrap_or(1) as usize;
        let age_secs = added_at.elapsed().as_secs();

        let (tier, is_whitelisted) = match submitter_ip {
            Some(ip) => self.get_submitter_tier(ip).await,
            None => (None, false),
        };

        PriorityScore::calculate(tier, is_whitelisted, fee, tx_size, age_secs)
    }

    /// Select transactions for block inclusion based on priority
    /// Returns up to `max_count` transactions, prioritizing higher tiers
    pub async fn select_for_block(
        &self,
        transactions: Vec<(Transaction, u64, Option<String>, Instant)>,
        max_count: usize,
        max_size_bytes: usize,
    ) -> Vec<Transaction> {
        let mut prioritized: Vec<PrioritizedTransaction> = Vec::new();

        // Calculate priority for each transaction
        for (tx, fee, submitter_ip, added_at) in transactions {
            let priority = self
                .calculate_priority(&tx, fee, submitter_ip.as_deref(), added_at)
                .await;

            prioritized.push(PrioritizedTransaction {
                tx,
                fee,
                priority,
                added_at,
                submitter_ip,
            });
        }

        // Sort by priority (highest first)
        prioritized.sort_by(|a, b| b.cmp(a));

        // Select transactions up to limits
        let mut selected = Vec::new();
        let mut total_size = 0usize;

        for ptx in prioritized {
            if selected.len() >= max_count {
                break;
            }

            let tx_size = bincode::serialized_size(&ptx.tx).unwrap_or(1) as usize;
            if total_size + tx_size > max_size_bytes {
                continue;
            }

            selected.push(ptx.tx);
            total_size += tx_size;
        }

        selected
    }

    /// Get priority statistics for monitoring
    pub async fn get_tier_distribution(
        &self,
        transactions: &[(Transaction, u64, Option<String>, Instant)],
    ) -> TierDistribution {
        let mut dist = TierDistribution::default();

        for (tx, fee, submitter_ip, added_at) in transactions {
            let priority = self
                .calculate_priority(tx, *fee, submitter_ip.as_deref(), *added_at)
                .await;

            match priority.tier_score {
                4 => dist.gold += 1,
                3 => dist.silver += 1,
                2 => dist.bronze += 1,
                1 => dist.whitelisted_free += 1,
                _ => dist.regular += 1,
            }
        }

        dist
    }
}

/// Distribution of transactions by tier
#[derive(Debug, Default, Clone)]
pub struct TierDistribution {
    pub gold: usize,
    pub silver: usize,
    pub bronze: usize,
    pub whitelisted_free: usize,
    pub regular: usize,
}

impl TierDistribution {
    pub fn total(&self) -> usize {
        self.gold + self.silver + self.bronze + self.whitelisted_free + self.regular
    }
}

/// Helper function to determine highest tier present in the network
pub async fn get_highest_active_tier(
    masternode_registry: &MasternodeRegistry,
    connection_manager: &ConnectionManager,
) -> Option<MasternodeTier> {
    let masternodes = masternode_registry
        .get_connected_active_masternodes(connection_manager)
        .await;

    if masternodes.is_empty() {
        return None;
    }

    // Find highest tier
    let mut highest: Option<MasternodeTier> = None;

    for mn in masternodes {
        let tier = mn.masternode.tier;
        highest = match highest {
            None => Some(tier),
            Some(MasternodeTier::Free) => Some(tier),
            Some(MasternodeTier::Bronze)
                if matches!(tier, MasternodeTier::Silver | MasternodeTier::Gold) =>
            {
                Some(tier)
            }
            Some(MasternodeTier::Silver) if matches!(tier, MasternodeTier::Gold) => Some(tier),
            Some(current) => Some(current),
        };
    }

    highest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_score_ordering() {
        // Gold tier should beat silver even with lower fee
        let gold = PriorityScore::calculate(Some(MasternodeTier::Gold), false, 100, 1000, 0);
        let silver = PriorityScore::calculate(Some(MasternodeTier::Silver), false, 1000, 1000, 0);
        assert!(gold > silver);

        // Within same tier, higher fee wins
        let high_fee = PriorityScore::calculate(Some(MasternodeTier::Bronze), false, 1000, 1000, 0);
        let low_fee = PriorityScore::calculate(Some(MasternodeTier::Bronze), false, 100, 1000, 0);
        assert!(high_fee > low_fee);

        // Whitelisted free beats non-whitelisted
        let whitelisted = PriorityScore::calculate(Some(MasternodeTier::Free), true, 100, 1000, 0);
        let regular = PriorityScore::calculate(None, false, 100, 1000, 0);
        assert!(whitelisted > regular);
    }

    #[test]
    fn test_priority_score_tie_breaking() {
        // Same tier and fee, older transaction wins
        let older = PriorityScore::calculate(Some(MasternodeTier::Bronze), false, 100, 1000, 100);
        let newer = PriorityScore::calculate(Some(MasternodeTier::Bronze), false, 100, 1000, 50);
        assert!(older > newer);
    }

    #[test]
    fn test_tier_hierarchy() {
        let gold = PriorityScore::calculate(Some(MasternodeTier::Gold), false, 1, 1000, 0);
        let silver = PriorityScore::calculate(Some(MasternodeTier::Silver), false, 1, 1000, 0);
        let bronze = PriorityScore::calculate(Some(MasternodeTier::Bronze), false, 1, 1000, 0);
        let whitelisted_free =
            PriorityScore::calculate(Some(MasternodeTier::Free), true, 1, 1000, 0);
        let free = PriorityScore::calculate(Some(MasternodeTier::Free), false, 1, 1000, 0);
        let non_mn = PriorityScore::calculate(None, false, 1, 1000, 0);

        assert!(gold > silver);
        assert!(silver > bronze);
        assert!(bronze > whitelisted_free);
        assert!(whitelisted_free > free);
        assert!(free >= non_mn); // Free masternodes same as non-masternodes if not whitelisted
    }
}
