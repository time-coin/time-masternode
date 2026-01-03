# Fix for 00000 Merkle Root Issue

## Problem Identified

The logs revealed a critical bug where nodes were producing blocks with `00000` merkle roots. The issue occurred because:

1. **Node syncs from peers** (receives historical blocks)
2. **Mempool remains empty** (synced blocks don't populate the mempool with pending transactions)
3. **Node immediately becomes TSDC catchup leader** 
4. **Produces blocks with ZERO transactions** (empty mempool)
5. **Results in `00000` merkle root** (hash of empty transaction list)

### Root Cause

When a node syncs blocks from peers using `sync_from_peers()`, it:
- ✅ Receives and stores historical blocks
- ❌ Does NOT populate its mempool with pending transactions
- ❌ Needs time to receive transactions via P2P gossip

The node was being selected as catchup leader **immediately after syncing**, before its mempool could populate with pending transactions from the network.

## Solution Implemented

Added a **60-second cooldown period** after syncing before a node can become a catchup leader:

### Changes Made (`src/main.rs`)

1. **Track Last Sync Time**
   ```rust
   let mut last_sync_time: Option<std::time::Instant> = None;
   let min_time_after_sync = std::time::Duration::from_secs(60);
   ```

2. **Update Sync Time on Completion**
   - Set `last_sync_time` when `sync_from_peers()` succeeds
   - Set `last_sync_time` when blocks are received from peers

3. **Enforce Cooldown Before Catchup Leadership**
   ```rust
   if let Some(sync_time) = last_sync_time {
       let time_since_sync = sync_time.elapsed();
       if time_since_sync < min_time_after_sync {
           // Wait for mempool to populate
           continue;
       }
   }
   ```

### Why 60 Seconds?

- Gives P2P network time to propagate pending transactions
- Allows mempool to populate via transaction gossip
- Ensures node has transactions to include in catchup blocks
- Prevents producing blocks with empty transaction lists

## Expected Behavior After Fix

### Before Fix
```
18:05:00 - ✅ Responsive sync successful
18:05:15 - 🎯 SELECTED AS CATCHUP LEADER (immediately!)
18:05:15 - 💰 Block 4857 - merkle_root: 00000... (EMPTY!)
```

### After Fix
```
18:05:00 - ✅ Responsive sync successful
18:05:15 - 🎯 Selected as catchup leader
18:05:15 - ⏸️  Just synced 15s ago - waiting 45s more for mempool
18:06:00 - 🎯 SELECTED AS CATCHUP LEADER (after cooldown)
18:06:00 - 💰 Block 4857 - merkle_root: a3f8b2... (WITH TXs!)
```

## Testing

To verify the fix:

1. **Watch for sync events**:
   ```bash
   journalctl -u timed -f | grep -E "Sync complete|CATCHUP LEADER|just synced"
   ```

2. **Verify merkle roots are NOT 00000**:
   ```bash
   journalctl -u timed -f | grep merkle_root
   ```

3. **Check blocks have transactions**:
   ```bash
   curl -s http://localhost:8545 -d '{"jsonrpc":"2.0","method":"get_block","params":[4857],"id":1}' | jq .result.transactions
   ```

## Additional Notes

- This fix does NOT affect normal block production (only catchup scenarios)
- Other nodes can become catchup leader if selected by TSDC
- The 60s cooldown only applies after syncing from peers
- Mempool still functions normally for transaction validation and propagation

## Automatic Cleanup on Startup

The node now automatically scans and removes invalid blocks on startup:

```rust
// During node initialization (src/main.rs):
blockchain.cleanup_invalid_merkle_blocks().await
```

This function:
1. **Scans all blocks** (except genesis at height 0)
2. **Identifies blocks** with all-zero merkle roots (`00000...`)
3. **Deletes invalid blocks** using existing `delete_corrupt_blocks()` 
4. **Rolls back chain height** to lowest deleted block - 1
5. **Triggers re-sync** from peers to get valid blocks

### Startup Log Example

```
✅ Chain integrity validation passed
🔍 Scanning blocks 1-4860 for invalid merkle roots (00000...)
⚠️  Found invalid block at height 4857 with 00000 merkle root
⚠️  Found invalid block at height 4858 with 00000 merkle root
⚠️  Found invalid block at height 4859 with 00000 merkle root
⚠️  Found invalid block at height 4860 with 00000 merkle root
🗑️  Found 4 block(s) with invalid merkle roots: [4857, 4858, 4859, 4860]
🔧 Deleting 4 corrupt blocks to trigger re-sync
🗑️  Deleted corrupt block 4857
🗑️  Deleted corrupt block 4858
🗑️  Deleted corrupt block 4859
🗑️  Deleted corrupt block 4860
📉 Rolled back chain height to 4856 (lowest corrupt block was 4857)
✅ Removed 4 block(s) with invalid merkle roots
```

## Deployment

1. Stop the node: `sudo systemctl stop timed`
2. Deploy new binary: `./target/release/timed`
3. Start the node: `sudo systemctl start timed`
4. **Node will automatically clean up invalid blocks on startup**
5. Monitor logs: `journalctl -u timed -f`
