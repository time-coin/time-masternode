# 🏆 TIME Coin Masternode Guide

## Overview

TIME Coin supports tiered masternodes with optional locked collateral (Dash-style). This guide covers setup, operation, and management of masternodes.

---

## Masternode Tiers

TIME Coin has four masternode tiers with different collateral requirements and reward weights:

| Tier | Collateral | Reward Weight | Voting Power | Sampling Weight |
|------|-----------|---------------|--------------|-----------------|
| **Free** | 0 TIME | 1x | 0x | 1x |
| **Bronze** | 1,000 TIME | 10x | 1x | 10x |
| **Silver** | 10,000 TIME | 100x | 10x | 100x |
| **Gold** | 100,000 TIME | 1000x | 100x | 1000x |

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

#### Step 1: Prepare Collateral UTXO

Ensure you have a UTXO with the required amount:
```bash
# List your UTXOs
time-cli listunspent

# Example output:
# txid: abc123def456...
# vout: 0
# amount: 1000.00000000
# confirmations: 5
```

**Requirements:**
- Bronze: 1,000 TIME
- Silver: 10,000 TIME
- Gold: 100,000 TIME
- Minimum 3 block confirmations (~30 minutes)

#### Step 2: Register with Locked Collateral

```bash
time-cli masternoderegister <tier> <txid> <vout> <reward_address> <node_address>
```

**Example:**
```bash
time-cli masternoderegister bronze \
  abc123def456789... \
  0 \
  TIMEyourrewardaddress \
  node1.example.com
```

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

#### Step 3: Verify Registration

```bash
# List all masternodes
time-cli masternodelist

# Check your collateral
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
# Check balance
time-cli getbalance

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
