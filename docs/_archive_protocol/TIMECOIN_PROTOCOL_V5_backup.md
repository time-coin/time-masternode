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

### System Components

```
┌────────────────────────────────────────────────────────────┐
│                   TIME Coin Node                          │
├────────────────────────────────────────────────────────────┤
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐ │
│  │  Blockchain  │  │ UTXO Manager │  │ Consensus Engine │ │
│  │   (Chain)    │◄─┤ (Lock/Unlock)│◄─┤ (TSDC+Avalanche) │ │
│  └──────────────┘  └──────────────┘  └──────────────────┘ │
│         ▲                 ▲                    ▲            │
│         │                 │                    │            │
│  ┌──────┴─────────────────┴────────────────────┴─────────┐ │
│  │            P2P Network Layer                         │ │
│  └──────────────────────────────────────────────────────┘ │
│         ▲                                         ▲        │
│         │                                         │        │
│  ┌──────┴──────┐                        ┌────────┴─────┐  │
│  │ Masternode  │                        │  VRF Leader  │  │
│  │  Registry   │                        │  Selector    │  │
│  └─────────────┘                        └──────────────┘  │
└────────────────────────────────────────────────────────────┘
```

---

## Time-Scheduled Deterministic Consensus (TSDC)

TSDC provides deterministic, clock-aligned leader election and block production without stalls.

### Slot-Based Architecture

**Time is divided into slots:**
- Slot duration: **5 seconds** (configurable)
- Each slot has exactly **one designated leader**
- Slots are numbered: S₀, S₁, S₂, …

**Formula:** `current_slot = ⌊current_time / 5⌋`

### Leader Selection via VRF

**Deterministic Leader:** For slot Sₖ:
```
L(Sₖ) = argmin(VRF(R || Sₖ, vᵢ.vrf_key)) for all validators vᵢ
```

Where:
- `R` = Epoch randomness (derived from previous finalized block hash)
- `||` = Concatenation
- `VRF()` = Verifiable Random Function (ed25519-based)
- Ties broken by validator ID

**Guarantee:** Every slot has exactly one deterministic leader, known in advance.

### Block Production in Slot Sₖ

1. **Leader L(Sₖ) forms block Bₖ:**
   - `parent_hash` = chain_head (or last_finalized_block)
   - `slot` = Sₖ
   - `txs` = mempool entries (up to max size)
   - `vrf_proof` = VRF(R || Sₖ, leader_vrf_key)
   - `timestamp` = current_time

2. **Leader broadcasts PREPARE(Bₖ, vrf_proof)**

3. **Validators verify and sample:**
   - Verify VRF proof
   - Verify block structure
   - If valid: broadcast PRECOMMIT(Bₖ, SIGNᵢ(Bₖ))

### Handling Missing Leaders

**If no PREPARE(Bₖ) within slot duration:**
- Slot Sₖ is treated as empty
- Leader for S(ₖ₊₁) may set `parent_hash = last_valid_block`
- Sets `prev_slot_reference = NULL` to indicate gap

**Result:** Consensus continues without stall; forks resolve via finality.

---

## Avalanche Instant Finality

Avalanche provides sub-second finality through weighted-majority sampling, replacing multi-round BFT voting.

### Avalanche Snowball Algorithm

Each validator runs a **Snowball instance** for each proposed block:

```
State Variables:
  preference ∈ {Block_A, Block_B, …}  // Current preferred block
  confidence : Integer                  // Finality counter
  k : Integer                          // Sample size (default: 8)
  β : Integer                          // Finality threshold (default: 20)
```

### Update Rule (Each Round)

1. **Sample k peers** (stake-weighted): 
   - Probability(selecting validator vᵢ) ∝ vᵢ.stake

2. **Query sampled peers:** "What's your preference for this block?"

3. **Count responses:**
   - count_yes = peers preferring this block
   - count_no = peers preferring other blocks

4. **Update decision:**
   - If count_yes > count_no AND count_yes > k/2:
     - confidence += 1
     - If confidence ≥ β: **Block is finalized**
   - Else if count_no > count_yes:
     - preference = other block
     - confidence = 0

