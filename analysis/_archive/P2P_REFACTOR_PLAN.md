# P2P Network Architecture Refactor Plan

**Created:** 2024-12-18  
**Status:** 🔨 In Progress  
**Priority:** CRITICAL

## Progress Tracker
- ✅ 1.1: IP-based Peer Identity - COMPLETE
- ✅ 1.2: Unified Connection Management - COMPLETE  
- ✅ 1.3: Deterministic Connection Direction - COMPLETE
  - Added `should_connect_to()` method using IP comparison
  - Integrated into server.rs handshake logic
  - Local IP set in ConnectionManager on startup
- ⏳ 1.4: Persistent Connections
- ⏳ 2.1-2.3: Unified Message Handler
- ⏳ 3.1-3.2: Testing & Validation

## Changes Made (Step 1.3)

### 1. connection_manager.rs
- Added `local_ip: Arc<RwLock<Option<String>>>` field
- Added `set_local_ip()` method to configure local IP
- Added `should_connect_to()` method with proper IP comparison:
  - Parses IPs to IpAddr for numeric comparison
  - Handles IPv4, IPv6, and mixed scenarios
  - Deterministic: higher IP connects OUT to lower IP
  - Fallback to string comparison if parsing fails

### 2. server.rs (line ~300)
- Replaced string comparison with `connection_manager.should_connect_to()`
- Clearer logging messages indicating direction logic
- Same behavior but using centralized method

### 3. main.rs (line ~422)
- Call `connection_manager.set_local_ip()` after detecting local IP
- Ensures ConnectionManager knows which direction to enforce

## Executive Summary

The current P2P implementation has fundamental architectural issues causing:
- Connection instability (cycling every 90s)
- Peer registry bloat (same IP counted multiple times)
- Ping/pong failures on outbound connections
- Block sync failures
- Unnecessary complexity with separate client/server

## Root Cause Analysis

### 1. **Dual Identity Problem**
- **Current:** Peers identified by `IP:PORT` 
- **Issue:** Same peer appears twice in registry
  - Inbound: `50.28.104.50:12345` (ephemeral port)
  - Outbound: `50.28.104.50:24100` (listening port)
- **Result:** Peer count bloat, duplicate tracking

### 2. **Client/Server Split**
- **Current:** Separate `client.rs` and `server.rs`
- **Issue:** 
  - Duplicated message handling logic
  - Different ping/pong behavior
  - Increased complexity
  - No true P2P symmetry
- **Result:** Inbound pongs work, outbound pongs fail

### 3. **Connection Cycling**
- **Current:** Connections close every ~90 seconds
- **Issue:** 
  - Heartbeat/port changes trigger reconnections
  - Deterministic connection direction logic closes connections
  - No persistent connection maintenance
- **Result:** Network instability, failed block sync

### 4. **Message Loop Duplication**
- **Current:** Separate loops in client.rs and server.rs
- **Issue:** 
  - Ping handling in server (✅ works)
  - Pong handling missing in client (❌ fails)
  - Inconsistent message processing
- **Result:** Outbound connections timeout

## Proposed Architecture

### Core Principle: **IP-Based Peer Identity**

```
Peer Identity = IP Address ONLY
├── Static Listening Port: 24100 (for accepting connections)
└── Active Connection
    ├── Socket (bidirectional I/O)
    ├── Remote IP
    └── Remote Ephemeral Port (for this connection only)
```

### New Structure

```
PeerConnection (replaces client + server)
├── peer_ip: String (identity)
├── socket: TcpStream (bidirectional)
├── local_port: u16 (24100)
├── remote_port: u16 (ephemeral)
├── direction: ConnectionDirection (Inbound/Outbound)
├── reader: OwnedReadHalf
├── writer: OwnedWriteHalf
└── message_handler: unified loop
```

## Refactor Steps

### Phase 1: Unified Connection Management (Week 1)

#### Step 1.1: Create New `peer_connection.rs`
**File:** `src/network/peer_connection.rs`

```rust
pub struct PeerConnection {
    peer_ip: String,
    socket: Arc<TcpStream>,
    direction: ConnectionDirection,
    reader: OwnedReadHalf,
    writer: Arc<Mutex<BufWriter<OwnedWriteHalf>>>,
    ping_state: Arc<RwLock<PingState>>,
}

pub enum ConnectionDirection {
    Inbound,   // They connected to us
    Outbound,  // We connected to them
}

impl PeerConnection {
    pub async fn new_outbound(peer_ip: String) -> Result<Self>
    pub async fn new_inbound(socket: TcpStream) -> Result<Self>
    pub async fn run_message_loop(self, ctx: NetworkContext)
}
```

#### Step 1.2: Update `ConnectionManager`
- Change from `HashSet<String>` (IP:PORT) to `HashSet<String>` (IP only)
- Add `active_connections: HashMap<String, Arc<PeerConnection>>`
- Track single connection per IP

#### Step 1.3: Update `PeerConnectionRegistry`
- Store by IP only: `HashMap<String, PeerWriter>`
- Add `get_connection_port()` method
- Remove port from identity

