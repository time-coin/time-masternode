# TimeCoin Architecture Overview

**Last Updated:** 2025-12-23  
**Version:** 2.0 (Avalanche Consensus + TSDC Checkpointing)

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
┌──────────────────────┐ ┌─────────────────┐ ┌──────────────────┐
│  Consensus           │ │  Network        │ │  Storage         │
│  Engines             │ │  Layer          │ │  Layer           │
│  - Avalanche         │ │  - P2P Mesh     │ │  - Sled DB       │
│    (TX Finality)     │ │  - Message Relay│ │  - UTXO Manager  │
│  - TSDC              │ │  - Peer Mgmt    │ │  - TX Pool       │
│    (Block Producer)  │ │  - Heartbeats   │ │  - Block Chain   │
└──────────────────────┘ └─────────────────┘ └──────────────────┘
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

### 1. Consensus Engine - Avalanche (`consensus.rs`)

**Responsibility:** Transaction validation, ordering, and finality

**Key Features:**
- **Avalanche Protocol:** Continuous voting consensus with random validator sampling
- **Snowflake/Snowball:** Low-latency finality primitives
- **Stake-Weighted Sampling:** Validators selected proportional to collateral
- **Instant Finality:** Transactions finalized in ~750ms average
- **UTXO Locking:** Prevents double-spending during consensus
- **Deterministic Finality:** No forks after finality achieved

**Optimizations:**
- ✅ ArcSwap for masternode list (lock-free reads)
- ✅ OnceLock for identity (set-once, read-many)
- ✅ spawn_blocking for signature verification
- ✅ DashMap for transaction state tracking (per-txid)
- ✅ Per-txid consensus isolation (parallel processing)

**Data Structures:**
```rust
pub struct ConsensusEngine {
    avalanche: Arc<AvalancheConsensus>,          // Consensus state
    masternodes: ArcSwap<Vec<Masternode>>,      // Lock-free
    utxo_manager: Arc<UTXOStateManager>,        // UTXO state
    tx_pool: Arc<TransactionPool>,              // Mempool
}

pub struct AvalancheConsensus {
    tx_state: DashMap<Hash256, Arc<RwLock<Snowball>>>,      // Per-TX state
    active_rounds: DashMap<Hash256, Arc<RwLock<QueryRound>>>, // Vote tracking
    finalized_txs: DashMap<Hash256, Preference>,             // Finalized set
}
```

---

### 2. Block Production - TSDC (`tsdc.rs`)

**Responsibility:** Deterministic block production and checkpointing

**Key Features:**
- **Time-Slot Deterministic Consensus (TSDC):** Block leader elected per 10-min slot
- **VRF-Based Leader Selection:** Cryptographically verifiable randomness
- **Fixed Block Time:** Blocks produced every 10 minutes (600 seconds)
- **Checkpoint Creation:** Finalizes all pending Avalanche transactions
- **Masternode Rotation:** Fair leader selection based on stake

**Key Insight:**
- TSDC is **NOT** a consensus algorithm - it's a block production schedule
- Actual consensus for transaction finality happens in Avalanche (seconds)
- TSDC just bundles already-finalized transactions into periodic blocks

**Optimizations:**
- ✅ VRF prevents leader bias
- ✅ Deterministic output (no randomness after computation)
- ✅ O(1) leader lookup per slot

**Data Structures:**
```rust
pub struct TSCDConsensus {
    validators: Arc<RwLock<Vec<TSCDValidator>>>,  // Active validators
    current_slot: AtomicU64,                       // Current time slot
    finalized_height: AtomicU64,                   // Last finalized block
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

**Responsibility:** UTXO state management with consensus integration

**Key Features:**
- Tracks unspent transaction outputs with state machine:
  - **Unspent:** Available for spending
  - **SpentPending:** Input locked during Avalanche consensus
  - **Spent:** Transaction finalized
- Prevents double-spending via state locking
- Calculates UTXO set hash for validation
- State transitions during consensus rounds

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
    SpentPending {
        txid: Hash256,
        votes: u32,
        total_nodes: u32,
        spent_at: i64,
    },
    Spent,
}
```

---

### 6. Network Layer

**Responsibility:** P2P peer communication with persistent connections

**Key Features:**
- **Persistent Masternode Mesh:** Two-way connections established once, never disconnected
- **Message Types:**
  - TransactionBroadcast: New transactions
  - TransactionVoteRequest: Avalanche vote requests
  - TransactionVote: Validator votes for Avalanche
  - UTXOStateUpdate: State changes during consensus
  - BlockProposal: TSDC block production
  - Heartbeat: Liveness detection
- **Peer Discovery:** Masternode registry queries
- **Handshakes:** Network validation and peer identification
- **Connection Pooling:** Persistent connections per peer

