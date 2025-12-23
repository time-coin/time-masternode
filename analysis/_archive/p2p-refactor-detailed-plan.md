# P2P Network Refactor - Detailed Implementation Plan
**Date:** 2025-12-18
**Status:** In Progress

## Problem Summary
1. **Peer identity confusion**: Peers identified by "IP:Port" instead of just "IP"
2. **Connection cycling**: Connections close every ~90 seconds
3. **Duplicate connections**: Same peer appears multiple times (inbound + outbound)
4. **Ping/Pong failures**: Outbound connections never receive pongs
5. **Block sync stuck**: Some nodes stuck at height 0, can't catch up

## Root Cause Analysis

### 1. Peer Identity Issue
**Current behavior:**
- Inbound from `50.28.104.50:12345` → Peer "50.28.104.50:12345"
- Outbound to `50.28.104.50:24100` → Peer "50.28.104.50:24100"
- Result: Same machine = 2 different peers!

**Correct behavior:**
- Peer identity = IP address ONLY
- One peer = one connection (bidirectional)
- Track ephemeral ports separately for socket I/O

### 2. Connection Cycling Issue
**Current behavior:**
- Peers reconnect every ~90 seconds
- Both directions try to establish connections simultaneously
- Deterministic tie-breaking closes one direction
- Result: Constant reconnection loop

**Correct behavior:**
- ONE connection per peer, kept open permanently
- Deterministic rule: lower IP initiates (is outbound)
- Connection only closes on error, not for "cleanup"

### 3. Ping/Pong Failure
**Current behavior:**
- Outbound: Sends ping, never receives pong → timeout
- Inbound: Receives ping, sends pong → works
- Root cause: Message routing issue or separate message loops

**Correct behavior:**
- Single message loop per connection
- Both ping and pong handled in same loop
- Proper response routing

## Implementation Steps

### Step 1: Refactor Peer Registry ✅ (READY)

**File:** `src/network/peer_registry.rs`

**Changes:**
```rust
// OLD - peer identified by "IP:Port"
peers: HashMap<String, PeerInfo>  // key = "1.2.3.4:24100"

// NEW - peer identified by IP only
peers: HashMap<IpAddr, PeerInfo>  // key = IpAddr(1.2.3.4)

struct PeerInfo {
    ip: IpAddr,                    // Identity
    listening_port: u16,           // Default 24100
    active_connection: Option<ConnectionHandle>,  // Only one!
    last_seen: Instant,
}

struct ConnectionHandle {
    socket_addr: SocketAddr,       // Actual socket (includes ephemeral port)
    direction: ConnectionDirection,
    writer: Arc<Mutex<Writer>>,
}
```

### Step 2: Unified Connection Management ✅ (IN PROGRESS)

**File:** `src/network/peer_connection.rs`

**Current state:** Partially implemented
**Remaining work:**
1. Remove duplicate message loop logic
2. Ensure single message loop handles all message types
3. Fix pong routing for outbound connections

**Key changes needed:**
```rust
impl PeerConnection {
    // KEEP: Single message loop for all messages
    pub async fn run_message_loop() {
        loop {
            tokio::select! {
                // Handle ALL incoming messages (ping, pong, blocks, etc.)
                result = self.reader.read_line(&mut buffer) => {
                    self.handle_message(&buffer).await
                }
                
                // Send periodic pings
                _ = ping_interval.tick() => {
                    self.send_ping().await
                }
            }
        }
    }
    
    // KEEP: Handle all message types in ONE place
    async fn handle_message(&self, line: &str) {
        match message {
            Ping => self.handle_ping().await,
            Pong => self.handle_pong().await,  // ← Must work for outbound!
            Block => self.handle_block().await,
            // ... etc
        }
    }
}
```

### Step 3: Connection Deduplication Logic ✅ (NEEDS IMPLEMENTATION)

**File:** `src/network/client.rs` + `src/network/server.rs`

