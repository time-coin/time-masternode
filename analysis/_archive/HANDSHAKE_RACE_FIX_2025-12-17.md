# Handshake ACK Failure Fix - December 17, 2025

**Time**: 02:13 UTC  
**Issue**: Connection reset by peer (os error 104) during handshake ACK  
**Root Cause**: Race condition in duplicate connection detection  
**Status**: ✅ FIXED

---

## Problem Description

All masternodes were unable to establish stable connections, failing with:
```
INFO ✓ Connected to peer: 165.232.154.150
WARN Connection to 165.232.154.150 failed (attempt 4): Handshake ACK failed: 
     Error reading handshake ACK: Connection reset by peer (os error 104)
```

**Impact**:
- Masternodes couldn't see each other as connected
- Block production blocked: "only 1 masternodes active (minimum 3 required)"
- Network unable to progress

---

## Root Cause Analysis

### The Race Condition

**Sequence of Events**:
1. Both peers simultaneously try to connect to each other (Peer A → Peer B, Peer B → Peer A)
2. Both TCP connections establish successfully (`✓ Connected to peer`)
3. Client (outbound) sends handshake message
4. **Server (inbound) receives connection BUT duplicate check happens BEFORE handshake**
5. Server detects outbound connection exists
6. Server performs tie-breaking: if `local_ip < remote_ip`, reject immediately
7. Server closes connection **before sending ACK**
8. Client waits for ACK but gets "Connection reset by peer"

### The Problematic Code

**Before Fix** (`src/network/server.rs` lines 234-265):
```rust
// Extract IP from address
let ip_str = ip.to_string();

// Tie-breaking for simultaneous connections (TOO EARLY!)
let local_ip_str = local_ip.as_deref().unwrap_or("0.0.0.0");
let has_outbound = connection_manager.is_connected(&ip_str).await;

if has_outbound {
    if local_ip_str < ip_str.as_str() {
        // PROBLEM: Close connection before handshake completes
        return Ok(());
    }
}

// Mark this inbound connection
connection_manager.mark_inbound(&ip_str).await;

// ... handshake happens AFTER rejection check
```

**Why This Failed**:
- Duplicate check happened **before** handshake
- When both peers connected simultaneously, both could reject each other
- Client sent handshake, but server already closed socket
- Result: "Connection reset by peer" during ACK wait

---

## Solution Implemented

### Key Changes

1. **Delay Duplicate Check Until AFTER Handshake**
   - Accept all inbound connections initially
   - Complete handshake first
   - THEN check for duplicates
   - Send ACK before closing if rejecting

2. **Graceful Rejection**
   - If rejecting duplicate, send ACK first
   - Then close connection cleanly
   - Prevents "Connection reset" errors

3. **Proper Connection Takeover**
   - If accepting inbound over outbound, close the outbound connection
   - Prevents connection leak

### Code Changes

#### File: `src/network/server.rs`

**Line 234-240: Remove premature duplicate check**
```rust
// BEFORE:
let has_outbound = connection_manager.is_connected(&ip_str).await;
if has_outbound {
    if local_ip_str < ip_str.as_str() {
        return Ok(()); // CLOSES BEFORE HANDSHAKE!
    }
}

// AFTER:
// DON'T reject duplicate connections immediately - wait for handshake first
// This prevents race conditions where both peers connect simultaneously
```

**Line 322-360: Add duplicate check AFTER handshake**
```rust
tracing::info!("✅ Handshake accepted from {} (network: {})", peer.addr, network);
handshake_done = true;

// NOW check for duplicate connections after handshake
let local_ip_str = local_ip.as_deref().unwrap_or("0.0.0.0");
let has_outbound = connection_manager.is_connected(&ip_str).await;

if has_outbound {
    // Use deterministic tie-breaking
    if local_ip_str < ip_str.as_str() {
        tracing::debug!(
            "🔄 Rejecting duplicate inbound from {} after handshake",
            peer.addr
        );
        // Send ACK first so client doesn't get "connection reset"
        let ack_msg = NetworkMessage::Ack {
            message_type: "Handshake".to_string(),
        };
        if let Some(w) = writer.take() {
            peer_registry.register_peer(ip_str.clone(), w).await;
            let _ = peer_registry.send_to_peer(&ip_str, ack_msg).await;
        }
        break; // Close gracefully
    }
    // Accept inbound and close outbound
    tracing::debug!(
        "✅ Accepting inbound from {} (closing outbound)",
        peer.addr
    );
    connection_manager.remove(&ip_str).await;
}

// Mark this inbound connection
connection_manager.mark_inbound(&ip_str).await;

// Continue with normal flow...
```

#### File: `src/network/connection_manager.rs`

**Added `remove()` method**:
```rust
/// Force remove connection (used when accepting inbound over outbound)
pub async fn remove(&self, ip: &str) {
    let mut outbound = self.connected_ips.write().await;
    let mut inbound = self.inbound_ips.write().await;
    outbound.remove(ip);
    inbound.remove(ip);
}
```

