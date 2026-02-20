# Masternode Reward Rotation System

**Version:** 2.0  
**Date:** February 20, 2026  
**Status:** Active

---

## Overview

TimeCoin distributes block rewards using **per-tier pools with fairness rotation**. Each masternode tier (Gold, Silver, Bronze, Free) has a dedicated reward allocation. Within each tier, a fairness bonus ensures every node gets paid in turn — no starvation, no micro-transactions.

## Problem Statement

### Single Pool Limitations
- **Weight-proportional single pool**: With 10,000 Free nodes sharing a pool with 5 Gold nodes, Free nodes receive dust (< 0.01 TIME)
- **Micro-transactions**: Thousands of tiny UTXOs bloat the chain
- **No guaranteed income**: Low-weight nodes may never reach the minimum payout threshold

### Solution: Per-Tier Pools + Fairness Rotation
Each tier gets a dedicated pool. Rewards are split equally among selected nodes within a tier. When a tier has more nodes than the per-block cap, fairness rotation selects the longest-waiting nodes first.

---

## Reward Structure (§10.4)

```
Total Reward = 100 TIME + transaction_fees

Distribution:
  Block Producer:  35 TIME + fees  (VRF-selected leader bonus)
  Gold pool:       25 TIME         (shared equally among Gold nodes)
  Silver pool:     18 TIME         (shared equally among Silver nodes)
  Bronze pool:     14 TIME         (shared equally among Bronze nodes)
  Free pool:        8 TIME         (shared equally among Free nodes)

The block producer also receives their tier's pool share (merged into one output).
Empty tier pool → goes to block producer.
```

---

## Fairness Rotation Algorithm

### 1. Eligibility

A masternode is eligible for its tier's pool if:
1. **Registered** and **active** in the masternode registry
2. **Maturity gate** (Free tier only, mainnet): registered for ≥72 blocks (~12 hours)
3. Paid tiers (Bronze/Silver/Gold): always eligible (collateral = sybil resistance)

### 2. Fairness Bonus Calculation

```
blocks_without_pool_reward = blocks since this node last appeared in masternode_rewards
fairness_bonus = min(blocks_without_pool_reward / 10, 20)
```

- Computed on-chain by scanning `masternode_rewards` in recent blocks (up to 1000 blocks back)
- Deterministic: all nodes independently derive the same values
- Capped at 20 to prevent unbounded growth

### 3. Selection Per Tier

For each tier:
1. Collect all eligible nodes of that tier
2. Compute fairness_bonus for each
3. Sort by fairness_bonus **DESC**, then address **ASC** (deterministic tiebreak)
4. Select top `MAX_TIER_RECIPIENTS` (25) nodes
5. Distribute `tier_pool / recipient_count` equally to each

```rust
// Pseudocode for per-tier distribution
for tier in [Gold, Silver, Bronze, Free] {
    let tier_pool = tier.pool_allocation();
    let mut nodes = get_eligible_nodes(tier);
    
    nodes.sort_by(|a, b| b.fairness_bonus.cmp(&a.fairness_bonus)
        .then(a.address.cmp(&b.address)));
    
    let recipients = nodes[..min(25, nodes.len())];
    let per_node = tier_pool / recipients.len();
    
    for node in recipients {
        distribute(node, per_node);
    }
}
```

### 4. Minimum Payout Guard

If `tier_pool / recipient_count < 1 TIME` (100,000,000 satoshis), the tier's pool goes to the block producer instead. This prevents dust outputs.

---

## Example Scenarios

### Scenario 1: Small Network (10 nodes)

**Composition:** 1 Gold, 2 Silver, 3 Bronze, 4 Free

```
Block producer: Silver node A (won VRF)
- Leader bonus: 35 TIME + fees

Gold pool (25 TIME ÷ 1):    Gold A = 25 TIME
Silver pool (18 TIME ÷ 2):  Silver A = 9 TIME (merged with leader = 44 TIME)
                             Silver B = 9 TIME
Bronze pool (14 TIME ÷ 3):  Bronze A = 4.67, B = 4.67, C = 4.66 TIME
Free pool (8 TIME ÷ 4):     Free A = 2, B = 2, C = 2, D = 2 TIME

Every node is paid every block.
```

### Scenario 2: Large Network (500 nodes)

**Composition:** 5 Gold, 20 Silver, 75 Bronze, 400 Free

```
Block producer: Bronze node (won VRF)
- Leader bonus: 35 TIME + fees

Gold pool (25 ÷ 5):    5 TIME each — all paid every block
Silver pool (18 ÷ 20):  0.9 TIME each — all paid every block
Bronze pool (14 ÷ 25):  0.56 TIME each — top 25 of 75 by fairness
                         All 75 rotate through in 3 blocks
Free pool (8 ÷ 8):      1 TIME each — top 8 of 400 by fairness
                         All 400 rotate through in 50 blocks (~8.3 hours)
```

### Scenario 3: Extreme Scale (10,000 Free nodes)

```
Free pool: 8 TIME ÷ 8 = 1 TIME each (max 8 recipients, since 8 TIME / 1 TIME min = 8)
Rotation: Each node paid every ~1,250 blocks (~8.7 days)
Per payment: 1 TIME (meaningful, not dust)
```

---

## Consensus Safety

### Determinism Guarantees

All nodes MUST produce identical reward lists because:
1. **Same eligible set**: Derived from on-chain masternode registry + maturity rules
2. **Same fairness bonus**: Derived from on-chain `masternode_rewards` scan
3. **Same sort order**: fairness_bonus DESC, then address ASC
4. **Same selection**: Top 25 per tier, identical across all validators
5. **Same arithmetic**: Integer division with remainder to last recipient

### Validation

Every validating node independently re-derives the expected reward list in `validate_pool_distribution()` and rejects blocks with incorrect distributions. Tolerance of 1 TIME per output handles minor chain-view divergence during sync.

---

## Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `PRODUCER_REWARD_SATOSHIS` | 35 × 10⁸ | Leader bonus (35 TIME) |
| `GOLD_POOL_SATOSHIS` | 25 × 10⁸ | Gold tier pool (25 TIME) |
| `SILVER_POOL_SATOSHIS` | 18 × 10⁸ | Silver tier pool (18 TIME) |
| `BRONZE_POOL_SATOSHIS` | 14 × 10⁸ | Bronze tier pool (14 TIME) |
| `FREE_POOL_SATOSHIS` | 8 × 10⁸ | Free tier pool (8 TIME) |
| `MIN_POOL_PAYOUT_SATOSHIS` | 10⁸ | Minimum 1 TIME per recipient |
| `MAX_TIER_RECIPIENTS` | 25 | Max recipients per tier per block |
| `FREE_MATURITY_BLOCKS` | 72 | Free tier maturity gate (mainnet) |

---

## Key Implementation Files

- `src/constants.rs` — All reward constants
- `src/types.rs` — `MasternodeTier::pool_allocation()` and `reward_weight()`
- `src/blockchain.rs` — `produce_block_at_height()` (distribution) and `validate_pool_distribution()` (validation)
- `src/masternode_registry.rs` — `get_eligible_pool_nodes()` and `get_pool_reward_tracking()`
- `docs/TIMECOIN_PROTOCOL.md` — §10.4 (normative specification)

---

## Changelog

- **2026-02-20:** v2.0 — Replaced 10-node rotation with per-tier pools + fairness rotation
- **2026-02-19:** v1.1 — Replaced 50/50 Free-only pool with unified weighted pool
- **2026-01-21:** v1.0 — Initial 10-node rotation specification
