# Phase 2: Masternode Connection Protection & Stability

**Status**: ✅ IMPLEMENTED  
**Date**: 2026-01-03  
**Objective**: Ensure whitelisted masternodes remain connected with aggressive reconnection and enhanced protection

---

## Overview

Phase 2 builds on Phase 1's relaxed ping/pong timeouts by adding comprehensive protection mechanisms to prevent whitelisted masternodes from being disconnected and ensuring rapid reconnection if disconnections occur.

## Problem Statement

From the logs, masternodes were:
1. Disconnecting despite whitelist status
2. Falling out of sync (heights: 1919-4805)
3. Missing pongs but not being protected
4. Not reconnecting aggressively enough

## Phase 2 Enhancements

### 1. Connection Slot Protection (Priority Slots)

**Location**: `src/network/connection_manager.rs`

```rust
const MAX_TOTAL_CONNECTIONS: usize = 125;
const RESERVED_MASTERNODE_SLOTS: usize = 50;  // Reserved for whitelisted masternodes
const MAX_REGULAR_PEER_CONNECTIONS: usize = 75; // Remaining slots for regular peers
```

**Features**:
- Whitelisted masternodes bypass regular connection limits
- 50 slots reserved exclusively for trusted masternodes
- Regular peers limited to 75 slots
- Prevents resource exhaustion attacks while protecting masternodes

**Implementation**:
```rust
pub fn can_accept_inbound(&self, peer_ip: &str, is_whitelisted: bool) -> Result<(), String> {
    // Whitelisted masternodes bypass regular connection limits
    if is_whitelisted {
        let total = self.connected_count();
        if total >= MAX_TOTAL_CONNECTIONS {
            return Err("Max total connections reached");
        }
        return Ok(()); // Allow whitelisted connection - bypass all other checks
    }
    
    // Regular peers face stricter limits
    let regular_count = self.count_regular_peer_connections();
    if regular_count >= MAX_REGULAR_PEER_CONNECTIONS {
        return Err("Max regular peer connections reached");
    }
    // ... additional checks
}
```

### 2. Whitelist Status Tracking

**Location**: `src/network/connection_manager.rs`

**Features**:
- `mark_whitelisted(peer_ip)`: Mark connection as protected masternode
- `is_whitelisted(peer_ip)`: Check whitelist status
- `should_protect(peer_ip)`: Convenience method for protection checks
- Tracked in `ConnectionInfo` struct

**Implementation**:
```rust
pub fn mark_whitelisted(&self, peer_ip: &str) {
    if let Some(mut entry) = self.connections.get_mut(peer_ip) {
        entry.is_whitelisted = true;
    }
}

pub fn should_protect(&self, peer_ip: &str) -> bool {
    self.is_whitelisted(peer_ip)
}
```

### 3. Aggressive Reconnection for Masternodes

**Location**: `src/network/client.rs`

**Before** (Phase 1):
- Initial retry delay: 5 seconds (all peers)
- Max failures: 20 for masternodes, 10 for regular
- Max backoff: 300 seconds (5 minutes)

**After** (Phase 2):
- Initial retry delay: **2 seconds for masternodes**, 5s for regular peers
- Max failures: **50 for masternodes** (up from 20), 10 for regular
- Max backoff: **60 seconds for masternodes**, 300s for regular peers

**Exponential Backoff**:
- Masternodes: 2s → 4s → 8s → 16s → 32s → **60s (cap)**
- Regular peers: 5s → 10s → 20s → 40s → 80s → 160s → **300s (cap)**

**Implementation**:
```rust
// Phase 2: Aggressive reconnection for whitelisted masternodes
let mut retry_delay = if is_masternode { 2 } else { 5 }; // Masternodes reconnect faster
let max_failures = if is_masternode { 50 } else { 10 }; // Masternodes get many more retries

// Phase 2: Exponential backoff with lower max for masternodes
retry_delay = if is_masternode {
    (retry_delay * 2).min(60) // Cap at 1 minute for masternodes
} else {
    (retry_delay * 2).min(300) // Cap at 5 minutes for regular peers
};
```

### 4. Connection Protection Marking

**Location**: `src/network/client.rs`

When establishing outbound connections to masternodes:

```rust
// Mark as connected in connection_manager
connection_manager.mark_connected(&peer_ip);

// Phase 2: Mark whitelisted masternodes in connection_manager for protection
if is_masternode {
    connection_manager.mark_whitelisted(&peer_ip);
    tracing::info!("🛡️ Marked {} as whitelisted masternode with enhanced protection", peer_ip);
}
```

