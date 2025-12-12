# Blockchain Fork Resolution - Design Document

## Overview

TimeCoin uses BFT (Byzantine Fault Tolerant) consensus with 2/3 quorum. When a node detects it's on a different chain than the network, it must reorganize to the chain with consensus support.

## Problem Statement

**Scenario:**
1. Node goes offline for maintenance
2. Network continues producing blocks 1000-1100 with VDF
3. Node comes back online, generates blocks 1000-1100 locally without VDF
4. Result: **FORK** - Same heights, different hashes

**Current Behavior (Broken):**
- Node receives block 1101 from peer
- Validation fails: "Invalid previous hash"
- Node rejects the block
- Node stuck on forked chain forever

**Required Behavior:**
- Node detects fork (previous hash mismatch)
- Node queries peers for consensus
- Node reorganizes to consensus chain
- Node continues syncing normally

---

## Fork Resolution Algorithm

### Phase 1: Fork Detection

When receiving a block that doesn't match our chain:

```rust
if block.previous_hash != our_block.hash() {
    // FORK DETECTED
    handle_fork_and_reorg(block).await
}
```

**Detection Points:**
1. **Invalid previous hash** - Block N+1 doesn't link to our block N
2. **Different hash at same height** - Peer has different block 1000 than us
3. **Peer height mismatch** - Peer claims different hash at our current height

### Phase 2: Consensus Check (Critical)

**Before reorganizing, verify the peer's chain has 2/3+ masternode consensus:**

```
1. Query all connected masternodes: "What's your block hash at height X?"
2. Count responses:
   - Chain A: 7 masternodes
   - Chain B: 3 masternodes
3. Calculate consensus:
   - Total: 10 masternodes
   - Required: 7 (2/3 + 1)
   - Chain A has consensus ✅
4. Reorganize to Chain A
```

**Implementation:**
```rust
async fn verify_chain_consensus(height: u64, block_hash: [u8; 32]) -> Result<bool, String> {
    let peers = get_connected_masternodes().await;
    let total = peers.len();
    let required = (total * 2) / 3 + 1;
    
    let mut matching_count = 0;
    
    for peer in peers {
        let response = peer.request_block_hash(height).await?;
        if response.hash == block_hash {
            matching_count += 1;
        }
    }
    
    Ok(matching_count >= required)
}
```

### Phase 3: Find Common Ancestor

Walk backwards to find where chains diverged:

```
Our Chain:    [0] -> [1] -> [2] -> [3] -> [4A] -> [5A]
Peer Chain:   [0] -> [1] -> [2] -> [3] -> [4B] -> [5B] -> [6B]
                                    ^
                              Common Ancestor = Block 3
```

**Algorithm:**
```rust
async fn find_common_ancestor(fork_height: u64) -> Result<u64, String> {
    let mut height = fork_height - 1;
    
    while height > 0 {
        let our_hash = get_block_hash(height).await?;
        let peer_hash = peer.request_block_hash(height).await?;
        
        if our_hash == peer_hash {
            return Ok(height); // Found common ancestor
        }
        
        height -= 1;
    }
    
    Ok(0) // Common ancestor is genesis
}
```

### Phase 4: Rollback to Common Ancestor

**Safety Checks:**
```rust
let reorg_depth = current_height - common_ancestor;

if reorg_depth > 100 {
    return Err("Fork too deep - manual intervention required");
}

if reorg_depth > 10 {
    tracing::warn!("⚠️ Deep reorg: {} blocks", reorg_depth);
    // Log warning to monitoring system
}
```

**Rollback Process:**
```rust
async fn rollback_to_height(target: u64) -> Result<(), String> {
    let current = get_height().await;
    
    // 1. Delete blocks in reverse order
    for height in (target + 1)..=current {
        delete_block(height).await?;
    }
    
    // 2. Revert UTXO state
    revert_utxos_to_height(target).await?;
    
    // 3. Revert masternode state
    revert_masternodes_to_height(target).await?;
    
    // 4. Update chain height
    set_height(target).await?;
    
    tracing::info!("✅ Rolled back to height {}", target);
    Ok(())
}
```

### Phase 5: Accept Consensus Chain

Once rolled back to common ancestor:

```rust
// Accept new blocks from peer's chain
for height in (common_ancestor + 1)..=peer_height {
    let block = peer.request_block(height).await?;
    add_block(block).await?;
}

tracing::info!("✅ Reorganization complete");
tracing::info!("🔄 Synced to consensus chain at height {}", peer_height);
```

---

## BFT Consensus Integration

### Block Validation with Consensus

Every block should include consensus proof:

```rust
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
    pub consensus_proof: ConsensusProof, // NEW
}

pub struct ConsensusProof {
    pub signatures: Vec<MasternodeSignature>,
    pub quorum_achieved: bool,
}
```

