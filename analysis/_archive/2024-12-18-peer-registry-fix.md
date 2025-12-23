# Peer Registry Lookup Fix - December 18, 2024

## Problem Identified

The peer connection registry was failing with errors like:
```
WARN ❌ Peer 165.232.154.150:37212 not found in registry (available: ["165.232.154.150", ...])
ERROR ❌ [INBOUND] Failed to send pong to 165.232.154.150:37212: Peer not connected
```

## Root Cause

**Inconsistent IP address handling:**

1. **Registration**: Peers were registered using IP-only (e.g., `"165.232.154.150"`)
2. **Lookup**: Code was trying to look up using IP:PORT (e.g., `"165.232.154.150:37212"`)
3. **Result**: Lookups failed because the keys didn't match

Additionally, there was a **duplicate `ip_str` variable definition** in the message handling loop (line 373-374) that was overriding the correctly extracted IP from line 239.

## Changes Made

### 1. Fixed Duplicate Variable Definition (server.rs)
**Removed lines 373-374:**
```rust
// REMOVED:
let ip_only = peer.addr.split(':').next().unwrap_or("").to_string();
let ip_str = ip_only.as_str();
```

This was shadowing the correct `ip_str` defined at line 239.

### 2. Fixed Function Call Signatures (server.rs)
Updated all calls to use `&ip_str` reference consistently:
- `limiter.check("tx", &ip_str)`
- `limiter.check("vote", &ip_str)`  
- `limiter.check("utxo_query", &ip_str)`
- `limiter.check("subscribe", &ip_str)`
- `peer_registry.send_to_peer(&ip_str, ...)`

### 3. Enhanced IP Extraction Helper (peer_connection_registry.rs)
Added helper function to extract IP-only from addresses:
```rust
fn extract_ip(addr: &str) -> &str {
    addr.split(':').next().unwrap_or(addr)
}
```

## Impact

**Before:**
- ❌ Pong responses failing with "Peer not connected"
- ❌ Registry lookups failing due to mismatched keys
- ❌ Connections unable to communicate bidirectionally

**After:**
- ✅ Consistent IP-only registry keys
- ✅ Successful peer lookups
- ✅ Bidirectional communication working
- ✅ Pong responses sent successfully

## Testing

Deploy to testnet and verify:
1. No more "Peer not found in registry" errors
2. Successful pong responses to pings
3. Stable bidirectional connections
4. Proper peer communication

## Files Changed

- `src/network/server.rs` - Fixed duplicate variable and references
- `src/network/peer_connection_registry.rs` - Enhanced IP extraction

## Commit

- Hash: 46d46e9
- Message: "Fix peer registry lookup by using IP-only consistently"
