# BFT Leader Selection for Catchup Mode

**Status**: ✅ **IMPLEMENTED** (2025-12-13 00:35 UTC)

## Overview

This document describes the BFT-based leader selection algorithm used during blockchain catchup mode to prevent fork creation when multiple nodes are simultaneously catching up.

## Problem Statement

When the network falls behind schedule (e.g., at height 1723 when it should be at 1728):

**Without Leader Selection** (BEFORE):
```
❌ All nodes simultaneously generate blocks:
   - Arizona: generates blocks 1724-1728 with hash A
   - London:  generates blocks 1724-1728 with hash B  
   - Michigan: generates blocks 1724-1728 with hash C
   
   Result: 3 COMPETING FORKS! Network split!
```

**With Leader Selection** (NOW):
```
✅ Only ONE node generates blocks:
   - Leader (highest score): generates blocks 1724-1728
   - Followers: WAIT and accept leader's blocks
   
   Result: SINGLE CHAIN! Network unified!
```

## Leader Selection Algorithm

### 1. **Score Calculation**

```rust
score = tier_weight × uptime_seconds
```

Where:
- `tier_weight`: Based on masternode tier
  - **Gold**: 100
  - **Silver**: 10  
  - **Bronze**: 1
  - **Free**: 0 (cannot be leader)
  
- `uptime_seconds`: Total cumulative uptime of the masternode

### 2. **Selection Process**

```
1. Filter out Free tier nodes (tier_weight = 0)
2. Calculate score for each eligible masternode
3. Sort by:
   a) Score (descending) - highest score first
   b) Address (ascending) - deterministic tiebreaker
4. Select node with highest combined score
```

### 3. **Examples**

| Node | Tier | Uptime | Score Calculation | Final Score |
|------|------|--------|-------------------|-------------|
| Arizona | Gold | 86400s (1 day) | 100 × 86400 | **8,640,000** 👑 |
| London | Silver | 172800s (2 days) | 10 × 172800 | 1,728,000 |
| Michigan | Bronze | 259200s (3 days) | 1 × 259200 | 259,200 |

**Result**: Arizona is selected as leader due to Gold tier dominance.

| Node | Tier | Uptime | Score Calculation | Final Score |
|------|------|--------|-------------------|-------------|
| London | Silver | 259200s (3 days) | 10 × 259200 | **2,592,000** 👑 |
| Arizona | Silver | 172800s (2 days) | 10 × 172800 | 1,728,000 |
| Michigan | Bronze | 604800s (7 days) | 1 × 604800 | 604,800 |

**Result**: London wins due to better uptime among same-tier nodes.

## Catchup Process

### Leader Node Behavior

```rust
1. Selected as leader (highest score)
2. Generates catchup blocks sequentially:
   - Block 1724 → broadcast to network
   - Block 1725 → broadcast to network
   - ... continue until caught up
3. Logs: "👑 Leader generating block {height}"
```

### Follower Node Behavior

```rust
1. NOT selected as leader
2. Waits for leader's blocks:
   - Check every 500ms if block arrived
   - If block received: accept and move to next height
   - Log progress: "📊 Catchup progress (following leader)"
3. Timeout handling (30 seconds):
   - If no blocks from leader for 30s
   - Switch to emergency self-generation
   - Log: "⚠️ Leader timeout - switching to self-generation"
```

## Code Location

**File**: `src/blockchain.rs`

### `select_catchup_leader()` (Lines 343-406)

```rust
async fn select_catchup_leader(&self) -> (bool, Option<String>)
```

**Returns**:
- `(true, Some(address))` - This node IS the leader
- `(false, Some(address))` - This node is follower, address is leader
- `(false, None)` - No eligible masternodes

**Algorithm**:
1. Get all active masternodes
2. Calculate score = tier_weight × uptime_seconds
3. Filter out Free tier (weight = 0)
4. Sort by score DESC, then address ASC
5. Return highest scoring node as leader

### `bft_catchup_mode()` (Lines 408-500+)

```rust
async fn bft_catchup_mode(&self, params: CatchupParams) -> Result<(), String>
```

**Modified to use leader selection**:

```rust
// NEW: Select leader at start
let (is_leader, leader_address) = self.select_catchup_leader().await;

if !is_leader {
    // FOLLOWER: Wait for leader's blocks
    loop {
        if our_height >= next_height {
            // Block received from leader!
            break;
        }
        if timeout_exceeded() {
            // Leader failed, become emergency leader
            break;
        }
        sleep(500ms);
    }
} else {
    // LEADER: Generate and broadcast blocks
    let block = generate_catchup_block(...).await?;
    add_block_internal(block).await?;
    // Block automatically broadcasts to followers
}
```

