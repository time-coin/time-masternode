# Deployment & Rollback Guide - TIME Coin Critical Fixes
**Date:** December 21, 2025  
**Version:** 1.0  
**Audience:** DevOps Engineers, Release Managers

---

## Overview

This document outlines safe deployment procedures for TIME Coin critical fixes across testnet and mainnet environments.

---

## Pre-Deployment Checklist

### Code Quality (MUST PASS)
- [ ] `cargo fmt` - All code formatted
- [ ] `cargo clippy` - Zero new warnings
- [ ] `cargo build --release` - Compiles without errors
- [ ] `cargo test` - All tests passing
- [ ] Code review approved (2+ reviewers for critical code)
- [ ] Security review completed (blockchain-specific)
- [ ] No dead code or commented-out code
- [ ] All error messages are clear and actionable

### Testing (MUST PASS)
- [ ] Unit tests: 50+ tests, 100% passing
- [ ] Integration tests: 10+ scenarios, 100% passing
- [ ] Stress tests: 1000 tx/sec validated
- [ ] Byzantine tests: All consensus scenarios tested
- [ ] Network tests: Partition recovery tested
- [ ] Code coverage: >90% for critical paths

### Documentation (MUST COMPLETE)
- [ ] Code comments for complex logic
- [ ] Runbooks written and tested
- [ ] Change log updated
- [ ] Migration guide (if needed)
- [ ] Rollback procedure documented
- [ ] Monitoring alerting configured

### Backup (MUST COMPLETE)
- [ ] Database backup taken
- [ ] Current config backed up
- [ ] Previous binary saved
- [ ] All backups tested and verified
- [ ] Recovery procedure documented

---

## Deployment Environments

### Environment 1: LOCAL (Single Developer Machine)
**Purpose:** Individual testing before team integration  
**Steps:**
1. Build binary: `cargo build --release`
2. Test locally: `cargo test`
3. Verify: No compiler warnings
4. Proceed to TESTNET

### Environment 2: TESTNET (Shared Testing Network)
**Purpose:** Multi-node testing before mainnet  
**Nodes:** 3-5 nodes in shared environment  
**Duration:** 24-48 hours per phase  
**Rollback:** Easy (testnet has no real value)

### Environment 3: MAINNET (Production)
**Purpose:** Live network with real user value  
**Nodes:** All production nodes  
**Duration:** Permanent  
**Rollback:** Only if critical issue discovered

---

## Phase-by-Phase Deployment

### Phase 1: Signature Verification (Week 1)

#### Pre-Deployment (Monday)
```bash
# 1. Pull latest code
git checkout develop
git pull origin develop

# 2. Create release branch
git checkout -b release/phase1-sig-verify

# 3. Verify code
cargo fmt
cargo clippy
cargo build --release
cargo test

# 4. Get approval
# - Code review: 2 developers minimum
# - Security review: 1 security expert
# - QA sign-off: Tests passing

# 5. Tag release
git tag -a phase1-sig-verify-1.0.0 \
  -m "Phase 1: Signature verification implementation"
git push origin phase1-sig-verify-1.0.0
```

#### Testnet Deployment (Wednesday)
```bash
# 1. Stop testnet nodes (graceful shutdown)
systemctl stop timed@testnet-1
systemctl stop timed@testnet-2
systemctl stop timed@testnet-3

# 2. Backup current binaries
cp /usr/local/bin/timed /backup/timed-phase0-$(date +%s)

# 3. Install new binary
cargo build --release
cp target/release/timed /usr/local/bin/timed

# 4. Verify binary
timed --version
sha256sum /usr/local/bin/timed > /backup/binary.sha256

# 5. Start first node (monitor closely)
systemctl start timed@testnet-1
sleep 10
journalctl -u timed@testnet-1 -n 50  # Check logs

# 6. If OK, start remaining nodes
systemctl start timed@testnet-2
sleep 5
systemctl start timed@testnet-3
sleep 5

# 7. Monitor for 1 hour
# Check: Logs for errors
# Check: All nodes connecting
# Check: Consensus working
# Check: No crashes
```

