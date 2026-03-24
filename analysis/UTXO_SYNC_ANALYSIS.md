# TIME Coin UTXO Synchronization & Consistency Mechanisms

## 1. UTXO MANAGER (src/utxo_manager.rs)

### Architecture
The UTXOStateManager is the central hub for UTXO state tracking:

**Structure:**
- Storage backend: Arc<dyn UtxoStorage> (pluggable, supports in-memory or persistent)
- State tracking: DashMap<OutPoint, UTXOState> (lock-free, concurrent)
- Collateral locks: DashMap<OutPoint, LockedCollateral> (separate from UTXO state)
- Collateral DB: Option<sled::Tree> (persistent disk storage)
- Address index: DashMap<String, DashSet<OutPoint>> (fast address lookups)

**Pre-allocation:** 100k UTXO capacity to reduce rehashing

### UTXO State Enum
\\\ust
pub enum UTXOState {
    Unspent,  // Available to spend
    Locked {
        txid: Hash256,      // Locking transaction
        locked_at: i64,     // Timestamp (600s timeout)
    },
    SpentPending {
        txid: Hash256,      // Spending transaction
        votes: u32,         // TimeVote consensus votes
        total_nodes: u32,   // Total eligible voters
        spent_at: i64,
    },
    SpentFinalized {
        txid: Hash256,
        finalized_at: i64,
        votes: u32,
    },
    Archived {
        txid: Hash256,
        block_height: u64,  // Block where spent
        archived_at: i64,
    },
}
\\\

### Core Methods

#### UTXO Lifecycle
- **add_utxo()** — Add new UTXO, mark Unspent, update address index
- **remove_utxo()** — Remove UTXO and clean address index
- **spend_utxo()** — Mark as SpentFinalized (called during block processing)
- **restore_utxo()** — Restore UTXO during rollback (forces Unspent state)

#### Locking (Per-Transaction)
- **lock_utxo(outpoint, txid)** — Lock for pending tx (10-minute timeout)
- **unlock_utxo(outpoint, txid)** — Unlock failed transaction
- **lock_utxos_atomic(outpoints, txid)** — All-or-nothing multi-UTXO lock
- **cleanup_expired_locks()** — Remove expired locks (>600 seconds old)
- **is_spendable(outpoint, by_txid?)** — Check if can be spent

#### State Synchronization
- **calculate_utxo_set_hash()** — SHA256 of sorted UTXO set for peer comparison
- **get_utxo_diff(remote_utxos)** — Returns (to_remove, to_add) for reconciliation
- **reconcile_utxo_state(to_remove, to_add)** — Apply diffs to local state
- **update_state(outpoint, state)** — Network state update (from peers)

#### Collateral Operations
- **lock_collateral()** — Lock UTXO as masternode collateral (persisted to disk)
- **unlock_collateral()** — Release collateral lock
- **is_collateral_locked()** — Check if UTXO is locked as collateral
- **list_locked_collaterals()** — Get all collateral locks
- **rebuild_collateral_locks()** — Restore locks from masternode registry (on startup)

#### Initialization
- **initialize_states()** — Load UTXOs from storage into state map
- **clear_all()** — Wipe all UTXOs (used during reindex)
- **enable_collateral_persistence(db)** — Enable disk persistence for collateral

---

## 2. NETWORK SYNC MESSAGES (src/network/message.rs)

The following NetworkMessage variants handle UTXO synchronization:

### State Query & Response
\\\ust
UTXOStateQuery(Vec<OutPoint>)
  → Request state of specific UTXOs
  ← UTXOStateResponse(Vec<(OutPoint, UTXOState)>)
\\\

### Direct State Update (CRITICAL for instant finality)
\\\ust
UTXOStateUpdate { outpoint: OutPoint, state: UTXOState }
  → Update UTXO state without verification (fire-and-forget)
  ← No response
\\`\

### Set Hashing (Consistency Check)
\\\ust
GetUTXOStateHash
  → Request hash of peer's UTXO set
  ← UTXOStateHashResponse { hash, height, utxo_count }
\\`\

### Full Set Sync (Recovery)
\\\ust
GetUTXOSet
  → Request entire UTXO set
  ← UTXOSetResponse(Vec<UTXO>)
