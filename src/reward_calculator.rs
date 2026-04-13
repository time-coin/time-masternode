//! Deterministic block reward distribution calculator.
//!
//! Single source of truth for reward computation.  Both the block producer and
//! every validator call [`compute`] with the same inputs and MUST arrive at
//! identical outputs — any divergence is proof of a misbehaving producer.
//!
//! ## Inputs (all deterministic at or after `FREE_TIER_ONCHAIN_HEIGHT`)
//! - `active_nodes`: masternodes encoded in the block's `active_masternodes_bitmap`
//!   (= nodes that voted on the previous block).  Passed in already-decoded form.
//! - `fairness_map`: `blocks_without_reward` counters from in-memory on-chain history.
//! - `fees`: transaction fees committed in the block header.
//! - `total_reward`: `block.header.block_reward` (already validated to equal
//!   `base_reward + fees − treasury` by `validate_block_rewards`).
//! - `free_tier_registered`: set of on-chain-registered Free-tier node addresses
//!   (only enforced at/after `FREE_TIER_ONCHAIN_HEIGHT`; pass an empty set below that).
//!
//! ## Output
//! `Vec<(String, u64)>` — `(payout_address, satoshis)` pairs.
//! The producer entry is always first.  Entries with the same address are merged.
//!
//! ## Invariants
//! - Sum of all output amounts == `total_reward`.
//! - Producer entry.1 >= `PRODUCER_REWARD_SATOSHIS` (in tier-based mode).
//! - Every payout address appears at most once (duplicates merged).

use crate::constants::blockchain::{
    FREE_TIER_ONCHAIN_HEIGHT, MAX_FREE_TIER_RECIPIENTS, PRODUCER_REWARD_SATOSHIS,
};
use crate::masternode_registry::MasternodeInfo;
use crate::types::MasternodeTier;
use crate::utxo_manager::UTXOStateManager;
use std::collections::{HashMap, HashSet};

/// Returns the effective payout address for a masternode
/// (`reward_address` takes priority over `wallet_address`).
fn payout_addr(info: &MasternodeInfo) -> &str {
    if !info.reward_address.is_empty() {
        &info.reward_address
    } else {
        &info.masternode.wallet_address
    }
}

/// All deterministic inputs needed to compute a block's reward distribution.
pub struct RewardInput<'a> {
    /// Block height being produced / validated.
    pub height: u64,
    /// Payout address of the block producer (receives the leader bonus + fees).
    pub producer_wallet: &'a str,
    /// Active masternodes — decoded from `block.active_masternodes_bitmap`.
    /// For production: the set of nodes that voted on the previous block.
    /// For validation: the set decoded from the proposed block's bitmap.
    pub active_nodes: &'a [MasternodeInfo],
    /// On-chain `blocks_without_reward` counters (`get_reward_tracking_from_memory`).
    pub fairness_map: &'a HashMap<String, u64>,
    /// Transaction fees committed in the block (satoshis).
    pub fees: u64,
    /// `block.header.block_reward` — already independently validated as
    /// `base_reward + fees − treasury` by `validate_block_rewards`.
    pub total_reward: u64,
    /// On-chain-registered Free-tier node addresses for `FREE_TIER_ONCHAIN_HEIGHT` filter.
    /// Pass an empty `HashSet` when below `FREE_TIER_ONCHAIN_HEIGHT`.
    pub free_tier_registered: &'a HashSet<String>,
}

