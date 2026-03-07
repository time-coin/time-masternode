# Network Architecture - TIME Coin Protocol v6.2

**Document Version:** 1.2  
**Last Updated:** March 7, 2026  
**Status:** Production-Ready

---

## Overview

The TIME Coin network layer implements a production-ready P2P system with:
- Lock-free concurrent peer management (DashMap)
- Secure TLS encryption
- Message signing and verification
- Rate limiting and DOS protection
- IP blacklisting
- Peer discovery and bootstrap
- State synchronization

---

## Network Module Organization

### Core Modules

#### `connection_manager.rs` ⭐ NEW
**Purpose:** Lock-free peer connection lifecycle management

**Key Features:**
- Synchronous API (no async overhead)
- DashMap-based concurrent state tracking
- O(1) connection lookups
- Atomic peer counters
- States: Disconnected, Connecting, Connected, Reconnecting

**Methods:**
```rust
pub fn is_connected(&self, peer_ip: &str) -> bool
pub fn mark_connecting(&self, peer_ip: &str) -> bool
pub fn mark_connected(&self, peer_ip: &str) -> bool
pub fn is_reconnecting(&self, peer_ip: &str) -> bool
pub fn mark_reconnecting(&self, peer_ip: &str, retry_delay, failures)
pub fn clear_reconnecting(&self, peer_ip: &str)
pub fn connected_count(&self) -> usize
pub fn get_connected_peers(&self) -> Vec<String>
```

**Performance:**
- Connection check: O(1)
- No lock contention (lock-free)
- Suitable for 1000+ concurrent peers

---

#### `peer_discovery.rs` ⭐ NEW
**Purpose:** Bootstrap peer service for network discovery

**Current Implementation:**
- Returns configured bootstrap peers from `time.conf` (addnode entries)
- Ready for HTTP-based peer discovery service

**Methods:**
```rust
pub fn new(discovery_url: String) -> Self
pub async fn fetch_peers_with_fallback(
    &self, 
    fallback_peers: Vec<String>
) -> Vec<DiscoveredPeer>
```

**Future Enhancement:**
```
HTTP GET https://api.time-coin.io/peers
Response: List of active peer addresses
Fallback: Use configured bootstrap peers if service unavailable
```

---

#### `peer_connection.rs`
**Purpose:** Individual peer connection handler

**Key Components:**
- `PeerConnection`: Handles inbound/outbound TCP connections
- `PeerStateManager`: Tracks peer connection states
- Health monitoring (ping/pong)
- Graceful connection closure

**State Transitions:**
```
Disconnected → Connecting → Connected → Reconnecting → Disconnected
```

---

#### `peer_connection_registry.rs`
**Purpose:** Registry of active peer connections with messaging

**Key Features:**
- Track all connected peers
- Register/unregister peers
- Send messages to peers
- Broadcast to multiple peers
- Gossip protocol support
- Per-peer load tracking via `peer_load` DashMap (used for PeerExchange)

**Methods:**
```rust
pub fn register_peer(&self, ip: &str) -> Result<(), String>
pub fn unregister_peer(&self, ip: &str)
pub async fn send_to_peer(&self, peer_ip: &str, message: NetworkMessage) -> Result<(), String>
pub async fn broadcast(&self, message: NetworkMessage)
pub async fn get_connected_peers(&self) -> Vec<String>  // post-handshake only (see §Connection States)
pub async fn peer_count(&self) -> usize
```

**Connection States and `get_connected_peers()` Behavior:**

Peers progress through states: `Connecting → Connected`. `get_connected_peers()` cross-references the `peer_writers` DashMap — only IPs with a live, non-closed writer channel are returned. Peers that are still in TCP-handshake (`Connecting` state) are intentionally excluded. This prevents the AI peer selector and sync logic from targeting not-yet-connected peers.

---

#### `ai/adaptive_reconnection.rs` — Peer Eviction

**Exponential backoff and permanent eviction:**

| Consecutive Failures | Cooldown Before Retry |
|---------------------|-----------------------|
| 3 | 10 minutes |
| 5 | 40 minutes |
| 7 | 2.7 hours |
| 9+ | 24 hours (max) |

