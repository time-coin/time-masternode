# Pure Avalanche Consensus Architecture

## Overview

TimeCoin implements **pure Avalanche consensus** without Byzantine Fault Tolerance assumptions. This document describes the updated consensus architecture after removing all BFT references.

## Key Changes from BFT to Avalanche

### 1. **Finality Threshold Model**

#### ❌ **Old (BFT-based)**
- Required 2/3 (67%) of stake for finality
- Assumed Byzantine fault tolerance
- Static threshold across all conditions

#### ✅ **New (Avalanche-based)**
- Requires **majority stake** (>50%) for finality
- Uses continuous sampling-based voting
- Threshold: `threshold = (total_stake + 1) / 2`
- Quorum parameters from Avalanche protocol:
  - **k = 20**: Sample size (validators queried per round)
  - **α = 14**: Quorum threshold (minimum positive responses needed)
  - **β = 20**: Confidence threshold (consecutive confirmations for finality)

### 2. **Voting Mechanism**

| Aspect | BFT | Avalanche |
|--------|-----|-----------|
| **Approach** | All-or-nothing voting | Continuous sampling & polling |
| **Threshold** | 2/3 of all validators | Majority of sampled validators |
| **Finality** | Binary (finalized/unfinalized) | Probabilistic → deterministic |
| **Confirmation** | Single round | β consecutive rounds |
| **Fault Model** | Byzantine (assumes worst-case dishonesty) | Crash fault (assumes rational actors) |

### 3. **Implementation Changes**

#### **TSDC Config** (`src/tsdc.rs`)
```rust
// Before:
pub struct TSCDConfig {
    pub slot_duration_secs: u64,
    pub finality_threshold: f64,  // ❌ Removed
    pub leader_timeout_secs: u64,
}

// After:
pub struct TSCDConfig {
    pub slot_duration_secs: u64,
    pub leader_timeout_secs: u64,
}
```

#### **Finality Proof Manager** (`src/finality_proof.rs`)
```rust
// Before: Checked if votes >= (total_avs_weight * 67) / 100
let threshold = (total_avs_weight * 67).div_ceil(100);

// After: Check if votes >= (total_avs_weight + 1) / 2 (majority)
let threshold = (total_avs_weight + 1) / 2;
```

#### **Block Finalization** (`src/tsdc.rs`)
All block finality checks now use majority stake consensus:
```rust
// Consensus check for finality
let threshold = (total_stake + 1) / 2; // Majority stake
if signed_stake > threshold && !state.is_finalized {
    state.is_finalized = true;
    // Block is finalized with Avalanche consensus
}
```

## Avalanche Protocol Parameters

### Current Configuration

```rust
pub struct AvalancheConfig {
    pub sample_size: usize,         // k = 20 validators per round
    pub quorum_size: usize,         // α = 14 (quorum threshold)
    pub finality_confidence: usize, // β = 20 (consecutive confirms)
    pub query_timeout_ms: u64,      // 2000ms per round
    pub max_rounds: usize,          // 100 max rounds
}
```

### Finality Mechanism

1. **Initial Acceptance**: Transaction accepted by local Avalanche sample
2. **Continuous Polling**: Round-by-round querying of random validator samples
3. **Threshold Achievement**:
   - Query k=20 validators per round
   - Need α=14 confirmations (>70% of sample) for that round
   - Track consecutive confirmations (β=20)
4. **Final Finality**: After β=20 consecutive rounds of quorum achievement, transaction is finalized

### Fault Tolerance

- **Avalanche model**: Can tolerate up to ~50% crash faults (non-malicious)
- **Without Byzantine assumption**: No protection against coordinated attacks by >50% stake
- **In practice**: Requires active monitoring and governance for > 50% stake concentration

## Consensus Flow

