# Reorg Loop Fix

## Problem
The node was stuck in an infinite reorganization loop:
1. Node syncs to height (e.g., 2896)
2. Peer 178.128.199.144 (on a fork) sends blocks
3. Fork detected, node requests "earlier blocks to find common ancestor"
4. Node reorganizes backwards (rollback of 110 blocks)
5. Process repeats indefinitely - node never makes forward progress

## Root Cause
When a fork was detected during block synchronization, the code would request MORE blocks from the same peer that sent the incompatible blocks. This created an infinite loop where the node would:
- Detect fork → Request earlier blocks from bad peer
- Receive more incompatible blocks → Reorganize backwards
- Repeat forever

## Solution
Changed the fork detection logic to **immediately disconnect** peers that send incompatible blocks, rather than trying to resolve the fork with them.

**File Modified:** `src/network/peer_connection.rs` (lines 827-850)

**Before:**
```rust
// If fork was detected, request earlier blocks to find common ancestor
if let Some(fork_height) = fork_detected_at {
    warn!("🔄 Fork detected at height {}, requesting earlier blocks...");
    // Request blocks from THE SAME PEER that's on the fork
    let msg = NetworkMessage::GetBlocks(reorg_start, peer_height + 100);
    self.send_message(&msg).await;
    return Ok(());
}
```

**After:**
```rust
// If fork was detected, disconnect this peer - they're on a different chain
if let Some(fork_height) = fork_detected_at {
    error!("🚫 Peer {} is on a fork - disconnecting", self.peer_ip);
    return Err(format!("Peer {} is on a fork - detected at height {}", 
        self.peer_ip, fork_height));
}
```

## Expected Behavior After Fix
1. When peer 178.128.199.144 sends incompatible blocks, the node will detect the fork
2. The peer will be immediately disconnected
3. The node will continue syncing with valid peers (165.232.154.150, 50.28.104.50, etc.)
4. No more infinite reorg loops

## Testing
After deploying the fix:
1. Restart the node
2. Monitor logs for successful forward sync progress
3. Verify peer 178.128.199.144 gets disconnected and stays disconnected
4. Confirm the node reaches and maintains the correct chain height

## Deployment
```bash
# Build the fixed version
cargo build --release

# Restart the node with the new binary
sudo systemctl restart timed
# or
sudo systemctl stop timed
./target/release/timed
```