After **10 consecutive failures** a peer is **permanently evicted** — removed from the sled `peer_manager` database and its AI profile is deleted. Evicted peers can re-enter the peer set only via a new `PeerExchange` response from another peer (i.e., must be re-advertised by the network).

Phase 3-MN (the dedicated masternode reconnect loop) has been removed; masternodes connect outbound themselves on daemon startup.

---

#### `client.rs`
**Purpose:** Network client for outbound peer connections

**Responsibilities:**
- Initiate connections to peers
- Prioritize masternode connections
- Implement exponential backoff
- Handle connection recovery
- Peer discovery integration

**Two-Phase Connection Strategy:**
1. **Phase 1**: Detect total masternode count.
   - If total ≤ `FULL_MESH_THRESHOLD` (50): connect to **all** other masternodes regardless of tier (full-mesh mode — ensures testnet and small networks see each other for gossip, voting, and rewards).
   - If total > 50: connect to upstream tiers only (pyramid mode — Gold/Silver/Bronze/Free hierarchy).
2. **Phase 2**: Connect to regular peers (best-effort)

---

#### `server.rs`
**Purpose:** Network server for inbound peer connections

**Responsibilities:**
- Accept incoming connections
- Handle peer authentication
- Route incoming messages
- Manage peer subscriptions
- Rate limiting per peer

**Security Features:**
- IP blacklisting
- Rate limiting (token bucket)
- Message size validation
- Handshake validation

---

### Security Modules

#### `tls.rs`
**Purpose:** TLS encryption for P2P communication

**Features:**
- Self-signed certificates (P2P)
- TLS 1.3 support
- Certificate pinning ready
- Session resumption

**Implementation:**
```rust
pub fn new_self_signed() -> Result<Self, TlsError>
pub fn from_pem_files(cert_path, key_path) -> Result<Self, TlsError>
```

---

#### `signed_message.rs`
**Purpose:** Ed25519 message signing and verification

**Features:**
- Cryptographic message authentication
- Timestamp validation (prevent replay attacks)
- Sender identity verification
- Signature validation

**Implementation:**
```rust
pub fn new(payload, signing_key, timestamp) -> Result<SignedMessage>
pub fn verify(&self) -> Result<(), SignedMessageError>
pub fn is_timestamp_valid(&self, max_age_seconds) -> bool
```

---

#### `secure_transport.rs`
**Purpose:** Combined TLS + signing transport layer

**Status:** Consolidated into client/server modules (legacy)

---

### Utility Modules

#### `rate_limiter.rs`
**Purpose:** Token bucket rate limiting per peer

**Features:**
- Per-peer rate limits
- Token bucket algorithm
- Configurable rates
- Adaptive limits based on masternode count

**Implementation:**
```rust
pub fn check_limit(&mut self, peer_ip: &str, tokens: u32) -> bool
pub fn reset_limit(&mut self, peer_ip: &str)
```

---

#### `blacklist.rs`
**Purpose:** IP blacklisting with TTL expiration

**Features:**
- Temporary blacklist entries
- Automatic cleanup (TTL-based)
- Batch operations
- Thread-safe operations

**Implementation:**
```rust
pub fn add(&mut self, ip: &str, ttl: Duration)
pub fn is_blacklisted(&self, ip: &str) -> bool
pub fn remove(&mut self, ip: &str)
pub fn cleanup_expired()
```

---

#### `dedup_filter.rs`
**Purpose:** Message deduplication with Bloom filter

**Features:**
- Bloom filter for O(1) lookups
- Automatic rotation (TTL-based)
- Prevents duplicate message propagation
- Low false-positive rate

**Implementation:**
```rust
pub fn insert(&self, item: &[u8])
pub fn contains(&self, item: &[u8]) -> bool
pub fn rotate_if_expired()
```

---

#### `message.rs`
**Purpose:** Network message types and serialization

**Message Categories:**
- **Consensus**: Voting, block proposals
- **Sync**: Block/UTXO requests
- **Peer**: Discovery, heartbeat, handshake
- **Data**: Transaction, block broadcasting