5. **Repeat until finality or preference stabilizes**

### Key Properties

| Property | Value |
|----------|-------|
| Expected finality time | ~300ms for n=100 validators |
| Communication per block | O(k log n) messages |
| Safety threshold | >50% honest stake |
| Liveness threshold | >50% honest stake + connectivity |
| Byzantine resilience | <33% malicious validators |

### Avalanche vs. Traditional BFT

| Aspect | PBFT/BFT | Avalanche |
|--------|----------|-----------|
| Rounds needed | O(log n) | O(log log n) |
| Finality time | 3-5 seconds | <1 second |
| Communication | O(n²) | O(k log n) |
| Scalability | ~100 validators | 1000+ validators |
| Leader dependency | Critical | Low (sampling-based) |

---

## Hybrid Integration: TSDC + Avalanche

### Consensus Flow

```
┌─────────────────────────────────────────────────────────────┐
│ Slot Sₖ Begins (Time t = k * 5 seconds)                    │
└──────────────────────┬──────────────────────────────────────┘
                       │
        ┌──────────────▼──────────────┐
        │ VRF Leader Selection        │
        │ L(Sₖ) = argmin(VRF(...))   │
        └──────────────┬──────────────┘
                       │
        ┌──────────────▼──────────────┐
        │ Leader broadcasts PREPARE   │
        │ Block Bₖ created            │
        └──────────────┬──────────────┘
                       │
        ┌──────────────▼──────────────┐
        │ Validators receive block    │
        │ Verify structure & VRF      │
        └──────────────┬──────────────┘
                       │
        ┌──────────────▼──────────────┐
        │ Avalanche Sampling Begins   │
        │ Query k sampled validators  │
        └──────────────┬──────────────┘
                       │
           ┌───────────┴───────────┐
           │                       │
    ┌──────▼──────┐        ┌─────▼──────┐
    │ confidence  │        │ confidence │
    │ reaches β   │        │ < β after  │
    │             │        │ timeout    │
    └──────┬──────┘        └─────┬──────┘
           │                     │
    ┌──────▼──────┐        ┌─────▼──────┐
    │ Block       │        │ Reject or  │
    │ Finalized   │        │ Retry      │
    │ <300ms      │        │ Next slot  │
    └─────────────┘        └────────────┘
```

### Timeline

- **t=0ms**: Slot begins; leader determined by VRF
- **t=0-100ms**: Leader broadcasts block; validators sample
- **t=100-300ms**: Avalanche rounds (typically 3-4 rounds)
- **t=300ms**: Block finality reached (~300ms total)
- **t=5000ms**: Next slot begins

### State Synchronization

1. **UTXO locks** are acquired when block receives >50% initial sampling
2. **UTXO commits** happen when block reaches 2/3 stake finality
3. **Rollback** occurs if block fails Avalanche (locks are released)

---

## UTXO State Machine

TIME Coin implements a sophisticated UTXO lifecycle with lock/unlock/commit semantics for atomic safety.

### State Diagram

```
    ┌─────────┐
    │ Unspent │ ◄─── Initial state
    └────┬────┘
         │ lock_utxo(outpoint, txid)
         ▼
    ┌─────────┐
    │  Locked │ ◄─── Prevents double-spend
    └────┬────┘
         │ Block enters Avalanche sampling
         ▼
┌──────────────┐
│    Voting    │ ◄─── Sampled by other validators
└──────┬───────┘
       │
       ├─── confidence ≥ β ────┐
       │                       ▼
       │              ┌────────────────┐
       │              │   Finalized    │ ◄─── Instant finality!
       │              │ commit_spend() │
       │              └────────────────┘
       │
       └─── timeout ────► unlock_utxo() ────► Unspent
```

### State Transitions

| Transition | Function | Condition |
|-----------|----------|-----------|
| Unspent → Locked | `lock_utxo()` | UTXO exists in storage |
| Locked → Voting | Automatic | Block enters Avalanche |
| Voting → Finalized | `commit_spend()` | confidence ≥ β |
| Locked → Unspent | `unlock_utxo()` | Lock timeout OR tx rejected |
| Finalized → [removed] | Storage cleanup | UTXO is spent; removed |

