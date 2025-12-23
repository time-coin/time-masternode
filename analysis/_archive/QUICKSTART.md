# Quick Start Guide - P2P Refactor Continuation

## Current Status
✅ `PeerConnection` is ready to use  
❌ Not integrated into server/client yet

## Next Action (30 seconds to understand)

### What to do:
Replace the message loop in `server.rs` and `client.rs` with calls to `PeerConnection`

### Why:
The old code has separate message handlers for inbound/outbound, causing pongs to get lost. The new unified `PeerConnection` fixes this.

## Quick Integration Guide

### server.rs Changes

**Find:** Line ~200-400 (the big message loop)
```rust
loop {
    tokio::select! {
        result = reader.read_line(&mut buffer) => {
            // 200 lines of message handling
        }
    }
}
```

**Replace with:**
```rust
use crate::network::peer_connection::PeerConnection;

let peer_conn = PeerConnection::new_inbound(stream).await?;
tokio::spawn(async move {
    if let Err(e) = peer_conn.run_message_loop(masternode_registry).await {
        error!("Peer connection error: {}", e);
    }
});
```

### client.rs Changes  

**Find:** Line ~800-1100 (the big message loop)
```rust
loop {
    tokio::select! {
        result = reader.read_line(&mut buffer) => {
            // 200 lines of message handling
        }
    }
}
```

**Replace with:**
```rust
use crate::network::peer_connection::PeerConnection;

let peer_conn = PeerConnection::new_outbound(ip, port).await?;
tokio::spawn(async move {
    if let Err(e) = peer_conn.run_message_loop(masternode_registry).await {
        error!("Peer connection error: {}", e);
    }
});
```

## That's It!

Seriously, that's the whole change. The `PeerConnection` already handles:
- ✅ Ping/Pong (both directions)
- ✅ Timeout detection
- ✅ Connection cleanup
- ✅ Proper logging

## Test Locally

```bash
cargo build --release
./target/release/timed --testnet
# Watch for: "✅ Received pong" after "📤 Sent ping"
```

## Deploy to Testnet

```bash
systemctl stop timed
cp target/release/timed /usr/local/bin/
systemctl start timed  
journalctl -u timed -f
```

## What to Look For

### Good Signs ✅
- `🔄 Starting message loop for <ip>`
- `📤 Sent ping to <ip> (nonce: xxx)`
- `✅ Received pong from <ip> (nonce: xxx)`
- No timeout warnings
- Connections stay open

### Bad Signs ❌
- `⚠️ Ping timeout`
- Connection cycling
- Compile errors (we'll fix them)

## Detailed Instructions

See `SESSION_SUMMARY_2025-12-18.md` for full context and step-by-step guide.

---

**Bottom Line:** Replace ~400 lines of duplicate code with ~5 lines using the unified `PeerConnection`. 🎯
