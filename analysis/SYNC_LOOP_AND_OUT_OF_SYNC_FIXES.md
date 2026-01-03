# Sync Loop and Out-of-Sync Block Production Fixes

## Problems Identified (from LW-Arizona logs)

### Problem 1: Infinite GetBlocks Sync Loop
**Symptom:**
```
📥 [Outbound] Received 101 blocks (height 2525-2625) from 64.91.241.10 (our height: 2626)
📤 Requesting blocks 2626-3737 for complete chain
📥 [Outbound] Received 101 blocks (height 2525-2625) from 64.91.241.10 (our height: 2626)
[repeats infinitely until connection breaks with "Broken pipe"]
```

**Root Cause:**
Node at height 2626 requests blocks 2626-3737, but peer keeps responding with blocks 2525-2625 (blocks already stored). This creates an infinite loop where:
1. Node detects fork (peer at 3737, we're at 2626)
2. Finds common ancestor at 2625
3. Requests blocks 2626-3737
4. Receives blocks 2525-2625 (WRONG RANGE)
5. Loop repeats until connection breaks

### Problem 2: Block Production While Out of Sync
**Symptom:**
```
INFO 🎯 SELECTED AS LEADER for slot 2945775
INFO 💰 Distributing 100 TIME to 2 masternodes (50 TIME each)
INFO 📦 Proposed block at height 2627 with 0 transactions
(Node is 1111 blocks behind: 2626 vs 3737)
```

**Root Causes:**
1. **TSDC proposal loop** runs as separate task - has NO sync check
2. **TSDC `propose_block()`** creates blocks with `merkle_root: Hash256::default()` (TODO placeholder)
3. This incomplete TSDC system was creating invalid blocks when node was far behind

## Solutions Implemented

### Fix 1: Prevent GetBlocks Sync Loop
**File:** `src/network/peer_connection.rs`

Added detection for when node receives blocks it already has:

```rust
// CRITICAL FIX: If we keep receiving blocks at or below our height,
// we're in a sync loop. Break out and let periodic sync handle it.
if end_height <= our_height {
    warn!(
        "⚠️ Received blocks {}-{} but we're already at height {}. Breaking potential sync loop.",
        start_height, end_height, our_height
    );
    return Ok(());
}

// Only request blocks if we actually need them
if request_start > our_height {
    // Send request
} else {
    warn!(
        "⏭️  Skipping redundant block request {}-{} (we have up to {})",
        request_start, peer_tip_height, our_height
    );
}
```

### Fix 2: Prevent Normal Block Production When Behind
**File:** `src/main.rs` (normal block production loop, line ~1485)

```rust
if is_producer {
    // CRITICAL: Do NOT produce blocks if we're significantly behind
    // This prevents creating forks when out of sync
    if blocks_behind > 10 {
        tracing::warn!(
            "⚠️ Skipping normal block production: {} blocks behind ({}. Expected: {}). Must sync first.",
            blocks_behind,
            current_height,
            expected_height
        );
        continue;
    }
    // ... proceed with block production
}
```

### Fix 3: REMOVE Incomplete TSDC Proposal Loop
**File:** `src/main.rs` (line ~549)

**The incomplete TSDC proposal loop has been REMOVED entirely** because:
1. It creates blocks with `merkle_root: Hash256::default()` (TODO at line 544 in `src/tsdc.rs`)
2. The main block production loop already uses TSDC for catchup leader selection
3. Normal hash-based leader selection works well for regular block production
4. This was the source of blocks with 00000 merkle roots when nodes were out of sync

**~190 lines of incomplete code removed** for cleaner, more maintainable codebase.

## Expected Behavior After Fixes

### Before Fixes
```
18:30:00 - 🔀 Fork detected: height 3737 > 2626
18:30:00 - 📤 Requesting blocks 2626-3737
18:30:00 - 📥 Received 101 blocks (2525-2625) ❌ WRONG RANGE
18:30:00 - 📤 Requesting blocks 2626-3737
18:30:00 - 📥 Received 101 blocks (2525-2625) ❌ LOOP
18:30:00 - 🎯 SELECTED AS LEADER for slot ❌ TSDC SEPARATE LOOP
18:30:00 - 📦 Proposed block 2627 with 0 transactions ❌ 00000 MERKLE ROOT
18:30:35 - Connection reset: Broken pipe ❌ OVERLOAD
```

### After Fixes
```
18:30:00 - 🔀 Fork detected: height 3737 > 2626
18:30:00 - 📤 Requesting blocks 2626-3737
18:30:00 - 📥 Received 101 blocks (2525-2625)
18:30:00 - ⚠️ Received blocks we already have. Breaking sync loop. ✅
18:30:10 - ⚠️ Skipping normal block production: 1111 blocks behind ✅
[TSDC proposal loop no longer runs - disabled] ✅
[Node continues trying to sync via periodic checks]
```

## Testing

Monitor logs for these changes:

```bash
# Should see these when node is behind:
journalctl -u timed -f | grep "Skipping.*blocks behind"

# Should NOT see these anymore:
journalctl -u timed -f | grep "SELECTED AS LEADER for slot"
journalctl -u timed -f | grep "Proposed block.*with 0 transactions"
journalctl -u timed -f | grep "Distributing.*TIME to.*masternodes"

# Should NOT see sync loops:
journalctl -u timed -f | grep "Received.*blocks.*2525-2625"
```

## Block Production Flow After Fixes

The node now uses a **single, unified block production system**:

1. **Main 10-minute loop** handles ALL block production:
   - **Catchup mode** (when behind): Uses TSDC leader selection
   - **Normal mode** (at height): Uses hash-based deterministic leader selection

2. **TSDC proposal loop**: DISABLED (incomplete implementation)

3. **All production paths** now check sync status before producing blocks

## Re-enabling TSDC Proposal Loop (Future)

**The TSDC proposal loop has been REMOVED.** If it needs to be re-implemented in the future:

1. **Implement merkle root** in `src/tsdc.rs` line 544:
   ```rust
   // Replace this:
   merkle_root: Hash256::default(), // TODO: Compute merkle root
   
   // With actual merkle root calculation:
   merkle_root: compute_merkle_root(&transactions),
   ```

2. **Re-implement the TSDC slot loop** in `src/main.rs` (was at line ~549)

3. **Add sync check before block proposals**:
   ```rust
   if blocks_behind > 10 {
       tracing::warn!("⚠️ Skipping TSDC proposal: {} blocks behind", blocks_behind);
       continue;
   }
   ```

4. **Consider if it's needed** - the main loop already uses TSDC for catchup coordination

## Files Modified

1. `src/network/peer_connection.rs` - Sync loop detection
2. `src/main.rs` - Block production sync checks + TSDC loop disabled

All code passes:
- ✅ `cargo fmt`
- ✅ `cargo check`
- ✅ `cargo clippy --all-targets --all-features -- -D warnings`
