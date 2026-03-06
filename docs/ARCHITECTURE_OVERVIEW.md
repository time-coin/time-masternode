# TimeCoin Architecture Overview

**Last Updated:** 2026-03-06  
**Version:** 1.3.0 (Block Producer Signatures, VRF Rolling Window, Genesis Checkpoint)

---

## Recent Updates (v1.3.0 - March 2026)

### Ed25519 Block Producer Signatures

- **`producer_signature` field added to `BlockHeader`**: the block producer signs the block hash with its Ed25519 key after VRF selection
- **Prevents VRF proof reuse**: without this, a valid VRF proof could be detached from its original block and paired with tampered content (different transactions, rewards, or merkle root)
- **Verified in two places**: `validate_block_before_vote()` for newly proposed blocks; `add_block()` for synced blocks from peers
- **Backward compatible**: empty signatures are accepted for pre-signature blocks; `#[serde(default)]` on the field

### VRF Eligibility: 3-Block Rolling Participation Window

- **Replaced single-block bitmap gate** with a rolling window spanning the last 3 blocks
- A masternode is VRF-eligible if it appeared in the `consensus_participants_bitmap` (or was block producer) in any of the 3 most-recent blocks
- **Motivation**: high-latency nodes whose precommit vote arrived slightly late for one round were excluded from the bitmap and systematically locked out of VRF sortition, losing block-producer rewards
- **Grace period**: a node must miss 3 consecutive rounds before losing VRF eligibility; one late vote no longer disqualifies

### Genesis Checkpoint Enforcement

- **Testnet genesis hash hardcoded** in `constants.rs`; `GenesisBlock::verify_checkpoint()` validates the hash on startup and on every `add_block` at height 0
- **Infinite fork-resolution loop fixed**: previously, a genesis hash mismatch caused endless retry loops as the node attempted to resolve a fork with a peer on a different chain; now a `genesis_mismatch_detected` flag is set after the first mismatch at `common_ancestor=0` and further attempts are suppressed with a logged warning
- **No automatic data deletion**: the operator must manually resolve a genesis mismatch; the node never deletes its own chain based on a peer's claim

### Block Producer Signature Mismatch During Sync (Warning, Not Error)

- **Changed from fatal rejection to logged warning** when a synced block's `producer_signature` fails verification
- **Root cause**: a freshly syncing node has stale public keys in its masternode registry (loaded from disk before the chain is rebuilt); collateral-UTXO checks prevent live announcements from updating those keys until enough UTXOs are synced, creating a dead-lock
- **Safety**: the block hash chain still guarantees integrity; once the node reaches the chain tip the registry is refreshed via live announcements and real-time blocks are fully verified

### Reward Address Routing Fix

