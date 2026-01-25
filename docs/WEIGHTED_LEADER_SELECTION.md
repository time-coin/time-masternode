# Weighted Leader Selection and Producer-Only Rewards

**Status:** Implemented  
**Version:** 1.1.0  
**Date:** January 25, 2026

## Overview

This document describes the weighted leader selection mechanism and producer-only reward system implemented to provide tier-based incentives while maintaining blockchain efficiency.

## Motivation

### Problem: Blockchain Bloat

The original participation-based rewards system would have stored a list of all participating masternodes in every block. With thousands of masternodes, this would result in significant blockchain bloat:

- 1,000 masternodes × ~20 bytes per address = ~20 KB per block
- 10,000 masternodes × ~20 bytes = ~200 KB per block
- Over time, this adds hundreds of GB of unnecessary data

### Solution: Producer-Only Rewards + Weighted Selection

Instead of storing participant lists, we:

1. **Only reward the block producer** (already stored in `header.leader`)
2. **Weight leader selection by tier** using `reward_weight()`
3. **Higher tiers produce more blocks** → earn more rewards over time
4. **Fixed reward per block** (50 TIME base + transaction fees)

This achieves the same economic outcome (higher tiers earn more) without blockchain bloat.

---

## How It Works

### Leader Selection Algorithm

```
1. Get all active masternodes (sorted deterministically by address)
2. Build cumulative weight array based on tier.reward_weight()
3. Generate deterministic random value from:
   Hash(prev_block_hash || block_height || attempt_number)
4. Select masternode using weighted random selection
5. Higher weights → higher probability of selection
```

### Tier Weights

Each masternode tier has a selection weight:

| Tier | Weight | Relative to Bronze |
|------|--------|-------------------|
| Free | 100 | 0.1x |
| Bronze | 1,000 | 1x (baseline) |
| Silver | 10,000 | 10x |
| Gold | 100,000 | 100x |

### Block Rewards

- **Fixed reward:** 50 TIME per block (+ transaction fees)
- **Single recipient:** Block producer only
- **No participant lists:** Minimal blockchain data

---

## Economic Model

### Selection Probability

```
Probability(masternode) = tier.reward_weight() / total_network_weight
```

**Example:**
- 5 Free nodes (total weight: 500)
- 1 Bronze node (weight: 1,000)
- **Total network weight: 1,500**

**Selection chances:**
- Each Free node: 100/1,500 = **6.67%**
- Bronze node: 1,000/1,500 = **66.67%**

### Expected Earnings

Assuming 10-minute blocks (144 blocks/day):

#### Scenario 1: 5 Free + 1 Bronze

| Tier | Blocks/Day | Daily Earnings | Monthly Earnings |
|------|-----------|----------------|------------------|
| Free (each) | 9.6 | 480 TIME | 14,400 TIME |
| Bronze | 96 | 4,800 TIME | 144,000 TIME |

**Result:** Bronze earns **10x more** than each Free node.

---

#### Scenario 2: Balanced Network (10 of each tier)

**Network weights:**
- 10 Free: 1,000 total
- 10 Bronze: 10,000 total
- 10 Silver: 100,000 total
- 10 Gold: 1,000,000 total
- **Total: 1,111,000**

| Tier | Blocks/Month (each) | Monthly Earnings (each) |
|------|---------------------|------------------------|
| Free | 3.9 | 194 TIME |
| Bronze | 38.9 | 1,944 TIME |
| Silver | 388.8 | 19,440 TIME |
| Gold | 3,888 | 194,400 TIME |

**Multipliers:**
- Bronze: 10x Free
- Silver: 100x Free
- Gold: 1,000x Free

---

#### Scenario 3: Gold Whale (1 Gold + 100 Free + 10 Bronze)

**Network weights:**
- 100 Free: 10,000
- 10 Bronze: 10,000
- 1 Gold: 100,000
- **Total: 120,000**

| Tier | Network Share | Monthly Earnings |
|------|---------------|------------------|
| 100 Free (total) | 8.3% | 1,800 TIME |
| 10 Bronze (total) | 8.3% | 1,800 TIME |
| 1 Gold | 83.3% | 180,000 TIME |

**The Gold whale dominates 83% of all blocks!**

---

#### Scenario 4: Mature Network (1,000 Free, 100 Bronze, 10 Silver, 1 Gold)

