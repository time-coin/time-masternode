# Manual Testing Guide: Checkpoint & UTXO Rollback

This guide provides procedures for manually testing the checkpoint and UTXO rollback features on a live testnet.

## Prerequisites

- 2-4 testnet nodes running
- RPC access to nodes
- curl or similar HTTP client
- Ability to monitor node logs

## Test 1: Checkpoint Validation

### Objective
Verify that checkpoint validation prevents adding blocks with invalid hashes.

### Procedure

1. **Check Genesis Checkpoint**
   ```bash
   curl -X POST http://node:8332 \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","method":"getblockhash","params":[0],"id":1}'
   ```
   
2. **Monitor Logs for Checkpoint Validation**
   ```bash
   tail -f /path/to/node/data/node.log | grep -i checkpoint
   ```
   
3. **Expected Output**
   - Genesis block hash should match testnet genesis
   - No checkpoint validation errors in logs
   - `validate_checkpoint` function being called

### Success Criteria
- ✓ Genesis checkpoint exists
- ✓ No checkpoint validation failures
- ✓ Checkpoint infrastructure operational

---

## Test 2: Rollback Prevention Past Checkpoints

### Objective
Verify that rollbacks cannot go past checkpoint boundaries.

### Procedure

1. **Get Current Height**
   ```bash
   curl -X POST http://node:8332 \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","method":"getblockchaininfo","id":1}'
   ```

2. **Monitor for Rollback Attempts**
   ```bash
   tail -f node.log | grep -i "rollback\|checkpoint"
   ```

3. **Look for Protection Messages**
   - `Cannot rollback past checkpoint at height X`
   - `Checkpoint protection active`

### Success Criteria
- ✓ Rollback prevention logs present
- ✓ No rollbacks past genesis
- ✓ Checkpoint boundaries respected

---

## Test 3: UTXO Rollback During Reorganization

### Objective
Verify UTXO state is rolled back during chain reorganization.

### Prerequisites
- At least 2 connected nodes
- Active block production

### Procedure

1. **Monitor UTXO Activity**
   ```bash
   tail -f node.log | grep -i "utxo\|rollback"
   ```

2. **Check for Reorg Events**
   ```bash
   tail -f node.log | grep -i "reorg\|reorganiz"
   ```

3. **Look for UTXO Rollback**
   - `Rolled back X UTXO changes`
   - `Removed outputs from rolled-back blocks`
   - UTXO count changes

### Expected Behavior
During a reorganization:
1. Old blocks are removed
2. UTXOs from those blocks are removed
3. New blocks are added
4. New UTXOs are created
5. UTXO count reflects changes

### Success Criteria
- ✓ UTXO rollback logs present during reorg
- ✓ UTXO count adjusted correctly
- ✓ No UTXO inconsistency errors

---

## Test 4: Reorganization Metrics Tracking

### Objective
Verify that reorganization events are tracked with metrics.

### Procedure

1. **Trigger a Reorganization** (if possible)
   - Temporarily partition network
   - Let both sides produce blocks
   - Reconnect and observe reorg

2. **Check Reorg Metrics in Logs**
   ```bash
   grep "REORG" node.log
   ```

3. **Look for Metrics**
   - `REORG INITIATED`
   - `REORG COMPLETE`
   - Duration in milliseconds
   - Blocks removed/added count
   - Transactions needing replay
   - Common ancestor height

### Sample Log Entry
```
⚠️  REORG INITIATED: rollback 100 -> 95, then apply 6 blocks
✅ REORG COMPLETE: new height 101, took 245ms, 3 txs need replay
```

### Success Criteria
- ✓ Reorg events logged with metrics
- ✓ Timing information recorded
- ✓ Transaction counts accurate
- ✓ Block counts correct

---

## Test 5: Transaction Replay Identification

### Objective
Verify transactions are identified for mempool replay after reorg.

### Procedure

1. **Create Transactions** (before reorg)
   - Send transactions between addresses
   - Wait for inclusion in blocks

2. **Trigger Reorganization**
   - See Test 4 procedure

3. **Check Logs for Replay Info**
   ```bash
   grep "need.*replay\|mempool.*replay" node.log
   ```

### Expected Output
```
🔄 3 transactions need mempool replay after reorg
```

### Success Criteria
- ✓ Transaction replay count logged
- ✓ Correct number of transactions identified
- ✓ No transactions lost

---

## Test 6: Chain Work Comparison

### Objective
Verify chain work is being calculated and compared correctly.

### Procedure

1. **Check Initial Chain Work**
   ```bash
   curl -X POST http://node:8332 \
     -H "Content-Type: application/json" \
     -d '{"jsonrpc":"2.0","method":"getblockchaininfo","id":1}' \
     | jq '.result.chainwork'
   ```

2. **Monitor Chain Work in Logs**
   ```bash
   tail -f node.log | grep -i "chain_work\|cumulative_work"
   ```

3. **Verify Work Increases with Blocks**
   - Check work at height 100
   - Check work at height 200
   - Work should increase

### Success Criteria
- ✓ Chain work tracked
- ✓ Work increases with blocks
- ✓ Work comparison used in fork resolution

---

## Test 7: Reorg History API

### Objective
Verify reorg history is stored and accessible.

### Procedure

1. **After a Reorg, Check History**
   - Look for reorg_history in logs
   - Verify metrics are stored

