# TimeCoin Mainnet Fork Issue - Root Cause Analysis

**Date:** Current Session  
**Affected Heights:** Fork originated at block 5922, nodes stuck between 5923-5932  
**Status:** ROOT CAUSE IDENTIFIED

---

## Executive Summary

The blockchain network experienced a critical fork at height 5922 where 4+ different chain states emerged. Nodes are unable to reconcile because of a **fundamental bug in the fork resolution sync mechanism**: when attempting to sync the consensus chain, nodes do not rollback far enough to find the common ancestor before requesting new blocks. This causes all incoming blocks to be rejected due to `previous_hash` mismatches, creating an infinite loop.

---

## Timeline of the Fork

### Block 5922 - Fork Origin
- Multiple competing blocks produced at height 5922
- Each had different `previous_hash` values
- Network fragmented into 4+ distinct chains

### Current State (Block 5932)
- **LW-Michigan** (64.91.241.10): Stuck at height **5923** with hash `7c1efe6e08108401`
- **LW-Arizona** (50.28.104.50): At height **5932** with hash `9a21831625215630`
- **LW-London** (165.84.215.117): At height **5932** with hash `243c7f65f5c59e9e`
- **Node 165.232.154.150**: At height **5932** with hash `243c7f65f5c59e9e` ✅ **Consensus** (2 peers agree)
- **Node 178.128.199.144**: At height **5931** with hash `f3837f1265d25a92`

---

## Root Cause

### Bug #1: Sync Without Rollback

**Location:** `src/blockchain.rs` line 951-978 in function `sync_from_specific_peer()`

**Problem:**
```rust
pub async fn sync_from_specific_peer(&self, peer_ip: &str) -> Result<(), String> {
    let current = self.current_height.load(Ordering::Acquire);
    // ...
    let batch_start = current + 1;  // ← BUG: Starts from current + 1
    let batch_end = time_expected;
    
    let req = NetworkMessage::GetBlocks(batch_start, batch_end);
    registry.send_to_peer(peer_ip, req).await // Requests blocks without rollback
}
```

**Why This Fails:**
1. Michigan node at height 5923 has block hash `abc123`
2. Consensus chain at 5923 has block hash `def456` (different!)
3. Michigan requests blocks starting at 5924 from consensus peer
4. Consensus sends block 5924 with `previous_hash = def456`
5. Michigan rejects block 5924 because it expects `previous_hash = abc123`
6. Error: "Fork detected: block 5924 previous_hash mismatch"
7. Sync fails, timeout occurs
8. Loop repeats: detect fork → sync → reject → timeout → detect fork...

**Fix Required:**
Before requesting blocks, the function MUST:
1. Query the peer for their block hash at `current` height
2. If hashes differ, search backwards to find common ancestor
3. Rollback to common ancestor height
4. THEN request blocks from `common_ancestor + 1`

---

### Bug #2: Incorrect Masternode Authority Analysis

**Location:** `src/masternode_authority.rs` line 273-281

**Problem:**
```rust
pub async fn analyze_our_chain_authority(...) -> ChainAuthorityAnalysis {
    // Get all active masternodes
    let active_masternodes = masternode_registry.list_active().await;
    
    // Filter to only connected masternodes (they support our chain)
    let connected_masternodes: Vec<&MasternodeInfo> = if let Some(cm) = connection_manager {
        active_masternodes
            .iter()
            .filter(|mn| cm.is_connected(&mn.masternode.address))  // ← BUG
            .collect()
    }
    // ...
}
```

**Why This Is Wrong:**
- "Connected" doesn't mean "on the same chain"
- A masternode can be connected to Michigan node but be on the consensus chain
- Michigan thinks it has WF:5 (5 whitelisted free masternodes connected)
- Consensus has WF:2 (2 peers at consensus height)
- Michigan incorrectly decides "KEEP our chain - we have higher authority"
- But those 5 masternodes might actually be on the consensus chain!

**Fix Required:**
Instead of checking connection status, need to:
1. Query each connected peer for their block hash at specific heights
2. Count how many peers have the same hash as us (truly on our chain)
3. Count how many peers have the consensus hash (on peer chain)
4. Use ACTUAL chain alignment, not connection status

---

### Bug #3: Common Ancestor Search Not Used in Sync Flow

**Location:** `src/network/fork_resolver.rs` has excellent `find_common_ancestor()` function

