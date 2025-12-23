# TimeCoin Architecture Overview

**Last Updated:** 2025-12-22  
**Version:** 1.0 (Production Ready)

---

## System Architecture

```
┌─────────────────────────────────────────────────────────┐
│                      Application Layer                   │
│  ┌──────────────────────────────────────────────────┐   │
│  │  Main Application (main.rs)                      │   │
│  │  - Initialization & Configuration              │   │
│  │  - Graceful Shutdown Manager                   │   │
│  │  - Task Coordination                           │   │
│  └──────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────┘
                           │
       ┌───────────────────┼───────────────────┐
       │                   │                   │
       ▼                   ▼                   ▼
┌─────────────────┐ ┌─────────────────┐ ┌──────────────────┐
│  Consensus      │ │  Network        │ │  Storage         │
│  Engine         │ │  Layer          │ │  Layer           │
│  - PBFT         │ │  - P2P Mesh     │ │  - Sled DB       │
│  - BFT          │ │  - Message Relay│ │  - UTXO Manager  │
│  - Votes        │ │  - Peer Mgmt    │ │  - TX Pool       │
└─────────────────┘ └─────────────────┘ └──────────────────┘
       │                   │                   │
       └───────────────────┼───────────────────┘
                           │
                    ┌──────▼──────┐
                    │  Blockchain │
                    │  - Blocks   │
                    │  - Chain    │
                    │  - State    │
                    └─────────────┘
```

---

## Core Components

### 1. Consensus Engine (`consensus.rs`)

**Responsibility:** Transaction validation and ordering

**Key Features:**
- Validates transactions before consensus
- Locks UTXOs to prevent double-spending
- Manages pending transaction pool
- Broadcasts transaction proposals

**Optimizations:**
- ✅ ArcSwap for masternode list (lock-free reads)
- ✅ OnceLock for identity (set-once, read-many)
- ✅ spawn_blocking for signature verification
- ✅ DashMap for transaction voting

**Data Structures:**
```rust
pub struct ConsensusEngine {
    masternodes: ArcSwap<Vec<Masternode>>,        // Lock-free
    identity: OnceLock<NodeIdentity>,             // Set-once
    votes: DashMap<Hash256, Vec<Vote>>,           // Lock-free concurrent
    utxo_manager: Arc<UTXOStateManager>,          // Shared state
    tx_pool: Arc<TransactionPool>,                // Shared pool
}
```

---

### 2. BFT Consensus (`bft_consensus.rs`)

**Responsibility:** Block consensus and finalization

**Key Features:**
- Implements PBFT protocol phases (Pre-prepare, Prepare, Commit)
- Manages consensus rounds per block height
- Handles view changes on timeout
- Tracks committed blocks

**Optimizations:**
- ✅ DashMap for per-height round isolation (eliminates global lock)
- ✅ OnceLock for signing key (set once at startup)
- ✅ Parking lot Mutex for committed blocks (simple, rarely contested)
- ✅ Block hash index for O(1) vote routing
- ✅ Background timeout monitor task

**Data Structures:**
```rust
pub struct BFTConsensus {
    rounds: DashMap<u64, ConsensusRound>,         // Per-height lock-free
    block_hash_index: DashMap<Hash256, u64>,      // Fast lookup
    committed_blocks: Mutex<Vec<Block>>,          // Simple mutex
    signing_key: OnceLock<SigningKey>,            // Set-once
    masternode_count: AtomicUsize,                // O(1) access
}
```

---

### 3. Transaction Pool (`transaction_pool.rs`)

**Responsibility:** Mempool management

**Key Features:**
- Stores pending transactions awaiting consensus
- Enforces size limits (10,000 tx max, 300MB max)
- Evicts lowest-fee transactions when full
- Tracks finalized transactions
- Maintains rejection cache

**Optimizations:**
- ✅ DashMap for lock-free access (no global lock)
- ✅ AtomicUsize for O(1) metrics
- ✅ PoolEntry metadata (fee, size, timestamp)
- ✅ Fee-based eviction policy

**Data Structures:**
```rust
pub struct TransactionPool {
    pending: DashMap<Hash256, PoolEntry>,         // Lock-free pending
    finalized: DashMap<Hash256, PoolEntry>,       // Lock-free finalized
    rejected: DashMap<Hash256, (String, Instant)>,// Rejection cache
    pending_count: AtomicUsize,                   // O(1) counter
    pending_bytes: AtomicUsize,                   // O(1) counter
}

struct PoolEntry {
    tx: Transaction,
    fee: u64,
    added_at: Instant,
    size: usize,
}
```

---

### 4. Storage Layer (`storage.rs`)

**Responsibility:** Persistent data storage

**Key Features:**
- Sled-based key-value store
- Batch operations for atomic writes
- High-throughput mode enabled
- Optimized cache sizing

**Optimizations:**
- ✅ spawn_blocking for all I/O operations
- ✅ Batch operations for atomicity
- ✅ Optimized sysinfo usage
- ✅ Proper error types