```
┌──────────────────────────────────────────────────────────────┐
│                    TRANSACTION ARRIVAL                        │
└──────────────────────────────────────────────────────────────┘
                            ↓
┌──────────────────────────────────────────────────────────────┐
│    AVALANCHE SAMPLING (Phase 1: Continuous Voting)           │
│  - Query k=20 random validators                              │
│  - Wait for responses (timeout: 2s)                          │
│  - Track preferences for each conflicting transaction        │
└──────────────────────────────────────────────────────────────┘
                            ↓
                    ┌───────────────┐
                    │ QUORUM REACHED│
                    │  (α=14 votes) │
                    └───────────────┘
                            ↓
            ┌───────────────────────────────┐
            │ CONFIDENCE LOOP (β=20 rounds) │
            │ - Continue sampling           │
            │ - Track consecutive confirms  │
            └───────────────────────────────┘
                            ↓
              ┌────────────────────────────┐
              │  FINALITY ACHIEVED         │
              │ (β consecutive rounds ✓)   │
              └────────────────────────────┘
                            ↓
┌──────────────────────────────────────────────────────────────┐
│    VFP GENERATION (Verifiable Finality Proof)               │
│  - Collect finality votes from validators                   │
│  - Check majority stake threshold (>50%)                    │
│  - Create immutable proof record                            │
└──────────────────────────────────────────────────────────────┘
                            ↓
┌──────────────────────────────────────────────────────────────┐
│  TSDC CHECKPOINT (Every 10 minutes)                         │
│  - Finalized transactions batched into block                │
│  - Deterministic block ordering via VRF sortition          │
│  - Cryptographic commitment on-chain                       │
└──────────────────────────────────────────────────────────────┘
```

## Security Properties

### ✅ **What Avalanche Provides**
- **Instant finality**: No blockchain reorganizations once finalized
- **Probabilistic → Deterministic**: Local acceptance → global VFP
- **Stake-weighted voting**: Larger validators have proportionally more influence
- **Censorship resistance**: Decentralized sampling prevents single-point control
- **Liveness**: Continues despite network failures (no stop-the-world)

### ⚠️ **What Avalanche Does NOT Provide**
- **Byzantine fault tolerance**: No assumption about adversarial validators
- **Protection from >50% attacks**: Majority stake can finalize any transaction
- **Formal safety guarantees**: Probabilistic, not mathematically proven for all conditions

### 🛡️ **TimeCoin's Mitigation**
1. **Masternode collateral**: Stake-based participation (skin in the game)
2. **Heartbeat attestation**: Validators prove continuous participation
3. **Governance oversight**: Community monitoring of validator set composition
4. **Slashing (future)**: Economic penalties for detected misbehavior

## Testing & Validation

### Avalanche Consensus Tests
- ✅ Quorum achievement with k=20, α=14
- ✅ Confidence accumulation (β=20)
- ✅ Finality proof validation
- ✅ Majority stake threshold (>50%)
- ✅ Network partition recovery
- ✅ Validator sampling distribution

### Integration Tests
- ✅ Transaction → Avalanche vote → VFP → Block finalization pipeline
- ✅ Multi-round consensus with network latency
- ✅ Stake-weighted sampling correctness

## Configuration

### Production Parameters (Mainnet)
```yaml
avalanche:
  sample_size: 20           # Query 20 validators per round
  quorum_size: 14          # Need 14+ confirmations (70%)
  finality_confidence: 20  # 20 consecutive rounds for finality
  query_timeout_ms: 2000   # 2 second timeout
  max_rounds: 100          # Max 100 rounds before abort

tsdc:
  slot_duration_secs: 600   # 10 minutes between blocks
  leader_timeout_secs: 5    # 5 second leader timeout

consensus:
  finality_threshold: 0.5   # Majority stake (>50%) for finality
```

### Testnet Parameters
```yaml
avalanche:
  sample_size: 10           # Smaller sample for testing
  quorum_size: 7           # 70% of 10
  finality_confidence: 5   # Faster finality for testing
  query_timeout_ms: 1000
  max_rounds: 50

tsdc:
  slot_duration_secs: 60    # 1 minute blocks for testing
  leader_timeout_secs: 3
```

## Future Enhancements

1. **Hybrid consensus**: Consider optional Byzantine threshold with economic incentives
2. **Adaptive parameters**: Dynamic α/β based on network conditions
3. **Stake slashing**: Implement penalties for detected misbehavior
4. **VRF-based sampling**: More sophisticated validator selection
5. **Parallel chains**: Increase throughput with multiple Avalanche instances

## References

- **Protocol Spec**: `docs/TIME_COIN_PROTOCOL_V6.md` §7 (Avalanche Consensus)
- **Consensus Theory**: Ava Labs whitepaper (2018)
- **Implementation**: `src/consensus.rs`, `src/avalanche.rs`, `src/tsdc.rs`

---

**Last Updated**: 2025-12-23  
**Status**: ✅ Pure Avalanche Consensus Active (BFT References Removed)
