# Comprehensive Transaction Flow Test Plan

**Version:** 1.1.0  
**Date:** January 28, 2026  
**Purpose:** Verify all functions in transaction flow from submission to blockchain storage

---

## Table of Contents

1. [Test Environment Setup](#test-environment-setup)
2. [Phase 1 Tests: Transaction Submission](#phase-1-tests-transaction-submission)
3. [Phase 2 Tests: Network Broadcast](#phase-2-tests-network-broadcast)
4. [Phase 3 Tests: TimeVote Consensus](#phase-3-tests-timevote-consensus)
5. [Phase 4 Tests: Vote Collection](#phase-4-tests-vote-collection)
6. [Phase 5 Tests: Finalization](#phase-5-tests-finalization)
7. [Phase 6 Tests: Finalization Propagation](#phase-6-tests-finalization-propagation)
8. [Phase 7 Tests: Block Production](#phase-7-tests-block-production)
9. [Phase 8 Tests: Block Consensus](#phase-8-tests-block-consensus)
10. [Phase 9 Tests: Block Storage](#phase-9-tests-block-storage)
11. [Bug Verification Tests](#bug-verification-tests)
12. [Edge Case Tests](#edge-case-tests)
13. [Performance Tests](#performance-tests)
14. [Stress Tests](#stress-tests)
15. [Test Results Template](#test-results-template)

---

## Test Environment Setup

### Prerequisites

**Network Topology:**
- Minimum 6 masternodes required for full testing
- All nodes on same version (1.1.0)
- Network connectivity between all nodes verified

**Test Node Setup:**
```bash
# On each test node
cd ~/timecoin
git pull
git checkout main
cargo build --release
sudo systemctl restart timed
```

**Verify Node Status:**
```bash
# Check version
time-cli getinfo | jq '.version'
# Expected: "1.1.0"

# Check sync status
time-cli getblockcount
# Should be same on all nodes

# Check masternode count
time-cli getmasternodes | jq 'length'
# Should show 6+ masternodes

# Check wallet balance
time-cli getbalance
# Should have sufficient funds for testing (>10 TIME)
```

**Log Monitoring Setup:**
```bash
# Terminal 1: Michigan node logs
ssh root@LW-Michigan
journalctl -u timed -f | grep -E "Transaction|finalized|Block"

# Terminal 2: Other node logs
ssh root@[other-node]
journalctl -u timed -f | grep -E "Transaction|finalized|Block"
```

**Test Data:**
```bash
# Get test addresses
export TEST_ADDR_1=$(time-cli getmasternodes | jq -r '.[0].address')
export TEST_ADDR_2=$(time-cli getmasternodes | jq -r '.[1].address')

# Note block height at start
export START_HEIGHT=$(time-cli getblockcount)
echo "Starting at block: $START_HEIGHT"
```

---

## Phase 1 Tests: Transaction Submission

### Test 1.1: Basic Transaction Creation

**Objective:** Verify RPC handler creates valid transaction

**Steps:**
```bash
# Send 1 TIME to test address
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0 2>&1)
echo "TXID: $TXID"
```

**Success Criteria:**
- [ ] Command returns 64-character hex TXID
- [ ] No error messages
- [ ] TXID format: `^[0-9a-f]{64}$`

**Logs to Check:**
```bash
journalctl -u timed --since "1 minute ago" | grep "Validating transaction"
```

**Expected Log:**
```
INFO 🔍 Validating transaction abc123...
INFO ✅ Transaction abc123... validation passed
```

---

### Test 1.2: UTXO Selection

**Objective:** Verify correct UTXO selection and fee calculation

**Steps:**
```bash
# Get wallet UTXOs before
time-cli listunspent > utxos_before.json

# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Calculate expected fee (0.1% of inputs)
# Should be at least 1,000 satoshis
```

**Success Criteria:**
- [ ] Sufficient UTXOs selected to cover amount + fee
- [ ] No collateral UTXOs used (masternode stakes protected)
- [ ] Only `Unspent` state UTXOs selected
- [ ] Change output created if necessary
- [ ] Fee ≥ 0.001 TIME (1,000 satoshis)

**Verification Query:**
```bash
# Check transaction structure
time-cli getrawtransaction "$TXID" | jq '{
  inputs: (.inputs | length),
  outputs: (.outputs | length),
  input_sum: .input_sum,
  output_sum: .output_sum,
  fee: (.input_sum - .output_sum)
}'
```

---

### Test 1.3: Transaction Validation

**Objective:** Verify lock_and_validate_transaction works

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Check UTXO states immediately
time-cli listunspent | jq '.[] | select(.state == "Locked")'
```

**Success Criteria:**
- [ ] All input UTXOs locked during validation
- [ ] Lock contains correct TXID
- [ ] Validation checks: existence, state, balance
- [ ] On failure: UTXOs unlocked

**Logs to Check:**
```bash
journalctl -u timed --since "1 minute ago" | grep -E "Locked UTXO|validation"
```

---

### Test 1.4: Insufficient Balance

**Objective:** Verify rejection when balance too low

**Steps:**
```bash
# Get current balance
BALANCE=$(time-cli getbalance | jq -r '.available')

# Try to send more than balance
time-cli sendtoaddress "$TEST_ADDR_1" $(echo "$BALANCE + 1" | bc) 2>&1
```

**Success Criteria:**
- [ ] Error: "Insufficient funds"
- [ ] RPC error code: -6
- [ ] No TXID returned
- [ ] No UTXOs locked

---

### Test 1.5: Invalid Address

**Objective:** Verify rejection of invalid addresses

**Steps:**
```bash
# Send to invalid address
time-cli sendtoaddress "INVALID_ADDRESS_123" 1.0 2>&1
```

**Success Criteria:**
- [ ] Error message about invalid address
- [ ] Transaction not created
- [ ] No network broadcast

---

### Test 1.6: Collateral Protection

**Objective:** Verify masternode collateral cannot be spent

**Setup:**
```bash
# Register a masternode if not already
time-cli registermasternode "$TEST_ADDR_1" "free"
```

**Steps:**
```bash
# Try to spend all funds (should skip collateral)
time-cli sendtoaddress "$TEST_ADDR_2" 9999999.0 2>&1
```

**Success Criteria:**
- [ ] Collateral UTXOs not selected as inputs
- [ ] Transaction uses only non-collateral UTXOs
- [ ] Masternode remains registered after TX

**Verification:**
```bash
time-cli getmasternodes | jq '.[] | select(.address == env.TEST_ADDR_1)'
# Should still show masternode as active
```

---

## Phase 2 Tests: Network Broadcast

### Test 2.1: TransactionBroadcast Message

**Objective:** Verify transaction broadcasts to all peers

**Steps:**
```bash
# On Node A (Michigan):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# On Node B (different node):
sleep 1
journalctl -u timed --since "10 seconds ago" | grep "Received new transaction"
```

**Success Criteria:**
- [ ] Node B logs "📥 Received new transaction [txid]"
- [ ] Node C, D, E, F also receive transaction
- [ ] Propagation time < 500ms
- [ ] All nodes add TX to pending pool

**Verification Commands:**
```bash
# On each node:
journalctl -u timed --since "1 minute ago" | grep "$TXID"
# Should show "Received new transaction" on all nodes
```

---

### Test 2.2: Duplicate Detection

**Objective:** Verify Bloom filter prevents duplicate processing

**Steps:**
```bash
# Send transaction from Node A
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for propagation
sleep 2

# Check logs on Node B for duplicate detection
journalctl -u timed --since "10 seconds ago" | grep -E "$TXID.*duplicate"
```

**Success Criteria:**
- [ ] First receipt: "📥 Received new transaction"
- [ ] Subsequent receipts: "🔁 Ignoring duplicate transaction"
- [ ] Transaction processed only once per node
- [ ] No redundant gossip after first propagation

---

### Test 2.3: Gossip Propagation

**Objective:** Verify transaction gossips to all peers

**Setup:**
```bash
# Get peer count on each node
for node in node1 node2 node3 node4 node5 node6; do
  ssh root@$node "time-cli getpeerinfo | jq 'length'"
done
```

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Track propagation
for i in {1..10}; do
  COUNT=0
  for node in node1 node2 node3 node4 node5 node6; do
    if ssh root@$node "journalctl -u timed | grep '$TXID' | grep 'Received'" &>/dev/null; then
      COUNT=$((COUNT+1))
    fi
  done
  echo "Time ${i}s: $COUNT/6 nodes have TX"
  sleep 1
  if [ $COUNT -eq 6 ]; then break; fi
done
```

**Success Criteria:**
- [ ] All 6 nodes receive transaction within 2 seconds
- [ ] Gossip follows exponential propagation pattern
- [ ] No network partitions detected

---

### Test 2.4: Invalid Transaction Rejection

**Objective:** Verify nodes reject invalid transactions

**Steps:**
```bash
# This requires crafting an invalid transaction manually
# For now, test by trying to double-spend
TXID1=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)
# Immediately try to spend same UTXOs
TXID2=$(time-cli sendtoaddress "$TEST_ADDR_2" 1.0)
```

**Success Criteria:**
- [ ] Second transaction rejected
- [ ] Logs show: "❌ Transaction rejected: UTXOs already locked"
- [ ] No gossip of invalid transaction
- [ ] IP not blacklisted (legitimate rejection)

---

## Phase 3 Tests: TimeVote Consensus

### Test 3.1: Add to Pending Pool

**Objective:** Verify transaction added to pending pool

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Check mempool immediately
time-cli getmempoolinfo
```

**Success Criteria:**
- [ ] `pending_count` incremented by 1
- [ ] TX visible in mempool
- [ ] Fee calculated correctly
- [ ] Timestamp recorded

**Verification:**
```bash
journalctl -u timed --since "1 minute ago" | grep "add.*pending pool"
```

---

### Test 3.2: UTXO State Transition to SpentPending

**Objective:** Verify UTXOs move to SpentPending state

**Steps:**
```bash
# Get UTXOs before
time-cli listunspent > before.json

# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Check UTXO states
time-cli listunspent | jq '.[] | select(.state == "SpentPending")'
```

**Success Criteria:**
- [ ] Input UTXOs in `SpentPending` state
- [ ] State includes: txid, votes=0, total_nodes=N, spent_at
- [ ] UTXOs no longer in "Unspent" state
- [ ] UTXOs not yet removed from set

**Logs to Check:**
```bash
journalctl -u timed --since "1 minute ago" | grep "SpentPending"
```

---

### Test 3.3: Auto-Finalize (Insufficient Validators)

**Objective:** Verify auto-finalize when <3 validators

**Setup:**
```bash
# Requires test network with only 1-2 masternodes
# Or set TIMECOIN_DEV_MODE=1
export TIMECOIN_DEV_MODE=1
sudo systemctl restart timed
```

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Should auto-finalize immediately
sleep 1
journalctl -u timed --since "10 seconds ago" | grep "auto-finalized"
```

**Success Criteria:**
- [ ] Log: "⚡ Auto-finalizing TX ... - insufficient validators"
- [ ] OR: "⚡ DEV MODE: Auto-finalizing TX"
- [ ] TX in finalized pool immediately
- [ ] No TimeVote rounds executed
- [ ] TransactionFinalized broadcast sent

---

### Test 3.4: Snowball State Initialization

**Objective:** Verify Snowball voting state created

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Check logs for Snowball initialization
journalctl -u timed --since "1 minute ago" | grep -E "Starting TimeVote|preference.*Accept"
```

**Success Criteria:**
- [ ] Log: "🔄 Starting TimeVote consensus for TX ... with N validators"
- [ ] Initial preference: Accept
- [ ] Initial confidence: 0
- [ ] VotingState created with validator list

---

### Test 3.5: TimeVoteRequest Broadcast

**Objective:** Verify vote request sent to validators

**Steps:**
```bash
# On Node A (submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# On Node B (validator):
journalctl -u timed -f | grep -E "TimeVoteRequest.*$TXID"
```

**Success Criteria:**
- [ ] Log on submitter: "📡 Broadcasting TimeVoteRequest for TX"
- [ ] Includes: txid, tx_hash_commitment, slot_index
- [ ] 200ms delay after TransactionBroadcast
- [ ] Broadcast to all validators
- [ ] No error: "No broadcast callback available"

**Verification:**
```bash
# Check all validator nodes received request
for node in node1 node2 node3 node4 node5 node6; do
  ssh root@$node "journalctl -u timed --since '1 minute ago' | grep 'TimeVoteRequest' | grep '$TXID'" && echo "$node: ✅" || echo "$node: ❌"
done
```

---

### Test 3.6: Mempool Limit Enforcement

**Objective:** Verify mempool rejects when full

**Setup:**
```bash
# Would require sending 10,000 transactions
# Skip for now - edge case test
```

**Success Criteria:**
- [ ] After 10,000 pending TXs, new TXs rejected
- [ ] Error: "Mempool full"
- [ ] Oldest TXs not evicted (no replacement policy yet)

---

## Phase 4 Tests: Vote Collection

### Test 4.1: Validator Receives Vote Request

**Objective:** Verify validators process TimeVoteRequest

**Steps:**
```bash
# On Node A (submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# On Node B (validator):
journalctl -u timed -f | grep -A 10 "TimeVoteRequest.*$TXID"
```

**Success Criteria:**
- [ ] Validator logs: "Received TimeVoteRequest for TX ..."
- [ ] Validator checks if TX in pending pool
- [ ] Validator validates tx_hash_commitment
- [ ] Validator checks UTXO availability
- [ ] Vote decision made: Accept or Reject

---

### Test 4.2: Validator Sends Vote Response

**Objective:** Verify validators send TimeVoteResponse

**Steps:**
```bash
# On Node A (submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Watch for vote responses
journalctl -u timed -f | grep -E "TimeVoteResponse.*$TXID|Received.*vote"
```

**Success Criteria:**
- [ ] Validator logs: "✅ Vote response sent for TX ..."
- [ ] Response includes: txid, preference, validator_addr, signature
- [ ] Response sent within 100ms of request
- [ ] Signature valid

---

### Test 4.3: Vote Accumulation

**Objective:** Verify submitter accumulates votes

**Steps:**
```bash
# On Node A (submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Watch vote collection
journalctl -u timed -f | grep -E "Round.*votes|Tally result"
```

**Success Criteria:**
- [ ] Logs show: "Round N: Tally result - X votes for Accept"
- [ ] Votes weighted by validator tier
- [ ] Vote count increases over rounds
- [ ] Consensus determined (Accept vs Reject)

**Verification:**
```bash
# Count votes received
journalctl -u timed --since "1 minute ago" | grep "$TXID" | grep -c "vote"
# Should be >= number of validators
```

---

### Test 4.4: Stake-Weighted Voting

**Objective:** Verify vote weights based on masternode tiers

**Setup:**
```bash
# Check validator tiers
time-cli getmasternodes | jq '.[] | {address: .address, tier: .tier}'
```

**Test Cases:**

**Case A: All Free Tier (weight=1 each)**
```bash
# With 6 Free validators, need 4 votes for consensus (>50%)
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)
# Check if 4 votes = consensus
```

**Case B: Mixed Tiers**
```bash
# 5 Free (weight=1) + 1 Gold (weight=1000)
# Gold validator vote should dominate
# Need to test if Gold alone can finalize
```

**Success Criteria:**
- [ ] Vote weights applied correctly
- [ ] Free: 1, Bronze: 10, Silver: 100, Gold: 1000
- [ ] Consensus = sum(Accept_weights) > sum(Reject_weights)
- [ ] Higher tiers have proportional influence

---

### Test 4.5: Vote Rejection Handling

**Objective:** Verify handling of Reject votes

**Setup:**
```bash
# Create scenario where validators vote Reject
# E.g., send TX with already-spent UTXOs
```

**Steps:**
```bash
# Send valid TX
TXID1=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Try to send same UTXOs again (should fail earlier, but test vote rejection)
```

**Success Criteria:**
- [ ] Validators vote Reject for double-spend
- [ ] Preference switches to Reject if majority votes Reject
- [ ] TX not finalized
- [ ] TX removed from pending pool after rejection

---

### Test 4.6: Multiple Query Rounds

**Objective:** Verify Snowball executes multiple rounds

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Watch for multiple rounds
journalctl -u timed -f | grep -E "Round [0-9].*$TXID"
```

**Success Criteria:**
- [ ] Multiple rounds logged (Round 0, 1, 2, ...)
- [ ] Each round: vote request → wait 200ms → tally
- [ ] Confidence increments with consistent preference
- [ ] Max 10 rounds before termination

**Count Rounds:**
```bash
journalctl -u timed --since "1 minute ago" | grep "$TXID" | grep -c "Round"
```

---

## Phase 5 Tests: Finalization

### Test 5.1: Normal Finalization (Snowball Confidence)

**Objective:** Verify finalization when confidence threshold reached

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization
sleep 2

# Check logs
journalctl -u timed --since "1 minute ago" | grep -E "$TXID.*finalized.*confidence"
```

**Success Criteria:**
- [ ] Log: "✅ TX ... finalized via TimeVote after round N"
- [ ] Log: "📦 TX ... moved to finalized pool (Snowball confidence threshold reached)"
- [ ] Confidence >= 20 (default threshold)
- [ ] TX moved from pending → finalized pool
- [ ] Finalization broadcast sent

**Verify Finalized Pool:**
```bash
time-cli getmempoolinfo | jq '.finalized_count'
# Should be >= 1
```

---

### Test 5.2: Auto-Finalize (Zero Votes)

**Objective:** Verify auto-finalize when validators don't respond

**Setup:**
```bash
# Requires network where validators don't respond
# Or firewall validator nodes temporarily
```

**Steps:**
```bash
# Block validator responses temporarily
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for timeout (10 rounds × 300ms = 3s)
sleep 5

# Check logs
journalctl -u timed --since "10 seconds ago" | grep -E "$TXID.*auto-finalized.*0 votes"
```

**Success Criteria:**
- [ ] Log: "⚠️ TX ... received 0 votes"
- [ ] Log: "✅ TX ... auto-finalized (UTXO-lock protected)"
- [ ] TX finalized despite no votes
- [ ] Safety: UTXOs were locked, preventing double-spend

---

### Test 5.3: Transaction Status Transition

**Objective:** Verify status transitions during finalization

**Steps:**
```bash
# Send transaction and track status
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Check status over time
for i in {1..5}; do
  STATUS=$(journalctl -u timed --since "10 seconds ago" | grep "$TXID" | grep -E "Voting|Finalized" | tail -1)
  echo "T+${i}s: $STATUS"
  sleep 1
done
```

**Success Criteria:**
- [ ] Initial: "TX ... → Voting"
- [ ] After consensus: "TX ... → Finalized"
- [ ] No "FallbackResolution" state (for normal case)
- [ ] Finalized timestamp recorded

---

### Test 5.4: Finalize Transaction in Pool

**Objective:** Verify finalize_transaction() moves TX correctly

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Before finalization
PENDING_BEFORE=$(time-cli getmempoolinfo | jq '.size')
FINALIZED_BEFORE=$(time-cli getmempoolinfo | jq '.finalized_count')

# Wait for finalization
sleep 3

# After finalization
PENDING_AFTER=$(time-cli getmempoolinfo | jq '.size')
FINALIZED_AFTER=$(time-cli getmempoolinfo | jq '.finalized_count')

echo "Pending: $PENDING_BEFORE → $PENDING_AFTER"
echo "Finalized: $FINALIZED_BEFORE → $FINALIZED_AFTER"
```

**Success Criteria:**
- [ ] Pending count decreased by 1
- [ ] Finalized count increased by 1
- [ ] TX removed from pending pool
- [ ] TX added to finalized pool
- [ ] TX data preserved (not corrupted)

---

### Test 5.5: UTXO State Transition to SpentFinalized

**Objective:** Verify UTXOs update to SpentFinalized

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization
sleep 3

# Check UTXO states
time-cli listunspent | jq '.[] | select(.state == "SpentFinalized")'
```

**Success Criteria:**
- [ ] Input UTXOs in `SpentFinalized` state
- [ ] State includes: txid, finalized_at, votes
- [ ] UTXOs no longer in `SpentPending`
- [ ] UTXOs still in UTXO set (not removed yet)

---

## Phase 6 Tests: Finalization Propagation

### Test 6.1: TransactionFinalized Broadcast ⭐ CRITICAL

**Objective:** Verify finalization broadcasts to all nodes

**Steps:**
```bash
# On Node A (submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization
sleep 3

# Check Node A logs
journalctl -u timed --since "10 seconds ago" | grep -E "$TXID.*Broadcast TransactionFinalized"

# On Node B, C, D, E, F (other nodes):
ssh root@[other-node] "journalctl -u timed --since '10 seconds ago' | grep 'Received TransactionFinalized' | grep '$TXID'"
```

**Success Criteria:**
- [ ] Node A logs: "📡 Broadcast TransactionFinalized for ..."
- [ ] All other nodes log: "✅ Transaction ... finalized (from [nodeA])"
- [ ] Broadcast within 100ms of finalization
- [ ] All nodes receive message within 2 seconds

**Bug Fix Verification:**
- [ ] This broadcast was MISSING before v1.1.0
- [ ] Must see broadcast logs on submitter
- [ ] Must see receipt logs on all other nodes

---

### Test 6.2: Handler Finalizes Transaction Locally ⭐ CRITICAL

**Objective:** Verify receiving nodes actually finalize TX

**Steps:**
```bash
# On Node A (submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization
sleep 3

# On Node B (receiver):
ssh root@[node-B] "journalctl -u timed --since '10 seconds ago' | grep '$TXID' | grep 'Moved TX.*to finalized pool on this node'"

# Check Node B's finalized pool
ssh root@[node-B] "time-cli getmempoolinfo | jq '.finalized_count'"
```

**Success Criteria:**
- [ ] Node B logs: "📦 Moved TX ... to finalized pool on this node"
- [ ] Node B's finalized_count increased
- [ ] Node B can query TX from finalized pool
- [ ] TX not in Node B's pending pool anymore

**Bug Fix Verification:**
- [ ] Handler only LOGGED before v1.1.0, didn't finalize
- [ ] Must see "Moved TX to finalized pool" log
- [ ] Must confirm TX in finalized pool via getmempoolinfo

---

### Test 6.3: Network-Wide Finalized Pool Sync ⭐ CRITICAL

**Objective:** Verify ALL nodes have TX in finalized pool

**Steps:**
```bash
# On Node A (submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization propagation
sleep 5

# Check finalized pool on all nodes
for node in node1 node2 node3 node4 node5 node6; do
  COUNT=$(ssh root@$node "time-cli getmempoolinfo | jq '.finalized_count'")
  echo "$node: $COUNT finalized TXs"
done
```

**Success Criteria:**
- [ ] All 6 nodes show finalized_count >= 1
- [ ] All 6 nodes have same finalized_count
- [ ] TX present in finalized pool on ALL nodes
- [ ] Sync completes within 5 seconds

**Bug Fix Verification:**
- [ ] Before v1.1.0: Only submitter had TX in finalized pool
- [ ] After v1.1.0: ALL nodes have TX in finalized pool
- [ ] This is THE critical fix for multi-node consensus

---

### Test 6.4: Gossip Propagation of Finalization

**Objective:** Verify finalization gossips beyond direct neighbors

**Steps:**
```bash
# Send transaction from Node A
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization
sleep 3

# Check propagation depth
for node in node1 node2 node3 node4 node5 node6; do
  ssh root@$node "journalctl -u timed --since '10 seconds ago' | grep '$TXID' | grep -E 'Received|Gossiped' | tail -1"
done
```

**Success Criteria:**
- [ ] Finalization gossips to all peers
- [ ] Each node forwards to its peers
- [ ] Full network coverage within 2 seconds
- [ ] Log: "🔄 Gossiped finalization to N peer(s)"

---

## Phase 7 Tests: Block Production

### Test 7.1: Deterministic Leader Selection

**Objective:** Verify VRF-based leader selection

**Steps:**
```bash
# Wait for next block
START_HEIGHT=$(time-cli getblockcount)
while [ $(time-cli getblockcount) -eq $START_HEIGHT ]; do
  sleep 1
done

# Check who produced the block
LEADER=$(time-cli getblock $((START_HEIGHT+1)) | jq -r '.header.leader')
echo "Block $((START_HEIGHT+1)) leader: $LEADER"

# Verify VRF output
time-cli getblock $((START_HEIGHT+1)) | jq '.header | {vrf_output, vrf_score}'
```

**Success Criteria:**
- [ ] Block has valid leader address
- [ ] VRF output is 32-byte hash
- [ ] VRF score is deterministic from prev_hash + timestamp
- [ ] Same node would be selected by all validators

**Logs to Check:**
```bash
journalctl -u timed --since "2 minutes ago" | grep "leader selection"
```

**Expected Log:**
```
INFO 🎲 Block 1827 leader selection: 6 of 6 masternodes, selected: 69.167.168.176 (us: YES)
```

---

### Test 7.2: Query Finalized Transactions ⭐ CRITICAL

**Objective:** Verify block producer includes finalized TXs

**Steps:**
```bash
# On ANY node (not just submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization propagation
sleep 5

# Wait for next block (any node can be producer)
START_HEIGHT=$(time-cli getblockcount)
while [ $(time-cli getblockcount) -eq $START_HEIGHT ]; do
  sleep 5
done

# Check if TX is in block
time-cli getblock $((START_HEIGHT+1)) | jq '.transactions[].txid' | grep "$TXID"
```

**Success Criteria:**
- [ ] TX appears in block produced by ANY node
- [ ] Not just blocks produced by submitter
- [ ] Block producer queries finalized pool
- [ ] Log: "🔍 Block N: Including X finalized transaction(s)"

**Bug Fix Verification:**
- [ ] Before v1.1.0: Only submitter could include TX
- [ ] After v1.1.0: ANY block producer can include TX
- [ ] Test specifically when non-submitter produces block

---

### Test 7.3: Transaction Fees Calculation ⭐ BUG FIX

**Objective:** Verify fees calculated from current block TXs

**Steps:**
```bash
# Send transaction with known fee
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Get transaction details
TX_JSON=$(time-cli getrawtransaction "$TXID")
INPUT_SUM=$(echo "$TX_JSON" | jq '.input_sum')
OUTPUT_SUM=$(echo "$TX_JSON" | jq '.output_sum')
EXPECTED_FEE=$((INPUT_SUM - OUTPUT_SUM))

# Wait for block inclusion
sleep 65

# Get block that included TX
BLOCK_HEIGHT=$(time-cli gettransaction "$TXID" | jq '.blockheight')
BLOCK=$(time-cli getblock "$BLOCK_HEIGHT")

# Check block reward
BASE_REWARD=10000000000  # 100 TIME
BLOCK_REWARD=$(echo "$BLOCK" | jq '.header.block_reward')
INCLUDED_FEE=$((BLOCK_REWARD - BASE_REWARD))

echo "Expected fee: $EXPECTED_FEE"
echo "Included fee: $INCLUDED_FEE"
```

**Success Criteria:**
- [ ] Block reward = base_reward + fees_from_block_txs
- [ ] Fees calculated from TXs in THIS block
- [ ] NOT stored for next block
- [ ] Log: "💸 Block N: included X satoshis in fees"

**Bug Fix Verification:**
- [ ] Before v1.1.0: Block reward incorrect, blocks rejected
- [ ] After v1.1.0: Block reward correct
- [ ] No "incorrect block_reward" errors in logs

---

### Test 7.4: Block Transaction Structure

**Objective:** Verify correct block transaction ordering

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for block inclusion
sleep 65

# Get block
BLOCK_HEIGHT=$(time-cli gettransaction "$TXID" | jq '.blockheight')
BLOCK=$(time-cli getblock "$BLOCK_HEIGHT")

# Check transaction order
echo "$BLOCK" | jq '.transactions[] | {index: .index, type: .type, txid: .txid}'
```

**Success Criteria:**
- [ ] Transaction 0: Coinbase (creates reward)
- [ ] Transaction 1: Reward distribution (pays producer)
- [ ] Transactions 2+: User transactions (sorted by TXID)
- [ ] All TXs have valid structure

---

### Test 7.5: Merkle Root Calculation

**Objective:** Verify deterministic merkle root

**Steps:**
```bash
# Get block
BLOCK_HEIGHT=$(time-cli getblockcount)
BLOCK=$(time-cli getblock "$BLOCK_HEIGHT")

# Extract merkle root
MERKLE_ROOT=$(echo "$BLOCK" | jq -r '.header.merkle_root')

# Verify it's a valid 32-byte hash
echo "$MERKLE_ROOT" | grep -E '^[0-9a-f]{64}$'
```

**Success Criteria:**
- [ ] Merkle root is 64-character hex string
- [ ] All nodes compute same merkle root
- [ ] Root changes if transactions change
- [ ] Deterministic based on transaction order

---

### Test 7.6: Block Proposal Broadcast

**Objective:** Verify block proposal broadcasts to validators

**Steps:**
```bash
# When we're the block producer:
journalctl -u timed -f | grep -E "Block.*produced.*broadcasting"

# On other nodes:
ssh root@[other-node] "journalctl -u timed -f | grep 'Received TimeLock Block proposal'"
```

**Success Criteria:**
- [ ] Producer logs: "📦 Block N produced: X txs - broadcasting for consensus"
- [ ] Validators log: "📦 Received TimeLock Block proposal at height N"
- [ ] Broadcast within 100ms of block creation
- [ ] All validators receive proposal

---

## Phase 8 Tests: Block Consensus

### Test 8.1: Prepare Phase Voting

**Objective:** Verify validators send prepare votes

**Steps:**
```bash
# Monitor for prepare votes
journalctl -u timed -f | grep -E "prepare vote|Block.*prepare"
```

**Success Criteria:**
- [ ] Log: "🗳️  Cast prepare vote for block N"
- [ ] Prepare votes from multiple validators
- [ ] Vote includes block hash
- [ ] Vote weight based on validator tier

---

### Test 8.2: Precommit Phase Voting

**Objective:** Verify precommit votes after prepare threshold

**Steps:**
```bash
# Monitor for precommit votes
journalctl -u timed -f | grep -E "precommit vote|Block.*precommit"
```

**Success Criteria:**
- [ ] Log: "Generated precommit vote for block ..."
- [ ] Precommit follows prepare phase
- [ ] 67% threshold required
- [ ] Votes accumulate toward commit

---

### Test 8.3: Block Acceptance

**Objective:** Verify block accepted after consensus

**Steps:**
```bash
# Wait for new block
START_HEIGHT=$(time-cli getblockcount)
while [ $(time-cli getblockcount) -eq $START_HEIGHT ]; do
  sleep 1
done

# Check acceptance logs
journalctl -u timed --since "30 seconds ago" | grep -E "Block.*accepted|added to chain"
```

**Success Criteria:**
- [ ] Log: "✅ Block N accepted by consensus"
- [ ] Block added to local chain
- [ ] All nodes accept same block
- [ ] No forks created

---

### Test 8.4: Block Rejection Handling

**Objective:** Verify invalid blocks rejected

**Steps:**
```bash
# This requires crafting an invalid block
# For now, monitor for natural rejections
journalctl -u timed -f | grep -E "Failed to add block|Block.*rejected"
```

**Success Criteria:**
- [ ] Invalid blocks not added to chain
- [ ] Log shows rejection reason
- [ ] Chain height not incremented
- [ ] Sender may be blacklisted if malicious

---

## Phase 9 Tests: Block Storage

### Test 9.1: Block Storage and Retrieval

**Objective:** Verify block stored correctly in RocksDB

**Steps:**
```bash
# Get latest block
HEIGHT=$(time-cli getblockcount)
BLOCK=$(time-cli getblock "$HEIGHT")

# Verify storage
echo "$BLOCK" | jq '{
  height: .header.height,
  hash: .hash,
  transactions: (.transactions | length),
  timestamp: .header.timestamp
}'
```

**Success Criteria:**
- [ ] Block retrievable by height
- [ ] Block retrievable by hash
- [ ] All fields intact
- [ ] Transactions preserved

---

### Test 9.2: UTXO Set Updates

**Objective:** Verify UTXOs updated after block storage

**Steps:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Get input UTXOs
INPUTS=$(time-cli getrawtransaction "$TXID" | jq -r '.inputs[].previous_output')

# Wait for block inclusion
sleep 65

# Check UTXOs removed
for input in $INPUTS; do
  time-cli getutxo "$input" 2>&1 | grep "not found"
done

# Check new UTXOs created
OUTPUTS=$(time-cli getrawtransaction "$TXID" | jq -r '.outputs | keys[]')
for idx in $OUTPUTS; do
  time-cli getutxo "${TXID}:${idx}" && echo "Output $idx exists"
done
```

**Success Criteria:**
- [ ] Input UTXOs removed from set
- [ ] Output UTXOs added to set
- [ ] New UTXOs in `Confirmed` state
- [ ] UTXO database consistent

---

### Test 9.3: Finalized Pool Cleanup ⭐ BUG FIX

**Objective:** Verify only block TXs cleared from finalized pool

**Steps:**
```bash
# Send multiple transactions
TXID1=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)
sleep 1
TXID2=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for both to finalize
sleep 5

# Check finalized count
FINALIZED_BEFORE=$(time-cli getmempoolinfo | jq '.finalized_count')
echo "Finalized before block: $FINALIZED_BEFORE"

# Wait for next block (may include only TXID1)
sleep 65

# Check finalized count after block
FINALIZED_AFTER=$(time-cli getmempoolinfo | jq '.finalized_count')
echo "Finalized after block: $FINALIZED_AFTER"

# If block included 1 TX, pool should decrease by 1
# If block included 2 TXs, pool should decrease by 2
```

**Success Criteria:**
- [ ] Only TXs in block removed from finalized pool
- [ ] TXs not in block remain in finalized pool
- [ ] Log: "🔍 Block N: Clearing X finalized transaction(s) from pool"
- [ ] No premature clearing

**Bug Fix Verification:**
- [ ] Before v1.1.0: ALL finalized TXs cleared
- [ ] After v1.1.0: Only block TXs cleared
- [ ] Selective clearing maintains pool integrity

---

### Test 9.4: Chain State Updates

**Objective:** Verify chain height and work updated

**Steps:**
```bash
# Get chain state before block
HEIGHT_BEFORE=$(time-cli getblockcount)
WORK_BEFORE=$(time-cli getinfo | jq '.chainwork')

# Wait for new block
sleep 65

# Get chain state after block
HEIGHT_AFTER=$(time-cli getblockcount)
WORK_AFTER=$(time-cli getinfo | jq '.chainwork')

echo "Height: $HEIGHT_BEFORE → $HEIGHT_AFTER"
echo "Work: $WORK_BEFORE → $WORK_AFTER"
```

**Success Criteria:**
- [ ] Chain height incremented by 1
- [ ] Cumulative work increased
- [ ] Current height stored in database
- [ ] Chain work entry created

---

### Test 9.5: Undo Log Creation

**Objective:** Verify undo log created for rollback support

**Steps:**
```bash
# This requires database inspection
# Log should show undo log creation
journalctl -u timed --since "2 minutes ago" | grep "undo log"
```

**Success Criteria:**
- [ ] Undo log created for each block
- [ ] Contains spent UTXOs (for restoration)
- [ ] Contains created UTXOs (for removal)
- [ ] Stored with block height key

---

### Test 9.6: Block Hash Verification ⭐ CRITICAL

**Objective:** Verify block hash doesn't change after storage

**Steps:**
```bash
# Monitor block storage
journalctl -u timed -f | grep -E "PRE-STORAGE|POST-STORAGE|hash"
```

**Success Criteria:**
- [ ] Log: "PRE-STORAGE: Block N hash abc123..."
- [ ] Log: "POST-STORAGE: Block N hash abc123..." (same!)
- [ ] No hash mismatch errors
- [ ] Block accepted if hash matches

**Error Case:**
- [ ] If hash mismatch: block rejected, removed from storage
- [ ] Log: "CRITICAL: POST-STORAGE HASH MISMATCH"

---

## Bug Verification Tests

### Bug Test 1: Finalized Pool Not Cleared Prematurely

**Bug:** Pool cleared after every block, even when TX not in block

**Test:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization
sleep 5

# Check finalized pool
FINALIZED=$(time-cli getmempoolinfo | jq '.finalized_count')

# If another node produces next block WITHOUT our TX:
# Our finalized pool should KEEP the TX
sleep 65

FINALIZED_AFTER=$(time-cli getmempoolinfo | jq '.finalized_count')

echo "Before block: $FINALIZED"
echo "After block: $FINALIZED_AFTER"
```

**Success Criteria:**
- [ ] If TX not in block: finalized_count unchanged
- [ ] If TX in block: finalized_count decreases by 1
- [ ] No clearing of unrelated TXs

**Before v1.1.0:** FAIL - all TXs cleared  
**After v1.1.0:** PASS - selective clearing

---

### Bug Test 2: Broadcast Callback Wired

**Bug:** Vote requests not sent because callback not wired

**Test:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Check for vote request broadcast
journalctl -u timed --since "10 seconds ago" | grep -E "Broadcasting TimeVoteRequest|No broadcast callback"
```

**Success Criteria:**
- [ ] Log: "📡 Broadcasting TimeVoteRequest"
- [ ] NO log: "❌ No broadcast callback available"
- [ ] Vote requests sent to network

**Before v1.1.0:** FAIL - "No broadcast callback available"  
**After v1.1.0:** PASS - broadcasts sent

---

### Bug Test 3: Fees in Current Block

**Bug:** Fees calculated for next block, causing reward mismatch

**Test:**
```bash
# Send transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for block inclusion
sleep 65

# Check for block rejection
journalctl -u timed --since "2 minutes ago" | grep "incorrect block_reward"
```

**Success Criteria:**
- [ ] NO "incorrect block_reward" errors
- [ ] Blocks accepted by all nodes
- [ ] Block reward = base + fees_from_included_txs

**Before v1.1.0:** FAIL - blocks rejected  
**After v1.1.0:** PASS - blocks accepted

---

### Bug Test 4: Finalization Propagates Network-Wide ⭐ PRIMARY BUG

**Bug:** Only submitter had TX in finalized pool

**Test:**
```bash
# On Node A (submitter):
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization
sleep 5

# Check ALL nodes for finalized pool
for node in node1 node2 node3 node4 node5 node6; do
  ssh root@$node "time-cli getmempoolinfo | jq '.finalized_count'"
done

# Also check if non-submitter can include TX in block
# Wait for a block produced by different node
sleep 65
BLOCK_HEIGHT=$(time-cli gettransaction "$TXID" | jq '.blockheight')
BLOCK_LEADER=$(time-cli getblock "$BLOCK_HEIGHT" | jq -r '.header.leader')
echo "TX included in block produced by: $BLOCK_LEADER"
```

**Success Criteria:**
- [ ] All 6 nodes have TX in finalized pool
- [ ] finalized_count consistent across all nodes
- [ ] Block produced by ANY node includes TX
- [ ] Not just blocks produced by submitter

**Before v1.1.0:** FAIL - only submitter has TX, others can't include it  
**After v1.1.0:** PASS - all nodes have TX, any node can include it

---

## Edge Case Tests

### Edge 1: Concurrent Transactions

**Objective:** Multiple TXs submitted simultaneously

**Test:**
```bash
# Send multiple transactions concurrently
for i in {1..10}; do
  (time-cli sendtoaddress "$TEST_ADDR_1" 0.1 &)
done

# Wait for all to complete
sleep 10

# Check all finalized
time-cli getmempoolinfo | jq '{pending: .size, finalized: .finalized_count}'
```

**Success Criteria:**
- [ ] All 10 TXs accepted
- [ ] All finalize independently
- [ ] No UTXO conflicts
- [ ] All included in subsequent blocks

---

### Edge 2: Large Transaction

**Objective:** TX with many inputs/outputs

**Test:**
```bash
# Merge many UTXOs into one
time-cli mergeutxos

# Send large transaction
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 100.0)
```

**Success Criteria:**
- [ ] Large TX accepted
- [ ] Broadcasts within size limits
- [ ] Validators process correctly
- [ ] Included in block

---

### Edge 3: Network Partition

**Objective:** Finalization during network split

**Setup:**
```bash
# Temporarily partition network (firewall rules)
# Requires manual network manipulation
```

**Test:**
- Send TX on partition A
- Verify finalization on partition A only
- Rejoin network
- Verify finalization propagates to partition B

**Success Criteria:**
- [ ] TX finalizes on reachable nodes
- [ ] Finalization propagates after partition heals
- [ ] No double-finalization issues

---

### Edge 4: Block Producer Rotation

**Objective:** Different nodes produce consecutive blocks

**Test:**
```bash
# Watch block producers over time
for i in {1..10}; do
  HEIGHT=$(time-cli getblockcount)
  LEADER=$(time-cli getblock "$HEIGHT" | jq -r '.header.leader')
  echo "Block $HEIGHT: $LEADER"
  sleep 60
done
```

**Success Criteria:**
- [ ] Different nodes produce blocks
- [ ] Fair rotation based on VRF
- [ ] All finalized TXs included regardless of producer
- [ ] No single node dominance

---

### Edge 5: Transaction Replacement (RBF)

**Objective:** Attempt to replace pending transaction

**Test:**
```bash
# Send TX with low fee
TXID1=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Try to send same UTXOs with higher fee
TXID2=$(time-cli sendtoaddress "$TEST_ADDR_1" 0.9)  # More fee
```

**Success Criteria:**
- [ ] Second TX rejected (UTXOs locked)
- [ ] First TX completes normally
- [ ] No RBF support (locked UTXOs prevent replacement)

---

## Performance Tests

### Perf 1: Transaction Finalization Latency

**Objective:** Measure time from submission to finalization

**Test:**
```bash
#!/bin/bash
for i in {1..100}; do
  START=$(date +%s%N)
  TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 0.1)
  
  # Poll for finalization
  while true; do
    if time-cli getmempoolinfo | jq -e ".finalized_transactions | any(.txid == \"$TXID\")" &>/dev/null; then
      END=$(date +%s%N)
      LATENCY=$(( (END - START) / 1000000 ))  # Convert to ms
      echo "TX $i: ${LATENCY}ms"
      break
    fi
    sleep 0.1
  done
done
```

**Target:** <2000ms (2 seconds) per transaction

---

### Perf 2: Block Inclusion Time

**Objective:** Measure time from finalization to block inclusion

**Test:**
```bash
#!/bin/bash
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)

# Wait for finalization
while ! time-cli getmempoolinfo | jq -e ".finalized_transactions | any(.txid == \"$TXID\")" &>/dev/null; do
  sleep 0.1
done
FINALIZED_AT=$(date +%s)

# Wait for block inclusion
while ! time-cli gettransaction "$TXID" 2>/dev/null | jq -e '.blockheight' &>/dev/null; do
  sleep 1
done
INCLUDED_AT=$(date +%s)

WAIT_TIME=$((INCLUDED_AT - FINALIZED_AT))
echo "Wait time: ${WAIT_TIME}s"
```

**Target:** <60 seconds (next block)

---

### Perf 3: Throughput (TPS)

**Objective:** Maximum transactions per second

**Test:**
```bash
#!/bin/bash
COUNT=0
START=$(date +%s)

# Send transactions for 60 seconds
timeout 60 bash -c 'while true; do
  time-cli sendtoaddress "$TEST_ADDR_1" 0.01 &>/dev/null && ((COUNT++))
done'

END=$(date +%s)
DURATION=$((END - START))
TPS=$(echo "scale=2; $COUNT / $DURATION" | bc)

echo "Sent $COUNT transactions in ${DURATION}s"
echo "TPS: $TPS"
```

**Target:** >10 TPS sustained

---

### Perf 4: Network Propagation Time

**Objective:** Measure TX broadcast propagation time

**Test:**
```bash
# Requires synchronized clocks on all nodes
TXID=$(time-cli sendtoaddress "$TEST_ADDR_1" 1.0)
SENT_AT=$(date +%s%N)

# On each other node, check when received
for node in node2 node3 node4 node5 node6; do
  ssh root@$node "journalctl -u timed --since '1 second ago' | grep '$TXID' | grep 'Received'" | while read line; do
    # Extract timestamp and calculate delta
    echo "$node: received"
  done
done
```

**Target:** <500ms to all nodes

---

## Stress Tests

### Stress 1: Mempool Saturation

**Objective:** Fill mempool to capacity

**Test:**
```bash
# Send 10,000 transactions (mempool limit)
for i in {1..10000}; do
  time-cli sendtoaddress "$TEST_ADDR_1" 0.001 &
  if (( i % 100 == 0 )); then
    echo "Sent $i transactions"
    wait
  fi
done
```

**Monitor:**
- Mempool size
- Node performance
- Memory usage
- Time to process all TXs

---

### Stress 2: Validator Load

**Objective:** Multiple transactions under voting

**Test:**
```bash
# Send 100 concurrent transactions
for i in {1..100}; do
  time-cli sendtoaddress "$TEST_ADDR_1" 0.1 &
done

# Monitor validator logs
journalctl -u timed -f | grep -E "TimeVoteRequest|vote"
```

**Monitor:**
- Vote processing rate
- CPU usage on validators
- Vote response latency

---

### Stress 3: Block Size

**Objective:** Maximum transactions per block

**Test:**
```bash
# Send many transactions
for i in {1..1000}; do
  time-cli sendtoaddress "$TEST_ADDR_1" 0.001 &
done

# Wait for finalization
sleep 10

# Wait for block
sleep 65

# Check block size
LATEST=$(time-cli getblockcount)
time-cli getblock "$LATEST" | jq '{
  height: .header.height,
  tx_count: (.transactions | length),
  size_bytes: (. | tostring | length)
}'
```

**Monitor:**
- Max TXs per block
- Block propagation time
- Storage efficiency

---

## Test Results Template

### Summary

| Test Suite | Total | Passed | Failed | Skipped |
|------------|-------|--------|--------|---------|
| Phase 1: Submission | 6 | | | |
| Phase 2: Broadcast | 4 | | | |
| Phase 3: Consensus | 6 | | | |
| Phase 4: Voting | 6 | | | |
| Phase 5: Finalization | 5 | | | |
| Phase 6: Propagation | 4 | | | |
| Phase 7: Block Production | 6 | | | |
| Phase 8: Block Consensus | 4 | | | |
| Phase 9: Storage | 6 | | | |
| Bug Verification | 4 | | | |
| Edge Cases | 5 | | | |
| Performance | 4 | | | |
| Stress | 3 | | | |
| **TOTAL** | **63** | | | |

### Critical Tests (Must Pass)

- [ ] Test 6.1: TransactionFinalized Broadcast
- [ ] Test 6.2: Handler Finalizes Locally
- [ ] Test 6.3: Network-Wide Finalized Pool Sync
- [ ] Test 7.2: Query Finalized Transactions
- [ ] Test 7.3: Transaction Fees Calculation
- [ ] Test 9.3: Finalized Pool Cleanup
- [ ] Bug Test 4: Finalization Propagates Network-Wide

### Known Issues

| Issue | Status | Workaround |
|-------|--------|------------|
| | | |

### Environment

- **Network:** Mainnet / Testnet / Dev
- **Node Count:** 6
- **Version:** 1.1.0
- **Test Date:** 2026-01-28
- **Test Duration:** X hours
- **Hardware:** 
- **Network Latency:**

### Notes

---

**End of Test Plan**