**Network weights (perfectly balanced):**
- 1,000 Free: 100,000
- 100 Bronze: 100,000
- 10 Silver: 100,000
- 1 Gold: 100,000
- **Total: 400,000**

| Tier | Each Node (Monthly) | Tier Total (Monthly) |
|------|---------------------|---------------------|
| Free | 1.08 TIME | 1,080 TIME |
| Bronze | 10.8 TIME | 1,080 TIME |
| Silver | 108 TIME | 1,080 TIME |
| Gold | 1,080 TIME | 1,080 TIME |

**Perfect balance:** Each tier collectively earns the same when weights are balanced.

---

## Key Insights

1. **Early adopters with high tiers make massive returns** when few exist
2. **Network naturally balances over time** as more high-tier nodes join
3. **Free nodes become diluted** in mature networks with many participants
4. **Gold nodes can dominate** if they're rare (creates strong incentive to upgrade)
5. **No blockchain bloat** - only producer address stored per block

---

## Implementation Details

### Code Changes

**1. Weighted Leader Selection (`src/main.rs`):**
```rust
// Build cumulative weight array
let mut cumulative_weights: Vec<u64> = Vec::new();
let mut total_weight = 0u64;
for mn in &masternodes {
    total_weight = total_weight.saturating_add(mn.tier.reward_weight());
    cumulative_weights.push(total_weight);
}

// Deterministic random selection based on weights
let random_value = hash_to_u64(selection_hash) % total_weight;
let producer_index = cumulative_weights
    .iter()
    .position(|&w| random_value < w)
    .unwrap_or(masternodes.len() - 1);
```

**2. Producer-Only Rewards (`src/masternode_registry.rs`):**
```rust
pub async fn get_masternodes_for_rewards(
    &self,
    blockchain: &Blockchain,
) -> Vec<MasternodeInfo> {
    // Only return the block producer for rewards
    // Producer address is in blockchain.header.leader
    // No need to store participant lists
}
```

### Deterministic Properties

- **Selection is deterministic:** All nodes compute same leader from same inputs
- **Rotation on timeout:** `attempt` counter changes selection if leader fails
- **Canonical ordering:** Masternodes sorted by address for consistency
- **No randomness source needed:** Uses block hash as entropy

---

## Benefits

### 1. Minimal Blockchain Data
- Only stores producer address (already required)
- No participant lists or voting records
- Scales to millions of masternodes

### 2. Strong Tier Incentives
- Gold nodes earn 1,000x more than Free
- Clear upgrade path: Free → Bronze → Silver → Gold
- Economic pressure to increase collateral

### 3. Deterministic and Fair
- All nodes agree on leader selection
- Transparent probability based on weight
- No favoritism or manipulation possible

### 4. Network Decentralization
- Multiple participants can still compete
- No single entity can monopolize (unless they own majority weight)
- Free nodes can still participate (just earn less)

---

## Migration Notes

### From Participation-Based System

Previous system tracked all participants per block. Migration path:

1. **Bootstrap period (blocks 0-3):** Use all active masternodes for rewards
2. **Legacy blocks:** Fall back to active masternodes if no producer recorded
3. **After deployment:** Only producer receives rewards going forward

### Backward Compatibility

- Old blocks still validate correctly
- Reward distribution changes only affect new blocks
- No consensus fork required (deterministic selection maintains agreement)

---

## Future Considerations

### Dynamic Weight Adjustment

If network becomes too centralized (e.g., one Gold whale), consider:

- Weight caps (max 10% of network weight per node)
- Progressive weight decay for large holders
- Minimum diversity requirements

### Fee Distribution

Currently all fees go to producer. Could be enhanced:

- Split fees among recent participants
- Burn mechanism for fee reduction
- Developer fund allocation

### Collateral Requirements

As network matures, consider:

- Adjusting tier thresholds
- Adding Platinum/Diamond tiers
- Dynamic collateral based on network size

---

## References

- [MASTERNODE_GUIDE.md](MASTERNODE_GUIDE.md) - How to run masternodes
- [MASTERNODE_REWARD_ROTATION.md](MASTERNODE_REWARD_ROTATION.md) - Previous rotation system
- [TIMECOIN_PROTOCOL.md](TIMECOIN_PROTOCOL.md) - Overall protocol design

## Changelog

**v1.1.0 (Jan 25, 2026):**
- Implemented weighted leader selection
- Changed to producer-only rewards
- Removed participant list storage
- Updated economic model with tier weights
