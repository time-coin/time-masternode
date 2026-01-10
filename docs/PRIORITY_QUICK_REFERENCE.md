# Transaction Priority System - Quick Reference

## Quick Start

### 1. Add Transaction with Submitter Tracking

```rust
// When receiving a transaction from a peer
consensus.tx_pool.add_pending_with_submitter(
    transaction,
    fee,
    Some(peer_ip.to_string())  // Track who submitted it
)?;
```

### 2. Select Transactions for Block

```rust
use crate::transaction_selection::TransactionSelector;

let selector = TransactionSelector::new(
    tx_pool,
    masternode_registry,
    connection_manager,
);

let txs = selector.select_for_block(
    1000,      // Max transaction count
    1_000_000  // Max size in bytes
).await;
```

### 3. Monitor Pool Statistics

```rust
let stats = selector.get_selection_stats().await;

println!("Pending: {}", stats.total_pending);
println!("High-tier %: {:.1}%", stats.high_tier_percentage());
println!("Gold: {}, Silver: {}, Bronze: {}", 
    stats.tier_distribution.gold,
    stats.tier_distribution.silver,
    stats.tier_distribution.bronze
);
```

## Priority Tiers

| Tier | Collateral | Score | Priority |
|------|-----------|-------|----------|
| Gold | 100,000 TIME | 4 | Highest |
| Silver | 10,000 TIME | 3 | High |
| Bronze | 1,000 TIME | 2 | Medium |
| Whitelisted Free | 0 TIME | 1 | Low |
| Free / Non-MN | 0 TIME | 0 | Base |

## Priority Formula

```
Priority = (tier_score × 10^12) + (fee_per_byte × 10^6) + age_seconds
```

This ensures:
- Tier always wins (Gold > Silver > Bronze > Free)
- Fee matters within same tier
- Age breaks ties

## Common Patterns

### Pattern 1: Transaction Reception

```rust
// In network message handler
match message {
    NetworkMessage::Transaction { tx, .. } => {
        let fee = calculate_fee(&tx)?;
        
        consensus.tx_pool.add_pending_with_submitter(
            tx,
            fee,
            Some(peer_ip.clone())
        )?;
    }
}
```

### Pattern 2: Block Generation

```rust
// In blockchain.rs generate_next_block()
let selector = TransactionSelector::new(
    self.consensus.tx_pool.clone(),
    self.masternode_registry.clone(),
    self.connection_manager.clone(),
);

let selected_txs = selector.select_for_block(
    MAX_BLOCK_TRANSACTIONS,
    MAX_BLOCK_SIZE
).await;

// Use selected_txs for block...
```

### Pattern 3: Priority Monitoring

```rust
// Get current highest tier in network
use crate::transaction_priority::get_highest_active_tier;

let highest = get_highest_active_tier(
    &masternode_registry,
    &connection_manager
).await;

match highest {
    Some(MasternodeTier::Gold) => info!("Gold tier active"),
    Some(MasternodeTier::Silver) => info!("Silver tier active"),
    Some(MasternodeTier::Bronze) => info!("Bronze tier active"),
    Some(MasternodeTier::Free) => info!("Only free tier"),
    None => info!("No masternodes"),
}
```

## Testing

```bash
# Run priority system tests
cargo test transaction_priority --lib

# Run selection tests
cargo test transaction_selection --lib

# Run all related tests
cargo test --lib transaction_priority transaction_selection
```

## Example Scenarios

### Scenario: Gold Node Transaction

```rust
// Gold masternode submits transaction with 1 sat/byte fee
// Free node submits transaction with 100 sat/byte fee

// Gold transaction selected FIRST despite lower fee
// because tier_score (4) dominates fee_per_byte
```

### Scenario: Same Tier Competition

```rust
// Two Bronze nodes submit transactions:
// - Node A: 10 sat/byte, age 60 seconds
// - Node B: 5 sat/byte, age 120 seconds

// Node A selected first (higher fee within same tier)
```

