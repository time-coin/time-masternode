# Current Block Production System (After Fixes)

## Single Unified Production Loop

**ONE** main loop runs every **10 minutes** and handles ALL block production.

---

## Architecture Overview

```
┌────────────────────────────────────────────────────────────────┐
│                  Main Block Production Loop                     │
│                      (Every 10 minutes)                         │
└────────────────────────────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────┐
        │  Get current_height & expected_height   │
        │  Calculate blocks_behind                │
        └─────────────────────────────────────────┘
                              │
                              ▼
        ┌─────────────────────────────────────────┐
        │  Are we behind schedule?                │
        │  (>2 blocks OR >5min past expected)    │
        └─────────────────────────────────────────┘
                    │                    │
                    │ YES (Catchup)      │ NO (Normal)
                    ▼                    ▼
        ┌───────────────────┐   ┌───────────────────┐
        │  CATCHUP MODE     │   │  NORMAL MODE      │
        └───────────────────┘   └───────────────────┘
```

---

## Mode 1: CATCHUP MODE (When Behind)

**Triggered when:**
- More than 2 blocks behind, OR
- 1+ blocks behind AND >5 minutes past expected time

**Process:**
1. **Sync from peers** first (try to get blocks from network)
2. **60-second cooldown** after sync (allow mempool to populate)
3. **Verify peers** don't have longer chains (15-second wait)
4. **TSDC leader selection** for catchup coordination
   - Deterministic: All nodes agree on same leader
   - Backup leaders if primary times out (60s)
5. **Leader produces blocks** rapidly to catch up
   - Non-leaders wait for leader's blocks
   - Prevents forks (only one producer)

**Safety Checks:**
- ✅ Must have connected peers (prevent isolated forks)
- ✅ 60s cooldown after sync (prevent empty mempool blocks)
- ✅ Verify no peer has longer chain
- ✅ TSDC coordination (prevent competing blocks)

**Leader Selection:**
```rust
// TSDC deterministic leader based on expected_height
block_tsdc.select_leader_for_catchup(attempt, expected_height).await
```

---

## Mode 2: NORMAL MODE (At Expected Height)

**Triggered when:**
- At expected height, OR
- Behind but within 5-minute grace period (waiting for natural production)

**Process:**
1. **Hash-based leader selection** (simple & efficient)
   - Hash of: previous_block_hash + current_height
   - Modulo masternode count
   - Deterministic: All nodes compute same result
2. **Selected producer creates block**
3. **Broadcast to all peers**

**Safety Checks:**
- ✅ Must NOT be >10 blocks behind (NEW - prevents out-of-sync production)
- ✅ Must have valid chain time
- ✅ Must have ≥2 connected peers (3 nodes total)
- ✅ Mutual exclusion lock (prevent double production)

**Leader Selection:**
```rust
// Simple deterministic hash-based selection
let selection_hash = SHA256(prev_block_hash || current_height);
let producer_index = selection_hash % masternode_count;
```

---

## What Got DISABLED

### TSDC Proposal Loop (Separate Task)
**Status:** ❌ DISABLED (commented out)

**Why disabled:**
1. Incomplete implementation (`merkle_root: Hash256::default()`)
2. No sync checks (produced blocks when 1000+ behind)
3. Redundant (main loop already uses TSDC for catchup)
4. Source of 00000 merkle root blocks

**Location:** `src/main.rs` line ~549 (now commented out)

---

## Block Production Safety Matrix

| Scenario | Can Produce? | Method | Notes |
|----------|--------------|--------|-------|
| At height, synced | ✅ YES | Normal (hash-based) | Standard operation |
| 1 block behind, <5min | ✅ YES | Normal (hash-based) | Grace period |
| 1 block behind, >5min | ⚠️ MAYBE | Catchup (TSDC) | If selected as leader |
| 2+ blocks behind | ⚠️ MAYBE | Catchup (TSDC) | If selected as leader |
| >10 blocks behind | ❌ NO | SKIPPED | Must sync first |
| No peers connected | ❌ NO | SKIPPED | Prevent isolated forks |
| Just synced (<60s) | ❌ NO | SKIPPED | Mempool cooldown |
| <3 masternodes | ❌ NO | SKIPPED | Insufficient network |

---

## Current Leader Selection Summary

### Catchup (Behind Schedule)
- **Method:** TSDC deterministic selection
- **Based on:** Expected height (all nodes agree)
- **Coordination:** One leader, others wait
- **Goal:** Fast coordinated catchup

### Normal (At Height)
- **Method:** Hash-based deterministic selection
- **Based on:** Previous block hash + height
- **Coordination:** Implicit (all compute same result)
- **Goal:** Fair rotation, efficient

### TSDC Proposals (Removed)
- **Status:** ❌ Removed (~190 lines deleted)
- **Reason:** Incomplete merkle root implementation (`Hash256::default()`)
- **Can re-implement:** After fixing `src/tsdc.rs` line 544 (if needed)

---

## Key Improvements After Fixes

1. **No more 00000 merkle roots** - 60s cooldown + TSDC loop disabled
2. **No out-of-sync production** - >10 blocks behind = skip
3. **No sync loops** - Detects redundant block requests
4. **Single source of truth** - One unified production system
5. **Proper TSDC usage** - Only for coordinated catchup (its intended purpose)

---

## Code Entry Point

**File:** `src/main.rs`  
**Function:** Block production task (spawned at line ~956)  
**Interval:** 10 minutes (600 seconds)  
**Core Logic:** Lines 1050-1580

```rust
// Simplified structure
let mut interval = tokio::time::interval(Duration::from_secs(600));
loop {
    interval.tick().await;
    
    let blocks_behind = expected_height - current_height;
    
    if should_catchup(blocks_behind) {
        // Catchup mode: TSDC leader selection
        sync_from_peers();
        wait_60s_if_just_synced();
        tsdc_leader = select_leader_for_catchup();
        if we_are_leader {
            produce_catchup_blocks();
        }
    } else {
        // Normal mode: Hash-based selection
        producer = hash_based_selection();
        if we_are_producer && not_too_far_behind {
            produce_block();
        }
    }
}
```

---

## Summary

**Current system = Simple, Safe, Unified**

- **ONE loop** handles everything (no competing systems)
- **TWO modes** based on sync status (catchup vs normal)
- **TWO leader methods** optimized for each mode (TSDC vs hash)
- **ZERO incomplete systems** running in production
- **Multiple safety checks** at every decision point

The block production system is now **predictable, coordinated, and safe**.
