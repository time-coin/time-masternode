# Masternode Reward Rotation System

**Version:** 1.0  
**Date:** January 21, 2026  
**Status:** Active

---

## Overview

TimeCoin implements a deterministic 10-node rotation system for distributing block rewards. This ensures rewards remain meaningful as the network scales to thousands of masternodes while maintaining consensus safety.

## Problem Statement

### Before Rotation System
- **Network with 100 masternodes:** Each receives 1 TIME per block (acceptable)
- **Network with 1,000 masternodes:** Each receives 0.1 TIME per block (too small)
- **Network with 10,000 masternodes:** Each receives 0.01 TIME per block (impractical)

Free-tier nodes would receive negligible rewards, discouraging participation.

### Solution: Rotating 10-Node Queue
Instead of distributing to ALL masternodes every block, distribute to a rotating subset of 10 nodes.

---

## Design Goals

1. **Substantial Rewards:** Each masternode in rotation receives ~10 TIME (vs ~0.1 TIME)
2. **Deterministic Selection:** All nodes must select identical masternodes (consensus-safe)
3. **Fair Rotation:** Every masternode participates equally over time
4. **Tier Support:** Support Bronze, Silver, Gold tiers with weighted rewards
5. **Anti-Gaming:** Require 1-hour consensus participation before eligibility

---

## Algorithm Specification

### 1. Eligibility Requirements

Before a masternode can receive rewards, it MUST:

1. **Be registered** in the masternode registry
2. **Participate in consensus** for at least 1 hour (60 minutes)

**Participation Tracking:**
```rust
// A masternode is eligible if it has voted on any block in the last hour
fn is_eligible_for_rewards(
    mn: &Masternode,
    current_height: u64,
    blockchain: &Blockchain
) -> bool {
    // Look back approximately 60 blocks (~1 hour at 1 block/min)
    let lookback_height = current_height.saturating_sub(60);
    
    // Check if masternode voted on ANY block in this range
    for height in lookback_height..current_height {
        if blockchain.has_consensus_vote(height, &mn.ip_address) {
            return true;
        }
    }
    
    false
}
```

**Why 1 Hour?**
- Prevents "reward gaming" (spinning up nodes just before their rotation slot)
- Ensures masternodes contribute to network security before receiving rewards
- Long enough to prove commitment, short enough to allow quick onboarding
- Consensus-safe: all nodes can verify the same vote history on-chain

### 2. Masternode Ordering

All nodes MUST use **identical ordering** of ELIGIBLE masternodes:

```rust
fn get_sorted_eligible_masternodes(
    block_height: u64,
    blockchain: &Blockchain
) -> Vec<Masternode> {
    let all_masternodes = get_all_registered_masternodes();
    
    // Filter to only eligible masternodes
    let mut eligible: Vec<_> = all_masternodes
        .into_iter()
        .filter(|mn| is_eligible_for_rewards(mn, block_height, blockchain))
        .collect();
    
    // Sort by IP address (lexicographic order)
    eligible.sort_by(|a, b| a.ip_address.cmp(&b.ip_address));
    
    eligible
}
```

**Critical:** Lexicographic sorting by IP ensures:
- Deterministic across all nodes
- No timezone dependencies
- No floating-point arithmetic
- Simple to implement and verify

### 3. Rotation Index Calculation

```rust
fn calculate_rotation_offset(block_height: u64, total_masternodes: usize) -> usize {
    (block_height as usize) % total_masternodes
}
```

**Example:**
- Block 1783, 100 masternodes: `1783 % 100 = 83`
- Block 1784, 100 masternodes: `1784 % 100 = 84`
- Block 1885, 100 masternodes: `1885 % 100 = 85`

### 4. Select 10 Nodes (Circular Wrap)

```rust
fn select_reward_recipients(
    block_height: u64,
    masternodes: &[Masternode]
) -> Vec<Masternode> {
    let total = masternodes.len();
    let offset = calculate_rotation_offset(block_height, total);
    
    let mut selected = Vec::with_capacity(10);
    for i in 0..10 {
        let index = (offset + i) % total;  // Circular wrap-around
        selected.push(masternodes[index].clone());
    }
    
    selected
}
```

**Circular Wrap Example:**
- 100 masternodes, block 1796 → offset = 96
- Select nodes: 96, 97, 98, 99, 0, 1, 2, 3, 4, 5

### 5. Tier Weighting

Each masternode tier has a weight that affects reward distribution:

| Tier | Weight | Monthly Cost | Expected Monthly Rewards (est) |
|------|--------|--------------|-------------------------------|
| Free | 100 | $0 | 4,320 TIME (~$432) |
| Bronze | 200 | $10/month | 8,640 TIME (~$864) |
| Silver | 500 | $50/month | 21,600 TIME (~$2,160) |
| Gold | 1000 | $200/month | 43,200 TIME (~$4,320) |

**Note:** Reward estimates assume $0.10/TIME, 100 masternodes, 10-block rotation cycle.

### 6. Reward Distribution

```rust
fn distribute_rewards(
    selected_masternodes: &[Masternode],
    total_reward: u64  // 100 TIME in satoshis = 10,000,000,000
) -> Vec<(String, u64)> {
    let total_weight: u64 = selected_masternodes
        .iter()
        .map(|mn| mn.tier.weight())
        .sum();
    
    let mut distributions = Vec::new();
    let mut remaining = total_reward;
    
    for (i, mn) in selected_masternodes.iter().enumerate() {
        let share = if i == selected_masternodes.len() - 1 {
            remaining  // Last node gets remainder (handles rounding)
        } else {
            (total_reward * mn.tier.weight()) / total_weight
        };
        
        distributions.push((mn.reward_address.clone(), share));
        remaining -= share;
    }
    
    distributions
}
```

---

## Example Scenarios

### Scenario 1: All Free Tier (100 nodes)

**Block 1783:**
- Offset: `1783 % 100 = 83`
- Selected: Nodes 83-92
- Total weight: `10 × 100 = 1,000`
- Each receives: `100 TIME / 10 = 10 TIME`

**Block 1784:**
- Offset: `1784 % 100 = 84`
- Selected: Nodes 84-93
- Each receives: `10 TIME`

**Block 1883:** (full cycle complete)
- Offset: `1883 % 100 = 83`
- Selected: Nodes 83-92 again
- Each masternode participated exactly once in 100 blocks

### Scenario 2: Mixed Tiers (100 nodes)

**Composition:**
- 90 Free (weight 100 each)
- 8 Bronze (weight 200 each)
- 2 Gold (weight 1000 each)

**Block 1783:**
- Selected: 7 Free, 2 Bronze, 1 Gold
- Total weight: `(7×100) + (2×200) + (1×1000) = 2,100`
- Free node: `(100 TIME × 100) / 2,100 ≈ 4.76 TIME`
- Bronze node: `(100 TIME × 200) / 2,100 ≈ 9.52 TIME`
- Gold node: `(100 TIME × 1000) / 2,100 ≈ 47.62 TIME`

### Scenario 3: Large Network (1,000 nodes)

**Block 5432:**
- Offset: `5432 % 1000 = 432`
- Selected: Nodes 432-441
- Each Free node receives: ~10 TIME
- Each masternode participates once every 100 blocks (vs once per block in old system)

---

## Consensus Safety

### Critical Requirements

All nodes MUST:
1. ✅ Use **identical eligibility check** (60-block lookback for consensus votes)
2. ✅ Use **identical sorting** (lexicographic by IP)
3. ✅ Use **same rotation formula** (`block_height % total_eligible_masternodes`)
4. ✅ Use **same tier weights** (Free=100, Bronze=200, Silver=500, Gold=1000)
5. ✅ Use **same consensus vote history** at block height

### Why This Is Safe

1. **Deterministic:** Same inputs → same outputs on all nodes
2. **No Randomness:** No VRF, no probabilistic selection
3. **On-Chain Verification:** Consensus votes are recorded in blocks
4. **Fork-Free:** Conflicting blocks would select identical reward recipients
5. **Gaming-Resistant:** 1-hour participation requirement prevents manipulation

### Validation During Block Verification

```rust
fn verify_block_rewards(block: &Block) -> Result<(), Error> {
    // 1. Get registered masternodes at this height
    let masternodes = get_registered_masternodes(block.height);
    
    // 2. Calculate expected recipients
    let expected = select_reward_recipients(block.height, &masternodes);
    
    // 3. Calculate expected reward amounts
    let expected_rewards = distribute_rewards(&expected, BASE_REWARD);
    
    // 4. Verify block contains exactly these rewards
    if block.reward_outputs != expected_rewards {
        return Err(Error::InvalidRewardDistribution);
    }
    
    Ok(())
}
```

---

## Migration Strategy

### Phase 1: Implementation (Current)
- ✅ Add rotation logic to block production
- ✅ Update reward distribution code
- ✅ Update documentation

### Phase 2: Testing (Testnet)
- Deploy to testnet
- Verify deterministic selection across nodes
- Monitor reward distribution fairness
- Test edge cases (1 masternode, exactly 10 masternodes, etc.)