### Implementation Details

```rust
#[derive(Debug, Clone)]
pub enum UTXOState {
    Unspent,
    Locked {
        txid: Hash256,
        locked_at: i64,
    },
    Voted {
        txid: Hash256,
        votes_yes: usize,
        votes_no: usize,
    },
    Finalized {
        txid: Hash256,
        block_height: u64,
    },
}

pub struct UTXOStateManager {
    storage: Arc<dyn UtxoStorage>,
    utxo_states: DashMap<OutPoint, UTXOState>,
}
```

### State Descriptions

#### 1. Unspent
- **Meaning**: UTXO is available for spending
- **Conditions**: No active locks
- **Transitions**: → Locked (when used as tx input)

#### 2. Locked
- **Meaning**: Reserved for a specific transaction
- **Purpose**: Prevents double-spending at protocol level
- **Metadata**: Locked by transaction ID
- **Transitions**: 
  - → SpentPending (tx broadcast)
  - → Unspent (timeout/failure)

#### 3. SpentPending
- **Meaning**: Transaction broadcast, awaiting consensus
- **Metadata**: Vote count, masternode votes
- **Duration**: Typically 1-3 seconds
- **Transitions**:
  - → SpentFinalized (≥ 2/3 votes)
  - → Unspent (< 2/3 votes or rejection)

#### 4. SpentFinalized
- **Meaning**: **Transaction irreversible** - instant finality achieved
- **Guarantees**: 
  - Cannot be double-spent
  - Guaranteed inclusion in next block
  - Full settlement complete
- **Transitions**: → Confirmed (block inclusion)

#### 5. Confirmed
- **Meaning**: Included in blockchain block
- **Purpose**: Permanent historical record
- **No further transitions**: Final state

### Atomic State Transitions

All state changes are:
- **Atomic**: Succeed completely or fail completely
- **Broadcast**: All masternodes notified in real-time
- **Synchronized**: Network maintains consistent state
- **Auditable**: Full state history available

---

## Instant Finality

### Transaction Lifecycle

```
Step 1: Submit Transaction
    │
    ├─ Validate inputs exist
    ├─ Verify sufficient balance
    └─ Lock input UTXOs (state: Locked)
    │
    ▼
Step 2: Broadcast to Network
    │
    ├─ Send to all masternodes
    ├─ UTXOs → SpentPending
    └─ Vote request initiated
    │
    ▼
Step 3: Masternode Voting (Parallel)
    │
    ├─ Masternode 1: Validate + Vote
    ├─ Masternode 2: Validate + Vote
    ├─ Masternode 3: Validate + Vote
    └─ ... (all masternodes vote)
    │
    ▼
Step 4: Consensus Decision
    │
    ├─ Count votes
    ├─ Check quorum: need ⌈2n/3⌉ votes
    │
    ├─ IF votes ≥ quorum:
    │   ├─ Input UTXOs → SpentFinalized
    │   ├─ Create output UTXOs (Unspent)
    │   └─ ✅ INSTANT FINALITY ACHIEVED
    │
    └─ IF votes < quorum:
        ├─ Unlock input UTXOs → Unspent
        └─ ❌ Transaction rejected
    │
    ▼
Step 5: Block Inclusion (every 10 min)
    │
    ├─ Collect all finalized transactions
    ├─ Build deterministic block
    └─ UTXOs → Confirmed
```

### Finality Guarantees

Once a transaction reaches **SpentFinalized**:

1. **Irreversible**: Cannot be rolled back
2. **Guaranteed**: Will be in the next block
3. **Settled**: Funds are fully transferred
4. **Spendable**: New outputs can be spent immediately

### Performance

- **Finality Time**: < 3 seconds (typically 1-2 seconds)
- **Throughput**: Network bandwidth limited, not consensus limited
- **Scalability**: O(n) with masternode count
- **Latency**: Sub-second with 100 masternodes

---

## BFT Consensus

