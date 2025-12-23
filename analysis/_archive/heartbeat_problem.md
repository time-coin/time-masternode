# Heartbeat Attestation System - Problem Analysis

**Date**: December 13, 2024  
**Issue**: Michigan2 node thinks only 1 masternode is active, blocking block generation

---

## The Problem

Michigan2 refuses to create blocks:
```
Dec 13 04:30:00 LW-Michigan2: WARN ⚠️ Skipping block production: only 1 masternodes active (minimum 3 required)
Dec 13 04:40:00 LW-Michigan2: WARN ⚠️ Skipping block production: only 1 masternodes active (minimum 3 required)
```

All other masternodes marked as offline:
```
Dec 13 04:24:34 LW-Michigan2: WARN ⚠️ Masternode 50.28.104.50 marked offline (no heartbeat for 215s)
Dec 13 04:24:34 LW-Michigan2: WARN ⚠️ Masternode 69.167.168.176 marked offline (no heartbeat for 215s)
Dec 13 04:24:34 LW-Michigan2: WARN ⚠️ Masternode 165.84.215.117 marked offline (no heartbeat for 215s)
Dec 13 04:24:34 LW-Michigan2: INFO 📊 Status: Height=1754, Active Masternodes=1
```

**But** Michigan2 HAS persistent connections and exchanges height messages:
```
Dec 13 04:40:51 LW-Michigan2: INFO 📊 Peer 69.167.168.176 has height 1754
Dec 13 04:40:59 LW-Michigan2: INFO 📊 Peer 50.28.104.50 has height 1754
```

---

## Root Cause Analysis

### What Should Happen
1. Every masternode broadcasts `MasternodeAnnouncement` every ~30 seconds
2. Receiving nodes process the announcement and update `last_heartbeat` timestamp
3. Nodes check `last_heartbeat` and mark offline if > 60 seconds old
4. Active masternodes = those with recent heartbeats

### What's Actually Happening
1. ✅ Masternodes ARE sending announcements (we see them in logs)
2. ✅ Michigan2 IS receiving the announcements
3. ❌ Michigan2 is NOT updating `last_heartbeat` correctly
4. ❌ Timeout threshold is inconsistent (29s vs 229s)

### Evidence
**Announcements are being received:**
```
Dec 13 04:22:51 LW-Michigan2: INFO 📨 Received masternode announcement from 64.91.241.10:44204
Dec 13 04:22:51 LW-Michigan2: INFO ✅ Registered masternode 64.91.241.10 (total: 14)
```

**But heartbeat timestamps not updated:**
```
Dec 13 04:24:34 LW-Michigan2: WARN ⚠️ Masternode 64.91.241.10 marked offline (no heartbeat for 229s)
```

**Time gap**: Only 103 seconds between receiving announcement and marking offline!

---

## Code Investigation Needed

### 1. Check `MasternodeAnnouncement` Handling
```rust
// In src/network/peer.rs or wherever announcements are processed
Message::MasternodeAnnouncement { ... } => {
    // Does this call registry.update_heartbeat()?
    // Or does it only call registry.register()?
}
```

**Hypothesis**: `register()` doesn't update `last_heartbeat` for existing masternodes.

### 2. Check Heartbeat Update Logic
```rust
// In src/masternode_registry.rs
pub fn update_heartbeat(&self, address: &str) {
    // Does this actually update the timestamp?
    // Or is it a no-op for some reason?
}
```

### 3. Check Active Masternode Counting
```rust
// Where "Active Masternodes" count is computed
pub fn get_active_count(&self) -> usize {
    let now = current_timestamp();
    self.masternodes.values()
        .filter(|mn| mn.is_active && (now - mn.last_heartbeat) < HEARTBEAT_TIMEOUT)
        .count()
}
```

**Questions:**
- What is `HEARTBEAT_TIMEOUT` set to?
- Is `is_active` being set correctly?
- Is `last_heartbeat` being initialized properly on first registration?

### 4. Check Timeout Values
Logs show two different timeouts:
- `29s` - Short timeout (probably intended)
- `229s` - Long timeout (probably a bug - 200s too long?)

Where is this calculated? Why the inconsistency?

---

## Potential Fixes

### Fix #1: Update Heartbeat on Announcement
```rust
// In announcement handler
Message::MasternodeAnnouncement { masternode, ip, timestamp, .. } => {
    let address = format!("{}:{}", ip, masternode.port);
    
    // BOTH register AND update heartbeat
    self.registry.register_masternode(masternode, reward_address);
    self.registry.update_heartbeat(&address);  // ← ADD THIS
}
```

### Fix #2: Make `register_masternode()` Update Heartbeat
```rust
// In src/masternode_registry.rs
pub fn register_masternode(&self, masternode: Masternode, reward_address: String) {
    let address = format!("{}:{}", masternode.ip, masternode.port);
    let now = current_timestamp();
    
    if let Some(existing) = self.masternodes.get_mut(&address) {
        // Update existing masternode
        existing.last_heartbeat = now;  // ← ADD THIS
        existing.is_active = true;
    } else {
        // Create new masternode
        self.masternodes.insert(address, MasternodeInfo {
            masternode,
            reward_address,
            last_heartbeat: now,
            is_active: true,
            uptime_start: now,
            total_uptime: 0,
        });
    }
}
```

### Fix #3: Use Consistent Timeout
```rust
const HEARTBEAT_INTERVAL: u64 = 30;  // Broadcast every 30s
const HEARTBEAT_TIMEOUT: u64 = 60;   // Mark offline after 60s (2x interval)

pub fn check_heartbeats(&self) {
    let now = current_timestamp();
    for (address, info) in self.masternodes.iter_mut() {
        if info.is_active && (now - info.last_heartbeat) > HEARTBEAT_TIMEOUT {
            warn!("⚠️  Masternode {} marked offline (no heartbeat for {}s)", 
                  address, now - info.last_heartbeat);
            info.is_active = false;
        }
    }
}
```

---

## Testing Plan

1. **Add Debug Logging**
   ```rust
   debug!("📊 Heartbeat update: {} last_seen={} now={} diff={}s", 
          address, last_heartbeat, now, now - last_heartbeat);
   ```

2. **Deploy to Michigan2**
   - See if heartbeat updates are actually being called
   - Check if timestamps are progressing correctly

3. **Verify Active Count**
   - Should see 4 active masternodes (Arizona, London, Michigan, Michigan2)
   - Check logs for "Active Masternodes" count

4. **Test Block Generation**
   - Once 3+ masternodes active, block creation should resume
   - Verify blocks created at 10-minute intervals

---

## Impact on Block Generation

Without fixing heartbeats:
- ❌ Michigan2 refuses to create blocks (only 1 active)
- ❌ Other nodes may also undercount active masternodes
- ❌ BFT consensus cannot work (needs accurate masternode list)
- ❌ Network stuck at current height

With heartbeats fixed:
- ✅ All nodes see 4 active masternodes
- ✅ BFT consensus can elect leader
- ✅ Blocks created on schedule
- ✅ Network progresses normally

---

## Related Issues

1. **Inconsistent Timeout Values**
   - 29s vs 229s in logs
   - Need to standardize

2. **Initial Heartbeat Timestamp**
   - New masternodes may start with `last_heartbeat = 0`
   - Would immediately be marked offline
   - Should initialize to `current_timestamp()` on first registration

3. **Race Condition?**
   - Could there be a race between heartbeat check and update?
   - Need mutex/lock protection

---

## Priority: CRITICAL

This blocks all block generation. Must fix before deploying BFT consensus integration.
