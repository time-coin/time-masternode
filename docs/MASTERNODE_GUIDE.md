# TIME Coin Masternode Guide

> Last updated: 2026-03-24

## Overview

TIME Coin supports tiered masternodes with locked collateral (Dash-style). Configuration uses two files: `time.conf` (daemon settings and private key) and `masternode.conf` (collateral info). The daemon handles registration on startup.

> **Linux users:** See **[LINUX_INSTALLATION.md](LINUX_INSTALLATION.md)** for
> the step-by-step installation guide. This document covers masternode
> **operations** — tiers, collateral, rewards, monitoring, and management.
>
> **Windows users:** See the [Windows Setup](#windows-setup) section below.

---

## Quick Start

### Free Tier (No Collateral)

Set `masternode=1` in `time.conf` and start the daemon — that's it. The node
begins earning rewards immediately.

### Staked Tier (Bronze/Silver/Gold)

See **[LINUX_INSTALLATION.md §5.3](LINUX_INSTALLATION.md#53-staked-tiers-bronze--silver--gold)**
for the step-by-step collateral setup process.

---

## Windows Setup

### Prerequisites

- **Git for Windows**: https://git-scm.com/download/win
- **Rust**: https://rustup.rs (download and run `rustup-init.exe`, accept defaults)
- **Visual Studio Build Tools**: Install "Desktop development with C++" workload

### Automated Installation (Recommended)

The install script handles cloning, building, config generation, and firewall
setup in one step. Open a **Command Prompt or PowerShell** and run:

```
cd %USERPROFILE%
git clone https://github.com/time-coin/time-masternode.git
cd time-masternode

REM Testnet (recommended for first-time setup)
scripts\install-masternode.bat testnet

REM — or —

REM Mainnet (production)
scripts\install-masternode.bat mainnet
```

The script will:
1. Check that Git and Rust are installed (and tell you how to install them if not)
2. Clone or update the repository
3. Build release binaries (`timed.exe`, `time-cli.exe`)
4. Create the data directory and generate a `time.conf` with random RPC credentials
5. Copy binaries and add them to `PATH`
6. Add a Windows Firewall inbound rule for the P2P port

After installation, edit your config to set a reward address:
```
notepad %APPDATA%\timecoin\time.conf          REM mainnet
notepad %APPDATA%\timecoin\testnet\time.conf  REM testnet
```

### Manual Install and Build

If you prefer manual control, open a terminal (PowerShell or Command Prompt):

```
cd %USERPROFILE%
git clone https://github.com/time-coin/time-masternode.git
cd time-masternode
cargo build --release --bin timed --bin time-cli
```

### Run

```
REM Testnet
target\release\timed.exe --testnet

REM Mainnet
target\release\timed.exe
```

### Data Directories

| Network | Directory |
|---------|-----------|
| Mainnet | `%APPDATA%\timecoin\` |
| Testnet | `%APPDATA%\timecoin\testnet\` |

### Configuration

Create or edit `%APPDATA%\timecoin\time.conf`:

```ini
masternode=1
masternodeprivkey=<your-key>
reward_address=<your-wallet-address>
```

For testnet, edit `%APPDATA%\timecoin\testnet\time.conf` and add `testnet=1`.

### Running as a Windows Service

Use **NSSM** (Non-Sucking Service Manager) to run `timed` as a background
service:

```
nssm install timed "%USERPROFILE%\time-masternode\target\release\timed.exe"
nssm start timed
```

For testnet, install a separate service:

```
nssm install timetd "%USERPROFILE%\time-masternode\target\release\timed.exe" "--testnet"
nssm start timetd
```

### Updating (Windows)

```
cd %USERPROFILE%\time-masternode
scripts\update.bat testnet
```

Or update both networks: `scripts\update.bat`

The script pulls latest code, rebuilds, stops the running node, copies new
binaries, and restarts.

### Uninstalling (Windows)

```
cd %USERPROFILE%\time-masternode
scripts\uninstall-masternode.bat testnet
```

Or for mainnet: `scripts\uninstall-masternode.bat` (defaults to mainnet)

The script stops the running process, removes any NSSM service, deletes the
firewall rule, and removes the binaries. **Blockchain data and your wallet
are preserved** — you will be shown the commands to delete them manually if
you want a full wipe.

### Firewall

Open the P2P port (PowerShell as admin):

```powershell
# Mainnet
netsh advfirewall firewall add rule name="TIME P2P" dir=in action=allow protocol=tcp localport=24000

# Testnet
netsh advfirewall firewall add rule name="TIME P2P Testnet" dir=in action=allow protocol=tcp localport=24100
```

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
- **Voting Power**: Weight in on-chain governance decisions (Bronze=1, Silver=10, Gold=100)
- **Sampling Weight**: Probability of being selected for consensus voting and VRF block production

---

## On-Chain Governance

Bronze, Silver, and Gold masternodes can submit proposals and vote on protocol changes. Free-tier nodes have no governance access.

### Voting Weights

| Tier | Governance Weight |
|------|------------------:|
| Free | 0 (no access) |
| Bronze | 1 |
| Silver | 10 |
| Gold | 100 |

A proposal passes when **YES weight ≥ 67%** of total active governance weight at the end of the 1,008-block (~1 week) voting window.

### Common Governance Operations

```bash
# See all active proposals
time-cli listproposals active

# Vote YES on a proposal
time-cli voteproposal <proposal_id> yes

# Vote NO on a proposal
time-cli voteproposal <proposal_id> no

# Get proposal details and current tally
time-cli getproposal <proposal_id>

# Submit a treasury spend proposal (Bronze/Silver/Gold only)
time-cli submitproposal treasury <recipient_address> <amount_TIME> "<description>"

# Submit a fee schedule change
time-cli submitproposal feeschedule <new_min_fee_TIME> '<[{"upper":100,"rate_bps":100},...]>'
```

See [GOVERNANCE.md](GOVERNANCE.md) for the full governance reference.

---

## Configuration

Masternode configuration uses two files:
- **`time.conf`** — daemon settings and masternode private key
- **`masternode.conf`** — collateral info (alias, txid, vout)

### time.conf Settings

```
masternode=1
masternodeprivkey=5HueCGU8rMjxEXxiPuD5BDku4MkFqeZyd4dZ1jvhTVqvbTLvyTJ

# Optional: send rewards to a specific address (defaults to wallet address)
#reward_address=TIME1...
```

Generate a key with `time-cli masternode genkey`. If omitted, the node uses its wallet's auto-generated key.

> **Important:** Set `reward_address` to the **same address you sent the collateral to** in
> your GUI wallet. The daemon uses this to prove ownership of the collateral UTXO — if
> `reward_address` matches the on-chain UTXO output address, the node can evict any squatter
> that grabbed the UTXO before you announced. See [Squatter protection](#squatter-protection)
> below.

> **Note on `reward_address` changes:** If you update `reward_address` in `time.conf` and restart, the daemon overwrites the stored `wallet_address` on re-registration so block rewards route to the new address immediately. (Prior to v1.3.0 the stale address persisted until a full re-collateralization.)

### masternode.conf Format

```
# alias  collateral_txid  collateral_vout
mn1  abc123def456...  0
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
mn1 abc123def456789012345678901234567890123456789012345678901234abcd 0
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

---

## Squatter Protection

### What is collateral squatting?

An attacker who monitors the mempool can see a new collateral UTXO appear
(your send-to-self transaction) and immediately gossip a masternode announcement
claiming that TXID before your daemon starts. Under the old first-claim rule,
this permanently blocked your legitimate registration.

### How the daemon protects you (v1.4.35+)

When your daemon announces itself, it signs a **V4 collateral proof**:

```
"TIME_COLLATERAL_CLAIM:<txid>:<vout>"  signed with masternodeprivkey
```

If another node already holds the collateral lock, two conditions are checked:

1. **Proof valid** — the signature verifies against your public key over the
   exact UTXO outpoint.
2. **Reward address matches** — your `reward_address` in `time.conf` must
   equal the on-chain address of the collateral UTXO (i.e. the wallet address
   you sent the coins to).

If both pass, the squatter is **automatically evicted** and you are registered.

### What you need to do

Set `reward_address` in `time.conf` to the same address you used when sending
collateral from the GUI wallet:

```ini
masternode=1
masternodeprivkey=5HueCGU8...
reward_address=TIME1YourCollateralWalletAddressHere
```

No additional steps required. The daemon generates and broadcasts the proof
automatically on every announcement (startup + every 60 seconds).

### What an attacker cannot do

| Attack | Why it fails |
|---|---|
| Squatter uses own `reward_address` | `reward_address ≠ utxo.address` → condition 2 fails |
| Squatter uses victim's `reward_address` | All rewards go to victim's wallet — no benefit |
| Squatter forges the proof signature | Requires victim's `masternodeprivkey` — not feasible |

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

## Monitoring Your Node

### TUI Dashboard

TIME Coin includes a built-in terminal dashboard that displays real-time node
status in a single view:

```bash
# Linux
bash scripts/dashboard.sh
```

```
REM Windows
scripts\dashboard.bat
```

**Tabs** (switch with `1`–`5` or arrow keys):

| Tab | Shows |
|-----|-------|
| **Overview** | Chain height, sync progress, wallet balance, peer count, masternode info |
| **Masternode** | Your node's tier/uptime, full network masternode list with status |
| **Mempool** | Pending and finalized transactions, fee details, transaction inspector |
| **Blocks** | Recent blocks with height, hash, timestamp, tx count, reward |
| **Governance** | Active proposals — vote directly from the dashboard |

**Keys:** `q` quit · `↑↓` scroll · `Enter` expand detail · `1`–`5` jump to tab

The dashboard auto-detects which network is running (by checking `.cookie`
files) and reads RPC credentials from `time.conf` automatically.

### CLI Commands

```bash
# Node status
time-cli getblockchaininfo       # Chain height, sync progress, consensus
time-cli getnetworkinfo           # Version, connections
time-cli getpeerinfo              # Connected peers with latency and tier

# Masternode
time-cli masternodestatus         # Your node's status, tier, uptime
time-cli masternodelist           # All registered masternodes
time-cli masternodelist true      # Include inactive masternodes

# Wallet
time-cli getbalance               # Available balance
time-cli getwalletinfo            # Balance, locked, available, tx count
time-cli listtransactions         # Recent transactions

# Collateral
time-cli listlockedcollaterals    # Show locked collateral UTXOs
time-cli masternoderegstatus      # Registration eligibility check
```

### Log Monitoring

```bash
# Linux (systemd)
journalctl -u timed -f                  # mainnet live
journalctl -u timetd -f                 # testnet live
journalctl -u timed -n 200 --no-pager   # last 200 lines

# Windows (logs in data directory)
type %APPDATA%\timecoin\debug.log
type %APPDATA%\timecoin\testnet\debug.log
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

## Deregistering and Unlocking Collateral

### Normal case — you have the same node and wallet

Edit `masternode.conf` and comment out (or remove) the collateral line:

```
# MN1 abc123...txid 0
```

Then restart the daemon:

```bash
sudo systemctl restart timed
```

The daemon detects the removed collateral line and automatically broadcasts a
signed **`MasternodeUnlock`** gossip message to all connected peers. Within
~15 seconds of peer connections establishing, every node on the network releases
the collateral lock and unregisters the masternode — **no on-chain transaction
is required**.

This mechanism is analogous to Dash's `ProUpRevTx`: the unlock is authenticated
by an Ed25519 signature over the collateral outpoint using `masternodeprivkey`,
so peers accept it without requiring the collateral UTXO to be spent first. The
collateral UTXO becomes fully spendable on all nodes as soon as the signed
message propagates.

**⚠️ Warning:** Deregistering stops your masternode and ends reward eligibility.

---

### Changing Tiers

To upgrade or downgrade your tier:

1. Update `masternode.conf` with the new txid and vout (replace the old line)
2. Restart the daemon

The daemon will automatically:
- Re-announce with the new collateral outpoint; tier is auto-detected from the
  collateral amount (Bronze = 1,000 TIME, Silver = 10,000 TIME, Gold = 100,000 TIME)
- Queue the old collateral outpoint for release — the lock is removed and the
  sled anchor cleared within 30 seconds on both this node and all connected peers
  that receive the new announcement

**Tier upgrades (e.g. Silver → Gold)** are directly supported by the
Collateral-Churn guard: the guard verifies that the new UTXO is owned by the
same `wallet_address` and, when confirmed, allows the outpoint replacement
without requiring a full deregistration cycle first.

**If the old collateral appears stuck after an upgrade** (shows as locked in
the wallet despite the new tier being active), use:

```bash
time-cli releasecollateral <old_txid> <old_vout>
```

This clears both the in-memory collateral lock and the persistent sled anchor
for the specific outpoint in a single command. Remote nodes self-correct when
they receive the next gossip announcement from your node (within 30 seconds).

---

### Wallet backup is required for collateral recovery

The signed `MasternodeUnlock` message is created using the `masternodeprivkey`
stored in `time.conf`. If you reinstall the daemon on a new server, you must
restore both config files so the daemon can sign the unlock message with the
correct key.

1. **Restore `time.conf`** (contains `masternodeprivkey`) before starting
2. **Restore `masternode.conf`** (contains the collateral outpoint)
3. Comment out the collateral line, then restart — the daemon will broadcast the
   signed `MasternodeUnlock` automatically

**Always back up your wallet file** (needed to spend the collateral UTXO itself):

```bash
cp ~/.timecoin/wallet.dat ~/wallet-backup-$(date +%Y%m%d).dat
# Or for testnet:
cp ~/.timecoin/testnet/wallet.dat ~/wallet-testnet-backup-$(date +%Y%m%d).dat
```

---

### Recovery if the masternode is deleted without deregistering

The collateral UTXO is **never locked at the blockchain protocol level**. The
lock is application-layer tracking inside the daemon, not an on-chain script
constraint. This means the coins are always accessible from the wallet that
holds the private key.

In the typical setup, collateral was sent from a separate GUI wallet (not the
server). That wallet always retains full control of the funds — no action on
the masternode server is required to spend them.

**What happens automatically after deletion:**

| Time after node goes offline | What happens |
|------------------------------|-------------|
| 5 minutes | Gossip reports expire — node becomes inactive |
| 1 hour | Registry auto-removes the node, releases the application-level lock |

After 1 hour the masternode is fully cleaned up from the network's registry.
The collateral UTXO is returned to spendable status in any daemon that was
tracking it.

**Preferred approach — broadcast MasternodeUnlock (immediate):**

If you still have access to the server (or your config files), the cleanest
recovery is to comment out the collateral line in `masternode.conf` and restart
the daemon. The signed `MasternodeUnlock` message propagates to all peers within
15 seconds and they release the lock immediately — no waiting for the 1-hour
auto-expiry.

**Spending the collateral without the server (no waiting needed):**

Since the coins are controlled by the originating wallet, you can spend them
at any time from that wallet. If the daemon on the sending wallet shows the
UTXO as locked, use:

```bash
time-cli unlockorphanedutxos
```

Then send normally. Peers that have not yet received a `MasternodeUnlock` will
auto-deregister within 3 blocks once the UTXO disappears from the chain.

**If you want to re-use the same server IP with new collateral:**
Restart the daemon with the new collateral in `masternode.conf`. The daemon
will broadcast a `MasternodeUnlock` for the old outpoint and register the new
collateral automatically on startup.

---

## Masternode Active Status

### How a Node Becomes (and Stays) Active

The daemon determines whether a masternode is `is_active` using two complementary signals:

**1. Direct TCP Connection (Authoritative)**

If a masternode has an established, post-handshake TCP connection in `PeerConnectionRegistry`, it is considered active regardless of gossip counts. `cleanup_stale_reports()` accepts a `peer_registry` reference and will never flip `is_active = false` for a directly-connected peer. Direct connections are the ground truth.

**2. Gossip-Based Status (Secondary)**

Masternodes broadcast `StatusGossip` messages to the network roughly every 60 seconds. A masternode records its own status locally (self-recording) before broadcasting, so it appears in its own registry from the first cycle. Peers relay these sightings, and the `cleanup_stale_reports()` sweep deactivates nodes that have not been seen within the TTL window.

**Dynamic Minimum-Reports Threshold**

The number of distinct gossip sightings required before a node is considered active scales with network size:

| Network Size | `min_reports` Required |
|--------------|----------------------|
| ≤ 4 nodes | 1 |
| 5–12 nodes | 2 |
| 13+ nodes | 3 |

This prevents premature deactivation on small testnets where only a few peers exist.

### Summary of Active-Status Rules

```
Direct TCP connection present → is_active = true (cleanup cannot override)
No direct connection + gossip sightings ≥ min_reports within TTL → is_active = true
No direct connection + gossip sightings < min_reports or TTL expired → is_active = false
```

---

## Reward Distribution

### Reward Eligibility — Bitmap Gate

Before a masternode can receive tier-pool rewards it must appear in the **`consensus_participants_bitmap`** of the most recent block. This bitmap is built by the block producer from two sources:
1. **Direct voters** — nodes that sent `TimeVotePrepare` or `TimeVotePrecommit` for that block
2. **Gossip-active masternodes** — nodes recorded as active via gossip sightings (~30–60 s accumulation)

Both sets are merged into the bitmap. This ensures pyramid-topology nodes that are not directly connected to the block producer still appear and remain eligible.

**One-block delay for new participants:**
A node must be in block N's bitmap to receive rewards in block N+1. Nodes joining mid-block or with insufficient gossip history at production time will not appear in the current block's bitmap and must wait for the next block cycle.

```
Eligibility chain:
  gossip sighting OR direct TimeVote → bitmap in block N → reward payout in block N+1
```

> **Practical implication:** A newly started masternode may miss the first reward block while gossip propagates (~30–60 s). This is expected behavior, not a bug.

### How Rewards Work

The distribution mode depends on whether any paid-tier (Bronze/Silver/Gold) masternodes are active.

**Tier-Based Mode** (at least one paid-tier node present):

- **5 TIME** → Treasury (on-chain governance fund)
- **30 TIME + fees** → Block producer (VRF-selected leader bonus)
- **65 TIME** → Four per-tier pools (Gold=25, Silver=18, Bronze=14, Free=8)

Within each tier's pool, rewards are divided equally among selected recipients. The block producer also receives their tier's pool share. If a tier has no active nodes, its pool goes to the block producer instead.

**All-Free Mode** (no paid-tier nodes present):

- **5 TIME** → Treasury
- **95 TIME + fees** → Free pool, split equally among up to 25 eligible Free nodes (sorted by fairness bonus, longest-waiting first)

There is no separate producer/leader bonus in all-Free mode — the block producer is simply one of the Free nodes sharing the pool.

### Fairness Rotation

When there are more eligible nodes than the per-block cap of 25, a fairness bonus ensures every node eventually gets paid:

1. **Fairness bonus** per node: `blocks_since_last_paid / 10` — computed on-chain by scanning `masternode_rewards` in recent blocks (up to 1,000 blocks back); deterministic across all validators
2. **Sort** eligible nodes by fairness bonus (descending), then address (ascending) as a deterministic tiebreaker
3. **Select** top 25 nodes; distribute `tier_pool / recipient_count` equally

There is no minimum per-node payout threshold — any share amount, however small, is valid. All nodes in a tier receive payment within `ceil(tier_count / 25)` blocks.

#### Free Tier Maturity Gate

On mainnet, Free-tier nodes must be registered for ≥ 72 blocks (~12 hours) before becoming eligible for pool rewards. Paid tiers (Bronze/Silver/Gold) are always eligible — their collateral acts as sybil resistance.

### Example Scenarios

#### All-Free Network (8 Free nodes, no paid tier)

```
Block producer: Free node C (won VRF)
- Treasury: 5 TIME

Free pool (95 TIME ÷ 8 nodes, sorted by fairness bonus):
- Free node A: 11.875 TIME  (waiting longest)
- Free node B: 11.875 TIME
- Free node C: 11.875 TIME  (producer — no separate leader bonus)
- Free node D: 11.875 TIME
- Free node E: 11.875 TIME
- Free node F: 11.875 TIME
- Free node G: 11.875 TIME
- Free node H: 11.875 TIME

Every node is paid every block.
```

#### Small Tier-Based Network (1 Gold, 2 Silver, 3 Bronze, 4 Free)

```
Block producer: Silver node A (won VRF)
- Treasury: 5 TIME
- Leader bonus: 30 TIME + fees

Gold pool (25 TIME ÷ 1):    Gold A = 25 TIME
Silver pool (18 TIME ÷ 2):  Silver A = 9 TIME (merged with leader = 39 TIME)
                             Silver B = 9 TIME
Bronze pool (14 TIME ÷ 3):  Bronze A = 4.67, B = 4.67, C = 4.66 TIME
Free pool (8 TIME ÷ 4):     Free A = 2, B = 2, C = 2, D = 2 TIME

Every node is paid every block.
```

#### Large Tier-Based Network (5 Gold, 20 Silver, 75 Bronze, 400 Free)

```
Treasury: 5 TIME
Gold pool (25 ÷ 5):     5 TIME each   — all paid every block
Silver pool (18 ÷ 20):  0.9 TIME each — all paid every block
Bronze pool (14 ÷ 25):  0.56 TIME each — top 25 of 75 by fairness
                          All 75 rotate through in 3 blocks
Free pool (8 ÷ 25):     0.32 TIME each — top 25 of 400 by fairness
                          All 400 rotate through in ~16 blocks
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
| `PRODUCER_REWARD_SATOSHIS` | 30 × 10⁸ | Leader bonus (30 TIME, tier-based mode) |
| `TREASURY_REWARD_SATOSHIS` | 5 × 10⁸ | Treasury allocation (5 TIME, both modes) |
| `ALL_FREE_POOL_SATOSHIS` | 95 × 10⁸ | Free pool in all-Free mode (95 TIME) |
| `GOLD_POOL_SATOSHIS` | 25 × 10⁸ | Gold tier pool (25 TIME) |
| `SILVER_POOL_SATOSHIS` | 18 × 10⁸ | Silver tier pool (18 TIME) |
| `BRONZE_POOL_SATOSHIS` | 14 × 10⁸ | Bronze tier pool (14 TIME) |
| `FREE_POOL_SATOSHIS` | 8 × 10⁸ | Free tier pool in tier-based mode (8 TIME) |
| `MAX_TIER_RECIPIENTS` | 25 | Max recipients per tier per block |
| `FREE_MATURITY_BLOCKS` | 72 | Free tier maturity gate (mainnet) |
| `REWARD_VIOLATION_THRESHOLD` | 3 | Strikes before producer is marked misbehaving |

**Key implementation files:** `src/constants.rs` (all reward constants), `src/types.rs` (`MasternodeTier::pool_allocation()` and `sampling_weight()`), `src/blockchain.rs` (`produce_block_at_height()` and `validate_pool_distribution()`), `src/masternode_registry.rs` (`get_eligible_pool_nodes()` and `get_pool_reward_tracking()`).

---

## Block Producer Selection

TIME Coin selects block producers using **weighted VRF sortition** — each masternode's probability of being chosen as leader is proportional to its tier's sampling weight. Higher tiers produce more blocks and earn proportionally more leader bonuses over time, with no participant lists stored in blocks.

### Selection Algorithm

```
1. Collect all active masternodes, sorted deterministically by address
2. Build a cumulative weight array using each node's tier.sampling_weight()
3. Derive a deterministic random value:
   Hash(prev_block_hash || block_height || attempt_number)
4. Select the masternode where the random value falls in the cumulative array
5. Higher tier weight → higher selection probability
```

On timeout the `attempt` counter increments, rotating to a different producer deterministically. Block hash provides all entropy — no external randomness source required.

### Tier Selection Weights

| Tier | Weight | Relative to Bronze |
|------|--------|--------------------|
| Free | 1 | 0.1x |
| Bronze | 10 | 1x (baseline) |
| Silver | 100 | 10x |
| Gold | 1,000 | 100x |

### Selection Probability

```
Probability(node) = node.tier.sampling_weight() / total_network_weight
```

**Example — 5 Free + 1 Bronze (total weight: 15):**
- Each Free node: 1 / 15 = **6.67%**
- Bronze node: 10 / 15 = **66.67%**

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

**Implementation:** `src/main.rs` (cumulative weight array + deterministic selection), `src/masternode_registry.rs`, `src/types.rs` (`MasternodeTier::sampling_weight()`), `src/constants.rs` (weight constants).

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

## Upgrading

### Linux

```bash
cd ~/time-masternode

# Update both networks (default)
sudo bash scripts/update.sh

# Update only testnet
sudo bash scripts/update.sh testnet

# Update only mainnet
sudo bash scripts/update.sh mainnet
```

#### Deep fork recovery (Linux)

If your node ends up on a minority fork after an update, run:

```bash
sudo bash scripts/update.sh resync           # both networks
sudo bash scripts/update.sh resync mainnet   # mainnet only
```

This resets the BFT finality lock and resyncs the full chain from whitelisted peers. The daemon must be running (started automatically if not).

### Windows

```
cd %USERPROFILE%\time-masternode

REM Update both networks
scripts\update.bat

REM Update only testnet
scripts\update.bat testnet

REM Update only mainnet
scripts\update.bat mainnet
```

Both scripts pull the latest code, rebuild, stop the node, copy new binaries,
and restart.

### Automatic Updates (Linux)

To have your node check GitHub for new commits every 30 minutes and update
itself automatically, install the auto-update systemd timer:

```bash
sudo cp scripts/auto-update.service /etc/systemd/system/
sudo cp scripts/auto-update.timer   /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now auto-update.timer
```

The timer fires 5 minutes after boot and then every 30 minutes. It only
calls `update.sh` when it detects a new commit on `origin/main` — if you
are already on the latest version, nothing happens.

**Monitor auto-update activity:**
```bash
# Live log stream
journalctl -t time-auto-update -f

# Timer schedule and last/next run times
systemctl status auto-update.timer

# Run a check immediately (without waiting for the timer)
sudo bash scripts/auto-update.sh

# Test what would happen without making any changes
sudo bash scripts/auto-update.sh --dry-run
```

**Disable auto-updates:**
```bash
sudo systemctl disable --now auto-update.timer
```

> **Note:** Each auto-update triggers a daemon restart and a UTXO reindex
> (same as a manual `update.sh` run). Expect a ~2–5 minute reward gap per
> update. If you prefer to control exactly when updates are applied, keep
> the timer disabled and run `update.sh` manually.

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
**A:** Comment out (or remove) the collateral line in `masternode.conf` and restart the daemon. The daemon broadcasts a signed `MasternodeUnlock` gossip message; all peers release the collateral lock within ~15 seconds. No on-chain transaction is needed.

### Q: What happens if I spend locked collateral?
**A:** Peers tracking your collateral will detect the spent UTXO and auto-deregister your masternode within 3 blocks. The preferred deregistration path is to comment out the collateral line and restart, which broadcasts a signed `MasternodeUnlock` and releases the lock on all peers immediately.

### Q: How long to wait for rewards?
**A:** Depends on total masternodes. With 50 MNs, expect rewards every ~50 minutes.

### Q: Can I change tier after registration?
**A:** Yes. Update `masternode.conf` with the new collateral txid/vout and restart. The daemon broadcasts a `MasternodeUnlock` for the old outpoint and registers with the new collateral. Tier upgrades (e.g. Silver → Gold) are supported directly — the Collateral-Churn guard allows the swap when the new UTXO is owned by the same wallet address.

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
mn1 <collateral_txid> <collateral_vout>
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
- ✅ Signed `MasternodeUnlock` gossip releases collateral on all peers (no on-chain tx needed)
