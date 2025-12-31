# P2P Refactor Implementation Plan
**Date:** December 18, 2024 06:00 UTC

## Phase 1.2: Integration Plan - DETAILED STEPS

### Current State Analysis

#### Files Involved:
1. `src/network/client.rs` - Outbound connections
2. `src/network/server.rs` - Inbound connections  
3. `src/network/connection_manager.rs` - IP tracking (duplicate prevention)
4. `src/network/peer_connection_registry.rs` - Peer metadata
5. `src/network/peer_state.rs` - **NEW** Unified connection state

#### Current Flow:
```
CLIENT (Outbound):
1. Get peers from PeerManager
2. Check ConnectionManager (is IP already connected?)
3. Create TCP connection
4. Handshake
5. Spawn message loop (handle_peer_messages)
6. Register in PeerConnectionRegistry with "IP:PORT"

SERVER (Inbound):  
1. Accept TCP connection
2. Extract IP from socket
3. Handshake
4. Spawn message loop (handle_client)
5. Register in PeerConnectionRegistry with "IP:PORT"

PROBLEM: Same IP registered twice with different ports!
```

### Step-by-Step Integration

#### Step 1: Add PeerStateManager to Structs
**Files:** `client.rs`, `server.rs`, `main.rs`

**Changes:**
1. Add `peer_state: Arc<PeerStateManager>` field to both NetworkClient and NetworkServer
2. Create single shared instance in main.rs
3. Pass to both client and server constructors

**Code Template:**
```rust
// In main.rs
use network::peer_state::PeerStateManager;

let peer_state = Arc::new(PeerStateManager::new());

let network_client = NetworkClient::new(
    // ... existing args ...
    peer_state.clone(),
);

let network_server = NetworkServer::new(
    // ... existing args ...
    peer_state.clone(),
);
```

#### Step 2: Update Connection Establishment (Client)
**File:** `client.rs`

**Current code location:** Around line 170 (in handle_peer_messages function)

**Changes:**
```rust
// BEFORE: Just check ConnectionManager
if !connection_manager.mark_connecting(&ip).await {
    continue;
}

// AFTER: Check PeerStateManager first
if peer_state.has_connection(&ip).await {
    tracing::debug!("Already connected to {}, skipping", ip);
    continue;
}

// Then connect and register
let stream = TcpStream::connect(socket_addr).await?;
let remote_addr = stream.peer_addr()?;

// Create message channel
let (tx, mut rx) = mpsc::unbounded_channel();

// Register in PeerStateManager
if !peer_state.add_connection(
    ip.clone(),
    remote_addr,
    ConnectionDirection::Outbound,
    tx.clone(),
).await? {
    tracing::warn!("Race condition: {} connected while we were connecting", ip);
    continue;
}

// Spawn writer task
tokio::spawn(async move {
    while let Some(msg) = rx.recv().await {
        // Write message to socket
    }
});
```

#### Step 3: Update Connection Acceptance (Server)
**File:** `server.rs`

**Current code location:** In accept loop

**Changes:**
```rust
// Get IP from socket
let peer_ip = socket.peer_addr()?.ip().to_string();

// Check if already connected
if peer_state.has_connection(&peer_ip).await {
    // Decide: keep inbound or keep existing?
    // For now: prefer inbound (we know their ephemeral port)
    tracing::info!("Replacing existing connection to {} with new inbound", peer_ip);
    peer_state.remove_connection(&peer_ip).await;
}

// Create message channel
let (tx, mut rx) = mpsc::unbounded_channel();

// Register connection
peer_state.add_connection(
    peer_ip.clone(),
    socket.peer_addr()?,
    ConnectionDirection::Inbound,
    tx.clone(),
).await?;

// Spawn writer task
tokio::spawn(async move {
    while let Some(msg) = rx.recv().await {
        // Write to socket
    }
});
```

#### Step 4: Update Message Sending
**Files:** All files that send messages to peers

**Before:**
```rust
// Direct socket write or complex message passing
writer.write_all(&bytes).await?;
```

**After:**
```rust
// Send via PeerStateManager
peer_state.send_to_peer(&ip, message).await?;

// Or broadcast
peer_state.broadcast(message).await;
```

#### Step 5: Fix Ping/Pong
**File:** `client.rs` and any ping handler

**Add pong reception logging:**
```rust
NetworkMessage::Pong { nonce } => {
    tracing::info!("📨 [OUTBOUND] Received pong from {} (nonce: {})", peer_ip, nonce);
    
    // Update activity
    peer_state.mark_peer_active(&peer_ip).await;
    
    // Clear missed pings
    if let Some(conn) = peer_state.get_connection(&peer_ip).await {
        let mut missed = conn.missed_pings.write().await;
        *missed = 0;
    }
}
```

#### Step 6: Remove Port from Peer Registry
**File:** `peer_connection_registry.rs`

**Change key from "IP:PORT" to just "IP":**
```rust
// Before
peers.insert(format!("{}:{}", ip, port), peer_data);

// After  
peers.insert(ip.to_string(), peer_data);
```

**Update all lookups to use IP only**

#### Step 7: Update ConnectionManager
**File:** `connection_manager.rs`

**Simplify to just track IPs (or deprecate entirely since PeerStateManager does this):**
```rust
// Option 1: Keep for backoff tracking only
// Option 2: Merge into PeerStateManager
// Option 3: Remove and use PeerStateManager.has_connection()
```

### Testing Checklist

After each step:
- [ ] Code compiles
- [ ] Client can connect to server
- [ ] No duplicate connections for same IP
- [ ] Messages send successfully
- [ ] Ping/pong works
- [ ] Connections persist (no cycling)

### Rollback Plan

If issues arise:
1. Git commit after each working step
2. Can revert individual steps
3. Keep old code commented until confirmed working

### Success Metrics

- [ ] Only one entry per IP in peer registry
- [ ] Connections stay open >5 minutes
- [ ] Ping response rate >95%
- [ ] No "already connected" races
- [ ] Block sync works

---

## Implementation Order

1. ✅ Create peer_state.rs
2. ⏳ Add PeerStateManager to main.rs (NEXT)
3. Update NetworkClient constructor
4. Update NetworkServer constructor
5. Update client connection logic
6. Update server accept logic
7. Update message sending
8. Fix ping/pong
9. Update peer registry
10. Test and validate

**Current Step:** #2 - Add to main.rs
**Estimated Time:** 2-3 hours for full integration
**Risk Level:** Medium (major refactor but well-planned)

