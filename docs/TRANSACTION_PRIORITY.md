# Tiered Masternode Transaction Priority System

## Overview

This system implements transaction prioritization based on masternode tier rankings, ensuring that higher-tier masternodes get priority for their transactions while maintaining fair access for all network participants.

## Priority Hierarchy

The system uses a 5-tier priority ranking:

1. **Gold Tier Masternodes** - Highest priority (100,000 TIME collateral)
2. **Silver Tier Masternodes** - High priority (10,000 TIME collateral)
3. **Bronze Tier Masternodes** - Medium priority (1,000 TIME collateral)
4. **Whitelisted Free Tier** - Low priority (0 collateral, but whitelisted/connected)
5. **Regular Free Tier** - Base priority (0 collateral)
6. **Non-Masternode** - Lowest priority (fee-based only)

## Priority Calculation

Priority is calculated using a composite score:

```
Priority = (tier_score × 10^12) + (fee_per_byte × 10^6) + age_seconds
```

This ensures:
- **Tier dominates**: A Gold masternode transaction will always beat lower tiers, even with minimal fees
- **Fee matters within tier**: Higher fees win within the same tier
- **Age breaks ties**: Older transactions are prioritized in case of exact matches

## Usage

### 1. Adding Transactions with Submitter IP

When receiving transactions from peers, track the submitter IP:

```rust
use crate::transaction_pool::TransactionPool;

let tx_pool = Arc::new(TransactionPool::new());

// Add transaction with submitter IP for priority calculation
tx_pool.add_pending_with_submitter(
    transaction,
    fee,
    Some(peer_ip.to_string())
)?;
```

### 2. Selecting Transactions for Block Production

Use the `TransactionSelector` to select transactions based on priority:

```rust
use crate::transaction_selection::TransactionSelector;

let selector = TransactionSelector::new(
    tx_pool.clone(),
    masternode_registry.clone(),
    connection_manager.clone(),
);

// Select up to 1000 transactions, max 1MB total size
let selected = selector.select_for_block(1000, 1_000_000).await;

// These transactions are now ordered by priority
for tx in selected {
    // Add to block
}
```

### 3. Monitoring Tier Distribution

Get statistics about the transaction pool:

```rust
let stats = selector.get_selection_stats().await;

println!("Pending: {}", stats.total_pending);
println!("Gold: {}", stats.tier_distribution.gold);
println!("Silver: {}", stats.tier_distribution.silver);
println!("High-tier %: {:.1}%", stats.high_tier_percentage());
```

## Implementation Details

### Automatic Tier Detection

The system automatically detects masternode tiers by:

1. Querying the active masternode registry
2. Matching transaction submitter IP to registered masternodes
3. Checking whitelist status in connection manager
4. Assigning appropriate priority score

### Fair Access Guarantees

While high-tier nodes get priority, the system maintains fairness:

- **Age-based promotion**: Old transactions eventually get processed
- **Fee market**: Within each tier, fees still determine order
- **No starvation**: Lower-tier transactions are not blocked indefinitely
- **Memory pressure**: LRU eviction removes lowest-priority transactions when full

### Current Active Tier

For free nodes, the system uses **whitelisted nodes** as the highest ranking. If Bronze, Silver, or Gold masternodes exist and are connected, they become the highest ranking tiers respectively.

The `get_highest_active_tier()` helper function determines the current highest tier:

```rust
use crate::transaction_priority::get_highest_active_tier;

let highest_tier = get_highest_active_tier(
    &masternode_registry,
    &connection_manager
).await;

match highest_tier {
    Some(MasternodeTier::Gold) => println!("Gold nodes active"),
    Some(MasternodeTier::Silver) => println!("Silver highest"),
    Some(MasternodeTier::Bronze) => println!("Bronze highest"),
    Some(MasternodeTier::Free) => println!("Only free nodes"),
    None => println!("No masternodes"),
}
```

## Integration Points

### Transaction Pool

Modified `TransactionPool` to track submitter IPs:

- `add_pending_with_submitter()` - Add with IP tracking
- `get_all_pending_with_metadata()` - Get transactions with metadata

### Block Production

Integrate into block generator:

