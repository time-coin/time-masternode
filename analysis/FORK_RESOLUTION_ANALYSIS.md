# Chain Fork Resolution Analysis

## Current Problem (Block 3724 Fork)

### Symptoms
- Node stuck at height 3724 with hash `719887f3053a4571`
- Other nodes are at height 3725+ with different block 3724 hashes:
  - 50.28.104.50: `a2fcb74779650c78`
  - 165.84.215.117: `4b46522935aff189` 
- Node cannot sync because every block 3725 it receives has wrong `previous_hash`
- Fork detection is working but reorganization is NOT happening

### Root Cause

The code has fork resolution logic but it's **not being triggered correctly**:

1. **Detection works**: `add_block_with_fork_handling()` detects when incoming block has wrong `previous_hash`
2. **But doesn't reorg**: When fork detected, it just returns `Ok(false)` and skips the block
3. **Reorg never triggered**: The `reorganize_to_chain()` function exists but is never called in this scenario

### The Bug

In `src/blockchain.rs`, around line 1062-1075:

```rust
// Case 1: Block is exactly what we expect (next block)
if block_height == current + 1 {
    let expected_prev_hash = self.get_block_hash(current)?;
    
    if block.header.previous_hash != expected_prev_hash {
        tracing::warn!(
            "🔀 Fork detected: block {} previous_hash mismatch",
            block_height
        );
        // BUG: Just returns false instead of triggering reorg!
        return Ok(false);
    }
    //...
}
```

### Why It Doesn't Reorganize

The reorg logic in `reorganize_to_chain()` is only called from the block sync handler in `src/network/server.rs` when:

1. Peer sends multiple blocks (GetBlocks response)
2. AND peer's chain is longer 
3. AND we can verify a common ancestor

But in the logs, peers are sending block announcements ONE AT A TIME, so the multi-block reorg path is never taken.

## Solution Required

The code needs to:

1. **When fork detected at height N+1**: Don't just skip
2. **Request chain from peer**: Go back and request blocks from height N-10 to find common ancestor
3. **Compare cumulative work**: Determine which chain has more work
4. **Trigger reorg if needed**: Call `reorganize_to_chain()` if peer has more work

### Implementation Plan

1. Modify `add_block_with_fork_handling()` to trigger sync when fork detected
2. Add method to request chain from specific peer when fork detected
3. Ensure `reorganize_to_chain()` is called with proper common ancestor

### Alternative: Manual Recovery

Since the network is small and this is a permanent fork, the stuck node needs manual intervention:

1. **Stop daemon**
2. **Delete blockchain database** (not the entire data directory, just chain data)
3. **Restart daemon** - will resync from peers' longer chain
4. **OR rollback**: Use admin tool to rollback to height 3700 and resync

## Expected Behavior

When a node detects that a peer has a different block at the same height:

1. Request blocks from peer going back 10-20 blocks to find common ancestor
2. Compare cumulative chain work
3. If peer has more work, reorganize to peer's chain
4. If we have more work, keep our chain

## Current Behavior

When node detects fork:
- Logs warning
- Skips block
- Never reorganizes
- Stuck forever if minority chain