- **`masternode re-registration now overwrites `wallet_address`** when `reward_address` in `time.conf` changes
- **Previous bug**: changing `reward_address` and restarting did not update the stored `masternode.wallet_address`; block rewards continued routing to the old local wallet instead of the newly configured GUI wallet address
- **Fix applied in `register_internal()`** in `masternode_registry.rs`

---

## Recent Updates (v1.2.0 - February 22, 2026)

### Fork Resolution Simplification

- **Removed stake override logic** from `fork_resolver.rs`: stake can no longer override the longest chain rule
- **Three simple rules**: (1) reject future timestamps, (2) longer chain always wins, (3) same height uses stake then hash tiebreaker
- **`handle_fork()`** simplified to flat early-return structure (no stake override acceptance path)
- **`check_2_3_consensus_for_production()`** now counts behind-peers as agreeing and includes own weight in total

### VRF Sortition Tightening

- **`TARGET_PROPOSERS` reduced from 3 to 1**: targets exactly one block producer per slot, reducing competing blocks
- **Wall-clock deadlock detection**: VRF threshold relaxation now uses real elapsed time waiting at a height, not time since slot was scheduled (prevents all nodes being eligible during catch-up)
- **Free-tier sybil protection**: Free nodes require 60s of deadlock (attempt ≥ 6) before receiving VRF boost

### Catch-up Micro-fork Prevention

- **Non-consensus peer filter relaxed for small gaps**: blocks from peers 1-5 blocks ahead are accepted from any whitelisted peer (consensus list is stale during rapid catch-up)

### Masternode Key System (Dash-style)

- **Replaced certificate-based key system** with single `masternodeprivkey` in `time.conf`
- **`masternode genkey` RPC/CLI command**: generates base58check-encoded Ed25519 private key
- **masternode.conf simplified**: 4-field format (alias, IP:port, txid, vout) — key is in time.conf, not masternode.conf
- **Certificate system removed**: no more `MASTERNODE_AUTHORITY_PUBKEY`, `verify_masternode_certificate()`, or website registration
- **Backward compatibility**: old 5/6-field masternode.conf formats still parsed (extra fields ignored)

### Previous Updates (v1.1.0 - February 2026)

**Bug #4: Fork Resolution Inconsistency (Feb 1, 2026)**
- **Issue**: VRF tiebreaker used "higher score wins" but hash tiebreaker used "lower hash wins"
- **Impact**: Network fragmentation - nodes on same-height fork couldn't agree on canonical chain
- **Root Cause**: `choose_canonical_chain()` had VRF score comparison that contradicted hash tiebreaker
- **Fix**: Removed VRF score from fork resolution; now uses "lower hash wins" consistently everywhere
- **Result**: All fork resolution paths (blockchain.rs, masternode_authority.rs) now agree using 2/3 weighted stake consensus

**Bug #1: Broadcast Callback Not Wired**
- **Issue**: Consensus engine had no way to broadcast TimeVote requests
- **Impact**: Vote requests never sent to network, transactions never finalized network-wide
- **Root Cause**: `set_broadcast_callback()` method existed but was never called in initialization
- **Fix**: Wired up `peer_connection_registry.broadcast()` as consensus callback in main.rs after network server initialization
- **Result**: TimeVote consensus now fully functional end-to-end

**Bug #2: Finalized Pool Premature Clearing**
- **Issue**: Finalized transaction pool cleared after EVERY block addition
- **Impact**: Locally finalized transactions lost before they could be included in locally produced blocks
- **Root Cause**: `clear_finalized_transactions()` called blindly without checking if TXs were in the block
- **Fix**: Added `clear_finalized_txs(txids)` to selectively clear only transactions actually in the added block
- **Result**: Finalized transactions now properly persist until included in a block

**Bug #3: Hardcoded Version String**
- **Issue**: Version hardcoded as "1.0.0" instead of using Cargo.toml
- **Impact**: Impossible to distinguish nodes with new TimeVote code from old nodes
- **Fix**: Use `env!("CARGO_PKG_VERSION")` compile-time macro
- **Result**: Version now automatically reflects Cargo.toml (currently 1.1.0)

### TimeVote Transaction Flow (Now Working)

```
1. TX Submission → RPC (sendtoaddress)
                ↓
2. Validation → Lock UTXOs (SpentPending state)
                ↓
3. Broadcast → TransactionBroadcast to all peers
                ↓
4. TimeVote Request → Broadcast vote request (NOW WORKING!)
                ↓
5. Vote Collection → Validators respond with signed votes
                ↓
6. Vote Accumulation → Stake-weighted sum calculated
                ↓
7. Finalization → 67% threshold → Move to finalized pool (ALL NODES)
                ↓
8. TimeProof Assembly → Collect Accept votes, create proof
                ↓
9. Block Production → Query finalized pool, include TXs
                ↓
10. Block Addition → Process UTXOs, selectively clear finalized pool (NOW WORKING!)
                ↓