```rust
// In blockchain.rs generate_next_block()
let selector = TransactionSelector::new(
    self.consensus.tx_pool.clone(),
    self.masternode_registry.clone(),
    self.connection_manager.clone(),
);

let transactions = selector.select_for_block(
    MAX_BLOCK_TXS,
    MAX_BLOCK_SIZE
).await;
```

### RPC Monitoring

Add RPC endpoints to monitor priority system:

```rust
// Get tier distribution
GET /api/pool/tier_distribution

Response:
{
  "gold": 10,
  "silver": 25,
  "bronze": 50,
  "whitelisted_free": 100,
  "regular": 200,
  "high_tier_percentage": 22.1
}
```

## Testing

The system includes comprehensive unit tests:

```bash
# Run all priority tests
cargo test transaction_priority

# Run selection tests
cargo test transaction_selection
```

## Performance Considerations

- **O(1) tier lookup**: Masternode registry query is constant time
- **O(n log n) sorting**: Priority calculation and sorting scales efficiently
- **Lock-free pool**: DashMap allows concurrent access without contention
- **Lazy evaluation**: Priority is only calculated when selecting for blocks

## Migration Path

### Phase 1: Deploy Priority System (Current)
- Add transaction_priority module
- Update TransactionPool to track submitter IPs
- Add TransactionSelector for block production

### Phase 2: Integrate with Block Production
- Modify generate_next_block() to use TransactionSelector
- Add logging for tier distribution
- Monitor high-tier transaction percentage

### Phase 3: RPC and Monitoring
- Add RPC endpoints for tier statistics
- Add Prometheus metrics
- Create dashboard for priority monitoring

### Phase 4: Optimization
- Tune priority weights based on network behavior
- Implement adaptive priority adjustments
- Add rate limiting per tier if needed

## Security Considerations

### Abuse Prevention

- **Fee minimum**: All transactions still require minimum fee
- **Spam protection**: Memory pressure evicts lowest priority first
- **Sybil resistance**: Requires actual masternode collateral
- **DOS mitigation**: Connection limits per tier prevent flooding

### Fairness Guarantees

- **No starvation**: Age-based tie-breaking ensures old txs get processed
- **Fee market**: Economic incentives still work within tiers
- **Transparent rules**: Priority calculation is deterministic and public
- **No censorship**: Cannot block transactions permanently

## Example Scenarios

### Scenario 1: Mixed Network (Gold + Free nodes)

```
Pool: 100 transactions
- 10 from Gold masternodes (fee: 1 sat/byte)
- 90 from free nodes (fee: 10 sat/byte)

Selection order:
1-10: All Gold transactions (despite lower fees)
11-100: Free node transactions (by fee)
```

### Scenario 2: Within-Tier Competition

```
Pool: 50 Bronze masternode transactions

Selection order:
1. Highest fee per byte
2. If tied, oldest transaction
3. If still tied, by transaction hash (deterministic)
```

### Scenario 3: Network Upgrade (Bronze → Silver)

```
Before: Bronze is highest tier (tier_score = 2)
After: First Silver node joins (tier_score = 3)

Result:
- Silver transactions immediately get priority
- Bronze transactions now compete with each other
- Free node transactions unchanged
```

## Monitoring Commands

```bash
# Check transaction pool tier distribution
curl http://localhost:9650/api/pool/tier_distribution

# Check current highest active tier
curl http://localhost:9650/api/masternodes/highest_tier

# Get selection statistics
curl http://localhost:9650/api/pool/selection_stats
```

## Future Enhancements

1. **Dynamic Priority Adjustment**
   - Adjust tier weights based on network congestion
   - Implement surge pricing during high demand

2. **Tier-Based Rate Limits**
   - Allow higher TPS for higher-tier nodes
   - Prevent abuse while rewarding participation

3. **Priority Decay**
   - Gradually reduce priority advantage over time
   - Ensure eventual processing of all transactions

4. **Multi-Factor Priority**
   - Consider transaction type (e.g., governance vs transfer)
   - Weight by masternode uptime and reliability
   - Factor in network contribution metrics

## Conclusion

This tiered priority system rewards masternode operators based on their collateral commitment while maintaining network fairness and preventing abuse. It provides clear economic incentives for running higher-tier masternodes while ensuring all transactions can eventually be processed.
