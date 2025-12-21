# Local 3-Node Network Test Setup

**Date:** December 20, 2025  
**Purpose:** Validate message handling and connectivity  
**Duration:** 5-10 minutes per node

---

## Quick Start

### Terminal 1: Node 1
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 1 --p2p-port 7000
```

### Terminal 2: Node 2
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 2 --p2p-port 7001
```

### Terminal 3: Node 3
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 3 --p2p-port 7002
```

---

## What to Look For

### Success Indicators ✅

In the logs, look for these patterns:

**1. Ping/Pong Messages (Connection Health)**
```
📤 [OUTBOUND] Sent ping to 127.0.0.1:7001 (nonce: 12345)
📨 [OUTBOUND] Received pong from 127.0.0.1:7001 (nonce: 12345)
✅ [OUTBOUND] Pong matches! 127.0.0.1:7001 (RTT: 45ms)
```

**2. Message Logging (Not Silent Drops)**
```
📨 [OUTBOUND] Received message from 127.0.0.1:7001 (type: BlockAnnouncement)
📨 [OUTBOUND] Received message from 127.0.0.1:7001 (type: TransactionBroadcast)
```

**3. Connection Stability**
```
✓ Connected to peer: 127.0.0.1:7001
🔄 Starting message loop...
```

### Warning Signs ❌

Avoid seeing these:

**1. Ping Timeouts**
```
⚠️ [OUTBOUND] Ping timeout (missed: 1/3)
❌ [OUTBOUND] Peer unresponsive after 3 missed pongs
```

**2. Connection Cycling**
```
🔌 [OUTBOUND] Connection to peer closed
[Reconnecting immediately]
```

**3. Silent Message Drops**
```
[No message logging at all]
```

---

## Detailed Testing Steps

### Step 1: Start Node 1
Open Terminal 1 and run:
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 1 --p2p-port 7000
```

**Wait for:** Initial startup messages, should see database initialization.

### Step 2: Start Node 2
Open Terminal 2 and run:
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 2 --p2p-port 7001
```

**Wait for:** Node 2 to try connecting to Node 1.

**Expected in Node 2 logs:**
```
Attempting to connect to peer...
✓ Connected to peer: 127.0.0.1:7000
📤 [OUTBOUND] Sent ping
```

### Step 3: Start Node 3
Open Terminal 3 and run:
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 3 --p2p-port 7002
```

**Expected in Node 3 logs:**
```
Attempting to connect to peer...
✓ Connected to peer: 127.0.0.1:7000
```

### Step 4: Monitor for 5-10 Minutes
Let all three nodes run and observe:

**In each terminal, you should see:**
1. Ping/pong messages every few seconds
2. Message types being logged (not silent drops)
3. Stable connections (no reconnect cycling)
4. Possible block announcements or transactions

### Step 5: Verification Checklist

Check each node's logs:

**Node 1:**
- [ ] Connected to at least 1 peer
- [ ] Receiving pongs for sent pings
- [ ] Messages logged (not dropped)
- [ ] No error messages

**Node 2:**
- [ ] Connected to at least 1 peer
- [ ] Receiving pongs for sent pings
- [ ] Messages logged (not dropped)
- [ ] No error messages

**Node 3:**
- [ ] Connected to at least 1 peer
- [ ] Receiving pongs for sent pings
- [ ] Messages logged (not dropped)
- [ ] No error messages

---

## Success Criteria

✅ **Test Passes If:**
- All 3 nodes connect to each other
- Ping/pong messages visible in logs (every few seconds)
- No connection cycling or rapid reconnects
- Messages logged with types (not silent drops)
- No error messages

❌ **Test Fails If:**
- Nodes can't connect to each other
- No ping/pong messages visible
- Connection drops repeatedly
- "Peer unresponsive" errors
- Silent message drops (no logging)

---

## Troubleshooting

### Issue: "Port already in use"
**Solution:** Kill existing timed processes or use different ports

### Issue: "Failed to connect"
**Solution:** Ensure nodes are on same machine or network, check firewall

### Issue: "Peer unresponsive"
**Solution:** This indicates the message fix may not be working - check logs carefully

### Issue: "No message logging"
**Solution:** Check that log level is correct, should see debug messages

---

## Recording Results

### To capture logs to file:
On Windows, you can redirect output:

```bash
.\target\release\timed --node-id 1 --p2p-port 7000 > node1.log 2>&1
```

Then analyze:
```bash
# Count ping messages
Select-String "Sent ping" node1.log | Measure-Object -Line

# Check for errors
Select-String "ERROR\|error\|failed" node1.log

# Check for messages being logged
Select-String "Received message" node1.log | Measure-Object -Line
```

---

## Next Steps After Testing

If test **PASSES** ✅:
1. Document results
2. Proceed to single testnet node deployment
3. Start Phase 2 planning

If test **FAILS** ❌:
1. Identify the issue from logs
2. Review code changes
3. Check if issue is in new code or pre-existing
4. Create bug report
5. Fix and retry

---

## Quick Reference

**Binary Location:** `target/release/timed.exe`  
**Start Node N:** `.\target\release\timed --node-id N --p2p-port XXXX`  
**Expected Log Messages:**
- "Sent ping" - Connection health check
- "Received pong" - Response received
- "Received message" - Message logging (NOT silent drop)
- "Connected to peer" - Peer connection established

**Ports Used:**
- Node 1: 7000
- Node 2: 7001
- Node 3: 7002

---

**Test Duration:** 5-10 minutes  
**Effort:** Minimal (just watching logs)  
**Risk:** None (local test only)
