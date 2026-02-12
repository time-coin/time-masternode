# 🏆 TIME Coin Masternode Guide

## Overview

TIME Coin supports tiered masternodes with locked collateral (Dash-style). This guide covers setup, operation, and management of masternodes.

---

## 🚀 Quick Start (5 Steps)

**Want to set up a masternode right now?** Follow these steps:

### 1. Check Balance
```bash
time-cli getbalance
# Need: 1,000 TIME (Bronze), 10,000 (Silver), or 100,000 (Gold)
```

### 2. Create Collateral UTXO
```bash
time-cli sendtoaddress <your_address> 1000.0
# Sends collateral to yourself, creates lockable UTXO
```

### 3. Wait 30 Minutes
```bash
time-cli listunspent
# Check confirmations >= 3
```

### 4. Register Masternode
```bash
time-cli masternoderegister bronze <txid_from_step_2> 0 <your_address> <your_node_ip>
```

### 5. Verify
```bash
time-cli getbalance
# Should show locked collateral

time-cli masternodelist
# Should show your masternode with 🔒 Locked
```

**Done!** Your masternode is now active and earning rewards. Read below for details.

---

## 📊 Collateral Lock Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                    MASTERNODE SETUP FLOW                        │
└─────────────────────────────────────────────────────────────────┘

1. PREPARE FUNDS                    2. CREATE UTXO
   ┌──────────────┐                    ┌──────────────┐
   │ Total: 1500  │                    │ Total: 1500  │
   │ Locked: 0    │ ──sendtoaddress──> │ Locked: 0    │
   │ Avail: 1500  │                    │ Avail: 1500  │
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
            │ masternoderegister
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
6. RECEIVE REWARDS                     7. UNLOCK (OPTIONAL)
   ┌──────────────┐                    ┌──────────────┐
   │ Total: 2500  │                    │ Total: 2500  │
   │ Locked: 1000 │ masternodeunlock   │ Locked: 0    │
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

## Setup Methods

### Method 1: Legacy Masternode (No Locked Collateral)

**Pros:**
- Simple setup
- No UTXO locking
- Backward compatible

**Cons:**
- Can accidentally spend collateral
- No on-chain proof of stake

**Setup:**

1. Edit `config.toml`:
```toml
[masternode]
enabled = true
tier = "Bronze"  # or Silver, Gold
wallet_address = "TIMEyouraddresshere"
```

2. Start node:
```bash
./target/release/timed
```

---

### Method 2: Locked Collateral Masternode (Recommended)

**Pros:**
- ✅ Prevents accidental spending
- ✅ On-chain proof of stake
- ✅ Dash-style security model
- ✅ Automatic validation

**Cons:**
- UTXO locked while masternode active
- Must deregister to unlock

**Setup:**

#### Step 1: Check Your Balance

First, verify you have enough funds:
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

**Requirements:**
- Bronze: 1,000 TIME
- Silver: 10,000 TIME
- Gold: 100,000 TIME

#### Step 2: Create Collateral UTXO

Send the exact collateral amount to yourself:
```bash
# Get your address
time-cli getnewaddress

# Send collateral to yourself
time-cli sendtoaddress <your_address> 1000.0

# Returns transaction ID
# abc123def456789...
```

**Why send to yourself?**
- Creates a distinct UTXO to lock
- Easier to track and manage
- Standard practice (Dash-style)

#### Step 3: Wait for Confirmations

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

**Note the txid and vout** - you'll need these for registration.

#### Step 4: Register with Locked Collateral

```bash
time-cli masternoderegister bronze abc123def456789... 0 <your_address> <your_node_ip>
```

**Parameters (positional):**
1. `tier` — bronze, silver, or gold
2. `collateral_txid` — Transaction ID from Step 2 (hex)
3. `vout` — Output index (usually 0)
4. `reward_address` — Your address for receiving rewards
5. `node_address` — Your node's public IP

