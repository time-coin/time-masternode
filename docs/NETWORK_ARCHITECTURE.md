# Network Architecture - TIME Coin Protocol v6.2

**Document Version:** 1.5
**Last Updated:** April 28, 2026
**Status:** Production-Ready

---

## Recent Changes (April 28, 2026)

### ConnectionManager as Single Authority for Inbound + Outbound

`ConnectionManager` now tracks **all** connections regardless of direction. Previously only outbound connections were registered (via `mark_connecting` / `mark_connected`), so `is_connected()` returned `false` for inbound sessions. This caused PHASE3 in `client.rs` to see an inbound peer as "not connected" and dial it again, creating simultaneous-connection collisions.

**New API:**
```rust
pub fn accept_inbound(&self, peer_ip: &str, is_whitelisted: bool) -> bool
```
- Called at the top of `server.rs::handle_peer()`, before any message work.
- Atomically registers the inbound session; returns `false` (drop the connection) if the peer is already in any state (Connecting/Connected/Reconnecting) or inbound capacity is full.
- Increments `inbound_count` and calls `record_new_connection()` on success.

**PHASE3 change:** `client.rs` PHASE3 now checks `connection_manager.is_connected(mn_ip)` (both directions) instead of `peer_registry.is_connected(mn_ip)` (outbound-only). All connection-count and subnet-count (AV25) checks in `client.rs` were migrated to use only `ConnectionManager`.

**Role separation:**
- `ConnectionManager` — authoritative connection state (Disconnected / Connecting / Connected / Reconnecting), capacity enforcement, direction tracking.
- `PeerConnectionRegistry` — writer-channel router (peer_ip → PeerWriterTx), broadcast/gossip delivery. No longer used as a connection state source.

### Conflict-Only TimeVote Voting

Transactions that have no competing spender in the mempool are auto-finalized immediately instead of waiting for 67% stake-weighted vote accumulation. Only genuine double-spend conflicts trigger a full TimeVote round. This eliminates the root cause of transactions sitting in the mempool for hours when connectivity is degraded and vote collection stalls.

### Avalanche-Style Transaction Gossip

`TransactionBroadcast` is now relayed to all peers **before** local `process_transaction()` is called. Nodes that are still syncing relay the transaction but skip local processing. This ensures a transaction propagates through the full network even if some intermediate nodes are in a state that would cause them to reject it locally.

### Connection Collision Fix

`priority_reconnect_notify` now fires only when a disconnected session lasted ≥ 10 seconds. Collision-dropped connections (very short lifetime) exit quietly without triggering an immediate re-dial, preventing the reconnect storm where both sides perpetually dropped and re-created the same collision.

---

## Overview

The TIME Coin network layer implements a production-ready P2P system with:
- Lock-free concurrent peer management (DashMap)
- Secure TLS encryption
- Message signing and verification
- Rate limiting and DOS protection
- IP banning
- Peer discovery and bootstrap
- State synchronization
- Fork alert chain tip propagation (v1.2.2)

---

## Recent Changes (v1.5.5 - May 8, 2026)

### TLS Bridge Cancellation-Safety Fix (FrameBomb Root Cause — `e78e843`)

The `tokio::select!(read_message, write_rx.recv())` single-task TLS bridge introduced in commit `986435a` (May 5) was not cancellation-safe. `read_message` uses `read_exact` internally, which is explicitly documented as not cancellation-safe. When `select!` chose the write branch while `read_message` was mid-frame, already-consumed bytes were permanently lost, leaving the stream at a wrong offset. The next `read_message` interpreted mid-payload bytes as a 4-byte BE frame-length prefix — producing deterministic 100 MB–3 GB "FrameBomb" sizes across every TLS connection on the network. All v1.5.5 nodes were bombing each other symmetrically.

**Fix:** Both TLS bridges (`server.rs` inbound, `peer_connection.rs` outbound) now use `tokio::io::split()` with separate dedicated reader and writer tasks — identical to the non-TLS path that had zero FrameBombs. `tokio::io::split()` is safe because rustls uses TLS 1.3 exclusively (no renegotiation, no cross-direction I/O post-handshake). The `select!` bridge is removed entirely.

**History:** The `split()` approach was the original design (documented in v1.2.4 below). It was incorrectly reverted in `986435a` based on a TLS 1.2 renegotiation concern that does not apply to rustls. Today's commit restores the correct design with an updated explanation in the code comments.

### `is_syncing` Flag Stuck After Rollback (`45f8a77`)

`SyncGuard` (RAII wrapper that clears `is_syncing` on drop) was created *after* the early-exit check in `sync_from_peers`. Callers that set `is_syncing=true` before calling `sync_from_peers` — specifically rollback-to-genesis and fork-detection paths — would hit the early exit without the guard ever being created, leaving `is_syncing` permanently true. Affected nodes reported `initialblockdownload=true` indefinitely and were filtered out of wallet connection tabs. Fix: `SyncGuard` is now created before the early-exit check.

