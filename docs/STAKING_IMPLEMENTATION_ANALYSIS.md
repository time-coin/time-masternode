# Staking in TIME Coin Protocol V6: Specification vs Implementation

**Date:** December 23, 2025  
**Status:** Analysis - Gaps Identified

---

## What Does Staking Mean in V6?

### Protocol Definition (§5.3, §17.2)

In TIME Coin Protocol V6, **staking** is the mechanism by which validators join the **Active Validator Set (AVS)** and earn voting power proportional to their collateral.

#### Key Concepts:

1. **Collateral Locking**: Validators must lock TIME coins in an **on-chain staking UTXO** to participate.
   - Script format: `OP_STAKE <tier_id> <pubkey> <unlock_height> <op_unlock>`
   - Locked amount determines the validator's weight in Avalanche voting.

2. **Tier System**: Collateral requirement depends on validator tier:
   - **Free Tier**: 0 TIME (can receive rewards, cannot vote on governance)
   - **Bronze Tier**: 1,000 TIME (baseline voting power)
   - **Silver Tier**: 10,000 TIME (10x voting power)
   - **Gold Tier**: 100,000 TIME (100x voting power)

3. **Stake Maturity**: A staking UTXO becomes eligible for voting only after being included in a **checkpoint block** (TSDC).
   - Prevents "vote before finalization" attacks.
   - Creates a time-lock on fresh stakes.

4. **Weight Calculation**: Voting power in Avalanche consensus is **stake-weighted**:
   ```
   validator_weight = locked_collateral / (10^8)  // Convert to TIME units
   probability_of_selection ∝ validator_weight / total_stake
   ```

5. **Governance Integration**: Higher tiers get voting rights on protocol governance proposals (§15).

---

## Current Implementation Status

### ✅ IMPLEMENTED (Core Staking)

| Feature | Location | Status | Details |
|---------|----------|--------|---------|
| **Tier Definition** | `src/types.rs` (L102-147) | ✅ COMPLETE | Free, Bronze, Silver, Gold tiers with collateral amounts |
| **Collateral Field** | `src/types.rs` (L96) | ✅ COMPLETE | `pub collateral: u64` in `Masternode` struct |
| **Weight Calculation** | `src/consensus.rs` | ✅ COMPLETE | Uses `masternode.tier.collateral()` for voting weight |
| **Tier Registry** | `src/masternode_registry.rs` | ✅ COMPLETE | Stores masternode tier and collateral on disk |
| **RPC Integration** | `src/rpc/handler.rs` | ✅ COMPLETE | Returns collateral in masternode info queries |
| **Network Propagation** | `src/network/server.rs` | ✅ COMPLETE | Broadcasts collateral with heartbeats |

### ⚠️ PARTIALLY IMPLEMENTED (Staking Scripts & UTXO Locking)

| Feature | Requirement | Current Status | Gap |
|---------|-------------|-----------------|-----|
| **OP_STAKE Script** | Parse/validate staking script | ❌ NOT IMPLEMENTED | No script parser for `OP_STAKE` opcode |
| **UTXO Lock Script Validation** | Verify output is locked by staking script | ❌ NOT IMPLEMENTED | `script_pubkey` stored but not validated |
| **Unlock Height Enforcement** | Prevent withdrawal before `unlock_height` | ❌ NOT IMPLEMENTED | No maturity check on spending |
| **Staking UTXO Creation** | Generate staking transactions | ⚠️ PARTIAL | Can create masternodes, but not via `OP_STAKE` |
| **Maturity Tracking** | Know when stake becomes eligible | ⚠️ PARTIAL | Masternodes register immediately; no maturity delay |
| **Tier Changes** | Allow validator to upgrade/downgrade tier | ❌ NOT IMPLEMENTED | No mechanism to change collateral after registration |

### 🔴 NOT IMPLEMENTED (Staking Lifecycle)