#### Verification
```bash
# Verify signature verification is working
curl http://testnet-1:8332/rpc -d '{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "validatetransaction",
  "params": [{
    "inputs": [...],
    "outputs": [...]
  }]
}'

# Expected: Transaction validated with signature check
# Not expected: Transaction validated without signature check
```

#### Success Criteria
- [ ] All 3 testnet nodes start successfully
- [ ] No errors in logs (signature related)
- [ ] Consensus working (blocks being produced)
- [ ] Signature verification active
- [ ] Monitor for 24 hours without issues

#### Rollback (If Issues)
```bash
# 1. Stop all nodes
systemctl stop timed@testnet-{1,2,3}

# 2. Restore previous binary
cp /backup/timed-phase0-* /usr/local/bin/timed

# 3. Verify restored binary
sha256sum /usr/local/bin/timed  # Should match previous

# 4. Start nodes from previous state
systemctl start timed@testnet-{1,2,3}

# 5. Verify rollback
journalctl -u timed@testnet-1 -n 50
```

---

### Phase 2: Finality & Fork Resolution (Week 2)

#### Pre-Deployment (Monday)
```bash
# Similar to Phase 1 but:
# - Build on Phase 1 code (don't revert)
# - Tag: phase2-finality-fork-resolution-1.0.0
# - Ensure Phase 1 still working on testnet
```

#### Testnet Deployment (Wednesday)
```bash
# Same deployment process as Phase 1
# But Phase 1 must be stable first

# Additional verification for Phase 2:
# 1. Check that finality is working
# 2. Verify fork resolution with 3+ nodes
# 3. Simulate network partition and recovery
```

#### Testing Finality
```bash
# Produce block and check finality
# Block should show: phase: Finalized

# Try to reorg after finalization
# Should fail with: "Block already finalized"
```

---

### Phase 3: Testing & Bugfixes (Week 3)

#### Process
```bash
# 1. Run full stress test suite
cargo test --release test_stress

# 2. Run network partition tests
cargo test --release test_partition

# 3. Fix any bugs found
# 4. Re-test
# 5. Deploy to testnet if all pass
```

#### Deployment
Same as Phase 1, but:
- Tag: phase3-validation-1.0.0
- Verify stress tests work on real network

---

### Phase 4: Monitoring & Hardening (Week 4)

#### Pre-Deployment
```bash
# 1. Verify metrics endpoint
curl http://testnet-1:8332/metrics
# Expected: Prometheus format metrics

# 2. Verify structured logging
journalctl -u timed@testnet-1 -o json | jq .
# Expected: JSON formatted logs
```

#### Deployment
```bash
# Same safe deployment process
# Tag: phase4-production-ready-1.0.0

# After deployment:
# 1. Configure Prometheus scraping
# 2. Create Grafana dashboards
# 3. Set up AlertManager rules
# 4. Test alert triggers
```

---

## Mainnet Deployment (After External Audit)

### Pre-Mainnet Checklist
- [ ] All phases complete on testnet
- [ ] 48+ hour stability run successful
- [ ] External security audit complete
- [ ] All audit findings fixed and re-audited
- [ ] Penetration testing complete
- [ ] No critical issues remaining
- [ ] All masternodes upgraded to same version
- [ ] Backup disaster recovery tested
- [ ] Monitoring fully configured
- [ ] On-call support assigned

### Mainnet Deployment Steps

#### Step 1: Coordination (Date TBD)
```bash
# 1. Announce planned upgrade
#    - 48 hours notice
#    - Scheduled maintenance window
#    - Expected downtime: 30 minutes per node

# 2. Get consensus
#    - 2/3+ masternodes agree to upgrade
#    - All nodes have rollback plan

# 3. Prepare rollback
#    - All nodes backup current binary
#    - Previous version ready to deploy
#    - Rollback plan tested
```