11. Archival → TX confirmed on blockchain
```

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
│  - TimeVote          │ │  - P2P TCP      │ │  - Sled DB       │
│    (TX Finality)     │ │  - Message Relay│ │  - UTXO Manager  │
│  - TimeLock          │ │  - Peer Mgmt    │ │  - TX Pool       │
│    (Block Producer)  │ │  - Heartbeats   │ │  - Block Chain   │
│  - AI Fork Resolver  │ │  - Fork Sync    │ │  - AI History    │
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

### 1. Consensus Engine - TimeVote Protocol (`consensus.rs`)

**Responsibility:** Transaction validation, ordering, and finality

**Key Features:**
- **TimeVote Protocol:** Continuous voting consensus with stake-weighted validator voting
- **Progressive TimeProof Assembly:** Signed votes accumulate to form verifiable proof
- **Unified Finality:** Single finality state (67% weight threshold, liveness fallback to 51% after 30s)
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
    timevote: Arc<TimeVoteConsensus>,           // Consensus state
    masternodes: ArcSwap<Vec<Masternode>>,      // Lock-free
    utxo_manager: Arc<UTXOStateManager>,        // UTXO state
    tx_pool: Arc<TransactionPool>,              // Mempool
}

pub struct TimeVoteConsensus {
    tx_state: DashMap<Hash256, Arc<RwLock<VotingState>>>,      // Per-TX state
    active_rounds: DashMap<Hash256, Arc<RwLock<QueryRound>>>, // Vote tracking
    finalized_txs: DashMap<Hash256, Preference>,             // Finalized set
}
```

---

### 2. Block Production - TimeLock (`tsdc.rs`)

**Responsibility:** Deterministic block production and checkpointing

**Key Features:**
- **TimeLock:** Block leader elected per 10-min slot
- **VRF-Based Leader Selection:** Cryptographically verifiable randomness
- **Fixed Block Time:** Blocks produced every 10 minutes (600 seconds)
- **Checkpoint Creation:** Finalizes all pending TimeVote transactions
- **Masternode Rotation:** Fair leader selection based on stake

**Key Insight:**
- TimeLock is **NOT** a consensus algorithm - it's a block production schedule
- Actual consensus for transaction finality happens in TimeVote (seconds)
- TimeLock just bundles already-finalized transactions into periodic blocks

**Optimizations:**
- ✅ VRF prevents leader bias
- ✅ Deterministic output (no randomness after computation)
- ✅ O(1) leader lookup per slot

**Data Structures:**
```rust
pub struct TimeLockConsensus {
    validators: Arc<RwLock<Vec<TimeLockValidator>>>,  // Active validators
    current_slot: AtomicU64,                           // Current time slot
    finalized_height: AtomicU64,                       // Last finalized block
}
```

---

### 2.1 Fork Resolution Rules

**Canonical Chain Selection** (deterministic, all nodes agree):

1. **Longer chain wins** - Higher block height is always canonical
2. **Lower hash wins** - At equal height, lexicographically smaller block hash is canonical

**Consistency:** This rule is applied uniformly across:
- `blockchain.rs` - `compare_chain_with_peers()` (height-first, stake tiebreaker)
- `ai/fork_resolver.rs` - Longest-chain fork decisions (3 simple rules)
- `masternode_authority.rs` - Masternode chain authority analysis
- `network/peer_connection.rs` - Peer chain comparison

**Why lower hash?**
- Deterministic: All nodes compute same result
- Simple: No external dependencies
- Standard: Follows Bitcoin/Ethereum convention
- Verifiable: Anyone can check the comparison

**Note:** VRF is used for **leader selection** (who produces blocks), NOT for fork resolution tiebreaking.

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
  - **SpentPending:** Input locked during TimeVote consensus
  - **Spent:** Transaction finalized
- Prevents double-spending via state locking
- Calculates UTXO set hash for validation
- State transitions during consensus rounds

**Optimizations:**
- ✅ DashMap for lock-free concurrent access
- ✅ Per-address UTXO index (`DashMap<String, DashSet<OutPoint>>`) for O(n-per-address) lookups
- ✅ Streaming UTXO iteration
- ✅ Efficient hash calculation
- ✅ Entry API for atomic operations
- ✅ Auto-consolidation when transfers need >5000 inputs

