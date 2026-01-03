# Phase 3: Masternode Synchronization Optimization

**Status**: 🚧 IN PROGRESS  
**Date**: 2026-01-03  
**Objective**: Ensure masternodes stay synchronized through intelligent sync coordination and prioritized block propagation

---

## Overview

Phase 3 builds on Phase 1's relaxed timeouts and Phase 2's connection protection by adding **intelligent synchronization** to prevent the height divergence observed in logs (nodes at 1919-4805 blocks).

## Problem Statement from Logs

Masternodes showed:
1. **Severe height divergence**: Heights 1919, 2817, 3651, 4805
2. **Failed sync attempts**: Multiple "missing pongs" during critical sync periods
3. **No proactive sync**: Nodes waited for peer-initiated sync instead of actively catching up
4. **Equal treatment**: Whitelisted masternodes treated same as regular peers for sync priority

## Root Causes

### 1. Passive Sync Model
- Nodes only sync when they receive unsolicited blocks
- No active "am I behind?" checking
- No prioritization of trusted masternode sources

### 2. Consensus-Based Sync Rejection
From `peer_connection.rs` lines 840-846:
```rust
// If less than 50% of peers agree with this peer, it's not canonical
if total_peers_with_height > 0
    && (supporting_peers_count as f64 / total_peers_with_height as f64) < 0.5
{
    info!("📊 Peer {} NOT on consensus chain", self.peer_ip);
    // Defers sync - problem if most peers are also behind!
}
```
**Issue**: If most connected peers are behind, node won't sync from ahead peers.

### 3. No Sync Coordinator
- No background task checking for better chains
- No proactive block requests from masternodes
- Manual intervention required when nodes fall behind

---

## Phase 3 Implementation

### Component 1: Sync Coordinator (High Priority)

**Purpose**: Background task that continuously monitors masternode heights and proactively syncs from best sources.

**Location**: `src/blockchain.rs` (new method + background task)

**Features**:
- Runs every 30 seconds (configurable)
- Queries all whitelisted masternode heights
- Finds consensus height (median of whitelisted nodes)
- If local height < consensus - 5 blocks: initiate aggressive sync
- Prioritizes whitelisted masternodes as truth source

**Implementation**:

```rust
/// Spawns background sync coordinator for masternode networks
pub fn spawn_sync_coordinator(
    self: Arc<Self>,
    peer_registry: Arc<PeerConnectionRegistry>,
    masternode_registry: Arc<MasternodeRegistry>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(
            tokio::time::Duration::from_secs(30) // Check every 30 seconds
        );
        
        loop {
            interval.tick().await;
            
            // Get all whitelisted masternode heights
            let mut mn_heights: Vec<(String, u64)> = Vec::new();
            
            // Query registered masternodes
            if let Ok(masternodes) = masternode_registry.get_active_masternodes().await {
                for mn in masternodes {
                    if let Some(height) = peer_registry
                        .get_peer_height(&mn.masternode.address)
                        .await
                    {
                        mn_heights.push((mn.masternode.address.clone(), height));
                    }
                }
            }
            
            // Also check any connected whitelisted peers
            for peer_ip in peer_registry.get_connected_peers().await {
                if peer_registry.is_whitelisted(&peer_ip).await {
                    if let Some(height) = peer_registry.get_peer_height(&peer_ip).await {
                        if !mn_heights.iter().any(|(ip, _)| ip == &peer_ip) {
                            mn_heights.push((peer_ip, height));
                        }
                    }
                }
            }
            
            if mn_heights.is_empty() {
                continue; // No masternodes to sync from
            }
            
            // Calculate consensus height (median)
            let mut heights: Vec<u64> = mn_heights.iter().map(|(_, h)| *h).collect();
            heights.sort_unstable();
            let consensus_height = if heights.len() % 2 == 0 {
                (heights[heights.len() / 2 - 1] + heights[heights.len() / 2]) / 2
            } else {
                heights[heights.len() / 2]
            };
            
            let our_height = self.get_height().await;
            
            // Are we behind consensus?
            if consensus_height > our_height + 5 {
                // Find best peer to sync from (highest height among whitelisted)
                let best_peer = mn_heights
                    .iter()
                    .max_by_key(|(_, h)| h)
                    .map(|(ip, _)| ip.clone());
                
                if let Some(peer_ip) = best_peer {
                    tracing::warn!(
                        "🔄 SYNC COORDINATOR: We're at height {}, masternode consensus at {}. \
                         Requesting sync from whitelisted peer {}",
                        our_height, consensus_height, peer_ip
                    );
                    
                    // Request blocks from our height to consensus
                    if let Err(e) = peer_registry
                        .send_to_peer(
                            &peer_ip,
                            &NetworkMessage::GetBlockRange {
                                start_height: our_height + 1,
                                end_height: consensus_height.min(our_height + 500), // Max 500 blocks per request
                            },
                        )
                        .await
                    {
                        tracing::error!("Failed to request sync from {}: {}", peer_ip, e);
                    }
                } else {
                    tracing::warn!(
                        "🔄 SYNC COORDINATOR: Behind consensus (our: {}, consensus: {}) \
                         but no suitable peer found",
                        our_height, consensus_height
                    );
                }
            } else if our_height > consensus_height + 100 {
                // We're significantly ahead - potential fork or bad state
                tracing::error!(
                    "⚠️ SYNC COORDINATOR: We're at height {} but masternode consensus is {}. \
                     Possible fork or network partition!",
                    our_height, consensus_height
                );
            } else {
                tracing::debug!(
                    "✅ SYNC COORDINATOR: Height {} in sync with consensus {}",
                    our_height, consensus_height
                );
            }
        }
    })
}
```

