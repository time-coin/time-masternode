# 🏆 TIME Coin Masternode Guide

## Overview

TIME Coin supports tiered masternodes with locked collateral (Dash-style). Configuration uses two files: `time.conf` (daemon settings and private key) and `masternode.conf` (collateral info). The daemon handles registration on startup.

> **First time?** See **[LINUX_INSTALLATION.md](LINUX_INSTALLATION.md)** for
> the step-by-step installation and setup guide. This document covers
> masternode **operations** — tiers, collateral, rewards, and management.

---

## 🚀 Quick Start

### Free Tier (No Collateral)

Set `masternode=1` in `time.conf` and start the daemon — that's it. The node
begins earning rewards immediately.

See **[LINUX_INSTALLATION.md](LINUX_INSTALLATION.md)** for full installation
steps.

### Staked Tier (Bronze/Silver/Gold)

See **[LINUX_INSTALLATION.md §5.3](LINUX_INSTALLATION.md#53-staked-tiers-bronze--silver--gold)**
for the step-by-step collateral setup process.

---

## 📊 Collateral Lock Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                    MASTERNODE SETUP FLOW                        │
└─────────────────────────────────────────────────────────────────┘

1. PREPARE FUNDS                    2. CREATE UTXO
   ┌──────────────┐                    ┌──────────────┐
   │ Total: 1501  │                    │ Total: 1501  │
   │ Locked: 0    │ ──sendtoaddress──> │ Locked: 0    │
   │ Avail: 1501  │   (1000 + fee)     │ Avail: 1501  │
   └──────────────┘                    └──────────────┘
                                              │
                                       Wait 3 blocks
                                              │
3. WAIT CONFIRMATIONS                         ▼
   ┌─────────────────────────┐       ┌──────────────┐
   │ UTXO Ready              │       │ Confirmations│
   │ txid: abc123...         │       │     = 3      │
   │ vout: 0                 │       └──────────────┘
   │ amount: 1000 TIME       │
   └─────────────────────────┘
            │
            │ Edit time.conf + masternode.conf
            │ Restart daemon
            ▼
4. LOCK COLLATERAL                   5. MASTERNODE ACTIVE
   ┌──────────────┐                    ┌────────────────┐
   │ Total: 1500  │                    │ Status: Active │
   │ Locked: 1000 │ ───────────────>   │ Tier: Bronze   │
   │ Avail: 500   │                    │ 🔒 Locked      │
   └──────────────┘                    └────────────────┘
                                              │
                                       Earning Rewards
                                              │
                                              ▼
6. RECEIVE REWARDS                     7. DEREGISTER (OPTIONAL)
   ┌──────────────┐                    ┌──────────────┐
   │ Total: 2500  │  Set enabled=false │ Total: 2500  │
   │ Locked: 1000 │  Restart daemon    │ Locked: 0    │
   │ Avail: 1500  │ ───────────────>   │ Avail: 2500  │
   └──────────────┘                    └──────────────┘
```

---

## Masternode Tiers

TIME Coin has four masternode tiers with different collateral requirements and dedicated reward pools:

| Tier | Collateral | Pool Allocation | Governance | Sampling Weight |
|------|-----------|-----------------|------------|-----------------|
| **Free** | 0 TIME | 8 TIME/block | None | 1x |
| **Bronze** | 1,000 TIME (exact) | 14 TIME/block | 1 vote | 10x |
| **Silver** | 10,000 TIME (exact) | 18 TIME/block | 10 votes | 100x |
| **Gold** | 100,000 TIME (exact) | 25 TIME/block | 100 votes | 1000x |

### Tier Benefits

- **Pool Allocation**: Each tier has a dedicated reward pool shared equally among active nodes in that tier (max 25 per block, fairness rotation for overflow)
- **Voting Power**: Weight in governance decisions
- **Sampling Weight**: Probability of being selected for consensus voting and VRF block production

---

## Configuration

Masternode configuration uses two files:
- **`time.conf`** — daemon settings and masternode private key
- **`masternode.conf`** — collateral info (alias, IP, txid, vout)

### time.conf Settings

```
masternode=1
masternodeprivkey=5HueCGU8rMjxEXxiPuD5BDku4MkFqeZyd4dZ1jvhTVqvbTLvyTJ

# Optional: send rewards to a specific address (defaults to wallet address)
#reward_address=TIME1...
```

Generate a key with `time-cli masternode genkey`. If omitted, the node uses its wallet's auto-generated key.

> **Note on `reward_address` changes:** If you update `reward_address` in `time.conf` and restart, the daemon overwrites the stored `wallet_address` on re-registration so block rewards route to the new address immediately. (Prior to v1.3.0 the stale address persisted until a full re-collateralization.)

### masternode.conf Format

```
# alias  IP:port  collateral_txid  collateral_vout
mn1  69.167.168.176:24100  abc123def456...  0
```

### Free Tier (No Collateral)

In `time.conf`:
```
masternode=1
```

No `masternode.conf` entry needed (or use 4-field format without collateral).

### Staked Tier (Bronze Example)

In `time.conf`:
```
masternode=1
masternodeprivkey=5HueCGU8rMjxEXxiPuD5BDku4MkFqeZyd4dZ1jvhTVqvbTLvyTJ
```

In `masternode.conf`:
```
mn1 69.167.168.176:24100 abc123def456789012345678901234567890123456789012345678901234abcd 0
```

---

## Setup Guide (Staked Tiers)

For detailed step-by-step instructions including key generation, collateral
creation, and configuration, see
**[LINUX_INSTALLATION.md §5.3](LINUX_INSTALLATION.md#53-staked-tiers-bronze--silver--gold)**.

After registration, the daemon automatically:
1. Parses the collateral UTXO from config
2. Verifies the UTXO exists and has the correct amount
3. Auto-detects the tier from the collateral amount
4. Locks the collateral
5. Registers the masternode on the network
6. Begins participating in consensus

### Verify Registration

```bash
# Check your balance (should show locked collateral)
time-cli getbalance
```

**Output:**
```
Wallet Balance:
  Total:         1500.00000000 TIME
  Locked:        1000.00000000 TIME (collateral)
  Available:      500.00000000 TIME (spendable)
```

```bash
# List all masternodes (should show 🔒 Locked)
time-cli masternodelist

# Check locked collaterals
time-cli listlockedcollaterals
```

---

## Managing Your Masternode

### Check Status

```bash
# Local masternode status
time-cli masternodestatus

# List all masternodes
time-cli masternodelist
```

### Monitor Rewards

```bash
# Check balance (shows total, locked, available)
time-cli getbalance
```

**What you see:**
- **Total**: All funds in your wallet
- **Locked**: Collateral locked for masternode(s)
- **Available**: Spendable funds (includes rewards)

### View Locked Collaterals

```bash
time-cli listlockedcollaterals
```

---

## Deregistering Your Masternode

To stop your masternode and unlock collateral, edit `time.conf`:

```
masternode=0
```

Then restart the daemon:
```bash
sudo systemctl restart timed
```

Your collateral is now unlocked and spendable.

**⚠️ Warning:** Deregistering stops your masternode and ends reward eligibility.

### Changing Tiers

To upgrade or downgrade your tier:

1. Set `masternode=0` in time.conf and restart (unlocks current collateral)
2. Create a new collateral UTXO for the new tier amount
3. Update `masternode.conf` with the new txid and vout
4. Set `masternode=1` and restart (tier auto-detects from new collateral amount)

---

## Reward Distribution

### How Rewards Work

Each block distributes 100 TIME + transaction fees:

- **35 TIME + fees** → Block producer (VRF-selected leader bonus)
- **65 TIME** → Four per-tier pools (Gold=25, Silver=18, Bronze=14, Free=8)

Within each tier's pool, rewards are divided equally among selected recipients. The block producer also receives their tier's pool share.

### Per-Tier Rotation

If a tier has more than 25 active nodes:
- Fairness bonus selects the 25 longest-waiting nodes
- Selected nodes split their tier's pool equally
- Remaining nodes rotate in on subsequent blocks
- All nodes in a tier get paid within `ceil(tier_count / 25)` blocks

---

## Validation & Automatic Cleanup

### Collateral Validation

After each block, the system validates all locked collaterals:

✅ **Valid if:**
- UTXO still exists
- UTXO not spent
- Collateral still locked
- UTXO is Unspent but not yet locked → **auto-locked** (handles recollateralization race)

❌ **Invalid if:**
- UTXO spent
- Collateral unlocked and UTXO does not exist

### Automatic Deregistration

If collateral becomes invalid:
1. Masternode automatically deregistered
2. Removed from reward rotation
3. Logged in system

> **Note:** The **local masternode** (this node) is never auto-deregistered by `cleanup_invalid_collaterals()`. The operator must explicitly set `masternode=0` in time.conf to deregister. This prevents false deregistration during recollateralization when the new UTXO exists but hasn't been formally locked yet.
>
> If the local masternode is unexpectedly deregistered, wallet RPCs (`getbalance`, `listunspent`) fall back to a stored `local_wallet_address` so UTXOs remain visible.

---

## Troubleshooting

### Error: "Collateral UTXO not found"

**Cause:** The specified UTXO doesn't exist or has been spent.

**Solution:**
```bash
time-cli listunspent
# Verify the txid and vout in masternode.conf match an unspent UTXO
```

### Error: "Invalid collateral_txid hex"

**Cause:** The `collateral_txid` in masternode.conf is not valid hex.

**Solution:** Ensure the txid is a 64-character hex string (no 0x prefix).

### Error: "Insufficient collateral confirmations"

**Cause:** UTXO needs 3 confirmations (~30 minutes).

**Solution:** Wait for more blocks, then restart the daemon.

### Masternode Not Receiving Rewards

**Possible causes:**
1. **Not active:** Check `masternodelist` — must show `Active: true`
2. **Collateral spent:** Run `listlockedcollaterals` — verify it's locked
3. **Rotation:** With many masternodes, you receive rewards periodically
4. **Just registered:** Wait 1 hour for eligibility

---

## Security

Masternode management is **local only**:
- Registration and deregistration are done via `time.conf` on the node
- No RPC commands can register or deregister masternodes
- The signing key is set via `masternodeprivkey` in `time.conf` (generated with `masternode genkey`)
- No one can remotely deregister your masternode

---

## Best Practices

### Security

✅ **Do:**
- Keep private keys secure
- Monitor collateral status regularly
- Keep node software updated
- Use a dedicated server for masternodes

❌ **Don't:**
- Share private keys
- Spend collateral UTXOs manually
- Ignore validation errors

### Operations

- **Monitor logs** for auto-deregistration warnings
- **Check rewards** regularly with `getbalance`
- **Verify collateral** with `listlockedcollaterals`
- **Maintain uptime** for maximum rewards

---

## FAQ

### Q: How do I register a masternode?
**A:** Generate a key with `time-cli masternode genkey`, add it to `time.conf`, configure collateral in `masternode.conf`, then start/restart the daemon.

### Q: How do I deregister a masternode?
**A:** Set `masternode=0` in `time.conf` and restart the daemon.

### Q: What happens if I spend locked collateral?
**A:** The transaction will be rejected — locked collateral UTXOs cannot be spent while the masternode is registered. Deregister first by setting `masternode=0` and restarting.

### Q: How long to wait for rewards?
**A:** Depends on total masternodes. With 50 MNs, expect rewards every ~50 minutes.

### Q: Can I change tier after registration?
**A:** Yes. Deregister (`masternode=0`, restart), create new collateral UTXO, update `masternode.conf`, restart. Tier auto-detects.

### Q: What if my node goes offline?
**A:** After 5 missed heartbeats (5 minutes), marked inactive. No rewards while inactive.

### Q: Do I need to save a signing key?
**A:** Yes. The `masternodeprivkey` in `time.conf` is your signing key. Back it up securely. Generate one with `time-cli masternode genkey`.

---

## Quick Reference

### Commands
```bash
# Generate masternode key
time-cli masternode genkey

# List masternodes
time-cli masternode list

# List locked collaterals
time-cli listlockedcollaterals

# Check status
time-cli masternode status

# Check balance
time-cli getbalance
```

### Config
**time.conf:**
```
masternode=1
masternodeprivkey=<base58check_key>
#reward_address=<TIME address>
```

**masternode.conf:**
```
mn1 <ip>:24100 <collateral_txid> <collateral_vout>
```

### Collateral Requirements
- **Free:** 0 TIME
- **Bronze:** 1,000 TIME (exact)
- **Silver:** 10,000 TIME (exact)
- **Gold:** 100,000 TIME (exact)
- **Confirmations:** 3 blocks (~30 minutes)

### Key Points
- ✅ Two-file config: `time.conf` (key + settings) + `masternode.conf` (collateral)
- ✅ Generate key with `time-cli masternode genkey`
- ✅ Locked collateral prevents accidental spending
- ✅ Automatic validation and cleanup
- ✅ Local-only security (no remote deregistration)