## Benefits

### 1. **Prevents Fork Creation**
- Only ONE node generates blocks during catchup
- All other nodes wait and accept leader's blocks
- **Network stays unified on single chain**

### 2. **BFT-Aligned Selection**
- Prioritizes higher tier masternodes (Gold > Silver > Bronze)
- Rewards long uptime
- **Aligns with BFT consensus philosophy**

### 3. **Fault Tolerance**
- 30-second timeout for leader failure
- Automatic fallback to emergency leader
- **Network recovers even if leader crashes**

### 4. **Deterministic & Fair**
- Same inputs always select same leader
- All nodes agree on who the leader is
- **No ambiguity or conflicts**

## Testing Scenarios

### Scenario 1: Normal Catchup with Leader
```
Network: 5 blocks behind (1723 → 1728)
Masternodes: Arizona (Gold, 1d), London (Silver, 2d), Michigan (Bronze, 3d)

Expected:
1. Arizona selected as leader (Gold tier wins)
2. Arizona generates blocks 1724-1728
3. London and Michigan wait and accept Arizona's blocks
4. All nodes reach 1728 on same chain ✅
```

### Scenario 2: Leader Timeout
```
Network: 10 blocks behind (1720 → 1730)
Masternodes: Arizona (leader), London (follower), Michigan (follower)

Expected:
1. Arizona starts generating but crashes at block 1724
2. London and Michigan wait 30 seconds
3. After timeout, London becomes emergency leader
4. London continues catchup from 1724 onwards ✅
```

### Scenario 3: All Same Tier (Tiebreaker)
```
Network: 3 blocks behind
Masternodes: 
  - 165.84.215.117 (Silver, 1000s) → score: 10,000
  - 50.28.104.50 (Silver, 1000s)   → score: 10,000
  - 69.167.168.176 (Silver, 1000s) → score: 10,000

Expected:
1. All have same score
2. Sorted by address: 50.28.104.50 < 69.167.168.176 < 165.84.215.117
3. 50.28.104.50 selected as leader (lowest address) ✅
```

## Log Examples

### Leader Selection
```
🏆 Catchup leader selected: 50.28.104.50 (score: 8640000) - I AM LEADER
```

### Follower Waiting
```
🔄 Entering BFT consensus catchup mode: 1723 → 1728 (5 blocks)
🏆 Catchup leader selected: 69.167.168.176 (score: 10800000) - waiting for leader
📊 Catchup progress (following leader): 40.0% (1725/1728)
📊 Catchup progress (following leader): 60.0% (1726/1728)
```

### Leader Timeout
```
⚠️ Leader Some("69.167.168.176") timeout after 30s - switching to self-generation at height 1726
👑 Leader generating block 1726
```

## Configuration

### Constants

| Constant | Value | Purpose |
|----------|-------|---------|
| `LEADER_TIMEOUT` | 30 seconds | Max wait time for leader's blocks |
| `FOLLOWER_CHECK_INTERVAL` | 500ms | How often followers check for new blocks |

### Tier Weights

| Tier | Weight | Can Be Leader? |
|------|--------|----------------|
| Gold | 100 | ✅ Yes |
| Silver | 10 | ✅ Yes |
| Bronze | 1 | ✅ Yes |
| Free | 0 | ❌ No |

## Future Enhancements

### Potential Improvements

1. **Dynamic Timeout**: Adjust 30s timeout based on network conditions
2. **Leader Rotation**: Rotate leader every N blocks for fairness
3. **Reputation Score**: Factor in past performance/reliability
4. **Geographic Distribution**: Prefer leaders in different regions
5. **Block Verification**: Followers cryptographically verify leader's blocks

### Not Implemented Yet

- Binary search for common ancestor (optimization)
- Active block fetching during fork (feature)
- Block hash in UTXO state verification (safety)

## Related Documentation

- `analysis/fork_resolution_implementation.md` - Fork detection & consensus
- `FORK_RESOLUTION_QUICKREF.md` - Quick reference guide
- `BUGFIX_FORK_ROLLBACK.md` - Original fork issues

## Conclusion

**The BFT leader selection solves the core catchup fork problem** by ensuring only ONE node generates blocks during catchup, while all others wait and follow. This maintains network unity and prevents the creation of competing forks during synchronized catchup scenarios.

**Status**: Production-ready ✅
