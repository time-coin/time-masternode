# TimeCoin Testing & Validation Roadmap

## Phase 5: Testnet Validation (Next 2 weeks)

### 5.1 Single-Node Tests (Day 1-2)
```bash
# Start single node and verify basic operations
timed --config config.testnet.toml

# Test: Block production
- Verify blocks are created every 60 seconds
- Check block signatures are valid
- Validate timestamps are monotonic increasing

# Test: Transaction processing
- Submit test transactions to mempool
- Verify they're validated correctly
- Check UTXO state transitions
```

**Success Criteria**:
- [ ] Node produces blocks with valid signatures
- [ ] UTXO states update correctly
- [ ] No panics or crashes

### 5.2 3-Node Consensus Tests (Day 3-4)
```bash
# Start 3 mastermodes
Node A: timed --config config1.testnet.toml
Node B: timed --config config2.testnet.toml  
Node C: timed --config config3.testnet.toml

# Monitor consensus:
- Check heartbeat attestations (every 5 secs)
- Verify block propagation time (<2 sec)
- Monitor peer connections (should be fully connected)
```

**Success Criteria**:
- [ ] All 3 nodes reach consensus on blocks
- [ ] Block propagation latency < 2 seconds
- [ ] No fork/double-spend issues
- [ ] Byzantine nodes can be tolerated (if testing with 4+ nodes)

### 5.3 Network Stress Tests (Day 5-7)
```bash
# Test synchronization speed
- Stop one node for 5 blocks
- Restart and measure sync time
- Expected: < 30 seconds to catch up

# Test under load
- Submit 100 TPS (transactions per second)
- Monitor node performance (CPU, memory, network)
- Check finality time

# Test peer churn
- Kill 1 node every 10 seconds
- Restart random nodes
- Verify network recovers
```

**Success Criteria**:
- [ ] Sync catches up within 30 seconds
- [ ] Handles 100 TPS without dropping blocks
- [ ] Network self-heals after peer churn
- [ ] Memory usage stable (no leaks)

### 5.4 Byzantine Tolerance Tests (Day 8-10)
```bash
# With 4 nodes (f=1 Byzantine tolerance)
- Node A: Honest
- Node B: Honest
- Node C: Honest
- Node D: Malicious (send bad signatures)

# Test: Can the network tolerate D?
- Honest nodes should reach consensus
- D should be eventually isolated (rate-limited)
- Chain progress should continue

# Test: 2 Byzantine nodes
- Add Node E: Malicious
- With f=1, should NOT reach consensus
- Verify network doesn't fork
```

**Success Criteria**:
- [ ] 1 Byzantine node: Chain progresses ✓
- [ ] 2 Byzantine nodes: Chain halts safely ✓
- [ ] No invalid blocks accepted
- [ ] Rate limiting prevents spam

### 5.5 Fork Resolution Tests (Day 11-14)
```bash
# Network partition
- Partition network A (nodes 1,2) vs B (nodes 3,4)
- A has 2/3 weight → should produce blocks
- B has 1/3 weight → should reject blocks

# Resolution
- Heal network partition
- Both partitions should converge to A's chain
- All nodes end with same state
```

**Success Criteria**:
- [ ] Minority partition rejects blocks
- [ ] Majority partition produces blocks  
- [ ] After healing: Convergence in <1 minute
- [ ] No conflicting transactions

## Critical Metrics to Monitor

### Consensus Health
```
Metric                    | Target    | Alert If
--------------------------|-----------|----------
Block time               | 60 sec    | > 120 sec
Heartbeat interval       | 5 sec     | > 10 sec
Consensus latency        | < 2 sec   | > 5 sec
Finality time            | < 10 sec  | > 30 sec
2/3 quorum hits          | 100%      | < 95%
```

### Network Health
```
Metric                    | Target    | Alert If
--------------------------|-----------|----------
Peer connections         | 2+ (N-1)  | < 1
Msg delivery latency     | < 500ms   | > 1000ms
Peer churn rate          | < 1/min   | > 5/min
Network partition events | 0         | > 0
```

### Node Health
```
Metric                    | Target    | Alert If
--------------------------|-----------|----------
Memory usage             | < 512 MB  | > 1 GB
CPU usage (idle)         | < 5%      | > 20%
Block validation time    | < 100ms   | > 500ms
UTXO cache hit rate      | > 90%     | < 70%
Disk I/O wait            | < 1%      | > 5%
```

## Load Test Scenarios

### Scenario 1: TPS Ramp (1 → 100 TPS)
```
t=0-60s:   10 TPS
t=60-120s: 25 TPS  
t=120-180s: 50 TPS
t=180-240s: 100 TPS
```
**Check**: Block times, validation latency, mempool size

### Scenario 2: Transaction Mix
```
- 70% simple transfers
- 20% multi-input transactions  
- 10% contract calls (future)
```
**Check**: Validation time per transaction type

### Scenario 3: Peer Churn
```
- Kill random node every 30 seconds
- Restart it after 60 seconds
- Run for 10 minutes
```
**Check**: Sync time, consensus continuity

### Scenario 4: Byzantine Behavior
```
- Node sends:
  - Invalid signatures (1/100 msgs)
  - Old blocks (10 blocks old)
  - Empty blocks (no valid txs)
```
**Check**: Detection & isolation timing

## Failure Scenarios to Test

### Network Partition (A=2 nodes, B=1 node)
- [ ] A continues producing blocks (2/3 majority)
- [ ] B stops producing (minority)
- [ ] After heal: B syncs to A's chain
- [ ] No double-spends from parallel chains

### Clock Skew (Node runs 10 seconds behind)
- [ ] Blocks still accepted within window
- [ ] After NTP sync: State converges
- [ ] No timestamp-based rejections

### Sybil Attack (1000 fake peers)
- [ ] Rate limiting prevents spam
- [ ] Real peers' messages still prioritized
- [ ] Malicious peers eventually disconnected
- [ ] Node remains responsive

### Double-Spend Attempt
- [ ] UTXO locked after first broadcast
- [ ] Second transaction rejected
- [ ] Pending state visible to clients
- [ ] After finality: State irreversible

## Checklist for Testnet Launch

Before deploying to testnet:

**Core Functionality**
- [ ] Block creation working (60s interval)
- [ ] Signature validation passing
- [ ] UTXO state management correct
- [ ] Consensus reaching 2/3 quorum
- [ ] Peer discovery functional
- [ ] Network messages serializing correctly

**Performance**
- [ ] Block validation < 100ms
- [ ] Message propagation < 2 sec
- [ ] Startup time < 30 sec
- [ ] Memory stable (no leaks)
- [ ] CPU idle usage < 5%

**Security**
- [ ] All ed25519 sigs validated ✓
- [ ] Rate limiting active
- [ ] Peer authentication working
- [ ] No panics in error paths
- [ ] Proper timeouts on network ops

**Monitoring**
- [ ] Logs capture all events
- [ ] Prometheus metrics exposed
- [ ] Dashboard shows consensus health
- [ ] Alerts configured for failures
- [ ] Remote debugging available

---

**Timeline**: 2 weeks for Phase 5  
**Risk Level**: Medium (testnet only)  
**Resource Requirements**: 3 VPS + monitoring
