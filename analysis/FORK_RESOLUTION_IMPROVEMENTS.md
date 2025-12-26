# Fork Resolution Improvements

**Date**: December 26, 2024  
**Commit**: 2cba0b1

## Problem

The network experienced a multi-way fork where:
1. Each node was stuck on a different chain at the same or similar heights
2. Nodes could not resolve ties when chains had equal work and equal height
3. Blocks were being generated even when nodes were not sufficiently synced
4. No deterministic method existed to choose between competing forks of equal quality

## Root Causes

1. **No Tie-Breaking Logic**: When two forks had equal cumulative work and equal height, there was no deterministic way to choose which chain to follow
2. **Premature Block Generation**: Leaders could propose blocks even when only connected to 1-2 peers, leading to network fragmentation
3. **Simple Comparison**: Chain comparison only considered work and height, not a tie-breaker

## Solution Implemented

### 1. Deterministic Tie-Breaker (Lexicographic Hash Comparison)

Added deterministic fork resolution in `blockchain.rs`:

```rust
pub async fn should_switch_by_work(&self, peer_work: u128, peer_height: u64, peer_tip_hash: &[u8; 32]) -> bool {
    let our_work = *self.cumulative_work.read().await;
    let our_height = *self.current_height.read().await;

    // Rule 1: Prefer chain with more work
    if peer_work > our_work {
        return true;
    }

    // Rule 2: If equal work, prefer longer chain
    if peer_work == our_work && peer_height > our_height {
        return true;
    }

    // Rule 3: If equal work AND equal height, use deterministic tie-breaker
    // Compare block hashes lexicographically - choose the smaller one
    if peer_work == our_work && peer_height == our_height {
        if let Ok(our_tip) = self.get_block_by_height(our_height).await {
            let our_hash = our_tip.hash();
            if peer_tip_hash < &our_hash {
                return true;
            }
        }
    }

    false
}
```

**Key Points**:
- Uses lexicographic comparison of block hashes (byte-by-byte)
- Deterministic: all nodes will choose the same chain given the same hashes
- Only applied when work and height are equal (rare case)
- Prevents permanent network splits from identical-quality forks

### 2. Minimum Sync Requirement

Added validation in `main.rs` before block proposal:

```rust
// Check if we have enough synced nodes before proposing
let connected_count = peer_registry_tsdc.connected_count();

// Require at least 3 nodes total (including ourselves) for consensus
let required_sync = 3;
let total_nodes = connected_count + 1; // +1 for ourselves

if total_nodes < required_sync {
    tracing::warn!(
        "⚠️  Not enough synced peers for block proposal: {}/{} required",
        total_nodes,
        required_sync
    );
    continue; // Skip block proposal
}
```

**Benefits**:
- Prevents blocks from being created in isolation
- Ensures network consensus can be achieved
- Avoids premature blocks during network startup
- Minimum of 3 nodes provides basic majority

### 3. Improved Chain Comparison

Updated `network/server.rs` to use comprehensive comparison:

```rust
NetworkMessage::ChainWorkResponse { height, tip_hash, cumulative_work } => {
    // Use comprehensive comparison including tie-breaker
    if blockchain.should_switch_by_work(*cumulative_work, *height, tip_hash).await {
        tracing::info!("📊 Peer has better chain, requesting blocks");
        
        if let Some(fork_height) = blockchain.detect_fork(*height, *tip_hash).await {
            // Request blocks for reorganization
            let request = NetworkMessage::GetBlockRange {
                start_height: fork_height,
                end_height: *height,
            };
            let _ = peer_registry.send_to_peer(&ip_str, request).await;
        }
    }
}
```

## Testing

To verify these fixes work correctly:

1. **Tie-Breaker Test**:
   - Start 4 nodes from genesis
   - Let them create competing blocks at the same height
   - Verify all nodes eventually converge to the same chain (lowest hash)

2. **Sync Requirement Test**:
   - Start a node in isolation
   - Verify it does NOT create blocks
   - Connect 2 more nodes
   - Verify blocks start being created once 3 nodes are connected

3. **Fork Resolution Test**:
   - Create a fork scenario with equal-height chains
   - Verify nodes switch to the chain with the lexicographically smallest hash
   - Verify chain stays consistent after resolution

## Commit Details

```
commit 2cba0b1
Author: [Author]
Date:   Thu Dec 26 2024

    Add deterministic fork resolution and minimum sync requirement
    
    - Implement deterministic tie-breaker for equal-height equal-work forks
      using lexicographically smallest block hash
    - Add minimum 3-node sync requirement before block proposal to prevent
      premature blocks
    - Update chain comparison to use comprehensive should_switch_by_work()
      method that handles work, height, and hash comparisons
    - Prevent block generation when insufficient peers are synced
```

## Expected Behavior After Fix

1. **Network Startup**: Nodes wait until 3+ nodes are connected before proposing blocks
2. **Fork Detection**: When forks are detected, nodes compare using work → height → hash
3. **Convergence**: All nodes converge to the same chain deterministically
4. **No Splits**: Network cannot permanently split on equal-quality chains
5. **Stable Consensus**: Once converged, the chain remains consistent

## Related Issues

- Multi-way fork at heights 3724-3727
- Premature block generation during network initialization
- Non-deterministic fork resolution leading to permanent splits