**PeerExchange (Updated):**

`GetPeers` responses now carry rich per-peer metadata instead of bare IP strings:

```rust
pub struct PeerExchangeEntry {
    pub address: String,
    pub connection_count: u32,   // current inbound load
    pub is_masternode: bool,
    pub tier: Option<MasternodeTier>,
}
```

Entries are sorted by tier (Gold first) then ascending `connection_count` so connecting nodes naturally prefer underloaded peers. A node whose inbound count exceeds **70 % of `MAX_INBOUND` (100)** rejects new inbounds and redirects the connecting peer with an alternative `PeerExchangeEntry` list.

---

#### `state_sync.rs`
**Purpose:** Blockchain state synchronization

**Features:**
- Block synchronization
- UTXO set synchronization
- Catch-up mechanisms
- Progressive synchronization

---

## Architecture Diagrams

### Peer Connection Lifecycle

```
┌─────────────────────────────────────────────────────────────┐
│                    Network Layer                            │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────────────┐              ┌──────────────────┐   │
│  │  Network Client  │◄────────────►│  Network Server  │   │
│  │  (Outbound)      │              │  (Inbound)       │   │
│  └────────┬─────────┘              └────────┬─────────┘   │
│           │                                 │               │
│           ├────────────────────────────────┤               │
│           │                                │               │
│           ▼                                ▼               │
│  ┌────────────────────────────────────────────────────┐   │
│  │  PeerConnectionRegistry (Central Registry)        │   │
│  │  - Register/Unregister peers                      │   │
│  │  - Send messages                                  │   │
│  │  - Broadcast/Gossip                               │   │
│  │  - Track connection metrics                       │   │
│  └────────┬─────────────────────────────────────────┘   │
│           │                                              │
│           ▼                                              │
│  ┌────────────────────────────────────────────────────┐   │
│  │  ConnectionManager (Lock-free)                     │   │
│  │  - Track connection states (DashMap)              │   │
│  │  - O(1) lookups                                    │   │
│  │  - Atomic counters                                │   │
│  │  - Thread-safe operations                          │   │
│  └────────────────────────────────────────────────────┘   │
│                                                             │
│  ┌──────────────────────────────────────────────────┐     │
│  │  Security & Filtering                            │     │
│  │  - TLS encryption  - Rate limiting               │     │
│  │  - Message signing - Blacklist                   │     │
│  │  - Deduplication                                 │     │
│  └──────────────────────────────────────────────────┘     │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### Message Flow

```
Consensus Layer (TimeVote + TimeLock)
        │
        ▼
┌──────────────────────────┐
│  Message Generation      │
│  (NetworkMessage enum)   │
└──────────┬───────────────┘
           │
           ▼
┌──────────────────────────────────┐
│  Message Signing & Encryption    │
│  - Ed25519 signature             │
│  - TLS encryption                │
│  - Timestamp validation           │
└──────────┬───────────────────────┘
           │
           ▼
┌──────────────────────────────────┐
│  Rate Limiting & Blacklist       │
│  - Token bucket per peer         │
│  - Verify not blacklisted        │
└──────────┬───────────────────────┘
           │
           ▼
┌──────────────────────────────────┐
│  Network Transport               │
│  - Send via TCP (TLS encrypted)  │
│  - Gossip to selected peers      │
│  - Broadcast to all connected    │
└──────────┬───────────────────────┘
           │
           ▼
┌──────────────────────────────────┐
│  Deduplication Check             │
│  - Bloom filter (recipient)      │
│  - Mark as seen                  │
└──────────┬───────────────────────┘
           │
           ▼
Application Layer (Consensus Engine)
```

---

## Performance Characteristics

### Connection Management
- **Lookup Latency**: O(1) with DashMap
- **Concurrent Peers**: Support 1000+ peers
- **Lock Contention**: Zero (lock-free)
- **Memory Per Peer**: ~200 bytes

### Message Handling
- **Serialization**: Bincode (fast binary format)
- **Rate Limiting**: O(1) per peer
- **Deduplication**: O(1) with Bloom filter
- **Throughput**: >10k messages/sec per connection

### Network Bandwidth
- **Message Size**: 0.5-1.0 KB typical
- **Broadcast**: N × message_size
- **Gossip**: K × message_size (K = fan-out)

---

## Configuration

### Network Settings (time.conf)

```ini
# Accept incoming connections
listen=1