TIME Coin uses Byzantine Fault Tolerant consensus adapted for UTXO-based transactions.

### Quorum Requirements

```
Required Votes = ⌈2n/3⌉  (ceiling of 2/3 of total masternodes)

Examples:
- 3 masternodes: need 2 votes (67%)
- 10 masternodes: need 7 votes (70%)
- 100 masternodes: need 67 votes (67%)
- 1000 masternodes: need 667 votes (67%)
```

### Voting Process

#### 1. Transaction Validation

Each masternode independently validates:
- ✅ All input UTXOs exist
- ✅ Input UTXOs are in correct state (Unspent or Locked)
- ✅ Sum of inputs ≥ sum of outputs + fees
- ✅ Signatures are valid (if signing enabled)
- ✅ No double-spending attempts

#### 2. Vote Casting

```rust
pub struct Vote {
    pub txid: Hash256,
    pub voter: String,          // Masternode address
    pub approve: bool,           // true = approve, false = reject
    pub timestamp: i64,
    pub signature: Signature,    // Ed25519 signature
}
```

#### 3. Vote Aggregation

- Votes collected in real-time
- Each masternode votes once per transaction
- Votes are broadcast to all peers
- System counts votes continuously

#### 4. Consensus Determination

```rust
if approved_votes >= quorum {
    finalize_transaction();  // Instant finality
} else if total_votes >= total_masternodes {
    reject_transaction();    // Not enough support
} else {
    continue_waiting();      // More votes needed
}
```

### Byzantine Fault Tolerance

- **Tolerance**: Up to ⌊n/3⌋ malicious masternodes
- **Safety**: Cannot finalize invalid transactions
- **Liveness**: Progress guaranteed with > 2/3 honest nodes
- **Consistency**: All honest nodes agree on finalized state

### Attack Resistance

#### Double-Spend Attack
**Attack**: Try to spend same UTXO twice  
**Defense**: UTXO locking at protocol level, atomic state transitions  
**Result**: Second transaction fails validation (UTXO already locked)

#### Censorship Attack
**Attack**: Malicious masternodes refuse to vote  
**Defense**: Only need 2/3 honest nodes  
**Result**: Transaction proceeds with honest votes

#### Network Partition
**Attack**: Split network into isolated groups  
**Defense**: Time synchronization, gossip protocol  
**Result**: Smaller partition cannot reach quorum, larger partition continues

---

## Masternode System

### Tier Structure

| Tier | Collateral | Block Rewards | Governance | Weight | Governance Voting |
|------|-----------|---------------|------------|--------|-------------------|
| **Free** | 0 TIME | ✅ Yes | ✅ Yes | 100 | ❌ No |
| **Bronze** | 1,000 TIME | ✅ Yes | ✅ Yes | 1,000 | ✅ Yes |
| **Silver** | 10,000 TIME | ✅ Yes | ✅ Yes | 10,000 | ✅ Yes |
| **Gold** | 100,000 TIME | ✅ Yes | ✅ Yes | 100,000 | ✅ Yes |

### Free Tier Philosophy

The **Free Tier** enables:
- **Zero-barrier Entry**: Anyone can run a masternode
- **Network Decentralization**: More nodes = more security
- **Community Participation**: Contribute without capital
- **BFT Consensus**: Full voting on transactions
- **Economic Rewards**: Earn TIME for securing network

**Limitation**: No governance voting to prevent Sybil attacks

### Masternode Requirements

#### Minimum Requirements (All Tiers)
- **CPU**: 1 core
- **RAM**: 2 GB
- **Disk**: 10 GB SSD
- **Network**: 100 Mbps, <100ms latency
- **Uptime**: >95% recommended

#### Setup Process

1. **Install Node**:
```bash
cargo build --release
```

2. **Configure**:
```toml
[masternode]
enabled = true
tier = "free"  # or bronze/silver/gold
wallet_address = "TIME1your_wallet_address_here"
```

3. **Start**:
```bash
./target/release/timed
```

4. **Verify**:
```bash
./target/release/time-cli masternode status
```

### Masternode Duties

