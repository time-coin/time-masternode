# BFT Consensus Deployment Guide

## 🚀 Quick Start

### Prerequisites
- 3+ masternodes running the latest code (commit 13b1554+)
- All nodes on same network (testnet or mainnet)
- Proper NTP time synchronization
- Open P2P ports between nodes

### Deployment Steps

1. **Pull latest code** on all masternode servers:
```bash
cd ~/timecoin
git pull origin main
cargo build --release
```

2. **Restart all masternodes** (one at a time):
```bash
systemctl restart timed
# Or if not using systemd:
pkill timed
./target/release/timed --masternode --config config.toml
```

3. **Verify BFT initialization** in logs:
```bash
tail -f ~/timecoin/data/timed.log | grep BFT
```

Expected output:
```
✓ BFT consensus initialized for masternode
✓ BFT consensus signing key configured
🏆 BFT Round started for height 1234: Leader is US
```

## 📊 Monitoring BFT Consensus

### Key Log Patterns

**Leader Election:**
```
🏆 We are BFT leader for height 1234, proposing block
⏸️  Not BFT leader for height 1234, waiting for proposal
```

**Block Proposal:**
```
📋 Proposing block at height 1234 with 5 transactions
```

**Voting:**
```
🗳️  Voted APPROVE on block proposal at height 1234
📊 Received APPROVE vote from <address> for height 1234
```

**Consensus Reached:**
```
✅ BFT Consensus reached for height 1234: 3/3 votes (quorum: 2)
```

**Block Commit:**
```
✅ Processed 1 BFT-committed block(s)
✅ Adding BFT-committed block 1234 with 5 transactions
```

### Check Consensus Status

**Via RPC:**
```bash
curl -s http://localhost:8332/blockchain/height
```

**Via Logs:**
```bash
grep "BFT Consensus reached" ~/timecoin/data/timed.log | tail -5
```

**Via System:**
```bash
tail -f ~/timecoin/data/timed.log | grep -E "(BFT|leader|vote|consensus)"
```

## 🔍 Troubleshooting

### Issue: "Not BFT leader" on all nodes

**Symptom:** No blocks being produced, all nodes waiting

**Cause:** Leader selection determinism issue or network partition

**Fix:**
1. Check all nodes have same genesis hash:
```bash
curl localhost:8332/blockchain/genesis
```

2. Verify masternode list consistency:
```bash
curl localhost:8332/masternodes/list
```

3. Ensure nodes can reach each other:
```bash
curl localhost:8332/network/peers
```

### Issue: "No signing key available"

**Symptom:** Votes not being signed

**Cause:** BFT signing key not initialized

**Fix:**
1. Ensure running with `--masternode` flag
2. Check masternode registration succeeded:
```bash
grep "Registered masternode" ~/timecoin/data/timed.log
```

3. Verify signing key setup:
```bash
grep "BFT consensus signing key configured" ~/timecoin/data/timed.log
```

### Issue: Consensus not reaching 2/3+ quorum

**Symptom:** Votes collected but no commit

**Cause:** 
- Insufficient active masternodes
- Nodes voting REJECT
- Network message delays

**Fix:**
1. Check active masternode count:
```bash
curl localhost:8332/masternodes/active | jq length
# Must be >= 3
```

2. Check for REJECT votes:
```bash
grep "REJECT" ~/timecoin/data/timed.log | tail -10
```

3. Check vote collection:
```bash
grep "Received.*vote" ~/timecoin/data/timed.log | tail -20
```

### Issue: "Failed to handle BFT message"

**Symptom:** BFT messages being rejected

**Cause:** 
- Invalid signatures
- Wrong block height
- Corrupted message

**Fix:**
1. Check signature verification:
```bash
grep "verify.*signature" ~/timecoin/data/timed.log
```

2. Check block height alignment:
```bash
curl localhost:8332/blockchain/height
```

3. Restart node to resync state

## 📈 Performance Metrics

### Expected Behavior

**Block Production Time:** <5 seconds
- Leader election: <100ms
- Block proposal: <500ms
- Vote collection: <2s
- Block commit: <1s
- Chain inclusion: <1s

**Network Traffic:**
- 1 BlockProposal per block
- N BlockVotes per block (N = masternode count)
- 1 BlockCommit per block
- Total: ~(N+2) messages per block

**CPU Usage:**
- Signature generation: <1ms per operation
- Signature verification: <5ms per vote
- Block validation: <10ms

### Performance Tuning

**Reduce latency:**
- Increase committed block processor frequency (currently 5s)
- Add signature batching
- Optimize vote collection

**Reduce bandwidth:**
- Compress BFT messages
- Batch votes per node
- Only gossip to subset of peers

## 🔐 Security Checklist

- [ ] All masternodes using unique Ed25519 keys
- [ ] Signatures being verified on all votes
- [ ] Block validation happening before vote
- [ ] 2/3+ quorum enforced
- [ ] No single point of failure
- [ ] Network messages rate-limited
- [ ] Invalid votes being rejected
- [ ] Duplicate votes prevented (per round)

## 🐛 Common Pitfalls

1. **Clock Skew**: Nodes must have NTP-synchronized clocks within 30s
   ```bash
   timedatectl status
   ```

2. **Network Partition**: All nodes must reach each other
   ```bash
   ping <other-masternode-ip>
   ```

3. **Port Blocking**: Firewall must allow P2P connections
   ```bash
   sudo ufw allow 8333/tcp
   ```

4. **Insufficient Masternodes**: Need minimum 3 active nodes
   ```bash
   curl localhost:8332/masternodes/active | jq length
   ```

5. **Mixed Versions**: All nodes must run same code version
   ```bash
   ./target/release/timed --version
   ```

## 📞 Support

### Debug Info to Collect

When reporting issues, include:

1. **Node logs** (last 1000 lines):
```bash
tail -1000 ~/timecoin/data/timed.log > debug.log
```

2. **BFT-specific logs**:
```bash
grep -E "(BFT|consensus|vote|leader)" ~/timecoin/data/timed.log > bft-debug.log
```

3. **Masternode status**:
```bash
curl localhost:8332/masternodes/list > masternodes.json
curl localhost:8332/blockchain/height > height.txt
curl localhost:8332/network/peers > peers.json
```

4. **System info**:
```bash
uname -a > sysinfo.txt
timedatectl status >> sysinfo.txt
cargo --version >> sysinfo.txt
git rev-parse HEAD >> sysinfo.txt
```

### Contact

- GitHub Issues: https://github.com/time-coin/timecoin/issues
- Discord: [Your Discord]
- Email: [Your Email]

## 🎯 Success Indicators

BFT is working correctly when you see:

✅ Regular leader rotation (different node each block)
✅ <5s block production time
✅ 100% of blocks reaching consensus
✅ All masternodes voting on proposals
✅ No timeout fallbacks needed
✅ Consistent blockchain across all nodes

## 📚 Additional Resources

- [BFT_INTEGRATION_COMPLETE.md](./BFT_INTEGRATION_COMPLETE.md) - Technical details
- [FORK_RESOLUTION_QUICKREF.md](./FORK_RESOLUTION_QUICKREF.md) - Fork handling
- [CLI_GUIDE.md](./CLI_GUIDE.md) - Command reference
- [docs/INSTANT_FINALITY.md](./docs/INSTANT_FINALITY.md) - Transaction finality

---

**Last Updated**: 2025-12-13
**Version**: 0.1.0 (commit 13b1554)
