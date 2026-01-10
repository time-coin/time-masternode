# Tiered Masternode Authority System for Fork Resolution

## Overview

This implementation adds a hierarchical masternode authority system to determine the canonical chain during fork situations. The system uses the existing tiered masternode structure (Gold, Silver, Bronze, Free) with whitelisting to make intelligent decisions about which chain to follow.

## Authority Hierarchy

The system implements the following authority levels (highest to lowest):

1. **Gold Masternodes** - Highest authority (1000x weight)
2. **Silver Masternodes** - High authority (100x weight)
3. **Bronze Masternodes** - Medium authority (10x weight)
4. **Whitelisted Free Masternodes** - Low authority (2x weight)
5. **Regular Free Masternodes** - Lowest authority (1x weight)

## How It Works

### Chain Selection Algorithm

When a fork is detected, the system follows this decision process:

1. **Masternode Authority Comparison (PRIMARY)**
   - Analyzes which tier of masternodes support each competing chain
   - The chain with the highest tier support is selected
   - If the same highest tier, compare authority scores (weighted sum)
   - If equal scores, compare total node count

2. **Chain Work Comparison (SECONDARY)**
   - Used when masternode authority is equal
   - The chain with more cumulative proof-of-work wins

3. **Chain Length (TERTIARY)**
   - Used when authority and work are equal
   - Longer chain wins

4. **Hash Tiebreaker (FINAL)**
   - Deterministic tiebreaker using lexicographically smallest hash
   - Ensures all nodes make the same decision

### Authority Score Calculation

The authority score is a weighted sum:
```
score = (gold_count × 1000) + (silver_count × 100) + (bronze_count × 10) + 
        (whitelisted_free × 2) + (regular_free × 1)
```

This exponential weighting ensures that higher-tier masternodes have significantly more influence.

## Implementation Details

### New Module: `masternode_authority.rs`

This module provides:

- `AuthorityLevel` enum - Represents the hierarchy of authority levels
- `ChainAuthorityAnalysis` struct - Analyzes masternode support for a chain
- `CanonicalChainSelector` - Core logic for determining canonical chain

### Key Functions

#### `analyze_our_chain_authority()`
Analyzes which masternodes are connected to and supporting our current chain.

#### `analyze_peer_chain_authority()`
Analyzes which masternodes are supporting a peer's competing chain.

#### `should_switch_to_peer_chain()`
Makes the final decision on whether to switch to a peer's chain based on:
- Masternode authority analysis
- Chain work comparison
- Chain length
- Hash tiebreaker

### Integration Points

The system is integrated into:

1. **`blockchain.rs::compare_chain_with_peers()`**
   - Periodic fork detection and resolution
   - Uses masternode authority as primary decision factor

2. **`blockchain.rs::should_switch_by_work()`**
   - Enhanced to include masternode authority analysis
   - Used when receiving chain work updates from peers

3. **`network/server.rs`**
   - Updated to pass peer IP to authority system
   - Enables per-peer authority analysis

## Fork Resolution Scenario

### Example: Multi-Way Fork at Height 5872

Consider the scenario from your network:
- **Chain A (Michigan)**: Hash 45bf9dc... at height 5882
- **Chain B (Michigan2)**: Hash b34ade... at height 5882  
- **Chain C (Arizona)**: Hash e15a94e... at height 5882
- **Chain D (London)**: Hash 86d5a7d... at height 5881

**Resolution Process:**

1. Each node queries connected peers for their chain tips
2. Groups peers by (height, hash) to identify competing chains
3. For each competing chain, analyzes masternode support:
   - Which masternodes support Chain A?
   - Which masternodes support Chain B?
   - etc.

4. Determines highest authority level for each chain:
   - If Chain A has Gold masternode support → Gold authority
   - If Chain B has only Bronze support → Bronze authority
   - Chain A wins on authority

5. If authority is equal, compares chain work, then length, then hash

### Longest Running Node Preference

If one node (e.g., Michigan) has been running longest and is a high-tier masternode:

1. It will have the **highest authority level** (assuming it's Gold/Silver/Bronze)
2. Other nodes will analyze its chain and see high-tier support
3. They will **automatically switch** to its chain due to superior authority
4. Network converges on the canonical chain determined by highest-tier masternodes

## Benefits

1. **Sybil Resistance**: Higher-tier masternodes have exponentially more influence
2. **Economic Security**: Nodes with more collateral staked determine canonical chain
3. **Deterministic**: All nodes apply same logic and reach same conclusion
4. **Backwards Compatible**: Falls back to chain work/length if authority is equal
5. **Prevents Network Splits**: Clear hierarchy prevents prolonged forks

## Testing

The module includes unit tests for:
- Authority level hierarchy
- Authority score calculation
- Chain comparison logic
- Tiebreaker scenarios

Run tests with:
```bash
cargo test masternode_authority
```

## Configuration

No additional configuration is required. The system uses:
- Existing masternode tier definitions from `types.rs`
- Existing whitelist configuration from `config.toml`
- Existing connection manager and peer registry

## Future Enhancements

Potential improvements:
1. **Historical Authority Tracking**: Track which masternodes have been online longest
2. **Stake-Weighted Authority**: Factor in actual collateral amounts beyond tier
3. **Geographic Diversity Bonus**: Reward chains with geographically diverse support
4. **Uptime Multiplier**: Give bonus authority to masternodes with high uptime

## Summary

This implementation provides a robust, hierarchical approach to fork resolution that leverages the existing tiered masternode system. Higher-tier masternodes (Gold, Silver, Bronze) have exponentially more authority than free nodes, ensuring that nodes with significant economic stake determine the canonical chain. This prevents network splits and ensures all nodes converge on the same chain following the guidance of the most authoritative masternodes.
