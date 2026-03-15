# CRITICAL DEADLOCK ANALYSIS - TIME Coin Daemon at Block ~12435

## DEADLOCK #1: CRITICAL - Fork State Write Lock Held During Await

**Location:** C:\Users\wmcor\projects\time-masternode\src\blockchain.rs:8532-8589

**Problem:** The fork_state.write() lock is held across multiple async operations:

\\\ust
// Line 8532: ACQUIRE fork_state.write()
*self.fork_state.write().await = ForkResolutionState::Reorging { ... };

// Line 8539: AWAIT while holding fork_state.write()
self.rollback_to_height(common_ancestor).await?;

// Inside rollback_to_height (lines 5861, 5874, 5890):
// Still holding fork_state.write() lock!
self.utxo_manager.restore_utxo(utxo).await { ... }
self.get_block_by_height(height).await { ... }
self.utxo_manager.remove_utxo(&outpoint).await { ... }

// Lines 8587-8589: More awaits while lock held
self.add_block(block.clone()).await?;
\\\

**Why It Deadlocks:**
1. Thread A: In perform_reorg(), holds fork_state.write(), then awaits add_block()
2. add_block() tries to acquire block_processing_lock at line 3529
3. Meanwhile Thread B: In add_block_with_fork_handling(), holds block_processing_lock
4. Thread B tries to read fork_state at line 6228 (blocked by Thread A's write lock)
5. **DEADLOCK**: Thread A waits for Thread B to release block_processing_lock, but Thread B waits for Thread A to release fork_state

---

## DEADLOCK #2: Lock Ordering Violation

**Location:** C:\Users\wmcor\projects\time-masternode\src\blockchain.rs:6223-6228

**Problem:**
\\\ust
let _block_guard = self.block_processing_lock.lock().await;
let fork_state = self.fork_state.read().await;
\\\

**Lock Order A (add_block_with_fork_handling):**
- Acquire: block_processing_lock
- Acquire: fork_state (read)

**Lock Order B (perform_reorg):**
- Acquire: fork_state (write)
- Try to Acquire: block_processing_lock (via add_block call)

**Result:** Classic ABBA deadlock when lock orders conflict.

---

## DEADLOCK #3: Nested Lock Write

**Location:** C:\Users\wmcor\projects\time-masternode\src\blockchain.rs:8452-8455

**Problem:**
\\\ust
*self.fork_state.write().await = ForkResolutionState::None;
self.consensus_peers.write().await.clear();  // Nested write lock
\\\

While fork_state is being written to, also writing consensus_peers. This creates additional lock contention.

---

## ROOT CAUSE CHAIN

1. Sync coordinator (every 10s) triggers block sync from peer 69.167.168.176
2. New blocks arrive, triggering fork detection at height ~12435
3. handle_fork() spawns background task to perform reorg
4. perform_reorg() acquires fork_state.write() at line 8532
5. While holding lock, calls self.rollback_to_height().await at line 8539
6. rollback_to_height() makes UTXO manager calls with await
7. Meanwhile, network layer receives more blocks
8. Message handler calls add_block_with_fork_handling()
9. This acquires block_processing_lock then tries to read fork_state
10. Fork state is still held by step 5 thread
11. Deadlock occurs
12. RPC getblockchaininfo hangs trying to access consensus engine state
13. **Node becomes unresponsive**

---

## WHY LOGS STOP AFTER BLOCK 12435

Once deadlock occurs:
- All block processing threads are stuck
- Logging might be buffered (tokio runtime blocked)
- New RPC requests timeout
- Network I/O may be blocked by thread contention
- Process appears alive but completely unresponsive

---

## SPECIFIC DEADLOCK EVIDENCE

From the code flow:
1. File: blockchain.rs, function handle_fork() → perform_reorg()
2. Line 8532: Fork_state.write().await ← LOCK ACQUIRED
3. Lines 5861, 5874, 5890: Inside rollback_to_height() await calls ← WHILE HOLDING LOCK
4. Line 8539: self.rollback_to_height(common_ancestor).await? ← AWAIT WHILE LOCKED  
5. Lines 8587-8589: self.add_block(block.clone()).await? ← AWAIT WHILE LOCKED
6. Inside add_block() at line 6200: add_block_with_fork_handling() is called
7. Line 6223: block_processing_lock.lock().await ← TRIES TO ACQUIRE
8. Line 6228: fork_state.read().await ← But fork_state is already write-locked!

**THIS IS THE DEADLOCK**

---

## FIX STRATEGY

**Option 1: Don't hold fork_state.write() across await calls**
- Release fork_state lock BEFORE calling rollback_to_height()
- Release fork_state lock BEFORE calling add_block()
- Use local variables to track state instead of holding lock across awaits

**Option 2: Use parking_lot::Mutex instead of tokio::sync::Mutex/RwLock**
- parking_lot locks are synchronous, shorter hold time
- But this requires restructuring to avoid blocking the async runtime

**Option 3: Enforce consistent lock ordering**
- Always acquire fork_state BEFORE block_processing_lock
- Or never acquire both locks in the same context

**Recommended: Option 1**
- Most straightforward fix
- Minimal architectural change
- Preserves async nature of the code

---

## PROOF: Lock Structures in Code

Fork state definition (line 206):
\\\ust
pub fork_state: Arc<RwLock<ForkResolutionState>>,
\\\

Block processing lock definition (line 211):
\\\ust
block_processing_lock: Arc<tokio::sync::Mutex<()>>,
\\\

Both are tokio async locks, susceptible to deadlock if held across await points.

