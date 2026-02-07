# TIME Coin Protocol Specification (Improved)
**Document:** `TIMECOIN_PROTOCOL.md`  
**Version:** 6.2 (TimeVote Protocol + TimeLock Checkpoints + TimeProof + TimeGuard Protocol - COMPLETE)  
**Last Updated:** January 28, 2026  
**Status:** Implementation Spec (Normative)

---

## Table of Contents

1. [Overview](#1-overview)
2. [Design Goals and Non‑Goals](#2-design-goals-and-non-goals)
3. [System Architecture](#3-system-architecture)
4. [Cryptography and Identifiers](#4-cryptography-and-identifiers)
5. [Masternodes, Weight, and Active Validator Set (AVS)](#5-masternodes-weight-and-active-validator-set-avs)
6. [UTXO Model and Transaction Validity](#6-utxo-model-and-transaction-validity)
7. [TimeVote Protocol Finality](#7-timevote-protocol-finality)
8. [TimeProof (Verifiable Finality)](#8-timeproof-verifiable-finality)
9. [TimeLock Checkpoint Blocks (Archival Chain)](#9-timelock-checkpoint-blocks-archival-chain)
10. [Rewards and Fees](#10-rewards-and-fees)
11. [Network Protocol](#11-network-protocol)
12. [Mempool and Pooling Rules](#12-mempool-and-pooling-rules)
13. [Security Model](#13-security-model)
14. [Configuration Defaults](#14-configuration-defaults)
15. [Implementation Notes](#15-implementation-notes)
16. [Cryptographic Bindings (NORMATIVE ADDITIONS)](#16-cryptographic-bindings-normative-additions)
17. [Transaction and Staking UTXO Details](#17-transaction-and-staking-utxo-details)
18. [Network Transport Layer (NORMATIVE)](#18-network-transport-layer-normative)
19. [Genesis Block and Initial State (NORMATIVE)](#19-genesis-block-and-initial-state-normative)
20. [Clock Synchronization Requirements (NORMATIVE)](#20-clock-synchronization-requirements-normative)
21. [Light Client and SPV Support (OPTIONAL)](#21-light-client-and-spv-support-optional)
22. [Error Recovery and Edge Cases (NORMATIVE)](#22-error-recovery-and-edge-cases-normative)
23. [Address Format and Wallet Integration (NORMATIVE)](#23-address-format-and-wallet-integration-normative)
24. [Mempool Management and Fee Estimation (NORMATIVE)](#24-mempool-management-and-fee-estimation-normative)
25. [Economic Model (NORMATIVE)](#25-economic-model-normative)
26. [Implementation Checklist](#26-implementation-checklist)
27. [Test Vectors](#27-test-vectors)

---

## 1. Overview

TIME Coin separates **state finality** from **historical checkpointing**:

- **TimeVote Protocol (Transaction Layer):** fast, leaderless, stake-weighted voting that converges on a single winner among conflicting transactions. Progressive TimeProof assembly provides **unified finality** when 51% weight threshold is reached.
- **TimeProof:** Accumulates signed votes during consensus to create an **objectively verifiable artifact** that any node can validate offline. No separate assembly step needed.
- **TimeLock (Block Layer):** deterministic, VRF-sortition checkpoint blocks every 10 minutes. Blocks are **archival** (history + reward events), not the source of transaction finality.

> **Terminology note:** **AVS** means **Active Validator Set** (eligible active masternodes). It is purely a protocol term.

---

## 2. Design Goals and Non‑Goals

### 2.1 Goals
1. **Fast settlement:** typical confirmation < 1s under healthy network conditions.
2. **Leaderless transaction finality:** no global committee rounds for transaction acceptance.
3. **Sybil resistance:** voting influence proportional to stake weight.
4. **Objective verification:** third parties can verify that a transaction reached finality using a compact proof (TimeProof).
5. **Deterministic checkpoint schedule:** blocks every 600s aligned to wall clock.

### 2.2 Non‑Goals
- Deterministic BFT finality for transaction acceptance (TIME Coin uses probabilistic consensus + proofs).
- A single globally agreed mempool set at all times.
- “Blocks finalize transactions”; blocks only archive and distribute rewards.

---

## 3. System Architecture

Two time scales:

```
Real-time (Transactions)
Tx broadcast -> TimeVote (progressive voting) -> TimeProof assembled -> Finalized

Epoch-time (Blocks)
Every 10 minutes -> TimeLock checkpoint block archives finalized txs + rewards
```

---

## 4. Cryptography and Identifiers

### 4.1 Chain ID
All signed objects MUST include `chain_id` to prevent replay across networks.

### 4.2 Hashes
- `Hash256`: 32-byte cryptographic hash (e.g., SHA-256d or BLAKE3; MUST be fixed by implementation).
- `txid = H(serialized_tx)`

### 4.3 Signatures
- `Ed25519` signatures for node identity, heartbeats, attestations, and finality votes.

### 4.4 VRF
- VRF scheme MUST provide `(vrf_output, vrf_proof)` verifiable under a public key.
- VRF input MUST bind to `"TIMECOIN_VRF_V2" || height || prev_block_hash`.

---

## 5. Masternodes, Weight, and Active Validator Set (AVS)

### 5.1 Masternode Identity
A masternode has:
- `mn_id` (derived from pubkey)
- `pubkey`
- `weight w` (tier-derived)
- `vrf_pubkey` (may be same key)

### 5.2 Tier Weights

Each tier has multiple weight types for different protocol functions:

| Tier | Collateral (TIME) | Sampling Weight | Reward Weight | Voting Power |
|------|-------------------|-----------------|---------------|--------------|
| Free | 0 | 1 | 100 | 0 |
| Bronze | 1,000 | 10 | 1,000 | 1 |
| Silver | 10,000 | 100 | 10,000 | 10 |
| Gold | 100,000 | 1,000 | 100,000 | 100 |

- **Sampling Weight (`w`):** Used for stake-weighted validator selection during TimeVote polling (§7.4)
- **Reward Weight:** Used for proportional block reward distribution (§10.4). Scales 1:1 with collateral except Free tier (0.1x relative to Bronze)
- **Voting Power:** Used for governance. Free tier nodes cannot vote on governance (voting power = 0)

### 5.3 Collateral Enforcement (MUST CHOOSE ONE)
1. **On-chain staking UTXO (RECOMMENDED):** stake locked by a staking script; weight derived from locked amount and tier mapping.
2. **Registry authority:** external registry signs membership updates (not trustless).

This spec assumes **on-chain staking UTXO** unless explicitly configured otherwise.

### 5.4 Active Validator Set (AVS)
Only masternodes in the **AVS** may be:
- sampled for TimeVote queries
- counted for TimeProof weight thresholds
- eligible to produce/compete for TimeLock checkpoint blocks

A masternode is **AVS-active** if:
- It has a valid `SignedHeartbeat` within `HEARTBEAT_TTL` (default 180s), AND
- That heartbeat has ≥ `WITNESS_MIN` attestations (default 3) from distinct AVS-active witnesses.

Nodes MUST maintain and gossip AVS state.

### 5.5 Stake-weighted voting distribution
Validator selection for queries MUST be stake-weighted over AVS:
`P(i) = w_i / Σ_{j∈AVS} w_j`

Validator selection SHOULD be without replacement per poll.

---

## 6. UTXO Model and Transaction Validity

### 6.1 UTXO States (per outpoint)
- `Unspent`
- `Locked(txid)` (local reservation)
- `Spent(txid)` (by Finalized tx)
- `Archived(txid, height)` (spent + checkpointed)

### 6.2 Transaction Validity Preconditions
A node MUST treat a Tx as **invalid** (and vote `Invalid`) if:
1. Syntax/format invalid
2. Signature/script invalid
3. Any input outpoint is unknown or not `Unspent` locally (or known `Spent/Archived`)
4. Fee < `MIN_FEE`
5. Fails policy limits (size, etc., if enabled)

### 6.3 Conflict Sets
For each input outpoint `o`, define a conflict set `C(o)` containing all txids spending `o`.

Only one txid per outpoint may be Finalized.

---

## 7. TimeVote Protocol Finality

TIME Coin uses stake-weighted repeated voting with progressive proof accumulation. The protocol is defined on **conflict sets** (double spends), while non-conflicting transactions converge trivially.

### 7.1 Parameters
- `k`: sample size (default 20)
- `α`: successful poll threshold (default 14)
- `Q_finality`: finality threshold (51% of AVS weight)
- `POLL_TIMEOUT`: default 200ms
- `MAX_TXS_PER_QUERY`: default 64

### 7.2 Voting Response
When queried about transaction `X`, a validator MUST:

1. Validate transaction per §6
2. Check UTXO availability
3. Check for conflicts with preferred transactions
4. If valid and preferred: **Sign and return `FinalityVote`** with decision=Accept (§8.1)
5. If invalid or conflicting: **Sign and return `FinalityVote`** with decision=Reject (§8.1)

**CRITICAL SECURITY REQUIREMENT:** ALL votes MUST be signed, including Reject votes.

**Rationale (Equivocation Attack Prevention):**

Without signatures on Reject votes, the following attack is possible:

```
Attack Scenario (Network Partition):
1. Attacker broadcasts conflicting transactions tx_A and tx_B simultaneously
2. Network temporarily partitions (natural or induced)
3. Partition 1 sees tx_A first → 40% of validators vote Accept (signed)
4. Partition 2 sees tx_B first → 40% of validators vote Accept (signed)
5. Remaining 20% validators see both → vote Reject (unsigned)

Problem: The 20% can CHANGE their votes after partition heals:
- They have no cryptographic commitment to their Reject decision
- Could vote Accept for tx_A after seeing it reached 40%
- Or vote Accept for tx_B after seeing it reached 40%
- Both transactions could appear to reach 51% threshold (equivocation)
```

**With signed Reject votes:**
- Each validator creates cryptographic evidence of voting history
- Cannot later claim to have voted Accept when they voted Reject
- Equivocation becomes provably fraudulent (slashable offense)
- Network converges to single truth despite partitions

**Critical:** Every vote (Accept AND Reject) MUST include a signed `FinalityVote` immediately. This vote contributes to consensus safety.

Responder MUST NOT return `Accept` for two conflicting txs for the same outpoint.

### 7.3 TimeVote State (per txid)
Each node maintains:
- `status[X] ∈ {Seen, Voting, Finalized, Rejected, Archived}`
- `accumulated_votes[X]`: Set of unique `FinalityVote` signatures
- `accumulated_weight[X]`: u64 (sum of validator weights for votes in accumulated_votes)
- `confidence[X]`: consecutive successful polls (for optimization only)
- Per outpoint preference `preferred_txid[o]`

**State Descriptions:**
- **Seen**: Transaction received, pending validation
- **Voting**: Actively collecting signed votes and building TimeProof
- **Finalized**: Accumulated weight ≥ Q_finality, TimeProof complete
- **Rejected**: Invalid or lost conflict resolution
- **Archived**: Included in TimeLock checkpoint

Tie-breakers MUST be deterministic (RECOMMENDED: lowest `txid` wins ties).

### 7.4 Polling Loop (per txid)
For txid `X` in `Voting`:

1. Select `k` masternodes from the AVS (stake-weighted).
2. Send `VoteQuery` including `X` (batched allowed).
3. Collect **signed votes** until timeout.
4. For each new valid `FinalityVote` received:
   - Verify signature and voter eligibility
   - If vote is from a new voter (not already in `accumulated_votes[X]`):
     - Add vote to `accumulated_votes[X]`
     - Add voter's weight to `accumulated_weight[X]`
5. Let `v = count(Accept votes in this round)`.
6. If `v ≥ α`:
   - `confidence[X] += 1`
   - Update `preferred_txid[o]` for each input outpoint `o` (highest accumulated_weight wins).
7. Else:
   - `confidence[X] = 0`
8. **Check Finality:**
   - If `accumulated_weight[X] ≥ Q_finality`:
     - Set `status[X] = Finalized`
     - Assemble TimeProof from `accumulated_votes[X]`
     - Broadcast TimeProof to network
     - Stop polling for `X`

### 7.5 Finality Rule
A node MUST set `status[X] = Finalized` when:
- `accumulated_weight[X] ≥ Q_finality`, AND
- `accumulated_votes[X]` form a valid TimeProof per §8

When `X` reaches `Finalized`, the node MUST:
1. Mark all conflicting transactions for any input outpoint as `Rejected`
2. Broadcast the assembled TimeProof to peers
3. Stop polling for `X`

**There is only ONE finality state.** When a transaction is `Finalized`, it has an objective, verifiable TimeProof that any node can validate.

> **Wallet UX:** Wallets MAY show "Confirming (X% votes)" during `Voting` state for optimistic UX, but MUST clearly indicate that only `Finalized` represents cryptographic finality with TimeProof.
### 7.6 TimeGuard Protocol

The TimeVote Protocol's probabilistic voting can stall under adversarial conditions or network partitions. This section defines a **deterministic fallback mechanism** that guarantees bounded recovery time while preserving the leaderless nature of normal operation.

#### 7.6.1 Stall Detection

A node detects a **liveness stall** for transaction `X` if ALL of the following hold:

1. `status[X] == Voting` for duration `> STALL_TIMEOUT` (default: 30 seconds)
2. `accumulated_weight[X] < Q_finality` (transaction has not reached finality)
3. No conflicting transaction for any input of `X` has reached `Finalized`
4. Transaction `X` is valid per §6 validation rules

**Rationale:** Distinguishes genuine stalls from consensus-resolved conflicts or invalid transactions.

#### 7.6.2 Stall Evidence and Alert Broadcast

When a node detects a stall for transaction `X`:

1. **Assemble Evidence:**
   ```
   TimeGuardAlert {
     chain_id: u32,
     txid: Hash256,
     tx_hash_commitment: Hash256,
     slot_index: u64,              // Current slot when stall detected
     poll_history: Vec<PollResult>, // Last N poll results
     current_confidence: u32,
     stall_duration_ms: u64,
     reporter_mn_id: String,
     reporter_signature: Signature
   }
   
   PollResult {
     round: u64,
     votes_valid: u32,
     votes_invalid: u32,
     votes_unknown: u32,
     timestamp_ms: u64
   }
   ```

2. **Broadcast to Network:**
   - Send `TimeGuardAlert` to all connected peers
   - Peers MUST validate signature and that reporter is in AVS
   - Peers SHOULD relay alert if they also observe the stall

#### 7.6.3 Fallback Trigger Threshold

A node enters **Fallback Mode** for transaction `X` when:

- It receives `TimeGuardAlert` messages from `≥ f+1` distinct masternodes (where `f = ⌊(n-1)/3⌋`)
- All alerts reference the same `txid` and conflict set
- At least `FALLBACK_MIN_DURATION` (default: 20s) has elapsed since first alert

**Safety:** `f+1` alerts guarantees at least one honest node observed the stall (in a BFT-style `n ≥ 3f+1` model).

#### 7.6.4 Deterministic Resolution Round

Upon entering Fallback Mode for transaction `X`:

**Step 1: Freeze TimeVote Polling**
- Set `status[X] = FallbackResolution`
- Stop sending new `VoteQuery` messages for `X` or any conflicting transaction
- Continue responding to queries from peers (based on current preference)

**Step 2: Deterministic Leader Election**
```
fallback_leader = MN with minimum H(txid || slot_index || mn_pubkey)
```
- All nodes compute the same leader independently (no message exchange)
- Leader MUST be member of AVS snapshot at `slot_index`
- If elected leader is offline, timeout advances to next slot

**Step 3: Leader Proposal**
Within `FALLBACK_PROPOSAL_TIMEOUT` (default: 5 seconds), the leader broadcasts:
```
FinalityProposal {
  chain_id: u32,
  txid: Hash256,
  tx_hash_commitment: Hash256,
  slot_index: u64,
  decision: Accept | Reject,
  justification: String,  // OPTIONAL: debugging info
  leader_mn_id: String,
  leader_signature: Signature
}
```

**Decision Logic for Leader:**
- `Accept` if `accumulated_weight[X] > accumulated_weight[conflicting_tx]` for all conflicts
- `Reject` if any conflict has higher `accumulated_weight` value
- `Accept` if tied (use deterministic tie-breaker: lowest `txid`)

**Step 4: Voting Round**
All AVS members vote on the proposal within `FALLBACK_VOTE_TIMEOUT` (default: 5 seconds):
```
FallbackVote {
  chain_id: u32,
  proposal_hash: Hash256,  // H(FinalityProposal)
  vote: Approve | Reject,
  voter_mn_id: String,
  voter_weight: u64,
  voter_signature: Signature
}
```

**Voting Rule:**
- `Approve` if voter's local state agrees with leader's decision AND proposal is valid
- `Reject` otherwise

**Step 5: Finalization**
If proposal receives `≥ Q_finality` total weight in `Approve` votes (same threshold as TimeProof §8.3):

1. Set `status[X] = Finalized` (if decision = Accept) or `Rejected` (if decision = Reject)
2. Assemble TimeProof (§8) using the collected `FallbackVote` signatures
3. Mark conflicting transactions as `Rejected`
4. Resume normal TimeVote operation for other transactions

**Step 6: Timeout and Retry**
If no decision after `FALLBACK_ROUND_TIMEOUT` (default: 10 seconds):

1. Increment `slot_index += 1` (advances to next leader)
2. Repeat from Step 2 with new leader
3. After `MAX_FALLBACK_ROUNDS` (default: 5), escalate to TimeLock checkpoint synchronization

#### 7.6.5 TimeLock Checkpoint Synchronization (Ultimate Fallback)

If fallback rounds fail after `MAX_FALLBACK_ROUNDS` attempts:

1. Transaction `X` remains in `Voting` state
2. Wait for next TimeLock block boundary (≤ 10 minutes per §9)
3. TimeLock block producer (VRF-selected) MUST include a `TimeGuardRecoveryFlag` in block header
4. Block producer deterministically resolves all pending liveness stalls:
   - Includes transactions with `accumulated_weight[X] > 0` (at least one positive vote observed)
   - Excludes transactions with only negative votes
5. All nodes synchronize to TimeLock block state
6. Reset TimeVote polling for any remaining unresolved transactions

> **Note:** This is a rare fallback (expected frequency: < 0.01% of transactions under normal operation).

#### 7.6.6 State Transition Diagram

```
Seen → Voting ──────────────────────────────────→ Finalized
         │                                            (accumulated_weight ≥ Q_finality)
         │                                            (TimeProof complete)
         │
         │ (Conflicting tx finalized)
         ↓
      Rejected
         
         
         
Voting → (Stall detected) → FallbackResolution ─────→ Finalized (if proposal approved)
                                    │                        or
                                    │                    Rejected (if proposal rejected)
                                    ↓
                              (Fallback timeout)
                                    ↓
                          (Retry with new leader or TimeLock checkpoint)
```

#### 7.6.7 Protocol Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `STALL_TIMEOUT` | 30s | Duration before declaring liveness stall |
| `FALLBACK_MIN_DURATION` | 20s | Minimum time before accepting alerts |
| `FALLBACK_PROPOSAL_TIMEOUT` | 5s | Leader must propose within this time |
| `FALLBACK_VOTE_TIMEOUT` | 5s | Voting period duration |
| `FALLBACK_ROUND_TIMEOUT` | 10s | Total time for one fallback round |
| `MAX_FALLBACK_ROUNDS` | 5 | Attempts before TimeLock escalation |
| `ALERT_THRESHOLD` | f+1 | TimeGuardAlerts needed to trigger (`f = ⌊(n-1)/3⌋`) |

#### 7.6.8 Security Considerations

**Byzantine Resistance:**
- Fallback requires `f+1` alerts → at least one honest node confirms stall
- Leader is deterministic → no leader election attacks
- Voting requires `≥ Q_finality` weight → Byzantine minority cannot force decision
- TimeLock provides ultimate synchronization point

**Liveness Guarantees:**
- Worst-case resolution time: `30s (stall) + 5×10s (fallback rounds) + 10min (TimeLock) ≈ 11.3 minutes`
- Typical case: `30s (stall) + 10s (single fallback round) = 40 seconds`
- No indefinite deadlock possible (bounded by TimeLock interval)

**Comparison to Pure BFT:**
- Avoids view change storms (deterministic leader selection)
- No multi-phase commit complexity (single propose → vote → finalize)
- Fallback is rare (only after TimeVote stalls)
- Normal operation remains leaderless and fast (<1s)

#### 7.6.9 Implementation Requirements

Nodes MUST implement:
1. Stall detection timer per transaction in `Voting` state
2. `TimeGuardAlert` message handling and relay logic
3. Deterministic leader computation function
4. `FinalityProposal` and `FallbackVote` message types
5. Fallback voting logic and threshold checking
6. State transition from `FallbackResolution` to final state
7. Progressive accumulation of signed votes into `accumulated_votes[X]`
8. Weight tracking in `accumulated_weight[X]`
9. Finality threshold check after each polling round

Nodes SHOULD implement:
1. Metrics for fallback activation frequency
2. Logging of fallback events for network health monitoring
3. Dashboard indicators when node is in fallback mode
4. Optimistic UX indicators showing vote accumulation progress during `Voting` state

#### 7.6.10 v6.2 Implementation Details (January 2026)

**Implementation Status:** ✅ COMPLETE

The v6.2 release provides full implementation of §7.6 with the following components:

**Core Components:**
- `start_stall_detection()` - Background task monitoring transactions every 5s
- `elect_fallback_leader()` - Deterministic hash-based leader selection
- `execute_fallback_as_leader()` - Leader proposal workflow
- `start_fallback_resolution()` - Monitors FallbackResolution transactions
- `start_fallback_timeout_monitor()` - Handles round timeouts and retries
- `resolve_stalls_via_timelock()` - TimeLock integration for ultimate recovery

**Security Features:**
- `detect_alert_equivocation()` - Prevents duplicate alerts from same node
- `detect_vote_equivocation()` - Prevents conflicting votes from same voter
- `detect_multiple_proposals()` - Flags Byzantine behavior (multiple proposals/tx)
- `validate_vote_weight()` - Ensures vote weight doesn't exceed total AVS weight
- `flag_byzantine()` - Tracks and flags malicious masternodes

**Monitoring & Metrics:**
- `FallbackMetrics` - Comprehensive metrics snapshot
- `record_fallback_activation()` - Counter for fallback triggers
- `record_stall_detection()` - Counter for stall detections
- `record_timelock_resolution()` - Counter for TimeLock recoveries
- `log_fallback_status()` - Detailed logging for debugging

**Message Handlers:**
- `handle_liveness_alert()` - Validates, accumulates, triggers fallback at f+1
- `handle_finality_proposal()` - Validates leader, casts vote
- `handle_fallback_vote()` - Accumulates votes, finalizes at Q_finality

**Performance Characteristics:**
- Stall detection overhead: <1ms per transaction
- Memory overhead: ~1KB per stalled transaction
- Typical recovery: 35-45 seconds (including network latency)
- Worst-case recovery: ≤11.3 minutes (via TimeLock)
- Byzantine tolerance: up to f=(n-1)/3 malicious nodes

**Testing:**
- 10+ unit tests covering all critical paths
- Alert accumulation and threshold testing
- Vote accumulation and Q_finality validation
- Leader election determinism verification
- Byzantine behavior detection

**Files Modified:**
- `src/consensus.rs` - Core fallback logic (~500 lines)
- `src/network/message_handler.rs` - Message processing enhancements
- `src/blockchain.rs` - TimeLock integration
- `src/block/types.rs` - Added `liveness_recovery` flag
- `src/types.rs` - Added `FallbackMetrics` struct

----

## 8. TimeProof (Verifiable Finality)

TimeProof is the mechanism for achieving finality in TimeCoin. A TimeProof is assembled progressively as nodes collect finality votes during normal transaction validation. Once enough votes are collected (≥51% of AVS weight), the transaction achieves finality and the TimeProof can be:
- gossiped
- stored
- included (directly or by hash) in checkpoint blocks
- validated by any node without replaying sampling history

### 8.1 Finality Vote
A **FinalityVote** is a signed statement:

`FinalityVote = { chain_id, txid, tx_hash_commitment, slot_index, decision, voter_mn_id, voter_weight, signature }`

Where:
- `decision ∈ {Accept, Reject}` - REQUIRED: The voter's decision on this transaction
- `tx_hash_commitment = H(canonical_tx_bytes)` (canonical serialization MUST be specified)
- `slot_index` is the slot when the vote is issued (prevents indefinite replay)

Signature covers all fields including the `decision`.

**Eligibility:** A vote counts only if the voter is AVS-active in the referenced `slot_index` (see §8.4).

**Security Note:** Both Accept AND Reject votes MUST be signed to prevent equivocation attacks (see §7.2). A validator who signs "Reject" for transaction X creates cryptographic proof they rejected it, preventing them from later claiming they voted "Accept" during network partitions.

### 8.2 TimeProof Definition
A **TimeProof** for transaction `X` is:

`TimeProof(X) = { tx, slot_index, votes[] }`

Where `votes[]` contains ONLY votes with `decision=Accept`.

Validity conditions:
1. All `votes[]` signatures verify.
2. All votes agree on `(chain_id, txid, tx_hash_commitment, slot_index)`.
3. **All votes have `decision=Accept`** (Reject votes are signed but do NOT contribute to finality weight).
4. Voters are distinct (by `voter_mn_id`).
5. Each voter is a member of the **AVS snapshot** for that `slot_index`.
6. Sum of distinct voter weights `Σ w_i ≥ Q_finality(slot_index)`.

**Note on Reject Votes:**
- Reject votes MUST be signed (equivocation prevention per §7.2)
- Reject votes create cryptographic proof of rejection
- Reject votes do NOT count toward the 51% finality threshold
- Only Accept votes accumulate toward TimeProof finality weight

### 8.3 Finality threshold
Let `total_AVS_weight(slot_index)` be the total weight of the AVS at that slot.

Default:
- `Q_finality(slot_index) = 0.67 * total_AVS_weight(slot_index)` (rounded up)

The network MUST use a single, agreed rounding rule.

### 8.4 AVS snapshots
Nodes MUST retain **AVS snapshots** by slot for at least `ASS_SNAPSHOT_RETENTION` slots (rename retained for historical compatibility; see defaults).

An AVS snapshot MUST include:
- member `mn_id`
- `pubkey`
- `weight`
- (optional) `vrf_pubkey`

### 8.5 Assembling a TimeProof
TimeProof is assembled progressively during normal transaction validation:
- When performing `SampleQuery` during validation, responders SHOULD include a `FinalityVote` when responding `Valid` (if requested).
- The initiator accumulates unique votes as part of the normal polling process.
- Once the accumulated vote weight reaches the finality threshold (≥51% of AVS weight), the TimeProof is complete and the transaction achieves finality.

There is no separate "finalization phase" - votes are collected during the same process as validation polling.

### 8.6 Finalization Rule
A node MUST set `status[X] = Finalized` when it has a valid `TimeProof(X)`.

Once a transaction is `Finalized`, a node MUST reject any conflicting transaction `Y` spending any same outpoint.

The TimeProof IS the finality - there is only one finality state, achieved when the vote threshold is met.

### 8.7 Catastrophic conflict
If two conflicting transactions both obtain valid TimeProofs, the network’s safety assumptions have been violated. Clients SHOULD halt automatic finalization and surface an emergency condition. (Slashing/recovery is out of scope unless separately specified.)

---

## 9. TimeLock Checkpoint Blocks (Archival Chain)

Checkpoint blocks exist to:
- checkpoint history
- provide a reward schedule
- compactly summarize finalized transactions

### 9.1 Slot Timing
- `BLOCK_INTERVAL = 600s`
- `slot_time = slot_index * 600`

### 9.2 Sortition (Deterministic Candidate Ranking)
For each masternode `i` in the AVS at `slot_index`:
- `vrf_input = SHA256("TIMECOIN_VRF_V2" || uint64_le(height) || prev_block_hash)`
- `score_i = VRF(vrf_input, sk_i)`

Lower `score_i` is better.

**Security Note (VRF Grinding Mitigation):**
The VRF input MUST include `prev_block_hash` to prevent grinding attacks. The domain separator `"TIMECOIN_VRF_V2"` and block `height` are predictable, but `prev_block_hash` changes with each block and cannot be known in advance, making pre-computation attacks infeasible. This follows best practices from Algorand, Ethereum 2.0, and Cardano.

### 9.3 Canonical block selection (no timeout proofs)
Any AVS-active masternode MAY publish a candidate block for the slot.

Nodes select the canonical block for a slot by:
1. Validity first
2. Lowest `vrf_output` second
3. Tie-breaker: lowest block hash

This eliminates unverifiable “leader timeout” behavior.

### 9.4 Block Content
A block MUST contain:
- Header:
  - `height`
  - `slot_index`, `slot_time`
  - `prev_block_hash`
  - `producer_id`
  - `vrf_output`, `vrf_proof`
  - `finalized_root` (Merkle root over entries; REQUIRED)
- Body:
  - `entries[]` sorted lexicographically by `txid`

Each entry:
`FinalizedEntry = { txid, timeproof_hash }`

Blocks MAY optionally include full `TimeProof` payloads; otherwise nodes fetch TimeProofs by hash.

### 9.5 Block validity
A node MUST accept a block only if:
1. `prev_block_hash` matches the current canonical chain tip.
2. VRF proof verifies and binds to `(prev_block_hash, slot_time, chain_id)`.
3. `entries[]` are sorted and unique by txid.
4. For every entry, the referenced TimeProof is available and valid OR retrievable (implementation may mark as “pending” until fetched).
5. No two included transactions conflict (no outpoint is spent twice).
6. All included transactions are `Finalized` by TimeProof and pass base validity checks.

### 9.6 Archival transition
Upon block acceptance:
- Each included tx becomes `Archived`.
- UTXO updates are applied from the transaction content.
- Rewards are applied according to §10.

---

## 10. Rewards and Fees

### 10.1 Reward event
Rewards are created per checkpoint block.

### 10.2 Base reward
`R = 100 TIME` (fixed per block, 10,000,000,000 satoshis)

The base reward is a constant 100 TIME per checkpoint block, providing predictable issuance and simplifying consensus (all nodes compute identical rewards without needing to agree on AVS size).

### 10.3 Fee accounting
Fees are the sum of included archived transactions’ fees for the slot.

### 10.4 Payout split
- AVS masternodes: 100% of `(R + fees)` distributed proportional to tier reward weight
- No block producer premium (all masternodes share equally by weight)
- No treasury allocation

Masternodes are sorted canonically by address. Each receives `(total_reward * tier_reward_weight) / total_weight`. The last masternode in the sorted list receives the remainder to avoid rounding errors.

Payout MUST be represented as one or more on-chain reward transactions included in the checkpoint block (coinbase-style).

---

## 11. Network Protocol

### 11.1 Message Types (Wire)
```rust
pub enum NetworkMessage {
    // Tx propagation
    TxBroadcast { tx: Transaction },

    // TimeVote polling (batched)
    SampleQuery {
        chain_id: u32,
        request_id: u64,
        txids: Vec<Hash256>,
        want_votes: bool, // request signed FinalityVotes for Valid responses
    },
    SampleResponse {
        chain_id: u32,
        request_id: u64,
        responses: Vec<TxVoteBundle>,
    },

    // TimeProof gossip
    TimeProofGossip { txid: Hash256, TimeProof: TimeProof },

    // Blocks
    BlockBroadcast { block: Block },

    // Liveness
    Heartbeat { hb: SignedHeartbeat },
    Attestation { att: WitnessAttestation },
}

pub struct TxVoteBundle {
    pub txid: Hash256,
    pub vote: VoteResponse, // Valid/Invalid/Unknown
    pub finality_vote: FinalityVote, // REQUIRED: signed vote (includes decision: Accept/Reject)
}

pub enum VoteResponse { Valid, Invalid, Unknown }
```

**CRITICAL CHANGE (Security Enhancement):**

Previously, `finality_vote` was `Option<FinalityVote>` and only included for Valid votes. This created an equivocation vulnerability where validators could change their Reject votes after network partitions.

**New Requirement:** `finality_vote` is REQUIRED for ALL responses (Valid, Invalid, Unknown):
- `Valid` → `FinalityVote { decision: Accept, ... }`
- `Invalid` → `FinalityVote { decision: Reject, ... }`
- `Unknown` → `FinalityVote { decision: Reject, ... }` (conservative: unknown = unsafe)

### 11.2 Anti-replay / validation
All signed messages MUST include `chain_id` and a time/slot domain separator.

Nodes SHOULD rate-limit:
- polling requests per peer
- TimeProof payload sizes
- transaction relay

---

## 12. Mempool and Pooling Rules

### 12.1 Pools
Nodes maintain:
- `SeenPool`: known but not yet voting
- `VotingPool`: active in TimeVote consensus
- `FinalizedPool`: has TimeProof (`Finalized`)
- `ArchivedPool`: checkpointed

### 12.2 Checkpoint inclusion eligibility
Checkpoint blocks SHOULD include:
- all `FinalizedPool` txs not yet archived,
- subject to size limits.

Blocks MUST only include transactions with TimeProof.

---

## 13. Security Model

### 13.1 Assumptions
- A majority (by weight) of the AVS is honest (parameter-dependent).
- Network connectivity allows representative voting.
- AVS membership/weights are correctly enforced (staking/registry + heartbeats + witnesses).

### 13.2 Safety
- Transaction finality is objective once a TimeProof with threshold weight (≥51% of AVS) is obtained.
- The TimeProof mechanism ensures that conflicting transactions cannot both achieve finality under honest majority assumptions.

### 13.3 Liveness
If honest weight dominates and the network is connected, honest transactions can gather TimeProof signatures and be checkpointed.

---

## 14. Configuration Defaults

- `BLOCK_INTERVAL = 600s`
- `TIMEVOTE_K = 20` (sample size)
- `TIMEVOTE_ALPHA = 14` (quorum threshold)
- `Q_FINALITY = 0.67 * total_AVS_weight(slot_index)` (finality threshold)
- `HEARTBEAT_PERIOD = 60s`
- `HEARTBEAT_TTL = 180s`
- `WITNESS_MIN = 3`
- `POLL_TIMEOUT = 200ms`
- `MAX_TXS_PER_QUERY = 64`
- `MIN_FEE = 0.001 TIME`
- `AVS_SNAPSHOT_RETENTION = 7 days worth of slots` (RECOMMENDED; exact number depends on `BLOCK_INTERVAL`)

---

## 15. Implementation Notes

1. **AVS Snapshotting:** store AVS membership/weights by slot for verifying TimeProof voter eligibility.
2. **Bandwidth:** TimeProofs can be large; prefer `timeproof_hash` in blocks + fetch-on-demand.
3. **Conflict handling:** treat conflicts per outpoint; when a TimeProof is accepted, prune all competing spends.
4. **Archival chain reorg tolerance:** checkpoint blocks are archival; transaction finality comes from TimeProof. Reorgs should not affect finalized state unless you explicitly couple rewards/state to block order.
5. **Canonical TX serialization:** MUST be specified precisely, since `tx_hash_commitment` is signed. (Do not reuse non-canonical encodings.)

---

## 16. Cryptographic Bindings (NORMATIVE ADDITIONS)

### 16.1 Hash Function
**REQUIREMENT:** This specification was written with algorithm-agnosticity. For production deployment, implementations MUST pin:

```
HASH_FUNCTION = BLAKE3-256
Alternative for compatibility: SHA-256d (two rounds of SHA-256)
```

**Usage:** All cryptographic hashes (`txid`, `block_hash`, `tx_hash_commitment`, VRF input binding) MUST use the selected function consistently across all nodes.

**Why BLAKE3 (not Ed25519)?**  
BLAKE3 is a *hash function*, Ed25519 is a *signature scheme*. They serve different purposes:
- Hash: Create deterministic content IDs (txid, block_hash)
- Signature: Prove origin and integrity of messages

See **CRYPTOGRAPHY_RATIONALE.md** for detailed explanation.

### 16.2 VRF Scheme
**REQUIREMENT:** VRF is used in §9 for TimeLock sortition. The specification MUST pin a concrete VRF construction:

```
VRF_SCHEME = ECVRF-EDWARDS25519-SHA512-TAI (RFC 9381)
Alternative: deterministic construction from Ed25519 private key
```

**Properties:**
- Deterministic output given the same input (same privkey + input = same score)
- Publicly verifiable proof from public key (anyone can verify)
- Unpredictable to adversaries (only privkey holder knows score first)
- Rankable (numeric output allows sorting; lowest wins)

**Input binding (§9.2):**
```
vrf_input = SHA256("TIMECOIN_VRF_V2" || uint64_le(height) || prev_block_hash)
(vrf_output, vrf_proof) = VRF_Prove(vrf_sk, vrf_input)
```

**CRITICAL: VRF Grinding Attack Prevention**

The VRF input components are:

1. **`"TIMECOIN_VRF_V2"`** - Domain separator string (version-tagged for security enhancements)
2. **`height`** - Block height (deterministic, little-endian u64)
3. **`prev_block_hash`** - Provides unpredictable entropy that changes with each block

**Why this ordering matters:**

Without `prev_block_hash`, an attacker with many potential masternode keys could:
```
For each candidate_key in [key_1, ..., key_N]:
    For each future_slot in [t+1, t+2, ..., t+1000]:
        vrf_score = VRF(candidate_key, future_slot)
        if vrf_score wins:
            register this key as masternode
```

This allows selective registration of only "winning" keys, centralizing block production.

**With `prev_block_hash`:**
- Attacker cannot compute VRF(slot_t+2) until block_t+1 is produced
- Block hashes depend on transaction content (unpredictable)
- Pre-computation beyond 1 block ahead is cryptographically infeasible
- Fair competition among all registered masternodes

This mitigation follows proven approaches from:
- **Algorand:** Uses previous block hash in VRF sortition
- **Ethereum 2.0:** Uses RANDAO (previous block randomness) 
- **Cardano (Ouroboros Praos):** Uses epoch nonce from prior blocks

**Implementation Version:** TIMECOIN_VRF_V2 (grinding-resistant)

**Why VRF (not Ed25519 or BLAKE3 alone)?**  
- Ed25519 signatures cannot be ranked (are just bytes, not sortition-ready)
- BLAKE3 hashes are predictable to everyone (no privacy advantage from a privkey)
- VRF combines: deterministic output + unpredictability + verifiability + rankability

See **CRYPTOGRAPHY_RATIONALE.md** for detailed comparison.

### 16.3 Canonical Transaction Serialization
**REQUIREMENT:** Transaction serialization MUST be fully specified, as `tx_hash_commitment` (§8.1) is signed in finality votes.

**Format:**
```
TxSerialization = {
  version: u32_le,
  input_count: varint,
  inputs: TxInput[],
  output_count: varint,
  outputs: TxOutput[],
  lock_time: u64_le,
}

TxInput = {
  prev_txid: Hash256 (big-endian),
  prev_index: u32_le,
  script_length: varint,
  script: bytes[],
}

TxOutput = {
  value: u64_le,
  script_length: varint,
  script: bytes[],
}

varint = variable-length integer (little-endian, 1-9 bytes)
```

**Rules:**
- Fields MUST be serialized in the above order.
- No padding or alignment bytes.
- Arrays ordered as specified; no reordering.
- Hash computed as `txid = BLAKE3(canonical_bytes)`.

---

## 17. Transaction and Staking UTXO Details

### 17.1 Transaction Format
**Wire format:** See §16.3. This section elaborates on script semantics.

### 17.2 Staking UTXO Script System (NORMATIVE)
§5.3 references "on-chain staking UTXO" but requires detailed script semantics for implementation.

**Staking Output Script (Lock Script):**
```
OP_STAKE <tier_id: u8> <pubkey: 33 bytes> <unlock_height: u32> <op_unlock: 1 byte>
```

**Semantics:**
- `tier_id`: maps to tier weights (§5.2)
- `pubkey`: node's Ed25519 public key (masternode identity)
- `unlock_height`: earliest checkpoint block height at which stake can be withdrawn
- `op_unlock`: control byte for future extension

**Unlock/Withdrawal (Unlock Script):**
```
<signature: Ed25519Sig> <unlock_witness: bytes>
```

Must satisfy:
1. Signature from `pubkey` is valid over the spending transaction
2. Current checkpoint block height ≥ `unlock_height`

**Stake Maturation:**
- A staking output is **mature** once included in a checkpoint block.
- A masternode may only join the AVS after stake maturity.
- Weight corresponds to the locked amount's tier (§5.2).

**Tier Changes:**
- Require a new staking output to be created
- Old stake must be withdrawn before new stake becomes active
- AVS membership transitions enforce via heartbeat attestation grace period

### 17.3 Regular Transaction Outputs (Non-Staking)
```
<value: u64_le> <lock_script>

lock_script = {
  OP_CHECKSIG <pubkey_hash: 20 bytes>
  |
  OP_MULTISIG <m: u8> <pubkey1> ... <pubkeyn> <n: u8>
  |
  OP_RETURN <data: bytes> (unspendable)
}
```

---

## 18. Network Transport Layer (NORMATIVE)

### 18.1 Transport Protocol
**REQUIREMENT:** Specify the transport medium for §11 messages.

```
TRANSPORT_PROTOCOL = TCP (currently unencrypted)
Encryption: TLS v1.3 support (infrastructure ready, integration pending)
Development: Plain TCP for simplicity and debugging
```

**Current Implementation:**
- Raw TCP via `tokio::net::TcpStream`
- No transport-layer encryption (v1.0.0)
- TLS infrastructure exists in `src/network/secure_transport.rs` and `src/network/tls.rs`
- Future versions will integrate TLS encryption

**Justification:**
- TCP provides reliable, ordered delivery with universal compatibility
- Simpler debugging during initial deployment
- TLS v1.3 support ready for future integration
- Focus on protocol correctness before adding encryption overhead

**Security Note:**
- Current P2P communication is **not encrypted**
- Deploy nodes on trusted networks or use VPN/SSH tunneling
- Message-level signing provides authentication (even without encryption)
- TLS integration planned for v1.1.0

### 18.2 Message Framing
All messages MUST be length-prefixed:

```
Frame = {
  length: u32_be (network byte order, excludes this field),
  message_type: u8,
  payload: bytes[length - 1],
}
```

**Max message size:** `4 MB`  
**Connection limits:** `MAX_PEERS = 125` (inbound + outbound)

### 18.3 Serialization Format
**REQUIREMENT:** Pin message serialization.

```
SERIALIZATION_FORMAT = bincode v1.0 (or protobuf v3 for external APIs)
- bincode: compact, deterministic, suitable for internal wire protocol
- protobuf: forward-compatible, suitable for stable RPC APIs
```

Implementations MUST define a mapping from §11 Rust enums to wire bytes.

### 18.4 Peer Discovery and Bootstrap
**Bootstrap Process:**
1. Node reads hardcoded bootstrap peer list (DNS seeds or IP addresses).
2. Connects to bootstrap peers via TCP (with optional TLS).
3. Requests `PeerListRequest` to discover additional peers.
4. Maintains peer database; prefer geographic diversity and low latency.

**DNS Seeds (REQUIRED for mainnet):**
```
seed1.timecoin.dev
seed2.timecoin.dev
seed3.timecoin.dev
```

(To be populated by network operators.)

**Message Type:**
```rust
PeerListRequest { limit: u16 },
PeerListResponse { peers: Vec<PeerInfo> },

pub struct PeerInfo {
    pub addr: IpAddr,
    pub port: u16,
    pub services: u32,  // bitmap: validator, full_node, light_client
}
```

---

## 19. Genesis Block and Initial State (NORMATIVE)

### 19.1 Genesis Block Format
```rust
pub struct GenesisBlock {
    pub chain_id: u32,
    pub timestamp: u64,  // Unix seconds
    pub initial_utxos: Vec<UTXOEntry>,
    pub initial_avs: Vec<InitialValidatorEntry>,
}

pub struct UTXOEntry {
    pub txid: Hash256,
    pub output_index: u32,
    pub value: u64,
    pub script: bytes,
}

pub struct InitialValidatorEntry {
    pub mn_id: Hash256,  // derived from pubkey hash
    pub pubkey: [u8; 32],
    pub vrf_pubkey: [u8; 32],
    pub tier_weight: u16,
}
```

### 19.2 Bootstrap Procedure (Chicken-Egg Problem)
**Challenge:** AVS is required to validate, but AVS membership is on-chain.

**Solution:**
1. Genesis block specifies `initial_avs` set (pre-agreed by operators).
2. Each initial validator MUST stake on-chain in the first few blocks.
3. Once staking transaction is archived, stake becomes eligible.
4. AVS membership is then enforced by heartbeat + witness attestation (§5.4).

**Testnet Genesis (example):**
```json
{
  "chain_id": 1,
  "timestamp": 1703376000,
  "initial_avs": [
    {
      "mn_id": "mn_1...",
      "pubkey": "...",
      "tier_weight": 100
    }
  ]
}
```

### 19.3 Chain ID Assignment
- **Mainnet:** `chain_id = 1`
- **Testnet:** `chain_id = 2`
- **Devnet:** `chain_id = 3`

All signed objects (§8.1, §5.4) MUST include the correct `chain_id` to prevent replay attacks.

---

## 20. Clock Synchronization Requirements (NORMATIVE)

### 20.1 Wall-Clock Dependency
TimeLock (§9) relies on wall-clock time for slot alignment. Clocks MUST be synchronized to within a tight tolerance.

```
CLOCK_SYNC_REQUIREMENT = NTP v4 (RFC 5905) or GPS/PTP
MAX_CLOCK_DRIFT = ±10 seconds (acceptable per node)
```

### 20.2 Slot Boundary Grace Period
```
SLOT_GRACE_PERIOD = 30 seconds
- Blocks with slot_time in [current_slot - 30s, current_slot + 30s] are accepted
- Prevents legitimate blocks from being rejected due to minor clock skew
```

### 20.3 Future Block Rejection
```
FUTURE_BLOCK_TOLERANCE = 5 seconds
- Reject blocks with slot_time > now() + 5s
- Defends against attacks by nodes with skewed clocks
```

### 20.4 NTP Configuration (Recommended)
```
# /etc/ntp.conf (Linux) or equivalent
server 0.pool.ntp.org iburst
server 1.pool.ntp.org iburst
server 2.pool.ntp.org iburst
server 3.pool.ntp.org iburst

# Ensure systemd-timesyncd or ntpd is running
# Check: ntpq -p (or timedatectl status)
```

---

## 21. Light Client and SPV Support (OPTIONAL)

### 21.1 Light Client Model
Clients that cannot run full validation (e.g., mobile wallets) MAY:
- Verify transactions against **TimeProof** (§8) rather than replaying TimeVote consensus
- Query trusted peers for AVS snapshots (§8.4)
- Verify TimeProof signatures against AVS snapshot at transaction's `slot_index`

### 21.2 Block Header Format for Light Clients
```rust
pub struct BlockHeader {
    pub height: u64,
    pub slot_index: u64,
    pub slot_time: u64,
    pub prev_block_hash: Hash256,
    pub producer_id: Hash256,
    pub vrf_output: [u8; 32],
    pub vrf_proof: bytes,
    pub finalized_root: Hash256,  // Merkle root of entries
    pub timestamp_ms: u64,
}
```

### 21.3 Merkle Proof for Entry Verification
Light clients can verify that a specific `(txid, timeproof_hash)` is included in a block:

```rust
pub struct EntryProof {
    pub txid: Hash256,
    pub timeproof_hash: Hash256,
    pub inclusion_path: Vec<Hash256>,  // Merkle path to finalized_root
    pub leaf_index: u32,
}

// Verify: compute_merkle_root(txid || timeproof_hash, inclusion_path, leaf_index) == block.finalized_root
```

### 21.4 Trust Model
Light clients MUST:
1. Trust the canonical **header chain** (validated via VRF sortition).
2. Trust AVS snapshots returned by queried peers (or require multiple confirmations).
3. Assume TimeProof signature verification is correct (standard Ed25519).

---

## 22. Error Recovery and Edge Cases (NORMATIVE)

### 22.1 Conflicting TimeProofs
**Issue (§8.7):** Two conflicting transactions both obtain valid TimeProofs.

**Safety violation:** One or more AVS members produced signatures for conflicting transactions, or signatures were forged.

**Recovery:**
```
ON_CONFLICTING_TIMEPROOF:
  1. Detect: compare (txid_A, timeproof_A) vs (txid_B, timeproof_B) for same input outpoint
  2. Log: record both TimeProofs and all signatories as emergency event
  3. Halt: stop automatic finalization for that outpoint
  4. Surface: alert operators and light clients
  5. (Future) Governance: require manual intervention or protocol upgrade
     to slash dishonest validators if cryptographic proof of fraud exists
```

### 22.2 Network Partition Recovery
**Scenario:** Network splits; subsets temporarily cannot reach each other.

**Local behavior:**
- Each partition continues local consensus and block production
- Transactions finalize independently in each partition

**Reconnection:**
```
ON_RECONNECTION:
  1. Exchange block headers across partitions
  2. Canonical chain = partition with highest cumulative AVS weight (sum of all blocks' producers' weight)
  3. Minority partition rolls back uncommitted TimeProofs (§8.6)
  4. Replay finalized transactions from majority onto minority's UTXO set
```

**Implementation note:** Requires persistent block storage and reorg logic.

### 22.3 Orphan Transaction Handling
**Scenario:** A transaction references an input UTXO that has not yet been checkpointed.

**Behavior:**
```
ORPHAN_TXS:
  1. Keep in separate orphan pool (max 1000 entries, by LRU)
  2. When referenced UTXO is archived, retry orphan pool
  3. If orphan not resolved after 72 hours, evict
```

### 22.4 AVS Membership Disputes
**Scenario:** Node claims a masternode is AVS-active, but heartbeat attestations disagree.

**Resolution:**
```
MEMBERSHIP_VERIFICATION:
  - Require ≥ WITNESS_MIN (default 3) valid witness attestations
  - If dispute, request attestations from multiple peers
  - Canonical membership = result from peers with highest total weight
  - Cache locally for 1 heartbeat period (60s)
```

---

## 23. Address Format and Wallet Integration (NORMATIVE)

### 23.1 Address Encoding
```
ADDRESS_FORMAT = bech32m (BIP 350)
ADDRESS_PREFIX = "time1" (mainnet)
ADDRESS_PREFIX = "timet" (testnet)
```

**Example address:** `time1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx`

### 23.2 Address Generation
```
address = bech32m_encode("time1", RIPEMD160(SHA256(pubkey)))
```

### 23.3 Wallet RPC API (Recommended)
Implementations SHOULD expose a JSON-RPC 2.0 interface:

```json
{
  "jsonrpc": "2.0",
  "method": "sendtransaction",
  "params": { "tx": "<hex>" },
  "id": 1
}

{
  "jsonrpc": "2.0",
  "method": "gettransaction",
  "params": { "txid": "<hash>" },
  "id": 2
}

{
  "jsonrpc": "2.0",
  "method": "getbalance",
  "params": { "address": "<bech32>" },
  "id": 3
}
```

---

## 24. Mempool Management and Fee Estimation (NORMATIVE)

### 24.1 Mempool Size and Limits
```
MAX_MEMPOOL_SIZE = 300 MB
MAX_ENTRIES_PER_BLOCK = 10,000
MAX_BLOCK_SIZE = 2 MB
EVICTION_POLICY = lowest_fee_rate_first
```

### 24.2 Transaction Expiry
```
TX_EXPIRY_PERIOD = 72 hours
- Transactions not finalized within 72 hours are evicted from mempool
- Prevents mempool bloat from stuck transactions
```

### 24.3 Fee Estimation
Wallets should estimate fees based on:
```
fee_per_byte = median(fees_in_recent_finalized_txs / tx_size)
// or dynamic algorithm observing mempool congestion
```

**Minimum fee:** `MIN_FEE = 0.001 TIME per transaction`

---

## 25. Economic Model (NORMATIVE)

### 25.1 Initial Supply
```
INITIAL_SUPPLY = 0 (fair launch with no pre-mine)
```

### 25.2 Reward Schedule

**Implementation:**
```
Per checkpoint block (§10):
BLOCK_REWARD = 100 TIME (fixed)
Total reward = BLOCK_REWARD + transaction_fees

Blocks per year: 52,596 (600-second intervals)
Annual issuance: 5,259,600 TIME/year (constant)
```

**Inflation Rate Projection:**

| Year | Total Supply | Annual Inflation | Notes |
|------|--------------|------------------|-------|
| 1 | 5.26M TIME | ∞% → 0% | Bootstrap year |
| 5 | 26.3M TIME | 20% | Early growth |
| 10 | 52.6M TIME | 10% | Maturing network |
| 20 | 105M TIME | 5% | Stable operation |
| 50 | 263M TIME | 2% | Long-term equilibrium |

**Key Characteristics:**
- **Fixed issuance** (not supply-capped like Bitcoin)
- **Decreasing inflation rate** (percentage declines as supply grows)
- **Perpetual security budget** (always incentivizes validators)
- **No halving events** (predictable for economic planning)

### 25.3 Economic Philosophy: Transactional Utility Over Scarcity

**Design Goal:** TIME Coin is optimized as a **medium of exchange**, not a store of value.

**Rationale:**

Bitcoin's scarcity-driven model (21M cap) created a fundamental tension:
- **Success → High value → Poor for everyday transactions**
- Small purchases become impractical (e.g., $0.50 coffee requires 0.000001 BTC)
- Users hoard rather than spend (Gresham's law: "good money drives out bad")
- Psychological barrier: "I paid 10,000 BTC for pizza" regret

**TIME Coin's Alternative Approach:**

1. **Stable Value Target**
   - Predictable, modest inflation creates stable purchasing power
   - Similar philosophy to fiat currencies or algorithmic stablecoins
   - Goal: 1 TIME ≈ consistent real-world value over years
   - Encourages spending and circulation (velocity of money)

2. **Transaction-First Economics**
   - Low, predictable fees (§24: 0.001 TIME minimum)
   - Fast finality (<1s) removes friction from daily use
   - Economic incentives favor network usage over hoarding

3. **Security Through Utility**
   - Bitcoin's long-term security depends on fees (after 2140)
   - TIME Coin maintains security through perpetual issuance
   - No "fee pressure crisis" when block rewards end
   - Validator incentives remain strong regardless of transaction volume

4. **Controlled Inflation vs. Hyperinflation**
   - Fixed 100 TIME/block ≠ unlimited money printing
   - Inflation rate naturally decreases: 20% (year 5) → 2% (year 50)
   - Similar to Ethereum post-merge (~1-2% annual issuance)
   - Predictable, algorithmic issuance (not central bank discretion)

**Comparison:**

| Aspect | Bitcoin | TIME Coin |
|--------|---------|-----------|
| **Supply Cap** | 21M BTC (hard cap) | No cap (decreasing % inflation) |
| **Issuance** | Halving every 4 years | Fixed 100/block forever |
| **Philosophy** | Digital gold / Store of value | Digital cash / Medium of exchange |
| **User Behavior** | Encouraged to hold | Encouraged to transact |
| **Long-term Security** | Dependent on fees alone | Perpetual block rewards |
| **Price Stability** | High volatility expected | Stable value preferred |
| **Use Case** | Savings / Investment | Daily transactions / Payments |

**Trade-offs Acknowledged:**

❌ **Not optimized for:**
- "Number go up" investment thesis
- Scarcity-driven value appreciation
- Deflationary store-of-value narrative

✅ **Optimized for:**
- Everyday payments and transactions
- Predictable purchasing power
- Network utility and adoption
- Long-term validator economics
- Real-world usability

**Governance Note:**

This economic model is subject to community consensus. Future governance proposals may:
- Implement fee burning (reduce net inflation during high usage)
- Adjust reward schedule based on network maturity
- Introduce supply cap if community decides scarcity is preferred
- Modify parameters through on-chain voting (requires protocol upgrade)

**Reference:** This design philosophy aligns with Satoshi Nakamoto's original Bitcoin whitepaper subtitle: "A Peer-to-Peer **Electronic Cash System**" — emphasizing the transactional use case that Bitcoin's scarcity model ultimately moved away from.

### 25.4 Reward Distribution

**Per Block:**
```
Total Reward = 100 TIME + transaction_fees

Distribution:
- 100% to Active Validator Set (AVS) masternodes
- Proportional to tier reward weight (Gold=100000, Silver=10000, Bronze=1000, Free=100)
- No block producer premium (all masternodes share equally by weight)
- No treasury allocation (pure validator rewards)
- Masternodes sorted canonically by address; last node receives remainder
```

**Example Distribution (100 masternodes):**
```
AVS Composition:
- 1 Gold (reward_weight 100000) → ~47.6% of rewards
- 10 Silver (reward_weight 10000 each) → ~47.6% of rewards
- 89 Free (reward_weight 100 each) → ~4.2% of rewards

For a 100 TIME block:
- Gold masternode: ~47.6 TIME
- Each Silver: ~4.76 TIME
- Each Free: ~0.048 TIME
```

**Fair APY Design:**
- Rewards proportional to collateral (weight reflects stake)
- All tiers earn similar % return on investment
- Encourages network decentralization (low barrier to entry)
- No winner-take-all dynamics

See §10 for technical reward calculation details.

See §10 for details.

---

## 26. Implementation Checklist

Before shipping to mainnet, implementations MUST address:

- [ ] Cryptographic primitives finalized (§16: BLAKE3, ECVRF, serialization)
- [ ] Transaction format fully specified and tested (§17.3)
- [ ] Staking script semantics implemented (§17.2)
- [ ] Network transport, framing, and serialization defined (§18)
- [ ] Peer discovery and bootstrap process working (§18.4)
- [ ] Genesis block format and initialization tested (§19)
- [ ] Clock synchronization verified (NTP running, offset < 10s) (§20)
- [ ] Mempool eviction and fee estimation functioning (§24)
- [ ] Conflicting TimeProof detection and logging in place (§22.1)
- [ ] Network partition recovery tested (§22.2)
- [ ] Address format and RPC API standardized (§23)
- [ ] Reward calculation verified with test vectors (§25)
- [ ] Block size and entry count limits enforced (§24.1)
- [ ] Test vectors created for all cryptographic operations (§26)

---

## 27. Test Vectors

All implementations MUST verify against the following test vectors (to be populated during implementation):

```yaml
test_vectors:
  canonical_tx_serialization:
    - input: { version: 1, inputs: [...], outputs: [...] }
      output_hex: "..."
      txid: "..."

  vrf_output:
    - sk: "..."
      prev_block_hash: "..."
      slot_time: 600
      chain_id: 1
      output: "..."
      proof: "..."

  finality_vote_signature:
    - vote: { chain_id: 1, txid: "...", voter_mn_id: "..." }
      signature: "..."
      verification: true

  timeproof_threshold:
    - avs_size: 10
      avs_weight: 100
      q_finality: 67
      vote_weight: 68
      valid: true

  timevote_state_transitions:
    - status: "Voting"
      accumulated_weight: 67
      required_weight: 67
      poll_result: "Valid"
      expected_new_status: "Finalized"

  block_validity:
    - block_hash: "..."
      vrf_proof_valid: true
      entries_sorted: true
      no_conflicts: true
      valid: true
```

---