### Scenario: Network Upgrade

```rust
// Before: Only Bronze nodes exist
// Priority: Bronze (2) > Whitelisted Free (1) > Free (0)

// After: First Silver node joins
// Priority: Silver (3) > Bronze (2) > Whitelisted Free (1) > Free (0)

// All Silver transactions now get priority over Bronze
```

## Key Functions

### TransactionPriorityQueue

```rust
// Calculate priority for a transaction
async fn calculate_priority(
    &self,
    tx: &Transaction,
    fee: u64,
    submitter_ip: Option<&str>,
    added_at: Instant,
) -> PriorityScore

// Get submitter tier
async fn get_submitter_tier(&self, ip: &str) -> (Option<MasternodeTier>, bool)

// Select transactions for block
async fn select_for_block(
    &self,
    transactions: Vec<(Transaction, u64, Option<String>, Instant)>,
    max_count: usize,
    max_size_bytes: usize,
) -> Vec<Transaction>
```

### TransactionSelector

```rust
// Main selection method
async fn select_for_block(
    &self,
    max_count: usize,
    max_size_bytes: usize,
) -> Vec<Transaction>

// Get statistics
async fn get_selection_stats(&self) -> SelectionStats
```

### TransactionPool (Enhanced)

```rust
// Add with submitter tracking (NEW)
fn add_pending_with_submitter(
    &self,
    tx: Transaction,
    fee: u64,
    submitter_ip: Option<String>,
) -> Result<(), PoolError>

// Get with metadata (NEW)
fn get_all_pending_with_metadata(
    &self,
) -> Vec<(Transaction, u64, Option<String>, Instant)>

// Original methods still work
fn add_pending(&self, tx: Transaction, fee: u64) -> Result<(), PoolError>
```

## Backward Compatibility

All existing code continues to work:

```rust
// Old code (still works, gets base priority)
tx_pool.add_pending(tx, fee)?;

// New code (tracks submitter for priority)
tx_pool.add_pending_with_submitter(tx, fee, Some(ip))?;
```

## Performance Notes

- **O(1) tier lookup**: Fast masternode registry query
- **O(n log n) sorting**: Efficient priority calculation
- **Lock-free pool**: Concurrent access via DashMap
- **Lazy evaluation**: Priority calculated only when selecting

## Troubleshooting

### Issue: Transactions not getting priority

**Check:**
1. Submitter IP is being tracked (`add_pending_with_submitter`)
2. Masternode is in active registry
3. Connection manager recognizes masternode
4. Masternode tier is correctly set

### Issue: Priority seems wrong

**Verify:**
1. Check tier scores: Gold=4, Silver=3, Bronze=2, Free=1/0
2. Ensure fees are calculated per byte
3. Check transaction age
4. Review composite priority formula

### Issue: All transactions same priority

**Likely causes:**
1. Not using `add_pending_with_submitter` (all get base priority)
2. No submitter IPs being tracked
3. All transactions from non-masternodes

## Migration Checklist

- [ ] Update transaction reception to use `add_pending_with_submitter`
- [ ] Integrate `TransactionSelector` into block generation
- [ ] Add logging for tier distribution
- [ ] Test with multiple tier levels
- [ ] Monitor priority statistics
- [ ] Update documentation
- [ ] Add RPC endpoints (optional)
- [ ] Add metrics (optional)

## Additional Resources

- `docs/TRANSACTION_PRIORITY.md` - Full documentation
- `docs/PRIORITY_IMPLEMENTATION_SUMMARY.md` - Implementation details
- `src/transaction_priority.rs` - Core implementation
- `src/transaction_selection.rs` - Selection API
- `src/transaction_pool.rs` - Pool enhancements

## Support

For questions or issues:
1. Check the full documentation in `docs/`
2. Review test cases for usage examples
3. Check implementation comments in source code