1. **Transaction Validation**: Vote on all transactions
2. **Heartbeat Broadcasting**: Send signed heartbeat every 60 seconds
3. **Witness Attestation**: Attest to other masternodes' heartbeats
4. **Block Validation**: Verify deterministic blocks
5. **Network Participation**: Maintain P2P connections
6. **State Synchronization**: Keep UTXO set current

---

## Heartbeat Attestation

TIME Coin implements **peer-verified uptime** to prevent Sybil attacks and uptime fraud.

### Problem Statement

Traditional masternode systems allow self-reported uptime, enabling:
- Fake nodes claiming historical uptime
- Timestamp manipulation
- Collusion to vouch for offline nodes
- No cryptographic proof of availability

### Solution: Cryptographic Attestation

#### 1. Signed Heartbeats

Every 60 seconds, each masternode broadcasts:

```rust
pub struct SignedHeartbeat {
    pub masternode_address: String,
    pub sequence_number: u64,        // Monotonically increasing
    pub timestamp: i64,               // Unix timestamp
    pub masternode_pubkey: VerifyingKey,  // Ed25519 public key
    pub signature: Signature,         // Ed25519 signature
}
```

**Security Properties**:
- Non-forgeable (Ed25519 signatures)
- Replay-resistant (sequence numbers)
- Time-bound (3-minute validity window)
- Self-verifying (any node can validate)

#### 2. Witness Attestations

When a masternode receives a heartbeat, it creates an attestation:

```rust
pub struct WitnessAttestation {
    pub heartbeat_hash: [u8; 32],     // Hash of heartbeat
    pub witness_address: String,
    pub witness_pubkey: VerifyingKey,
    pub witness_timestamp: i64,       // When witness saw it
    pub signature: Signature,         // Witness signature
}
```

#### 3. Verification Quorum

A heartbeat is **verified** when:
- ✅ Valid masternode signature
- ✅ ≥ 3 independent witness attestations
- ✅ All attestations valid
- ✅ Timestamp within 3-minute window

### Attack Resistance

#### Sybil Attack
**Attack**: Spin up fake nodes with claimed history  
**Defense**: New nodes start at sequence 1, need 3+ real witnesses  
**Result**: Cannot fake historical uptime

#### Timestamp Manipulation
**Attack**: Claim heartbeats from past/future  
**Defense**: 3-minute validity window, monotonic sequences  
**Result**: Old/future heartbeats rejected

#### Collusion Attack
**Attack**: Small group attests fake heartbeats  
**Defense**: Need 3 witnesses, pseudo-random selection, public audit  
**Result**: Expensive, easily detected

#### Replay Attack
**Attack**: Reuse old heartbeats  
**Defense**: Strictly increasing sequence numbers  
**Result**: Duplicates rejected

### Cryptographic Primitives

- **Signature Scheme**: Ed25519 (Curve25519)
  - 128-bit security level
  - 64-byte signatures, 32-byte keys
  - ~60,000 signatures/sec verification
- **Hash Function**: SHA-256
- **Time Source**: NTP + consensus time

---

## Block Production

Blocks are produced **deterministically** every 10 minutes using clock-aligned timestamps.

### Block Schedule

```
Block Height: 0,  1,  2,  3, ...
Timestamp:    :00, :10, :20, :30, ... (minutes)

Examples:
Block 1: 2024-12-14 00:00:00 UTC
Block 2: 2024-12-14 00:10:00 UTC
Block 3: 2024-12-14 00:20:00 UTC
```

### Block Generation Algorithm

```rust
fn generate_block(height: u64, timestamp: i64) -> Block {
    // 1. Get all finalized transactions
    let transactions = get_finalized_transactions();
    
    // 2. Sort transactions by TXID (deterministic)
    transactions.sort_by(|a, b| a.txid().cmp(&b.txid()));
    
    // 3. Calculate rewards
    let rewards = calculate_rewards(
        active_masternodes(),
        base_reward + transaction_fees
    );
    
    // 4. Build coinbase transaction
    let coinbase = create_coinbase(height, rewards);
    
    // 5. Assemble block
    let mut all_txs = vec![coinbase];
    all_txs.extend(transactions);
    
    // 6. Calculate merkle root
    let merkle_root = calculate_merkle_root(&all_txs);
    
    Block {
        header: BlockHeader {
            version: 1,
            height,
            previous_hash: get_block_hash(height - 1),
            merkle_root,
            timestamp,
            block_reward: base_reward + fees,
        },
        transactions: all_txs,
        masternode_rewards: rewards,
    }
}
```

