# Fork Resolution - Quick Reference

**Last Updated:** 2025-12-12  
**Status:** ✅ Production Ready

---

## How It Works

### Fork Detection

```
Node receives block → previous_hash doesn't match → Fork detected!
```

### Resolution Steps

1. **Detect Fork** - Block doesn't build on our chain
2. **Check Consensus** - Query masternodes (need 2/3+ agreement)
3. **Find Common Ancestor** - Last block both chains agree on
4. **Decide Action** - Rollback if needed, or skip if already at ancestor
5. **Accept New Blocks** - Sync from common ancestor forward

---

## Three Scenarios

### Scenario 1: Already at Fork Point (COMMON)

```
Current height:     1703
Fork detected at:   1704
Common ancestor:    1703
Action:             ✅ No rollback needed
Result:             Accept blocks 1704+ directly
```

**Log Pattern:**
```
✅ Already at common ancestor (height 1703). No rollback needed.
🔄 Ready to accept blocks from height 1704 onward
```

### Scenario 2: Need Rollback (NORMAL)

```
Current height:     1710
Fork detected at:   1704  
Common ancestor:    1703
Action:             🔄 Rollback 7 blocks (1710 → 1703)
Result:             Then accept blocks 1704-1708
```

**Log Pattern:**
```
🔄 Rolling back from 1710 to 1703...
✅ Rollback complete. Ready to sync from height 1704
```

### Scenario 3: Behind Ancestor (RARE)

```
Current height:     1700
Common ancestor:    1703
Action:             ⚠️  Log warning
Result:             Investigate - shouldn't happen
```

**Log Pattern:**
```
⚠️  Current height 1700 is below common ancestor 1703. This shouldn't happen.
```

---

## Consensus Check

### With Sufficient Masternodes (3+)

```
✅ Peer's chain has 2/3+ consensus - proceeding with reorg
```
**Action:** Proceed with fork resolution

```
❌ Our chain has 2/3+ consensus - rejecting peer's fork
```
**Action:** Stay on our chain, reject peer's blocks

```
⚠️  No chain has 2/3+ consensus - network may be split
```
**Action:** Stay on current chain until consensus emerges

### Without Sufficient Masternodes (<3)

```
⚠️  Not enough peers to verify consensus (need 3+)
⚠️  Proceeding with reorg based on depth limits only
```
**Action:** Use depth limits as fallback (max 100 blocks)

---

## Safety Limits

| Limit | Value | Action |
|-------|-------|--------|
| Max reorg depth | 100 blocks | Hard limit - reject deeper reorgs |
| Deep reorg warning | 10 blocks | Warn but proceed |
| Min masternodes | 3 | For BFT consensus check |

---

## Common Issues

### Issue 1: "Cannot rollback: target >= current"

**Cause:** Bug in old code (now fixed)  
**Solution:** Update to latest version  
**Status:** ✅ Fixed in commit 0b4ede9

### Issue 2: "Insufficient consensus"

**Cause:** Less than 3 masternodes online  
**Solution:** Wait for more masternodes to come online  
**Mitigation:** System uses depth limits as fallback

### Issue 3: "Successfully added 0 blocks"

**Cause:** Rollback failed, can't add new blocks  
**Solution:** Check logs for rollback error, update to latest version  
**Status:** ✅ Should not occur with bugfix

---

## Monitoring

### Healthy Fork Resolution

```
🍴 Fork detected at height 1704 (current height: 1703)
🔍 Querying peers for consensus on fork at height 1704...
📊 Fork consensus check: 5 masternodes, need 4 for 2/3 majority
✅ Peer's chain has 2/3+ consensus - proceeding with reorg
📍 Common ancestor found at height 1703
✅ Already at common ancestor (height 1703). No rollback needed.
🔄 Ready to accept blocks from height 1704 onward
```

### Problem Fork Resolution

```
🍴 Fork detected at height 1704
❌ Our chain has 2/3+ consensus - rejecting peer's fork
```
**Action:** This is normal - peer is on wrong chain

```
⚠️  No chain has 2/3+ consensus - network may be split
```
**Action:** Network split - manual investigation may be needed

---

## Key Metrics

Monitor these in production:

- **Fork detection rate** - How often forks occur
- **Reorg success rate** - % of successful reorganizations  
- **Consensus query failures** - Failed consensus checks
- **Average reorg depth** - How deep rollbacks go
- **Nodes stuck** - Nodes not advancing (should be 0)

---

## Recovery Actions

### If Nodes Are Stuck

1. **Check logs** for rollback errors
2. **Verify version** - should have commit 0b4ede9 or later
3. **Check peer connections** - need 3+ for consensus
4. **Monitor height** - should be advancing
5. **Restart if needed** - fresh sync from peers

### If Network Split Detected

1. **Identify chains** - which nodes on which chain
2. **Check consensus** - which chain has majority
3. **Wait for convergence** - system will auto-resolve
4. **Manual intervention** - only if split persists >1 hour

---

## Code Reference

### Main Files

- `src/blockchain.rs` - Fork resolution logic
  - `handle_fork_and_reorg()` - Main orchestrator
  - `query_fork_consensus()` - BFT consensus check
  - `find_common_ancestor()` - Find fork point
  - `rollback_to_height()` - Rollback blocks

### Related Documentation

- `BUGFIX_FORK_ROLLBACK.md` - Detailed bugfix documentation
- `analysis/FORK_RESOLUTION.md` - Design document (if exists)
- `analysis/FORK_RESOLUTION_STATUS.md` - Implementation status (if exists)

---

## Quick Troubleshooting

| Symptom | Cause | Solution |
|---------|-------|----------|
| Nodes stuck at same height | Rollback bug | Update to ffef964+ |
| "Cannot rollback" errors | Old version | Update to 0b4ede9+ |
| No consensus warnings | <3 masternodes | Wait for more nodes |
| Deep reorg warnings | Major network split | Monitor, may be normal |
| "Successfully added 0 blocks" | Rollback failed | Check logs, update version |

---

## Version History

| Version | Date | Change |
|---------|------|--------|
| 0b4ede9 | 2025-12-12 | **Fix:** Skip rollback when at common ancestor |
| b28d3f1 | 2025-12-12 | Remove analysis folder from git |
| 5ac1732 | 2025-12-12 | Implement BFT consensus catchup mode |
| 90fac7b | 2025-12-12 | Update fork resolution status |
| 0f2234b | 2025-12-12 | Implement consensus verification |

---

**Current Status:** ✅ Production Ready  
**Known Issues:** None  
**Next Enhancements:** Real-time peer voting, checkpoint system
