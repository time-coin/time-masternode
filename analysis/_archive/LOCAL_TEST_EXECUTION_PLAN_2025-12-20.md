# Local Test Execution Plan

**Date:** December 20, 2025  
**Status:** Ready to execute  
**Duration:** 5-10 minutes of observation  
**Objective:** Validate message handling and connectivity

---

## Test Setup

### Required
- Windows PowerShell or Command Prompt
- 3 terminal windows
- Binary: `target\release\timed.exe` (already built - 11.29 MB)

### Configuration
```
Node 1: --node-id 1 --p2p-port 7000
Node 2: --node-id 2 --p2p-port 7001
Node 3: --node-id 3 --p2p-port 7002
```

---

## Execution Steps

### Phase 1: Node 1 Startup (2 min)

**Command:**
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 1 --p2p-port 7000
```

**Wait for:**
- Database initialization
- Startup messages

**Expected:**
```
[Startup messages]
Node 1 initialized on port 7000
Ready to accept connections
```

**Check:** ✅ No errors

---

### Phase 2: Node 2 Startup (2 min)

**Command (in 2nd terminal):**
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 2 --p2p-port 7001
```

**Wait for:**
- Node 2 connects to Node 1
- Ping/pong messages appear

**Expected:**
```
✓ Connected to peer: 127.0.0.1:7000
📤 [OUTBOUND] Sent ping
📨 [OUTBOUND] Received pong
✅ [OUTBOUND] Pong matches
```

**Check:** 
- [ ] Connection established
- [ ] Pings visible in logs
- [ ] Pongs received

---

### Phase 3: Node 3 Startup (2 min)

**Command (in 3rd terminal):**
```bash
cd C:\Users\wmcor\projects\timecoin
.\target\release\timed --node-id 3 --p2p-port 7002
```

**Wait for:**
- Node 3 connects to Node 1
- Ping/pong messages appear

**Expected:**
```
✓ Connected to peer: 127.0.0.1:7000
📤 [OUTBOUND] Sent ping
📨 [OUTBOUND] Received pong
```

**Check:**
- [ ] Connection established
- [ ] Pings visible in logs
- [ ] Pongs received

---

### Phase 4: Observation Period (5 min)

**Monitor all 3 terminals for:**

#### Connection Health
- [ ] No "Peer unresponsive" messages
- [ ] No "Ping timeout" messages
- [ ] No rapid reconnects
- [ ] Consistent peer count

#### Message Logging
- [ ] Messages logged with types
- [ ] Not silently dropping messages
- [ ] Debug output visible
- [ ] No "silent drop" pattern

#### Network Stability
- [ ] Connections staying open
- [ ] Regular ping/pong messages
- [ ] No error messages
- [ ] Smooth operation

#### Performance
- [ ] CPU usage reasonable
- [ ] Memory stable
- [ ] No lag in logging
- [ ] No resource exhaustion

---

## Success Criteria

### Minimum Success ✅
All of these must pass:
- [ ] All 3 nodes start successfully
- [ ] Nodes establish connections to each other
- [ ] Ping/pong messages visible in logs
- [ ] No error messages
- [ ] No connection cycling

### Ideal Success 🌟
Plus these would be good to see:
- [ ] Message types logged (TransactionBroadcast, BlockAnnouncement, etc.)
- [ ] Multiple peers connected per node
- [ ] Consistent metrics
- [ ] Clean shutdown

### Failure Indicators ❌
Stop test if you see:
- Nodes can't connect to each other
- "Peer unresponsive" messages
- Connection drops every few seconds
- Silent message drops (no logging)
- Error stacktraces

---

## Detailed Observation Checklist

### Node 1 Logs
```
✓ Database initialized
✓ Listening on port 7000
✓ Accepting inbound connections
✓ Peer discovery working
✓ Message loop stable
✓ No errors in 5 minutes
```

### Node 2 Logs
```
✓ Connects to 127.0.0.1:7000
✓ Sends ping messages
✓ Receives pong responses
✓ Pongs match sent pings
✓ Message loop running
✓ No connection cycling
```