**Implementation:**
```rust
pub struct SledUtxoStorage {
    db: sled::Db,
}

impl UtxoStorage for SledUtxoStorage {
    async fn get_utxo(&self, outpoint: &OutPoint) -> Option<UTXO> {
        let db = self.db.clone();
        spawn_blocking(move || {
            let key = bincode::serialize(outpoint).ok()?;
            let value = db.get(&key).ok()??;
            bincode::deserialize(&value).ok()
        }).await.ok()?
    }
}
```

---

### 5. UTXO Manager (`utxo_manager.rs`)

**Responsibility:** UTXO state management

**Key Features:**
- Tracks unspent transaction outputs
- Prevents double-spending via locking
- Calculates UTXO set hash for validation
- Manages state transitions

**Optimizations:**
- ✅ DashMap for lock-free concurrent access
- ✅ Streaming UTXO iteration
- ✅ Efficient hash calculation
- ✅ Entry API for atomic operations

**Data Structures:**
```rust
pub struct UTXOStateManager {
    storage: Arc<dyn UtxoStorage>,
    utxo_states: DashMap<OutPoint, UTXOState>,    // Lock-free state
}

pub enum UTXOState {
    Unspent,
    Locked { txid: Hash256, locked_at: i64 },
    Spent,
}
```

---

### 6. Connection Manager (`connection_manager.rs`)

**Responsibility:** P2P peer connection management

**Key Features:**
- Tracks inbound and outbound connections
- Maintains connection state
- Handles reconnection logic
- Enforces connection limits

**Optimizations:**
- ✅ DashMap for lock-free concurrent access
- ✅ ArcSwapOption for local IP (atomic updates)
- ✅ AtomicUsize for O(1) counters
- ✅ Entry API for atomicity

**Data Structures:**
```rust
pub struct ConnectionManager {
    connections: DashMap<String, ConnectionState>,// Lock-free
    reconnecting: DashMap<String, ReconnectionState>,
    local_ip: ArcSwapOption<String>,              // Atomic updates
    inbound_count: AtomicUsize,                   // O(1) counter
    outbound_count: AtomicUsize,                  // O(1) counter
}
```

---

### 7. Network Layer

**Responsibility:** P2P message handling

**Components:**
- **Message Types:** Transaction broadcasts, block proposals, votes
- **Peer Discovery:** Masternode registry queries
- **Handshakes:** Network validation and peer identification
- **Heartbeats:** Liveness detection via ping/pong

**Features:**
- Message compression ready (flate2 infrastructure)
- Size limits on responses
- Pagination support for large data sets
- Connection pooling

---

### 8. Main Application (`main.rs` + new modules)

**Module Structure:**
```
main.rs
├── app_context.rs       - Shared application state
├── app_builder.rs       - Initialization builder
├── app_utils.rs         - Utility functions
├── error.rs             - Unified error types
└── shutdown.rs          - Graceful shutdown management
```

**Key Features:**
- ✅ Graceful shutdown with CancellationToken
- ✅ Task registration and cleanup
- ✅ Configuration management
- ✅ Comprehensive error handling

**Shutdown Flow:**
```
Ctrl+C Signal
    │
    ▼
ShutdownManager::cancel()
    │
    ├─→ CancellationToken::cancel()
    │
    └─→ All spawned tasks receive signal
            │
            ├─→ Heartbeat task exits
            ├─→ Block production exits
            ├─→ Network loop exits
            ├─→ Consensus tasks exit
            │
            ▼
        All await handles completed
            │
            ▼
        Process exits cleanly
```

---

## Data Flow

### Transaction Processing Flow

```
User submits transaction
    │
    ▼
ConsensusEngine::submit_transaction()
    ├─→ Validate transaction syntax
    ├─→ Lock UTXOs (prevent double-spend)
    ├─→ Broadcast to all peers
    │
    ▼
ConsensusEngine::process_transaction()
    ├─→ Add to TransactionPool
    ├─→ Update transaction pool votes
    │
    ▼
Consensus voting reaches quorum
    │
    ├─→ finalize_transaction_approved()
    ├─→ Move to finalized pool
    ├─→ Clean up votes
    │
    ▼
BFTConsensus::propose_block()
    ├─→ Select highest-fee transactions
    ├─→ Include in block proposal
    │
    ▼
BFT voting reaches quorum
    │
    ├─→ Commit block
    ├─→ Update blockchain
    │
    ▼
Finalization complete
```

### Block Consensus Flow

```
BFTConsensus::propose_block(block, signature)
    │
    ├─→ Store in rounds[height].proposed_block
    ├─→ Index block hash for O(1) lookup
    ├─→ Broadcast Pre-prepare message
    │
    ▼
Peers receive Pre-prepare
    │
    ├─→ Validate block signature
    ├─→ Send Prepare vote
    │
    ▼
handle_vote(prepare_vote)
    │
    ├─→ Look up height via block_hash_index (O(1))
    ├─→ Add to rounds[height].votes
    ├─→ Check consensus threshold
    │
    ├─→ If 2/3 quorum reached:
    │   ├─→ Send Commit vote
    │   ├─→ Wait for commit votes
    │
    ├─→ If final quorum reached:
    │   ├─→ Commit block
    │   ├─→ Clean up votes
    │   ├─→ Update UTXO state
    │
    ▼
Block finalized
```