2. **Expected Storage**
   - Last 100 reorg events kept
   - Each event has full metrics
   - Timestamp, heights, duration

### Success Criteria
- ✓ Reorg history maintained
- ✓ 100-event limit enforced
- ✓ Metrics complete and accurate

---

## Test 8: Max Reorg Depth Protection

### Objective
Verify max reorg depth (1000 blocks) is enforced.

### Procedure

1. **Check Configuration**
   ```rust
   const MAX_REORG_DEPTH: u64 = 1_000;
   const ALERT_REORG_DEPTH: u64 = 100;
   ```

2. **Monitor for Depth Warnings**
   ```bash
   grep "LARGE REORG\|MAX_REORG_DEPTH" node.log
   ```

3. **Look for Protection**
   - Large reorg warnings (>100 blocks)
   - Rejection of too-deep reorgs (>1000 blocks)

### Success Criteria
- ✓ Reorg depth limits enforced
- ✓ Warnings at 100+ blocks
- ✓ Rejection at 1000+ blocks

---

## Test Scenarios

### Scenario A: Network Partition

**Setup:**
1. Start 4 nodes: A, B, C, D
2. Partition into [A, B] and [C, D]
3. Let each produce blocks for 30 minutes
4. Reconnect

**Expected:**
- Minority partition reorganizes to majority chain
- UTXO states reconciled
- Transactions replayed to mempool
- All nodes reach consensus

**Validation:**
```bash
# Check heights match
for port in 8332 8333 8334 8335; do
  curl -s -X POST http://localhost:$port \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"getblockchaininfo","id":1}' \
    | jq '.result.height'
done

# Check tip hashes match
for port in 8332 8333 8334 8335; do
  curl -s -X POST http://localhost:$port \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"getbestblockhash","id":1}' \
    | jq -r '.result'
done
```

### Scenario B: Rolling Restart

**Setup:**
1. Start 4 nodes
2. Let them sync to height 1000+
3. Stop all nodes
4. Start them one by one

**Expected:**
- Each node loads correct chain tip
- Chain work calculated correctly
- No spurious reorganizations
- Nodes sync quickly

**Validation:**
- Check logs for clean startup
- Verify no rollbacks on restart
- Confirm work matches across nodes

### Scenario C: Checkpoint Enforcement

**Setup:**
1. Manually add checkpoint at height 1000
2. Try to sync from genesis
3. Verify checkpoint validated

**Expected:**
- Block at height 1000 validated against checkpoint
- Sync fails if hash doesn't match
- Sync succeeds if hash matches

---

## Monitoring Dashboard

### Key Metrics to Watch

1. **Chain Height**
   - All nodes should converge
   - Should increase steadily

2. **Chain Work**
   - Should increase with blocks
   - Should match across nodes

3. **Reorg Count**
   - Track number of reorgs
   - Should be low in healthy network

4. **UTXO Set Size**
   - Should grow with transactions
   - Should be consistent across nodes

5. **Peer Count**
   - Minimum 2 peers recommended
   - More peers = more reliable

### Log Patterns to Monitor

**Good:**
```
✅ Checkpoint validated at height X
✅ Reorganization complete: new height Y
🔄 Rolled back N UTXO changes
```

**Warning:**
```
⚠️  LARGE REORG: Rolling back X blocks
⚠️  No connected peers
⚠️  UTXO restoration incomplete
```

**Error:**
```
❌ Checkpoint validation failed
❌ Rollback too deep
❌ Cannot rollback past checkpoint
```

---

## Troubleshooting

### Issue: Nodes Won't Reorganize

**Check:**
- Are nodes connected?
- Is chain work being compared?
- Are blocks valid?

**Solutions:**
- Verify peer connections
- Check logs for validation errors
- Ensure time sync is correct

### Issue: UTXO Inconsistencies

**Check:**
- Were UTXOs rolled back?
- Did reorg complete successfully?
- Are blocks being processed?

**Solutions:**
- Review rollback logs
- Check for partial reorg
- May need to resync from genesis

### Issue: Missing Transactions

**Check:**
- Were transactions replayed?
- Are they in mempool?
- Did they have proper fees?

**Solutions:**
- Check transaction replay logs
- Verify mempool contents
- May need to resubmit

---

## Reporting Issues

When reporting issues, include:

1. **Node Logs**
   - Full logs around the issue
   - Especially reorg/checkpoint/UTXO lines

2. **Network State**
   - All node heights
   - Peer connections
   - Time since last block

3. **Reproduction Steps**
   - Exact commands run
   - Node configuration
   - Network topology

4. **Expected vs Actual**
   - What should have happened
   - What actually happened
   - Any error messages

---

## Success Checklist

After manual testing, verify:

- [ ] Checkpoint system prevents invalid blocks
- [ ] Rollbacks respect checkpoint boundaries
- [ ] UTXO state rolled back during reorgs
- [ ] Reorganization metrics tracked correctly
- [ ] Transaction replay identified
- [ ] Chain work compared for fork resolution
- [ ] Reorg history maintained
- [ ] Max reorg depth enforced
- [ ] No UTXO inconsistencies
- [ ] No transaction loss
- [ ] All nodes reach consensus
- [ ] Logs show expected behavior

---

## Conclusion

Manual testing provides comprehensive validation of the checkpoint and UTXO rollback system in real-world conditions. Use this guide to systematically verify each feature and scenario.

For automated testing, see `test_checkpoint_rollback.ps1` or `.sh` in this directory.
