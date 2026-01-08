# Fork Resolution Fixes - Quick Start Guide

## What Was Fixed

Your nodes were stuck in an **infinite fork resolution loop**. The problem:
- ✅ Forks were detected correctly
- ✅ Nodes requested blocks to find common ancestor
- ❌ **No stop condition when fork too deep (> 100 blocks)**
- ❌ **No retry limit - kept trying forever**
- ❌ **Same blocks requested repeatedly without progress**

## Changes Made

### 1. Circuit Breaker (Deep Fork Detection)
- Stops when fork exceeds 100 blocks
- Logs: `🚨 DEEP FORK DETECTED: X blocks deep`
- Prevents searching indefinitely for common ancestor

### 2. Retry Limit
- Maximum 50 attempts per fork
- Maximum 15 minutes per fork resolution
- Logs: `🚨 Fork resolution exceeded retry limit`

### 3. Enhanced Logging
- Clear visibility when reorg succeeds: `✅✅✅ REORGANIZATION SUCCESSFUL ✅✅✅`
- Clear errors when reorg fails: `❌❌❌ REORGANIZATION FAILED ❌❌❌`
- Shows exactly what happened and why

### 4. Fail-Fast for Whitelist
- If trusted masternode's chain can't be applied, stop immediately
- Don't retry - indicates serious problem

## Quick Deployment

### Option 1: If Nodes Are Responsive
```bash
# From your Windows machine, deploy to all nodes
cd C:\Users\wmcor\projects\timecoin
bash scripts/deploy_fork_fixes.sh
```

This will:
1. Upload new binary to all servers
2. Stop each node
3. Replace binary
4. Restart node
5. Brief pause between each

### Option 2: If Nodes Are Stuck (Emergency Recovery)
```bash
# Use this if circuit breaker triggers repeatedly
bash scripts/emergency_recovery.sh
```

This will:
1. Stop all nodes
2. Backup all databases
3. Clear databases except on chosen seed
4. Restart seed node first
5. Restart other nodes (they resync from seed)

## Monitoring After Deployment

### Check Overall Status
```bash
bash scripts/diagnose_fork_state.sh
```

Shows:
- Current heights of all nodes
- Fork activity (last 5 minutes)
- Deep fork detections
- Successful reorganizations
- Active fork resolution attempts

### Watch Logs in Real-Time
```bash
# On individual node
ssh LW-Michigan2 'journalctl -u timed -f | grep -E "(REORG|FORK|DEEP)"'
```

### What to Look For

**✅ Good Signs**
- Nodes reach same height
- Forks resolve within 1-5 attempts
- Clear "REORGANIZATION SUCCESSFUL" messages
- No "DEEP FORK DETECTED" messages

**⚠️ Warning Signs**
- "DEEP FORK DETECTED" appears
- Fork resolution attempts > 10
- Same blocks requested repeatedly

**🚨 Emergency Signs**
- Circuit breaker triggers repeatedly (fork > 100 blocks)
- All nodes stuck at different heights
- No progress after 10+ minutes

## Files Created

1. **FORK_RESOLUTION_FIXES.md** - Complete technical documentation
2. **scripts/diagnose_fork_state.sh** - Diagnostic tool
3. **scripts/deploy_fork_fixes.sh** - Automated deployment
4. **scripts/emergency_recovery.sh** - Emergency recovery

## Typical Deployment Timeline

```
T+0:00  Deploy fixes to all nodes (5-10 minutes)
T+0:10  Nodes restart and begin syncing
T+0:15  Check status with diagnose_fork_state.sh
T+0:20  Nodes should be synced (if shallow forks)
```

If deep forks detected:
```
T+0:00  Run diagnose_fork_state.sh
        See "DEEP FORK DETECTED" messages
T+0:05  Run emergency_recovery.sh
        Choose seed node (e.g., LW-Arizona)
T+0:10  Seed restarts, others cleared
T+0:15  Other nodes restart
T+0:30  Check sync progress
T+1:00  All nodes should be synced
```

## Key Commands

```bash
# Deploy fixes
cd C:\Users\wmcor\projects\timecoin
bash scripts/deploy_fork_fixes.sh

# Check status
bash scripts/diagnose_fork_state.sh

# Emergency recovery (if needed)
bash scripts/emergency_recovery.sh

# Watch specific node
ssh LW-Michigan2 'journalctl -u timed -f'

# Check heights of all nodes
for s in LW-Michigan2 LW-Arizona LW-London reitools NewYork; do
  echo -n "$s: "
  ssh $s "curl -s localhost:24101/blockchain/info | jq -r .height"
done
```

## Decision Tree

```
Are nodes stuck in fork loops? (diagnose_fork_state.sh)
│
├─ NO → Deploy fixes normally (deploy_fork_fixes.sh)
│       Monitor for 15 minutes
│       Should resolve naturally
│
└─ YES → Are deep forks detected?
         │
         ├─ NO → Deploy fixes, may resolve on its own
         │       If not, escalate to emergency recovery
         │
         └─ YES → Emergency recovery required
                  (emergency_recovery.sh)
                  Fork too deep for normal resolution
```

## Support

If problems persist after fixes:
1. Check FORK_RESOLUTION_FIXES.md for detailed troubleshooting
2. Verify all nodes running same binary version
3. Check network connectivity between nodes
4. Verify genesis blocks match across all nodes
5. Consider manual blockchain state inspection

## Summary

**Before**: Nodes stuck in infinite loops requesting same blocks forever
**After**: Circuit breaker stops after 100 blocks or 50 attempts, with clear logging

The fix adds critical safety mechanisms that were missing. Nodes will now gracefully handle deep forks and make it obvious when manual intervention is needed.
