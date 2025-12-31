# Future Block Attack Prevention

## Problem Identified

**Date:** 2025-12-26  
**Severity:** Critical

### The Vulnerability

Arizona node produced blocks 265 ahead of schedule (height 4005 when time only allows ~3740). These future blocks had:
- Valid timestamps (within ±15 minutes of current time)
- Longest chain (highest height)
- Valid signatures and merkle roots

But they violated the blockchain's **fundamental timing constraint**: blocks should only exist at the rate of 1 per 10 minutes from genesis.

Other nodes were accepting this invalid chain because validation only checked:
1. ✅ Timestamp within ±15 min of current wall clock time
2. ✅ Longest chain by height
3. ✅ Valid signatures/merkle roots

It did NOT check:
4. ❌ Whether blocks align with blockchain's expected timeline

### Attack Scenario

A malicious or buggy node could:
1. Generate 1000 blocks with timestamps all within ±15 minutes of "now"
2. Create a chain that's 1000 blocks ahead
3. Force all honest nodes to sync to this invalid chain (longest chain rule)
4. Completely disrupt the network

## Solution Implemented

Added **blockchain timeline validation** to `validate_block()`:

```rust
// Expected time = genesis_time + (height * block_time)
let genesis_time = self.genesis_timestamp();
let expected_time = genesis_time + (block.header.height as i64 * BLOCK_TIME_SECONDS);
let time_drift = block.header.timestamp - expected_time;

const MAX_DRIFT_FROM_SCHEDULE: i64 = 3600; // 1 hour ahead of schedule
if time_drift > MAX_DRIFT_FROM_SCHEDULE {
    return Err(format!(
        "Block {} timestamp {} is too far ahead of expected schedule (expected: {}, drift: {}s)",
        block.header.height, block.header.timestamp, expected_time, time_drift
    ));
}
```

### How It Works

For each block, we now validate:
1. **Wall clock check**: Timestamp within ±15 min of current time (prevents too far future/past)
2. **Timeline check**: Timestamp within 1 hour of blockchain's expected schedule
   - Expected = genesis + (height × 600 seconds)
   - Allows some flexibility for network delays and clock drift
   - Prevents massive jumps ahead

### Example

- Genesis: Jan 1, 2025, 00:00:00
- Block 100 expected: ~16.7 hours later
- Block 100 timestamp: Can be up to 17.7 hours after genesis (1 hour ahead is OK)
- Block 100 timestamp: Cannot be 50 hours after genesis (way too early)

### Impact

This fix prevents:
- ✅ Nodes from producing hundreds of blocks ahead of schedule
- ✅ Network from syncing to impossible future chains
- ✅ Time-based consensus attacks
- ✅ Denial of service via chain flooding

It allows:
- ✅ Normal clock drift (up to 1 hour ahead)
- ✅ Network delays and propagation time
- ✅ Occasional fast block production (if validators are quick)

## Testing Recommendations

1. Verify Arizona's blocks 3733-4005 are now rejected on fresh sync
2. Confirm honest nodes can still produce blocks normally
3. Test edge cases:
   - Fast block production (e.g., 3 blocks in 15 minutes)
   - Slow block production (e.g., 1 block every 20 minutes)
   - Clock drift scenarios

## Deployment

- **File modified:** `src/blockchain.rs`
- **Function:** `validate_block()`
- **Backward compatible:** Yes (adds validation, doesn't change data structures)
- **Requires restart:** Yes
- **Network upgrade:** All nodes should update to prevent future attacks

## Related Issues

- Merkle root mismatch (blocks 3733+) - separate bug
- Network consensus restoration - requires chain rollback to valid height