# Your public IP (for NAT/firewalls)
#externalip=1.2.3.4

# Maximum peer connections
#maxconnections=50

# Add seed nodes
#addnode=seed1.time-coin.io
enable_message_signing = true
message_max_age_seconds = 300  # 5 minutes
enable_rate_limiting = true
max_requests_per_second = 1000
```

### Connection Manager

```toml
# Implicit settings (hardcoded):
# - State: Disconnected, Connecting, Connected, Reconnecting
# - Reconnect backoff: 1s, 2s, 4s, 8s, 16s, 32s (exponential)
# - Max peers: Configurable via max_peers
# - Reserved masternode slots: 40% of max_peers
```

---

## Production Deployment

### Network Topology

```
Small Network (≤ 50 masternodes) — Full Mesh:
  Every masternode ←→ every other masternode
  Guarantees universal gossip, vote, and reward-eligibility visibility

Large Network (> 50 masternodes) — Pyramid:
  Gold ←→ Gold          (upstream tier, fully connected)
  Gold ←→ Silver        (cross-tier upstream connections)
  Silver ←→ Bronze      (cross-tier upstream connections)
  Bronze ←→ Free        (downstream tier connects up)
  Free → Bronze/Silver  (outbound-only to upstream)
```

**Testnet:** Uses full-mesh automatically (nearly always ≤ 50 nodes).  
**Mainnet:** Expected to exceed the threshold; pyramid topology applies.

The topology is evaluated per connection cycle — as the network grows past 50 masternodes it transitions from full-mesh to pyramid without operator intervention.

```
P2P Ports:
  Testnet: 24100
  Mainnet: 24000

RPC Ports:
  Testnet: 24101
  Mainnet: 24001

Min Masternodes: 3 (quorum)
MAX_INBOUND:     100
```

### Recommended Configuration

**Small Network (10-50 nodes):**
```toml
max_peers = 50
bootstrap_peers = ["seed1.time-coin.io:24100", "seed2.time-coin.io:24100"]
```

**Large Network (100+ nodes):**
```toml
max_peers = 100
enable_peer_discovery = true
bootstrap_peers = ["seed1.time-coin.io:24100", "seed2.time-coin.io:24100"]
```

---

## Consolidation Status

### ✅ Completed
- Network directory modules consolidated
- Connection management unified
- Lock-free data structures implemented
- Peer discovery service created
- Security module organization
- TLS and signing separation of concerns

### ✅ Testing
- Unit tests for connection manager
- Integration tests for peer registry
- Message signing verification tests
- Rate limiting threshold tests

### 🔄 Future Enhancements
- HTTP-based peer discovery API
- WebSocket support for wallets
- DNS seed integration
- UPnP/NAT traversal improvements
- Performance monitoring metrics

---

## References

- **Protocol**: [TIME Coin Protocol v5](../docs/TIMECOIN_PROTOCOL_V5.md)
- **Build**: [Compilation Status](../COMPILATION_COMPLETE.md)
- **Analysis**: See `analysis/` directory for detailed studies

---

*For implementation details, see source code comments in `src/network/`*

---

## Network Configuration Reference

### Network Summary

| Network | P2P Port | RPC Port | Address Prefix | Magic Bytes |
|---------|----------|----------|----------------|-------------|
| **Mainnet** | 24000 | 24001 | time1 | 0xC01D7E4D ("COLD TIME") |
| **Testnet** | 24100 | 24101 | time1 | 0x7E577E4D ("TEST TIME") |

### Configuration Files

- `time.conf` — Daemon configuration (key=value format, Dash-style)
- `masternode.conf` — Collateral entries (one per line)

Both files go in the data directory:
- **Mainnet:** `~/.timecoin/`
- **Testnet:** `~/.timecoin/testnet/`

### Network Type

The network is set in `time.conf`:

```ini
# Testnet
testnet=1