**Output:**
```
✅ Masternode Registered Successfully

Masternode Address: node1.example.com
Tier: Bronze
Collateral: 1,000 TIME
Collateral UTXO: abc123def456...:0
Reward Address: TIMEyourrewardaddress

⚠️  IMPORTANT: Your collateral is now LOCKED
   - Cannot spend this UTXO while masternode is active
   - Use 'masternodeunlock' to deregister and unlock
```

#### Step 5: Verify Registration

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

**Output:**
```
Wallet Balance:
  Total:         2500.00000000 TIME
  Locked:        1000.00000000 TIME (collateral)
  Available:     1500.00000000 TIME (spendable)
```

**What you see:**
- **Total**: All funds in your wallet
- **Locked**: Collateral locked for masternode(s)
- **Available**: Spendable funds (includes rewards)

```bash
# Get detailed wallet info
time-cli getwalletinfo

# List recent transactions
time-cli listunspent
```

### View Locked Collaterals

```bash
# List all locked collaterals
time-cli listlockedcollaterals
```

**Output:**
```
Locked Collaterals:
Outpoint                                                           Masternode            Amount (TIME)  Height
abc123def456...:0                                                  node1.example.com     1000.00000000  12345

Total Locked: 1
```

---

## Deregistering & Unlocking Collateral

To stop your masternode and unlock collateral:

```bash
# Unlock local masternode
time-cli masternodeunlock

# Or unlock specific masternode
time-cli masternodeunlock node1.example.com
```

**Output:**
```
✅ Masternode Unlocked Successfully

Masternode Address: node1.example.com
Collateral UTXO: abc123def456...:0
Status: Deregistered

Your collateral is now unlocked and spendable.
```

**⚠️ Warning:** Deregistering stops your masternode and ends reward eligibility.

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

### Example

With 50 masternodes:
- 10 selected per block
- Your masternode receives rewards every 5 blocks
- At 10 minutes per block = rewards every 50 minutes

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

**Example log:**
```
🗑️ Auto-deregistered 1 masternode(s) with invalid collateral at height 12345
```

---

## Troubleshooting

### Error: "Collateral UTXO not found"

**Cause:** The specified UTXO doesn't exist or has been spent.

**Solution:**
```bash
# Check your UTXOs
time-cli listunspent

# Use a valid UTXO from the list
```

### Error: "Insufficient collateral confirmations"

**Cause:** UTXO needs 3 confirmations (~30 minutes).

**Solution:**
```bash
# Check confirmations
time-cli listunspent

# Wait for 3+ confirmations
time-cli getblockcount
```

### Error: "Collateral UTXO already locked"

**Cause:** This UTXO is already used by another masternode.

**Solution:**
```bash
# Find locked collaterals
time-cli listlockedcollaterals

# Use a different UTXO
time-cli listunspent
```

### Error: "Collateral has been spent"

**Cause:** The UTXO was spent (no longer exists).

**Solution:**
```bash
# Find unspent UTXOs
time-cli listunspent

# Register with an unspent UTXO
```

### Masternode Not Receiving Rewards

**Possible causes:**
1. **Not active:** Check `masternodelist` - must show `Active: true`
2. **Collateral spent:** Run `listlockedcollaterals` - verify it's locked
3. **Rotation:** With many masternodes, you receive rewards periodically
4. **Just registered:** Wait 1 hour for eligibility

**Debug steps:**
```bash
# Check if active
time-cli masternodelist | grep youraddress

# Verify collateral locked
time-cli listlockedcollaterals

# Check node uptime
time-cli masternodestatus
```

---

## Migration Guide

### Upgrading Legacy → Locked Collateral

If you have an existing legacy masternode:

1. **Optional:** Legacy masternodes continue to work indefinitely
2. **No deadline:** Migrate at your convenience
3. **No penalties:** Both types receive rewards

**To migrate:**