**Call Site**: `src/main.rs` after server initialization

```rust
// Phase 3: Start sync coordinator
let sync_handle = blockchain.clone().spawn_sync_coordinator(
    peer_registry.clone(),
    masternode_registry.clone(),
);
```

---

### Component 2: Prioritized Sync from Whitelisted Peers

**Purpose**: When receiving blocks from whitelisted masternodes, bypass consensus checks and trust the source.

**Location**: `src/network/peer_connection.rs` (modify existing sync logic)

**Current Problem** (lines 821-846):
```rust
// CONSENSUS CHECK: If we're significantly behind, verify this peer is on consensus chain
let peer_tip = self.peer_height.read().await.unwrap_or(end_height);
if peer_tip > our_height + 50 {
    // Count how many peers agree with this peer
    // If <50% agree, reject sync
}
```

**Phase 3 Enhancement**:

```rust
// CONSENSUS CHECK: If we're significantly behind, verify this peer is on consensus chain
// Phase 3: EXEMPT whitelisted masternodes from consensus check - they ARE the consensus
let peer_tip = self.peer_height.read().await.unwrap_or(end_height);
if peer_tip > our_height + 50 && !is_whitelisted {
    // Regular peers need consensus verification
    let connected_peers = peer_registry.get_connected_peers().await;
    // ... existing consensus check logic ...
    
    if (supporting_peers_count as f64 / total_peers_with_height as f64) < 0.5 {
        info!(
            "📊 Peer {} NOT on consensus chain ({}/{} peers agree at height {}). \
             Deferring to periodic consensus.",
            self.peer_ip, supporting_peers_count, total_peers_with_height, peer_tip
        );
        // Try to add sequential blocks, but don't do fork resolution
        // ... existing code ...
        return Ok(());
    }
} else if is_whitelisted && peer_tip > our_height + 50 {
    // Phase 3: Whitelisted masternodes bypass consensus - they define consensus
    info!(
        "🛡️ PRIORITY SYNC: Whitelisted masternode {} at height {} (we're at {}). \
         Trusting without consensus check.",
        self.peer_ip, peer_tip, our_height
    );
}
```

**Benefits**:
- Whitelisted masternodes can sync nodes even when most peers are behind
- Breaks deadlock where all nodes wait for consensus
- Trusted source = faster convergence

---

### Component 3: Aggressive Fork Resolution for Masternodes

**Purpose**: When whitelisted masternode has conflicting chain, resolve more aggressively.

**Location**: `src/network/peer_connection.rs` (in fork handling sections)

**Enhancement**:

```rust
// Phase 3: More aggressive fork resolution for whitelisted peers
let max_search_depth = if is_whitelisted { 10000 } else { 2000 };
let max_retry_attempts = if is_whitelisted { 10 } else { 3 };

if is_whitelisted {
    tracing::warn!(
        "🔀 PRIORITY FORK RESOLUTION: Whitelisted masternode {} has conflicting chain. \
         Aggressively searching for common ancestor (max depth: {}).",
        self.peer_ip, max_search_depth
    );
}
```

**Impact**: Trusted peers get deeper search and more retries to resolve forks.

---

### Component 4: Height Announcement Improvements

**Purpose**: Ensure all nodes know each other's heights for better sync decisions.

**Location**: `src/network/peer_connection.rs` (in heartbeat/ping logic)

**Current**: Height only shared during block responses  
**Phase 3**: Include height in periodic pings