#### Step 2: Rolling Upgrade (One Node at a Time)
```bash
# For each masternode:
for node in mainnet-{1,2,3,4,5}; do
  echo "Upgrading $node..."
  
  # 1. Stop node
  ssh $node "systemctl stop timed"
  
  # 2. Backup current state
  ssh $node "cp -r /data/timecoin /backup/timecoin-$(date +%s)"
  
  # 3. Backup binary
  ssh $node "cp /usr/local/bin/timed /backup/timed-previous"
  
  # 4. Deploy new binary
  scp target/release/timed root@$node:/usr/local/bin/
  
  # 5. Verify permissions
  ssh $node "chmod +x /usr/local/bin/timed"
  
  # 6. Start node
  ssh $node "systemctl start timed"
  
  # 7. Wait for sync
  sleep 30
  
  # 8. Verify connected to network
  ssh $node "curl -s localhost:8332/rpc" | grep -q error || exit 1
  
  # 9. Check logs
  ssh $node "journalctl -u timed -n 50" | grep -i error && exit 1
  
  echo "✓ $node upgraded successfully"
  sleep 60  # Wait before next node
done
```

#### Step 3: Verification
```bash
# After all nodes upgraded:

# 1. Check consensus
curl mainnet-1:8332/rpc -d '{"method":"getblockcount"}'
curl mainnet-2:8332/rpc -d '{"method":"getblockcount"}'
curl mainnet-3:8332/rpc -d '{"method":"getblockcount"}'
# All should show same block height

# 2. Check finality
curl mainnet-1:8332/rpc -d '{"method":"getblockinfo","params":[1]}'
# Should show: "phase": "Finalized"

# 3. Monitor for 24 hours
#    - Check logs: no errors
#    - Check metrics: normal values
#    - Check alerts: no false positives
```

#### Step 4: Completion
```bash
# If all OK:
# 1. Update version in code/docs
# 2. Post deployment announcement
# 3. Update runbooks if needed
# 4. Archive deployment logs

# If issues found:
# Go to ROLLBACK section below
```

---

## Emergency Rollback Procedure

### Trigger Conditions
Rollback if:
- ❌ Network consensus fails (no new blocks for 10 minutes)
- ❌ Critical data corruption detected
- ❌ Security vulnerability discovered in new code
- ❌ More than 20% of nodes crashed
- ❌ Finality not working properly

