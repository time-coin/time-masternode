# Masternode Connectivity Fix - Quick Deployment Guide

## Pre-Deployment

- [ ] Read `MASTERNODE_FIX_IMPLEMENTATION.md`
- [ ] Read `MASTERNODE_CONNECTIVITY_FIX.md` (detailed analysis)
- [ ] Backup current binary: `cp target/release/timed target/release/timed.backup`
- [ ] Review current config: `cat config.toml`

## Optional: Add Manual Whitelist

Edit `config.toml`:
```toml
[network]
whitelisted_peers = [
    "104.194.10.48",
    "104.194.10.49",
]
```

## Build & Deploy

```bash
# 1. Build new binary
cargo build --release

# 2. Stop node
systemctl stop timed

# 3. Binary is already in place from build

# 4. Start node
systemctl start timed

# 5. Watch logs for 5 minutes
journalctl -u timed -f
```

## What to Look For in Logs

### ✅ Good Signs:
```
🔐 Preparing whitelist with X trusted peer(s)...
✅ Whitelisted peer before server start: 104.194.10.48
✅ [WHITELIST] Accepting inbound connection from 104.194.10.48
```

### ⚠️ Expected Warnings (OK):
```
⚠️ [WHITELIST] Whitelisted peer X has 3 missed pongs - resetting counter but keeping connection
```
This means the whitelist protection is working!

### ❌ Bad Signs (Rollback):
```
❌ Disconnecting X.X.X.X due to timeout
(where X.X.X.X is a whitelisted masternode)
```

## Quick Health Check

```bash
# 1. Verify whitelisting happened
journalctl -u timed --since boot | grep "Whitelisted peer before server start"

# 2. Check connection stats (last 10 connections)
journalctl -u timed --since "10 minutes ago" | grep "Accepting inbound"

# 3. Watch for disconnections
journalctl -u timed --since "10 minutes ago" | grep "Disconnecting"

# 4. Check blockchain height (should match other nodes)
journalctl -u timed --since "1 minute ago" | grep "Height:"
```

## Rollback If Needed

```bash
systemctl stop timed
cp target/release/timed.backup target/release/timed
systemctl start timed
```

## Success Criteria

After 1 hour:
- [ ] No whitelisted masternode disconnections
- [ ] Height matches network (within 10 blocks)
- [ ] Regular peers still connecting normally
- [ ] No unusual errors in logs

After 24 hours:
- [ ] Zero masternode disconnections
- [ ] Height variance < 10 blocks
- [ ] No fork detections

## Changes Made

**Phase 1**: Whitelist populated BEFORE server starts (race condition fix)
**Phase 2**: Whitelisted peers never disconnect on timeout (already implemented)  
**Phase 3**: Reserved 50 connection slots for masternodes (priority access)

## Files Changed

- `src/network/server.rs` - Accept whitelist in constructor
- `src/main.rs` - Populate whitelist before server creation
- `src/network/connection_manager.rs` - Slot reservation logic

**Total**: 112 lines across 3 files
**Risk**: LOW (backward compatible, no protocol changes)

## Support

- Full details: `MASTERNODE_FIX_IMPLEMENTATION.md`
- Analysis: `MASTERNODE_CONNECTIVITY_FIX.md`
- Issues: Check logs with `journalctl -u timed -f`