**Connection Design:**
```
Masternode A ←→ Masternode B  (persistent TCP, no disconnect)
      ↓             ↓
Masternode C        
      ↓             ↓
   Full Node ←→ Full Node
```

---

### 7. Main Application (`main.rs` + new modules)

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
            ├─→ Avalanche consensus exits
            ├─→ TSDC block production exits
            ├─→ Network loop exits
            │
            ▼
        All await handles completed
            │
            ▼
        Process exits cleanly
```

---

## Data Flow

### Transaction Finality Flow (Avalanche Consensus)

```
User submits transaction (RPC sendrawtransaction)
    │
    ▼
ConsensusEngine::submit_transaction()
    ├─→ Validate transaction syntax & inputs
    ├─→ Lock UTXOs (state → SpentPending)
    ├─→ Broadcast to all masternodes
    ├─→ Add to TransactionPool (pending)
    │
    ▼
Initiate Avalanche Consensus
    ├─→ Create Snowball state machine
    ├─→ Create QueryRound for vote tracking
    │
    ▼
Execute Avalanche Rounds (10 rounds max)
    ├─→ Sample k validators (random)
    ├─→ Send TransactionVoteRequest
    ├─→ Collect votes for 2 seconds
    ├─→ Tally votes
    │
    ├─→ If α votes for Accept:
    │   ├─→ Update Snowball preference
    │   ├─→ Increment confidence counter
    │
    └─→ If confidence ≥ β (20):
        ├─→ Transaction FINALIZED
        ├─→ Move to finalized pool
        ├─→ Notify clients (instant finality ~750ms)
        │
        ▼
TSDC Block Production (every 10 minutes)
    ├─→ Collect finalized transactions
    ├─→ Select TSDC leader via VRF
    ├─→ Bundle into block
    ├─→ Commit to blockchain
    │
    ▼
Transaction in blockchain (permanent checkpoint)
```

**Avalanche Parameters:**
- **Sample size (k):** 20 validators per round
- **Quorum (α):** 14 responses needed for decision
- **Finality threshold (β):** 20 consecutive confirms
- **Query timeout:** 2 seconds per round
- **Typical finality:** 750ms (varies with network)

---

### Block Production Flow (TSDC)

```
Slot Timer (every 10 minutes)
    │
    ▼
TSDC::select_leader()
    ├─→ Calculate VRF output for current slot
    ├─→ Determine leader (deterministic)
    │
    ▼
If local node is leader:
    ├─→ Collect all finalized transactions
    ├─→ Generate deterministic block
    ├─→ Sign block
    ├─→ Broadcast BlockProposal
    │
    ▼
All nodes receive block
    ├─→ Validate block signature
    ├─→ Verify all transactions are finalized
    ├─→ Apply block to blockchain
    ├─→ Update UTXO state (SpentPending → Spent)
    │
    ▼
Block committed (immutable checkpoint)
    ├─→ Avalanche-finalized transactions now blockchain-confirmed
    ├─→ Clients can rely on finality
```

**TSDC Parameters:**
- **Block time:** 10 minutes (600 seconds)
- **Leader selection:** VRF-based (deterministic, cannot be gamed)
- **Transactions included:** Only those finalized by Avalanche
- **Block finality:** Permanent (cannot be reverted)

---

## Concurrency Model

### Lock Hierarchy

```
Application (no lock)
    │
    ├─→ DashMap operations (per-entry lock)
    │   ├─ ConsensusEngine.avalanche.tx_state (per-txid lock)
    │   ├─ ConsensusEngine.avalanche.active_rounds (per-txid lock)
    │   ├─ TransactionPool.pending (per-txid lock)
    │   ├─ TransactionPool.finalized (per-txid lock)
    │   ├─ UTXOStateManager.utxo_states (per-outpoint lock)
    │
    ├─→ ArcSwap operations (lock-free, atomic)
    │   ├─ ConsensusEngine.masternodes (lock-free swap)
    │
    ├─→ OnceLock operations (lock-free, set-once)
    │   ├─ ConsensusEngine.identity (set at startup)
    │
    ├─→ AtomicUsize operations (lock-free)
    │   ├─ TransactionPool.pending_count
    │   ├─ TransactionPool.pending_bytes
    │
    └─→ RwLock operations (reader-friendly)
        ├─ Snowball state (many readers during voting)
        ├─ QueryRound votes (collector updates)
```

### Async Runtime Isolation

**CPU-Intensive Work (moved off runtime):**
- ✅ Ed25519 signature verification (`spawn_blocking`)
- ✅ Sled I/O operations (`spawn_blocking`)
- ✅ Serialization/deserialization (in blocking context)

**Async Work (on runtime):**
- ✅ Network I/O and message relay
- ✅ Task coordination
- ✅ Timeout handling (vote collection windows)
- ✅ State updates (via lock-free structures)
- ✅ Avalanche round scheduling
- ✅ TSDC slot timing

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