### Deterministic Properties

Every masternode produces **identical blocks** because:

1. **Fixed Timestamp**: Clock-aligned to 10-minute intervals
2. **Sorted Transactions**: Alphabetical by TXID
3. **Sorted Masternodes**: Alphabetical by address
4. **Deterministic Rewards**: Same calculation on all nodes
5. **Merkle Root**: Calculated from sorted transactions

### Block Validation

```rust
fn validate_block(block: &Block) -> bool {
    // 1. Check timestamp alignment
    if block.header.timestamp % 600 != 0 {
        return false;
    }
    
    // 2. Verify all transactions are finalized
    for tx in &block.transactions[1..] {  // Skip coinbase
        if !is_finalized(&tx.txid()) {
            return false;
        }
    }
    
    // 3. Verify merkle root
    if block.header.merkle_root != calculate_merkle_root(&block.transactions) {
        return false;
    }
    
    // 4. Verify reward distribution
    if !verify_rewards(&block.masternode_rewards) {
        return false;
    }
    
    true
}
```

### Fork Resolution

If nodes generate different blocks (should never happen with correct implementation):

1. Compare block hashes
2. Query consensus from peers
3. Adopt chain with most masternode agreement
4. Rollback conflicting transactions
5. Regenerate block

---

## Reward Distribution

### Base Block Reward (Logarithmic)

```
R = 100 × (1 + ln(n))

Where:
- R = Total block reward in TIME
- n = Total active masternodes
- ln = Natural logarithm
```

**Examples**:
- 10 masternodes: ~330 TIME/block
- 100 masternodes: ~560 TIME/block
- 1,000 masternodes: ~790 TIME/block

### Distribution Formula

```
Node Reward = (Total Block Reward × Node Weight) / Total Network Weight
```

### Weight Calculation

```
Free:   weight = 100
Bronze: weight = 1,000
Silver: weight = 10,000
Gold:   weight = 100,000
```

### Example Distribution

**Network**: 10 Free, 5 Bronze, 2 Silver, 1 Gold  
**Block Reward**: 440 TIME

```
Total Weight = 10×100 + 5×1,000 + 2×10,000 + 1×100,000
             = 1,000 + 5,000 + 20,000 + 100,000
             = 126,000

Free node:   (440 × 100) / 126,000 = 0.35 TIME
Bronze node: (440 × 1,000) / 126,000 = 3.49 TIME
Silver node: (440 × 10,000) / 126,000 = 34.92 TIME
Gold node:   (440 × 100,000) / 126,000 = 349.21 TIME
```

### Special Case: Free Nodes Only

If only Free tier masternodes exist, they share rewards equally (no weight penalty). This ensures network bootstrap viability.

### Annual Returns

Estimated APY (assuming 100% uptime, 100 masternodes):

| Tier | Collateral | Est. Annual | Est. APY |
|------|-----------|-------------|----------|
| Free | 0 | Variable | N/A |
| Bronze | 1,000 | ~183,000 | ~18,300% |
| Silver | 10,000 | ~1,830,000 | ~18,300% |
| Gold | 100,000 | ~18,300,000 | ~18,300% |

*Note: Returns decrease as network grows and total supply increases*

---

## Transaction Fees

### Fee Structure

- **Base Fee**: 0.1% of transaction amount
- **Minimum**: 0.001 TIME (dust protection)
- **Calculation**: `fee = inputs - outputs`

### Examples

| Amount | Fee (0.1%) |
|--------|-----------|
| 100 TIME | 0.1 TIME |
| 1,000 TIME | 1.0 TIME |
| 10,000 TIME | 10.0 TIME |

### Fee Distribution

