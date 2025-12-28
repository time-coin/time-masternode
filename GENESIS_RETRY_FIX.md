# Genesis Retry Mechanism Fix

## Problem
All nodes were stuck at height 0, continuously requesting blocks from each other but none had any blocks to send. The logs showed:
```
📥 [Inbound] Received GetBlocks(0-500) from peer (our height: 0)
📤 [Inbound] Sending 0 blocks to peer (requested 0-500, effective 0-0)
```

This occurred because:
1. Initial genesis leader election happened once at startup
2. If the leader failed to create/broadcast genesis, or if genesis didn't propagate
3. Non-leaders timed out after 60 seconds but never retried
4. All nodes entered a permanent deadlock requesting blocks from each other

## Solution
Implemented a **periodic genesis retry mechanism** that:

### Features
- ✅ Background task checks every **2 minutes** if stuck at height 0
- ✅ Waits for the next **10-minute boundary** before retrying (ensures node synchronization)
- ✅ Re-runs **deterministic TSDC leader election** for genesis (slot 0)
- ✅ **Leader** creates and broadcasts genesis block using `GenesisAnnouncement`
- ✅ **Non-leaders** request genesis from the network using `RequestGenesis`
- ✅ Automatically stops when height > 0 or genesis block exists

### Code Changes
**File:** `src/main.rs`

Added new background task after fork detection:
```rust
tokio::spawn(async move {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(120)).await;
        
        // Only retry if still at height 0 without genesis
        let height = blockchain_genesis_retry.get_height().await;
        if height > 0 || blockchain_genesis_retry.get_block_by_height(0).await.is_ok() {
            continue;
        }
        
        // Wait for 10-minute boundary
        // Re-run leader election
        // Leader creates + broadcasts, non-leaders request
    }
});
```

### Timeline
1. **T+0s**: Nodes start, attempt initial genesis election
2. **T+60s**: Non-leaders timeout if no genesis received
3. **T+120s**: **First retry** - all nodes participate in new election
4. **T+240s**: **Second retry** (if still no genesis)
5. **T+360s**: **Third retry** (continues every 2 minutes)

### Benefits
- 🔄 **Automatic recovery** from failed genesis attempts
- 🎯 **Deterministic** - same leader selected across all nodes
- ⏰ **Synchronized** - waits for 10-minute boundaries
- 📡 **Network-aware** - uses proper message types for coordination
- 🛡️ **Safe** - stops automatically once genesis exists

### Logs to Watch For
Success case:
```
🔄 Still at height 0 without genesis - retrying genesis election
⏳ Waiting 120s for 10-minute boundary for genesis retry
🗳️  Retrying genesis election with 4 masternodes
👑 Genesis leader (retry): 64.91.241.10 (my IP: 64.91.241.10)
👑 I am the genesis leader (retry) - creating genesis block
✅ Genesis block created (retry) - broadcasting
```

Non-leader case:
```
🔄 Still at height 0 without genesis - retrying genesis election
👑 Genesis leader (retry): 64.91.241.10 (my IP: 69.167.168.176)
📥 Not genesis leader (retry) - requesting from network
```

## Testing
- ✅ `cargo fmt` - passed
- ✅ `cargo clippy` - no warnings
- ✅ `cargo check` - compiled successfully

## Deployment
1. Deploy updated binary to all nodes
2. Restart nodes (they will enter retry loop if stuck)
3. Within 2-10 minutes, genesis should be created and propagated
4. Monitor logs for "Genesis block created (retry)" message

## Related Issues
- Fixes height-0 deadlock where all nodes stuck requesting blocks
- Ensures network can bootstrap from cold start
- Complements existing genesis election logic with retry capability