\\`\

### Deprecated/Related
- UTXOStateNotification — Notify of state change (unused)
- GetBlocks/BlocksResponse — Sync blocks (includes UTXO changes)
- ChainTipResponse — Peer advertises height for opportunistic sync

---

## 3. MESSAGE HANDLING (src/network/message_handler.rs)

### Handler Functions for UTXO Messages

**handle_utxo_state_query()** (Line 2686-2713)
- Receives list of OutPoints
- Returns state of each UTXO from get_state()
- Response: UTXOStateResponse

**handle_get_utxo_state_hash()** (Line 2716-2733)
- Calls blockchain.get_utxo_state_hash() → utxo_manager.calculate_utxo_set_hash()
- Returns: { hash, height, utxo_count }
- Use: Peers detect divergence without full set sync

**handle_get_utxo_set()** (Line 2736-2748)
- Returns entire UTXO set via blockchain.get_all_utxos()
- Large message (10s+ MB); used for critical state divergence
- Response: UTXOSetResponse

**handle_utxo_state_update()** (Line 4043-4085) — CRITICAL
- Receives state update from peer
- Calls: consensus.utxo_manager.update_state()
- Logs transitions: Locked, SpentPending, SpentFinalized
- Fire-and-forget: No response sent
- **Purpose**: Allows instant finality between blocks

---

## 4. BLOCKCHAIN INTEGRATION (src/blockchain.rs)

### Main UTXO Processing: process_block_utxos()

**Location**: Line 4495-4588

**Purpose**: Atomically update UTXO state when block is added

**Flow**:
1. Create UndoLog with block hash and height
2. For each transaction in block:
   - **Spend inputs**: utxo_manager.spend_utxo() for each input
     - Saves UTXO to undo log (for rollback)
     - Marks state SpentFinalized
     - Removes from address index
   - **Create outputs**: Create new UTXO for each output
     - Call utxo_manager.add_utxo()
     - Mark state Unspent
     - Update address index
3. Save UndoLog to disk

**Log Output**:
\\\
💰 Block N indexed M UTXOs (C created, S spent, U in undo log)
\\`\

### Block Addition: add_block()

**Location**: Line 3507+

**Key Steps**:
1. Sanitize block data (clear oversized script_sig/pubkey)
2. Verify chain integrity:
   - No zero previous_hash (non-genesis)
   - previous_hash matches actual previous block hash
   - Previous block must exist (no gaps during sync)
3. Acquire block_processing_lock (serializes UTXO updates)
4. Call process_block_utxos()
5. Save undo log
6. Update blockchain state

### Full Reindex: reindex_utxos()

**Location**: Line 425-504

**When Used**: Chain corruption, operator request, recovery

**Flow**:
1. utxo_manager.clear_all() — wipe all UTXOs and states
2. For each block 0..current_height:
   - Fetch block
   - process_block_utxos()
   - Save undo log
3. Rebuild treasury balance
4. Log: "✅ UTXO reindex complete: N blocks, M UTXOs"

### State Verification Methods

**get_utxo_state_hash()** (Line 4102-4104)
- Returns SHA256 of sorted UTXO set
- Sorting: (txid, vout)
- Used: Peer consistency verification

**get_utxo_count()** (Line 4107-4109)
- Count of all unspent UTXOs

**get_all_utxos()** (Line 4112-4114)
- Full UTXO set (for network sync)

---

## 5. CONSENSUS ENGINE (src/consensus.rs)

### UTXO Integration

ConsensusEngine holds Arc<UTXOStateManager> for transaction validation:

**validate_transaction_utxos()**:
- For each input, check utxo_manager.get_state()
- Valid states: Unspent, Locked (same txid), SpentPending/Finalized (same txid)
- Invalid: Already spent by different tx, not found, locked by different tx

**Consistency Checks**:
- Check is_collateral_locked() before spending
- Verify signature matches UTXO script_pubkey (address)
- Prevent double-spend (lock check)

### State Transitions During Consensus
- Transaction proposed: UTXO locked
- TimeVote consensus: UTXO SpentPending (accumulate votes)
- Finality reached: UTXO SpentFinalized
- Block inclusion: UTXO Archived (with block height)

---

## 6. TYPES (src/types.rs)

### OutPoint (Transaction Reference)
\\\ust
pub struct OutPoint {
    pub txid: Hash256,  // 32-byte transaction ID
    pub vout: u32,      // Output index (0-based)
}
\\`\

### UTXO (Unspent Output)
\\\ust
pub struct UTXO {
    pub outpoint: OutPoint,
    pub value: u64,                // Satoshis
    pub script_pubkey: Vec<u8>,    // Stores address as UTF-8 bytes
    pub address: String,           // Recipient address
}
\\`\

