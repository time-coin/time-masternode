# TIME Coin Protocol Specification (Improved)
**Document:** `TIMECOIN_PROTOCOL.md`  
**Version:** 6.1 (Avalanche Snowball + TSDC Checkpoints + VFP + Liveness Fallback)  
**Last Updated:** January 17, 2026  
**Status:** Implementation Spec (Normative)

---

## Table of Contents

1. [Overview](#1-overview)
2. [Design Goals and Non‑Goals](#2-design-goals-and-non-goals)
3. [System Architecture](#3-system-architecture)
4. [Cryptography and Identifiers](#4-cryptography-and-identifiers)
5. [Masternodes, Weight, and Active Validator Set (AVS)](#5-masternodes-weight-and-active-validator-set-avs)
6. [UTXO Model and Transaction Validity](#6-utxo-model-and-transaction-validity)
7. [Avalanche Snowball Finality](#7-avalanche-snowball-finality)
8. [Verifiable Finality Proofs (VFP)](#8-verifiable-finality-proofs-vfp)
9. [Time-Scheduled Deterministic Consensus (TSDC) Checkpoint Blocks (Archival Chain)](#9-tsdc-checkpoint-blocks-archival-chain)
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

- **Avalanche Snowball (Transaction Layer):** fast, leaderless, stake-weighted sampling that converges on a single winner among conflicting transactions. Nodes can provide **sub‑second local acceptance**.
- **Verifiable Finality Proofs (VFP):** converts local probabilistic acceptance into an **objectively verifiable artifact** that any node can validate offline.
- **TSDC (Block Layer):** deterministic, VRF-sortition checkpoint blocks every 10 minutes. Blocks are **archival** (history + reward events), not the source of transaction finality.

> **Terminology note:** **AVS** means **Active Validator Set** (eligible active masternodes). It is purely a protocol term.

---

## 2. Design Goals and Non‑Goals

### 2.1 Goals
1. **Fast settlement:** typical confirmation < 1s under healthy network conditions.
2. **Leaderless transaction finality:** no global committee rounds for transaction acceptance.
3. **Sybil resistance:** sampling influence proportional to stake weight.
4. **Objective verification:** third parties can verify that a transaction reached finality using a compact proof (VFP).
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
Tx broadcast -> Avalanche sampling -> Local Accepted -> VFP assembled -> Globally Finalized

Epoch-time (Blocks)
Every 10 minutes -> TSDC checkpoint block archives globally-finalized txs + rewards
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
- VRF input MUST bind to `prev_block_hash || slot_time || chain_id`.

---

## 5. Masternodes, Weight, and Active Validator Set (AVS)

### 5.1 Masternode Identity
A masternode has:
- `mn_id` (derived from pubkey)
- `pubkey`
- `weight w` (tier-derived)
- `vrf_pubkey` (may be same key)

### 5.2 Tier Weights
| Tier | Collateral (TIME) | Weight `w` |
|------|-------------------|------------|
| Free | 0 | 1 |
| Bronze | 1,000 | 10 |
| Silver | 10,000 | 100 |
| Gold | 100,000 | 1,000 |

### 5.3 Collateral Enforcement (MUST CHOOSE ONE)
1. **On-chain staking UTXO (RECOMMENDED):** stake locked by a staking script; weight derived from locked amount and tier mapping.
2. **Registry authority:** external registry signs membership updates (not trustless).

This spec assumes **on-chain staking UTXO** unless explicitly configured otherwise.

### 5.4 Active Validator Set (AVS)
Only masternodes in the **AVS** may be:
- sampled for Avalanche queries
- counted for VFP weight thresholds
- eligible to produce/compete for TSDC checkpoint blocks

A masternode is **AVS-active** if:
- It has a valid `SignedHeartbeat` within `HEARTBEAT_TTL` (default 180s), AND
- That heartbeat has ≥ `WITNESS_MIN` attestations (default 3) from distinct AVS-active witnesses.

Nodes MUST maintain and gossip AVS state.

### 5.5 Stake-weighted sampling distribution
Sampling MUST be stake-weighted over AVS:
`P(i) = w_i / Σ_{j∈AVS} w_j`

Sampling SHOULD be without replacement per poll.

---

## 6. UTXO Model and Transaction Validity

### 6.1 UTXO States (per outpoint)
- `Unspent`
- `Locked(txid)` (local reservation)
- `Spent(txid)` (by Globally Finalized tx)
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

Only one txid per outpoint may be Globally Finalized.

---

## 7. Avalanche Snowball Finality

TIME Coin uses stake-weighted Snowball-style repeated sampling. The protocol is defined on **conflict sets** (double spends), while non-conflicting transactions converge trivially.

### 7.1 Parameters
- `k`: sample size (default 20)
- `α`: successful poll threshold (default 14)
- `β_local`: local acceptance threshold (default 20 consecutive successful polls)
- `POLL_TIMEOUT`: default 200ms
- `MAX_TXS_PER_QUERY`: default 64

### 7.2 Responder Rule (Voting)
On receiving a query for txid `X`, the responder returns `VoteResponse`:

- `Valid` if `X` is locally valid AND responder currently prefers `X` for all its input conflict sets.
- `Invalid` if `X` is locally invalid OR responder prefers a conflicting tx for any input.
- `Unknown` if responder cannot evaluate (missing Tx data) or Tx not known.

Responder MUST NOT return `Valid` for two conflicting txs for the same outpoint.

### 7.3 Local Snowball State (per txid)
Each node maintains:
- `status[X] ∈ {Seen, Sampling, LocallyAccepted, GloballyFinalized, Rejected, Archived}`
- `confidence[X]` (consecutive successful polls)
- `counter[X]` (cumulative successful polls; RECOMMENDED)
- Per outpoint preference `preferred_txid[o]`

Tie-breakers MUST be deterministic (RECOMMENDED: lowest `txid` wins ties).

### 7.4 Polling Loop (per txid)
For txid `X` in `Sampling`:

1. Select `k` masternodes from the AVS (stake-weighted).
2. Send `SampleQuery` including `X` (batched allowed).
3. Collect responses until timeout.
4. Let `v = count(Valid votes for X)`.
5. If `v ≥ α`:
   - `counter[X] += 1`
   - `confidence[X] += 1`
   - Update `preferred_txid[o]` for each input outpoint `o` using `argmax(counter[t])` among known conflicts.
6. Else:
   - `confidence[X] = 0`

### 7.5 Local Acceptance
A node MUST set `status[X] = LocallyAccepted` if:
- `confidence[X] ≥ β_local`, AND
- `preferred_txid[o] == X` for all inputs.

When a node locally accepts `X`, it MUST mark all conflicting txs for any input outpoint as `Rejected` locally.

> **Wallet UX:** “Confirmed” MAY correspond to `LocallyAccepted` for sub‑second UX.  
> **Protocol/objective finality:** requires VFP (`GloballyFinalized`).
### 7.6 Liveness Fallback Protocol

Avalanche's probabilistic consensus can stall under adversarial conditions or network partitions. This section defines a **deterministic fallback mechanism** that guarantees bounded recovery time while preserving the leaderless nature of normal operation.

#### 7.6.1 Stall Detection

A node detects a **liveness stall** for transaction `X` if ALL of the following hold:

1. `status[X] == Sampling` for duration `> STALL_TIMEOUT` (default: 30 seconds)
2. `confidence[X] < β_local` (transaction has not reached local acceptance)
3. No conflicting transaction for any input of `X` has reached `GloballyFinalized`
4. Transaction `X` is valid per §6 validation rules

**Rationale:** Distinguishes genuine stalls from consensus-resolved conflicts or invalid transactions.

#### 7.6.2 Stall Evidence and Alert Broadcast

When a node detects a stall for transaction `X`:

1. **Assemble Evidence:**
   ```
   LivenessAlert {
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
   - Send `LivenessAlert` to all connected peers
   - Peers MUST validate signature and that reporter is in AVS
   - Peers SHOULD relay alert if they also observe the stall

#### 7.6.3 Fallback Trigger Threshold

A node enters **Fallback Mode** for transaction `X` when:

- It receives `LivenessAlert` messages from `≥ f+1` distinct masternodes (where `f = ⌊(n-1)/3⌋`)
- All alerts reference the same `txid` and conflict set
- At least `FALLBACK_MIN_DURATION` (default: 20s) has elapsed since first alert

**Safety:** `f+1` alerts guarantees at least one honest node observed the stall (in a BFT-style `n ≥ 3f+1` model).

#### 7.6.4 Deterministic Resolution Round

Upon entering Fallback Mode for transaction `X`:

**Step 1: Freeze Avalanche Sampling**
- Set `status[X] = FallbackResolution`
- Stop sending new `SampleQuery` messages for `X` or any conflicting transaction
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
- `Accept` if `counter[X] > counter[conflicting_tx]` for all conflicts
- `Reject` if any conflict has higher `counter` value
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
If proposal receives `≥ Q_finality` total weight in `Approve` votes (same threshold as VFP §8.3):

1. Set `status[X] = GloballyFinalized` (if decision = Accept) or `Rejected` (if decision = Reject)
2. Assemble `VerifiableFinality` proof (§8) using the collected `FallbackVote` signatures
3. Mark conflicting transactions as `Rejected`
4. Resume normal Avalanche operation for other transactions

**Step 6: Timeout and Retry**
If no decision after `FALLBACK_ROUND_TIMEOUT` (default: 10 seconds):

1. Increment `slot_index += 1` (advances to next leader)
2. Repeat from Step 2 with new leader
3. After `MAX_FALLBACK_ROUNDS` (default: 5), escalate to TSDC checkpoint synchronization

#### 7.6.5 TSDC Checkpoint Synchronization (Ultimate Fallback)

If fallback rounds fail after `MAX_FALLBACK_ROUNDS` attempts:

1. Transaction `X` remains in `Sampling` state
2. Wait for next TSDC block boundary (≤ 10 minutes per §9)
3. TSDC block producer (VRF-selected) MUST include a `LivenessRecoveryFlag` in block header
4. Block producer deterministically resolves all pending liveness stalls:
   - Includes transactions with `counter[X] > 0` (at least one positive vote observed)
   - Excludes transactions with only negative votes
5. All nodes synchronize to TSDC block state
6. Reset Avalanche sampling for any remaining unresolved transactions

> **Note:** This is a rare fallback (expected frequency: < 0.01% of transactions under normal operation).

#### 7.6.6 State Transition Diagram

```
Seen → Sampling ──────────────────────→ LocallyAccepted → (VFP Assembly) → GloballyFinalized
         │                                     ↓
         │ (confidence ≥ β_local)          (Conflicting tx finalized)
         │                                     ↓
         │                                  Rejected
         ↓
    (Stall detected)
         ↓
  FallbackResolution ─────→ GloballyFinalized (if proposal approved)
         │                        or
         │                    Rejected (if proposal rejected)
         ↓
   (Fallback timeout)
         ↓
  (Retry with new leader or TSDC checkpoint)
```

#### 7.6.7 Protocol Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `STALL_TIMEOUT` | 30s | Duration before declaring liveness stall |
| `FALLBACK_MIN_DURATION` | 20s | Minimum time before accepting alerts |
| `FALLBACK_PROPOSAL_TIMEOUT` | 5s | Leader must propose within this time |
| `FALLBACK_VOTE_TIMEOUT` | 5s | Voting period duration |
| `FALLBACK_ROUND_TIMEOUT` | 10s | Total time for one fallback round |
| `MAX_FALLBACK_ROUNDS` | 5 | Attempts before TSDC escalation |
| `ALERT_THRESHOLD` | f+1 | LivenessAlerts needed to trigger (`f = ⌊(n-1)/3⌋`) |

#### 7.6.8 Security Considerations

**Byzantine Resistance:**
- Fallback requires `f+1` alerts → at least one honest node confirms stall
- Leader is deterministic → no leader election attacks
- Voting requires `≥ Q_finality` weight → Byzantine minority cannot force decision
- TSDC provides ultimate synchronization point

**Liveness Guarantees:**
- Worst-case resolution time: `30s (stall) + 5×10s (fallback rounds) + 10min (TSDC) ≈ 11.3 minutes`
- Typical case: `30s (stall) + 10s (single fallback round) = 40 seconds`
- No indefinite deadlock possible (bounded by TSDC interval)

**Comparison to Pure BFT:**
- Avoids view change storms (deterministic leader selection)
- No multi-phase commit complexity (single propose → vote → finalize)
- Fallback is rare (only after Avalanche stalls)
- Normal operation remains leaderless and fast (<1s)

#### 7.6.9 Implementation Requirements

Nodes MUST implement:
1. Stall detection timer per transaction in `Sampling` state
2. `LivenessAlert` message handling and relay logic
3. Deterministic leader computation function
4. `FinalityProposal` and `FallbackVote` message types
5. Fallback voting logic and threshold checking
6. State transition from `FallbackResolution` to final state

Nodes SHOULD implement:
1. Metrics for fallback activation frequency
2. Logging of fallback events for network health monitoring
3. Dashboard indicators when node is in fallback mode

----

## 8. Verifiable Finality Proofs (VFP)

VFP turns local acceptance into an objectively verifiable proof that can be:
- gossiped
- stored
- included (directly or by hash) in checkpoint blocks
- validated by any node without replaying sampling history

### 8.1 Finality Vote
A **FinalityVote** is a signed statement:

`FinalityVote = { chain_id, txid, tx_hash_commitment, slot_index, voter_mn_id, voter_weight, signature }`

Where:
- `tx_hash_commitment = H(canonical_tx_bytes)` (canonical serialization MUST be specified)
- `slot_index` is the slot when the vote is issued (prevents indefinite replay)

Signature covers all fields.

**Eligibility:** A vote counts only if the voter is AVS-active in the referenced `slot_index` (see §8.4).

### 8.2 VFP Definition
A **VFP** for transaction `X` is:

`VFP(X) = { tx, slot_index, votes[] }`

Validity conditions:
1. All `votes[]` signatures verify.
2. All votes agree on `(chain_id, txid, tx_hash_commitment, slot_index)`.
3. Voters are distinct (by `voter_mn_id`).
4. Each voter is a member of the **AVS snapshot** for that `slot_index`.
5. Sum of distinct voter weights `Σ w_i ≥ Q_finality(slot_index)`.

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

### 8.5 Assembling a VFP (How nodes obtain votes)
Any node MAY request signed votes from peers. Recommended flow:
- During normal `SampleQuery`, responders SHOULD include a `FinalityVote` when responding `Valid` (if requested).
- The initiator accumulates unique votes over time until the threshold is met.

### 8.6 Global Finalization Rule
A node MUST set `status[X] = GloballyFinalized` when it has a valid `VFP(X)`.

A node MUST reject any conflicting tx `Y` spending any same outpoint once `X` is `GloballyFinalized`.

### 8.7 Catastrophic conflict
If two conflicting transactions both obtain valid VFPs, the network’s safety assumptions have been violated. Clients SHOULD halt automatic finalization and surface an emergency condition. (Slashing/recovery is out of scope unless separately specified.)

---

## 9. Time-Scheduled Deterministic Consensus (TSDC) Checkpoint Blocks (Archival Chain)

Checkpoint blocks exist to:
- checkpoint history
- provide a reward schedule
- compactly summarize finalized transactions

### 9.1 Slot Timing
- `BLOCK_INTERVAL = 600s`
- `slot_time = slot_index * 600`

### 9.2 Sortition (Deterministic Candidate Ranking)
For each masternode `i` in the AVS at `slot_index`:
- `score_i = VRF(prev_block_hash || slot_time || chain_id, sk_i)`

Lower `score_i` is better.

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
`FinalizedEntry = { txid, vfp_hash }`

Blocks MAY optionally include full `VFP` payloads; otherwise nodes fetch VFPs by hash.

### 9.5 Block validity
A node MUST accept a block only if:
1. `prev_block_hash` matches the current canonical chain tip.
2. VRF proof verifies and binds to `(prev_block_hash, slot_time, chain_id)`.
3. `entries[]` are sorted and unique by txid.
4. For every entry, the referenced VFP is available and valid OR retrievable (implementation may mark as “pending” until fetched).
5. No two included transactions conflict (no outpoint is spent twice).
6. All included transactions are `GloballyFinalized` by VFP and pass base validity checks.

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
`R = 100 * (1 + ln(N))`

`N` MUST be defined as one of:
- `N = |AVS|` at the block’s `slot_index` (RECOMMENDED), or
- total registered masternodes

All nodes MUST use the same definition.

### 10.3 Fee accounting
Fees are the sum of included archived transactions’ fees for the slot.

### 10.4 Payout split
- Producer: 10% of `(R + fees)`
- AVS masternodes: 90% of `(R + fees)` distributed proportional to weight `w`

Payout MUST be represented as one or more on-chain reward transactions included in the checkpoint block (coinbase-style).

---

## 11. Network Protocol

### 11.1 Message Types (Wire)
```rust
pub enum NetworkMessage {
    // Tx propagation
    TxBroadcast { tx: Transaction },

    // Avalanche polling (batched)
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

    // Finality proof gossip
    VfpGossip { txid: Hash256, vfp: Vfp },

    // Blocks
    BlockBroadcast { block: Block },

    // Liveness
    Heartbeat { hb: SignedHeartbeat },
    Attestation { att: WitnessAttestation },
}

pub struct TxVoteBundle {
    pub txid: Hash256,
    pub vote: VoteResponse, // Valid/Invalid/Unknown
    pub finality_vote: Option<FinalityVote>, // present iff vote==Valid and want_votes==true
}

pub enum VoteResponse { Valid, Invalid, Unknown }
```

### 11.2 Anti-replay / validation
All signed messages MUST include `chain_id` and a time/slot domain separator.

Nodes SHOULD rate-limit:
- polling requests per peer
- VFP payload sizes
- transaction relay

---

## 12. Mempool and Pooling Rules

### 12.1 Pools
Nodes maintain:
- `SeenPool`: known but not sampling
- `SamplingPool`: active in Snowball
- `LocallyAcceptedPool`: fast-confirmed
- `FinalizedPool`: has VFP (`GloballyFinalized`)
- `ArchivedPool`: checkpointed

### 12.2 Checkpoint inclusion eligibility
Checkpoint blocks SHOULD include:
- all `FinalizedPool` txs not yet archived,
- subject to size limits.

Blocks MUST NOT include `LocallyAccepted` txs lacking VFP.

---

## 13. Security Model

### 13.1 Assumptions
- A majority (by weight) of the AVS is honest (parameter-dependent).
- Network connectivity allows representative sampling.
- AVS membership/weights are correctly enforced (staking/registry + heartbeats + witnesses).

### 13.2 Safety
- `LocallyAccepted` is probabilistic (tuned by `k, α, β_local`).
- `GloballyFinalized` is objective once a VFP with threshold weight is obtained.

### 13.3 Liveness
If honest weight dominates and the network is connected, honest transactions can gather VFP signatures and be checkpointed.

---

## 14. Configuration Defaults

- `BLOCK_INTERVAL = 600s`
- `AVALANCHE_K = 20`
- `AVALANCHE_ALPHA = 14`
- `AVALANCHE_BETA_LOCAL = 20`
- `Q_FINALITY = 0.67 * total_AVS_weight(slot_index)`
- `HEARTBEAT_PERIOD = 60s`
- `HEARTBEAT_TTL = 180s`
- `WITNESS_MIN = 3`
- `POLL_TIMEOUT = 200ms`
- `MAX_TXS_PER_QUERY = 64`
- `MIN_FEE = 0.001 TIME`
- `AVS_SNAPSHOT_RETENTION = 7 days worth of slots` (RECOMMENDED; exact number depends on `BLOCK_INTERVAL`)

---

## 15. Implementation Notes

1. **AVS Snapshotting:** store AVS membership/weights by slot for verifying VFP voter eligibility.
2. **Bandwidth:** VFPs can be large; prefer `vfp_hash` in blocks + fetch-on-demand.
3. **Conflict handling:** treat conflicts per outpoint; when a VFP is accepted, prune all competing spends.
4. **Archival chain reorg tolerance:** checkpoint blocks are archival; transaction finality comes from VFP. Reorgs should not affect finalized state unless you explicitly couple rewards/state to block order.
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
**REQUIREMENT:** VRF is used in §9 for TSDC sortition. The specification MUST pin a concrete VRF construction:

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
vrf_input = H_BLAKE3(prev_block_hash || uint64_le(slot_time) || uint32_le(chain_id))
(vrf_output, vrf_proof) = VRF_Prove(vrf_sk, vrf_input)
```

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
TSDC (§9) relies on wall-clock time for slot alignment. Clocks MUST be synchronized to within a tight tolerance.

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
- Verify transactions against **VFP** (§8) rather than replaying Snowball
- Query trusted peers for AVS snapshots (§8.4)
- Verify VFP signatures against AVS snapshot at transaction's `slot_index`

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
Light clients can verify that a specific `(txid, vfp_hash)` is included in a block:

```rust
pub struct EntryProof {
    pub txid: Hash256,
    pub vfp_hash: Hash256,
    pub inclusion_path: Vec<Hash256>,  // Merkle path to finalized_root
    pub leaf_index: u32,
}

// Verify: compute_merkle_root(txid || vfp_hash, inclusion_path, leaf_index) == block.finalized_root
```

### 21.4 Trust Model
Light clients MUST:
1. Trust the canonical **header chain** (validated via VRF sortition).
2. Trust AVS snapshots returned by queried peers (or require multiple confirmations).
3. Assume VFP signature verification is correct (standard Ed25519).

---

## 22. Error Recovery and Edge Cases (NORMATIVE)

### 22.1 Conflicting VFPs
**Issue (§8.7):** Two conflicting transactions both obtain valid VFPs.

**Safety violation:** One or more AVS members produced signatures for conflicting transactions, or signatures were forged.

**Recovery:**
```
ON_CONFLICTING_VFP:
  1. Detect: compare (txid_A, vfp_A) vs (txid_B, vfp_B) for same input outpoint
  2. Log: record both VFPs and all signatories as emergency event
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
  3. Minority partition rolls back uncommitted VFPs (§8.6)
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
// Alternative: X TIME reserved for foundation (to be decided)
```

### 25.2 Reward Schedule
```
Per checkpoint block (§10):
R = 100 * (1 + ln(N))
where N = |AVS| at the block's slot_index
```

**Example rewards:**
- N = 10: R ≈ 100 * (1 + 2.30) = 330 TIME
- N = 100: R ≈ 100 * (1 + 4.61) = 561 TIME
- N = 1000: R ≈ 100 * (1 + 6.91) = 791 TIME

**Note:** Logarithmic growth has no hard cap. Consider governance discussion on whether a cap is desired.

### 25.3 Reward Distribution
- **Producer:** 10% of (R + tx_fees)
- **AVS validators:** 90% of (R + tx_fees) proportional to weight

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
- [ ] Conflicting VFP detection and logging in place (§22.1)
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

  vfp_threshold:
    - avs_size: 10
      avs_weight: 100
      q_finality: 67
      vote_weight: 68
      valid: true

  snowball_state_transitions:
    - status: "Sampling"
      confidence: 19
      poll_result: "Valid"
      expected_new_confidence: 20
      expected_new_status: "LocallyAccepted"

  block_validity:
    - block_hash: "..."
      vrf_proof_valid: true
      entries_sorted: true
      no_conflicts: true
      valid: true
```

---