**Data Structures:**
```rust
pub struct UTXOStateManager {
    storage: Arc<dyn UtxoStorage>,
    utxo_states: DashMap<OutPoint, UTXOState>,              // Lock-free state
    address_index: DashMap<String, DashSet<OutPoint>>,      // Per-address UTXO index
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
  - TransactionVoteRequest: TimeVote vote requests
  - TransactionVote: Validator votes for TimeVote
  - UTXOStateUpdate: State changes during consensus
  - BlockProposal: TimeLock block production
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

### 7. Main Application (`main.rs`)

**Module Structure:**
```
main.rs
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
            ├─→ TimeVote consensus exits
            ├─→ TimeLock block production exits
            ├─→ Network loop exits
            │
            ▼
        All await handles completed
            │
            ▼
        Process exits cleanly
```

**Note:** Internal code may reference "Avalanche" for historical reasons - this refers to the TimeVote Protocol implementation.

---

## Data Flow

### Transaction Finality Flow (TimeVote Consensus)

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
Initiate TimeVote Consensus (Unified Finality)
    ├─→ Transaction enters "Voting" state
    ├─→ Create QueryRound for vote tracking
    │
    ▼
Execute TimeVote Rounds (progressive TimeProof assembly)
    ├─→ Sample k validators (stake-weighted)
    ├─→ Send TransactionVoteRequest
    ├─→ Collect signed votes for 2 seconds
    ├─→ Accumulate unique signed votes toward TimeProof
    │
    ├─→ If α votes for Accept:
    │   ├─→ Add signed votes to TimeProof
    │   ├─→ Update accumulated weight
    │
    └─→ If accumulated_weight ≥ Q_finality (67% of AVS weight, 51% liveness fallback):
        ├─→ Transaction FINALIZED (single unified state)
        ├─→ TimeProof complete (verifiable by anyone)
        ├─→ Move to finalized pool
        ├─→ Notify clients (instant finality ~750ms)
        │
        ▼
TimeLock Block Production (every 10 minutes)
    ├─→ Collect finalized transactions
    ├─→ Select TimeLock leader via VRF
    ├─→ Bundle into block
    ├─→ Commit to blockchain
    │
    ▼
Transaction in blockchain (permanent checkpoint)
```

**TimeVote Parameters:**
- **Sample size (k):** 20 validators per round
- **Quorum (α):** 14 responses needed for decision
- **Finality threshold (Q_finality):** 67% of AVS weight (falls back to 51% after 30s stall for liveness)
- **Query timeout:** 2 seconds per round
- **Typical finality:** 750ms (varies with network)

---

### Block Production Flow (TimeLock)

```
Slot Timer (every 10 minutes)
    │
    ▼
TimeLock::select_leader()
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
    ├─→ TimeVote-finalized transactions now blockchain-confirmed
    ├─→ Clients can rely on finality
```

**TimeLock Parameters:**
- **Block time:** 10 minutes (600 seconds)
- **Leader selection:** VRF-based (deterministic, cannot be gamed)
- **Transactions included:** Only those finalized by TimeVote
- **Block finality:** Permanent (cannot be reverted)

---

## Concurrency Model

### Lock Hierarchy