**Enhancement**:

```rust
// Modify NetworkMessage enum to include optional height
NetworkMessage::Ping { 
    nonce, 
    timestamp,
    height: Option<u64>, // Phase 3: Advertise our height
}

// When sending ping (lines ~400-410)
async fn send_ping(&self, blockchain: &Arc<Blockchain>) -> Result<(), String> {
    let nonce = rand::random();
    let timestamp = Utc::now().timestamp();
    let height = blockchain.get_height().await; // Phase 3: Include height
    
    self.send_message(&NetworkMessage::Ping { 
        nonce, 
        timestamp,
        height: Some(height), // Phase 3
    }).await
}

// When receiving ping
NetworkMessage::Ping { nonce, timestamp, height } => {
    self.handle_ping(*nonce, *timestamp).await?;
    
    // Phase 3: Update peer height from ping
    if let Some(h) = height {
        *self.peer_height.write().await = Some(*h);
    }
}
```

**Benefits**:
- Sync coordinator has real-time height data
- No need to wait for block responses to know peer state
- Faster detection of sync needs

---

### Component 5: Masternode-Specific Sync Timeouts

**Purpose**: Give masternode sync requests more time to complete.

**Location**: `src/blockchain.rs` (in sync request handling)

**Enhancement**:

```rust
// Phase 3: Extended sync timeout for whitelisted masternodes
const PEER_SYNC_TIMEOUT_SECS: u64 = 120; // Regular peers
const MASTERNODE_SYNC_TIMEOUT_SECS: u64 = 300; // 5 minutes for masternodes

// In sync request logic
let timeout = if is_whitelisted {
    MASTERNODE_SYNC_TIMEOUT_SECS
} else {
    PEER_SYNC_TIMEOUT_SECS
};
```

---

## Configuration

Add to `config.toml`:

```toml
[sync]
# Sync coordinator check interval (seconds)
coordinator_interval = 30

# Maximum blocks to request per sync batch
max_sync_batch_size = 500

# Height difference to trigger aggressive sync
sync_threshold = 5

# Enable/disable sync coordinator
enable_coordinator = true
```

---

## Testing & Validation

### Test Scenario 1: Node Behind by 100 Blocks

**Setup**: 
- Node A at height 1000
- Masternodes at height 1100

**Expected**:
1. Sync coordinator detects gap within 30 seconds
2. Logs: `🔄 SYNC COORDINATOR: We're at height 1000, masternode consensus at 1100`
3. Requests blocks 1001-1100 from highest masternode
4. Receives and applies blocks within 60 seconds
5. Logs: `✅ SYNC COORDINATOR: Height 1100 in sync with consensus 1100`

### Test Scenario 2: Most Peers Behind, One Masternode Ahead

**Setup**:
- Node A at height 2000
- Regular peers at height 2010-2050
- Whitelisted masternode at height 2500

**Expected**:
1. Without Phase 3: Consensus check rejects masternode blocks (minority chain)
2. With Phase 3: Whitelisted peer bypasses consensus check
3. Logs: `🛡️ PRIORITY SYNC: Whitelisted masternode X.X.X.X at height 2500`
4. Node syncs to 2500 despite regular peer consensus

### Test Scenario 3: Fork Resolution

**Setup**:
- Node A on fork A at height 3000
- Whitelisted masternode on fork B at height 3000 (different chain)

**Expected**:
1. Masternode sends blocks
2. Conflict detected
3. Logs: `🔀 PRIORITY FORK RESOLUTION: Whitelisted masternode has conflicting chain`
4. Aggressive search (10,000 depth vs 2,000 regular)
5. Common ancestor found and reorg performed

### Test Scenario 4: Height Awareness

**Expected Logs**:
```
📡 [Outbound] Sending ping to 192.168.1.10 (height: 5432)
📥 [Inbound] Received ping from 192.168.1.11 (height: 5450)
🔄 SYNC COORDINATOR: Peer 192.168.1.11 ahead by 18 blocks
```

---

## Monitoring

### Key Metrics

1. **Sync Coordinator Activity**:
   - `sync_coordinator_checks_total`: Total coordinator runs
   - `sync_coordinator_syncs_initiated`: Times sync was triggered
   - `sync_coordinator_height_diff_avg`: Average height difference detected

2. **Sync Performance**:
   - `masternode_sync_duration_seconds`: Time to sync from masternodes
   - `masternode_sync_blocks_received`: Blocks received from masternodes
   - `masternode_sync_failures`: Failed sync attempts

