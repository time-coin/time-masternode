# TIME Coin Protocol Implementation Gaps

## Current Status
- ✅ Core types and transaction structure
- ✅ VFP (VerifiableFinality) types added to types.rs
- ✅ Network layer (P2P messaging)
- ✅ Masternode registry with heartbeats
- ✅ TSDC checkpoint block structure
- ❌ **VFP Integration into Avalanche** - CRITICAL
- ❌ **AVS Snapshots** - Required for VFP validation
- ❌ **Finality Vote Requests** - Missing from query protocol
- ❌ **Global Finalization State Machine** - Not fully integrated

---

## Priority 1: Finality Vote Integration (CRITICAL PATH)

### 1.1 AVS Snapshots (§8.4)
**Current State:** Not implemented  
**Required:**
- Store AVS snapshots per slot_index
- Retain for `ASS_SNAPSHOT_RETENTION` slots (default 100)
- Include: `(mn_id, pubkey, weight)`

**Files to Modify:**
- `src/consensus.rs` - Add snapshot storage to `AvalancheConsensus`
- `src/types.rs` - Add `AVSSnapshot` struct

### 1.2 Query Response Enhancement (§8.5)
**Current State:** Responders return `Valid/Invalid/Unknown` only  
**Required:**
- Responders SHOULD include `FinalityVote` when responding `Valid` 
- Only if responder is AVS-active in the slot

**Files to Modify:**
- `src/network/message.rs` - Add `FinalityVote` to `VoteResponse`
- `src/consensus.rs` - Generate votes on valid responses

### 1.3 VFP Assembly (§8.5)
**Current State:** Not implemented  
**Required:**
- Accumulate votes during sampling
- Verify votes meet threshold (67% of AVS weight)
- Set tx status to `GloballyFinalized` when threshold met

**Files to Modify:**
- `src/consensus.rs` - Add `vfp_accumulator` to track pending votes
- `src/consensus.rs` - Add `check_vfp_finality()` after poll completion

---

## Priority 2: Block Integration

### 2.1 Finalized Transaction Archival (§9.6)
**Current State:** Blocks know about finalized txs but don't enforce it  
**Required:**
- Accept block only if all entries are `GloballyFinalized` by VFP
- Transition tx status from `GloballyFinalized` → `Archived`

**Files to Modify:**
- `src/blockchain.rs` - Verify VFP before accepting block entry

### 2.2 TSDC Checkpoint Loop (§9)
**Current State:** Checkpoint blocks generated but not persisted as canonical chain  
**Required:**
- Implement fork choice: select lowest VRF score per slot
- Persist canonical checkpoint chain

**Files to Modify:**
- `src/blockchain.rs` - Implement fork choice rule
- `src/tsdc.rs` - Integrate with finalized blocks

---

## Priority 3: Consensus State Machine

### 3.1 Transaction Status Tracking (§7.3)
**Current State:** Status enum exists but not fully implemented  
**Required:**
- `Seen` → `Sampling` → `LocallyAccepted` → `GloballyFinalized` → `Archived`
- Proper conflict detection and preference updates

**Files to Modify:**
- `src/consensus.rs` - Implement full state machine
- `src/transaction_pool.rs` - Track per-tx status

### 3.2 Conflict Set Tracking (§7.3)
**Current State:** Partial  
**Required:**
- For each outpoint, track all spending txids (conflict set)
- Ensure only one per outpoint can be finalized

**Files to Modify:**
- `src/transaction_pool.rs` - Add conflict set management

---

## Implementation Order
1. **AVS Snapshots** (enables vote verification)
2. **Finality Vote Generation** (nodes create votes on valid samples)
3. **VFP Assembly** (accumulate votes, set GloballyFinalized)
4. **Block Integration** (verify VFP before archival)
5. **State Machine** (clean up status tracking)
6. **Conflict Sets** (ensure single finality per outpoint)

---

## Test Coverage Gaps
- No tests for VFP validation
- No tests for AVS snapshot retention
- No tests for vote accumulation
- No tests for block finality rules

**Need to add:** `src/tests/vfp_tests.rs`