---

## Concurrency Model

### Lock Hierarchy

```
Application (no lock)
    │
    ├─→ DashMap operations (per-entry lock)
    │   ├─ ConsensusEngine.votes
    │   ├─ BFTConsensus.rounds
    │   ├─ TransactionPool.pending
    │   ├─ TransactionPool.finalized
    │   ├─ ConnectionManager.connections
    │
    ├─→ ArcSwap operations (lock-free, atomic)
    │   ├─ ConsensusEngine.masternodes
    │   ├─ ConnectionManager.local_ip
    │
    ├─→ OnceLock operations (lock-free, set-once)
    │   ├─ ConsensusEngine.identity
    │   ├─ BFTConsensus.signing_key
    │
    ├─→ AtomicUsize operations (lock-free)
    │   ├─ TransactionPool.pending_count
    │   ├─ TransactionPool.pending_bytes
    │   ├─ ConnectionManager.inbound_count
    │   ├─ BFTConsensus.masternode_count
    │
    └─→ Parking lot Mutex (simple lock)
        ├─ BFTConsensus.committed_blocks
```

### Async Runtime Isolation

**CPU-Intensive Work (moved off runtime):**
- ✅ Ed25519 signature verification (`spawn_blocking`)
- ✅ Sled I/O operations (`spawn_blocking`)
- ✅ Serialization/deserialization (in blocking context)

**Async Work (on runtime):**
- ✅ Network I/O
- ✅ Task coordination
- ✅ Timeout handling
- ✅ State updates (via lock-free structures)

---

## Error Handling

**Unified Error Type:**
```rust
#[derive(Error, Debug)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),
    
    #[error("Storage error: {0}")]
    Storage(#[from] StorageError),
    
    #[error("Consensus error: {0}")]
    Consensus(String),
    
    #[error("Network error: {0}")]
    Network(String),
}
```

**Error Propagation:**
- All async functions return `Result<T, AppError>`
- Main function catches and logs errors
- Graceful shutdown triggered on fatal errors

---

## Performance Characteristics

| Operation | Time Complexity | Space Complexity | Notes |
|-----------|-----------------|------------------|-------|
| Get UTXO | O(1) | O(1) | Lock-free DashMap |
| Add transaction | O(1) | O(n) | Atomic counter update |
| Check consensus | O(m) | O(m) | m = votes in round |
| List pending txs | O(n) | O(n) | n = pending count |
| Handle vote | O(1) | O(1) | Per-height lock |
| Route vote | O(1) | O(1) | Block hash index |
| Get connection count | O(1) | O(1) | Atomic counter |

---

## Scalability

**Horizontal Scaling:**
- Per-height BFT rounds enable parallel consensus
- DashMap enables many concurrent voters
- Lock-free primitives prevent contention

**Vertical Scaling:**
- Atomic counters for O(1) metrics
- Batch operations for database efficiency
- spawn_blocking prevents async runtime saturation

**Resource Limits:**
- Max 10,000 pending transactions
- Max 300MB pending transaction memory
- Max 50 peer connections
- Vote cleanup on finalization

---

## Deployment Architecture

```
┌────────────────────────────────────────┐
│  Load Balancer / DNS                   │
└────────────────────┬───────────────────┘
                     │
        ┌────────────┼────────────┐
        │            │            │
        ▼            ▼            ▼
    ┌───────┐   ┌───────┐   ┌───────┐
    │Node 1 │   │Node 2 │   │Node 3 │
    │Master │   │Master │   │Master │
    └───┬───┘   └───┬───┘   └───┬───┘
        │            │            │
        └────────────┼────────────┘
                     │
            P2P Mesh Network
                (Gossip)
                     │
        ┌────────────┼────────────┐
        │            │            │
        ▼            ▼            ▼
    ┌───────┐   ┌───────┐   ┌───────┐
    │Node 4 │   │Node 5 │   │Node 6 │
    │ Full  │   │ Full  │   │ Full  │
    └───────┘   └───────┘   └───────┘
```

**Minimum:** 3 masternodes for quorum (2/3)  
**Recommended:** 5+ masternodes for redundancy  
**Full nodes:** Can be unlimited

---

## Security Considerations

| Aspect | Implementation |
|--------|----------------|
| **Message Authentication** | Ed25519 signatures |
| **Double-Spend Prevention** | UTXO locking mechanism |
| **Byzantine Tolerance** | PBFT consensus (2/3 quorum) |
| **Sybil Protection** | Masternode registry |
| **Network Privacy** | Optional encryption layer |
| **DOS Protection** | Rate limiting per peer |

---

**Last Updated:** 2025-12-22  
**Architecture Version:** 1.0 (Stable)
