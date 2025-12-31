# Connection Diagnosis - December 18, 2025

## Problem Summary
Connections are cycling every ~90 seconds and nodes can't maintain stable connections

## Observed Behavior

### Symptoms
1. Inbound connections receive pings and send pongs ✅
2. Outbound connections send pings but never receive pongs ❌
3. Outbound connections timeout after 3 missed pongs (90 seconds)
4. Connections close and immediately reconnect in endless cycle

### Logs Analysis

**INBOUND (Server Side) - WORKS:**
```
📨 [INBOUND] Received ping from 50.28.104.50 (nonce: 3024847735846446637), sending pong
✅ [INBOUND] Sent pong to 50.28.104.50 (nonce: 3024847735846446637)
```

**OUTBOUND (Client Side) - BROKEN:**
```
📤 Sent ping to 178.128.199.144 (nonce: 11900434792055248454)
⚠️ Ping timeout from 178.128.199.144 (nonce: 11900434792055248454, missed: 1/3)
⚠️ Ping timeout from 178.128.199.144 (nonce: 6523960411979597777, missed: 2/3)
⚠️ Ping timeout from 178.128.199.144 (nonce: 17186102266762545919, missed: 3/3)
❌ Peer 178.128.199.144 unresponsive after 3 missed pongs, disconnecting
```

**NEVER SEEN:**
- `📨 [OUTBOUND] Received ping from ...`
- `📨 [OUTBOUND] Received pong from ...`

## Root Cause Analysis

The outbound client connection message loop is NOT receiving ANY messages from the remote peer:
- No pings received
- No pongs received  
- No block responses
- Nothing

### Why This Happens

Looking at the architecture:
1. **Client connects outbound** to Server
2. **Server accepts inbound** connection  
3. **Both register their writers** in PeerConnectionRegistry
4. **Client sends ping** via PeerConnectionRegistry → goes to Server's reader ✅
5. **Server receives ping** in its message loop ✅
6. **Server sends pong** via PeerConnectionRegistry → should go to Client's reader ❌
7. **Client's reader never receives the pong** ❌

### Hypothesis

The issue is likely one of:

1. **Writer/Reader Mismatch**: The PeerConnectionRegistry writer registered by the Server might not be connected to the Client's TCP reader
2. **Bidirectional Channel Issue**: TCP is bidirectional but maybe we're only reading from one direction
3. **Buffering**: Messages are being buffered and not flushed

## Additional Clues

1. Nodes can complete handshakes successfully
2. Inbound connections work perfectly (server side)
3. Masternode announcements work (seen in logs)
4. Block sync requests work initially
5. **Only ping/pong mechanism is completely broken for outbound**

## Why Connections Cycle

1. Both peers try to connect outbound (race condition)
2. Server with higher IP closes inbound: `break; // Close connection gracefully`
3. Client's outbound dies from ping timeout after 90s
4. Both reconnect
5. Repeat forever

## Solution Needed

The immediate fix is to make outbound connections receive messages properly. Once that works:
- Pings will be received
- Pongs will be sent back
- Connections will stay alive
- No more cycling

## Test Plan

Add logging to:
1. Every message sent via PeerConnectionRegistry
2. Every line read from TCP reader (both inbound and outbound)
3. Writer registration and mapping
4. Verify bidirectional flow

## Status: Investigating
Need to add detailed logging to trace message flow through PeerConnectionRegistry and verify TCP reader/writer pairing.