### FrameBomb Whitelist Bypass Fix (`banlist.rs`)

`IPBanlist.is_banned()` previously returned `None` (not banned) for whitelisted IPs before checking any ban maps, making FrameBomb bans completely ineffective for whitelisted peers. Fix: added `frame_bomb_bans: HashMap` checked **before** the whitelist early-return. Whitelisted peers receive shorter bans (2-min/15-min vs 5-min/1-hour) but are no longer exempt.

---

## Recent Changes (v1.2.4 - March 2026)

### TLS I/O Race Condition Fix
The inbound and outbound TLS paths previously used a single `tokio::select!` loop to interleave reads and writes on the same `TlsStream`. When both branches became ready simultaneously, Tokio would cancel the in-progress `read_message` future and process the write — silently discarding bytes already pulled from the TCP kernel buffer. The next `read_message` started at the wrong offset and interpreted mid-payload bytes as a frame length header, producing garbage multi-gigabyte values. This occurred at 30-second intervals (matching `PING_INTERVAL_SECS`) whenever an outbound ping coincided with incoming data.

**Fix:** Both TLS paths now use `tokio::io::split()` to obtain independent read and write halves, each run in a separate spawned task — identical to the existing non-TLS path. The shared-stream `select!` bridge is removed entirely.

### Block Size: Dual Constants
`MAX_BLOCK_SIZE` (validation, 4 MB) and `MAX_BLOCK_ASSEMBLY_SIZE` (producer cap, 1.9 MB) are now separate constants. The block producer truncates its transaction set at `MAX_BLOCK_ASSEMBLY_SIZE`; the validator accepts legacy blocks up to `MAX_BLOCK_SIZE`.

### Ping/Pong Soft Rate Limit
Excess pings are now dropped silently (`check_rate_limit_soft!`) rather than recording banlist violations. Previously, connection-churn during sync failures accumulated ping violations and triggered hour-long bans on legitimate masternodes.

### Reduced Ban Escalation (Non-Severe Violations)
- 3rd violation: 5 min → **1 min**
- 5th violation: 1 hr → **5 min**
- Severe violations (`record_severe_violation`): unchanged (1-hour ban, then permanent)

---

## Recent Changes (v1.2.2 - March 2026)

### Faster Peer Connection Timings
- **PEER_WAIT_SECS**: 15s → 5s — initial peer wait at startup
- **GENESIS_WAIT_SECS**: 20s → 10s — genesis block response wait
- **BASE_DISCOVERY_WAIT**: 30s → 10s — reduces exponential backoff from 30/60/90s to 10/20/30s
- **Peer exchange**: 60s → 30s — GetMasternodes broadcast interval
- **Health monitoring**: starts at 30s (was 120s), runs every 60s (was 120s)
- **PHASE 3 rediscovery**: 120s → 30s — periodic peer discovery loop

### Fork Alert Chain Tip Propagation
- `handle_fork_alert` now updates the alerting peer's chain tip in the `PeerConnectionRegistry` with the reported consensus height and hash
- This ensures `sync_from_peers` can discover peers at the consensus height even if the local chain tip cache is stale

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
pub fn is_connected(&self, peer_ip: &str) -> bool          // inbound OR outbound
pub fn accept_inbound(&self, peer_ip: &str, is_whitelisted: bool) -> bool  // register inbound; drop on conflict
pub fn mark_connecting(&self, peer_ip: &str) -> bool       // outbound dial initiated
pub fn mark_connected(&self, peer_ip: &str) -> bool        // outbound handshake complete
pub fn is_reconnecting(&self, peer_ip: &str) -> bool
pub fn mark_reconnecting(&self, peer_ip: &str, retry_delay, failures)
pub fn clear_reconnecting(&self, peer_ip: &str)
pub fn connected_count(&self) -> usize
pub fn inbound_count(&self) -> usize
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

**I/O Architecture:**
All connections (TLS and plain TCP) use `tokio::io::split()` to obtain independent read and write halves. Each half runs in its own spawned task, preventing write operations from cancelling in-progress reads. The reader task feeds a `mpsc::UnboundedSender<Result<Option<NetworkMessage>>>` channel; the writer task drains a `mpsc::UnboundedSender<Vec<u8>>` channel of pre-serialized frames.

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
| 5+ | Permanently evicted |

After **5 consecutive failures** a peer is **permanently evicted** — removed from the sled `peer_manager` database and its AI profile is deleted. The `PeerManager` records the evicted IP with a **1-hour cooldown**, during which `add_peer_candidate()` rejects the address even if re-advertised via PeerExchange gossip. After the cooldown expires, the peer may be re-added if another node gossips it.

The PHASE 3 eviction check runs **unconditionally** before AI reconnection advice is consulted — previously it only ran inside the AI cooldown branch, so peers whose cooldown had expired were never evicted regardless of failure count.

Additionally, `list_by_tier()` now filters for `is_active`, so PHASE 1 startup connections no longer dial inactive masternodes that will just timeout.

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
- IP banning
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
**Purpose:** Per-peer message rate limiting

