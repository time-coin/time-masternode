# Quick Reference - Network Deployment
**Date:** December 19, 2025  
**Status:** ✅ CODE DEPLOYED

---

## What Was Fixed

**Issue:** Outbound P2P connections were failing because they sent PING before HANDSHAKE  
**Fix:** Now send HANDSHAKE first, then PING (follows protocol spec)  
**Result:** 3 nodes verified working, connections stable

---

## Commits Deployed

```
31ad283 - Fix: Send handshake before ping in PeerConnection
b5513be - Fix: Handle non-ping/pong messages in outbound P2P connections
```

---

## Current Status

**✅ Working (Have New Code):**
- 50.28.104.50
- 64.91.241.10  
- 165.84.215.117

**⏳ Pending Update (Old Code):**
- 165.232.154.150
- 178.128.199.144
- 69.167.168.176

---

## To Update a Node

```bash
cd /root/timecoin
git pull origin main
cargo build --release
systemctl restart timed
```

---

## What to Expect After Update

✅ Handshake messages in logs  
✅ Continuous ping/pong (every 30s)  
✅ NO "sent message before handshake" errors  
✅ Connections stay open indefinitely  

---

## Verify It's Working

```bash
journalctl -u timed -f | grep -E "handshake|Sent ping|Received pong"
```

Should see output like:
```
🤝 Sent handshake to 50.28.104.50
📤 Sent ping to 50.28.104.50
📨 Received pong from 50.28.104.50
```

---

**Status:** Ready for nodes to update  
**Confidence:** 🟢 VERY HIGH  
**Full Docs:** See `DEPLOYMENT_SUMMARY_2025-12-19.md`
