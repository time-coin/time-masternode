# P2P Network Refactor Progress Report
**Date:** December 18, 2025  
**Session:** Comprehensive P2P Architecture Refactor

## Executive Summary

We are in the middle of a comprehensive refactor to fix persistent connection issues in the TIME Coin P2P network. The primary issues are:
1. **Connection cycling** - Connections close every ~90 seconds instead of staying persistent
2. **Ping/pong failures** - Outbound connections never receive pongs, causing timeouts
3. **Peer registry bloat** - Same IP counted multiple times due to different ephemeral ports
4. **Block sync failures** - Nodes stuck at height 0 unable to catch up

## Current Architecture Issues

### Problem 1: Dual Identity Model
**Issue:** Peers are identified by "IP:PORT" instead of just IP
- Inbound connection from `50.28.104.50:12345` = one peer
- Outbound connection to `50.28.104.50:24100` = different peer  
- **Result:** Same machine counted as 2+ peers (registry bloat)

### Problem 2: Connection Cycling
**Issue:** Connections constantly reconnect every ~90 seconds
- Inbound connections work fine, pings/pongs successful
- Outbound connections timeout (no pongs received)
- Nodes disconnect and immediately reconnect
- **Result:** Unstable mesh, wasted resources, sync failures

### Problem 3: Server/Client Split
**Issue:** Separate `server.rs` and `client.rs` with different message loops
- Server handles inbound connections
- Client handles outbound connections  
- Different ping/pong code paths
- **Result:** Asymmetric behavior, hard to debug

## Refactor Plan

### Phase 1: Unified Connection Management ✅ IN PROGRESS

#### 1.1 Create Peer Connection Module ✅ DONE
**File:** `src/network/peer_connection.rs`
- Single `PeerConnection` struct for both inbound/outbound
- Unified message handling loop
- IP-based identification
- Bidirectional communication channel

**Status:** ✅ Created but not yet integrated

#### 1.2 Create Connection Manager ✅ DONE  
**File:** `src/network/connection_manager.rs`
- Track connections by IP only (no port)
- Store active socket/writer for each peer
- Implement deterministic connection direction
- Prevent duplicate connections

**Status:** ✅ Created and partially integrated into server.rs

#### 1.3 Create Peer Registry ✅ DONE
**File:** `src/network/peer_connection_registry.rs`
- Map IP → Writer for sending messages
- Single source of truth for active connections
- Thread-safe read/write access

**Status:** ✅ Created and integrated

### Phase 2: Deterministic Connection Direction ⚠️ PARTIAL

#### 2.1 IP Comparison Logic ✅ DONE
- Compare local IP vs peer IP
- Higher IP connects OUT, lower IP accepts IN
- Prevents both peers connecting simultaneously

**Status:** ✅ Implemented in `connection_manager.rs` and `server.rs`

#### 2.2 Duplicate Detection ⚠️ NEEDS WORK
- Check for existing connection during handshake
- Close rejected connection gracefully AFTER sending ACK
- Ensure proper cleanup

**Status:** ⚠️ Implemented but may have timing issues

### Phase 3: Persistent Connection Management ❌ TODO

#### 3.1 Keep Connections Alive ❌ TODO
- TCP keepalive configured (✅ done)
- Application-level pings (⚠️ broken)
- Never close unless forced disconnect
- Proper error handling

**Status:** ❌ Connections still cycling - ping/pong broken

#### 3.2 Unified Message Loop ❌ TODO
- Single message processing function
- Same code path for inbound/outbound
- Proper pong handling for outbound connections

**Status:** ❌ Not started - `peer_connection.rs` created but not integrated

### Phase 4: Fix Ping/Pong ❌ TODO

**Critical Issue Found:**
```
Outbound connections:
📤 Sent ping to X.X.X.X (nonce: 12345)
⚠️ Ping timeout (nonce: 12345) - NO PONG RECEIVED
❌ Peer unresponsive, disconnecting

Inbound connections:
📨 [INBOUND] Received ping from X.X.X.X (nonce: 67890)
✅ [INBOUND] Sent pong (nonce: 67890) - WORKS FINE
```

**Root Cause:** Outbound connections are not receiving/processing pongs

**Fix Required:**
1. Ensure outbound connections listen for pongs
2. Match pong nonce to ping nonce
3. Clear timeout on pong receipt
4. Same message processing for both directions

**Status:** ❌ Critical blocker - must fix before connections stabilize

### Phase 5: Block Sync Fix ❌ TODO