**Features:**
- Per-peer, per-message-type rate limits
- Configurable windows and counts
- Emergency + periodic cleanup to bound memory usage

**Current limits (per peer):**

| Message type | Window | Max |
|---|---|---|
| `tx` | 1s | 50 |
| `block` | 1s | 10 |
| `vote` | 1s | 100 |
| `utxo_query` | 1s | 100 |
| `get_blocks` | 10s | 100 |
| `ping` | 10s | 6 |
| `pong` | 10s | 6 |
| `get_peers` | 60s | 5 |
| `masternode_announce` | 60s | 3 |
| `subscribe` | 60s | 10 |
| `general` | 1s | 100 |

> **Note:** `ping` and `pong` use a *soft* rate limit (`check_rate_limit_soft!`) — excess messages are dropped silently without recording a banlist violation. Pings burst during connection churn (e.g. sync retries) and should not penalise legitimate peers.

---

#### `banlist.rs`
**Purpose:** IP banning with TTL expiration

**Features:**
- Temporary and permanent banlist entries
- Whitelist (masternodes exempt from minor violations)
- Automatic cleanup (TTL-based)
- Thread-safe operations

**Violation escalation (normal violations):**

| Count | Action |
|-------|--------|
| 1–2 | Warning only |
| 3 | 1-minute temp ban |
| 5 | 5-minute temp ban |
| 10 | Permanent ban |

**Severe violations** (`record_severe_violation` — corrupted blocks, reorg attacks): immediate 1-hour ban; permanent ban if effective count reaches 10. Applies even to whitelisted peers.

**Ping/pong rate limit violations** do **not** go through this path — they use `check_rate_limit_soft!` and are dropped silently.

**Implementation:**
```rust
pub fn record_violation(&mut self, ip: IpAddr, reason: &str) -> bool
pub fn record_severe_violation(&mut self, ip: IpAddr, reason: &str) -> bool
pub fn add_temp_ban(&mut self, ip: IpAddr, duration: Duration, reason: &str)
pub fn is_banned(&mut self, ip: IpAddr) -> Option<String>
pub fn add_to_whitelist(&mut self, ip: IpAddr, reason: &str)
pub fn cleanup(&mut self)
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
│  │  - Message signing - Banlist                   │     │
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
│  Rate Limiting & Banlist       │
│  - Token bucket per peer         │
│  - Verify not banned        │
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

**Running both networks on the same host** is supported. The install script creates separate systemd units — `timed` (mainnet) and `timetd` (testnet) — with non-overlapping ports and data directories:

```bash
sudo ./scripts/install-masternode.sh mainnet   # → timed.service
sudo ./scripts/install-masternode.sh testnet   # → timetd.service
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


---

*For implementation details, see source code comments in `src/network/`*

---

## Network Configuration Reference

### Network Summary

| Network | P2P Port | RPC Port | Address Prefix | Magic Bytes |
|---------|----------|----------|----------------|-------------|
| **Mainnet** | 24000 | 24001 | TIME1 | 0xC01D7E4D ("COLD TIME") |
| **Testnet** | 24100 | 24101 | TIME0 | 0x54494D45 ("TIME" ASCII) |

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

TIME Coin addresses use distinct prefixes per network (38 chars total):

- **Mainnet**: `TIME1<payload>` (e.g. `TIME1abc...`)
- **Testnet**: `TIME0<payload>` (e.g. `TIME0abc...`)

Transactions are also network-isolated through magic bytes. Nodes on different networks will reject each other's messages.

```rust
NetworkType::Mainnet.magic_bytes() // [0xC0, 0x1D, 0x7E, 0x4D]  "COLD TIME"
NetworkType::Testnet.magic_bytes() // [0x54, 0x49, 0x4D, 0x45]  "TIME" ASCII
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
  └─ Magic Bytes: [84, 73, 77, 69]  ("TIME" ASCII)
  └─ Address Prefix: TIME0 (testnet) / TIME1 (mainnet)
```

**Mainnet**:

```bash
./target/release/timed --conf ~/.timecoin/time.conf
```

Output will show:
```
📡 Network: Mainnet
  └─ Magic Bytes: [192, 29, 126, 77]
  └─ Address Prefix: TIME0 (testnet) / TIME1 (mainnet)
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

| Tier | Collateral | Sampling Weight | Reward Weight | Governance Weight |
|------|------------|-----------------|---------------|-------------------|
| Free | 0 TIME | 1 | 1 | ❌ No vote |
| Bronze | 1,000 TIME | 10 | 5 | 1x |
| Silver | 10,000 TIME | 100 | 20 | 10x |
| Gold | 100,000 TIME | 1,000 | 60 | 100x |

*Sampling weight: VRF selection probability. Reward weight: block reward share. Governance weight: proposal voting power.*

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
*Solution*: Verify address starts with `TIME1` (mainnet) or `TIME0` (testnet).

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