### LockedCollateral (Masternode Collateral)
\\\ust
pub struct LockedCollateral {
    pub outpoint: OutPoint,
    pub masternode_address: String,
    pub lock_height: u64,          // Block when locked
    pub locked_at: u64,            // Unix timestamp
    pub unlock_height: Option<u64>, // Optional unlock block
    pub amount: u64,               // Satoshis
}
\\`\

---

## 7. INITIALIZATION (src/main.rs)

### Startup Flow

1. **Load Config** (lines 100-288)
   - Parse time.conf or legacy TOML
   - Load masternode.conf (collateral UTXO)

2. **Create UTXOStateManager**
   \\\ust
   let utxo_manager = Arc::new(UTXOStateManager::new());
   utxo_manager.initialize_states().await?;
   \\`\

3. **Enable Collateral Persistence**
   \\\ust
   if let Some(db) = sled_db {
       utxo_manager.enable_collateral_persistence(&db)?;
       utxo_manager.load_persisted_collateral_locks();
   }
   \\`\

4. **Rebuild Collateral Locks**
   - Restore in-memory locks from masternode registry
   - Call: utxo_manager.rebuild_collateral_locks(registry_entries)

### Periodic Tasks (SyncCoordinator)

**Location**: src/network/sync_coordinator.rs

**Configuration**:
- SYNC_THROTTLE_DURATION = 60 seconds (max 1 sync per peer)
- MAX_CONCURRENT_SYNCS = 1 (serial, prevents resource exhaustion)

**Sync Sources**:
- Periodic: Blockchain periodic comparison
- Opportunistic: From ChainTipResponse
- ForkResolution: During fork detection
- Manual: Explicit request

---

## 8. CONSISTENCY MECHANISMS

### 1. Atomic Concurrent Updates (DashMap)
- Lock-free updates prevent race conditions
- All state changes are immediately visible across threads

### 2. Undo Logs (Block Rollback)
- Save spent UTXOs when block is added
- On rollback: restore to Unspent state
- Enable safe chain reorganization

### 3. State Hash Verification (Peer Sync)
- SHA256 of sorted UTXO set
- Peers exchange hashes to detect divergence
- Triggers full UTXO set sync if mismatch

### 4. Lock Timeout (10 minutes = 600 seconds)
- Prevent indefinite locking on crashed transactions
- Allow UTXO reuse after timeout

### 5. Collateral Lock Persistence (Sled)
- Survive node restart
- Separate from UTXO state (resistant to reindex)
- Prevent accidental collateral unlock on reboot

### 6. Address Indexing
- O(1) address → OutPoint lookup
- Updated atomically with UTXO changes
- Fast queries like list_utxos_by_address()

### 7. Sync Coordination
- Max 1 concurrent sync (prevents resource exhaustion)
- 60-second throttle per peer (prevents sync storms)
- Queue-based request management

### 8. Block Processing Serialization
- block_processing_lock ensures UTXO updates are serial
- Prevents overlapping sync batches from corrupting state
- Critical for concurrent block acceptance

---

## 9. STATE MACHINE DIAGRAM

\\\
                    Unspent
                      |
          ┌───────────┼───────────┐
          |           |           |
       lock()    spend_in_block()  |
          |           |           |
          v           v           |
        Locked ──→ SpentFinalized  |
          |           ↑           |
      timeout()       |           |
          |         finalize()    |
          └───────────┴───────────┘
                      |
                      v
                   Archived
\\`\

---

## 10. COMPLETE API REFERENCE

**Construction**:
- new() / new_with_storage() / enable_collateral_persistence()

**Initialization**:
- initialize_states() / clear_all()

**UTXO Operations**:
- add_utxo() / remove_utxo() / get_utxo() / spend_utxo() / restore_utxo()

**Locking**:
- lock_utxo() / unlock_utxo() / commit_spend() / lock_utxos_atomic() / cleanup_expired_locks()

**Queries**:
- get_state() / update_state() / force_unlock() / get_locked_utxos() / is_spendable()

**Sync/Consistency**:
- calculate_utxo_set_hash() / get_utxo_diff() / reconcile_utxo_state()

**Collateral**:
- lock_collateral() / unlock_collateral() / is_collateral_locked() / list_locked_collaterals() / rebuild_collateral_locks()

**Listing**:
- list_all_utxos() / list_utxos_by_address() / get_finalized_transactions()

---

## 11. FILE REFERENCE

| File | Role |
|------|------|
| src/utxo_manager.rs | UTXO state management (950+ lines) |
| src/types.rs | UTXOState, UTXO, LockedCollateral types |
| src/network/message.rs | Network message definitions |
| src/network/message_handler.rs | UTXO message handlers (4200+ lines) |
| src/blockchain.rs | Block processing, UTXO updates (6200+ lines) |
| src/consensus.rs | Transaction validation w/ UTXO checks |
| src/network/sync_coordinator.rs | Sync throttling & coordination |
| src/main.rs | Initialization & startup |

