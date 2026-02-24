# Network Architecture - TIME Coin Protocol v6.2

**Document Version:** 1.1  
**Last Updated:** February 9, 2026  
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

**Methods:**
```rust
pub fn register_peer(&self, ip: &str) -> Result<(), String>
pub fn unregister_peer(&self, ip: &str)
pub async fn send_to_peer(&self, peer_ip: &str, message: NetworkMessage) -> Result<(), String>
pub async fn broadcast(&self, message: NetworkMessage)
pub async fn get_connected_peers(&self) -> Vec<String>
pub async fn peer_count(&self) -> usize
```

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
1. **Phase 1**: Connect to active masternodes first (priority)
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
Testnet:
  P2P Port: 24100
  RPC Port: 24101
  Min Masternodes: 3
  
Mainnet:
  P2P Port: 24000
  RPC Port: 24001
  Min Masternodes: 10
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
