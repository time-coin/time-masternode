# TimeCoin Consensus Improvement Proposal

**Date:** 2026-01-17  
**Status:** ✅ IMPLEMENTED (January 2026)  
**Author:** System Analysis

---

## ✅ Implementation Status

This proposal has been **fully implemented** in the TimeCoin protocol as of January 2026. The documentation (TIMECOIN_PROTOCOL.md, ARCHITECTURE_OVERVIEW.md, QUICK_REFERENCE.md) has been updated to reflect the unified TimeVote consensus model.

### Key Changes Implemented:
1. ✅ Single finality state (`Finalized`) replacing two-tier system
2. ✅ Progressive TimeProof assembly during voting rounds
3. ✅ Immediate vote signing with every positive response
4. ✅ Unified threshold: Q_finality = 67% of AVS weight
5. ✅ State machine: `Seen → Voting → Finalized`
6. ✅ Removed `β_local` parameter (no longer needed)

---

## Original Problem Statement

Previous TimeCoin protocol had **two-tier consensus**:

1. **LocallyAccepted** (β_local = 20 rounds)
   - Probabilistic
   - Node-specific
   - Not verifiable by others
   - Creates UX confusion ("confirmed" vs "finalized")

2. **GloballyFinalized** (Q_finality = 67% weight with TimeProof)
   - Objective
   - Verifiable by anyone
   - Requires separate TimeProof assembly
   - Adds latency and complexity

### Issues with Previous Model

1. **Two definitions of "finality"** - confusing for users and developers ✅ FIXED
2. **Delayed TimeProof assembly** - TimeProof was assembled *after* LocallyAccepted ✅ FIXED
3. **Non-verifiable local state** - LocallyAccepted could not be proven to others ✅ FIXED
4. **Wasted work** - Nodes did sampling rounds without producing verifiable artifacts ✅ FIXED
5. **UX ambiguity** - Wallets had to choose between fast (unsafe) vs slow (safe) confirmation ✅ FIXED

---

## ✅ Implemented Solution: Unified TimeVote Consensus

### Core Concept (NOW IMPLEMENTED)

**Every successful vote is immediately signed and becomes part of the TimeProof.**

Instead of:
```
Sampling (20 rounds) → LocallyAccepted → Assemble TimeProof → GloballyFinalized
```

Do:
```
TimeVote (accumulate signed votes) → Finalized (when threshold reached)
```

### Design

#### 1. Single Finality State

**States:**
- `Seen` - Transaction received, pending validation
- `Voting` - Actively collecting TimeProof votes
- `Finalized` - TimeProof threshold reached (67% weight)
- `Rejected` - Invalid or lost conflict
- `Archived` - Included in TimeLock checkpoint

**No more:** LocallyAccepted, GloballyFinalized distinction

#### 2. Signed Votes from the Start

When a validator responds to a query:
```rust
struct TimeVoteResponse {
    txid: Hash256,
    decision: VoteDecision, // Accept / Reject
    signed_vote: FinalityVote, // Immediately signed
}
```

Every positive vote is **immediately signed** and contributes to the TimeProof.

#### 3. Progressive TimeProof Assembly

```rust
struct TimeVoteState {
    txid: Hash256,
    status: ConsensusStatus,
    accumulated_votes: Vec<FinalityVote>, // Signed votes
    accumulated_weight: u64,              // Running total
    required_weight: u64,                 // 67% of AVS weight
    confidence: u32,                      // Consecutive successful rounds
}
```

**Algorithm:**
1. Node sends query to k validators
2. Each response includes signed FinalityVote
3. Node accumulates unique votes
4. When `accumulated_weight ≥ required_weight`:
   - Status → `Finalized`
   - TimeProof is complete (no additional assembly needed)
   - Broadcast TimeProof to network

#### 4. Optimistic Finality Check

To maintain sub-second UX:
```rust
if confidence >= 3 && accumulated_weight >= 0.51 * required_weight {
    // "Optimistically Confirmed" (UX only, not protocol state)
    // Still waiting for full 67% threshold
}
```

