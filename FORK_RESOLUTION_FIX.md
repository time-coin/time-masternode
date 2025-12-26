# Fork Resolution Fix

## Problem
The TIME Coin testnet nodes were stuck in a fork loop unable to synchronize. Analysis of logs showed:
- LW-Michigan at height 3734 expecting block 3735 with previous_hash `15f8ded8cdd76fae`
- LW-Arizona at height 4005 with block 3735 having previous_hash `77791c5ed33e751a`  
- LW-London at height 3734 expecting block 3735 with previous_hash `a2126bce62a8e9e7`
- LW-Michigan2 at height 3733

All nodes had **different block 3734 hashes**, indicating a fork before this point. The existing fork detection logic would:
1. Detect the fork when receiving block N+1 with wrong previous_hash
2. Skip the block and return `Ok(false)`
3. Sync logic would request the same block again
4. Loop indefinitely without ever going back to find common ancestor

## Root Causes

### 1. Fork Detection Without Action
**File:** `src/blockchain.rs` lines 1239-1247

The `add_block_with_fork_handling` function would detect forks but only return `Ok(false)`, treating it like a normal skip. The caller had no way to know a fork was detected and needed to request earlier blocks.

**Fix:** Changed to return an error when fork is detected:
```rust
return Err(format!(
    "Fork detected: block {} doesn't build on our chain (prev_hash mismatch)",
    block_height
));
```

### 2. Naive Fork Resolution in Sync
**File:** `src/network/peer_connection.rs` lines 706-724

The reorganization logic had a broken common ancestor finder that would:
- Go back one block
- Break immediately  
- Never actually find the true common ancestor

**Fix:** Implemented proper common ancestor search:
```rust
while common_ancestor > search_limit && common_ancestor > 0 {
    if let Ok(our_block) = blockchain.get_block(common_ancestor) {
        let potential_match = blocks.iter().find(|b| b.header.height == common_ancestor);
        if let Some(peer_block) = potential_match {
            if our_block.hash() == peer_block.hash() {
                // Found common ancestor!
                break;
            }
        }
    }
    common_ancestor = common_ancestor.saturating_sub(1);
}
```

### 3. No Fork Handling in Block Processing Loop
**File:** `src/network/peer_connection.rs` lines 747-776

When processing multiple blocks, if any block returned an error, the code would just skip it and log. Fork errors were not distinguished from other errors, so no special action was taken.

**Fix:** Added fork detection tracking and automatic request for earlier blocks:
```rust
let mut fork_detected_at = None;

for block in blocks {
    match blockchain.add_block_with_fork_handling(block.clone()).await {
        Ok(true) => added += 1,
        Ok(false) => skipped += 1,
        Err(e) => {
            if e.contains("Fork detected") {
                fork_detected_at = Some(block.header.height);
                // Log and track fork
            }
            skipped += 1;
        }
    }
}

// If fork was detected, request earlier blocks
if let Some(fork_height) = fork_detected_at {
    let reorg_start = fork_height.saturating_sub(10);
    let msg = NetworkMessage::GetBlocks(reorg_start, peer_height + 100);
    self.send_message(&msg).await?;
    return Ok(());
}
```

## Changes Made

### src/blockchain.rs
- Modified `add_block_with_fork_handling` to return error on fork detection instead of silently returning `Ok(false)`
- Error message: `"Fork detected: block N doesn't build on our chain (prev_hash mismatch)"`

### src/network/peer_connection.rs  
- Added `use crate::block::types::Block;` import
- Improved common ancestor finding algorithm to actually compare block hashes
- Added fork detection tracking in block processing loop
- Implemented automatic request for earlier blocks when fork detected
- Changed to use `self.send_message(&msg).await` instead of incorrect `self.writer.lock().await.send(msg).await`

## How It Works Now

1. **Fork Detection**: When receiving block N+1 that doesn't match our chain:
   - `add_block_with_fork_handling` returns error with "Fork detected" message
   - Error propagates to caller

2. **Automatic Reorg Request**: Block processing loop detects fork error:
   - Tracks the height where fork was detected
   - Requests blocks from 10 blocks before fork to peer's current height
   - This fetches enough context to find common ancestor

3. **Common Ancestor Search**: When processing the new batch:
   - Compares our blocks with received blocks at each height
   - Finds first matching block hash = common ancestor
   - Collects all blocks after common ancestor for reorg

4. **Reorganization**: Performs chain reorganization:
   - Rolls back to common ancestor
   - Applies new blocks from peer
   - Switches to longer chain

## Testing

Build completed successfully:
```
Finished `release` profile [optimized] target(s) in 1m 57s
```

## Next Steps

1. Deploy updated binary to all testnet nodes
2. Restart nodes to trigger resync with new fork resolution logic
3. Monitor logs for successful reorganization messages:
   - `✅ Found common ancestor at height X`
   - `🔄 Reorganizing from height X with Y blocks`
   - `✅ Chain reorganization successful`

## Expected Behavior

Nodes should now:
- Detect forks immediately when receiving incompatible blocks
- Automatically request earlier blocks to find common ancestor
- Successfully reorganize to the longest valid chain
- Resume normal operation after synchronization

The fork should resolve within minutes as nodes exchange blocks and find their common ancestor, then adopt the longest chain.