### Node 3 Logs
```
✓ Connects to 127.0.0.1:7000 (and possibly 7001)
✓ Sends ping messages
✓ Receives pong responses
✓ Pongs match sent pings
✓ Message loop running
✓ No connection cycling
```

### Cross-Node Verification
```
✓ Node 1 shows incoming connections from 2 and 3
✓ Node 2 shows outbound connection to 1
✓ Node 3 shows outbound connection to 1
✓ All nodes show regular ping/pong exchanges
✓ Message types are logged (not silent drops)
```

---

## Log Analysis

### Key Patterns to Search For

**Success Patterns:**
```
"Sent ping"              → Connection health check working
"Received pong"          → Receiving responses
"Pong matches"          → Nonce matching working
"Received message"      → NOT silently dropping messages
"Connected to peer"     → Connectivity working
```

**Error Patterns (Avoid):**
```
"Peer unresponsive"     → Ping timeout issue
"Ping timeout"          → Connection problem
"Failed to connect"     → Network issue
"Connection closed"     → Unexpected disconnect
[Silent - no logging]   → Silent message drop (BUG)
```

---

## Metrics to Record

After 5 minutes, collect:

```
Node 1:
  - Connected peers: ___
  - Ping messages sent: ___
  - Pong messages received: ___
  - Other messages: ___
  - Errors: ___

Node 2:
  - Connected peers: ___
  - Ping messages sent: ___
  - Pong messages received: ___
  - Other messages: ___
  - Errors: ___

Node 3:
  - Connected peers: ___
  - Ping messages sent: ___
  - Pong messages received: ___
  - Other messages: ___
  - Errors: ___
```

---

## Stopping the Test

### Graceful Shutdown
In each terminal, press `Ctrl+C` to stop

### Expected Shutdown Messages
```
Shutting down...
Closing connections...
Goodbye
```

### Cleanup
- [ ] All nodes stopped
- [ ] No orphaned processes
- [ ] Logs saved (if capturing)

---

## What to Do Based on Results

### If Test PASSES ✅
1. Document the results
2. Create `LOCAL_TEST_RESULTS_PASSED_2025-12-20.md`
3. Proceed to testnet deployment
4. Note any observations for Phase 2

### If Test FAILS ❌
1. Document what failed
2. Create `LOCAL_TEST_RESULTS_FAILED_2025-12-20.md`
3. Analyze logs for root cause
4. Identify if issue is:
   - In new code (message logging)
   - In RPC changes
   - Pre-existing issue
5. Create bug report
6. Plan fix

---

## Estimated Timeline

| Phase | Time | Task |
|-------|------|------|
| 1 | 2 min | Start Node 1, verify startup |
| 2 | 2 min | Start Node 2, verify connection |
| 3 | 2 min | Start Node 3, verify connection |
| 4 | 5 min | Observe all nodes |
| 5 | 2 min | Verify results and shutdown |
| **Total** | **~13 min** | Complete test |

---

## Equipment Checklist

Before starting:
- [ ] 3 terminal windows open
- [ ] Binary location verified
- [ ] Ports 7000-7002 available
- [ ] Current directory set to project root
- [ ] No other timed instances running

---

## Command Reference

### Start Nodes
```powershell
# Terminal 1 - Node 1
.\target\release\timed --node-id 1 --p2p-port 7000

# Terminal 2 - Node 2
.\target\release\timed --node-id 2 --p2p-port 7001

# Terminal 3 - Node 3
.\target\release\timed --node-id 3 --p2p-port 7002
```

### Monitor Processes
```powershell
Get-Process timed
```

### Kill All Nodes (if needed)
```powershell
Stop-Process -Name timed
```

### Check Ports
```powershell
netstat -ano | findstr "7000\|7001\|7002"
```

---

## Document Template

After test, create a results document with:
- Test date and time
- Duration
- Nodes started
- Connections established
- Key observations
- Metrics collected
- Success/Failure status
- Next steps

---

**Test Status:** Ready to execute  
**Confidence:** High (infrastructure tested)  
**Expected Duration:** ~13 minutes  
**Risk Level:** None (local test only)
