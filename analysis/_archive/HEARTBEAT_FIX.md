# Heartbeat Connection Issue - FIXED

## Problem Identified

**Root Cause:** The heartbeat system was creating NEW TCP connections for each heartbeat broadcast instead of using existing persistent peer connections.

### Evidence from Logs

Arizona logs showed:
```
Dec 13 06:15:44 LW-Arizona timed[19512]:  INFO 🔌 New peer connection from: 64.91.241.10:46882
Dec 13 06:15:44 LW-Arizona timed[19512]:  INFO ✅ Handshake accepted from 64.91.241.10:46882
Dec 13 06:15:44 LW-Arizona timed[19512]:  INFO 🔌 Peer 64.91.241.10:46882 disconnected (EOF)
```

Every 10 seconds, a new connection from Michigan2 would:
1. Connect
2. Handshake  
3. Immediately disconnect

Michigan2 logs showed NO inbound connections being logged - confirming it was the SENDER creating these short-lived connections.

### The Broken Code

In `src/masternode_registry.rs`, the `broadcast_heartbeat()` function had two problems:

```rust
pub async fn broadcast_heartbeat(&self, heartbeat: SignedHeartbeat) {
    // 1. Used broadcast channel (GOOD)
    if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
        let _ = tx.send(msg);
    }

    // 2. THEN created NEW connections to each peer (BAD!)
    for mn in masternodes {
        let addr = format!("{}:{}", mn.address, 24100);
        Self::send_message_to_peer(&addr, msg).await; // Creates new TCP connection!
    }
}
```

The `send_message_to_peer()` function would:
1. Create a NEW TcpStream connection
2. Send handshake
3. Send heartbeat message
4. Close connection immediately

This happened **every 10 seconds for every masternode**, creating hundreds of short-lived connections per minute.

## The Fix

**Solution:** Use ONLY the broadcast channel which sends to all existing persistent connections.

### Simplified Code

```rust
pub async fn broadcast_heartbeat(&self, heartbeat: SignedHeartbeat) {
    use crate::network::message::NetworkMessage;

    // Use broadcast channel to send to all connected peers
    if let Some(tx) = self.broadcast_tx.read().await.as_ref() {
        let msg = NetworkMessage::HeartbeatBroadcast(heartbeat.clone());
        match tx.send(msg) {
            Ok(receiver_count) => {
                if receiver_count > 0 {
                    tracing::debug!("📡 Broadcast heartbeat to {} peer(s)", receiver_count);
                }
            }
            Err(_) => {
                tracing::trace!("No peers connected to receive heartbeat");
            }
        }
    }
}
```

### What Was Removed

1. **Removed** the loop that created new connections to each masternode
2. **Deleted** the `send_message_to_peer()` function entirely (lines 462-505)
3. **Simplified** to use only the broadcast channel

## How It Works Now

1. **Persistent Connections**: Nodes maintain persistent TCP connections to peers
2. **Broadcast Channel**: Each connection subscribes to the broadcast channel
3. **Heartbeat Broadcast**: Every 10 seconds, heartbeat is sent via broadcast channel
4. **Single Send**: One broadcast operation sends to ALL connected peers simultaneously
5. **No New Connections**: Zero new connections created for heartbeats

## Expected Behavior

After deploying this fix:

1. **Stable Connections**: Peers will maintain persistent connections
2. **No Connection Spam**: No more "New peer connection" / "disconnected (EOF)" messages
3. **Active Masternodes**: Michigan2 should see all 14 masternodes as active
4. **Heartbeat Visibility**: All nodes will receive heartbeats over existing connections
5. **Block Production**: With 14 active masternodes, blocks will be produced normally

## Testing

Deploy to testnet and verify:

```bash
# Should show persistent connections, not constant new connections
sudo journalctl -u timed -f | grep "peer connection"

# Should show active masternodes count
sudo journalctl -u timed -f | grep "Active Masternodes"
```

Expected: Masternodes=14, no connection spam, blocks being produced.

## Files Changed

- `src/masternode_registry.rs`:
  - Simplified `broadcast_heartbeat()` (lines 400-418)
  - Removed `send_message_to_peer()` (lines 462-505)

## Deployment

```bash
# Build
cargo build --release

# Copy to servers
scp target/release/timed root@server:/usr/local/bin/

# Restart service
sudo systemctl restart timed
```

---

**Status:** ✅ FIXED - Ready for deployment
**Date:** 2025-12-13
**Issue:** Heartbeat system creating connection spam
**Solution:** Use only broadcast channel with persistent connections
