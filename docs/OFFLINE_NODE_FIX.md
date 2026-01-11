# Offline Node Selection Fix

## Problem Summary

The TimeCoin network was experiencing severe block production delays (30-40 minutes for 4 blocks) when masternodes went offline. Despite having 4 active masternodes online, the network would stall waiting for 2 offline masternodes.

## Root Cause Analysis

### Issue 1: No Heartbeat Filtering
- Both `select_leader()` and `select_leader_for_catchup()` use `registry.list_active()`
- `list_active()` only checks the `is_active` flag set at registration
- **This flag is never updated based on heartbeat status**
- Result: Offline masternodes remain in the leader selection pool indefinitely

### Issue 2: Deliberate Design Trade-off
In `src/tsdc.rs` line 376:
```rust
let heartbeat_window = u64::MAX; // Always disable filtering for catchup
```

Comment explains (lines 369-375):
> "CRITICAL FIX: ALWAYS disable heartbeat filtering during catchup to ensure all nodes use the SAME masternode list. The heartbeat filter creates non-deterministic leader selection because different nodes receive heartbeats at different times, causing disagreement on who the leader is."

**The deliberate choice**: Determinism over liveness
- **Why**: All nodes must select the same leader to prevent forks
- **Cost**: Network waits for offline nodes, causing delays

### Issue 3: Long Timeouts
- Leader timeout was 30 seconds (line 835)
- Wait cycle: 15s sleep + 15s check = 30s per attempt
- With 2 offline nodes selected 4 times = 120+ seconds of wasted time

## Solution Implemented

We fixed the liveness problem WITHOUT breaking deterministic consensus:

### Fix 1: Reduced Timeout (30s → 10s)
**File**: `src/main.rs` line 835
```rust
// Before:
let leader_timeout = std::time::Duration::from_secs(30);

// After:
let leader_timeout = std::time::Duration::from_secs(10);
```
- Faster rotation to backup leaders
- Reduces per-offline-node delay from 30s to 10s

### Fix 2: Peer Connectivity Check
**File**: `src/main.rs` lines 1207-1218
```rust
// Check if leader is connected before waiting
let leader_ip = tsdc_leader.id.split(':').next().unwrap_or(&tsdc_leader.id);
let leader_connected = block_peer_registry
    .get_connected_peers()
    .await
    .iter()
    .any(|p| p.contains(leader_ip));

if !leader_connected && wait_duration >= std::time::Duration::from_secs(5) {
    tracing::warn!(
        "⚠️  Leader {} not connected after {}s - rotating to backup leader",
        tsdc_leader.id,
        wait_duration.as_secs()
    );
    catchup_leader_tracker.remove(&expected_height);
    continue;
}
```
- Immediately skip offline leaders after 5s
- No need to wait full 10s timeout if we know they're not connected

### Fix 3: Self-Isolation Detection
**File**: `src/main.rs` lines 1265-1277
```rust
// If we're the leader but have few connected peers,
// we might be isolated/offline. Skip to let a backup leader try.
let connected_peers = block_peer_registry.get_connected_peers().await;
if connected_peers.len() < 2 {
    tracing::warn!(
        "⚠️  We're selected as leader but only {} peer(s) connected - likely offline/isolated",
        connected_peers.len()
    );
    catchup_leader_tracker.remove(&expected_height);
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    continue;
}
```
- When we're selected as leader but isolated, skip our turn
- Allows a better-connected node to become backup leader

### Fix 4: Faster Check Cycle (15s → 5s)
**File**: `src/main.rs` line 1229
```rust
// Before:
tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;

// After:
tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
```
- Faster detection of progress/failure
- Enables quicker rotation decisions

## Impact

### Before Fixes
- 2 offline nodes selected 4 times
- Each attempt: 15s wait + 15s check = 30s
- Total delay: 120+ seconds (plus 30s leader timeout × attempts)
- Result: ~40 minutes for 4 blocks

### After Fixes
- Offline node detected in ~5s (connectivity check)
- Leader timeout reduced to 10s maximum
- Expected delay per offline node: 5-10s instead of 30s
- **Result: 4 blocks should take 20-40s instead of 40 minutes**

## Why This Works

**Preserves Determinism:**
- All nodes still use `u64::MAX` heartbeat window
- All nodes select the same primary leader
- Leader selection hash remains deterministic

**Adds Smart Liveness:**
- Non-leaders check if leader is connected before waiting
- Leaders check if they're isolated before producing
- Fast rotation when offline nodes detected
- Backup leader mechanism remains intact

**No Fork Risk:**
- We don't change the leader selection algorithm
- We just skip waiting time for known-offline nodes
- All nodes still agree on who SHOULD be leader
- They just rotate to backup faster when primary is offline

## Related Issues

This fixes the block production delays reported in mainnet diagnostic:
- Testnet height 5919 vs expected 5923 (4 blocks missing)
- 2 inactive masternodes: 165.232.154.150 and 178.128.199.144
- Last seen 19 minutes ago but still in leader selection pool

## Testing

Before deploying to mainnet:
1. Deploy to testnet first
2. Monitor block production times
3. Verify no fork creation
4. Confirm offline nodes are skipped quickly
5. Check logs for rotation warnings

## Files Modified

- `src/main.rs`:
  - Line 835: Reduced leader_timeout from 30s to 10s
  - Lines 1207-1218: Added peer connectivity check before waiting
  - Line 1229: Reduced wait sleep from 15s to 5s
  - Lines 1265-1277: Added self-isolation detection

## Deployment Notes

1. This fix works with the fork resolution fix (commit d9c7521)
2. Both fixes should be deployed together
3. Requires all masternodes to upgrade for full benefit
4. Partial deployment is safe (nodes with fix will rotate faster)
5. No breaking changes to consensus protocol