This ensures:
- Connection manager knows this peer is protected
- Can apply special handling (relaxed timeouts, priority slots)
- Monitoring can identify protected connections

### 5. Enhanced Ping/Pong Timeouts (From Phase 1)

**Already Implemented** in `src/network/peer_connection.rs`:

**Regular Peers**:
- Timeout: 90 seconds
- Max missed pongs: 3

**Whitelisted Masternodes**:
- Timeout: **180 seconds** (3 minutes)
- Max missed pongs: **6**

```rust
const PONG_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_MISSED_PONGS: u32 = 3;

// Phase 1: Relaxed timeouts for whitelisted masternodes
const WHITELISTED_PONG_TIMEOUT: Duration = Duration::from_secs(180); // 3 minutes
const WHITELISTED_MAX_MISSED_PONGS: u32 = 6; // Allow more missed pongs
```

### 6. Whitelist Integration

**Whitelist Sources**:
1. **Config file** (`config.toml`): `whitelisted_peers = ["IP1", "IP2", ...]`
2. **time-coin.io API**: Peers fetched from official API are automatically trusted

**Implementation Flow** (`src/main.rs`):
```rust
// Collect IPs for whitelisting (these are from time-coin.io, so trusted)
let discovered_peer_ips: Vec<String> = discovered_peers
    .iter()
    .map(|p| p.address.clone())
    .collect();

// Prepare combined whitelist BEFORE creating server
let mut combined_whitelist = config.network.whitelisted_peers.clone();
combined_whitelist.extend(discovered_peer_ips.clone());

// Server initialized with combined whitelist
NetworkServer::new_with_blacklist(
    &p2p_addr,
    // ...
    config.network.blacklisted_peers.clone(),
    combined_whitelist, // ← Combined whitelist
    attestation_system.clone(),
)
```

**Whitelist Propagation**:
1. Server marks inbound connections from whitelisted IPs
2. Client marks outbound connections to masternodes as whitelisted
3. Blacklist system exempts whitelisted IPs from all bans

**Important**: 
- ❌ Masternodes announced via P2P are **NOT** auto-whitelisted
- ✅ Only peers from time-coin.io and config are trusted
- This prevents rogue nodes from claiming masternode status

---

## Testing & Validation

### What to Monitor

1. **Connection Stability**:
   ```
   🛡️ Marked <IP> as whitelisted masternode with enhanced protection
   ```

2. **Reconnection Speed**:
   ```
   [MASTERNODE] Reconnecting to <IP> in 2s...  (initial)
   [MASTERNODE] Reconnecting to <IP> in 4s...  (after 1 failure)
   [MASTERNODE] Reconnecting to <IP> in 8s...  (after 2 failures)
   ```

3. **Timeout Protection**:
   ```
   ⚠️ [Outbound] WHITELIST VIOLATION: Masternode <IP> unresponsive after 6 missed pongs (relaxed timeout: 180s)
   ```

4. **Connection Slots**:
   ```
   ✅ [WHITELIST] Accepting inbound connection from <IP> 
      (total: X, inbound: Y, whitelisted: Z)
   ```

### Expected Behavior

**Scenario 1: Masternode Temporary Network Issue**
- Detection: 6 missed pongs over 180 seconds
- Action: Disconnect, immediate 2-second reconnect attempt
- Result: Quick recovery, minimal sync disruption

**Scenario 2: Masternode Extended Downtime**
- Detection: Multiple failed reconnection attempts
- Action: Exponential backoff up to 60 seconds
- Result: Up to 50 retry attempts over extended period
- Note: Regular peers would give up after 10 attempts

**Scenario 3: Resource Attack**
- Regular peers: Limited to 75 connections
- Masternodes: Protected with 50 reserved slots
- Result: Masternodes maintain connectivity even under attack

**Scenario 4: Duplicate Connection**
- Peer registry detects existing connection
- Rejects duplicate to prevent confusion
- Maintains single, stable connection

---

## Integration with Phase 1

Phase 2 **extends** Phase 1's foundation:

| Feature | Phase 1 | Phase 2 |
|---------|---------|---------|
| Ping/Pong Timeout | ✅ 180s for masternodes | ✅ Kept |
| Max Missed Pongs | ✅ 6 for masternodes | ✅ Kept |
| Reconnection Delay | ❌ 5s for all | ✅ 2s for masternodes |
| Max Retry Attempts | ⚠️ 20 for masternodes | ✅ 50 for masternodes |
| Backoff Cap | ❌ 300s for all | ✅ 60s for masternodes |
| Connection Slots | ❌ No reservation | ✅ 50 slots reserved |
| Whitelist Marking | ❌ Not tracked | ✅ Tracked in ConnectionManager |