All fees added to block reward and distributed proportionally by masternode weight.

### Validation

```rust
let min_fee = outputs.sum() * 0.001;
let actual_fee = inputs.sum() - outputs.sum();

if actual_fee < min_fee {
    return Err("Insufficient fee");
}
```

---

## Network Protocol

### Message Types

```rust
pub enum NetworkMessage {
    // Handshake
    Handshake { magic: [u8; 4], protocol_version: u32, network: String },
    Ack { message_type: String },
    
    // Transactions
    TransactionBroadcast(Transaction),
    TransactionFinalized { txid: Hash256, votes: usize },
    TransactionRejected { txid: Hash256, reason: String },
    
    // Blocks
    BlockAnnouncement(Block),
    GetBlockHeight,
    BlockHeightResponse { height: u64 },
    GetBlock { height: u64 },
    BlockResponse(Option<Block>),
    
    // UTXO State
    UTXOStateUpdate { outpoint: OutPoint, state: UTXOState },
    GetUTXOState { outpoint: OutPoint },
    UTXOStateResponse { outpoint: OutPoint, state: Option<UTXOState> },
    
    // Masternodes
    MasternodeAnnouncement { address: String, reward_address: String, tier: MasternodeTier, public_key: VerifyingKey },
    GetMasternodes,
    MasternodesResponse(Vec<MasternodeAnnouncementData>),
    
    // Heartbeats
    HeartbeatBroadcast(SignedHeartbeat),
    HeartbeatAttestation(WitnessAttestation),
    
    // Peer Discovery
    GetPeers,
    PeersResponse(Vec<String>),
    
    // Consensus
    Vote(Vote),
    GetPendingTransactions,
    PendingTransactionsResponse(Vec<Transaction>),
}
```

### Connection Flow

```
Client                                Server
  │                                     │
  ├──── TCP Connect ───────────────────►│
  │                                     │
  ├──── Handshake ─────────────────────►│
  │                                     │
  │◄─── Ack ─────────────────────────── │
  │                                     │
  ├──── MasternodeAnnouncement ────────►│  (if masternode)
  │                                     │
  │◄─── GetPeers ───────────────────────│
  │                                     │
  ├──── PeersResponse ─────────────────►│
  │                                     │
  │◄─── GetMasternodes ─────────────────│
  │                                     │
  ├──── MasternodesResponse ───────────►│
  │                                     │
  │      ... ongoing message exchange   │
  │                                     │
```

### Peer Discovery

1. Bootstrap from seed peers
2. Request peer list: `GetPeers`
3. Receive peer addresses: `PeersResponse`
4. Connect to discovered peers
5. Share peer list with others

**Result**: Decentralized mesh network

### Connection Management

- **Max Peers**: Configurable (default 50)
- **Duplicate Prevention**: Track both inbound and outbound
- **Connection Limit**: Reject inbound if already have outbound
- **Automatic Retry**: Exponential backoff for failed connections

---

## Security Model

### Threat Model

TIME Coin assumes:
- **Network**: Partially synchronous
- **Adversary**: Can control up to 33% of masternodes
- **Byzantine Nodes**: May behave arbitrarily maliciously

### Security Guarantees

#### Safety
- **No double-spends**: UTXO locking prevents protocol-level
- **Finality**: Transactions cannot be reversed after 2/3 votes
- **Consistency**: All honest nodes agree on finalized state

#### Liveness
- **Progress**: Network makes progress with >2/3 honest nodes
- **Availability**: Services available even under attack
- **Censorship Resistance**: Cannot prevent valid transactions

#### Privacy
- **Pseudonymous**: Addresses not linked to real identity
- **UTXO Model**: Better privacy than account-based
- **Network Privacy**: P2P gossip obscures origin

### Attack Vectors & Mitigations

#### 1. Double-Spend Attack
**Mitigation**: UTXO locking, atomic state transitions, BFT consensus

#### 2. Sybil Attack (Uptime Fraud)
**Mitigation**: Peer-attested heartbeats, 3-witness quorum, Ed25519 signatures

