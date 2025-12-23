```markdown
# TIME Coin Protocol Specification (Improved)
**Document:** `TIMECOIN_PROTOCOL_V6.md`  
**Version:** 6.0 (Avalanche Snowball + TSDC Checkpoints + Verifiable Finality Proofs)  
**Last Updated:** December 2025  
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

---

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