### Phase 3: Mainnet Activation
- Set activation block height
- All nodes upgrade before activation
- Monitor first 100 blocks to ensure proper rotation

---

## Edge Cases

### Case 1: New Masternode (< 1 Hour Participation)

A masternode registers at block 1750:
- Block 1750-1809: NOT eligible (< 60 blocks of participation)
- Block 1810+: Eligible for rewards (voted on blocks 1750-1809)
- Will participate in rotation once eligible

### Case 2: Inactive Masternode (No Recent Votes)

A masternode stops voting at block 1750:
- Block 1750-1809: Still eligible (has votes in 60-block lookback)
- Block 1810+: NOT eligible (no votes in lookback window)
- Automatically excluded from rotation

### Case 3: Fewer Than 10 Eligible Masternodes

If `eligible_masternodes < 10`:
- Select ALL eligible masternodes
- Distribute 100 TIME among them proportionally
- Example: 6 eligible masternodes → each Free tier gets ~16.67 TIME

```rust
let select_count = std::cmp::min(10, eligible_masternodes.len());
```

### Case 4: Exactly 10 Eligible Masternodes

- Perfect case: each block, all 10 eligible masternodes receive rewards
- No rotation needed (offset is irrelevant)

### Case 5: Masternode Joins/Leaves During Rotation

**Masternode joins at block 1785:**
- Block 1785-1844: NOT eligible (needs 60 blocks of votes)
- Block 1845: Becomes eligible
- Gets inserted into sorted list at correct IP position
- Offset continues normally (no disruption)

**Masternode leaves at block 1785:**
- Block 1785-1844: Still eligible (has recent votes)
- Block 1845+: NOT eligible (no votes in lookback)
- Removed from sorted list
- Offset continues normally

**Important:** Eligibility is recalculated fresh each block using 60-block lookback.

### Case 6: Genesis/Early Blocks

- Block 0-59: No masternodes eligible yet (need 60 blocks of participation)
- Block 60+: Masternodes that voted on blocks 0-59 become eligible
- System waits until 3+ masternodes before producing blocks
- Once 3+ eligible, rotation begins normally

---

## Performance Considerations

### Computational Cost

**Per Block Production:**
1. Sort all masternodes: `O(n log n)` where n = total masternodes
2. Select 10 nodes: `O(1)` (constant time circular selection)
3. Calculate rewards: `O(10)` (constant, only 10 recipients)

**Total:** `O(n log n)` dominated by sorting

**Optimization:** Cache sorted masternode list, only re-sort when registry changes.

### Network Bandwidth

- Reward transactions per block: 10 (vs 1000 in old system)
- Block size reduction: ~90% smaller reward section
- Faster block propagation and verification

---

## Monitoring and Metrics

### Key Metrics to Track

1. **Rotation Fairness:**
   - Blocks since last reward per masternode
   - Distribution histogram (should be uniform over 100-block windows)

2. **Reward Amounts:**
   - Average reward per Free-tier node
   - Average reward per tier
   - Reward variance

3. **Consensus Health:**
   - Block rejection rate due to invalid rewards
   - Fork events related to reward distribution

### Dashboard Queries

```sql
-- Blocks since last reward (check fairness)
SELECT masternode_ip, MAX(block_height) as last_reward_height
FROM block_rewards
GROUP BY masternode_ip
ORDER BY last_reward_height ASC;

-- Reward distribution by tier
SELECT tier, AVG(reward_amount) as avg_reward, COUNT(*) as reward_count
FROM block_rewards
WHERE block_height >= CURRENT_HEIGHT - 1000
GROUP BY tier;
```

---

## Future Improvements

### Potential Enhancements

1. **Dynamic Rotation Size:**
   - Adjust from 10 to 20 nodes if network grows to 10,000+
   - Maintains ~10 TIME per node target

2. **Tier-Based Rotation Frequency:**
   - Gold tier participates every 5 blocks
   - Free tier participates every 15 blocks
   - Balances reward incentives vs fairness

3. **Performance Optimization:**
   - Cache sorted masternode list
   - Only re-sort on masternode registration/removal events
   - Reduces per-block overhead

4. **Slashing Integration:**
   - Remove misbehaving nodes from rotation
   - Redistribute their slots to honest nodes

---

## References

- `src/tsdc.rs` - TimeLock block production and reward distribution
- `src/masternode.rs` - Masternode registration and tier management
- `docs/TIMECOIN_PROTOCOL.md` - Section 10: Rewards and Fees
- `docs/ARCHITECTURE_OVERVIEW.md` - TimeLock consensus architecture

---

## Changelog

- **2026-01-21:** Initial specification (v1.0)
- **2026-01-20:** Development and testing completed