| Feature | Description | Blocker |
|---------|-------------|---------|
| **Staking Transaction Creation** | User explicitly locks coins in `OP_STAKE` output | No CLI/RPC to create staking txs |
| **Collateral Proof** | Verify masternode control the staking UTXO | No UTXO ownership verification |
| **Maturity Period** | Enforce waiting time after staking (§5.4) | No block tracking for stake maturity |
| **Withdrawal Transactions** | Spend staking UTXO after `unlock_height` | No withdrawal logic |
| **In-Flight Staking** | Handle masternodes during the waiting period | Masternodes join AVS immediately |
| **Bootstrap Sequence** | Initial validators stake on-chain in block 0/1 | Not enforced; masternodes pre-configured |

---

## How Staking Works Today (Simplified)

### Current Workflow:

1. **Node operator creates masternode**:
   ```bash
   ./timed --validator-id mynode --stake 1000
   ```
   - Creates a `Masternode` struct with collateral = 1000.
   - Registers immediately in `MasternodeRegistry`.

2. **Voting weight is assigned**:
   ```rust
   let weight = masternode.tier.collateral() / 1_000_000_000;
   ```
   - Weight = 1000 / 10^9 = 0.000001 relative units.

3. **Consensus uses the weight**:
   - Avalanche sampling queries peers stake-weighted.
   - Finality requires >50% of total stake (§9.5).

4. **Rewards distributed by tier**:
   - Weight in block reward distribution matches voting weight.

### What's Missing:

1. **No on-chain proof** that the node actually locked collateral.
   - Collateral is a command-line parameter, not a blockchain artifact.
   
2. **No maturity period** before voting becomes eligible.
   - A fresh node with `--stake 1000` votes immediately.
   
3. **No withdrawal mechanism** to reclaim collateral.
   - Can't spend the staking UTXO (doesn't exist on-chain).

4. **No tier upgrade path**.
   - Node operator must stop/restart with new `--stake` to change tier.

---

## Protocol Requirements Not Met by Implementation

### 1. §5.3 - "On-Chain Staking UTXO"

**Protocol says:**
> "Stake locked by a staking script; weight derived from locked amount"

**Implementation does:**
- Collateral is an in-memory parameter, not a UTXO.
- No script validation.

**Fix needed:**
- Require `OP_STAKE` output in a genesis or early block.
- Verify masternode pubkey matches the script pubkey.
- Validate unlock conditions before accepting votes.

---

### 2. §17.2 - "Staking UTXO Script System"

**Protocol specifies:**
```
OP_STAKE <tier_id: u8> <pubkey: 33 bytes> <unlock_height: u32> <op_unlock: 1 byte>
```

**Implementation status:**
- No script parser.
- No opcode for `OP_STAKE`.
- `script_pubkey` field exists but never parsed.

**Fix needed:**
- Implement script VM or minimal parser.
- Define how `OP_STAKE` encodes collateral amount.
- Enforce unlock conditions in UTXO spending.

---

### 3. §5.4 - "Stake Maturity"

**Protocol says:**
> "A staking output is **mature** once included in a checkpoint block. Masternodes may only join AVS after stake maturity."

**Implementation does:**
- No maturity tracking.
- Masternodes join AVS at registration time (immediately).

**Fix needed:**
- Track which checkpoint block includes the staking UTXO.
- Disable voting until `current_checkpoint_height >= stake_inclusion_checkpoint`.
- Reject votes from immature stakes.

---

### 4. §5.5 - "Tier Changes"

**Protocol says:**
> "To upgrade/downgrade tier: operator creates a new staking output. Old stake withdrawn after unlock_height."

**Implementation does:**
- No way to change collateral after registration.
- Must restart node with different `--stake`.

**Fix needed:**
- Accept staking UTXO updates from the same pubkey.
- Detect when new stake matures.
- Transition voting power from old stake to new.

---

## Security Implications

### Current (Simplified) Model:

| Threat | Protection |
|--------|------------|
| **Sybil attack** | Stake-weighted voting (>50% stake required to finalize) |
| **Double-spend as validator** | Not possible; validators can't vote for conflicting txs |
| **Stake slashing** | Not implemented |
| **Costless block proposal** | Free tier nodes can participate with 0 collateral |

### Missing Protections (V6 requires):

1. **No verification** that node actually owns the collateral.
   - Operator can claim `--stake 1000000` without locking coins.
   
2. **No slashing** for misbehavior (yet; Phase 9 spec).
   - Validators don't risk losing collateral.
   
3. **No replay protection** across stake transitions.
   - Old stakes can be double-counted if not cleaned up.

---

## Implementation Roadmap

### Phase A: Staking Script Foundation (Required for V7)

1. **Define OP_STAKE encoding**:
   - How is collateral amount stored in the script?
   - How is unlock_height encoded?
   
2. **Implement script validation**:
   - Parse `OP_STAKE` from UTXO script_pubkey.
   - Extract tier, pubkey, unlock_height.
   
3. **Add UTXO ownership check**:
   - Verify masternode signature matches script pubkey.

### Phase B: Maturity Enforcement (Required for V7)

1. **Track staking UTXO inclusion**:
   - Record block height when staking UTXO is archived.
   
2. **Disable voting until maturity**:
   - Reject votes from masternodes with immature stakes.
   - Log and slashing candidates.

3. **Add maturity delay config**:
   - Default: mature after 1 checkpoint (600s).
   - Configurable per network.

### Phase C: Withdrawal & Tier Changes (Phase 8+)

1. **Implement spending of staking UTXOs**:
   - Create withdrawal transactions.
   - Verify unlock_height has passed.

2. **Support tier upgrades**:
   - Detect new staking UTXO from same pubkey.
   - Atomically transition voting power.

3. **Clean up old stakes**:
   - Archive old UTXOs after withdrawal period.

---

## Simplified Interim Solution (Testnet)

Until full staking UTXO implementation:

1. **Keep in-memory collateral** (current approach).
2. **Assume all stakes are mature** (not enforced).
3. **Disallow tier changes** (no upgrade support).
4. **Document limitations** in testnet README.

This allows Phase 6-8 work (network, rewards, checkpointing) to proceed while Phase 7-9 implements staking scripts.

---

## Questions for Protocol Team

1. **OP_STAKE Encoding**: Is collateral amount included in script, or only tier & pubkey?
   - Affects UTXO validation logic.

2. **Maturity Period**: Should it be measured in:
   - Blocks (e.g., 100 checkpoint blocks)?
   - Wall-clock time (e.g., 10 minutes)?
   - Both (e.g., 10 minutes OR 1 checkpoint, whichever is later)?

3. **Sybil Resistance for Free Tier**: Should free-tier nodes (0 collateral) have reduced sampling probability?
   - Current spec says >50% stake; free nodes don't count toward this.

4. **Bootstrap**: How do initial validators stake in block 0?
   - Protocol says "pre-agreed initial AVS + on-chain staking".
   - Can staking UTXO be in block 0, or must it exist before genesis?

---

## Summary

| Aspect | Status | Impact |
|--------|--------|--------|
| **Tier System** | ✅ Implemented | Validators have correct collateral amounts |
| **Weight Calculation** | ✅ Implemented | Voting power scales with stake |
| **Script Validation** | ❌ Missing | Masternodes can't prove they locked coins |
| **Maturity Enforcement** | ❌ Missing | Fresh stakes vote immediately (allows attacks) |
| **Withdrawal Logic** | ❌ Missing | Validators can't reclaim collateral |
| **Tier Upgrades** | ❌ Missing | No way to change staking amount |

**Conclusion:** TIME Coin has a **working stake-weighted consensus** with tiers and voting power calculation, but **lacks on-chain proof** that collateral was actually locked. This is acceptable for testnets and Phase 6-8 development but must be addressed in V7 before mainnet launch.

The simplest immediate fix: create staking UTXOs in genesis, validate against them, and enforce maturity before voting eligibility.