```
Application (no lock)
    │
    ├─→ DashMap operations (per-entry lock)
    │   ├─ ConsensusEngine.timevote.tx_state (per-txid lock)
    │   ├─ ConsensusEngine.timevote.active_rounds (per-txid lock)
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
        ├─ Voting state (many readers during consensus)
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
- ✅ TimeVote round scheduling
- ✅ TimeLock slot timing

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

**Minimum:** 3 masternodes for quorum (67% stake-weighted majority)  
**Recommended:** 5+ masternodes for redundancy  
**Full nodes:** Can be unlimited

---

## Security Considerations

| Aspect | Implementation |
|--------|----------------|
| **Message Authentication** | Ed25519 signatures |
| **Double-Spend Prevention** | UTXO locking mechanism |
| **Byzantine Tolerance** | Stake-weighted consensus (67% quorum, BFT-safe) |
| **Sybil Protection** | Masternode registry |
| **Network Privacy** | Optional encryption layer |
| **DOS Protection** | Rate limiting per peer |

---

**Last Updated:** 2025-12-24  
**Architecture Version:** 2.1 (Code cleanup)

---

## Complete Transaction & Consensus Flow

*Based on actual code analysis (codebase version 2026-02-16). Sections that duplicate content already covered above are omitted.*

---

### Node Startup Sequence (`main.rs`)

#### Initialization Order

1. **Parse CLI args** — config path, listen addr, masternode flag, verbose, demo, generate-config
2. **Print hostname banner** — node identity display
3. **Determine network type** — Mainnet or Testnet from config
4. **Setup logging** — tracing-subscriber with systemd detection, hostname prefix
5. **Open sled databases**:
   - `{data_dir}/db/peers` — Peer manager storage
   - `{data_dir}/db/registry` — Masternode registry
   - `{data_dir}/db/blocks` — Block storage (`flush_every_ms(None)`, `Mode::LowSpace`)
   - `{data_dir}/db/txindex` — Transaction index
6. **Initialize UTXO storage** — InMemoryUtxoStorage (or SledUtxoStorage)
7. **Initialize UTXOStateManager** — Loads UTXO states from storage
8. **Initialize PeerManager** — Peer discovery and tracking
9. **Initialize MasternodeRegistry** — With peer manager reference
10. **Initialize ConsensusEngine** — With masternode registry and UTXO manager
11. **Initialize AI System** — All 7 AI modules in AISystem struct (+ 3 wired separately) with shared sled Db
12. **Enable AI Transaction Validation** — On consensus engine
13. **Initialize Blockchain** — With block storage, consensus, registry, UTXO, network type
14. **Set AI System on Blockchain** — For intelligent decision recording
15. **Configure block compression** — Currently forced OFF
16. **Initialize Transaction Index** — For O(1) TX lookups
17. **Verify chain height integrity** — Fix inconsistencies from crashes
18. **Validate genesis block** — Create if needed, verify if exists
19. **Initialize TimeSync** — NTP-based time synchronization
20. **Start PeerConnectionRegistry** — Connection tracking
21. **Start ConnectionManager** — Manages connection lifecycle
22. **Start PeerStateManager** — Peer state machine
23. **Wire AI System on NetworkServer** — For attack enforcement
24. **Spawn block production task** — Event-driven + 10-minute interval TimeLock consensus
25. **Spawn status report task** — 60-second interval with AI reporting
26. **Spawn cleanup task** — 10-minute interval for memory management
27. **Start RPC server** — HTTP JSON-RPC interface
28. **Start NetworkServer** — Inbound peer connections (attack enforcement every 30 s)
29. **Start NetworkClient** — Outbound peer connections with adaptive reconnection
30. **Wait for shutdown** — Ctrl+C signal
31. **Flush sled to disk** — Critical: prevents block corruption

#### Genesis Block Handling

- If no genesis exists: create one with `Blockchain::create_genesis_block()`
- Genesis timestamp is network-type specific (Mainnet vs Testnet)
- Genesis block has height 0, `previous_hash = [0; 32]`
- Genesis is validated on startup: hash check, height-0 verification

---

### Block Production Flow

#### TimeLock Leader Selection

- **Block interval**: 600 seconds (10 minutes)
- **Leader selection**: VRF (ECVRF) using input:
  `SHA256("TIMECOIN_VRF_V2" || height_le_bytes || previous_hash)`
- Each masternode evaluates its own VRF proof; the single highest output wins
- Fallback leader rotation uses `TimeLock-leader-selection-v2` input on timeout

#### Block Production Loop (Event-Driven + Interval)

The main block production loop uses `tokio::select!` with four branches:
1. **Shutdown signal** — graceful exit
2. **Production trigger** — immediate wake when status check detects chain is behind
3. **`block_added_signal.notified()`** — event-driven wake when any block is added (sync, consensus, or own production), reducing latency to near-instant
4. **`interval.tick()`** — periodic 1-second fallback polling

#### Two-Phase Commit (2PC) for Block Finality

**Phase 1 — Propose:**
1. Leader assembles block from transaction pool
2. Broadcasts `TimeLockBlockProposal { block }` to all peers
3. Validators verify: valid transactions, correct previous hash, valid merkle root

**Phase 2a — Prepare Votes:**
1. Validators send `TimeVotePrepare { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PREPARE"`
3. Votes accumulate by validator **stake weight** (not raw count)
4. Threshold: >50% of participating validator weight

**Phase 2b — Precommit Votes:**
1. After prepare threshold met, validators send `TimeVotePrecommit { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PRECOMMIT"`
3. Threshold: >50% of participating validator weight
4. Block is finalized after precommit threshold

**TimeProof Finality (separate from 2PC):**
- Transactions achieve instant finality via TimeProof with **67% weighted stake** threshold (liveness fallback to 51% after 30 s)
- Weight is tier-based **sampling weight**: Free=1, Bronze=10, Silver=100, Gold=1000
- This is distinct from tier pool allocation and governance voting power

**Liveness Fallback:**
- Stall detection timeout: 30 seconds without consensus progress
- Broadcasts `LivenessAlert`, enters `FallbackResolution` state
- Up to 5 fallback rounds with 10-second round timeout each
- Deterministic hash-based leader selection per fallback round:
  `leader = MN with min SHA256(txid || slot_index || round || mn_address)`
- If validator count < 3 (early network / single node), block is added directly without votes

#### Block Structure

```
Block {
    header: BlockHeader {
        version: u32,
        height: u64,
        previous_hash: Hash256,
        merkle_root: Hash256,
        timestamp: i64,
        block_reward: u64,
        leader: String,
        attestation_root: Hash256,
        masternode_tiers: MasternodeTierCounts,
        vrf_proof: Vec<u8>,
        vrf_output: Hash256,
        vrf_score: u64,
        active_masternodes_bitmap: Vec<u8>,
        liveness_recovery: Option<bool>,
    },
    transactions: Vec<Transaction>,
    masternode_rewards: Vec<(String, u64)>,
    time_attestations: Vec<TimeAttestation>,
    consensus_participants_bitmap: Vec<u8>,
    liveness_recovery: Option<bool>,
}
```

#### Block Storage Key Formats

- Key format: `block_{height}` (current)
- Legacy `block:{height}` and BlockV1 schema migration supported in code but unused
- Chain height: `chain_height` key, bincode-serialized `u64`
- Chain tip: `tip_height` key, little-endian `u64` bytes
- Each write calls `db.flush()` with immediate readback verification
- Two-tier block cache: hot (deserialized) + warm (serialized) for 10–50× faster reads

---

### Transaction Flow

#### Transaction Structure

```
Transaction {
    inputs:    Vec<TxInput>,
    outputs:   Vec<TxOutput>,
    lock_time: u64,
    tx_type:   TransactionType,
}

TransactionType: Standard, CoinbaseReward, MasternodeReward,
                 MasternodeLock, MasternodeUnlock, GovernanceVote,
                 TimeProof, SmartContract
```

#### Transaction Processing Steps

1. **Receive**: `TransactionBroadcast` message from peer
2. **Dedup**: Check SeenTransactions filter (bloom-filter-like)
3. **AI Attack Detection**: Record transaction for double-spend tracking
4. **Consensus Processing**: `ConsensusEngine::process_transaction()`
   - Validate against UTXO set
   - AI transaction validation (spam/dust detection)
   - Add to transaction pool
5. **Gossip**: Broadcast to other connected peers
6. **TimeVote Finality**: Instant finality via TimeVote consensus

#### Per-Transaction State Machine (`TransactionStatus`)

```
Seen → Voting → Finalized → Archived
         │          ↑
         │     (accumulated_weight ≥ Q_finality, TimeProof complete)
         │
         ├→ FallbackResolution → Finalized / Rejected
         │   (stall > 30s, deterministic leader resolves)
         │
         └→ Rejected
             (conflict lost or invalid)
```

- **Seen**: Transaction received, pending validation
- **Voting**: Actively collecting signed FinalityVotes; tracks `accumulated_weight` and `confidence`
- **FallbackResolution**: Stall detected; deterministic fallback round in progress (tracks round number and alert count)
- **Finalized**: `accumulated_weight ≥ 67%` of AVS weight; TimeProof assembled
- **Rejected**: Lost conflict resolution or deemed invalid
- **Archived**: Included in TimeLock checkpoint block

#### Transaction Pool Details

- **Three-map structure**:
  - `pending` — Transactions in consensus (Seen + Voting states): `DashMap<Hash256, PoolEntry>`
  - `finalized` — Transactions ready for block inclusion: `DashMap<Hash256, PoolEntry>`
  - `rejected` — Previously rejected transactions with reason and timestamp: `DashMap<Hash256, (String, Instant)>`
- Max pool size: 100 MB (configurable)
- Pressure levels: Normal (0–60%), Warning (60–80%), Critical (80–90%), Emergency (90%+)
- Priority scoring: fee rate, age, TX type
- Eviction: lowest priority first when pool is full
- Rejected TX cleanup: after 1 hour

#### UTXO State Machine

Five states (not the typical 2):

- **Unspent**: Available for spending
- **Locked**: Masternode collateral — cannot be spent; created by `MasternodeLock` transaction
- **SpentPending**: Input locked during TimeVote consensus; tracks `txid`, vote counts, `spent_at`
- **SpentFinalized**: Transaction finalized with votes (51% or 67% threshold reached)
- **Archived**: Included in block; final on-chain state

Collateral locking includes a 10-minute timeout cleanup for orphaned locks.

---

### Network Sync and Fork Resolution Flow

#### Sync Flow

1. Node starts → checks current height vs expected height
2. If behind: calls `sync_from_peers(None)`
3. `sync_from_peers()`:
   - Gets connected peers from peer registry
   - Requests blocks from `current_height + 1` up to peer's height
   - Processes blocks sequentially
   - Stops at first missing block (no gap tolerance)
4. **Sync Coordinator** prevents storms:
   - Rate-limits sync requests
   - Tracks active sync operations
   - Prevents duplicate sync to the same height range

#### Fork Resolution Flow

Chain comparison in `compare_chain_with_peers()` (`blockchain.rs`):
1. **Height-first** (primary): longest chain wins
2. **Stake tiebreaker** (same height): higher cumulative `sampling_weight()` wins — Free=1, Bronze=10, Silver=100, Gold=1000
3. **Peer count** (same height + weight): more supporting peers wins
4. **Deterministic hash** (final): lexicographically lower block hash wins

`handle_fork()` decision flow (`blockchain.rs`):
1. Find common ancestor via binary search
2. Security checks: reject genesis reorgs, reject depth > 500 blocks, reject future timestamps
3. Call `fork_resolver.resolve_fork()`: longer chain always wins; same height → stake tiebreaker, then hash
4. If accepted, perform reorg: roll back to ancestor, replay peer chain

Fork alert protocol (`message_handler.rs`):
- When we're ahead: send `ForkAlert` to lagging peers (rate-limited to once per 60 s per peer)
- When peer is ahead and in consensus: request blocks to sync
- On receiving `ForkAlert`: request blocks from consensus chain if behind or hash differs
- Validations: timestamp, merkle root, signatures, chain continuity
- Finalized transaction protection: reject forks that would reverse finalized transactions

---

## TimeProof Conflict Detection

TimeProof conflict detection is a **security monitoring feature** that detects and logs anomalies indicating implementation bugs or Byzantine validator behavior. It does NOT prevent double-spends — that is handled by UTXO locking.

### Key Insight from Protocol Analysis

**By pigeonhole principle**, two transactions spending the same UTXO cannot both reach 67% finality:
- TX-A needs 67% weight = 6700 units (of 10,000 total)
- TX-B needs 67% weight = 6700 units
- Total: 13,400 > 10,000 — mathematically impossible

Therefore, multiple finalized TimeProofs for the same transaction indicates:
1. **UTXO state machine bug** — should have rejected one transaction at the validation layer
2. **Byzantine validator equivocation** — voting for conflicting transactions
3. **Stale proof** — from a network partition that lost consensus

### Data Structures (`src/types.rs`)

```rust
pub struct TimeProofConflictInfo {
    pub txid:                Hash256,
    pub slot_index:          u64,
    pub proof_count:         usize,   // Number of competing proofs
    pub proof_weights:       Vec<u64>,// Weight of each proof
    pub max_weight:          u64,     // Highest weight (winner)
    pub winning_proof_index: usize,   // Index of winning proof
    pub detected_at:         u64,     // Timestamp when detected
    pub resolved:            bool,    // Has conflict been resolved?
}
```

### Core Methods (`src/consensus.rs`)

#### `detect_competing_timeproof(proof: TimeProof, weight: u64) -> Result<usize, String>`
- Called when a new TimeProof is received
- If competing proofs exist → logs anomaly
- Returns index of winning proof (highest weight)
- Updates metrics: `timeproof_conflicts_detected`

#### `resolve_timeproof_fork(txid: Hash256) -> Result<Option<TimeProof>, String>`
- Selects canonical proof (highest accumulated weight)
- Marks conflict as resolved
- Used for partition healing reconciliation

#### `get_competing_timeproofs(txid: Hash256) -> Vec<TimeProof>`
- Retrieves all proofs for a transaction
- Used for security analysis

#### `get_conflict_info(txid: Hash256, slot_index: u64) -> Option<TimeProofConflictInfo>`
- Gets detailed conflict information for AI anomaly detector and monitoring dashboards

#### `conflicts_detected_count() -> usize`
- Metrics counter for security monitoring

### Test Coverage

8 comprehensive tests covering all scenarios:

| Category | Tests |
|----------|-------|
| Normal operation | `test_single_timeproof_no_conflict`, `test_competing_proofs_should_never_happen_normally` |
| Anomaly detection | `test_competing_timeproofs_detected_as_anomaly`, `test_stale_proof_detection_from_partition` |
| Fork resolution | `test_fork_resolution_selects_canonical`, `test_clear_competing_timeproofs_after_investigation` |
| Monitoring & metrics | `test_conflict_metrics_for_monitoring`, `test_conflict_info_for_security_alerts` |

All 8 tests pass.

### Usage

```rust
// When a TimeProof arrives from the network
let winning_idx = consensus.detect_competing_timeproof(proof, weight)?;
if winning_idx != 0 {
    tracing::warn!("Proof replaced - potential partition/Byzantine behavior");
}

// In security monitoring loop
let total_conflicts = consensus.conflicts_detected_count();
if let Some(conflict) = consensus.get_conflict_info(txid, slot_index) {
    alert_security_dashboard(conflict);
}

// After partition healing
let canonical = consensus.resolve_timeproof_fork(txid)?;
```

### Integration Points

| Layer | Role |
|-------|------|
| **Blockchain layer** | When adding finalized TX to block, check for conflicts; log alert and select canonical proof if found |
| **UTXO Manager** | Verify conflicting transactions were rejected at validation layer; conflicting TimeProofs indicate a state machine bug |
| **AI Anomaly Detector** | Feed conflict info to anomaly model; train on weight ratios, vote patterns, validator behavior |
| **Network layer** | Optional `ConflictNotification` message for partition healing coordination; broadcast winning TimeProof |

### Security Properties

| Property | Description |
|----------|-------------|
| **Byzantine detection** | Multiple signatures on conflicting proofs are caught |
| **Deterministic resolution** | Weight-based selection ensures unambiguous canonical outcome |
| **Partition-safe** | Minority partition's proof is marked as stale |
| **Non-blocking** | Node continues operating while investigating |
| **Audit trail** | All conflicts logged with timestamps and weights |

### Performance

- **Detection**: O(1) — constant-time conflict recording
- **Resolution**: O(N) where N = number of competing proofs (typically 2)
- **Memory**: O(N × M) where N = # transactions with conflicts, M = # proofs per transaction
- **Normal case**: Zero overhead (single proof per transaction)

### What This Does NOT Do

- ❌ Prevent double-spends (UTXO locking does that)
- ❌ Handle consensus forks (TimeGuard fallback does that)
- ❌ Blacklist validators (AI anomaly detector does that)
- ❌ Require network coordination (works unilaterally)

### Future Enhancements

1. **Network-wide conflict propagation** — broadcast `ConflictNotification` for coordination
2. **Validator reputation** — feed into Byzantine node detection system
3. **Automated slashing** — slash validators caught equivocating (if slashing is implemented)
4. **Dashboard integration** — real-time security monitoring UI

