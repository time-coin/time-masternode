# Transaction Flow Testing - Quick Start Guide

**Version:** 1.1.0  
**Date:** January 28, 2026

---

## Overview

This guide provides quick instructions for testing the complete transaction flow after the v1.1.0 bug fixes.

**What was fixed:**
- ✅ Bug #1: Finalized pool premature clearing
- ✅ Bug #2: Broadcast callback not wired
- ✅ Bug #3: Fee timing off by one block
- ✅ Bug #4: Transaction finalization not propagating to other nodes ⭐ **CRITICAL**

---

## Quick Test (5 minutes)

### Option 1: Automated Test Suite

```bash
# Run 10 critical tests automatically
bash scripts/test_critical_flow.sh
```

**Expected Output:**
```
✅ All tests passed!
Passed:  10
Failed:  0
Skipped: 0
```

### Option 2: Manual Quick Test

```bash
# 1. Send transaction
TXID=$(time-cli sendtoaddress <address> 1.0)
echo "TXID: $TXID"

# 2. Watch for finalization (should be <2 seconds)
journalctl -u timed -f | grep "$TXID"

# 3. Look for these logs:
#    - "📡 Broadcast TransactionFinalized" (Bug #4 fix!)
#    - "✅ Transaction finalized"
#    - "📦 Moved to finalized pool"

# 4. Wait for block inclusion (<60 seconds)
watch -n 5 "time-cli gettransaction $TXID"

# 5. Verify transaction confirmed
time-cli gettransaction $TXID | jq '.blockheight'
```

---

## Multi-Node Test (10 minutes) ⭐ CRITICAL

This test verifies Bug #4 fix - that finalization propagates to ALL nodes.

### Setup

Edit `scripts/test_finalization_propagation.sh` and add your nodes:

```bash
NODES=(
    "root@LW-Michigan"
    "root@node2-ip-or-hostname"
    "root@node3-ip-or-hostname"
    "root@node4-ip-or-hostname"
    "root@node5-ip-or-hostname"
    "root@node6-ip-or-hostname"
)
```

### Run Test

```bash
bash scripts/test_finalization_propagation.sh
```

### Expected Output

```
✅ ALL NODES VERIFIED ✓

Transaction finalization propagated correctly to all 6 node(s)
Bug #4 fix is working!

What this means:
  ✓ TransactionFinalized message broadcast from submitter
  ✓ All nodes received the message
  ✓ All nodes finalized the transaction locally
  ✓ All nodes have TX in finalized pool
  ✓ ANY node can now include this TX in blocks
```

### What to Check

**On submitting node:**
```bash
journalctl -u timed --since "1 minute ago" | grep "$TXID"
```

Look for:
- `📡 Broadcast TransactionFinalized for [txid]`
- `✅ Transaction finalized`
- `📦 Moved to finalized pool`

**On other nodes:**
```bash
ssh root@[other-node] "journalctl -u timed --since '1 minute ago' | grep '$TXID'"
```

Look for:
- `✅ Transaction [txid] finalized (from [submitter-ip])`
- `📦 Moved TX [txid] to finalized pool on this node`

**Before v1.1.0:** Only submitter had TX in finalized pool ❌  
**After v1.1.0:** ALL nodes have TX in finalized pool ✅

---

## Comprehensive Testing (2+ hours)

For complete test coverage, see:

**Test Plan:** `tests/transaction_flow_test_plan.md`
- 63 test cases
- All 9 transaction flow phases
- Bug verification tests
- Edge cases, performance, stress tests

**Flow Documentation:** `analysis/transaction_flow_complete.md`
- Complete technical documentation
- Code references for every step
- State transition diagrams
- UTXO lifecycle

---

## Deployment Checklist

### Before Testing

- [ ] All nodes on v1.1.0: `time-cli getinfo | jq '.version'`
- [ ] All nodes synced: `time-cli getblockcount` (same on all)
- [ ] Network connectivity: `time-cli getpeerinfo | jq 'length'` (≥5 peers)
- [ ] Sufficient balance: `time-cli getbalance` (>10 TIME)

### After Testing

- [ ] All tests passed (0 failures)
- [ ] No error logs: `journalctl -u timed --since "1 hour ago" | grep -i error`
- [ ] Blocks producing normally: `watch -n 10 time-cli getblockcount`
- [ ] Transactions confirming: Check multiple TXs end-to-end

---

## Expected Behavior (v1.1.0)

### Transaction Lifecycle

```
User submits TX
    ↓
Broadcast to network (all nodes receive)
    ↓
TimeVote consensus (validators vote)
    ↓
Finalization (51% threshold reached)
    ↓
Broadcast TransactionFinalized ⭐ NEW!
    ↓
All nodes finalize TX locally ⭐ NEW!
    ↓
Block producer includes TX (any node can produce)
    ↓
Block consensus (51% accept)
    ↓
Block storage (TX confirmed)
```