#### 3. 51% Attack
**Mitigation**: Need 67% to compromise (BFT threshold), expensive collateral

#### 4. Network Partition
**Mitigation**: Time synchronization (NTP), gossip protocol, quorum requirements

#### 5. Eclipse Attack
**Mitigation**: Diverse peer connections, peer exchange protocol, seed nodes

#### 6. DDoS Attack
**Mitigation**: Rate limiting, IP blacklisting, connection limits

#### 7. Replay Attack
**Mitigation**: Sequence numbers, timestamps, nonce values

---

## Implementation Details

### Technology Stack

- **Language**: Rust 2021 Edition
- **Async Runtime**: Tokio
- **Serialization**: Serde (JSON), Bincode (binary)
- **Cryptography**: Ed25519-Dalek, SHA-256
- **Storage**: Sled (embedded database)
- **Networking**: TCP with async I/O

### Performance Characteristics

#### Measured Performance
- **Transaction Processing**: ~1ms per tx
- **BFT Consensus**: <10ms (3 masternodes)
- **Block Generation**: <5ms
- **Signature Verification**: ~16μs per signature
- **Memory Usage**: ~50MB base + UTXO set

#### Scalability
- **Transaction Throughput**: 1,000+ TPS
- **Consensus Latency**: <100ms (100 masternodes)
- **UTXO Set**: O(n) growth with transactions
- **Storage**: ~1MB per day

### Code Structure

```
src/
├── main.rs                    # Entry point
├── types.rs                   # Core data structures
├── utxo_manager.rs            # UTXO state machine
├── consensus.rs               # BFT consensus engine
├── blockchain.rs              # Blockchain storage
├── masternode_registry.rs     # Masternode tracking
├── heartbeat_attestation.rs   # Uptime verification
├── peer_manager.rs            # Peer discovery
├── wallet.rs                  # Wallet management
├── address.rs                 # Address encoding
├── bft_consensus.rs           # BFT coordination
├── transaction_pool.rs        # Mempool management
├── block/
│   ├── types.rs              # Block structures
│   ├── generator.rs          # Block production
│   └── genesis.rs            # Genesis block
├── network/
│   ├── message.rs            # Protocol messages
│   ├── server.rs             # P2P server
│   ├── client.rs             # P2P client
│   ├── connection_manager.rs # Connection tracking
│   ├── rate_limiter.rs       # Rate limiting
│   ├── blacklist.rs          # IP blacklist
│   ├── tls.rs                # TLS encryption
│   └── signed_message.rs     # Message signing
├── rpc/
│   ├── server.rs             # RPC server
│   └── handler.rs            # RPC methods
└── storage/
    ├── mod.rs                # Storage trait
    └── sled_storage.rs       # Sled implementation
```

### Dependencies

```toml
tokio = { version = "1.38", features = ["full"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
ed25519-dalek = { version = "2.0", features = ["serde"] }
sha2 = "0.10"
sled = "0.34"
chrono = "0.4"
```

---

## Conclusion

TIME Coin Protocol represents a significant advancement in blockchain technology by combining:

1. **Instant Finality**: Sub-3-second irreversible settlement
2. **Zero-Barrier Entry**: Free tier masternodes
3. **Cryptographic Security**: Peer-attested uptime, Ed25519 signatures
4. **Deterministic Consensus**: Reproducible blocks, BFT voting
5. **Fair Economics**: Proportional rewards, logarithmic supply

The protocol is production-ready and suitable for:
- Point-of-sale payments
- Real-time settlements
- Micropayments
- Financial applications
- Decentralized applications (dApps)

---

## References

- Bitcoin UTXO Model: https://bitcoin.org/bitcoin.pdf
- Tendermint BFT: https://tendermint.com/static/docs/tendermint.pdf
- Ed25519 Signatures: https://ed25519.cr.yp.to/
- Practical Byzantine Fault Tolerance: Castro & Liskov, OSDI 1999

---

**For more information**:
- GitHub: https://github.com/time-coin/timecoin
- Documentation: https://github.com/time-coin/timecoin/tree/main/docs
- Community: (Discord/Telegram links)
