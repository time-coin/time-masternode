# Staking & Catch-Up Block Analysis

## What Staking Means in v6 Protocol

### Definition
Staking is the mechanism by which masternodes lock up TIME coins to participate in the Avalanche consensus and block production process. It serves three purposes:

1. **Consensus Participation**: Only staked masternodes vote in Avalanche sampling
2. **Network Security**: Slashing penalties discourage malicious behavior
3. **Reward Eligibility**: Only active stakers earn block rewards

### Staking Process in v6

#### 1. Registration (§5.3)
```
Masternode creates a staking UTXO:
- Locks coins to their public key
- Creates heartbeat attestation system
- Registers in MasternodeRegistry
- Becomes eligible after maturation period (currently 100 blocks)
```

#### 2. Active Participation
```
Registered masternode must:
- Maintain heartbeat attestations (witness votes every slot)
- Be included in the active validator set
- Participate in Avalanche voting on transactions
- Produce TSDC blocks when selected by VRF
```

#### 3. Reward Distribution (§10)
```
Reward = 100 * (1 + ln(N)) / k
- N = total staked amount (coins)
- k = number of active validators
- Distributed per slot based on participation
```

#### 4. Withdrawal (Future)
```
Maturation after unbond period → UTXO becomes spendable
```

---

## Current Implementation Status

### ✅ Fully Implemented

| Component | Location | Status |
|-----------|----------|--------|
| MasternodeRegistry | `src/masternode_registry.rs` | Complete |
| Registration Logic | `register_masternode()` | Working |
| Heartbeat Attestation | `src/heartbeat_attestation.rs` | Complete |
| AVS Membership Rules | `src/consensus/avalanche.rs` | Complete |
| Reward Formula | `src/tsdc.rs` | Implemented |
| Staking Tier System | `MasternodeInfo.tier` | In code |

### ⚠️ Partially Implemented

| Component | Issue | Impact |
|-----------|-------|--------|
| Catch-Up Blocks | **REMOVED** - no implementation | Cannot sync behind peers |
| Peer Discovery | Uses static config | Limited dynamic discovery |
| Slashing Logic | Not implemented | No penalty for malice |
| Unlock Script | No UTXO locking script | Cannot withdraw stake |

### 🔴 Not Implemented

| Component | Reason | Needed For |
|-----------|--------|-----------|
| Withdrawal Maturation | Time-based UTXO locks | Allowing unstaking |
| Slash Conditions | Fraud proof system | Security guarantees |
| Dynamic Tier Changes | Complex state management | Adaptive staking |

---

## The Catch-Up Block Problem

### What Was It?
A catch-up block mechanism allowed nodes that fell behind the block production schedule to quickly synchronize by:
1. Requesting blocks from peers
2. Applying them without waiting for real-time slot boundaries
3. "Fast-forwarding" to current block height

### Why Was It Removed?
The catch-up function was completely removed from the codebase. Current search shows NO references to:
- `catch_up_block()`
- `catchup_blocks`
- `CatchupBlock`
- `sync_catchup`

### The Original Problem You Mentioned

> "masternodes only showed themselves as connected masternodes, so catchup blocks didn't occur"

**Root Cause**: The network layer was filtering out non-masternode peers from the connected peer list, so catch-up sync only saw other masternodes (which were usually in sync) and never actually needed to catch up.

### Current Peer Connection Logic

Looking at `src/network/peer_connection_registry.rs`:
```rust
pub async fn get_connected_peers(&self) -> Vec<String>
pub async fn get_connected_peers_list(&self) -> Vec<String>
```

**Status**: Currently returns ALL connected peers, not filtered by masternode status. This FIXES the original issue, but WITHOUT a catch-up mechanism, nodes can only sync in real-time.

---

## Recommendation

### Option A: Re-implement Catch-Up Blocks (Recommended)
```rust
pub async fn catch_up_to_height(&self, target_height: u64) -> Result<(), String> {
    let current_height = self.get_height().await;
    if target_height <= current_height {
        return Ok(());
    }
    
    // Request blocks from peers in parallel
    let peers = self.peer_registry.get_connected_peers().await;
    for height in (current_height + 1)..=target_height {
        if let Ok(block) = self.request_block_from_peers(&peers, height).await {
            self.apply_block(&block).await?;
        }
    }
    Ok(())
}
```

**Benefits**:
- Nodes can sync without waiting for slots
- Faster chain recovery after downtime
- Works with current peer discovery

### Option B: Accept Real-Time Only Sync
```
Accept that syncing ONLY happens on block boundaries (every 1 hour).
Advantages: Simpler, more deterministic
Disadvantages: Requires always-on operation, no offline recovery
```

---

## Action Items

- [ ] Decide: Catch-up blocks or real-time only?
- [ ] If catch-up: Implement `sync_from_peers()` properly
- [ ] If catch-up: Add block request timeout logic
- [ ] Test peer discovery with mixed node types
- [ ] Document expected sync behavior