### Timing Targets

- **Finalization:** <2 seconds
- **Block inclusion:** <60 seconds (next block)
- **Total (submit → confirmed):** <62 seconds

### Log Patterns

**Successful Transaction:**
```
INFO 🔍 Validating transaction abc123...
INFO ✅ Transaction abc123... validation passed
INFO 📡 Broadcasting transaction to network
INFO 📡 Broadcasting TimeVoteRequest for TX
INFO ✅ Transaction finalized (51% threshold)
INFO 📡 Broadcast TransactionFinalized for abc123...
INFO 🔍 Block 1234: Including 1 finalized transaction(s)
INFO 💸 Block 1234: included 10000000 satoshis in fees
INFO 📦 Block 1234 produced: 3 txs
```

**On Other Nodes:**
```
INFO 📥 Received new transaction abc123...
INFO ✅ Transaction abc123... finalized (from 69.167.168.176)
INFO 📦 Moved TX abc123... to finalized pool on this node
```

---

## Troubleshooting

### Transaction Not Finalizing

**Symptom:** TX sent but never finalizes

**Check:**
```bash
# 1. Check vote requests sent
journalctl -u timed | grep "Broadcasting TimeVoteRequest"
# If missing: Bug #2 still present

# 2. Check validator count
time-cli getmasternodes | jq 'length'
# Need ≥4 for 51% threshold

# 3. Check vote responses
journalctl -u timed | grep "Received TimeVote"
# Should see votes from validators
```

### Finalization Not Propagating

**Symptom:** TX finalized on submitter but not other nodes

**Check:**
```bash
# 1. Check broadcast sent
journalctl -u timed | grep "Broadcast TransactionFinalized"
# If missing: Bug #4 part A still present

# 2. Check other nodes received
ssh root@[node] "journalctl -u timed | grep 'Received TransactionFinalized'"
# If missing: Network issue

# 3. Check handler finalized locally
ssh root@[node] "journalctl -u timed | grep 'Moved TX.*to finalized pool'"
# If missing: Bug #4 part B still present
```

### Transaction Not in Block

**Symptom:** TX finalized but never included in block

**Check:**
```bash
# 1. Verify finalized pool on block producer
# When block is produced, check which node produced it
LATEST=$(time-cli getblockcount)
LEADER=$(time-cli getblock $LATEST | jq -r '.header.leader')

# SSH to that node and check its finalized pool
ssh root@[leader-node] "time-cli getmempoolinfo | jq '.finalized_count'"
# Should be >0 if propagation working

# 2. Check for block rejection
journalctl -u timed | grep "incorrect block_reward"
# If present: Bug #3 still present
```

---

## Success Criteria

### ✅ Pass Conditions

- [ ] Transactions finalize in <2 seconds
- [ ] TransactionFinalized broadcast sent
- [ ] All nodes receive and finalize TX
- [ ] All nodes have TX in finalized pool
- [ ] Block producer includes TX (regardless of which node)
- [ ] Block accepted by network (no reward errors)
- [ ] TX confirmed in blockchain
- [ ] UTXOs updated correctly

### ❌ Fail Conditions

- Transaction never finalizes
- "No broadcast callback available" errors
- Finalization doesn't propagate to other nodes
- Only submitter can include TX in blocks
- "incorrect block_reward" errors
- Finalized pool cleared prematurely
- Transaction never confirmed

---

## Support

If issues persist:

1. **Collect logs:**
   ```bash
   journalctl -u timed --since "1 hour ago" > timed.log
   ```

2. **Collect test output:**
   ```bash
   bash scripts/test_critical_flow.sh > test_results.txt 2>&1
   ```

3. **Check version:**
   ```bash
   time-cli getinfo | jq '{version, chainwork, connections: .peers}'
   ```

4. **Check network:**
   ```bash
   time-cli getpeerinfo | jq '.[] | {addr, version, height}'
   ```

5. **Report with:**
   - Node version
   - Test results
   - Log excerpts
   - Transaction IDs that failed
   - Network topology (how many nodes)

---

## Next Steps

After successful testing:

1. **Monitor for 24 hours:**
   ```bash
   watch -n 60 'time-cli getblockcount && time-cli getmempoolinfo'
   ```

2. **Test high volume:**
   - Send 10-20 transactions concurrently
   - Verify all finalize and confirm

3. **Multi-node validation:**
   - Send TXs from different nodes
   - Verify all nodes can submit and confirm

4. **Performance benchmarking:**
   - Measure finalization latency
   - Measure block inclusion time
   - Track memory usage

---

**End of Quick Start Guide**
