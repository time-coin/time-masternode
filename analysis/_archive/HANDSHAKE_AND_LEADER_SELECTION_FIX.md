# Handshake Protocol and BFT Leader Selection Fix

**Date**: 2024-12-13  
**Status**: ✅ FIXED

## Issues Addressed

### 1. Handshake Protocol Violation (CRITICAL)

**Problem**: The `query_peer_block_hash()` function in `blockchain.rs` was connecting to peers and immediately sending `GetBlockHash` messages **without sending a handshake first**.

**Symptoms**:
```
Dec 13 00:59:44 LW-Arizona timed[15878]:  WARN ⚠️  64.91.241.10:56882 sent message before handshake - closing connection (not blacklisting)
```

**Root Cause**: When fork resolution code queries peers for block hashes during reorganization, it creates a direct TCP connection but skips the mandatory handshake.

**Fix Applied**: Modified `query_peer_block_hash()` to send the handshake before any other messages:

```rust
// Send handshake FIRST
let handshake = NetworkMessage::Handshake {
    magic: *b"TIME",
    protocol_version: 1,
    network: "Testnet".to_string(),
};
let handshake_json = serde_json::to_string(&handshake)?;
stream.write_all(handshake_json.as_bytes()).await?;
stream.write_all(b"\n").await?;
stream.flush().await?;

// THEN send GetBlockHash message
let message = NetworkMessage::GetBlockHash(height);
```

**Impact**: This was causing fork resolution to fail because nodes couldn't query peers for consensus during chain reorganization.

---

### 2. BFT Leader Selection Verification ✅

**Question**: Should BFT rules (uptime, tier weight, etc.) apply to leader selection during catchup?

**Answer**: YES - Already implemented correctly!

**Current Implementation**: 
- Located in `blockchain.rs::select_catchup_leader()`
- Uses formula: `score = tier_weight * uptime_seconds`
- Tier weights:
  - Gold: 100
  - Silver: 10
  - Bronze: 1
  - Free: 1

**For Free-Tier Only Networks**:
- All nodes have tier_weight = 1
- Leader selection becomes: `score = 1 * uptime_seconds`
- **The node with the longest uptime becomes the leader**
- Deterministic tiebreaker: address (alphabetically)

**Example**:
```
Node A: uptime = 5000s → score = 5000
Node B: uptime = 3000s → score = 3000
Node C: uptime = 8000s → score = 8000  ← LEADER
```

**Verification**: Code inspection confirms this is working as designed.

---

## Testing Recommendations

### 1. Handshake Fix Testing
- Restart all nodes with updated code
- Monitor logs for absence of "sent message before handshake" warnings
- Verify fork resolution completes successfully when forks occur
- Check that `query_peer_block_hash()` calls succeed

### 2. Leader Selection Testing
- With all free-tier nodes, verify the node with longest uptime becomes leader
- Introduce different uptime values and confirm leader changes accordingly
- Check logs for "🏆 Catchup leader selected" messages

---

## Related Code Locations

### Handshake Implementation
- ✅ `src/network/client.rs:233-248` - Persistent connection handshake (correct)
- ✅ `src/network/server.rs:209-250` - Server-side handshake validation (correct)
- ✅ `src/blockchain.rs:1418-1442` - Fork resolution handshake (FIXED)

### Leader Selection
- ✅ `src/blockchain.rs::select_catchup_leader()` - BFT leader election (correct)

---

## Summary

Both issues have been resolved:
1. **Handshake bug**: Fixed by adding handshake to `query_peer_block_hash()`
2. **Leader selection**: Already correctly implemented with BFT rules

The network should now:
- Properly handle fork resolution without protocol violations
- Select catchup leaders based on uptime (for free-tier networks)
- Prevent connection rejections during consensus queries