### Do NOT Rollback If
- ⚠️ Single node crashed (restart node, don't rollback network)
- ⚠️ Temporary network partition (<5 minutes)
- ⚠️ Normal bugs (can fix with patch)

### Rollback Procedure (FAST TRACK)

```bash
# IMMEDIATE: Stop all affected nodes
for node in mainnet-{1..10}; do
  ssh $node "systemctl stop timed" &
done
wait

# Alert all operators
echo "EMERGENCY: Rolling back all mainnet nodes"
# Send notifications to all operators

# Restore from backup (rolling)
for node in mainnet-{1..10}; do
  ssh $node "
    cp /backup/timed-previous /usr/local/bin/timed
    rm -rf /data/timecoin
    cp -r /backup/timecoin-* /data/timecoin/
  "
done

# Start nodes (one at a time)
for node in mainnet-{1..10}; do
  ssh $node "systemctl start timed"
  sleep 30
  # Verify connected
done

# Post-rollback analysis
# 1. Collect logs from all nodes
# 2. Identify root cause
# 3. Fix and re-test
# 4. Plan next upgrade attempt
```

---

## Version Management

### Version Naming
```
phase-<phase-number>-<feature>-<version>.tar.gz
Example: phase1-sig-verify-1.0.0.tar.gz
         phase2-finality-fork-1.0.1.tar.gz (bugfix)
```

### Checksum Verification
```bash
# After building binary
sha256sum target/release/timed > release/timed.sha256
cat release/timed.sha256
# Output: abc123def456... timed

# Before deployment
sha256sum -c release/timed.sha256
# Verify: timed: OK
```

### Release Artifacts
Store in `releases/` directory:
```
releases/
├── phase1-sig-verify-1.0.0/
│   ├── timed (binary)
│   ├── timed.sha256 (checksum)
│   ├── RELEASE_NOTES.md
│   └── INSTALLATION.md
├── phase2-finality-1.0.0/
│   └── ...
└── ...
```

---

## Monitoring During Deployment

### Key Metrics to Watch
```
Consensus Metrics:
- blocks_produced_per_minute (should be 0.17 = 1 block per 10 min)
- consensus_rounds_per_minute (should increase with blocks)
- finality_latency (should be <30 seconds)

Network Metrics:
- active_peers (should be stable, number of nodes - 1)
- message_rate (should be normal, not spiking)
- network_latency (should be <100ms)

Node Metrics:
- memory_usage (should be stable <500MB)
- cpu_usage (should be <50%)
- disk_io (should be normal)

Error Metrics:
- errors_total (should be 0 for critical errors)
- warnings_total (should be 0 for warnings)
- panics (should be 0)
```

### Alert Rules (AlertManager)
```yaml
# Critical alerts that trigger rollback decision
- alert: NoBlocksProduced
  expr: increase(blocks_produced[10m]) == 0
  for: 10m
  action: IMMEDIATE INVESTIGATION

- alert: ConsensusTimeout
  expr: increase(consensus_timeouts[10m]) > 3
  for: 5m
  action: CHECK LOGS, PREPARE ROLLBACK

- alert: CriticalErrors
  expr: increase(errors_total{severity="critical"}[5m]) > 0
  action: PAGE ON-CALL, PREPARE ROLLBACK
```

---

## Rollback Decision Matrix

| Symptom | Severity | Action |
|---------|----------|--------|
| No blocks 10+ min | CRITICAL | ROLLBACK IMMEDIATELY |
| Consensus timeout >3x | CRITICAL | ROLLBACK IMMEDIATELY |
| Data corruption | CRITICAL | ROLLBACK IMMEDIATELY |
| Network partition | HIGH | Monitor, rollback if >30 min |
| Signature failures | HIGH | ROLLBACK (data security risk) |
| Memory leak detected | MEDIUM | Deploy patch, monitor |
| Single node crash | LOW | Restart node, observe |

---

## Post-Deployment Verification

### Immediate (First 5 minutes)
- [ ] All nodes started successfully
- [ ] No startup errors in logs
- [ ] All nodes connecting to peers
- [ ] Consensus producing blocks
- [ ] Finality working

### Short-term (First hour)
- [ ] 6+ blocks produced
- [ ] All blocks finalized
- [ ] No signature verification errors
- [ ] No network partition
- [ ] Metrics reporting properly

### Long-term (First 24 hours)
- [ ] 144+ blocks produced (normal rate)
- [ ] All blocks finalized
- [ ] Zero critical errors
- [ ] Memory stable
- [ ] Peer connections stable
- [ ] Fork resolution working (if tested)

### Extended (First week)
- [ ] No unplanned restarts
- [ ] Performance stable
- [ ] No data corruption
- [ ] All metrics normal
- [ ] User transactions working

---

## Troubleshooting During Deployment

### Problem: Node won't start
```bash
# Check logs
journalctl -u timed -n 100

# Common causes:
# - Binary not executable: chmod +x /usr/local/bin/timed
# - Missing dependencies: check library versions
# - Data corruption: restore from backup
# - Configuration error: verify config syntax
```

### Problem: Nodes can't connect
```bash
# Check network
netstat -tuln | grep 24100  # P2P port
netstat -tuln | grep 24101  # RPC port

# Check firewall
ufw status
ufw allow 24100/tcp
ufw allow 24101/tcp

# Check peers
curl localhost:8332/rpc -d '{"method":"peerinfo"}'
```

### Problem: No consensus/blocks
```bash
# Check blockchain
curl localhost:8332/rpc -d '{"method":"getblockcount"}'

# Check consensus
curl localhost:8332/rpc -d '{"method":"consensusstatus"}'

# Check logs for errors
journalctl -u timed | grep -i error

# Restart if needed
systemctl restart timed
```

---

## Documentation Updates

After each deployment, update:
1. `DEPLOYMENT_LOG.md` - Record what was deployed and when
2. `RUNBOOKS.md` - Update procedures if changed
3. `CHANGELOG.md` - Document what was fixed
4. `VERSION_HISTORY.md` - Track all versions

---

## Handoff to Operations

Before going live, ensure ops team has:
- [ ] Deployment procedures documented
- [ ] Rollback procedures tested
- [ ] On-call runbooks ready
- [ ] Monitoring dashboards configured
- [ ] Alert rules tested
- [ ] Communication procedures defined
- [ ] Escalation contacts documented

---

**Document Version:** 1.0  
**Last Updated:** December 21, 2025  
**Next Review:** Before each deployment  

*This document is the definitive guide for safe deployment of TIME Coin critical fixes.*
