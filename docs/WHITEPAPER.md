# TIME Coin: A Dual-Layer Proof-of-Time Blockchain with Instant Finality

**Version:** 1.2  
**Date:** March 2026  
**Status:** Testnet Active — Mainnet Pending

---

## Abstract

TIME Coin is a masternode-based blockchain protocol that achieves **sub-second transaction finality** through a novel dual-layer architecture. The first layer, **TimeVote**, provides leaderless, stake-weighted transaction finality in real time before any block is produced. The second layer, **TimeLock**, produces deterministic VRF-sortition checkpoint blocks every 10 minutes to archive finalized transactions and distribute rewards. A compact cryptographic artifact called a **TimeProof** records every finality event and can be verified by any third party without trusting a specific node or waiting for block confirmations.

This design separates the concerns of *state finality* (which must be fast) from *historical archival* (which can afford latency), yielding a system where users receive confirmed, irreversible settlement in under one second under normal network conditions while maintaining the economic incentive structure and long-term historical integrity of a traditional blockchain.

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Design Goals](#2-design-goals)
3. [System Architecture Overview](#3-system-architecture-overview)
4. [Cryptographic Foundations](#4-cryptographic-foundations)
5. [Masternode System and Tiers](#5-masternode-system-and-tiers)
6. [TimeVote Protocol — Transaction Finality](#6-timevote-protocol--transaction-finality)
7. [TimeProof — Verifiable Finality Certificates](#7-timeproof--verifiable-finality-certificates)
8. [TimeLock Checkpoint Blocks](#8-timelock-checkpoint-blocks)
9. [TimeGuard — Liveness Fallback Protocol](#9-timeguard--liveness-fallback-protocol)
10. [UTXO State Machine](#10-utxo-state-machine)
11. [Reward Model and Economics](#11-reward-model-and-economics)
12. [Network Architecture](#12-network-architecture)
13. [Security Model](#13-security-model)
14. [Roadmap](#14-roadmap)
15. [Conclusion](#15-conclusion)

---

## 1. Introduction

Bitcoin demonstrated that a decentralized, trustless monetary system is possible, but its confirmation model requires users to wait ten minutes or more for a single block — and multiple blocks for high-value transactions. Ethereum and its successors improved throughput and reduced block times, but probabilistic finality means that merchants and exchanges must still wait for several confirmations before treating a payment as irreversible.

TIME Coin takes a different approach: **finality is not a property of blocks — finality is a property of votes**. Transactions are finalized by a quorum of stake-weighted masternodes the moment sufficient votes accumulate, typically in under one second. Blocks exist only to create an immutable, verifiable archive of what has already been finalized and to distribute network rewards.

This architecture allows TIME Coin to provide:

- **Instant settlement** for users and merchants
- **Deterministic, auditable history** through 10-minute checkpoint blocks
- **Objective proof** of finality that any party can verify without running a full node
- **Sybil resistance** through tiered staking and a maturity gate for free participants

---

## 2. Design Goals

### 2.1 Goals

| Goal | Mechanism |
|------|-----------|
| Transaction confirmation < 1 second | TimeVote leaderless quorum |
| No trusted intermediary for finality verification | TimeProof cryptographic certificate |
| Proportional influence based on stake | Stake-weighted AVS voting |
| Deterministic block production schedule | VRF sortition + 600-second clock alignment |
| Resilience against temporary node loss | TimeGuard multi-round fallback |
| Sybil resistance for free participants | 72-block maturity gate on mainnet |
| Long-range attack protection | 100-block hard reorg limit |

### 2.2 Non-Goals

- **BFT deterministic ordering before finality** — TIME Coin uses progressive vote accumulation, not multi-round committee consensus, for transaction finality.
- **Global mempool agreement** — Nodes maintain independent mempools; ordering disputes are resolved by the TimeVote vote.
- **Blocks as the source of truth** — Blocks archive; votes finalize. A transaction is irreversible the moment it accumulates 67% stake weight in votes, regardless of whether a block has been produced.

---

## 3. System Architecture Overview

TIME Coin operates on two distinct time scales:

```
Real-time layer (milliseconds to ~1 second)
────────────────────────────────────────────
TX submission → UTXO lock → TimeVote broadcast
    → Vote accumulation → 67% threshold crossed
    → TimeProof assembled → TX FINALIZED

Epoch layer (every 10 minutes)
────────────────────────────────────────────
VRF sortition selects block producer
    → Block archives finalized TXs + rewards
    → 2-Phase Commit among masternodes
    → Block appended to chain
```

The two layers are deliberately decoupled. A transaction can reach finality even if no block has been produced recently — and a block can be produced even if there are no pending transactions. This eliminates the common blockchain failure mode where block production stalls cause transaction processing to stall as well.

---

## 4. Cryptographic Foundations

### 4.1 Digital Signatures — Ed25519

All node identity, vote signing, heartbeat attestations, and block proposals use **Ed25519** (RFC 8032) via the `ed25519-dalek` library.

- **Private key:** 32 bytes
- **Public key:** 32 bytes  
- **Signature:** 64 bytes
- **Security level:** 128-bit equivalent

Every masternode generates an Ed25519 key pair at registration. The private key is stored locally in `time.conf` as a base58check-encoded string (`masternodeprivkey`). The public key is broadcast to the network and permanently associated with the node's IP address.

### 4.2 Hashing — SHA-256

Transaction IDs, block hashes, Merkle roots, TimeProof digests, and address checksums all use **SHA-256** (FIPS 180-4). The type alias `Hash256 = [u8; 32]` is used throughout the codebase.

```
txid = SHA-256(canonical_transaction_bytes)
block_hash = SHA-256(canonical_block_header_bytes)
```

### 4.3 VRF — ECVRF-EDWARDS25519-SHA512-TAI

Block producer selection uses **ECVRF** (RFC 9381) — a verifiable random function that allows any node to privately determine whether it is eligible to produce a given block, and to prove that eligibility to others without requiring interaction.

```
vrf_input  = SHA-256("TIMECOIN_VRF_V2" || height || prev_block_hash)
vrf_output = ECVRF.prove(masternode_privkey, vrf_input)
vrf_proof  = 80 bytes (publicly verifiable)
vrf_score  = first 8 bytes of vrf_output interpreted as big-endian u64
```

A node is eligible to produce a block if its `vrf_score` falls below a threshold proportional to its effective stake weight.

### 4.4 Address Format

TIME Coin addresses are derived from Ed25519 public keys:

```
address = "TIME" + network_digit + Base58(SHA-256(pubkey)[0..20] + checksum[0..4])
```

- **Testnet addresses** begin with `TIME0`
- **Mainnet addresses** begin with `TIME1`
- **Total length:** ~38 characters

---

## 5. Masternode System and Tiers

TIME Coin uses a tiered masternode system that allows participants at different capital levels to contribute to network security while receiving proportional rewards.

### 5.1 Tier Overview

| Tier | Collateral Required | Sampling Weight | Governance | Reward Pool |
|------|--------------------:|:--------------:|:----------:|:-----------:|
| **Free** | 0 TIME | 1× | ❌ | 8 TIME/block (shared) |
| **Bronze** | 1,000 TIME | 10× | ✅ | 14 TIME/block (winner) |
| **Silver** | 10,000 TIME | 100× | ✅ | 18 TIME/block (winner) |
| **Gold** | 100,000 TIME | 1,000× | ✅ | 25 TIME/block (winner) |

*Sampling weight determines a node's relative influence in both TimeVote finality and VRF sortition.*

### 5.2 Collateral Locking

Paid-tier masternodes must lock their collateral on-chain before registering. A `collateral_outpoint` (transaction ID + output index) points to a UTXO holding exactly the required amount. The node is rejected if the UTXO does not exist, has the wrong value, or is already locked by another masternode. The collateral UTXO transitions to `UTXOState::Locked` and cannot be spent while the masternode is active.

### 5.3 Registration and Discovery

Masternodes announce themselves to the network using `MasternodeAnnouncementV3`, which carries the node's address, reward address, tier, and Ed25519 public key. When a peer receives an announcement for a node it has not seen before, it **relays the announcement to all its other connected peers**, propagating knowledge of the new node transitively through the network. This means a node does not need a direct TCP connection to every other masternode to include it in consensus and reward calculations.

### 5.4 Free-Tier Sybil Protection

Free nodes require no financial stake, making them susceptible to Sybil attacks. TIME Coin applies two defenses:

1. **Maturity gate:** On mainnet, a Free-tier node must be online for **72 blocks (~12 hours)** before becoming eligible for VRF sortition or reward pools.
2. **Removal on disconnect:** Free-tier nodes are removed from the Active Validator Set immediately when they disconnect rather than being marked inactive. This prevents a transient flood of free nodes from inflating the quorum denominator and stalling the network.

### 5.5 Active Validator Set (AVS)

The AVS is the set of currently-active, registered masternodes whose votes count toward finality thresholds. Only nodes in the AVS are counted in the denominator when computing the 67% finality threshold. Nodes removed from the registry (disconnected Free nodes) or marked inactive (disconnected paid nodes) are excluded, ensuring that node churn never causes the network to stall waiting for nodes that are offline.

---

## 6. TimeVote Protocol — Transaction Finality

TimeVote is the real-time consensus protocol that finalizes individual transactions. It is **leaderless**: any masternode can initiate voting on any transaction, and votes from multiple masternodes accumulate progressively until the threshold is crossed.

### 6.1 Transaction Flow

```
1. User broadcasts signed transaction
2. Receiving nodes validate (signature, balance, UTXO state)
3. Input UTXOs transition: Unspent → SpentPending
4. TimeVoteRequest broadcast to all AVS members
5. Each AVS member votes Accept or Reject (signed Ed25519)
6. Votes accumulate — running weight tracked
7. When Accept weight ≥ 67% of total AVS weight:
   → TimeProof assembled from collected Accept votes
   → UTXOs transition: SpentPending → SpentFinalized
   → Transaction status: Voting → Finalized
   → TimeProof broadcast to all peers
```

### 6.2 Finality Threshold

```
Q_finality = ⌈0.67 × total_AVS_weight⌉
```

The threshold uses the **total AVS weight** — the sum of sampling weights of all currently active masternodes. Higher-tier masternodes carry proportionally more weight, ensuring that a majority of staked capital must agree before any transaction is finalized.

### 6.3 Vote Types

- **Accept vote:** Counted toward the finality weight. A transaction is finalized when the cumulative weight of Accept votes reaches the threshold.
- **Reject vote:** Signed and recorded but does not count toward finality. Cryptographic signing of Reject votes prevents a validator from changing its position after a network partition.

### 6.4 UTXO Locking

When a transaction is submitted and enters the Voting state, its input UTXOs are immediately locked (`SpentPending`). This prevents double-spend attempts on the same inputs even before finality is confirmed. A conflicting transaction referencing the same UTXOs will be rejected by all nodes during validation.

---

## 7. TimeProof — Verifiable Finality Certificates

A TimeProof is a compact, self-contained artifact that proves a transaction reached the finality threshold. It is assembled automatically as votes arrive — there is no separate assembly step.

### 7.1 Contents

A TimeProof contains:
- The transaction ID being finalized
- The collection of Accept votes (each: voter public key, signature, weight)
- The total accumulated weight at the time of finality
- A hash binding the proof to a specific block height context

### 7.2 Verification

Any third party — including light clients, exchanges, and auditors — can verify a TimeProof without running a full node:

1. Verify that each vote signature is valid under the claimed public key
2. Verify that each public key appears in the known AVS
3. Compute the sum of weights of valid votes
4. Verify the sum ≥ 67% of total AVS weight at the relevant height

A valid TimeProof is an **objective proof** of finality that requires no trust in any particular node.

---

## 8. TimeLock Checkpoint Blocks

Every 10 minutes, one masternode is selected via VRF sortition to produce a checkpoint block. Blocks serve three purposes:

1. **Archive:** Record all transactions finalized since the previous block
2. **Rewards:** Distribute block rewards to the producer and all eligible masternodes
3. **Checkpoint:** Establish a canonical chain height and hash for fork resolution

### 8.1 VRF Sortition

Block production is **permissionless among eligible masternodes**. Any node may self-evaluate its eligibility by computing:

```
effective_weight = sampling_weight + fairness_bonus
fairness_bonus   = blocks_without_reward / 10  (uncapped)
threshold        = (effective_weight / total_weight) × TARGET_PROPOSERS × 2^64
eligible         = vrf_score < threshold
```

`TARGET_PROPOSERS = 1`, targeting exactly one eligible producer per slot. The fairness bonus ensures that nodes which have not produced a recent block are progressively more likely to be selected, preventing long-run dominance by high-tier nodes.

If no block arrives within the expected time window, the threshold is progressively relaxed every 10 seconds, guaranteeing that eventually every eligible node becomes a proposer. Free-tier nodes receive their VRF boost only after 60 seconds of deadlock, providing an additional sybil resistance layer.

### 8.2 3-Block Participation Window

A masternode is eligible for VRF sortition if it participated (as voter or block producer) in any of the **3 most recent blocks**. This rolling window prevents nodes with occasional high latency from being permanently locked out of block production after a single missed vote.

### 8.3 Block Finalization — 2-Phase Commit

Once a candidate block is proposed, it is finalized through a lightweight 2-Phase Commit among connected masternodes:

1. **Prepare:** Validators verify the block and broadcast a signed `TimeVotePrepare`
2. **Precommit:** After ≥50% prepare votes, validators broadcast `TimeVotePrecommit`
3. **Finalize:** Block is added to the chain when precommit threshold is met

### 8.4 Block Header Fields

| Field | Description |
|-------|-------------|
| `height` | Sequential block number |
| `prev_hash` | SHA-256 of previous block header |
| `merkle_root` | Merkle root of archived transactions |
| `timestamp` | Unix timestamp (64-bit, Year 2106 safe) |
| `producer_address` | Block producer IP/address |
| `producer_signature` | Ed25519 signature over block hash |
| `vrf_proof` | 80-byte ECVRF proof |
| `vrf_output` | 32-byte VRF output (score) |
| `active_masternodes_bitmap` | Compact bit vector of active masternodes |
| `consensus_participants_bitmap` | Bit vector of 2PC voters |

---

## 9. TimeGuard — Liveness Fallback Protocol

TimeGuard activates when a transaction stalls in the Voting state for longer than expected. It provides a deterministic, escalating resolution path that guarantees liveness even under partial network failure.

### 9.1 Phases

| Phase | Duration | Action |
|-------|----------|--------|
| **1 — Stall Detection** | 30 seconds | Node detects stall, broadcasts `LivenessAlert` |
| **2 — Alert Accumulation** | ~2 seconds | Wait for f+1 alerts (f = ⌊(n−1)/3⌋) |
| **3 — Leader Election** | Instant | Deterministic: min H(txid ∥ slot ∥ prev_hash ∥ pubkey) |
| **4 — Fallback Vote** | 10 seconds | Leader proposes; AVS votes with 67% threshold |
| **5 — Retry / Escalate** | Up to 5 rounds | Retry up to MAX_FALLBACK_ROUNDS = 5 |
| **6 — TimeLock Recovery** | Next block | Block producer resolves all pending stalled TXs |

### 9.2 Timing Guarantees

- **Typical recovery:** ~40 seconds (stall detection + one fallback round)
- **Worst case:** ~11.3 minutes (5 fallback rounds + next block production)

### 9.3 Deterministic Leader Election

The fallback leader is computed identically by all nodes:

```
leader = argmin H(txid || slot_index || prev_block_hash || mn_pubkey)
                over all AVS members
```

No voting or communication is required to agree on the leader — all nodes derive the same result from the same inputs.

---

## 10. UTXO State Machine

TIME Coin extends Bitcoin's 2-state UTXO model (unspent / spent) into a **5-state lifecycle** that explicitly tracks in-progress finalization.

```
Unspent ──► SpentPending ──► SpentFinalized ──► Archived
               │                                    ▲
               │ (stall detected)                   │
               └──────────────────────────────────►─┘
                          (via TimeGuard recovery)

Unspent ──► Locked  (masternode collateral — cannot be spent)
```

| State | Description |
|-------|-------------|
| `Unspent` | Available to spend |
| `Locked` | Reserved as masternode collateral |
| `SpentPending` | Transaction in TimeVote voting (inputs reserved) |
| `SpentFinalized` | 67% vote threshold reached; TimeProof exists |
| `Archived` | Recorded in a TimeLock checkpoint block |

The key property: UTXOs enter `SpentPending` **before** voting begins, so double-spend conflicts are detected and rejected immediately without waiting for finality.

---

## 11. Reward Model and Economics

### 11.1 Block Reward Distribution

Every 10-minute checkpoint block distributes exactly **100 TIME** to network participants:

| Recipient | Amount | Mechanism |
|-----------|-------:|-----------|
| **Block Producer** | 30 TIME | Leader bonus + fees |
| **Treasury** | 5 TIME | On-chain governance fund |
| **Gold pool** | 25 TIME | Single fairness winner |
| **Silver pool** | 18 TIME | Single fairness winner |
| **Bronze pool** | 14 TIME | Single fairness winner |
| **Free pool** | 8 TIME | Shared among ≤25 active Free nodes |
| **Total** | **100 TIME** | Per block |

Transaction fees are collected separately and added to the block producer's reward on top of the 30 TIME base.

### 11.2 Tier Pool Winner Selection

For paid tiers (Bronze, Silver, Gold), the **single winner** of each tier pool per block is selected by a fairness rotation: the eligible node with the highest *fairness bonus* (blocks since it last received a reward ÷ 10) wins. Ties are broken deterministically by node address. This ensures that over time every eligible node receives roughly equal reward frequency regardless of the network size.

If no eligible node exists for a tier (e.g., no Gold masternodes), that tier's pool amount flows to the block producer.

### 11.3 Free Tier Pool

Up to **25 Free-tier nodes** share the 8 TIME pool equally each block. All active Free-tier nodes that meet the maturity requirement are eligible. The per-node reward decreases as more Free nodes join (8 TIME ÷ n nodes) and increases as nodes leave, naturally self-regulating participation incentives.

### 11.4 Emission Schedule

TIME Coin has **no halving** and no maximum supply. The emission is linear and predictable:

| Period | Emission |
|--------|----------|
| Per block | 100 TIME |
| Per day | 14,400 TIME (144 blocks) |
| Per year | ~5,256,000 TIME |
| Per decade | ~52,560,000 TIME |

The absence of halving events provides stable, predictable incentives for long-term masternode operators without the periodic disruption and miner capitulation seen in halving-based systems.

### 11.5 Bitmap Participation Requirement

A masternode is only eligible for pool rewards if it appeared in the `consensus_participants_bitmap` of a recent block — meaning it actively participated in at least one round of 2-Phase Commit voting. This prevents nodes from joining the network between blocks, collecting rewards for a block they never helped produce, and then leaving.

---

## 12. Network Architecture

### 12.1 Ports

| Network | P2P Port | RPC Port | WebSocket |
|---------|:--------:|:--------:|:---------:|
| Mainnet | 24000 | 24001 | 24002 |
| Testnet | 24100 | 24101 | 24102 |

### 12.2 Connection Model

TIME Coin uses a hybrid connection model:

- **Whitelisted peers** (masternodes in the registry) receive priority connection slots and extended keepalive timeouts (3 minutes vs. 90 seconds for regular peers).
- **Regular peers** fill remaining connection slots and are used for blockchain sync and transaction propagation.
- **Inbound connections** are accepted from any peer that passes rate limiting; known masternodes are automatically whitelisted.

### 12.3 Message Propagation

The primary gossip primitives are:

- **`MasternodeAnnouncementV3`** — relayed to all peers when a new node is seen for the first time, propagating node knowledge throughout the network without requiring full-mesh connectivity.
- **`MasternodeInactive`** — broadcast on disconnect and relayed one hop so all reachable nodes promptly update their AVS.
- **`MasternodeStatusGossip`** — periodic (every 30 seconds) broadcast listing currently-connected peer IPs, used for uptime attestation.
- **`BlockAnnouncement` / `BlockInventory`** — new blocks are announced by hash; peers request the full block only if they don't already have it, reducing bandwidth.

### 12.4 Protocol Magic Bytes

| Network | Magic |
|---------|-------|
| Mainnet | `[0xC0, 0x1D, 0x7E, 0x4D]` |
| Testnet | `[0x54, 0x49, 0x4D, 0x45]` ("TIME") |

Incoming messages are rejected if their magic bytes do not match, providing network-level partition between mainnet and testnet.

### 12.5 Rate Limiting and Blacklisting

- Per-peer rate limiting: 100 requests per 60-second window
- Automatic IP blacklisting after repeated invalid block submissions (≥5)
- Ping/pong keepalive every 30 seconds; disconnect after 3 missed pongs

---

## 13. Security Model

### 13.1 51% / Sybil Attacks

Because finality requires **67% of stake weight** (not 51% of node count), an attacker controlling a large number of low-stake Free-tier nodes gains little influence. To meaningfully threaten finality, an attacker must control more than 33% of total staked TIME, which requires acquiring a substantial fraction of the circulating supply.

The 72-block maturity gate on mainnet ensures that a sudden flood of new Free nodes cannot immediately inflate the AVS and dilute honest voting weight.

### 13.2 Long-Range Attacks

Blocks deeper than **100 from the chain tip** are considered irreversible. The node rejects any reorganization attempt that would modify blocks older than this threshold, regardless of the attacker's claimed chain length. Additionally, `MAX_FORK_SEARCH_DEPTH = 2,000` caps how far back fork resolution searches for a common ancestor.

### 13.3 Double-Spend Prevention

Input UTXOs are locked (`SpentPending`) before voting begins. A second transaction attempting to spend the same UTXO is rejected immediately at the validation stage — it does not need to wait for the first transaction's vote to complete.

### 13.4 Eclipse Attacks

The connection manager maintains separate reserved slots for registered masternodes and actively reconnects to them via the Phase 3 discovery loop. An attacker attempting to eclipse a node by filling all its connection slots with malicious peers would need to also occupy all the reserved masternode slots, requiring control of registered masternodes with locked collateral.

### 13.5 VRF Grinding

A node cannot predict or manipulate its VRF output for a future block without solving SHA-256 preimage problems, because the VRF input is bound to the previous block hash. Block producers cannot selectively withhold blocks to improve their future VRF score without sacrificing their current block reward.

### 13.6 Equivocation

Both Accept and Reject votes must be cryptographically signed. A masternode cannot cast different votes for the same transaction to different peers without producing conflicting signed messages, which are detectable evidence of misbehavior.

---

## 14. Roadmap

### Phase 1 — Network Stability (Q1 2026)

- TLS encryption for P2P connections (infrastructure complete in `src/network/tls.rs`)
- Enhanced fork resolution test coverage
- Network partition detection and automated recovery
- Metrics and monitoring dashboard (`time-dashboard` binary)

### Phase 2 — Feature Enhancement (Q2 2026)

- Smart contracts (basic scripting layer)
- Multi-signature wallet support
- Atomic cross-chain swaps
- Light client / SPV support
- Block explorer
- WebSocket event streaming API (infrastructure complete)
- Mobile wallet compatibility

### Phase 3 — Scalability (Q3 2026)

- Sharding research and prototype
- State compression and pruning
- Parallel transaction validation
- DHT-based peer discovery
- IPv6 support

### Phase 4 — Advanced Features (Q4 2026)

- On-chain governance with tier-weighted voting
- Treasury proposal and disbursement system
- Zero-knowledge proofs for private transactions
- Quantum-resistant signature research

### Mainnet Launch (Target: Q4 2026)

---

## 15. Conclusion

TIME Coin demonstrates that instant, provable transaction finality and a robust archival blockchain are not mutually exclusive. By separating the concern of *when a transaction is final* (answered in under a second by the TimeVote quorum) from the concern of *where transactions are permanently recorded* (answered every 10 minutes by TimeLock blocks), the protocol achieves properties that neither pure proof-of-work nor traditional BFT consensus can offer alone.

The tiered masternode model provides graduated participation: Free nodes bootstrap network density and consensus redundancy without financial barriers; paid tiers provide economic security through locked collateral and increased voting weight. The fairness rotation ensures that every participating node earns rewards predictably over time, supporting a sustainable and decentralized operator ecosystem.

TimeProof certificates make finality objective and portable — a payment confirmed by a TIME Coin TimeProof is as verifiable by an offline auditor as a Bitcoin block inclusion proof, but available in a fraction of a second.

---

## Appendix A — Key Protocol Parameters

| Parameter | Value | Notes |
|-----------|------:|-------|
| Block interval | 600 s | 10 minutes |
| Finality threshold | 67% AVS weight | BFT-safe majority |
| Fallback threshold | 51% AVS weight | Stall recovery |
| Block reward | 100 TIME | Fixed, no halving |
| Producer bonus | 30 TIME | Plus fees |
| Treasury per block | 5 TIME | Governance fund |
| Free maturity gate | 72 blocks | ~12 h, mainnet only |
| Max recipients / Free pool | 25 | Per block |
| Max reorg depth | 100 blocks | ~16.7 hours |
| Stall timeout | 30 s | TimeGuard trigger |
| Max fallback rounds | 5 | Then TimeLock recovery |
| Worst-case finality | ~11.3 min | Via TimeLock recovery |
| Max block size | 1 MB | |
| Signature scheme | Ed25519 | RFC 8032 |
| Hash function | SHA-256 | FIPS 180-4 |
| VRF scheme | ECVRF-EDWARDS25519-SHA512-TAI | RFC 9381 |
| Mainnet P2P port | 24000 | |
| Testnet P2P port | 24100 | |

## Appendix B — Masternode Tier Summary

| Tier | Collateral | Weight | Pool | Recipients |
|------|----------:|-------:|-----:|:----------:|
| Free | 0 TIME | 1× | 8 TIME | Up to 25, shared |
| Bronze | 1,000 TIME | 10× | 14 TIME | 1 winner |
| Silver | 10,000 TIME | 100× | 18 TIME | 1 winner |
| Gold | 100,000 TIME | 1,000× | 25 TIME | 1 winner |

## Appendix C — UTXO State Transitions

```
                    ┌──────────────┐
                    │   Unspent    │
                    └──────┬───────┘
                           │  spend attempt
              ┌────────────┼─────────────┐
              │ collateral  │ transaction  │
              ▼             ▼             │
         ┌────────┐  ┌─────────────┐     │
         │ Locked │  │SpentPending │     │
         └────────┘  └──────┬──────┘     │
                            │ 67% votes  │
                            ▼            │
                   ┌─────────────────┐   │
                   │ SpentFinalized  │   │
                   └────────┬────────┘   │
                            │ in block   │
                            ▼            │
                      ┌──────────┐       │
                      │ Archived │       │
                      └──────────┘       │
                                         │
                   (conflict detected) ──┘
                   (both UTXOs stay Unspent,
                    losing TX is Rejected)
```

---

*TIME Coin is open-source software. The reference implementation is written in Rust and available at [github.com/time-coin/time-masternode](https://github.com/time-coin/time-masternode).*
