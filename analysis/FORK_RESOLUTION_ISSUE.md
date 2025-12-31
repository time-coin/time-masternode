# Fork Resolution Issue Analysis

## Problem Summary
The network is experiencing a **permanent multi-way fork** where each of the 4 masternodes is stuck on a different chain at different heights. The nodes **detect the fork** but **fail to reorganize** to resolve it.

## Current State (Dec 26 21:14-21:17)

### Node Chain States
| Node | IP | Height | Block Hash (truncated) |
|------|------------|--------|----------------------|
| Michigan2 | 64.91.241.10 | 3724 | `719887f3053a4571` |
| Arizona | 50.28.104.50 | 3725 | `c2ffb6a23ff14ef5` |
| London | 165.84.215.117 | 3725 | `a613d43f7e7ae512` |
| Michigan | 69.167.168.176 | 3726 | `15fff58d09d16f37` |

**NOTE**: Even nodes at the same height (Arizona & London both at 3725) have **different block hashes**, proving they're on different forks.

## Root Cause

The blockchain sync logic in `src/network/server.rs` **detects forks** but only **skips incompatible blocks** without ever triggering chain reorganization:

```rust
WARN ⏭️ [Outbound] Skipped block 3725: Block 3725 previous_hash mismatch: expected 719887f3053a4571, got a2fcb74779650c78
WARN ⚠️ [Outbound] All 1 blocks skipped from 50.28.104.50
```

### What Should Happen
When a node receives blocks that don't connect to its chain:
1. **Detect fork** ✅ (working)
2. **Request blocks backwards** to find common ancestor ❌ (missing)
3. **Compare cumulative chain work** ❌ (not triggered)
4. **Reorganize to longer/heavier chain** ❌ (never called)

### What Actually Happens
1. **Detect fork** ✅
2. **Skip all incompatible blocks** ❌ (stops here)
3. **Keep requesting same blocks forever** ❌ (infinite loop)
4. **Never resolve** ❌

## Evidence from Logs

### Michigan2 trying to sync from 3724 → 3726
```
Dec 26 21:00:00 LW-Michigan2 timed[312887]:  INFO ⏳ Syncing from peers: 3724 → 3726 (2 blocks behind)
Dec 26 21:00:00 LW-Michigan2 timed[312887]:  INFO 📤 Requesting blocks 3725-3726 from 50.28.104.50
Dec 26 21:00:00 LW-Michigan2 timed[312887]:  WARN ⏭️ [Outbound] Skipped block 3725: Block 3725 previous_hash mismatch
Dec 26 21:01:01 LW-Michigan2 timed[312887]:  INFO ⏳ Still syncing... height 3724 / 3726 (60s elapsed)
Dec 26 21:01:01 LW-Michigan2 timed[312887]:  INFO 📤 Requesting blocks 3725-3726 from 50.28.104.50
Dec 26 21:01:01 LW-Michigan2 timed[312887]:  WARN ⏭️ [Outbound] Skipped block 3725: Block 3725 previous_hash mismatch
```
**Pattern**: Requests same blocks → Skips → Waits → Repeats **forever**

### Arizona trying to sync from 3725 → 3727
```
Dec 26 21:14:41 LW-Arizona timed[325888]:  INFO ⏳ Syncing from peers: 3725 → 3727 (2 blocks behind)
Dec 26 21:14:41 LW-Arizona timed[325888]:  WARN 🔀 Fork detected: block 3726 previous_hash mismatch
Dec 26 21:15:11 LW-Arizona timed[325888]:  INFO ⏳ Still syncing... height 3725 / 3727 (30s elapsed)
Dec 26 21:15:41 LW-Arizona timed[325888]:  INFO ⏳ Still syncing... height 3725 / 3727 (60s elapsed)
```
**Same issue**: Stuck in infinite retry loop.

## The Code That Exists But Never Runs

The codebase **has** reorganization logic at `src/blockchain.rs`:

```rust
pub async fn reorganize_to_chain(
    &self,
    common_ancestor: u64,
    new_blocks: Vec<Block>,
) -> Result<(), String>
```

And it's referenced in `src/network/server.rs` but the conditions to **trigger** it are never met during normal sync.

## Why Reorganization Never Triggers

Looking at the sync code flow:

1. **Receives blocks** via `NetworkMessage::Blocks`
2. **Tries to apply each block**
3. **If previous_hash doesn't match** → Skip block and log warning
4. **Never checks** if we should request earlier blocks
5. **Never calls** `should_switch_to_chain()` or `reorganize_to_chain()`

The reorganization code path appears to be designed for a different scenario (possibly manual intervention or full chain download) but is **not integrated into the automatic sync process**.

## Required Fixes

### 1. Implement Fork Resolution in Sync Logic
When blocks don't connect to our chain:
- **Binary search backwards** to find common ancestor
- **Request peer's entire chain** from common ancestor forward
- **Compare cumulative work** (already implemented)
- **Reorganize if peer has more work** (already implemented)

### 2. Add "Give Me Your Chain From Height X" Message
Current `GetBlocks(start, end)` isn't sufficient. Need ability to:
- Request a node's full chain from arbitrary height
- Get response with work proofs for comparison

### 3. Implement Proper Fork Choice Rule
Currently defaults to "first seen" but needs:
- **Longest chain by cumulative work**
- **Timestamp validation** (already partially there)
- **Tie-breaking rules**

## Impact

**Network is permanently stuck**. Without manual intervention (wiping databases and resyncing from a single source), nodes will never converge to a single chain.

This is a **critical consensus failure** that prevents the network from functioning.

## Recommended Actions

1. **Immediate**: Implement fork resolution trigger in sync logic
2. **Short-term**: Add chain work comparison to sync process  
3. **Testing**: Create fork scenarios in test suite to verify resolution
4. **Documentation**: Document expected fork resolution behavior
