# Masternode Connection Count Investigation

**Date:** 2026-04-27  
**Symptom:** Only 1-2 active masternodes out of 242 registered. Block production halted (requires ≥3 active).

---

## Symptom

Nodes show `1 active / 242 registered` masternodes in every health check. Block production is blocked (consensus requires ≥3 active masternodes in the quorum bitmap). The pattern is consistent across 4 observed nodes (LW-London, LW-Michigan, LW-Arizona, VT-Seoul) all running updated code.

---

## Investigation Path

### Step 1: Connection Limit (Initial Fix — Insufficient)

**Hypothesis:** Nodes were capped at 50 connections, preventing the mesh from forming.

**Changes made:**
- `src/config.rs`: `max_peers` default 50 → 250
- `src/network/connection_manager.rs`: All constants scaled up to 250/500
- `src/network/client.rs`: `.clamp(20, 30)` → `.clamp(20, 100)`
- `src/network/server.rs`: `INBOUND_REDIRECT_THRESHOLD` 70 → 175

**Result:** Connections still dropping to 1-2. The limit only affects our own node; remote nodes running old code had the old limits and were still disconnecting.

---

### Step 2: Registered Masternodes Being Redirected (Fixed)

**Finding:** The inbound redirect check (`cur_inbound > INBOUND_REDIRECT_THRESHOLD`) was sending registered masternodes to other servers instead of keeping them connected.

**Root cause:** `is_whitelisted` only checked operator config whitelist, not the masternode registry.

**Fix:** Added `is_registered_masternode` check in `server.rs` to skip redirect for nodes in the masternode registry.

---

### Step 3: Stale Cleanup Cascade (Fixed)

**Finding:** `cleanup_stale_reports()` had no grace period. Any brief disconnect would immediately mark all masternodes inactive.

**Fix:** Added 120-second grace window using `last_seen_at` timestamp. Also updated `register_masternode` to set `last_seen_at = now` on both activation and reconnection.

---

### Step 4: AV40 + AV3 Cycling Bug (Fixed — Commit 03e950c)

**Finding:** Three-bug chain causing masternodes to disconnect and stay disconnected:

1. **MasternodesResponse AV40 bypass:** The `MasternodesResponse` handler passed `(Free, Some(outpoint))` directly to `register_internal` without deferred-tier recovery, triggering AV40.

