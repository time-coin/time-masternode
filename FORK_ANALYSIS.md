# Analysis: Why Are There So Many Forks?

## Current Situation

Your mainnet has been experiencing **frequent forks** with nodes at different heights unable to sync. While the fix I just pushed will help nodes *recover* from forks, we need to understand **why forks are happening in the first place**.

## Root Causes of Frequent Forks

### 1. **Race Condition in Leader Selection** ⚠️ CRITICAL

**The Problem:**
- Block production happens **every 10 minutes** (600 seconds)
- Leader selection is based on: `SHA256(prev_block_hash + current_height)`
- However, during catchup/fork situations, **different nodes may have different `prev_block_hash` values**

**Example Fork Scenario:**
```
Time T=0:
- Node A is at height 5900 with hash `aaa...`
- Node B is at height 5900 with hash `bbb...` (different fork!)

Time T=600 (next block time):
- Node A: leader = SHA256(aaa... + 5901) → selects Leader X
- Node B: leader = SHA256(bbb... + 5901) → selects Leader Y
- **Both leaders produce blocks at height 5901** → FORK!
```

### 2. **Network Partition / Connectivity Issues**

Looking at your logs showing nodes at heights 5900, 5910, 5912, 5913:
- This suggests **4 different chains** coexisting
- Each node is isolated and producing its own chain
- This indicates **network connectivity problems** between masternodes

**Possible Causes:**
- Firewall blocking peer connections
- Network latency/packet loss
- Nodes behind NAT without proper port forwarding
- Peer discovery not working correctly
- Whitelist/masternode list out of sync

### 3. **Clock Skew Between Nodes** ⏰

**The Block Schedule:**
- Blocks should be produced at: `genesis_time + (height * 600)`
- Example: Height 5900 should be at timestamp `1767225600 + (5900 * 600) = 1770765600`

**The Problem:**
If nodes have **different system clocks**:
```
Node A clock: 2026-01-11 00:00:00 UTC → thinks it's time for block 5900
Node B clock: 2026-01-11 00:10:00 UTC → thinks it's time for block 5901
```

Both produce blocks at different heights → FORK!

### 4. **Multiple Nodes Think They're the Leader**

From your catchup logic (line 347-480 in tsdc.rs):
- During catchup, leader selection uses: `SHA256(b"catchup_leader_selection" + target_height + attempt)`
- **CRITICAL**: If nodes disagree on what `target_height` is (due to being on different forks), they'll select different leaders!

### 5. **Insufficient Sync Before Block Production**

Looking at the safeguards (main.rs:1418):
```rust
if blocks_behind > 10 {
    tracing::warn!("Skipping normal block production: {} blocks behind. Must sync first.");
    continue;
}
```

**The Problem:**
- Nodes only skip production if **>10 blocks behind**
- But if a node is **2-9 blocks behind** on a **different fork**, it will still produce blocks
- This creates competing chains

## Diagnostic Questions

To determine the exact cause, please check:

### 1. **Network Connectivity**
```bash
# On each masternode, check connections:
time-cli peer list

# Expected: Should see 4+ other masternodes connected
# If <3 peers: Network connectivity issue

# Test connectivity to specific masternodes:
# Mainnet uses port 24000, Testnet uses port 24100
nc -zv <masternode_ip> 24000  # For mainnet
# OR
nc -zv <masternode_ip> 24100  # For testnet
```

### 2. **Clock Synchronization**
```bash
# On each masternode:
timedatectl status

# Check "System clock synchronized: yes"
# Check time difference between nodes (should be <5 seconds)
```

### 3. **Current Heights and Hashes**
```bash
# On each masternode:
time-cli get-block-count
time-cli get-block-hash $(time-cli get-block-count)

# Compare: Do nodes at same height have same hash?
# If NO: They're on different forks
```

### 4. **Logs - Leader Selection**
```bash
# Check if multiple nodes think they're the leader:
grep "selected producer" /var/log/timed/timed.log | tail -20

# Look for: Multiple nodes claiming to be producer for same height
```

## Recommended Fixes

### **FIX A: Improve Leader Selection Determinism** 🔥 CRITICAL

The current leader selection has a **fundamental flaw** during forks:

**Current Code** (blockchain.rs:1392-1409):
```rust
// Use deterministic leader selection based on previous block hash
let mut hasher = Sha256::new();
hasher.update(prev_block_hash);  // ❌ DIFFERENT ON EACH FORK!
hasher.update(current_height.to_le_bytes());
```

**The Fix:**
Use **ONLY height** for leader selection during normal production, OR use a **canonical chain reference**:

```rust
// Option 1: Height-only leader selection
let mut hasher = Sha256::new();
hasher.update(b"leader_selection_v2");
hasher.update(current_height.to_le_bytes());
hasher.update(genesis_hash); // Same for all nodes
let selection_hash: [u8; 32] = hasher.finalize().into();
```

This ensures **all nodes agree on the leader** even if temporarily on different forks.

### **FIX B: Require Stricter Sync Before Production**

**Current**: Skip production if >10 blocks behind  
**Better**: Skip production if ANY disagreement with peers

```rust
// Before producing, check peer consensus:
let peer_tips = peer_registry.get_all_peer_chain_tips().await;
let our_hash = blockchain.get_block_hash(current_height)?;

// Count how many peers agree with our chain
let peers_on_our_chain = peer_tips.iter()
    .filter(|(h, hash)| *h == current_height && *hash == our_hash)
    .count();

// Only produce if we're on the majority chain
if peers_on_our_chain < peer_tips.len() / 2 {
    tracing::warn!("Skipping block production: we're on minority fork");
    continue;
}
```

### **FIX C: Enforce Clock Sync**

Add a check before block production:

```rust
// Query peer timestamps and compare
let peer_times: Vec<i64> = get_peer_timestamps().await;
let median_peer_time = median(peer_times);
let our_time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

if (our_time - median_peer_time).abs() > 30 {
    tracing::error!(
        "Clock skew detected: our time {} vs peer median {} ({}s difference)",
        our_time, median_peer_time, our_time - median_peer_time
    );
    return Err("Clock out of sync - refusing block production");
}
```

### **FIX D: Increase Peer Connectivity Requirements**

**Current**: Minimum 2 connected peers  
**Better**: Minimum 3 connected peers (majority in 5-node network)

```rust
// In mainnet with 5 masternodes, require at least 3 connected
let min_peers = if masternodes.len() >= 5 { 3 } else { 2 };

if connected_peers.len() < min_peers {
    tracing::warn!(
        "Skipping block production: only {} connected peers (need {})",
        connected_peers.len(), min_peers
    );
    continue;
}
```

## Immediate Actions

### 1. **Deploy Fork Resolution Fix** (Already Done ✅)
- This helps nodes recover from existing forks
- **Deploy immediately** to break the current deadlock

### 2. **Check Network Connectivity** 🔴 URGENT
```bash
# On each masternode:
netstat -an | grep :8333  # or your P2P port
iptables -L -n | grep 8333  # Check firewall
ping <other_masternode_ips>
```

### 3. **Verify Clock Sync** 🔴 URGENT
```bash
# On each masternode:
timedatectl
ntpq -p  # If using NTP
chronyc sources  # If using Chrony

# If clocks are out of sync:
systemctl restart systemd-timesyncd
# OR
ntpdate pool.ntp.org
```

### 4. **Manual Fork Resolution** (If needed)
If forks persist after deploying the fix:

```bash
# 1. Determine canonical chain (highest height with most nodes)
# 2. On minority nodes, manually rollback:
time-cli rollback --height 5900  # Or whatever the consensus height is
# 3. Restart node - it will sync from peers
systemctl restart timed
```

## Long-Term Solutions

1. **Deploy FIX A (Deterministic Leader Selection)** - Prevents forks from starting
2. **Deploy FIX B (Stricter Sync Requirements)** - Prevents minority nodes from producing
3. **Deploy FIX C (Clock Sync Enforcement)** - Ensures time alignment
4. **Deploy FIX D (Higher Peer Requirements)** - Ensures connectivity
5. **Add Network Health Monitoring** - Alert on fork detection
6. **Implement Peer Discovery Improvements** - Auto-reconnect to other masternodes

## Priority Order

1. 🔴 **IMMEDIATE**: Deploy fork resolution fix (done ✅)
2. 🔴 **IMMEDIATE**: Check network connectivity between masternodes
3. 🔴 **IMMEDIATE**: Verify clock synchronization (NTP)
4. 🟡 **HIGH**: Deploy deterministic leader selection fix (FIX A)
5. 🟡 **HIGH**: Increase sync strictness (FIX B)
6. 🟢 **MEDIUM**: Clock sync enforcement (FIX C)
7. 🟢 **MEDIUM**: Peer requirement increase (FIX D)

---

**Bottom Line**: The frequent forks are likely caused by **network partitions** (nodes can't reach each other) combined with **non-deterministic leader selection during forks**. The fork resolution fix will help recovery, but fixing leader selection and ensuring connectivity will **prevent forks from happening**.

Let me know the results of the connectivity and clock checks, and I can implement the appropriate fixes!
