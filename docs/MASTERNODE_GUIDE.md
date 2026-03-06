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

Within each tier's pool, rewards are divided equally among selected recipients. The block producer also receives their tier's pool share. If a tier has no active nodes, its pool goes to the block producer instead.

### Fairness Rotation

When a tier has more active nodes than the per-block cap of 25, a fairness bonus ensures every node eventually gets paid — no starvation, no dust outputs:

1. **Fairness bonus** per node: `min(blocks_since_last_paid / 10, 20)` — computed on-chain by scanning `masternode_rewards` in recent blocks (up to 1,000 blocks back); deterministic across all validators
2. **Sort** eligible nodes by fairness bonus (descending), then address (ascending) as a deterministic tiebreaker
3. **Select** top 25 nodes; distribute `tier_pool / recipient_count` equally
4. **Minimum payout guard**: if `tier_pool / recipients < 1 TIME`, the pool goes to the block producer to prevent dust outputs

All nodes in a tier receive payment within `ceil(tier_count / 25)` blocks.

#### Free Tier Maturity Gate

On mainnet, Free-tier nodes must be registered for ≥ 72 blocks (~12 hours) before becoming eligible for pool rewards. Paid tiers (Bronze/Silver/Gold) are always eligible — their collateral acts as sybil resistance.

### Example Scenarios

#### Small Network (1 Gold, 2 Silver, 3 Bronze, 4 Free)

```
Block producer: Silver node A (won VRF)
- Leader bonus: 35 TIME + fees

Gold pool (25 TIME ÷ 1):    Gold A = 25 TIME
Silver pool (18 TIME ÷ 2):  Silver A = 9 TIME (merged with leader = 44 TIME)
                             Silver B = 9 TIME
Bronze pool (14 TIME ÷ 3):  Bronze A = 4.67, B = 4.67, C = 4.66 TIME
Free pool (8 TIME ÷ 4):     Free A = 2, B = 2, C = 2, D = 2 TIME

Every node is paid every block.
```

#### Large Network (5 Gold, 20 Silver, 75 Bronze, 400 Free)

```
Gold pool (25 ÷ 5):     5 TIME each   — all paid every block
Silver pool (18 ÷ 20):  0.9 TIME each — all paid every block
Bronze pool (14 ÷ 25):  0.56 TIME each — top 25 of 75 by fairness
                          All 75 rotate through in 3 blocks
Free pool (8 ÷ 8):      1 TIME each   — top 8 of 400 by fairness
                          All 400 rotate through in 50 blocks (~8.3 hours)
```

#### Extreme Scale (10,000 Free nodes)

```
Free pool: 8 TIME ÷ 8 = 1 TIME each  (max 8 recipients — enforced by 1 TIME minimum)
Rotation:  Each node paid every ~1,250 blocks (~8.7 days)
Per payment: 1 TIME (meaningful, not dust)
```

### Consensus Safety

Block reward distribution is validated **before voting** in `validate_block_before_vote()`. Proposed rewards that deviate beyond `GOLD_POOL_SATOSHIS` (25 TIME) per recipient cause the node to refuse to vote; the block fails to reach consensus and TimeGuard fallback selects the next VRF producer.

During `add_block()`, per-recipient deviations up to 25 TIME are tolerated with a warning to handle minor masternode list divergence. Deviations beyond the cap are hard-rejected. The total block reward is always strictly validated.

Each node tracks reward-distribution violations per block producer address (lifetime counter). After **3 violations** (`REWARD_VIOLATION_THRESHOLD`), the producer is marked **misbehaving** and all future proposals from that address are rejected without voting.

```
⚠️ Producer X reward violation (1/3 strikes)
⚠️ Producer X reward violation (2/3 strikes)
🚨 Producer X has 3 reward violation(s) — now MISBEHAVING, future proposals will be rejected
```

### Reward Constants

| Constant | Value | Description |
|----------|-------|-------------|
| `PRODUCER_REWARD_SATOSHIS` | 35 × 10⁸ | Leader bonus (35 TIME) |
| `GOLD_POOL_SATOSHIS` | 25 × 10⁸ | Gold tier pool (25 TIME) |
| `SILVER_POOL_SATOSHIS` | 18 × 10⁸ | Silver tier pool (18 TIME) |
| `BRONZE_POOL_SATOSHIS` | 14 × 10⁸ | Bronze tier pool (14 TIME) |
| `FREE_POOL_SATOSHIS` | 8 × 10⁸ | Free tier pool (8 TIME) |
| `MIN_POOL_PAYOUT_SATOSHIS` | 10⁸ | Minimum 1 TIME per recipient |
| `MAX_TIER_RECIPIENTS` | 25 | Max recipients per tier per block |
| `FREE_MATURITY_BLOCKS` | 72 | Free tier maturity gate (mainnet) |
| `REWARD_VIOLATION_THRESHOLD` | 3 | Strikes before producer is marked misbehaving |