Wallets can show "Confirming..." until full finality, but **only one protocol state: Finalized**.

---

## Benefits

### 1. **Simplicity**
- One finality definition
- One finality mechanism
- No dual-state confusion

### 2. **Verifiability**
- Every transaction in `Finalized` state has a TimeProof
- No need to "assemble proof later"
- Anyone can verify immediately

### 3. **Efficiency**
- No wasted sampling rounds
- Every vote contributes to the final proof
- No duplicate work

### 4. **Better UX**
- Clear progression: "Pending" → "Finalized"
- Optional optimistic UX: show confidence % during voting
- No confusion about "confirmed but not final"

### 5. **Security**
- Same 67% Byzantine threshold
- Same TimeProof security guarantees
- Stronger because every vote is signed immediately

---

## Implementation Changes

### Protocol Specification (TIMECOIN_PROTOCOL.md)

**Section 7: TimeVote Protocol**

```
### 7.1 Parameters
- k: sample size (default 20)
- α: successful poll threshold (default 14)
- Q_finality: finality threshold (67% of AVS weight)
- POLL_TIMEOUT: default 200ms
- MAX_TXS_PER_QUERY: default 64

### 7.2 Voting Response
When queried about transaction X, validator MUST:
1. Validate transaction per §6
2. Check UTXO availability
3. Check for conflicts with preferred transactions
4. If valid: Sign and return FinalityVote with decision=Accept
5. If invalid: Return VoteResponse with decision=Reject (no signature needed)

### 7.3 TimeVote State (per txid)
Each node maintains:
- status[X] ∈ {Seen, Voting, Finalized, Rejected, Archived}
- accumulated_votes[X]: Set<FinalityVote>
- accumulated_weight[X]: u64
- confidence[X]: consecutive successful polls
- preferred_txid[o]: per outpoint preference

### 7.4 Polling Loop
For txid X in Voting:
1. Select k masternodes from AVS (stake-weighted)
2. Send VoteQuery for X
3. Collect signed votes until timeout
4. Add valid new votes to accumulated_votes[X]
5. Update accumulated_weight[X]
6. If v >= α (quorum in this round):
   - confidence[X] += 1
   - Update preferred_txid[o]
7. Else:
   - confidence[X] = 0
8. If accumulated_weight[X] >= Q_finality:
   - status[X] = Finalized
   - Assemble TimeProof from accumulated_votes
   - Broadcast TimeProof
   - Stop polling

### 7.5 Finality Rule
A node MUST set status[X] = Finalized when:
- accumulated_weight[X] >= Q_finality, AND
- accumulated_votes form valid TimeProof per §8

When X is Finalized, MUST mark all conflicting transactions as Rejected.

### 7.6 TimeProof
A TimeProof for transaction X is the set of accumulated FinalityVotes with:
- Σ weight >= Q_finality
- All votes verify
- All votes for same (txid, chain_id, tx_hash_commitment, slot_index)

No separate assembly needed - TimeProof = accumulated_votes.
```

### Code Changes

#### consensus.rs
```rust
#[derive(Debug, Clone, PartialEq)]
pub enum ConsensusStatus {
    Seen,           // Transaction received
    Voting,         // Actively collecting votes
    Finalized,      // TimeProof threshold reached
    Rejected,       // Invalid or lost conflict
    Archived,       // In TimeLock checkpoint
}

pub struct TimeVoteState {
    pub status: ConsensusStatus,
    pub accumulated_votes: Vec<FinalityVote>,
    pub accumulated_weight: u64,
    pub required_weight: u64,
    pub confidence: u32,
    pub first_seen: Instant,
    pub preferred_inputs: HashMap<OutPoint, Hash256>,
}

impl TimeVoteState {
    pub fn add_vote(&mut self, vote: FinalityVote, weight: u64) -> bool {
        // Check if vote is new
        if self.accumulated_votes.iter().any(|v| v.voter_mn_id == vote.voter_mn_id) {
            return false; // Duplicate
        }
        
        // Add vote
        self.accumulated_votes.push(vote);
        self.accumulated_weight += weight;
        
        // Check finality
        if self.accumulated_weight >= self.required_weight {
            self.status = ConsensusStatus::Finalized;
            true
        } else {
            false
        }
    }
    
    pub fn as_timeproof(&self) -> Option<TimeProof> {
        if self.status == ConsensusStatus::Finalized {
            Some(TimeProof {
                votes: self.accumulated_votes.clone(),
                total_weight: self.accumulated_weight,
            })
        } else {
            None
        }
    }
}
```

