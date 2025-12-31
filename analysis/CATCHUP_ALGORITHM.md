# TimeCoin Catchup Algorithm

## Overview

The catchup algorithm is an emergency synchronization mechanism that activates when the entire blockchain network falls significantly behind schedule. It enables rapid block generation to bring the network back to the expected height.

## When Catchup Activates

Catchup mode is triggered when:
- **Current block height + 3 blocks < Expected height** (MIN_BLOCKS_BEHIND_FOR_CATCHUP = 3)
- Peers cannot provide the missing blocks
- All peers are equally behind (network-wide issue, not just this node)

## Catchup Algorithm Flow

### Phase 1: Detect Network-Wide Synchronization Issue

```
Current Height: 100
Expected Height: 110  
Blocks Behind: 10 (> 3 minimum threshold)

Action: Query all connected peers for their heights
Result: All peers also at height ~100 (network-wide issue confirmed)
Decision: Enter catchup mode
```

### Phase 2: Select Temporary Leader

Leader is selected based on **weighted uptime score**:

```
Formula: score = tier_weight × uptime_seconds

Tier Weights:
- Gold:   100x multiplier
- Silver:  10x multiplier  
- Bronze:   1x multiplier
- Free:     1x multiplier

Example Scores:
- Gold tier, 86400 uptime:    8,640,000
- Silver tier, 86400 uptime:    864,000
- Bronze tier, 86400 uptime:     86,400

Selection: Highest score wins (deterministic, ties broken by address ASC)
```

### Phase 3: Fast-Track Block Production

**Leader Node Behavior:**
- Generate blocks at accelerated rate (no 10-minute wait)
- Broadcast each block to the network
- Continue until reaching target height
- Then exit catchup mode

**Follower Node Behavior:**
- Wait for leader to broadcast blocks
- Receive and validate blocks from leader
- Progress tracker: update height when leader's block arrives
- Monitor leader for timeout (>30 seconds with no activity)
- If timeout: **exit immediately** (cannot self-generate to avoid forking)

### Phase 4: Return to Normal Operation

Once `current_height >= target_height`:
1. Exit catchup mode
2. Reset block generation to normal 10-minute schedule
3. Resume TSDC consensus

## Detailed State Machine

```
┌─────────────────────────────────────────────────────────┐
│ Normal Operation (10-minute blocks via TSDC)            │
└────────────────────┬────────────────────────────────────┘
                     │
                     ▼
         Check: Behind >= 3 blocks?
                     │
          ┌──────────┴──────────┐
          NO                   YES
          │                     │
          ▼                     ▼
       Continue         Try Peer Sync
     Normal Ops         (Request blocks)
                              │
                    ┌─────────┴─────────┐
                   YES                 NO
               (Got blocks)        (Sync failed)
                    │                   │
                    ▼                   ▼
               Continue             Check: All peers
              Normal Ops            equally behind?
                                        │
                            ┌───────────┴───────────┐
                           NO                      YES
                            │                       │
                            ▼                       ▼
                        Exit with                Enter
                        Error              Catchup Mode
                                                  │
                    ┌─────────────────────────────┤
                    │                             │
            ┌───────▼──────┐         ┌────────────▼──────┐
            │ Select Leader │         │ Select Follower  │
            └───────┬──────┘         └────────────┬──────┘
                    │                             │
        ┌───────────▼───────────┐    ┌────────────▼─────────┐
        │ Generate & Broadcast  │    │ Wait for Leader      │
        │ Blocks at Fast Rate   │    │ Follow Height        │
        │ Until Target Height   │    │ Monitor Timeout (30s)│
        └───────────┬───────────┘    └────────────┬─────────┘
                    │                             │
                    └──────────────┬──────────────┘
                                   │
                            Height >= Target?
                                   │
                        ┌──────────┴──────────┐
                       YES                  NO
                        │                    │
                        ▼                    ▼
                   Exit Catchup        Continue Until:
                  Return to Normal    - Leader timeout
                  10-minute TSDC      - Target reached
```

## Key Properties

### Safety (No Forking)
- Only **one leader** generates blocks (elected by weighted uptime)
- Followers **cannot self-generate** (would create fork)
- Leader timeout forces exit (manual sync needed)
- No competing block producers

### Efficiency
- Leader generates blocks rapidly (no 10-min wait)
- Network recovers to expected height quickly
- Reduced time spent out-of-sync

### Resilience
- Deterministic leader selection (same everywhere)
- Weighted by tier to favor stable, long-running nodes
- If leader fails: graceful exit, not escalation

## Example Scenario

```
Time: 21:00 UTC
Network falls offline for 2 hours

Node A Height: 100 blocks behind
Node B Height: 100 blocks behind  
Node C Height: 100 blocks behind
(All peers equally affected)

Step 1: Catchup triggered
Step 2: Leader election
  - Gold masternode: score = 1,000,000
  - Silver masternode: score = 500,000
  - Winner: Gold masternode is leader

Step 3: Fast-track production
  - Leader generates 100 blocks in ~15 minutes
  - Followers receive and validate each block
  - No 10-minute delays

Step 4: Return to normal
  - All nodes now at correct height
  - Back to TSDC 10-minute block schedule
```

## Configuration Parameters

Location: `src/blockchain.rs`

```rust
const MIN_BLOCKS_BEHIND_FOR_CATCHUP: u64 = 3;  // Threshold
const BLOCK_TIME_SECONDS: i64 = 600;           // 10 minutes normal
const LEADER_TIMEOUT: Duration = 30 seconds;   // Before follower exits
```

## Error Conditions

1. **No Masternodes Available**
   - Action: Cannot elect leader
   - Result: Exit with error

2. **Leader Times Out**
   - Action: Leader hasn't broadcast in 30 seconds
   - Result: Followers exit, return to normal sync
   - Reason: Don't want to split into competing chains

3. **Partial Network Agreement**
   - Action: Some peers ahead, some behind
   - Result: Exit catchup, manual sync required
   - Reason: Indicates network partition risk

## Future Improvements

- [ ] Implement leader replacement if timeout
- [ ] Add catchup telemetry/metrics
- [ ] Verify blocks signed by leader
- [ ] Support partial catchup (resume from checkpoints)
- [ ] Cross-shard catchup coordination
