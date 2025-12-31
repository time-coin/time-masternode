# Timestamp Attack Vulnerability & Fix

## Problem
Current validation allows blocks with timestamps up to 15 minutes in the future (`TIMESTAMP_TOLERANCE_SECS = 900`). A malicious or misconfigured node can:

1. Set clock 15 minutes ahead
2. Produce blocks with future timestamps that pass validation
3. Create longest chain (e.g., 265 blocks ahead at height 4005)
4. Force other nodes to sync to this invalid chain

## Current Code Issues

### Block Production (line 605-617)
```rust
const MAX_FUTURE_BLOCKS: i64 = 2; // Allow max 2 blocks (20 minutes) ahead
let max_allowed_timestamp = now + (MAX_FUTURE_BLOCKS * BLOCK_TIME_SECONDS);
if deterministic_timestamp > max_allowed_timestamp {
    return Err(...);
}
```

### Block Validation (line 1155-1160)
```rust
const TIMESTAMP_TOLERANCE_SECS: i64 = 900; // ±15 minutes
if block.header.timestamp > now + TIMESTAMP_TOLERANCE_SECS {
    return Err(...);
}
```

**Mismatch**: Production limits to 20 min ahead, validation allows 15 min ahead = 1.5+ blocks of drift.

## Solution: Deterministic Timestamp Validation

Instead of comparing to wall clock (`now`), validate against the **deterministic schedule**:

```rust
// Calculate what the block timestamp SHOULD be based on height
let expected_timestamp = self.genesis_timestamp() + (block.header.height as i64 * BLOCK_TIME_SECONDS);

// Allow small tolerance for network latency (e.g., 2 minutes = 1/5 of block time)
const NETWORK_LATENCY_TOLERANCE: i64 = 120; // 2 minutes

if block.header.timestamp > expected_timestamp + NETWORK_LATENCY_TOLERANCE {
    return Err(format!(
        "Block {} timestamp {} exceeds expected schedule {} by {} seconds",
        block.header.height,
        block.header.timestamp,
        expected_timestamp,
        block.header.timestamp - expected_timestamp
    ));
}

// Still check against wall clock to prevent far-future attacks
let now = chrono::Utc::now().timestamp();
if block.header.timestamp > now + TIMESTAMP_TOLERANCE_SECS {
    return Err(format!("Block {} timestamp is too far in future", block.header.height));
}
```

## Benefits

1. **Consensus Protection**: Blocks must follow deterministic schedule
2. **Fork Prevention**: Can't create "fast" chains by manipulating timestamps
3. **Sync Safety**: Nodes reject chains that violate schedule
4. **Clock Tolerance**: Still allows reasonable network latency

## Implementation Priority

**CRITICAL** - This should be in Phase 1 as it's a consensus-breaking vulnerability.

Current Status: Arizona node at height 4005 is 265 blocks ahead, demonstrating this attack vector.