#### Remove LivenessAlert Logic
The TimeGuard protocol becomes simpler - if a transaction stalls in `Voting`:
1. Wait for timeout
2. Fall back to deterministic leader-based resolution
3. Leader's decision must still get 67% signed votes

No need for separate LocallyAccepted tracking.

---

## Migration Path

### Phase 1: Update Protocol Spec
- [ ] Rewrite §7 (TimeVote Protocol)
- [ ] Simplify §8 (TimeProof) - no separate assembly
- [ ] Update §7.6 (TimeGuard) - remove LocallyAccepted logic

### Phase 2: Update Code
- [ ] Refactor `ConsensusStatus` enum
- [ ] Refactor `TimeVoteState` struct
- [ ] Update vote collection logic
- [ ] Update finality detection
- [ ] Remove LocallyAccepted checks

### Phase 3: Update Documentation
- [ ] Architecture overview
- [ ] Quick reference
- [ ] CLI guide

### Phase 4: Testing
- [ ] Unit tests for new consensus logic
- [ ] Integration tests for finality
- [ ] Network simulations

---

## Comparison

### Old Model (Two-Tier)
```
User sends TX
    ↓
Seen → Sampling (rounds 1-20, no signatures)
    ↓
LocallyAccepted (confidence=20, probabilistic)
    ↓
Assemble TimeProof (request signatures separately)
    ↓
GloballyFinalized (67% weight)
    ↓
Checkpointed

Time to finality: ~4-5 seconds (20 rounds + signature collection)
```

### New Model (Unified)
```
User sends TX
    ↓
Seen → Voting (collect signed votes)
    ↓
Finalized (67% weight reached, TimeProof ready)
    ↓
Checkpointed

Time to finality: ~1-2 seconds (accumulate to threshold)
```

**Improvement:** Faster, simpler, more verifiable.

---

## Security Analysis

### Byzantine Tolerance
- **Same:** 67% threshold (tolerates <33% Byzantine)
- **Better:** Every vote is signed from the start (harder to forge)

### Liveness
- **Same:** TimeGuard protocol provides fallback
- **Better:** Faster detection of stalls (no LocallyAccepted delay)

### Double-Spend Prevention
- **Same:** UTXO locking + conflict resolution
- **Better:** Finalized = TimeProof exists (objective proof)

### Network Partition
- **Same:** Partitions can't finalize transactions without 67% weight
- **Better:** Clearer state (either Finalized or not)

---

## Recommendation

**Implement this unified model.**

The two-tier system (LocallyAccepted vs GloballyFinalized) is a legacy of adapting Avalanche to blockchain. TimeCoin should have:

✅ **One finality**  
✅ **One proof**  
✅ **One definition**  

This makes the protocol simpler, faster, and more secure.

---

## Questions for Review

1. **Should we keep optimistic "confirming" UX?**
   - Suggestion: Yes, but only in wallet UI, not protocol state
   - Show "Confirming (45% of votes collected)" during voting

2. **What about very fast conflicts?**
   - Current model: LocallyAccepted provides fast probabilistic signal
   - New model: Can show confidence % during voting phase
   - Both achieve same UX, new model more honest

3. **Backward compatibility?**
   - This is a breaking change to protocol state machine
   - But TimeCoin is pre-mainnet, so now is the time

---

**Decision needed:** Proceed with implementation?
