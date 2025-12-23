# TIME Coin Protocol Specification

**Version:** 5.0 (TSDC + Avalanche Pure Hybrid)
**Last Updated:** December 2024
**Status:** Production Ready

## Table of Contents

1. [Overview](#overview)
2. [Protocol Architecture](#protocol-architecture)
3. [Avalanche Consensus (Instant Finality)](#avalanche-consensus)
4. [Time-Scheduled Deterministic Consensus (TSDC)](#tsdc)
5. [Hybrid Integration](#hybrid-integration)
6. [UTXO State Machine](#utxo-state-machine)
7. [Masternode System](#masternode-system)
8. [Heartbeat Attestation](#heartbeat-attestation)
9. [Reward Distribution](#reward-distribution)
10. [Network Protocol](#network-protocol)
11. [Security Model](#security-model)
12. [Implementation Details](#implementation-details)

---

## Overview

TIME Coin is a next-generation blockchain protocol that separates **state finality** from **chain history**. It achieves this through a novel hybrid architecture:

1.  **Avalanche Consensus (Transaction Layer)**: Provides sub-second, probabilistic instant finality for individual transactions using weighted subsampling.
2.  **Time-Scheduled Deterministic Consensus (Block Layer)**: A deterministic, VRF-based mechanism that packages already-finalized transactions into archival checkpoints (blocks) every 10 minutes.

Unlike traditional blockchains where transactions are finalized *by* blocks, TIME Coin transactions are finalized **before** block inclusion. Blocks serve strictly as historical checkpoints and reward distribution events.

### Key Innovations

-   **Instant Settlement**: <1 second transaction finality via Avalanche Snowball.
-   **Deterministic Checkpointing**: Blocks produced exactly every 10 minutes via TSDC.
-   **Leaderless Finality**: No leaders involved in transaction confirmation; purely peer-to-peer.
-   **No BFT Stalls**: No global committees, no voting rounds, no halting.
-   **Pre-Block Finality**: State becomes immutable minutes before being written to a block.
-   **Stake-Weighted Sampling**: Sybil resistance provided by stake-weighted peer gossip.

---

## Protocol Architecture

### System Flow

The system operates on two distinct time scales: **Real-Time (Transactions)** and **Epoch Time (Blocks)**.

```
┌──────────────────────────────────────────────────────────────┐
│                  Real-Time Layer (Avalanche)                 │
│         Transaction Broadcast -> Instant Finality            │
└──────────────────────────────┬───────────────────────────────┘
                               │
                               ▼
┌──────────────────────────────────────────────────────────────┐
│                  Mempool (Finalized Pool)                    │
│      Transactions waiting for archival (State is Safe)       │
└──────────────────────────────┬───────────────────────────────┘
                               │
                               ▼
┌──────────────────────────────────────────────────────────────┐
│                  Epoch Layer (TSDC Blocks)                   │
│      Every 10 mins: Leader packages Pool -> Block            │
└──────────────────────────────────────────────────────────────┘
```

---

## Avalanche Consensus (Instant Finality)

TIME Coin uses the **Avalanche Snowball** algorithm to achieve consensus on transactions. This replaces traditional BFT voting. There is no global quorum; consensus emerges from repeated local subsampling.

### The Snowball Algorithm

For every transaction $Tx$ conflicting with others (or simply being validated), every masternode runs a local instance of Snowball.

**Parameters:**
-   $n$: Total network size (masternodes).
-   $k$: Sample size (e.g., 20).
-   $\alpha$: Quorum size (e.g., 14).
-   $\beta$: Decision threshold (e.g., 20 consecutive successes).

**Execution Loop (per node):**

1.  **Query**: Select $k$ peers randomly, weighted by stake.
2.  **Request**: Ask for their preferred state (Valid/Invalid) regarding $Tx$.
3.  **Tally**:
    *   If $\ge \alpha$ peers respond "Valid":
        *   Increment `confidence` counter.
        *   Update local `preference` to Valid.
    *   Else:
        *   Reset `confidence` counter to 0.
4.  **Finalize**:
    *   If `confidence` $\ge \beta$: Mark $Tx$ as **Finalized**. Stop sampling.

### Consensus Properties

-   **Probabilistic Safety**: The probability of a safety violation (reorg) drops exponentially as $\beta$ increases.
-   **Fast Termination**: Non-conflicting transactions finalize in ~300ms.
-   **Scalability**: $O(k)$ complexity. Adding nodes does not slow down consensus.
-   **Parallelism**: Thousands of transactions can be sampled concurrently.

---

## Time-Scheduled Deterministic Consensus (TSDC)

TSDC is responsible solely for **creating blocks** and **distributing rewards**. It does not determine transaction validity (Avalanche does that).

### 10-Minute Checkpoint Schedule

-   **Slot Duration**: 600 seconds (10 minutes).
-   **Schedule**: Fixed clock alignment (:00, :10, :20, ...).
-   **Role**: Produce a cryptographic summary of all transactions finalized in the last 10 minutes.

### Deterministic Leader Selection

For Block Height $H$ at scheduled time $T$:

```
Leader(H) = argmin(VRF(Previous_Block_Hash || T, v_i.pubkey))
```

-   **Input**: The hash of the previous block and the target timestamp.
-   **Output**: The node with the lowest VRF output value is the leader.
-   **Backup**: If the leader fails to broadcast by $T + 5s$, the node with the second lowest VRF value takes over immediately.

### Block Content

The leader does **not** choose which transactions to include arbitrarily. The block content is deterministic:

1.  **Input**: All transactions marked `Finalized` by Avalanche since Block $H-1$.
2.  **Ordering**: Lexicographical sort by Transaction ID (TXID).
3.  **Validation**: All nodes verify the block contains *exactly* the set of finalized transactions they have observed.

---

## Hybrid Integration

This section details how Avalanche (Finality) hands off to TSDC (History).

### The Lifecycle of a Transaction

1.  **Broadcast**: User sends $Tx$.
2.  **Locking**: Masternodes lock input UTXOs (state: `Locked`).
3.  **Sampling (Avalanche)**:
    *   Nodes gossip and sample peers.
    *   Is $Tx$ valid? Is it a double spend?
    *   Network converges on "Yes" or "No".
4.  **Finalization (Instant)**:
    *   Node reaches $\beta$ confidence.
    *   $Tx$ state moves to `Finalized`.
    *   **User sees "Confirmed" (< 1s). Funds are safe to spend.**
5.  **Pooling**: $Tx$ sits in the "Finalized Pool" (Mempool).
6.  **Checkpointing (TSDC)**:
    *   10-minute mark arrives.
    *   Leader packages pool into Block $N$.
    *   $Tx$ state moves to `Archived`.
    *   Block is appended to chain; rewards are paid.

---

## UTXO State Machine

The UTXO model is adapted to support pre-block finality.

### State Diagram

```
    ┌─────────┐
    │ Unspent │
    └────┬────┘
         │ Transaction Created
         ▼
    ┌─────────┐
    │  Locked │ ◄─── Prevents Double Spend during Sampling
    └────┬────┘
         │ Avalanche Consensus Running
         ▼
    ┌─────────┐      Snowball Fail
    │ Sampling│ ──────────────────────┐
    └────┬────┘                       │
         │ Snowball Success (β hits)  │
         ▼                            ▼
    ┌─────────┐                  ┌─────────┐
    │Finalized│ ◄── SAFE         │ Invalid │
    └────┬────┘     STATE        └─────────┘
         │
         │ (Waiting for 10m slot)
         ▼
    ┌─────────┐
    │ Archived│ ◄── In Block
    └─────────┘
```

### State Transitions

| State | Condition | Meaning |
|-------|-----------|---------|
| **Unspent** | Default | Available funds. |
| **Locked** | Tx Broadcast | Inputs locked locally while validating. |
| **Sampling** | Validation OK | Actively querying peers via Snowball. |
| **Finalized** | Confidence $\ge \beta$ | **Irreversible**. Funds transferred. |
| **Archived** | Block Inclusion | Permanently recorded in history. |
| **Invalid** | Confidence fails | Tx rejected (e.g., double spend). |

---

## Masternode System

Masternodes provide the "peering surface" for Avalanche sampling.

### Tier Structure

| Tier | Collateral (TIME) | Sampling Weight |
|------|-------------------|-----------------|
| **Free** | 0 | 1 |
| **Bronze** | 1,000 | 10 |
| **Silver** | 10,000 | 100 |
| **Gold** | 100,000 | 1,000 |

### Stake-Weighted Sampling

In Avalanche, Sybil resistance is achieved by weighting the probability of being sampled:

$$ P(sampling\_node\_i) = \frac{Weight_i}{\sum Total\_Network\_Weight} $$

A Gold node is 1,000x more likely to be queried for validation than a Free node. This prevents an attacker from spinning up 1,000,000 free nodes to sway consensus.

---

## Heartbeat Attestation

To ensure sampling queries are only sent to live nodes, TIME Coin uses cryptographic uptime proofs.

### Mechanism

1.  **Pulse**: Every 60s, a Masternode broadcasts a `SignedHeartbeat`.
2.  **Witness**: Peers receiving the heartbeat sign a `WitnessAttestation` and gossip it back.
3.  **Registry**: To remain in the "Active Sampling Set," a node must have a valid heartbeat with $\ge 3$ distinct witness signatures within the last 3 minutes.

This ensures the Avalanche sampling algorithm $k$ only selects from nodes actually online.

---

## Reward Distribution

Rewards are distributed purely based on the 10-minute TSDC blocks.

### Logic

Even though transactions finalize instantly, rewards are calculated per block to minimize computational overhead.

1.  **Block Creation**: Leader builds Block $N$.
2.  **Fee Aggregation**: All fees from finalized transactions in Block $N$ are summed.
3.  **Inflation**: Base block reward is calculated ($100 \times (1 + \ln(n))$).
4.  **Payout**:
    *   Leader receives 10% (Block production incentive).
    *   90% distributed proportionally to all Active Masternodes based on Tier Weight.

---

## Network Protocol

The protocol uses a push-pull gossip mechanism for Avalanche.

### Message Types

```rust
pub enum NetworkMessage {
    // Avalanche Sampling
    SampleQuery {
        txids: Vec<Hash256>,
        request_id: u32
    },
    SampleResponse {
        request_id: u32,
        votes: Vec<VoteResponse> // Valid/Invalid/Unknown
    },

    // Transaction Propagation
    TxBroadcast(Transaction),

    // Block Propagation (Archival)
    BlockBroadcast(Block),

    // Network Maintenance
    Heartbeat(SignedHeartbeat),
    Attestation(WitnessAttestation)
}
```

### Sampling Optimization

Nodes batch multiple Transaction IDs into a single `SampleQuery` to reduce network overhead. A single round-trip can advance the Snowball state for dozens of transactions simultaneously.

---

## Security Model

### Avalanche Security

-   **Safety Threshold**: Classical consensus requires $>\frac{2}{3}$ honest actors. Avalanche maintains safety with $>\frac{1}{2}$ honest stake (parameter dependent).
-   **Liveness**: Guaranteed as long as the network is connected and $>50\%$ of stake is honest.
-   **Sybil Resistance**: Provided by Collateral Weights. A "Free" tier attack requires strictly $>50\%$ of the network's total weighted collateral, not just node count.

### TSDC Security

-   **Deterministic Fallback**: If a leader tries to censor transactions (exclude finalized txs from a block), the block is invalid.
-   **Backup Leaders**: If a leader goes offline, the deterministic backup takes over after 5 seconds, ensuring the 10-minute heartbeat is never missed.

### Conflict Resolution (Double Spends)

If User A sends $Tx_1$ to Bob and $Tx_2$ to Alice using the same inputs:
1.  The network sees two conflicting transactions.
2.  Avalanche naturally forces nodes to choose one.
3.  Snowball dynamics ensure the network rapidly converges to **one** winner.
4.  The loser is discarded; the winner becomes `Finalized`.

---

## Implementation Details

### Stack
-   **Language**: Rust (Tokio/Async).
-   **Consensus**: Custom Snowball implementation.
-   **Crypto**: Ed25519 (Signatures), VRF (Sortition).
-   **Database**: Sled (Local K-V store).

### Configuration Defaults
-   `BLOCK_INTERVAL`: 600 seconds
-   `AVALANCHE_K`: 20
-   `AVALANCHE_ALPHA`: 14
-   `AVALANCHE_BETA`: 20
-   `MIN_FEE`: 0.001 TIME

---

**Conclusion**: This protocol leverages Avalanche for what it does best (speed) and TSDC for what blockchains do best (history/ordering), removing the need for slow BFT voting rounds entirely.