**Problem:**
- The exponential/binary search for common ancestor exists (lines 194-321)
- But it's **never actually called** during fork resolution sync!
- The `sync_from_specific_peer()` function doesn't use this algorithm
- Instead it just blindly requests blocks from `current + 1`

**Fix Required:**
Integrate `find_common_ancestor()` into the sync flow:
1. When fork detected, call `find_common_ancestor()` with peer
2. Rollback to that height
3. Request blocks from there
4. Apply blocks sequentially with validation

---

## Why Nodes Are Stuck

### The Infinite Loop

```
┌─────────────────────────────────────────────────┐
│ 1. Node detects fork (consensus at 5932)       │
│    → "I'm at 5923, they're at 5932"             │
└────────────────┬────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────┐
│ 2. Masternode authority comparison              │
│    → Counts CONNECTED peers, not chain peers    │
│    → "I have WF:5, they have WF:2"              │
│    → Decision: "KEEP our chain" (WRONG!)        │
└────────────────┬────────────────────────────────┘
                 │ (Sometimes decides SWITCH)
                 ▼
┌─────────────────────────────────────────────────┐
│ 3. If SWITCH: Call sync_from_specific_peer()   │
│    → Request blocks 5924-5932                   │
│    → NO ROLLBACK performed                      │
└────────────────┬────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────┐
│ 4. Receive block 5924 from consensus            │
│    → previous_hash = consensus_5923_hash        │
│    → Our 5923 has different hash!               │
│    → Reject: "previous_hash mismatch"           │
└────────────────┬────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────┐
│ 5. Sync times out after 60 seconds              │
│    → "Failed to sync during fork resolution"    │
│    → No blocks applied                          │
└────────────────┬────────────────────────────────┘
                 │
                 └──► LOOP BACK TO STEP 1
```

---

## Proposed Fixes

### Fix 1: Add Rollback to `sync_from_specific_peer()`

**File:** `src/blockchain.rs` lines 951-1000

**Changes:**
```rust
pub async fn sync_from_specific_peer(&self, peer_ip: &str) -> Result<(), String> {
    let current = self.current_height.load(Ordering::Acquire);
    
    // NEW: Query peer's block hash at our current height
    let peer_registry = self.peer_registry.read().await;
    let registry = peer_registry.as_ref().ok_or("No peer registry available")?;
    
    let peer_hash_at_current = self.query_peer_block_hash(registry, peer_ip, current).await?;
    let our_hash = self.get_block_hash(current)?;
    
    // NEW: If hashes differ, find common ancestor and rollback
    let start_height = if peer_hash_at_current != our_hash {
        tracing::warn!(
            "🔀 Fork detected at height {}: our hash {} vs peer hash {}",
            current,
            hex::encode(&our_hash[..8]),
            hex::encode(&peer_hash_at_current[..8])
        );
        
        // Find common ancestor using exponential/binary search
        let common_ancestor = self.find_common_ancestor_with_peer(peer_ip, current).await?;
        
        tracing::info!("✓ Found common ancestor at height {}", common_ancestor);
        
        // Rollback to common ancestor
        self.rollback_to_height(common_ancestor).await?;
        
        common_ancestor + 1
    } else {
        current + 1
    };
    
    // Now request blocks from the correct starting point
    let batch_end = self.calculate_expected_height();
    let req = NetworkMessage::GetBlocks(start_height, batch_end);
    registry.send_to_peer(peer_ip, req).await?;
    
    // ... rest of sync logic
}
```

### Fix 2: Correct Masternode Authority Analysis

**File:** `src/masternode_authority.rs` lines 262-293