# Mainnet (default when testnet is not set)
#testnet=0
```

### Port Overrides

Ports are automatically selected based on network type. Override in `time.conf` if needed:

```ini
# Override P2P port (default: mainnet=24000, testnet=24100)
#port=24100

# Override RPC port (default: mainnet=24001, testnet=24101)
#rpcport=24101
```

### Address Prefixes

TIME Coin addresses use the `time1` prefix for both networks:

- **Mainnet**: `time1abc...`
- **Testnet**: `time1xyz...`

Both networks use the same address format, but transactions are network-isolated through magic bytes. Nodes on different networks will reject each other's messages.

```rust
NetworkType::Mainnet.magic_bytes() // [0xC0, 0x1D, 0x7E, 0x4D]
NetworkType::Testnet.magic_bytes() // [0x7E, 0x57, 0x7E, 0x4D]
```

### Running Different Networks

**Testnet (Default)**:

```bash
./target/release/timed
# Or explicitly:
./target/release/timed --conf ~/.timecoin/testnet/time.conf
```

Output will show:
```
📡 Network: Testnet
  └─ Magic Bytes: [126, 87, 126, 77]
  └─ Address Prefix: time1
```

**Mainnet**:

```bash
./target/release/timed --conf ~/.timecoin/time.conf
```

Output will show:
```
📡 Network: Mainnet
  └─ Magic Bytes: [192, 29, 126, 77]
  └─ Address Prefix: time1
```

### Masternode Configuration

**Free Tier** — in `time.conf`:
```ini
masternode=1
```
No `masternode.conf` entry needed (Free tier requires no collateral).

**Staked Tier (Bronze/Silver/Gold)** — in `time.conf`:
```ini
masternode=1
masternodeprivkey=<key from time-cli masternode genkey>
```

In `masternode.conf`:
```
mn1 <your_ip>:24000 <collateral_txid> <collateral_vout>
```

Tier is auto-detected from the collateral UTXO value.

### Reward Weights

| Tier | Collateral | Reward Weight | Can Vote |
|------|------------|---------------|----------|
| Free | 0 TIME | 1 | ❌ No |
| Bronze | 1,000 TIME | 1,000 | ✅ Yes (1x) |
| Silver | 10,000 TIME | 10,000 | ✅ Yes (10x) |
| Gold | 100,000 TIME | 100,000 | ✅ Yes (100x) |

### Peer Discovery Configuration

```toml
[network]
enable_peer_discovery = true
bootstrap_peers = [
    "seed1.time-coin.io:24100",  # Testnet
    "seed2.time-coin.io:24100",
]
```

For mainnet, use port 24000:

```toml
bootstrap_peers = [
    "seed1.time-coin.io:24000",
    "seed2.time-coin.io:24000",
]
```

### Storage

Data directories are network-specific to prevent blockchain data from being mixed between networks:

```toml
[storage]
data_dir = "./data/testnet"  # Testnet
# OR
data_dir = "./data/mainnet"  # Mainnet
```

### Network Configuration Security Notes

- **Never** mix mainnet and testnet — testnet coins have no value and magic bytes prevent cross-network communication.
- **Always** verify the network before sending transactions: check the address prefix, verify the RPC port matches the network, and check daemon output for network type.

### Network Configuration Troubleshooting

**Wrong network connected**  
*Error*: Peers rejecting connections  
*Solution*: Check magic bytes in daemon output match your intended network.

**Port already in use**  
*Error*: `Failed to start network: Address already in use`  
*Solution*: Stop the other node using that port, change to a different port in config, or switch networks (testnet vs mainnet use different ports).

**Address prefix mismatch**  
*Error*: Invalid address format  
*Solution*: Verify address starts with `time1` on both mainnet and testnet.

### Network Configuration Best Practices

1. **Development**: Always use testnet
2. **Testing**: Use free tier masternode on testnet
3. **Production**: Use mainnet with appropriate collateral
4. **Separate Data**: Keep testnet and mainnet data directories separate
5. **Verify Network**: Always check network type before transactions

---

## Integration Guide

**Goal**: Add message authentication and TLS encryption to the TIME Coin P2P network  
**Time Required**: 4–7 days  
**Complexity**: Medium

### Prerequisites

The following are already complete and ready to integrate:

- `signed_message.rs` and `tls.rs` written and tested
- Dependencies added (`blake3`, `zeroize`, `rustls`, etc.)
- Compiles without errors

### Step 1: Add Node Identity Key (30 minutes)

**File**: `src/main.rs`

```rust
use ed25519_dalek::SigningKey;
use crate::network::signed_message::SecureSigningKey;
use rand::rngs::OsRng;

