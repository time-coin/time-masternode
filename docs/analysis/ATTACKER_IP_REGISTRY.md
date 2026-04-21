# Attacker IP Registry

**Maintained by:** Network operators  
**Last Updated:** April 19, 2026  
**Purpose:** Track known malicious IPs/subnets observed attacking the TIME mainnet, the attack vectors they used, and the current ban status.

Run `./target/release/time-cli getblacklist` on each node to retrieve the live ban list. This document captures the human-analysed record — the *why* behind each entry.

---

## Confirmed Attacker IPs / Subnets

### 154.217.246.0/24

| Field | Value |
|-------|-------|
| **First Seen** | April 19, 2026 |
| **Last Seen** | April 19, 2026 |
| **Ban Status** | Permanently banned (all nodes) |
| **Hosting** | Unknown VPS provider |

**Attack vectors used:**
- **TLS flood (AV13/AV45)** — Dozens of IPs in this /24 hammered the TLS handshake listener simultaneously, exhausting connection slots
- **Reward-manipulation blocks (AV44)** — Nodes 154.217.246.67, .105, .187, .215 served block 598 with redistributed rewards (`TIME1LeGqigK` inflated from 2 TIME → 14 TIME, `TIME185toC88` zeroed out) to trigger false reward-hijack bans on nodes with stale registries
- **Ghost TX injection (AV41)** — Multiple IPs in range flooded `TransactionFinalized` with forged-signature 0-input/0-output masternode registration TXs
- **Coordinated multi-vector (AV45)** — All three attacks ran simultaneously; reward-manipulation isolated nodes while ghost TXs caused block hash divergence

**Specific IPs confirmed malicious:**

| IP | Role |
|----|------|
| 154.217.246.67 | Reward-manipulation blocks |
| 154.217.246.105 | Reward-manipulation blocks |
| 154.217.246.187 | Reward-manipulation blocks + ghost TXs |
| 154.217.246.215 | Reward-manipulation blocks |
| 154.217.246.236 | TLS flood / connection reset |

---

### 47.79.35.65

| Field | Value |
|-------|-------|
| **First Seen** | April 19, 2026 |
| **Last Seen** | April 19, 2026 |
| **Ban Status** | Permanently banned |
| **Hosting** | Unknown VPS provider |

**Attack vectors used:**
- **Post-handshake oversized frame DoS (AV43)** — Sent 842 MB frames after completing the protocol handshake, disconnecting nodes that were mid-sync
- Connected as inbound peer, completed handshake (commit 2004), then immediately sent oversized frame

---

### 47.79.38.55

| Field | Value |
|-------|-------|
| **First Seen** | April 19, 2026 |
| **Last Seen** | April 19, 2026 |
| **Ban Status** | Permanently banned |
| **Hosting** | Unknown VPS provider (same /16 as 47.79.35.65) |

**Attack vectors used:**
- **Post-handshake oversized frame DoS (AV43)** — Same pattern as 47.79.35.65

---

### 47.82.240.104

| Field | Value |
|-------|-------|
| **First Seen** | April 19, 2026 |
| **Last Seen** | April 19, 2026 |
| **Ban Status** | Permanently banned |
| **Hosting** | Unknown VPS provider (same /16 as 47.82.240.140, 47.82.239.38, 47.82.254.82) |

**Attack vectors used:**
- **Post-handshake oversized frame DoS (AV43)** — Sent 926 MB frames post-handshake
- Largest observed frame size in this attack wave

---

### 47.82.240.140 / 47.82.239.38 / 47.82.254.82

| Field | Value |
|-------|-------|
| **First Seen** | April 19, 2026 |
| **Last Seen** | April 19, 2026 |
| **Ban Status** | Monitored / violations recorded |
| **Hosting** | Same /16 as 47.82.240.104 |

**Attack vectors used:**
- Connected in coordinated bursts alongside the oversized-frame peers
- Genesis-confirmed (compatible), suggesting they may be probing or relaying for the attack cluster

---

### 47.79.37.107