**Key implementation files:** `src/constants.rs` (all reward constants), `src/types.rs` (`MasternodeTier::pool_allocation()` and `reward_weight()`), `src/blockchain.rs` (`produce_block_at_height()` and `validate_pool_distribution()`), `src/masternode_registry.rs` (`get_eligible_pool_nodes()` and `get_pool_reward_tracking()`).

---

## Block Producer Selection

TIME Coin selects block producers using **weighted VRF sortition** — each masternode's probability of being chosen as leader is proportional to its tier weight. Higher tiers produce more blocks and earn proportionally more leader bonuses over time, with no participant lists stored in blocks.

### Selection Algorithm

```
1. Collect all active masternodes, sorted deterministically by address
2. Build a cumulative weight array using each node's tier.reward_weight()
3. Derive a deterministic random value:
   Hash(prev_block_hash || block_height || attempt_number)
4. Select the masternode where the random value falls in the cumulative array
5. Higher tier weight → higher selection probability
```

On timeout the `attempt` counter increments, rotating to a different producer deterministically. Block hash provides all entropy — no external randomness source required.

### Tier Selection Weights

| Tier | Weight | Relative to Bronze |
|------|--------|--------------------|
| Free | 100 | 0.1x |
| Bronze | 1,000 | 1x (baseline) |
| Silver | 10,000 | 10x |
| Gold | 100,000 | 100x |

### Selection Probability

```
Probability(node) = node.tier.reward_weight() / total_network_weight
```

**Example — 5 Free + 1 Bronze (total weight: 1,500):**
- Each Free node: 100 / 1,500 = **6.67%**
- Bronze node: 1,000 / 1,500 = **66.67%**

### Expected Leader Earnings (144 blocks/day at 10-min blocks)

The leader bonus is 35 TIME + transaction fees per block produced. Higher-tier nodes produce proportionally more blocks:

#### Balanced Network (10 nodes per tier, total weight: 1,111,000)

| Tier | Blocks/Month (each) | Approx. Monthly Leader Earnings |
|------|---------------------|--------------------------------|
| Free | 3.9 | ~194 TIME |
| Bronze | 38.9 | ~1,944 TIME |
| Silver | 388.8 | ~19,440 TIME |
| Gold | 3,888 | ~194,400 TIME |

#### Mature Network (1,000 Free / 100 Bronze / 10 Silver / 1 Gold, total weight: 400,000)

Each tier contributes equal total weight, so each tier collectively earns the same from leader bonuses:

| Tier | Monthly Earnings (each) | Tier Total |
|------|------------------------|------------|
| Free | ~1.08 TIME | 1,080 TIME |
| Bronze | ~10.8 TIME | 1,080 TIME |
| Silver | ~108 TIME | 1,080 TIME |
| Gold | ~1,080 TIME | 1,080 TIME |

#### Gold Whale (1 Gold + 10 Bronze + 100 Free, total weight: 120,000)

| Tier | Network Share | Monthly Earnings |
|------|---------------|-----------------|
| 100 Free (total) | 8.3% | ~1,800 TIME |
| 10 Bronze (total) | 8.3% | ~1,800 TIME |
| 1 Gold | **83.3%** | ~180,000 TIME |

A Gold whale dominates block production until more high-tier nodes join — creating a strong economic incentive to increase collateral.

### Key Properties

- **No blockchain bloat**: Only the producer address is stored per block (already required in `header.leader`) — no participant lists, scales to millions of masternodes
- **Deterministic**: All nodes independently compute the same leader from the same inputs
- **Manipulation-resistant**: Uses block hash as entropy; no external randomness source required
- **Clear tier incentive**: Gold nodes earn ~100× more leader bonuses than Free nodes

### Future Considerations

- **Weight caps**: If one Gold whale dominates, per-node weight caps (e.g., max 10% of total network weight) could be introduced
- **Progressive weight decay** for very large holders
- **Fee distribution**: Currently all transaction fees go to the producer; a future enhancement could split fees among recent participants or include a developer fund allocation
- **Tier expansion**: Platinum/Diamond tiers or dynamic collateral thresholds as the network matures

**Implementation:** `src/main.rs` (cumulative weight array + deterministic selection), `src/masternode_registry.rs`, `src/types.rs` (`MasternodeTier::reward_weight()`), `src/constants.rs` (weight constants).

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