3. **Height Divergence**:
   - `peer_height_variance`: Standard deviation of peer heights
   - `masternode_consensus_height`: Median height of masternodes
   - `local_height_behind_consensus`: How far behind we are

### Log Patterns

**Healthy Sync**:
```
✅ SYNC COORDINATOR: Height 5432 in sync with consensus 5433
```

**Active Sync**:
```
🔄 SYNC COORDINATOR: We're at height 5400, masternode consensus at 5500. Requesting sync from whitelisted peer 192.168.1.10
📥 [Outbound] Received 100 blocks (height 5401-5500) from 192.168.1.10
✅ SYNC COORDINATOR: Height 5500 in sync with consensus 5500
```

**Fork Detection**:
```
⚠️ SYNC COORDINATOR: We're at height 5600 but masternode consensus is 5500. Possible fork!
```

---

## Integration with Phases 1 & 2

| Feature | Phase 1 | Phase 2 | Phase 3 |
|---------|---------|---------|---------|
| Connection Stability | ✅ 180s timeout | ✅ 2s reconnect | ✅ Extended sync timeout |
| Protection | ✅ 6 missed pongs | ✅ 50 reserved slots | ✅ Bypass consensus check |
| Sync Strategy | ❌ Passive | ❌ Passive | ✅ **Active coordinator** |
| Fork Resolution | ❌ Standard | ❌ Standard | ✅ **Aggressive for MN** |
| Height Awareness | ❌ On-demand | ❌ On-demand | ✅ **In ping/pong** |
| Reconnection | ✅ Exponential | ✅ Fast (2s) | ✅ Maintained |

---

## Implementation Order

### Step 1: Height in Ping/Pong (1 hour)
- Modify `NetworkMessage::Ping` enum
- Update ping sending logic
- Update ping handling logic
- Test: Verify height propagation in logs

### Step 2: Prioritized Sync Logic (2 hours)
- Add whitelist check to consensus validation
- Add aggressive fork resolution parameters
- Test: Sync from minority masternode

### Step 3: Sync Coordinator (3 hours)
- Implement `spawn_sync_coordinator` in `blockchain.rs`
- Add coordinator call in `main.rs`
- Add configuration options
- Test: Coordinator detects and syncs gaps

### Step 4: Extended Timeouts (30 minutes)
- Add masternode-specific sync timeouts
- Update relevant constants
- Test: Verify longer timeout in logs

### Step 5: Integration Testing (2 hours)
- Deploy to test network
- Simulate various height divergences
- Monitor sync coordinator behavior
- Validate fork resolution

**Total Estimated Time**: 8.5 hours

---

## Rollback Plan

If Phase 3 causes issues:

1. **Disable sync coordinator**: Set `enable_coordinator = false` in config
2. **Revert consensus bypass**: Comment out whitelist exemption in peer_connection.rs
3. **Keep height in ping**: Low risk, can remain even if other features reverted
4. **Monitor**: Phase 1 & 2 still provide connection stability

---

## Success Criteria

✅ All masternodes maintain height within ±10 blocks  
✅ Sync coordinator runs every 30 seconds without errors  
✅ Nodes behind by >5 blocks sync within 2 minutes  
✅ Whitelisted masternodes can sync nodes without consensus check  
✅ Fork resolution completes within 5 minutes for masternodes  
✅ No connection drops due to sync operations  

---

## Benefits

### 1. Proactive Sync
- Nodes actively seek better chains instead of waiting
- 30-second check interval catches divergence early
- Masternode consensus defines truth

### 2. Break Sync Deadlocks
- Whitelisted peers bypass consensus requirements
- One up-to-date masternode can sync entire network
- Prevents "all nodes behind" scenario

### 3. Intelligent Fork Resolution
- Trusted sources get deeper search
- More retries for important peers
- Faster convergence on canonical chain

### 4. Better Height Awareness
- Real-time peer state in ping/pong
- Coordinator has fresh data
- Faster sync decisions

### 5. Robust Sync Operations
- Extended timeouts for large syncs
- Handles slow networks gracefully
- Maintains Phase 1 & 2 stability

---

## Next Steps

1. ✅ Review implementation plan
2. 🚧 Implement Step 1: Height in Ping/Pong
3. 🚧 Implement Step 2: Prioritized Sync Logic
4. 🚧 Implement Step 3: Sync Coordinator
5. 🚧 Implement Step 4: Extended Timeouts
6. 🚧 Integration Testing
7. 📊 Deploy to mainnet with monitoring
8. 📈 Analyze sync performance metrics

---

**Document Version**: 1.0  
**Last Updated**: 2026-01-03  
**Next Review**: After integration testing