| Field | Value |
|-------|-------|
| **First Seen** | April 19, 2026 |
| **Last Seen** | April 19, 2026 |
| **Ban Status** | Violations recorded |
| **Hosting** | Same /16 as 47.79.35.65 |

**Attack vectors used:**
- **Collateral hijack attempt** — Tried to claim collateral UTXO `b9656b2f...` already anchored on-chain to 69.167.168.176
- Log entry: `🛡️ Collateral hijack rejected: 47.79.37.107 tried to claim b9656b2f...0 already anchored on-chain to 69.167.168.176`

---

### 64.118.152.210

| Field | Value |
|-------|-------|
| **First Seen** | April 18, 2026 (earlier attack wave) |
| **Last Seen** | April 19, 2026 |
| **Ban Status** | Rate-limited / violations recorded |
| **Hosting** | Unknown |

**Attack vectors used:**
- **`tx_finalized` spam (AV40)** — Flooded hundreds of `TransactionFinalized` messages per second, sustained over multiple minutes
- Created significant log noise masking other attack activity
- Registered as a Free-tier masternode on-chain (used legitimate registration to avoid easy dismissal)

---

### 154.64.252.184

| Field | Value |
|-------|-------|
| **First Seen** | April 18, 2026 (earlier attack wave) |
| **Last Seen** | April 19, 2026 |
| **Ban Status** | Rate-limited / violations recorded |
| **Hosting** | Unknown |

**Attack vectors used:**
- **`tx_finalized` spam (AV40)** — Same pattern as 64.118.152.210; likely coordinated
- Also registered as a Free-tier masternode on-chain

---

## Attack Timeline

| Date (UTC) | Event |
|------------|-------|
| Apr 18, 2026 | `tx_finalized` spam begins from 64.118.152.210 and 154.64.252.184 |
| Apr 18, 2026 | Ghost TX injection (Phase 1 — empty fields) observed; AV41 fix deployed |
| Apr 18–19, 2026 | Network fork instability; nodes cycling between minority and canonical chains |
| Apr 19, 03:58 UTC | Coordinated multi-vector attack peak: 154.217.246.0/24 TLS flood + reward-manipulation blocks + ghost TX Phase 2; 47.79.x/47.82.x oversized frame DoS |
| Apr 19, 03:58 UTC | Arizona node banned all whitelisted peers within 5s of startup (AV44 — now fixed) |
| Apr 19, ~04:00 UTC | Fixes deployed: AV41 Phase 2, AV42, AV43, AV44, AV45, AV46 |
| Apr 19, morning | Block production stabilised; ghost TXs purged from pools on restart |

---

## Subnet Clusters

The attacker appears to operate from at least two VPS clusters:

| Cluster | Subnets | Primary role |
|---------|---------|--------------|
| A | 154.217.246.0/24 | Reward-manipulation, TLS flood, ghost TX injection |
| B | 47.79.0.0/16, 47.82.0.0/16 | Oversized frame DoS, collateral hijacking, coordination |
| C | 64.118.152.0/24, 154.64.252.0/24 | tx_finalized spam, noise generation |

Clusters A and B acted simultaneously during the April 19 peak attack, suggesting a single operator or coordinated group.

---

## How to Add a New Entry

When adding a newly-identified attacker IP:

1. Run `./target/release/time-cli getblacklist` and note the ban reason
2. Cross-reference with `journalctl -u timed` to find the specific attack pattern
3. Add an entry above under the appropriate section (individual IP or subnet)
4. Update the Attack Timeline table
5. If a new attack vector is identified, add it to `docs/COMPREHENSIVE_SECURITY_AUDIT.md` Section 15 per the procedure in `CLAUDE.md`

---

## Notes

- Bans persist across restarts (stored in sled DB under the `blacklist` tree)
- The `getblacklist` RPC returns only in-memory state; on fresh restart, bans reload from sled
- Whitelisted IPs are immune to automatic bans — only manual `banpeer` can affect them
- Use `./target/release/time-cli banpeer <ip> <reason>` to manually ban an IP
- Use `./target/release/time-cli clearbanlist` to wipe all bans (use with caution on mainnet)