### Phase 2: Unified Message Loop (Week 1-2)

#### Step 2.1: Merge Message Handlers
**Location:** `peer_connection.rs::run_message_loop()`

```rust
async fn run_message_loop(&mut self, ctx: NetworkContext) {
    loop {
        tokio::select! {
            // Receive messages
            result = self.reader.read_line(&mut buffer) => {
                match self.parse_message(&buffer) {
                    NetworkMessage::Ping(nonce) => self.handle_ping(nonce).await,
                    NetworkMessage::Pong(nonce) => self.handle_pong(nonce).await,
                    // ... other messages
                }
            }
            
            // Send periodic pings
            _ = self.ping_interval.tick() => {
                self.send_ping().await?;
            }
            
            // Check for timeout
            _ = self.timeout_check.tick() => {
                if self.should_disconnect() {
                    break;
                }
            }
        }
    }
}
```

#### Step 2.2: Unified Ping/Pong
- Single ping sender (both directions)
- Single pong responder (both directions)
- Shared timeout tracking

### Phase 3: Remove Duplicate Code (Week 2)

#### Step 3.1: Delete `server.rs`
- Move `accept_connections()` to `peer_connection.rs`
- Convert to `PeerConnection::new_inbound()`

#### Step 3.2: Refactor `client.rs`
- Rename to `peer_connector.rs` (if needed)
- Convert to `PeerConnection::new_outbound()`
- Remove message loop (now in `peer_connection.rs`)

#### Step 3.3: Update `mod.rs`
```rust
pub mod peer_connection;      // NEW: unified connection
pub mod connection_manager;   // Updated
pub mod peer_discovery;       // No change
pub mod message;             // No change
```

### Phase 4: Connection Stability (Week 2)

#### Step 4.1: Remove Connection Cycling
- **Remove**: Deterministic connection direction logic
- **Keep**: Single connection per IP (first wins)
- **Add**: Connection quality metrics (prefer better connection)

#### Step 4.2: Persistent Connections
```rust
impl PeerConnection {
    const MAX_MISSED_PONGS: u32 = 5;  // Increase tolerance
    const PING_INTERVAL: Duration = Duration::from_secs(30);  // Less frequent
    
    async fn maintain_connection(&mut self) {
        // Only disconnect on actual failure, not arbitrary rules
    }
}
```

#### Step 4.3: Fix Heartbeat Logic
- Heartbeat should NOT trigger reconnection
- Port changes should NOT close connections
- Only disconnect on: timeout, error, explicit close

### Phase 5: Block Sync Fix (Week 2-3)

#### Step 5.1: Investigate Genesis Block
- Check if height=0 nodes have different genesis
- Verify block hash consistency
- Add genesis validation logging

#### Step 5.2: Improve Sync Logic
```rust
// Even 1 connection should trigger sync
if connected_peers >= 1 && height < network_height {
    start_block_sync().await;
}
```

#### Step 5.3: Add Sync Diagnostics
- Log why sync isn't starting
- Log block request/response details
- Track sync progress

## Implementation Order

### Week 1
- [ ] Create `peer_connection.rs` with basic structure
- [ ] Update `ConnectionManager` to use IP-only identity
- [ ] Implement unified message loop
- [ ] Test with 2 nodes

### Week 2  
- [ ] Delete `server.rs`, refactor `client.rs`
- [ ] Remove connection cycling logic
- [ ] Implement persistent connections
- [ ] Test with 6 nodes

### Week 3
- [ ] Fix block sync issues
- [ ] Add comprehensive diagnostics
- [ ] Performance testing
- [ ] Documentation

## Success Criteria

### Must Have
- ✅ Connections stay open indefinitely (no cycling)
- ✅ Each IP appears once in peer registry
- ✅ Ping/pong works in both directions
- ✅ Block sync succeeds with ≥1 peer
- ✅ All nodes reach same height

### Nice to Have
- ✅ Reduced code complexity (remove ~500 lines)
- ✅ Better diagnostics/logging
- ✅ Connection quality metrics
- ✅ Automatic recovery from network issues

## Testing Strategy

### Unit Tests
```rust
#[tokio::test]
async fn test_single_connection_per_ip()
#[tokio::test]
async fn test_ping_pong_both_directions()
#[tokio::test]
async fn test_connection_persistence()
```

### Integration Tests
- 6-node network
- Simulate node failures
- Test block propagation
- Measure connection stability

## Rollback Plan

If refactor fails:
1. Revert to last stable commit
2. Apply minimal fixes:
   - Fix pong handling in client
   - Disable connection cycling
   - Log genesis block hashes

## Notes

- Backup current code before starting
- Test each phase independently
- Keep old files until refactor complete
- Document all breaking changes

## Questions to Resolve

1. Should we keep connection direction enum or treat all connections equally?
2. How to handle simultaneous connections (both peers try to connect)?
3. What's the policy for connection quality (latency, reliability)?
4. Should genesis block validation be strict or lenient?

---

**Next Steps:**
1. Review and approve this plan
2. Create feature branch: `feature/p2p-refactor`
3. Begin Phase 1: Step 1.1
