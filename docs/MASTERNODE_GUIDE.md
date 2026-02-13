# 🏆 TIME Coin Masternode Guide

## Overview

TIME Coin supports tiered masternodes with locked collateral (Dash-style). Masternode management is **config-based** — you configure your masternode in `config.toml` and the daemon handles registration on startup.

---

## 🚀 Quick Start

### Free Tier (No Collateral)

1. Edit `config.toml`:
```toml
[masternode]
enabled = true
# No tier or collateral needed for free tier
```

2. Start/restart the daemon:
```bash
./target/release/timed
```

3. Verify:
```bash
time-cli masternodelist
```

### Staked Tier (Bronze/Silver/Gold)

1. Send exact collateral to yourself:
```bash
time-cli sendtoaddress <your_address> 1000.0
# Note the TXID from the output
```

2. Wait for 3 confirmations (~30 minutes):
```bash
time-cli listunspent
# Check confirmations >= 3, note the txid and vout
```

3. Edit `config.toml`:
```toml
[masternode]
enabled = true
collateral_txid = "abc123def456..."
collateral_vout = 0
# tier is auto-detected from the collateral UTXO amount
```

4. Restart the daemon:
```bash
sudo systemctl restart timed
```

5. Verify:
```bash
time-cli getbalance          # Should show locked collateral
time-cli masternodelist      # Should show your masternode with 🔒 Locked
time-cli listlockedcollaterals
```

**Done!** Your masternode is now active and earning rewards.

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
            │ Edit config.toml
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

TIME Coin has four masternode tiers with different collateral requirements and reward weights:

| Tier | Collateral | Reward Weight | Governance | Sampling Weight |
|------|-----------|---------------|------------|-----------------|
| **Free** | 0 TIME | 0.1x | None | 1x |
| **Bronze** | 1,000 TIME (exact) | 1x | 1 vote | 10x |
| **Silver** | 10,000 TIME (exact) | 10x | 10 votes | 100x |
| **Gold** | 100,000 TIME (exact) | 100x | 100 votes | 1000x |

### Tier Benefits

- **Reward Weight**: Determines share of block rewards in rotation
- **Voting Power**: Weight in governance decisions
- **Sampling Weight**: Probability of being selected for consensus voting

---

## Configuration

All masternode management is done through `config.toml`. No RPC commands are needed.

### config.toml Settings

```toml
[masternode]
enabled = true                          # Enable/disable masternode
# tier = "auto"                         # Auto-detected from collateral (default). Options: auto, free, bronze, silver, gold
collateral_txid = "abc123def456..."     # TXID of collateral UTXO (staked tiers only)
collateral_vout = 0                     # Output index of collateral UTXO
```

> **Tier auto-detection:** When `tier` is omitted or set to `"auto"` (default), the node automatically determines your tier from the collateral UTXO value on startup. You can still set `tier` explicitly if preferred.

### Free Tier (No Collateral)

```toml
[masternode]
enabled = true
collateral_txid = ""
collateral_vout = 0
```

### Staked Tier (Bronze Example)

```toml
[masternode]
enabled = true
collateral_txid = "abc123def456789012345678901234567890123456789012345678901234abcd"
collateral_vout = 0
```

---

## Setup Guide (Staked Tiers)

### Step 1: Check Your Balance

```bash
time-cli getbalance
```

**Output:**
```
Wallet Balance:
  Total:         1500.00000000 TIME
  Locked:           0.00000000 TIME (collateral)
  Available:     1500.00000000 TIME (spendable)
```

### Step 2: Create Collateral UTXO

Send the exact collateral amount to yourself. A 0.1% network fee applies, so your wallet needs slightly more than the collateral amount:

| Tier | Collateral | Fee (0.1%) | Total Needed |
|------|-----------|------------|--------------|
| Bronze | 1,000 TIME | 1.0 TIME | 1,001.0 TIME |
| Silver | 10,000 TIME | 10.0 TIME | 10,010.0 TIME |
| Gold | 100,000 TIME | 100.0 TIME | 100,100.0 TIME |

```bash
# Get your address
time-cli getnewaddress

# Send collateral to yourself (fee is added on top)
time-cli sendtoaddress <your_address> 1000.0
```

> ⚠️ **Do NOT use `--subtract-fee`** when creating collateral UTXOs. The collateral amount must be exactly 1,000 / 10,000 / 100,000 TIME. The fee must be paid on top.

**Why send to yourself?**
- Creates a distinct UTXO of exactly the required collateral amount
- Easier to track and manage
- Standard practice (Dash-style)

### Step 3: Wait for Confirmations

The UTXO needs 3 confirmations (~30 minutes):
```bash
# Check your UTXOs
time-cli listunspent

# Example output:
# txid: abc123def456...
# vout: 0
# amount: 1000.00000000
# confirmations: 3  ← Must be 3+
```