---

## Configuration

Add to `config.toml`:
```toml
[network]
# Manually specify trusted peers (optional - time-coin.io is primary source)
whitelisted_peers = [
    "192.168.1.10",
    "192.168.1.11",
]

# Block misbehaving peers
blacklisted_peers = [
    "10.0.0.5",
]
```

**Note**: The system automatically fetches and whitelists peers from time-coin.io API.

---

## Code Locations

### Modified Files

1. **`src/network/client.rs`**
   - Aggressive reconnection logic (lines 495-544)
   - Whitelist marking on connection (lines 645-650)

2. **`src/network/connection_manager.rs`**
   - Reserved slot allocation (lines 14-18)
   - Whitelist bypass logic (lines 78-90)
   - Protection methods (lines 458-476)

3. **`src/network/peer_connection.rs`**
   - Enhanced timeout constants (lines 181-182)
   - Timeout checking with whitelist support (lines 407-436)

### Unchanged (From Phase 1)

4. **`src/network/server.rs`**
   - Whitelist initialization from config (lines 141-149)
   - Inbound connection whitelist check (lines 225-228)

5. **`src/network/blacklist.rs`**
   - Whitelist exemption logic (lines 32-50, 82-89)

---

## Benefits

### 1. Stability
- Masternodes stay connected longer (180s timeout vs 90s)
- Immediate reconnection (2s vs 5s) minimizes downtime
- 50 retry attempts ensure recovery from extended issues

### 2. Sync Reliability
- Consistent connections prevent blockchain divergence
- Rapid recovery maintains sync state
- Protected slots ensure connectivity during network stress

### 3. Attack Resistance
- Reserved slots protect masternodes from resource exhaustion
- Whitelisted peers bypass rate limits and connection caps
- DoS attacks cannot evict trusted masternodes

### 4. Network Efficiency
- Less churn = fewer handshakes = lower bandwidth
- Stable peer set = better block propagation
- Priority reconnection = faster network healing

---

## Future Enhancements (Phase 3+)

1. **Dynamic Whitelist Management**
   - Periodic refresh from time-coin.io
   - Masternode health scoring
   - Automatic whitelisting of proven reliable peers

2. **Connection Quality Metrics**
   - Track latency, packet loss, throughput
   - Prioritize high-quality connections
   - Adaptive timeout adjustment based on quality

3. **Intelligent Reconnection**
   - Skip peers with consistent failures
   - Prioritize recently successful connections
   - Geographic/network diversity optimization

4. **Monitoring Dashboard**
   - Real-time whitelist status
   - Connection health visualization
   - Alert on masternode disconnections

---

## Troubleshooting

### Issue: Masternodes still disconnecting frequently

**Check**:
1. Verify peer is in whitelist: `grep "Marked.*as whitelisted" logs/`
2. Check timeout logs: `grep "WHITELIST VIOLATION" logs/`
3. Verify connection marking: `grep "🛡️" logs/`

**Solution**: Ensure peer IP is in `combined_whitelist` and matches exactly

### Issue: Slow reconnection to masternodes

**Check**:
1. Look for reconnection logs: `grep "Reconnecting to.*in.*s" logs/`
2. Verify `is_masternode` flag: Should see `[MASTERNODE]` prefix

**Solution**: Confirm peer is being connected as masternode in client.rs

### Issue: Regular peers filling masternode slots

**Check**:
1. Count whitelisted connections: Check `whitelisted: X` in logs
2. Verify slot reservation: Should show reserved slots message

**Solution**: Ensure `mark_whitelisted()` is being called for masternodes

---

## Summary

Phase 2 delivers **robust protection** for whitelisted masternodes through:

✅ **2-second reconnection** (vs 5s for regular peers)  
✅ **50 reserved connection slots** (out of 125 total)  
✅ **50 retry attempts** (vs 10 for regular peers)  
✅ **60-second max backoff** (vs 5 minutes for regular peers)  
✅ **Protected from DoS** (whitelist exemption from bans/limits)  
✅ **3-minute ping timeout** (vs 90s for regular peers)  
✅ **6 allowed missed pongs** (vs 3 for regular peers)  

These enhancements ensure masternodes remain connected, synchronized, and resilient against network issues.

---

**Next Steps**: Monitor logs for stability improvements and prepare Phase 3 (advanced sync optimizations).
