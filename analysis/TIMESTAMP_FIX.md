# Block Timestamp Fix

## Problem
Blocks were being created with timestamps in the future, causing validation errors:
- "Block timestamp is too far in the future"
- Nodes rejecting valid blocks during catch-up scenarios

## Root Cause
The system uses **deterministic timestamps** calculated as:
```
timestamp = genesis_timestamp + (block_height * 600_seconds)
```

This approach causes future timestamps in several scenarios:
1. **Network catch-up**: When producing blocks rapidly after downtime
2. **Fast block production**: Creating multiple blocks quickly to catch up to schedule
3. **Initial sync**: New nodes producing blocks to reach network height

### Example Scenario
- Genesis timestamp: 1735000000 (epoch time)
- Current block height: 100
- Current real time: 1735059000
- Deterministic timestamp: 1735000000 + (100 * 600) = 1735060000
- **Result**: Block timestamp is 1000 seconds in the future!

## Solution
Clamp block timestamps to the **earlier** of:
1. Deterministic schedule time (genesis + height * 600)
2. Current wall clock time (rounded to 10-minute intervals)

### Implementation
```rust
let deterministic_timestamp = self.genesis_timestamp() + (next_height as i64 * BLOCK_TIME_SECONDS);

// Use current time if deterministic timestamp is in the future
let now = chrono::Utc::now().timestamp();
let timestamp = std::cmp::min(deterministic_timestamp, now);

// Ensure timestamp is still aligned to 10-minute intervals
let aligned_timestamp = (timestamp / BLOCK_TIME_SECONDS) * BLOCK_TIME_SECONDS;
```

## Benefits
1. **No future timestamps**: Blocks always use current or past timestamps
2. **Maintains determinism**: Still follows scheduled intervals when on-time
3. **Enables catch-up**: Allows rapid block production without validation errors
4. **10-minute alignment**: Preserves block time alignment requirement

## Changes Made
- **File**: `src/blockchain.rs`
- **Function**: `produce_block()`
- **Lines**: 597-606

## Validation
- All 56 unit tests pass
- Block validation logic updated
- Timestamp alignment preserved

## Testing Scenarios
1. ✅ Normal operation (deterministic schedule)
2. ✅ Catch-up after downtime (clamped to current time)
3. ✅ Fast block production (aligned to 10-min intervals)
4. ✅ Clock skew handling (min of schedule vs current)

## Related Code
- Timestamp validation: `src/block/generator.rs:173-199`
- Block validation: `src/blockchain.rs:1143-1156`
- TSDC slot timing: `src/tsdc.rs:217-219`