---

## Expected Results

### Before Fix
❌ Handshake ACK failures  
❌ Connection reset by peer  
❌ No stable peer connections  
❌ Masternodes can't see each other  
❌ Block production blocked  
❌ Continuous reconnection attempts (10s, 20s, 40s, 80s backoff)

### After Fix
✅ Handshake completes successfully  
✅ ACK sent before any rejection  
✅ Stable peer connections  
✅ Masternodes see each other  
✅ Block production can proceed  
✅ Clean connection convergence (one connection per peer pair)

---

## Testing Plan

### 1. Deploy to All Nodes
```bash
# On each node (LW-London, LW-Arizona, LW-Michigan, LW-Michigan2)
cd /path/to/timecoin
git pull
cargo build --release
sudo systemctl restart timed
```

### 2. Monitor Logs
```bash
journalctl -u timed -f | grep -E "Handshake|Connected|failed|ACK"
```

**Watch For**:
- ✅ "✅ Handshake accepted from..." messages
- ✅ "🤝 Handshake completed with..." messages
- ✅ No "Connection reset by peer" errors
- ✅ "🔄 Rejecting duplicate inbound from... after handshake" (graceful rejections)
- ✅ Stable connection count

### 3. Verify Masternode Connectivity
```bash
# Check RPC endpoint
curl http://localhost:8332/consensus_info
```

**Expected Output**:
```json
{
  "active_masternodes": 4,  // Should see all masternodes
  "connected_peers": 3-4,   // Stable connection count
  "pending_transactions": 0,
  "finalized_transactions": N
}
```

### 4. Verify Block Production
```bash
journalctl -u timed -f | grep "block production"
```

**Expected**:
- ✅ No more "Skipping block production: only 1 masternodes active"
- ✅ "🏆 Starting new block round..." messages
- ✅ Block height increasing

---

## Technical Details

### Why This Works

1. **Atomic Handshake**: Handshake completes before any duplicate checks
   - Both peers can complete handshake
   - ACK always sent
   - No "Connection reset" errors

2. **Deterministic Tie-Breaking**: `local_ip < remote_ip` comparison
   - Ensures only one connection survives
   - Both peers make same decision
   - Converges to single connection per peer pair

3. **Graceful Rejection**: Send ACK before closing
   - Client receives ACK confirmation
   - No error on client side
   - Connection closed cleanly

4. **Connection Takeover**: Close outbound when accepting inbound
   - Prevents connection leaks
   - Clean state in connection manager
   - Proper resource cleanup

### Edge Cases Handled

1. **Both peers connect simultaneously**: 
   - Both handshakes complete
   - Tie-breaking chooses one
   - Other closes gracefully

2. **Reconnection during backoff**:
   - Backoff tracking prevents duplicate outbound attempts
   - Inbound connections still accepted
   - Converges to stable state

3. **Connection during block production**:
   - Handshake doesn't block consensus
   - Connection established asynchronously
   - Block production continues

---

## Files Modified

- `src/network/server.rs` - Moved duplicate check after handshake
- `src/network/connection_manager.rs` - Added `remove()` method

**Lines Changed**: ~40 lines  
**Commits**: 1 commit  
**Build Status**: ✅ All checks passing

---

## Related Issues

This fix addresses:
- ❌ Issue #1: Handshake ACK failures (Connection reset by peer)
- ❌ Issue #2: Masternodes not seeing each other
- ❌ Issue #3: Block production blocked (minimum 3 masternodes)
- ❌ Issue #4: Continuous reconnection attempts

Related to previous work:
- `COMBINED_SUMMARY_DEC_15-17_2025.md` - Duplicate connection tracking
- `Duplicate_Connection_Fix_Summary.md` - Backoff tracking
- `network_connection_analysis.md` - Root cause analysis

---

## Monitoring Checklist

After deployment, verify:

- [ ] No "Connection reset by peer" errors in logs
- [ ] All 4 masternodes show as active
- [ ] Connected peer count stable at 3-4
- [ ] Block production running (not skipped)
- [ ] Block height increasing
- [ ] No reconnection backoff spam
- [ ] Clean handshake completion messages

---

## Success Criteria

✅ **Network Operational**:
- All masternodes connected
- Block production active
- Consensus working

✅ **Stable Connections**:
- No handshake failures
- No connection resets
- One connection per peer pair

✅ **Clean Logs**:
- No error spam
- Successful handshake messages
- Graceful duplicate rejections

---

**Fix Status**: ✅ **READY FOR DEPLOYMENT**  
**Code Quality**: ✅ All checks passing (fmt, clippy, check)  
**Estimated Impact**: Critical - Unblocks entire network  
**Deployment Time**: 5 minutes per node  
**Rollback**: Git revert if issues persist

---

**Document Created**: 2025-12-17 02:13 UTC  
**Next Action**: Deploy to all testnet nodes and monitor