2. **AV40 over-broad block:** The AV40 check blocked ALL Free+outpoint registrations, including deferred-tier nodes (nodes whose UTXO exists on-chain but hadn't synced yet, so they temporarily announce as Free).

3. **AV3 cycling:** Deferred-tier nodes had their outpoint stripped → registered as `(Free, None)` → `has_collateral = false` → AV3 30s cooldown set on disconnect → AV3 fired on reconnect → disconnected again.

**Fix:**
- `message_handler.rs` (MasternodesResponse): Strip outpoint for Free-tier in peer exchange (prevent AV40 trigger via gossip relay)
- `masternode_registry.rs` (AV40 check): Allow direct connections with Free+outpoint (deferred-tier state)
- `message_handler.rs` (deferred-tier recovery): Keep outpoint for direct connections when UTXO not found (so `has_collateral = true` prevents AV3)

---

### Step 5: "Frame Too Large" from Old-Code Nodes

**Finding** (from VT-Seoul logs):
```
INFO Connection from 64.91.248.55:49582 ended: Frame too large: 775434286 bytes (max: 8388608)
INFO Connection from 67.225.241.132:41234 ended: Frame too large: 321579753 bytes (max: 8388608)
```

Old-code nodes (commit 2088) connect, complete handshake, then send garbled 321MB–775MB frames. This closes the connection immediately. These nodes subsequently accumulate violations and are permanently blacklisted. **Resolution: requires remote nodes to upgrade.**

---

### Step 6: Root Cause Found — Empty UTXO Set After Restart (Fixed — Commit 22ba621)

**Finding:** This is the primary reason for 1-2 active masternodes across all 4 nodes.

**Root cause:** The UTXO set (`InMemoryUtxoStorage`) is entirely in-memory and starts empty on every daemon restart. When blocks already exist in sled, `add_block()` returns early with "block already exists" and skips `process_block_utxos()`. As a result, the UTXO set remains permanently empty after any restart.

**Impact:** The collateral verification in `handle_masternode_announcement` (line ~3453):
```rust
let still_syncing = our_height < 100;
if !still_syncing {
    match utxo_manager.get_utxo(&outpoint).await {
        Ok(utxo) => { /* verify amount */ }
        Err(_) => {
            warn!("Rejecting Bronze masternode — collateral UTXO not found on-chain");
            return Ok(None);  // Silent rejection — never counted as active
        }
    }
}
```

With height = 1868 at startup, `still_syncing = false` immediately. Every paid-tier announcement (Bronze/Silver/Gold) fails with "collateral UTXO not found on-chain" and is silently rejected. The nodes connect at TCP level but never count as active.

**Why 1-2 persist:** Free-tier masternodes skip the collateral check entirely (`if tier != MasternodeTier::Free`). The 1-2 active masternodes are Free-tier nodes that directly connected. No paid-tier masternodes are ever counted.

**Evidence from logs (VT-Seoul):**
```
WARN [Outbound] Rejecting Bronze masternode from 188.166.243.108 — collateral UTXO not found on-chain
WARN [Outbound] Rejecting Bronze masternode from 188.166.243.108 — collateral UTXO not found on-chain
WARN [Outbound] Rejecting Bronze masternode from 188.166.243.108 — collateral UTXO not found on-chain
```
188.166.243.108 maintained a stable outbound connection (pings/pongs every 30s) but was rejected every announcement cycle.

**Fix applied in `src/main.rs`:**
```rust
// Rebuild in-memory UTXO set from stored blocks on every startup.
if blockchain.get_height() > 0 {
    match blockchain.reindex_utxos().await {
        Ok((blocks, utxos)) => {
            tracing::info!("UTXO reindex complete: {} blocks, {} UTXOs", blocks, utxos);
        }
        Err(e) => { tracing::warn!("UTXO reindex failed: {}", e); }
    }
}
```

Also demoted per-block UTXO log from `info` → `debug` (was ~1868 noisy lines per restart).

---

## Key Code Locations

| File | Line | Purpose |
|------|------|---------|
| `src/main.rs` | ~1126 | UTXO reindex on startup (new fix) |
| `src/blockchain.rs` | 625 | `reindex_utxos()` implementation |
| `src/network/message_handler.rs` | ~3450 | Collateral verification (`still_syncing` guard) |
| `src/network/message_handler.rs` | ~4085 | "collateral UTXO not found" rejection |
| `src/masternode_registry.rs` | ~3186 | `cleanup_stale_reports` `is_directly_connected` check |
| `src/masternode_registry.rs` | ~1435 | `is_active` set on registration |
| `src/network/client.rs` | ~496 | PHASE3-MN reconnection loop |
| `src/ai/adaptive_reconnection.rs` | 447 | AI reconnection cooldown (120s max for masternodes) |

---

## Active Count Logic Flow

For a masternode to be counted as active, ALL of the following must be true:

1. **TCP connection established** (PHASE3-MN loop dials every 30s)
2. **Handshake succeeds** (magic bytes, protocol version ≥2, commit count ≥2062)
3. **Announcement received** (`MasternodeAnnouncementV3/V4`)
4. **Deferred-tier resolved** (Free+outpoint → look up UTXO → upgrade to real tier)
5. **Collateral verified** (`get_utxo()` must succeed — requires UTXO reindex!)
6. **`register_direct()` succeeds** (no AV40, no duplicate collateral conflict)
7. **`is_active = true`** set in registry
8. **`cleanup_stale_reports` preserves active state** (`is_directly_connected` check)

Before the UTXO reindex fix, step 5 failed for every paid-tier node after restart, blocking all of steps 6-8.

---

## Remaining Considerations

1. **AI Reconnection Cooldowns:** After many failed connection attempts (due to the UTXO bug), the AI reconnection database may have accumulated failures for most masternodes. For masternodes, the max cooldown is 120 seconds (vs 24 hours for regular peers), so this clears automatically. The startup pass in PHASE3-MN bypasses AI cooldowns once on restart.

2. **Old-Code Nodes:** ~240 of the 242 registered masternodes are running commit ≤2088. These send garbled frames after handshake and disconnect quickly. Until they upgrade, the network will only have partial connectivity among nodes running commit ≥2090.

3. **Blacklist Accumulation:** Each node's blacklist will accumulate permanent bans for old-code nodes over time (obsolete software, frame too large, etc.). This is correct behavior and doesn't affect updated nodes.

4. **UTXO Reindex Timing:** The reindex must complete before `rebuild_collateral_locks` runs (now enforced by position in startup sequence). For 1868 blocks, this takes a few seconds.

---

## Commits

| Commit | Description |
|--------|-------------|
| `af98dc2` | network: increase max peer connections from 50 to 250 |
| `660b190` | network: fix masternode connection stability — exempt from redirect + grace window |
| `03e950c` | fix: allow deferred-tier nodes to connect without AV3/AV40 cycling |
| `22ba621` | fix: auto-reindex UTXO set on startup to restore collateral verification |
