# TIME Coin Protocol v6 Implementation Gap Analysis

**Date:** 2025-12-23  
**Status:** Gap Analysis (comparing spec to current code)

---

## Summary

TIME Coin v6 requires 3 main components:
1. **Avalanche Snowball** (transaction finality via stake-weighted sampling)
2. **Verifiable Finality Proofs** (VFP - signed votes proving finality)
3. **TSDC Checkpoints** (archival blocks every 600s with rewards)

Current implementation has **skeleton code** for all three, but they are **disconnected**. The RPC path does not trigger consensus.

---

## Critical Gaps

### 1. AVS (Active Validator Set) - NOT FULLY IMPLEMENTED
**Protocol requires (§5.4):**
- Heartbeat tracking with expiration (`HEARTBEAT_TTL = 180s`)
- Witness attestations (`WITNESS_MIN = 3` per heartbeat)
- Only heartbeat + witness-attested nodes are in AVS
- AVS snapshots retained by slot for VFP validation

**Current code:**
- ❌ `heartbeat_attestation.rs` - has struct but no actual heartbeat/attestation logic
- ❌ `masternode_registry.rs` - no AVS filtering based on heartbeat status
- ❌ No slot-based AVS snapshots (needed to validate VFP voters)

**Impact:** Cannot determine which masternodes can vote; VFP validation will fail

---

### 2. Avalanche Snowball Polling - MISSING INTEGRATION
**Protocol requires (§7):**
- `SampleQuery` sent to `k=20` randomly-sampled AVS masternodes (stake-weighted)
- Responder votes `Valid/Invalid/Unknown` based on local state
- Accumulate `confidence[X]` on successful polls
- Update `preferred_txid[o]` per outpoint using counter majority

**Current code:**
- ✅ `consensus.rs` - has Snowflake/Snowball/QueryRound structs
- ❌ **Never called** - no trigger from RPC to start polling
- ❌ `execute_query_round()` is not wired to actual network
- ❌ No handler for incoming `SampleQuery` messages
- ❌ Responder voting logic not implemented

**Impact:** Transactions never get sampled; no consensus possible

---

### 3. Finality Votes & VFP Assembly - NOT IMPLEMENTED
**Protocol requires (§8):**
- During valid poll responses, include signed `FinalityVote`
- Vote signed by validator with timestamp (`slot_index`)
- Accumulate votes until threshold `Q_finality = 0.67 * total_weight`
- Assemble into `VFP(txid) = { tx, slot_index, votes[] }`
- Mark transaction `GloballyFinalized` once VFP is valid

**Current code:**
- ❌ No `FinalityVote` struct or signing
- ❌ No VFP accumulation logic
- ❌ No VFP validation (check threshold, voter eligibility, etc.)
- ❌ No transition to `GloballyFinalized` state

**Impact:** No objective finality; transactions can never be committed to checkpoint blocks

---

### 4. TSDC Block Production - STUB ONLY
**Protocol requires (§9):**
- Every 600s, produce a candidate block for the slot
- VRF-sortition: score = VRF(prev_hash || slot_time || chain_id)
- Nodes select canonical block with lowest VRF score
- Block MUST include entries (txid + vfp_hash) for all `GloballyFinalized` txs
- Apply rewards: 10% producer, 90% AVS-weighted split

**Current code:**
- ✅ `tsdc.rs` - has `TSCDConsensus` struct with `finalize_block()`
- ❌ **Never called** - no slot timer or block production loop
- ❌ VRF scoring not implemented
- ❌ No block header assembly with `finalized_root` Merkle tree
- ❌ Reward calculation and payout not implemented

**Impact:** Checkpoint chain never advances; history not archival

---

### 5. Transaction State Lifecycle - PARTIALLY IMPLEMENTED
**Protocol requires (§6):**
UTXO states: `Unspent → Locked → Spent` or `Unspent → Locked → Locally Accepted → Globally Finalized → Archived`

**Current code:**
- ✅ `transaction_pool.rs` - tracks pools (Seen, Sampling, LocallyAccepted, Finalized, Archived)
- ❌ State transitions not triggered by consensus or finality
- ❌ Conflict sets per outpoint not managed
- ❌ Tie-breaking rule (lowest txid) not enforced

**Impact:** Transactions stuck in wrong states; double-spends not prevented

---

### 6. Network Message Handlers - INCOMPLETE
**Protocol requires (§11.1):**
- `SampleQuery` → query masternodes, return `SampleResponse` with votes
- `SampleResponse` → accumulate votes in Avalanche rounds
- `VfpGossip` → validate and store VFP proofs
- `BlockBroadcast` → validate TSDC blocks, archive transactions
- `Heartbeat` / `Attestation` → update AVS membership

**Current code:**
- ✅ `network/message.rs` - defines message types
- ❌ No handler for `SampleQuery` (responder voting)
- ❌ No handler for `SampleResponse` (vote accumulation)
- ❌ No handler for `VfpGossip`
- ❌ No handler for `Heartbeat` / `Attestation`
- ✅ `BlockBroadcast` handler exists but doesn't trigger reward distribution

**Impact:** Network is passive; no consensus participation

---

### 7. Configuration & Defaults - MISSING
**Protocol requires (§14):**
- `BLOCK_INTERVAL = 600s`
- `AVALANCHE_K = 20`
- `AVALANCHE_ALPHA = 14`
- `AVALANCHE_BETA_LOCAL = 20`
- `POLL_TIMEOUT = 200ms`
- `HEARTBEAT_PERIOD = 60s`
- `HEARTBEAT_TTL = 180s`
- Plus many more...

**Current code:**
- ⚠️ Hardcoded in various files; no centralized config
- ❌ No validation that all nodes use same values

**Impact:** Nodes may disagree on parameters; consensus failures

---

## Implementation Priority (MUST-HAVE order)

1. **AVS Management** (gates everything else)
   - Implement heartbeat generation and propagation
   - Implement witness attestation logic
   - Filter masternode registry to AVS-active only
   - Add slot-based AVS snapshots

2. **Avalanche RPC Integration** (connects consensus to chain)
   - Wire `send_raw_transaction` → start Avalanche sampling
   - Implement `SampleQuery` handler (responder voting)
   - Implement `SampleResponse` handler (vote accumulation)
   - Trigger local acceptance when `β_local` reached

3. **Finality Votes & VFP** (objective finality proof)
   - Add `FinalityVote` struct and signing
   - Add VFP accumulation logic
   - Validate VFP threshold and voter eligibility
   - Trigger `GloballyFinalized` state transition

4. **TSDC Block Production** (checkpointing)
   - Add slot timer that fires every 600s
   - Implement VRF sortition and canonical selection
   - Implement reward calculation per §10
   - Wire block production to include `GloballyFinalized` txs

5. **Centralize Configuration**
   - Move all protocol constants to `config.toml`
   - Validate at startup that all values match protocol

---

## Recommendation

**Do NOT remove dead code yet.** These are core protocol components waiting to be connected. The task is to:
1. Connect the pieces (RPC → Consensus → Finality → Blocks)
2. Implement missing handlers
3. Add slot timers and background loops
4. Test end-to-end from tx broadcast to checkpoint block

Once working, the code can be evaluated for cleanup.