**Logic:**
```rust
// When accepting inbound connection
async fn handle_inbound_connection(stream: TcpStream) {
    let peer_ip = stream.peer_addr().ip();
    
    // Check if we already have a connection to this IP
    if let Some(existing) = peer_registry.get_connection(peer_ip) {
        // Deterministic tie-breaker
        if should_keep_existing(local_ip, peer_ip, existing.direction) {
            // Close new inbound, keep existing
            drop(stream);
            return;
        } else {
            // Close existing, accept new inbound
            existing.close().await;
        }
    }
    
    // Accept and register new connection
    let conn = PeerConnection::new_inbound(stream).await;
    peer_registry.register_connection(peer_ip, conn);
    conn.run_message_loop().await;
}

// When making outbound connection
async fn connect_to_peer(peer_ip: IpAddr) {
    // Check if already connected
    if peer_registry.has_connection(peer_ip) {
        return; // Skip, already connected
    }
    
    // Check deterministic rule
    if !should_initiate_connection(local_ip, peer_ip) {
        return; // Let them connect to us
    }
    
    // Establish outbound
    let conn = PeerConnection::new_outbound(peer_ip, 24100).await;
    peer_registry.register_connection(peer_ip, conn);
    conn.run_message_loop().await;
}

fn should_initiate_connection(local_ip: IpAddr, peer_ip: IpAddr) -> bool {
    // Lower IP initiates
    local_ip < peer_ip
}
```

### Step 4: Fix Ping/Pong in Single Loop ⚠️ (CRITICAL)

**Current issue:** Outbound connections timeout because pongs aren't being received/processed

**Root cause analysis needed:**
1. Are pongs actually being sent by remote peer?
2. Are pongs being received but not processed?
3. Is there a separate message loop interfering?

**Debug steps:**
```rust
// In handle_pong() - ADD DETAILED LOGGING
async fn handle_pong(&self, nonce: u64) {
    info!("📨 [{:?}] RECEIVED PONG from {} (nonce: {})", 
          self.direction, self.peer_ip, nonce);
    
    let mut state = self.ping_state.write().await;
    let found = state.record_pong_received(nonce);
    
    info!("📊 Pong matched: {}, pending pings: {}", 
          found, state.pending_pings.len());
}
```

### Step 5: Remove Connection Cycling ✅ (NEEDS IMPLEMENTATION)

**Files to modify:**
- `src/network/client.rs` - Remove reconnection loop
- `src/network/server.rs` - Remove connection cleanup

**Changes:**
```rust
// REMOVE: Periodic reconnection timers
// REMOVE: Connection cleanup based on age
// KEEP: Only reconnect on actual errors (EOF, timeout, write failure)

// Connection should live forever unless:
// 1. Remote peer closes (EOF)
// 2. Network error
// 3. Ping timeout (3 missed pongs)
```

### Step 6: Fix Block Sync ⚠️ (BLOCKED BY PING/PONG)

**Current issue:** Nodes stuck at height 0 can't sync

**Likely cause:** Connections keep cycling, no stable connection for block transfer

**Once ping/pong fixed:**
1. Connections stay stable
2. Block requests can complete
3. Nodes sync to current height

## Testing Plan

### Phase 1: Verify Single Connection Per Peer
1. Start 3 nodes
2. Check peer registry - each peer should appear ONCE
3. Check logs - no duplicate "IP:Port" entries

### Phase 2: Verify Ping/Pong Works
1. Monitor outbound connections
2. Confirm pongs are received and processed
3. No timeout disconnections

### Phase 3: Verify Connection Stability
1. Connections should stay open indefinitely
2. No cycling/reconnection loops
3. Only disconnect on actual errors

### Phase 4: Verify Block Sync
1. Start node from genesis (height 0)
2. Should sync to network height
3. No "stuck at height 0" issues

## Current Status

✅ **Completed:**
- Analysis and root cause identification
- Architecture design
- Plan documentation
- Partial peer_connection.rs implementation

⚠️ **In Progress:**
- Step 2: Fix message loop for pong handling

❌ **Not Started:**
- Step 1: Refactor peer registry to use IP-only keys
- Step 3: Implement connection deduplication
- Step 5: Remove connection cycling
- Step 6: Verify block sync

## Next Actions

1. **IMMEDIATE:** Add debug logging to trace why pongs aren't working on outbound
2. Deploy and test with detailed logs
3. Fix pong handling based on findings
4. Then proceed with peer registry refactor
5. Implement connection deduplication
6. Remove reconnection timers
7. Test full sync from height 0

## Expected Outcomes

After completion:
- ✅ One connection per peer (not per IP:Port)
- ✅ Connections stable (no cycling)
- ✅ Ping/pong works bidirectionally
- ✅ Block sync works reliably
- ✅ Nodes can stay synced for days/weeks without issues