**Changes:**
```rust
pub async fn analyze_our_chain_authority(
    masternode_registry: &crate::masternode_registry::MasternodeRegistry,
    connection_manager: Option<&crate::network::connection_manager::ConnectionManager>,
    peer_registry: Option<&crate::network::peer_connection_registry::PeerConnectionRegistry>,
    our_height: u64,  // NEW: Need height to query
    our_tip_hash: &[u8; 32],  // NEW: Need hash to compare
) -> ChainAuthorityAnalysis {
    let active_masternodes = masternode_registry.list_active().await;
    
    // NEW: Query each connected masternode for their actual chain state
    let mut chain_aligned_masternodes = Vec::new();
    
    if let (Some(cm), Some(pr)) = (connection_manager, peer_registry) {
        for mn in &active_masternodes {
            if cm.is_connected(&mn.masternode.address) {
                // Query this masternode's block hash at our height
                if let Ok(their_hash) = pr.query_block_hash(&mn.masternode.address, our_height).await {
                    // Only count if they're actually on our chain
                    if their_hash == *our_tip_hash {
                        chain_aligned_masternodes.push(mn);
                    }
                }
            }
        }
    }
    
    // Build whitelist status for chain-aligned masternodes only
    let mut whitelist_status = HashMap::new();
    if let Some(pr) = peer_registry {
        for mn in &chain_aligned_masternodes {
            let is_whitelisted = pr.is_whitelisted(&mn.masternode.address).await;
            whitelist_status.insert(mn.masternode.address.clone(), is_whitelisted);
        }
    }
    
    ChainAuthorityAnalysis::from_masternodes(&chain_aligned_masternodes, &whitelist_status)
}
```

### Fix 3: Integrate Common Ancestor Search

**File:** `src/blockchain.rs` - add new helper method

```rust
/// Query a peer for their block hash at a specific height
async fn query_peer_block_hash(
    &self,
    registry: &PeerConnectionRegistry,
    peer_ip: &str,
    height: u64,
) -> Result<[u8; 32], String> {
    // Create message to request specific block hash
    let req = NetworkMessage::GetBlockHash(height);
    registry.send_to_peer(peer_ip, req).await?;
    
    // Wait for response with timeout
    // ... implementation
}

/// Find common ancestor with a peer using exponential/binary search
async fn find_common_ancestor_with_peer(
    &self,
    peer_ip: &str,
    search_start: u64,
) -> Result<u64, String> {
    let resolver = crate::network::fork_resolver::ForkResolver::default();
    
    let check_fn = |height: u64| async move {
        let our_hash = self.get_block_hash(height)?;
        let peer_hash = self.query_peer_block_hash(registry, peer_ip, height).await?;
        Ok(our_hash == peer_hash)
    };
    
    resolver.find_common_ancestor(search_start, search_start, check_fn).await
}
```

---

## Impact Assessment

### Current Damage
- ✅ **No funds lost** - transactions are deterministic across all forks
- ⚠️  **Network split** - 4+ competing chains, no consensus
- ⚠️  **Nodes stuck** - Cannot automatically resolve due to bugs
- ⚠️  **Block production stalled** - Some nodes cannot produce valid blocks

### After Fix
- ✅ Nodes will automatically find common ancestor
- ✅ Rollback to common ground
- ✅ Sync correct chain from consensus
- ✅ Network converges on single canonical chain

---

## Immediate Action Items

1. **Deploy fixes to all three bugs**
   - Prioritize Fix #1 (sync with rollback) - most critical
   - Fix #2 (authority analysis) - prevents wrong decisions
   - Fix #3 (integrate common ancestor) - ensures clean sync

2. **Test fixes on testnet**
   - Create artificial fork
   - Verify nodes resolve correctly
   - Confirm exponential search works

3. **Deploy to mainnet**
   - Rolling update to all nodes
   - Monitor fork resolution in logs
   - Verify network converges

4. **Manual intervention (if needed before fix)**
   - Identify consensus chain (likely `243c7f65f5c59e9e` with 2 peers)
   - Force all nodes to rollback to height 5921
   - Manually sync consensus blocks 5922-5932
   - Resume normal operation

---

## Prevention for Future

1. **Add network message: `GetBlockHash(height)`**
   - Allow querying specific block hash without full block
   - Enables efficient common ancestor finding

2. **Improve fork detection**
   - Add heartbeat to exchange tip hashes every 10 seconds
   - Detect forks immediately, not after multiple blocks

3. **Better logging**
   - Log exact peer chain states (height + hash)
   - Track which peer is on which chain
   - Add metrics for fork resolution success/failure

4. **Add fork resolution tests**
   - Test case: 10-block deep fork
   - Test case: Same-height fork with different hashes
   - Test case: Multiple competing chains
   - Verify common ancestor search works correctly

---

## Conclusion

The fork is NOT due to consensus failure or malicious actors. It's a **bug in the fork resolution sync mechanism** that prevents nodes from rolling back far enough before syncing. The fixes are well-defined and can be implemented surgically without major refactoring.

**Estimated time to fix:** 4-6 hours of development + testing  
**Risk level:** Low - fixes are in sync logic, not consensus  
**Priority:** CRITICAL - network is currently non-functional
