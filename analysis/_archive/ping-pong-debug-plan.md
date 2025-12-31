# Ping/Pong Debug Plan
**Date:** 2025-12-18
**Priority:** CRITICAL 🚨

## Problem Statement

Outbound connections are NOT receiving pongs, causing ping timeouts and connection cycling every ~90 seconds.

### Evidence from Logs

**Server (Inbound) - WORKS:**
```
Dec 18 01:45:17 LW-London:
  INFO 📨 [INBOUND] Received ping from 165.232.154.150:55488 (nonce: 15876933047231905), sending pong
  INFO ✅ [INBOUND] Sent pong to 165.232.154.150:55488 (nonce: 15876933047231905)
```

**Client (Outbound) - BROKEN:**
```
Dec 18 01:45:17 LW-London:
  INFO 📤 Sent ping to 178.128.199.144 (nonce: 11900434792055248454)
  WARN ⚠️ Ping timeout from 178.128.199.144 (nonce: 11900434792055248454, missed: 1/3)
  INFO 📤 Sent ping to 178.128.199.144 (nonce: 6523960411979597777)
  
  # NEVER SEES:
  # INFO 📨 [OUTBOUND] Received pong from 178.128.199.144 (nonce: ...)
```

## Root Cause Analysis

### Theory 1: Pongs Not Being Sent ❌ DISPROVEN
Server logs show: `✅ [INBOUND] Sent pong`  
Therefore, pongs ARE being sent.

### Theory 2: Pongs Not Being Received ✅ LIKELY
Client never logs: `📨 [OUTBOUND] Received pong`  
This means the message is not arriving at the client's message loop.

### Theory 3: Writer Ownership Issue ✅ FOUND!

**server.rs lines 342-345:**
```rust
// Register writer in peer registry after successful handshake
if let Some(w) = writer.take() {
    peer_registry.register_peer(ip_str.clone(), w).await;
}
```

After this, `writer` is `None` in the server message loop!

**server.rs line 741:**
```rust
let _ = peer_registry.send_to_peer(ip_str, pong_msg).await;
```

This SHOULD work because send_to_peer gets writer from registry...

### Theory 4: IP String Mismatch ⚠️ POSSIBLE

Server receives connection from: `50.28.104.50:12345` (ephemeral port)  
Server extracts IP: `50.28.104.50`  
Server registers in peer_registry: `50.28.104.50`

Client connects to: `50.28.104.50:24100` (listening port)  
Client tracks connection as: `50.28.104.50`

When server tries to send pong:
```rust
peer_registry.send_to_peer("50.28.104.50", pong_msg).await
```

**This should match!** Both use IP without port.

But wait... let me check what IP the client uses:

**client.rs:** Uses `ip` variable from start of function
**server.rs:** Extracts `ip_str` from peer.addr

They should both be just the IP address...

## Debugging Steps

### Step 1: Add Writer State Logging
Add logging to confirm writer is in registry:
- When writer is registered
- When send_to_peer is called
- Success/failure of write operation

### Step 2: Add IP Comparison Logging  
Log exact strings used:
- Server: What IP is used to register writer
- Server: What IP is used to send pong
- Client: What IP the client expects pong from

### Step 3: Check Write Errors
The current code ignores write errors:
```rust
let _ = peer_registry.send_to_peer(ip_str, pong_msg).await;
```

Change to:
```rust
if let Err(e) = peer_registry.send_to_peer(&ip_str, pong_msg).await {
    tracing::error!("❌ [INBOUND] Failed to send pong to {}: {}", peer.addr, e);
}
```

### Step 4: Verify Message Format
Ensure pong is serialized correctly:
- Add logging before serialization
- Log the JSON being sent
- Verify it matches expected format

## Proposed Fix

### Option A: Enhanced Logging (Immediate)
Add comprehensive logging to identify exact failure point:

1. **In peer_registry.rs send_to_peer():**
```rust
pub async fn send_to_peer(&self, peer_ip: &str, message: NetworkMessage) -> Result<(), String> {
    tracing::debug!("🔍 send_to_peer called for IP: {}", peer_ip);
    
    let mut connections = self.connections.write().await;
    tracing::debug!("🔍 Registry has {} connections", connections.len());
    
    if let Some(writer) = connections.get_mut(peer_ip) {
        tracing::debug!("✅ Found writer for {}", peer_ip);
        
        let msg_json = serde_json::to_string(&message)
            .map_err(|e| format!("Failed to serialize message: {}", e))?;
        
        tracing::debug!("📝 Serialized message: {}", msg_json);
        
        writer.write_all(format!("{}\n", msg_json).as_bytes()).await
            .map_err(|e| format!("Failed to write to peer {}: {}", peer_ip, e))?;
        
        writer.flush().await
            .map_err(|e| format!("Failed to flush to peer {}: {}", peer_ip, e))?;
        
        tracing::info!("✅ Successfully sent message to {}", peer_ip);
        Ok(())
    } else {
        tracing::error!("❌ Peer {} not found in registry (available: {:?})", 
            peer_ip, connections.keys().collect::<Vec<_>>());
        Err(format!("Peer {} not connected", peer_ip))
    }
}
```

2. **In server.rs pong handling:**
```rust
NetworkMessage::Ping { nonce, timestamp: _ } => {
    let pong_msg = NetworkMessage::Pong {
        nonce: *nonce,
        timestamp: chrono::Utc::now().timestamp(),
    };
    tracing::info!("📨 [INBOUND] Received ping from {} (nonce: {}), sending pong", peer.addr, nonce);
    tracing::debug!("🔍 Sending pong to IP: {}", ip_str);
    
    match peer_registry.send_to_peer(&ip_str, pong_msg).await {
        Ok(()) => {
            tracing::info!("✅ [INBOUND] Sent pong to {} (nonce: {})", peer.addr, nonce);
        }
        Err(e) => {
            tracing::error!("❌ [INBOUND] Failed to send pong to {}: {}", peer.addr, e);
        }
    }
}
```

### Option B: Separate Bidirectional Channel (If logging reveals design flaw)
If we discover the writer is not accessible or messages aren't flowing:

1. Keep writer in message loop (don't move to registry)
2. Use registry only for targeted messages from OTHER tasks
3. Or use mpsc channel to send messages to the loop

## Expected Outcome

After adding logging, we should see ONE of:

1. ✅ **"Peer X not found in registry"** → IP mismatch issue
2. ✅ **"Failed to write to peer"** → Socket error
3. ✅ **"Failed to serialize"** → Message format issue
4. ✅ **"Successfully sent message"** → Message sent but client not receiving (need to check client socket read)

## Next Actions

1. ✅ Add enhanced logging (Option A)
2. ✅ Deploy to test nodes
3. ✅ Collect logs showing exact failure point
4. ✅ Apply targeted fix based on findings
5. ✅ Verify connections stay stable

---
**Status:** Ready to implement logging  
**Timeline:** Should have answer within 10 minutes of deployment