// In main() or node startup:
let mut csprng = OsRng;
let signing_key = SigningKey::generate(&mut csprng);
let node_key = Arc::new(SecureSigningKey::new(signing_key));

tracing::info!("Node public key: {}", hex::encode(node_key.verifying_key().to_bytes()));
```

### Step 2: Sign Outgoing Messages (1 hour)

**File**: `src/network/client.rs` and `src/network/server.rs`

```rust
use crate::network::signed_message::SignedMessage;

// Before sending any message:
let timestamp = chrono::Utc::now().timestamp();
let signed_msg = SignedMessage::new(message, node_key.signing_key(), timestamp)?;
let bytes = bincode::serialize(&signed_msg)?;
writer.write_all(&bytes).await?;
```

### Step 3: Verify Incoming Messages (1 hour)

**File**: `src/network/client.rs` and `src/network/server.rs`

```rust
// After receiving message bytes:
let signed_msg: SignedMessage = bincode::deserialize(&bytes)?;

// Verify signature
signed_msg.verify()?;

// Check timestamp (reject messages older than 60 seconds)
if !signed_msg.is_timestamp_valid(60) {
    return Err("Message too old".into());
}

// Extract the actual message
let message = signed_msg.payload;
```

### Step 4: Initialize TLS (1 hour)

**File**: `src/main.rs`

```rust
use crate::network::tls::TlsConfig;

// At startup, create TLS config once:
let tls_config = if let (Some(cert), Some(key)) = 
    (&config.tls_cert_path, &config.tls_key_path) {
    // Production: Load from files
    Arc::new(TlsConfig::from_pem_files(cert, key)?)
} else {
    // Development: Use self-signed
    Arc::new(TlsConfig::new_self_signed()?)
};

tracing::info!("TLS initialized");
```

### Step 5: Wrap Client Connections with TLS (2 hours)

**File**: `src/network/client.rs`

```rust
// In maintain_peer_connection() or connect logic:

// OLD:
let stream = TcpStream::connect(&peer_addr).await?;
let mut reader = BufReader::new(stream.clone());
let mut writer = BufWriter::new(stream);

// NEW:
let tcp_stream = TcpStream::connect(&peer_addr).await?;
let tls_stream = tls_config.connect_client(tcp_stream, "peer").await?;

// Split the stream for reading and writing
let (read_half, write_half) = tokio::io::split(tls_stream);
let mut reader = BufReader::new(read_half);
let mut writer = BufWriter::new(write_half);
```

### Step 6: Wrap Server Connections with TLS (2 hours)

**File**: `src/network/server.rs`

```rust
// In run() or accept loop:

// OLD:
let (stream, addr) = self.listener.accept().await?;
let mut reader = BufReader::new(stream.clone());
let mut writer = BufWriter::new(stream);

// NEW:
let (tcp_stream, addr) = self.listener.accept().await?;
let tls_stream = tls_config.accept_server(tcp_stream).await?;

let (read_half, write_half) = tokio::io::split(tls_stream);
let mut reader = BufReader::new(read_half);
let mut writer = BufWriter::new(write_half);
```

### Step 7: Update Configuration (30 minutes)

**File**: `time.conf`

```ini
# Security settings are built-in defaults (no config needed for standard deployment)
# TLS is automatically enabled for P2P connections
accept_plain_connections = false        # Allow non-TLS during transition?
```

**File**: `src/config.rs`

```rust
#[derive(Deserialize)]
pub struct NetworkConfig {
    // ... existing fields ...