```bash
# Step 1: Identify collateral UTXO
time-cli listunspent

# Step 2: Register with locked collateral
time-cli masternoderegister <tier> <txid> <vout> <reward_addr> <node_addr>

# Step 3: Verify
time-cli masternodelist
time-cli listlockedcollaterals
```

**Note:** Your old legacy masternode and new locked masternode are separate. You can run both.

---

## Best Practices

### Security

✅ **Do:**
- Keep private keys secure
- Monitor collateral status regularly
- Verify UTXOs before locking
- Keep node software updated

❌ **Don't:**
- Share private keys
- Lock wrong UTXO
- Forget collateral is locked
- Ignore validation errors

### Operations

- **Monitor logs** for auto-deregistration warnings
- **Check rewards** regularly with `getbalance`
- **Verify collateral** with `listlockedcollaterals`
- **Maintain uptime** for maximum rewards
- **Use locked collateral** for better security

### Economics

- **Bronze:** Good starting point, 10x rewards
- **Silver:** Serious operators, 100x rewards
- **Gold:** Largest operators, 1000x rewards
- **Higher tiers:** More voting power and consensus weight

---

## Technical Details

### Collateral Lock Mechanism

When you register with locked collateral:
1. UTXO marked as "locked" in UTXO manager
2. Prevents spending in transactions
3. Validated after each block
4. Automatic cleanup if spent

### Network Protocol

- Collateral info included in masternode announcements
- Peers synchronize locked collateral data
- Conflict detection for double-locks
- Broadcast unlock events to network

### Storage

- Collateral locks stored in DashMap (thread-safe)
- Persisted with UTXO state
- Survives node restarts
- Binary compatible (no migration needed)

---

## FAQ

### Q: Do I need to migrate immediately?
**A:** No. Legacy masternodes work indefinitely. Migrate when convenient.

### Q: Can I have both legacy and locked masternodes?
**A:** Yes. You can run multiple masternodes of different types.

### Q: What happens if I spend locked collateral?
**A:** Your masternode is automatically deregistered and removed from rewards.

### Q: Can I unlock collateral anytime?
**A:** Yes, use `masternodeunlock`. This deregisters your masternode.

### Q: How long to wait for rewards?
**A:** Depends on total masternodes. With 50 MNs, expect rewards every ~50 minutes.

### Q: What's the benefit of locked collateral?
**A:** Prevents accidental spending, provides on-chain proof of stake, aligns with Dash security model.

### Q: Do locked masternodes get more rewards?
**A:** No. Both legacy and locked masternodes receive equal rewards based on tier.

### Q: Can I change tier after registration?
**A:** No. You must unlock, then register with new tier.

### Q: What if my node goes offline?
**A:** After 5 missed heartbeats (5 minutes), marked inactive. No rewards while inactive.

### Q: How do I backup my masternode?
**A:** Backup your wallet private keys. The masternode registration is on-chain.

---

## Support

For questions or issues:
- **GitHub Issues:** https://github.com/time-coin/timecoin/issues
- **Documentation:** https://github.com/time-coin/timecoin/tree/main/docs
- **Protocol Spec:** docs/TIMECOIN_PROTOCOL.md

---

## Quick Reference

### Commands
```bash
# Register with locked collateral
time-cli masternoderegister <tier> <txid> <vout> <reward_addr> <node_addr>

# Unlock collateral
time-cli masternodeunlock [node_addr]

# List masternodes
time-cli masternodelist

# List locked collaterals
time-cli listlockedcollaterals

# Check status
time-cli masternodestatus

# Check balance
time-cli getbalance
```

### Collateral Requirements
- **Bronze:** 1,000 TIME
- **Silver:** 10,000 TIME
- **Gold:** 100,000 TIME
- **Confirmations:** 3 blocks (~30 minutes)

### Key Points
- ✅ Locked collateral prevents accidental spending
- ✅ Automatic validation and cleanup
- ✅ Full backward compatibility
- ✅ No forced migration timeline
- ✅ 202 tests passing (production-ready)
