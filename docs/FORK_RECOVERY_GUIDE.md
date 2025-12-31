# Fork Recovery Guide - TIME Coin Testnet
# Date: December 31, 2024
# Issue: Network forked at heights 4388-4402

## Current Network Status

From latest logs (15:30 UTC):

| Node | Height | Status | Fork Point |
|------|--------|--------|------------|
| LW-Michigan | 4391 | Behind, cannot send blocks 4392+ | - |
| LW-Michigan2 | 4399 | Fork detected 4388-4399 | 4388 |
| LW-London | 4401 | Failed fork resolution | 4396 |
| LW-Arizona | 4402 | Ahead, forks at 4397+ | 4397 |

**Common Ancestor:** Height 4387 (last matching blocks across all nodes)

## Root Cause Analysis

1. **Solo catchup block production** (now disabled) created divergent chains
2. **Block request failures** prevented sync/resolution
3. **Multiple competing chains** at same heights with different hashes
4. **Previous hash corruption** in some transmitted blocks showing as `0000000000000000`

## Recovery Options

### Option 1: Rollback to Common Ancestor (RECOMMENDED)

**Steps:**
1. Stop all nodes
   ```bash
   sudo systemctl stop timed
   ```

2. Backup current state
   ```bash
   sudo cp -r /var/lib/timed /var/lib/timed.backup.$(date +%Y%m%d_%H%M%S)
   ```

3. Determine exact common ancestor height by comparing block hashes
   - Likely height 4387 based on logs
   - Verify each node has matching hash at 4387

4. Rollback each node's blockchain database
   ```bash
   # This requires database access to delete blocks > 4387
   # Or restore from backup taken at height 4387
   ```

5. Restart nodes simultaneously
   ```bash
   sudo systemctl start timed
   ```

6. Monitor consensus formation

**Pros:** Preserves maximum history, surgical fix
**Cons:** Requires database manipulation, coordination across all nodes

### Option 2: Nuclear Option - Fresh Genesis

**Steps:**
1. Stop all nodes
2. Backup all data
3. Clear blockchain data (keep genesis file)
   ```bash
   sudo rm -rf /var/lib/timed/blocks.db*
   sudo rm -rf /var/lib/timed/state.db*
   ```
4. Restart nodes - they will sync from genesis
5. Monitor closely

**Pros:** Clean slate, guaranteed resolution
**Cons:** Loses all transaction/block history

### Option 3: Designate Leader Chain

**Steps:**
1. Identify which node has "best" chain (longest valid chain from genesis)
2. Stop all other nodes
3. Clear their data
4. Restart them to sync from the designated leader
5. Once synced, restart leader node

**Pros:** Fast recovery
**Cons:** Requires manual chain validation

## Immediate Recommendations

Based on logs showing consistent fork detection and `previous_hash` corruption:

1. **STOP ALL NODES immediately** to prevent further divergence
2. **Backup all data** before any recovery attempt
3. **Choose Option 1 (Rollback to 4387)** as the least destructive
4. **Coordinate restart** - all nodes must start together
5. **Monitor for 100 blocks** to ensure consensus holds

## Prevention Measures (Already Implemented)

Latest code (commit 778e57e) includes:

- ✅ Solo catchup block production disabled
- ✅ Refuse block production when >50 blocks behind
- ✅ Exponential backoff for block requests (30s base, up to 50s)
- ✅ 5 retry attempts before giving up
- ✅ Extended fork detection window (30s, 5 attempts)
- ✅ Better error logging for diagnostics

## Post-Recovery Monitoring

After recovery, watch for:

1. **Fork warnings** - Should be zero after recovery
2. **Block sync timeouts** - Should decrease with new retry logic
3. **Height divergence** - All nodes should stay within 1-2 blocks
4. **"Ahead of consensus" warnings** - Should not occur

## Contact

If manual database manipulation is needed, the team should:
1. Access the RocksDB database directly
2. Delete blocks after fork point
3. Update height metadata
4. Verify integrity before restart

## Emergency Rollback Commands

```bash
# Emergency stop
sudo systemctl stop timed

# Check current height from logs
sudo journalctl -u timed -n 100 | grep "height.*Status"

# Backup before any changes
sudo tar -czf /root/timed-backup-$(date +%Y%m%d_%H%M%S).tar.gz /var/lib/timed

# After fixes, restart
sudo systemctl start timed

# Monitor live
sudo journalctl -u timed -f
```