    #[serde(default)]
    pub require_signed_messages: bool,
    #[serde(default)]
    pub tls_enabled: bool,
    pub tls_cert_path: Option<PathBuf>,
    pub tls_key_path: Option<PathBuf>,
    #[serde(default = "default_max_message_age")]
    pub max_message_age_seconds: i64,
}

fn default_max_message_age() -> i64 { 60 }
```

### Step 8: Test Everything (1–2 days)

**Unit Tests**:
```bash
cargo test signed_message
cargo test tls
```

**Integration Tests**:
1. Start two nodes
2. Verify they connect with TLS
3. Send transactions
4. Verify signatures are checked
5. Test with invalid signature (should be rejected)
6. Test with old timestamp (should be rejected)

**Performance Tests**:
```bash
# Benchmark signature verification speed
cargo bench

# Monitor CPU usage with security enabled
htop

# Check latency impact
ping peer_node
```

### Integration Troubleshooting

**"TLS handshake failed"**  
*Cause*: Clock skew or certificate issues  
*Fix*:
```bash
# Check time sync
timedatectl status

# Regenerate self-signed cert
rm -rf ~/.timecoin/tls/
# Will auto-regenerate on next start
```

**"Signature verification failed"**  
*Cause*: Wrong key or message tampering  
*Fix*:
```rust
// Add debug logging:
tracing::debug!("Sender pubkey: {}", hex::encode(signed_msg.sender_pubkey_bytes()));
tracing::debug!("Expected pubkey: {}", hex::encode(expected_key.to_bytes()));
```

**"Message too old"**  
*Cause*: Clock drift between nodes  
*Fix*:
```bash
# Install NTP
sudo apt install ntp
sudo systemctl enable ntp
sudo systemctl start ntp

# Or increase tolerance in config
max_message_age_seconds = 300  # 5 minutes
```

**"Connection refused" after TLS**  
*Cause*: Peer doesn't have TLS enabled yet  
*Fix*: Enable gradual rollout:
```toml
accept_plain_connections = true  # Temporarily allow non-TLS
```

### Verification Checklist

- [ ] Code compiles without errors
- [ ] Node generates and logs public key at startup
- [ ] Outgoing messages are signed
- [ ] Incoming messages are verified
- [ ] Invalid signatures are rejected
- [ ] Old messages are rejected
- [ ] TLS handshake succeeds
- [ ] Traffic is encrypted (verify with Wireshark)
- [ ] Performance is acceptable (<5% CPU increase)
- [ ] Logs show security events (signatures, TLS)

### Success Criteria

After integration, you should see:

```
[INFO] Node public key: a3f8e2... (64 hex characters)
[INFO] TLS initialized
[INFO] ✓ Connected to peer: 50.28.104.50 (TLS enabled)
[DEBUG] Message signature verified from: b4c9d1...
[DEBUG] Message timestamp valid: 1702345678
```

And you should **not** see:
```
[ERROR] Signature verification failed  ❌ (unless peer misbehaving)
[ERROR] TLS handshake failed          ❌ (unless peer down)
[WARN] Message too old, rejecting     ⚠️  (occasional is OK)
```

### Rollout Strategy

**Phase 1 — Testnet (Week 1)**:
- Deploy to 2–3 test nodes
- Monitor for issues
- Performance benchmarking

**Phase 2 — Partial Rollout (Week 2)**:
- Deploy to 50% of masternodes
- Keep `accept_unsigned_messages = true`
- Monitor mixed-mode operation

**Phase 3 — Full Enforcement (Week 3)**:
- Deploy to all masternodes
- Set `require_signed_messages = true`
- Set `accept_plain_connections = false`
- Full security enforcement

### Emergency Rollback

If something goes wrong:

```bash
# Quick rollback:
1. Stop the node: systemctl stop timed
2. Check time.conf settings
3. Restart: systemctl start timed
```

Logs to check:
```bash
journalctl -u timed -n 100 --no-pager | grep -i "error\|tls\|signature"
```