/// Compute the deterministic reward distribution for a block.
///
/// Returns `(payout_address, satoshis)` pairs.  The producer entry is first.
/// Returns an empty vec only if `active_nodes` is empty (no masternodes at all).
///
/// This function is intentionally async because paid-tier rewards are always
/// redirected to the collateral UTXO owner's address, which requires a UTXO
/// lookup to resolve (eliminates the economic incentive for collateral squatting).
pub async fn compute(input: &RewardInput<'_>, utxo_manager: &UTXOStateManager) -> Vec<(String, u64)> {
    let height = input.height;
    let producer_wallet = input.producer_wallet;
    let active_nodes = input.active_nodes;
    let fairness_map = input.fairness_map;
    let fees = input.fees;
    let total_reward = input.total_reward;

    if active_nodes.is_empty() {
        return vec![];
    }

    // Fairness bonus is the raw blocks_without_reward counter (v2 formula, always active).
    let bonus_for = |blocks_without: u64| -> u64 { blocks_without };

    // Whether any non-producer paid-tier node is active (determines distribution mode).
    let has_paid_tier_nodes = active_nodes.iter().any(|mn| {
        mn.masternode.tier != MasternodeTier::Free && payout_addr(mn) != producer_wallet
    });

    let apply_onchain_filter =
        height >= FREE_TIER_ONCHAIN_HEIGHT && !input.free_tier_registered.is_empty();

    let mut rewards: Vec<(String, u64)> = Vec::new();

    // Helper: push or merge into the rewards list.
    let push_reward = |rewards: &mut Vec<(String, u64)>, addr: String, amount: u64| {
        if let Some(entry) = rewards.iter_mut().find(|(a, _)| *a == addr) {
            entry.1 += amount;
        } else {
            rewards.push((addr, amount));
        }
    };

    if !has_paid_tier_nodes {
        // ── All-Free mode ──────────────────────────────────────────────────────
        // 95 TIME (= total_reward) split equally among the top-N Free nodes
        // sorted by fairness bonus.  Mirrors produce_block_at_height All-Free path.
        let mut free_nodes: Vec<(&MasternodeInfo, u64)> = active_nodes
            .iter()
            .filter(|mn| mn.masternode.tier == MasternodeTier::Free)
            .filter(|mn| {
                if apply_onchain_filter {
                    input.free_tier_registered.contains(&mn.masternode.address)
                } else {
                    true
                }
            })
            .map(|mn| {
                let blocks_without = fairness_map
                    .get(&mn.masternode.address)
                    .copied()
                    .unwrap_or(0);
                (mn, bonus_for(blocks_without))
            })
            .collect();

        free_nodes.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.masternode.address.cmp(&b.0.masternode.address))
        });

        let recipient_count = free_nodes.len().min(MAX_FREE_TIER_RECIPIENTS);
        if recipient_count == 0 {
            return rewards;
        }

        let per_node = total_reward / recipient_count as u64;
        let mut distributed = 0u64;
        for (i, (mn, _)) in free_nodes.iter().take(recipient_count).enumerate() {
            // Last recipient absorbs integer-division remainder.
            let share = if i == recipient_count - 1 {
                total_reward.saturating_sub(distributed)
            } else {
                per_node
            };
            push_reward(&mut rewards, payout_addr(mn).to_string(), share);
            distributed += share;
        }
        return rewards;
    }

    // ── Tier-based mode ──────────────────────────────────────────────────────
    // Producer gets 30 TIME leader bonus + all block fees.
    let producer_share = PRODUCER_REWARD_SATOSHIS + fees;
    push_reward(&mut rewards, producer_wallet.to_string(), producer_share);

    // Pre-build the set of paid-tier payout addresses to exclude wallet-overlap from
    // the Free distribution (mirrors produce_block_at_height `paid_tier_wallet_set`).
    let paid_tier_wallet_set: HashSet<String> = active_nodes
        .iter()
        .filter(|mn| mn.masternode.tier != MasternodeTier::Free)
        .map(|mn| payout_addr(mn).to_string())
        .collect();

    let tiers = [
        MasternodeTier::Gold,
        MasternodeTier::Silver,
        MasternodeTier::Bronze,
        MasternodeTier::Free,
    ];

    let mut rounding_dust = 0u64;

    for tier in &tiers {
        let tier_pool = tier.pool_allocation();
        let is_free = matches!(tier, MasternodeTier::Free);

        let mut tier_nodes: Vec<(&MasternodeInfo, u64)> = active_nodes
            .iter()
            .filter(|mn| mn.masternode.tier == *tier)
            // Producer already has their share; exclude them from tier pools.
            .filter(|mn| payout_addr(mn) != producer_wallet)
            // Free tier: drop wallets that are also a paid-tier payout address.
            .filter(|mn| {
                if is_free {
                    !paid_tier_wallet_set.contains(payout_addr(mn))
                } else {
                    true
                }
            })
            // Free tier: on-chain registration gate (active at FREE_TIER_ONCHAIN_HEIGHT).
            .filter(|mn| {
                if is_free && apply_onchain_filter {
                    input.free_tier_registered.contains(&mn.masternode.address)
                } else {
                    true
                }
            })
            .map(|mn| {
                let blocks_without = fairness_map
                    .get(&mn.masternode.address)
                    .copied()
                    .unwrap_or(0);
                (mn, bonus_for(blocks_without))
            })
            .collect();

        if tier_nodes.is_empty() {
            // Empty tier — full pool rolls up to producer.
            rewards[0].1 += tier_pool;
            continue;
        }

        tier_nodes.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.masternode.address.cmp(&b.0.masternode.address))
        });

        let recipient_count = if is_free {
            tier_nodes.len().min(MAX_FREE_TIER_RECIPIENTS)
        } else {
            1 // paid tier: single fairness-rotation winner
        };

        let per_node = tier_pool / recipient_count as u64;
        let mut distributed = 0u64;

        for (mn, _) in tier_nodes.iter().take(recipient_count) {
            // Paid-tier rewards always flow to the collateral UTXO owner's address,
            // not the registered wallet.  This eliminates the economic incentive for
            // collateral squatting (AV4): whoever owns the UTXO key receives the reward.
            let dest = if !is_free {
                if let Some(ref outpoint) = mn.masternode.collateral_outpoint {
                    match utxo_manager.get_utxo(outpoint).await {
                        Ok(utxo) if !utxo.address.is_empty() => utxo.address,
                        _ => payout_addr(mn).to_string(),
                    }
                } else {
                    payout_addr(mn).to_string()
                }
            } else {
                payout_addr(mn).to_string()
            };

            push_reward(&mut rewards, dest, per_node);
            distributed += per_node;
        }

        // Track rounding dust from integer division — added to producer at the end.
        rounding_dust += tier_pool.saturating_sub(distributed);
    }

    // Route rounding dust to the producer so sum(rewards) == total_reward exactly.
    if rounding_dust > 0 {
        rewards[0].1 += rounding_dust;
    }

    rewards
}

/// Normalise a reward list into a `HashMap<address, total_satoshis>`.
///
/// Merges duplicate address entries (can happen when two masternodes share a
/// wallet address) and strips zero-amount entries.  Used to compare expected
/// vs. actual rewards without caring about order or duplicate representation.
pub fn normalize(rewards: &[(String, u64)]) -> HashMap<String, u64> {
    let mut map: HashMap<String, u64> = HashMap::new();
    for (addr, amt) in rewards {
        if *amt > 0 {
            *map.entry(addr.clone()).or_insert(0) += amt;
        }
    }
    map
}
