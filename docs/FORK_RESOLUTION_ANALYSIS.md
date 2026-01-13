# Fork Resolution Analysis - Root Cause of Network Desync

## Executive Summary

After deep analysis of the codebase, I've identified **multiple competing fork resolution paths** and **uncoordinated block production** as the root causes of nodes failing to sync. The network has at least 5 different mechanisms that can trigger chain switches, often conflicting with each other.

---

## 🔴 Critical Issues Identified

### Issue 1: Multiple Competing Fork Resolution Systems

The codebase has **5 separate fork resolution mechanisms** that can all trigger simultaneously:

| # | System | Location | Trigger |
|---|--------|----------|---------|
| 1 | `network/fork_resolver.rs` | Centralized state machine | Block requests, timeouts |
| 2 | `ai/fork_resolver.rs` | AI scoring system | resolve_fork() calls |
| 3 | `compare_chain_with_peers()` | blockchain.rs:3255 | Periodic (15s & 30s intervals) |
| 4 | `add_block_with_fork_handling()` | blockchain.rs:2615 | Every incoming block |
| 5 | `handle_fork()` | blockchain.rs:3665 | Fork block batches |

**Problem**: These systems can make conflicting decisions. For example:
- `compare_chain_with_peers()` might decide to switch to Chain A
- `add_block_with_fork_handling()` might reject Chain A's blocks due to hash mismatch
- `ai/fork_resolver.rs` might score Chain B higher
- Result: Node oscillates between chains

#### Code Example - Conflicting Decisions

```rust
// blockchain.rs:3361-3431 - compare_chain_with_peers() uses masternode authority
if consensus_height == our_height && consensus_hash != our_hash {
    // Uses masternode_authority::CanonicalChainSelector
    let (should_switch, reason) = 
        crate::masternode_authority::CanonicalChainSelector::should_switch_to_peer_chain(...);
    if should_switch {
        return Some((consensus_height, consensus_peers[0].clone()));
    }
}

// BUT ai/fork_resolver.rs:124-280 uses different scoring
pub async fn resolve_fork(&self, params: ForkResolutionParams) -> ForkResolution {
    // Uses multi-factor scoring: height (40%), work (30%), time (15%), peer consensus (15%)
    score_breakdown.total_score = (score_breakdown.height_score * 0.40)
        + (score_breakdown.work_score * 0.30)
        + (score_breakdown.time_score * 0.15)
        + (score_breakdown.peer_consensus_score * 0.15)
        + ...
}
```

These two systems use **completely different algorithms** and will often disagree!

---

### Issue 2: Catchup Block Production Creates Forks

**The catchup mechanism allows nodes to produce their own blocks when behind**, instead of syncing from peers. This is the PRIMARY source of forks.

#### Code Location: `main.rs:1300-1415`

```rust
// main.rs:1350-1400 - Catchup block production
// Double-check: NEVER produce if current blockchain height >= target
let current_height_check = block_blockchain.get_height();
if current_height_check >= target_height {
    continue;
}

match block_blockchain.produce_block_at_height(Some(target_height)).await {
    Ok(block) => {
        // Add block to our chain
        if let Err(e) = block_blockchain.add_block(block.clone()).await {
            break;
        }
        // Broadcast to peers
        block_registry.broadcast_block(block.clone()).await;
        catchup_produced += 1;
    }
}
```

**The Problem**:
1. Node A is at height 6200, expected height is 6224
2. Node A produces catchup blocks 6201-6224 (24 blocks!)
3. Node B is ALSO at height 6200, produces its OWN catchup blocks 6201-6224
4. Now we have 2 competing chains with different blocks at each height
5. Both nodes broadcast their blocks → Network splits

The `catchup_leader_tracker` (line 1284) attempts to prevent this, but it's **local-only** - there's no network consensus on who should produce catchup blocks.

---

### Issue 3: No Leader Election for Catchup Mode

Normal block production uses deterministic leader selection:

```rust
// main.rs:1428-1449 - Normal block production uses VRF-based leader selection
let mut hasher = Sha256::new();
hasher.update(prev_block_hash);
hasher.update(current_height.to_le_bytes());
let selection_hash: [u8; 32] = hasher.finalize().into();

let producer_index = {
    let mut val = 0u64;
    for (i, &byte) in selection_hash.iter().take(8).enumerate() {
        val |= (byte as u64) << (i * 8);
    }
    (val % masternodes.len() as u64) as usize
};
```

BUT catchup mode (lines 1300-1415) has **no leader election** - any node that thinks it's behind will produce blocks!

---

### Issue 4: Race Condition in Fork Resolution Lock

```rust
// blockchain.rs:3256-3259
pub async fn compare_chain_with_peers(&self) -> Option<(u64, String)> {
    // CRITICAL: Acquire fork resolution lock
    let _lock = self.fork_resolution_lock.lock().await;
    // ... rest of function
}
```