**Validation:**
```rust
fn validate_consensus_proof(block: &Block) -> Result<(), String> {
    let total_masternodes = get_active_masternodes().len();
    let required_signatures = (total_masternodes * 2) / 3 + 1;
    
    if block.consensus_proof.signatures.len() < required_signatures {
        return Err(format!(
            "Insufficient consensus: {} of {} required",
            block.consensus_proof.signatures.len(),
            required_signatures
        ));
    }
    
    // Verify each signature
    for sig in &block.consensus_proof.signatures {
        if !sig.verify(&block.hash()) {
            return Err("Invalid masternode signature".to_string());
        }
    }
    
    Ok(())
}
```

### Fork Resolution Decision Matrix

| Scenario | Our Chain | Peer Chain | Action |
|----------|-----------|------------|--------|
| **Consensus** | No consensus (1/10) | Has consensus (7/10) | **Reorg to peer chain** ✅ |
| **Length** | Height 1000 | Height 1100 | **Sync from peer** ✅ |
| **Both valid** | Has consensus (5/10) | Has consensus (5/10) | **Keep our chain** (tie) |
| **Deep fork** | Height 1000 | Height 900, fork 100 blocks deep | **Manual intervention** ⚠️ |
| **Supermajority** | No consensus (2/10) | Supermajority (9/10) | **Reorg immediately** ✅ |

---

## Network Messages

### GetBlockHash Request

```rust
pub struct GetBlockHashRequest {
    pub height: u64,
}

pub struct GetBlockHashResponse {
    pub height: u64,
    pub hash: [u8; 32],
    pub has_block: bool,
}
```

### ConsensusQuery Request

```rust
pub struct ConsensusQueryRequest {
    pub height: u64,
    pub block_hash: [u8; 32],
}

pub struct ConsensusQueryResponse {
    pub agrees: bool,
    pub masternode_signature: MasternodeSignature,
}
```

### BlockRange Request (for reorg)

```rust
pub struct GetBlockRangeRequest {
    pub start_height: u64,
    pub end_height: u64,
}

pub struct GetBlockRangeResponse {
    pub blocks: Vec<Block>,
}
```

---

## Implementation Phases

### Phase 1: Basic Fork Detection ✅ (Partially Done)
- [x] Detect fork when previous hash doesn't match
- [x] Log fork warnings
- [x] Reject conflicting blocks

### Phase 2: Consensus Query (TODO)
- [ ] Add `GetBlockHash` network message
- [ ] Add `ConsensusQuery` network message
- [ ] Implement peer consensus polling
- [ ] Calculate 2/3 quorum

### Phase 3: Rollback Implementation (TODO)
- [ ] Implement `find_common_ancestor()`
- [ ] Implement `rollback_to_height()`
- [ ] Add UTXO state reversal
- [ ] Add masternode state reversal
- [ ] Test rollback with 1, 10, 50 block depths

### Phase 4: Automatic Reorganization (TODO)
- [ ] Implement `handle_fork_and_reorg()`
- [ ] Request block range from peer
- [ ] Validate and apply peer's chain
- [ ] Verify final consensus

### Phase 5: Safety & Monitoring (TODO)
- [ ] Add reorg depth limits (max 100 blocks)
- [ ] Log reorg events to monitoring system
- [ ] Add metrics: reorg_count, reorg_depth
- [ ] Alert operators on deep reorgs (>10 blocks)
- [ ] Manual intervention mode for extreme cases

---

## Safety Considerations

### 1. Double-Spend Prevention

**Problem:** During reorg, transactions might be reversed and re-spent differently.

**Solution:**
- Keep rolled-back blocks in orphan pool for 24 hours
- Check for conflicting transactions
- Alert if double-spend detected

### 2. Finality Rules

**Once a block has consensus (2/3+ signatures):**
- Should be considered "final" after 6 blocks
- Reorg should only happen if consensus shifts
- Never reorg finalized blocks unless supermajority (>90%)

### 3. Attack Prevention

**51% Attack:**
- Attacker controls 6/10 masternodes
- Creates alternate chain
- Solution: Checkpoint system (trusted block hashes every 1000 blocks)

**Eclipse Attack:**
- Node only connects to attacker's peers
- Sees fake "consensus"
- Solution: DNS seeds, hardcoded seed nodes, peer diversity

**Long-Range Attack:**
- Attacker creates alternate chain from genesis
- Solution: Reject chains that fork >100 blocks back

---

## Testing Strategy

### Unit Tests

```rust
#[tokio::test]
async fn test_find_common_ancestor() {
    // Create two chains that fork at block 50
    let chain_a = create_test_chain(0, 100);
    let chain_b = create_test_chain(0, 50)
        .extend(create_alternate_chain(51, 80));
    
    let ancestor = find_common_ancestor(80).await.unwrap();
    assert_eq!(ancestor, 50);
}

#[tokio::test]
async fn test_rollback() {
    let blockchain = create_test_blockchain(100);
    blockchain.rollback_to_height(50).await.unwrap();
    
    assert_eq!(blockchain.get_height().await, 50);
    assert!(blockchain.get_block(51).await.is_err());
}

#[tokio::test]
async fn test_consensus_check() {
    let peers = create_mock_peers(10);
    
    // 7 peers agree on block hash
    for i in 0..7 {
        peers[i].set_block_hash(100, hash_a);
    }
    // 3 peers disagree
    for i in 7..10 {
        peers[i].set_block_hash(100, hash_b);
    }
    
    let has_consensus = verify_chain_consensus(100, hash_a).await.unwrap();
    assert!(has_consensus); // 7/10 = 70% > 66.6%
}
```