**Note the txid and vout** — you'll need these for the config file.

### Step 4: Update config.toml

```toml
[masternode]
enabled = true
collateral_txid = "abc123def456..."   # From Step 2
collateral_vout = 0                    # From Step 3
# tier is auto-detected from the collateral amount
```

### Step 5: Restart the Daemon

```bash
sudo systemctl restart timed
# Or: ./target/release/timed
```

The daemon will automatically:
1. Parse the collateral UTXO from config
2. Verify the UTXO exists and has the correct amount
3. Auto-detect the tier from the collateral amount
4. Lock the collateral
5. Register the masternode on the network
6. Begin participating in consensus

### Step 6: Verify Registration

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

To stop your masternode and unlock collateral, edit `config.toml`:

```toml
[masternode]
enabled = false
```

Then restart the daemon:
```bash
sudo systemctl restart timed
```

Your collateral is now unlocked and spendable.

**⚠️ Warning:** Deregistering stops your masternode and ends reward eligibility.

### Changing Tiers

To upgrade or downgrade your tier:

1. Set `enabled = false` in config.toml and restart (unlocks current collateral)
2. Create a new collateral UTXO for the new tier amount
3. Update `collateral_txid` and `collateral_vout` in config.toml
4. Set `enabled = true` and restart (tier auto-detects from new collateral amount)

---

## Reward Distribution

### How Rewards Work

- **10 masternodes** are selected per block
- Selection uses deterministic rotation based on block height
- All nodes agree on which masternodes receive rewards
- Rewards distributed proportional to tier weight

### Rotation System

If there are more than 10 masternodes:
- Block 1: Nodes 1-10 receive rewards
- Block 2: Nodes 11-20 receive rewards
- Block N: Rotation continues through all masternodes
- Each node receives rewards every `N/10` blocks (where N = total masternodes)

---

## Validation & Automatic Cleanup

### Collateral Validation

After each block, the system validates all locked collaterals:

✅ **Valid if:**
- UTXO still exists
- UTXO not spent
- Collateral still locked

❌ **Invalid if:**
- UTXO spent
- Collateral unlocked

### Automatic Deregistration

If collateral becomes invalid:
1. Masternode automatically deregistered
2. Removed from reward rotation
3. Logged in system

---

## Troubleshooting

### Error: "Collateral UTXO not found"

**Cause:** The specified UTXO doesn't exist or has been spent.

**Solution:**
```bash
time-cli listunspent
# Verify the txid and vout in config.toml match an unspent UTXO
```

### Error: "Invalid collateral_txid hex"

**Cause:** The `collateral_txid` in config.toml is not valid hex.

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
- Registration and deregistration are done via `config.toml` on the node
- No RPC commands can register or deregister masternodes
- The signing key is derived from your node's wallet
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
**A:** Edit `config.toml` with your collateral info (tier is auto-detected), then start/restart the daemon.

### Q: How do I deregister a masternode?
**A:** Set `enabled = false` in `config.toml` and restart the daemon.

### Q: What happens if I spend locked collateral?
**A:** Your masternode is automatically deregistered and removed from rewards.

### Q: How long to wait for rewards?
**A:** Depends on total masternodes. With 50 MNs, expect rewards every ~50 minutes.

### Q: Can I change tier after registration?
**A:** Yes. Deregister (set `enabled = false`, restart), create new collateral UTXO, update `collateral_txid`/`collateral_vout`, restart. Tier auto-detects.

### Q: What if my node goes offline?
**A:** After 5 missed heartbeats (5 minutes), marked inactive. No rewards while inactive.

### Q: Do I need to save a signing key?
**A:** No. The signing key is derived from your node's wallet automatically.

---

## Quick Reference

### Commands
```bash
# List masternodes
time-cli masternodelist

# List locked collaterals
time-cli listlockedcollaterals

# Check status
time-cli masternodestatus

# Check balance
time-cli getbalance
```

### Config
```toml
[masternode]
enabled = true
# tier = "auto"                    # Auto-detected from collateral (default)
collateral_txid = "abc123..."      # TXID of collateral UTXO
collateral_vout = 0                # Output index
```

### Collateral Requirements
- **Free:** 0 TIME
- **Bronze:** 1,000 TIME (exact)
- **Silver:** 10,000 TIME (exact)
- **Gold:** 100,000 TIME (exact)
- **Confirmations:** 3 blocks (~30 minutes)

### Key Points
- ✅ Config-based management (no RPC commands needed)
- ✅ Locked collateral prevents accidental spending
- ✅ Automatic validation and cleanup
- ✅ Local-only security (no remote deregistration)