**Issue:** Some nodes stuck at height 0:
```
Dec 18 05:25:00 LW-London:   Height=0, Active Masternodes=2
Dec 18 05:25:00 LW-Michigan: Height=0, Active Masternodes=5  
Dec 18 05:25:00 LW-Arizona:  Height=2480, Active Masternodes=4
```

**Possible Causes:**
1. Genesis block mismatch between nodes
2. Block sync request/response not working
3. Connection instability preventing sync
4. Catchup logic broken

**Status:** ❌ Needs investigation after ping/pong fixed

## Implementation Status

### Completed ✅
- [x] Created `peer_connection.rs` module
- [x] Created `connection_manager.rs` module  
- [x] Created `peer_connection_registry.rs` module
- [x] IP-based peer identification
- [x] Deterministic connection direction logic
- [x] TCP keepalive configuration
- [x] Diagnostic logging for ping/pong

### In Progress ⚠️
- [ ] Integrate `peer_connection.rs` into server/client
- [ ] Fix ping/pong for outbound connections
- [ ] Test duplicate connection rejection

### Not Started ❌
- [ ] Remove `client.rs` and unify with `server.rs`
- [ ] Ensure connections never cycle
- [ ] Fix block sync for nodes stuck at height 0
- [ ] Comprehensive testing
- [ ] Performance optimization

## Next Steps (Priority Order)

### 1. FIX PING/PONG (CRITICAL) 🚨
**Why:** This is the root cause of connection cycling
**How:**
- Investigate why outbound connections don't receive pongs
- Check if pongs are being sent but not received
- Check if pongs are received but not processed
- Unify ping/pong handling for both directions

### 2. Integrate Unified Connection
**Why:** Eliminate duplicate code paths
**How:**
- Replace `handle_peer` in `server.rs` with `PeerConnection::handle()`
- Replace connection logic in `client.rs` with `PeerConnection::handle()`
- Ensure single message processing loop

### 3. Test Connection Stability
**Why:** Validate that connections stay open
**How:**
- Deploy updated code to all nodes
- Monitor logs for connection cycling
- Verify connections last >10 minutes
- Check peer counts stay stable

### 4. Fix Block Sync
**Why:** Nodes need to catch up to current height
**How:**
- Investigate genesis block hash differences
- Test block request/response with stable connections
- Verify catchup logic works with single connection

### 5. Remove Client/Server Split
**Why:** Simplify architecture
**How:**
- Move all connection logic to `PeerConnection`
- Keep server only for accepting sockets
- Use `PeerConnection::handle()` for both inbound/outbound

## Code Files Modified

### New Files Created
1. `src/network/peer_connection.rs` - Unified connection handling
2. `src/network/connection_manager.rs` - Connection state tracking
3. `src/network/peer_connection_registry.rs` - Writer registry

### Files Modified
1. `src/network/server.rs` - Integrated new modules
2. `src/network/client.rs` - Added diagnostic logging
3. `src/network/mod.rs` - Exported new modules
4. `build.rs` - Added git version info

### Files Need Modification
1. `src/network/server.rs` - Replace handle_peer with PeerConnection
2. `src/network/client.rs` - Replace with PeerConnection (or remove)
3. `src/network/message.rs` - Ensure pong handling correct

## Testing Checklist

### Connection Stability
- [ ] Connections stay open >10 minutes
- [ ] No connection cycling
- [ ] Ping/pong successful on all connections
- [ ] Peer counts accurate (no bloat)

### Block Sync
- [ ] Nodes catch up from height 0
- [ ] Block propagation works
- [ ] Genesis block hash matches

### Network Health
- [ ] All masternodes connected
- [ ] Block production working
- [ ] Transaction propagation working
- [ ] Consensus reaching quorum

## Deployment Strategy

1. **Fix ping/pong first** - Critical blocker
2. **Test on 2 nodes** - Verify stability  
3. **Deploy to testnet** - All 6 nodes
4. **Monitor for 1 hour** - Check for issues
5. **Fix block sync** - Get nodes to same height
6. **Final cleanup** - Remove old code, optimize

## Rollback Plan

If refactor causes issues:
1. Revert to commit before refactor started
2. Apply only ping/pong fix as hotfix
3. Complete refactor in staging environment
4. Re-deploy after thorough testing

## Notes

- **Do not delete old code yet** - Keep until validated
- **Add extensive logging** - Debug connection issues
- **Test incrementally** - One change at a time
- **Document decisions** - Why we made choices

---
**Last Updated:** 2025-12-18  
**Status:** IN PROGRESS - Ping/pong fix is critical priority