This lock only protects `compare_chain_with_peers()`. But other fork resolution paths (`add_block_with_fork_handling`, `handle_fork`) don't acquire this lock, leading to race conditions.

---

### Issue 5: Sync Loop Detection Bypass

The node has sync loop detection to prevent infinite GetBlocks requests:

```rust
// network/message_handler.rs:231
"🚨 [{}] Possible sync loop detected: {} sent {} similar GetBlocks requests in 30s"
```

But when nodes are on different forks, this detection **prevents** the legitimate sync that would resolve the fork!

---

## 📊 What the Logs Show

From your logs:
```
Jan 13 05:31:56: Fork detected at height 6216: different block at same height
Jan 13 05:32:01: Consensus: height 6224 hash 5541e3066f2eacaf (2 peers agree). Our height: 6224 hash f077c96e8d65f979
Jan 13 05:32:01: Fork at same height 6224: our hash f077c96e vs consensus hash 5541e306 (2 peers)
```

This shows:
1. Multiple nodes produced blocks at height 6224
2. 2 peers have hash `5541e306...`
3. Arizona has hash `f077c96e...`
4. Fork resolution runs but nodes don't converge

---

## 🔧 Recommended Fixes

### Fix 1: Single Authority Fork Resolution

Consolidate all fork resolution into ONE system:

```rust
// Proposed: Single fork_resolution module
pub enum ForkDecision {
    KeepOurChain,
    SwitchToPeer { peer: String, rollback_to: u64 },
    NeedMoreBlocks { from_height: u64, peer: String },
}

pub async fn resolve_fork_unified(
    our_chain: &ChainState,
    peer_chains: &[(String, ChainState)],
    masternode_registry: &MasternodeRegistry,
) -> ForkDecision {
    // Single decision algorithm combining:
    // 1. Masternode authority
    // 2. Chain work
    // 3. Peer consensus count
    // 4. Hash tiebreaker
}
```

### Fix 2: Disable Catchup Block Production

When behind, nodes should ONLY sync from peers, never produce their own blocks:

```rust
// main.rs - REMOVE catchup block production entirely
// if blocks_behind > 5 {
//     // OLD: Produce catchup blocks
//     // NEW: Only sync from peers
//     tracing::info!("Behind by {} blocks, syncing from peers only", blocks_behind);
//     blockchain.sync_from_peers().await?;
// }
```

### Fix 3: Global Leader Election for Catchup

If catchup production is needed, use network-wide leader election:

```rust
// Proposed: Network-wide catchup leader election
async fn elect_catchup_leader(
    blockchain: &Blockchain,
    peer_registry: &PeerRegistry,
    target_height: u64,
) -> Option<String> {
    // 1. Get all connected masternodes
    // 2. Hash(prev_block_hash || target_height || "catchup")
    // 3. Select leader deterministically
    // 4. Only that node produces catchup blocks
    // 5. All other nodes wait and sync
}
```

### Fix 4: Immediate Rollback on Same-Height Fork

When detecting same-height fork with peer majority:

```rust
// blockchain.rs - In compare_chain_with_peers()
if consensus_height == our_height && consensus_hash != our_hash {
    if consensus_peers.len() >= 2 {
        // Immediately rollback and sync - don't try complex resolution
        let rollback_to = our_height.saturating_sub(5); // Go back 5 blocks
        self.rollback_to_height(rollback_to).await?;
        self.sync_from_peers().await?;
        return;
    }
}
```

---

## 🔍 Debugging Steps

1. **Identify the fork point**:
   ```bash
   # On each node, find where chains diverge
   for height in 6210 6215 6220 6224; do
     hash=$(curl -s localhost:3030/block/$height | jq -r '.hash')
     echo "Height $height: $hash"
   done
   ```

2. **Check who produced conflicting blocks**:
   ```bash
   # Get the leader field from conflicting blocks
   curl -s localhost:3030/block/6224 | jq '.header.leader'
   ```

3. **Stop all catchup production** (temporary fix):
   ```bash
   # Set environment variable to disable catchup
   export TIMECOIN_DISABLE_CATCHUP=1
   ```

---

## Files to Modify

| File | Change |
|------|--------|
| `src/main.rs:1300-1415` | Remove/disable catchup block production |
| `src/blockchain.rs:3255-3472` | Simplify `compare_chain_with_peers()` |
| `src/blockchain.rs:2615-2791` | Add fork resolution lock to `add_block_with_fork_handling()` |
| `src/ai/fork_resolver.rs` | Deprecate or unify with main fork resolution |
| `src/network/fork_resolver.rs` | Deprecate or unify with main fork resolution |

---

## Immediate Action Plan

1. **Stop all nodes** - Network is in fractured state
2. **Identify longest valid chain** - Find which node has the "correct" chain
3. **Reset all nodes to that chain** - Clear DBs and resync from correct chain
4. **Deploy fix** - Disable catchup production, simplify fork resolution
5. **Restart network** - Monitor for new forks

Would you like me to implement any of these fixes?