### Integration Tests

```bash
# Test 1: Node rejoins after offline
1. Start 10 masternodes
2. Stop node 1
3. Network produces blocks 1000-1100
4. Start node 1 (will generate blocks locally)
5. Node 1 detects fork
6. Node 1 queries consensus (9/10 agree on network chain)
7. Node 1 reorganizes to network chain
8. Verify node 1 at height 1100 with correct hashes

# Test 2: Network split (5 vs 5)
1. Start 10 masternodes
2. Split network into two groups (5+5)
3. Each group produces different blocks
4. Reconnect network
5. Neither has 2/3 consensus
6. Nodes should NOT reorganize (tie)
7. Next block producer creates block on one chain
8. That chain gets 6/10 consensus
9. All nodes reorganize to majority chain

# Test 3: Deep fork
1. Node offline for 150 blocks
2. Node rejoins
3. Detects fork 150 blocks deep
4. Rejects automatic reorg (>100 limit)
5. Logs error requiring manual intervention
6. Operator must delete blockchain and resync
```

---

## Metrics & Monitoring

### Key Metrics

```rust
pub struct ForkResolutionMetrics {
    pub total_forks_detected: u64,
    pub successful_reorgs: u64,
    pub failed_reorgs: u64,
    pub max_reorg_depth: u64,
    pub avg_reorg_time_ms: u64,
    pub consensus_query_failures: u64,
}
```

### Alerts

**Critical Alerts:**
- Fork detected deeper than 10 blocks
- Reorg failed after 3 attempts
- Unable to find consensus (network split)
- Node isolated (no peer connections)

**Warning Alerts:**
- Fork detected (shallow <10 blocks)
- Reorg in progress
- Consensus query timeouts

---

## Edge Cases

### 1. Node Joins Fresh Network

```
Network: Block 0 (genesis only)
Node: Starts fresh

Action: No fork, sync normally
```

### 2. Node is Ahead of Network

```
Network: Height 1000
Node: Height 1100 (generated locally)

Action:
- Check consensus for node's blocks 1001-1100
- If no consensus: rollback to 1000, resync
- If consensus: network syncs from node (rare)
```

### 3. Multiple Competing Chains

```
Chain A: 4/10 masternodes
Chain B: 4/10 masternodes  
Chain C: 2/10 masternodes

Action:
- No chain has 2/3 consensus
- Wait for next block
- First chain to reach 7/10 wins
- All nodes reorg to winning chain
```

### 4. Genesis Mismatch

```
Node: Genesis hash AAAA
Network: Genesis hash BBBB

Action:
- FATAL ERROR - incompatible networks
- Cannot reorganize (no common ancestor)
- Node must be reconfigured with correct genesis
```

---

## Configuration

```toml
[consensus]
# Maximum blocks to reorganize automatically
max_reorg_depth = 100

# Minimum consensus percentage required (0-100)
min_consensus_percent = 67

# Timeout for consensus queries (seconds)
consensus_query_timeout = 30

# Number of peers to query for consensus
consensus_query_peers = 10

# Whether to automatically reorganize on fork detection
auto_reorg_enabled = true

# Deep reorg threshold for alerts (blocks)
deep_reorg_threshold = 10
```

---

## Future Enhancements

### 1. Checkpoint System
- Hardcode trusted block hashes every 1000 blocks
- Prevent long-range attacks
- Faster sync for new nodes

### 2. Fast Sync
- Download block headers first
- Verify consensus
- Download full blocks only for recent history

### 3. Proof of Consensus
- Include 2/3 masternode signatures in block header
- Instant verification without querying network
- Reduces consensus query overhead

### 4. Optimistic Reorg
- Accept blocks with >50% consensus temporarily
- Wait for 2/3 confirmation
- Roll back if consensus fails

---

## Conclusion

Proper fork resolution is **critical** for TimeCoin's network health. The key principles:

1. **BFT Consensus First** - Always sync to chain with 2/3+ masternodes
2. **Safe Rollbacks** - Limit depth, revert state properly
3. **Automatic Recovery** - Node should self-heal without operator intervention
4. **Monitoring** - Track forks, reorgs, consensus failures

**Implementation Priority:**
1. Phase 2: Consensus Query (highest priority)
2. Phase 3: Rollback Implementation
3. Phase 4: Automatic Reorganization
4. Phase 5: Safety & Monitoring

**Status:** Design complete, implementation pending

---

**Last Updated:** 2025-12-12  
**Author:** TimeCoin Development Team  
**Status:** Design Document - Not Yet Implemented
