# Phase 3: TSDC Block Production - Roadmap

**Date:** December 23, 2025  
**Status:** Ready to begin

---

## Overview

Phase 3 implements block production using the TSDC protocol:
1. VRF-based leader election for slot leaders
2. Block proposal from finalized transactions
3. Prepare phase validation by peers
4. Block finalization and checkpointing

---

## Current State

✅ **Prerequisites Complete:**
- Phase 1: AVS snapshots for validator tracking
- Phase 2: Voting infrastructure and vote finality
- Transaction finalization via Avalanche

**Ready to implement:**
- Block production pipeline
- TSDC state machine
- Leader election

---

## Phase 3 Tasks

### 3a: Slot Clock & Leader Election
**Goal:** Implement VRF-based leader selection

**Files to modify:**
- `src/tsdc.rs` - select_leader() implementation
- `src/consensus.rs` - Add slot tracking

**What needs to happen:**
1. Track current slot number (slot = time / slot_duration)
2. Calculate slot_seed = hash(slot_index + validator_id)
3. Run VRF with seed
4. Check if VRF_output < threshold (chance based on stake)
5. If leader: proceed to block proposal

**Output:**
- Slot tracking in ConsensusEngine
- Leader detection per slot
- Ready for block proposal

---

### 3b: Block Proposal
**Goal:** Selected leader proposes blocks

**Files to modify:**
- `src/tsdc.rs` - block_propose()
- `src/consensus.rs` - Block assembly logic

**What needs to happen:**
1. Get finalized transactions from finalized pool
2. Assemble block with:
   - Height (chain tip + 1)
   - Timestamp (current time)
   - Proposer address (our address)
   - Transactions (up to block size limit)
   - Parent hash (previous block)
3. Sign block with validator's key
4. Broadcast TSCDBlockProposal to all peers

**Output:**
- Block assembly from finalized transactions
- Network broadcasting
- Peer reception handling

---

### 3c: Prepare Phase
**Goal:** Validators validate and prepare blocks

**Files to modify:**
- `src/tsdc.rs` - validate_prepare()
- `src/network/server.rs` - Handle TSCDBlockProposal

**What needs to happen:**
1. Receive block proposal from leader
2. Validate:
   - Block signature
   - Transactions in block
   - Height sequence
   - Timestamp validity
3. Send PrepareVote message
4. Accumulate prepare votes

**Output:**
- Prepare phase validation
- Vote collection for consensus
- Block acceptance or rejection

---

### 3d: Precommit Phase
**Goal:** Reach consensus and commit blocks

**Files to modify:**
- `src/tsdc.rs` - on_precommit()
- Network message handling

**What needs to happen:**
1. After 2/3 prepare votes
2. Send PrecommitVote
3. Reach 2/3 precommit consensus
4. Finalize block

**Output:**
- Block finalized to chain
- Deterministic finality
- Ready for checkpoint

---

### 3e: Finality & Checkpointing
**Goal:** Checkpoint blocks into chain

**Files to modify:**
- `src/blockchain.rs` - Add block finalization
- `src/tsdc.rs` - Finality logic

**What needs to happen:**
1. After block consensus reached
2. Mark block as finalized
3. Update chain tip
4. Emit block events
5. Start next slot

**Output:**
- Blocks added to chain
- Deterministic history
- Ready for Phase 4

---

## Implementation Order

1. **3a - Slot Clock** (Prerequisite for others)
   - Enables leader election
   - Time-based consensus
   - ~200 lines

2. **3b - Block Proposal** (Uses 3a)
   - Leader can produce blocks
   - Network dissemination
   - ~150 lines

3. **3c - Prepare Phase** (Uses 3b)
   - Validator consensus
   - Vote collection
   - ~200 lines

4. **3d - Precommit Phase** (Uses 3c)
   - Final consensus round
   - Block commitment
   - ~150 lines

5. **3e - Finality** (Uses 3d)
   - Add to chain
   - Complete TSDC cycle
   - ~100 lines

**Total:** ~800 lines of new implementation

---

## Key Integration Points

### From Phase 2:
- AVS snapshots for validator lists
- FinalityVote voting infrastructure
- Vote accumulation mechanisms

### New Message Types:
```rust
TSCDBlockProposal {
    block: Block,
}

PrepareVote {
    block_hash: [u8; 32],
    voter: String,
}

PrecommitVote {
    block_hash: [u8; 32],
    voter: String,
}
```

### Network Server Integration:
- Add handlers for block proposals
- Collect prepare votes
- Collect precommit votes
- Emit events to consensus engine

---

## Success Criteria

- [ ] Slot clock implementation
- [ ] Leader election working
- [ ] Blocks proposed and broadcast
- [ ] Prepare phase consensus
- [ ] Precommit phase consensus
- [ ] Blocks added to chain
- [ ] All compiles with zero errors
- [ ] cargo fmt && cargo clippy pass

---

## Estimated Completion

- **3a - Slot Clock:** ~1-2 hours
- **3b - Block Proposal:** ~1-2 hours
- **3c - Prepare Phase:** ~1-2 hours
- **3d - Precommit Phase:** ~1 hour
- **3e - Finality:** ~1 hour

**Total estimate:** 5-8 hours

---

## Risk Mitigation

- ✅ Small incremental changes per sub-phase
- ✅ Frequent compilation checks
- ✅ Each phase builds on proven infrastructure
- ✅ Network layer already proven with voting
- ✅ Block structure already defined

---

## Dependencies

- ✅ Avalanche consensus (Phase 2)
- ✅ Network infrastructure (Phase 1-2)
- ✅ UTXO management (existing)
- ✅ Block structure (existing)
- ✅ Transaction signing (existing)

---

## Notes

- TSDC is checkpointing layer, not primary consensus
- Primary consensus is Avalanche (fast finality)
- TSDC adds deterministic history
- No breaking changes expected
- All code should compile immediately after each sub-phase

