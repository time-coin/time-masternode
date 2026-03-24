# TIME Coin — Linux Installation Guide

**Version**: 1.2.0
**Last Updated**: 2026-03-24

This guide walks you through installing and running a TIME Coin masternode on a
fresh Linux server. Follow it from top to bottom for a production-ready node.

---

## Table of Contents

1. [Prerequisites](#1-prerequisites)
2. [Automated Installation (Recommended)](#2-automated-installation-recommended)
3. [Manual Installation](#3-manual-installation)
4. [Verify Your Node](#4-verify-your-node)
5. [Masternode Configuration](#5-masternode-configuration)
6. [Security Hardening](#6-security-hardening)
7. [Upgrading](#7-upgrading)
8. [Uninstalling](#8-uninstalling)
9. [Directory Layout](#9-directory-layout)
10. [Configuration Reference](#10-configuration-reference)
11. [Troubleshooting](#11-troubleshooting)
12. [Portal Registration (Peer Whitelisting)](#12-portal-registration-peer-whitelisting)

---

## 1. Prerequisites

### Hardware

| Requirement | Minimum | Recommended |
|-------------|---------|-------------|
| OS          | Ubuntu 20.04+, Debian 10+ | Ubuntu 22.04 LTS |
| CPU         | 2 cores | 4+ cores |
| RAM         | 2 GB    | 4 GB+ |
| Disk        | 10 GB   | 50 GB+ SSD |
| Network     | Stable internet | Static IP |

### Software

- `git`, `curl`
- Rust 1.75+ (the install script handles this automatically)

### Network Ports

| Network | P2P Port | RPC Port |
|---------|----------|----------|
| Mainnet | 24000    | 24001    |
| Testnet | 24100    | 24101    |

> **RPC is bound to 127.0.0.1 by default.** Only the P2P port needs to be
> open to the internet.

---

## 2. Automated Installation (Recommended)

The install script handles everything: dependencies, Rust toolchain, building
from source, configuration files, systemd service, and firewall rules.

### Step 1 — Download the source

```bash
git clone https://github.com/time-coin/time-masternode.git
cd time-masternode
```

### Step 2 — Run the installer

```bash
# Testnet (recommended for first-time setup)
sudo ./scripts/install-masternode.sh testnet

# — or —

# Mainnet (production)
sudo ./scripts/install-masternode.sh mainnet
```

### Step 3 — Follow the prompts

The installer will ask two questions:

1. **Reward payout address** — Enter a TIME address to receive masternode
   rewards, or press **Enter** to use the node's built-in wallet address.
   - Testnet addresses start with `TIME0`
   - Mainnet addresses start with `TIME1`
   - The installer validates the address format and network match before
     proceeding.

2. **Start the service now?** — Answer `y` to start immediately.

### Step 4 — Verify

```bash
# Mainnet
systemctl status timed
journalctl -u timed -f          # watch logs (Ctrl+C to stop)

# Testnet
systemctl status timetd
journalctl -u timetd -f

time-cli getblockchaininfo       # query the node
```

### What gets installed

| Component | Mainnet | Testnet |
|-----------|---------|---------|
| Binaries  | `/usr/local/bin/timed`, `/usr/local/bin/time-cli` | (same binaries) |
| Config    | `~/.timecoin/time.conf` | `~/.timecoin/testnet/time.conf` |
| Collateral conf | `~/.timecoin/masternode.conf` | `~/.timecoin/testnet/masternode.conf` |
| Blockchain data | `~/.timecoin/` | `~/.timecoin/testnet/` |
| Systemd service | `/etc/systemd/system/timed.service` | `/etc/systemd/system/timetd.service` |

Both networks can run simultaneously on the same host — the ports and data directories are non-overlapping.

### Running both networks on the same server

You can run mainnet and testnet side by side. Just run the installer twice:

```bash
sudo ./scripts/install-masternode.sh mainnet
sudo ./scripts/install-masternode.sh testnet
```

Each gets its own systemd service, config, and data directory. Manage them
independently:

```bash
systemctl start timed    # mainnet
systemctl start timetd   # testnet
```

On a 2-CPU / 4 GB server, both run comfortably. If memory pressure appears as
the chain grows, reduce the sled cache size in each network's `time.conf`.

That's it — your masternode is running. By default it starts as a **Free tier**
node (no collateral required) and begins earning rewards immediately.

For staked tiers (Bronze/Silver/Gold) with higher rewards, see
[§5 — Masternode Configuration](#5-masternode-configuration).

### Step 5 — Register at the Portal

**This is the final and most important step.** Log in to the TIME Coin Masternode
Portal and submit your node's IP for peer whitelisting:

👉 **[https://time-coin.io/dashboard.html](https://time-coin.io/dashboard.html)**

Without this step your node can still participate in the network, but it will
not appear in the official peer registry and other nodes won't be able to
whitelist it. See [§12 — Portal Registration](#12-portal-registration-peer-whitelisting)
for the full details and what you get from registering.

---

## 3. Manual Installation

For users who prefer full control over the process.

### 3.1 Install system dependencies

**Ubuntu / Debian:**
```bash
sudo apt update
sudo apt install -y curl git build-essential libssl-dev pkg-config nasm
```

**Fedora / RHEL:**
```bash
sudo dnf install -y curl git gcc openssl-devel pkgconfig nasm
```

### 3.2 Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustc --version    # should print 1.75+
```

### 3.3 Build from source

```bash
git clone https://github.com/time-coin/time-masternode.git
cd time-masternode
cargo build --release --bin timed --bin time-cli
```

Build takes roughly one minute. Binaries land in `target/release/`.

### 3.4 Install binaries

```bash
sudo cp target/release/timed /usr/local/bin/
sudo cp target/release/time-cli /usr/local/bin/
sudo chmod +x /usr/local/bin/timed /usr/local/bin/time-cli
```

### 3.5 Create configuration

```bash
# Mainnet
mkdir -p ~/.timecoin
./scripts/deploy-config.sh mainnet

# — or —

# Testnet
mkdir -p ~/.timecoin/testnet
./scripts/deploy-config.sh testnet
```

This generates `time.conf` and `masternode.conf` with sensible defaults. Edit
as needed:

```bash
nano ~/.timecoin/time.conf          # mainnet
nano ~/.timecoin/testnet/time.conf  # testnet
```

### 3.6 Create a systemd service

**Mainnet (`timed`):**
```bash
sudo tee /etc/systemd/system/timed.service > /dev/null <<EOF
[Unit]
Description=TIME Coin Daemon (Mainnet)
After=network.target

[Service]
Type=simple
User=$USER
ExecStart=/usr/local/bin/timed --conf /home/$USER/.timecoin/time.conf
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable timed
sudo systemctl start timed
```

**Testnet (`timetd`):**
```bash
sudo tee /etc/systemd/system/timetd.service > /dev/null <<EOF
[Unit]
Description=TIME Coin Daemon (Testnet)
After=network.target

[Service]
Type=simple
User=$USER
ExecStart=/usr/local/bin/timed --conf /home/$USER/.timecoin/testnet/time.conf --testnet
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable timetd
sudo systemctl start timetd
```

### 3.7 Open the P2P port

```bash
# UFW (Ubuntu)
sudo ufw allow 24000/tcp comment 'TIME Coin P2P'   # mainnet
sudo ufw allow 24100/tcp comment 'TIME Coin P2P'   # testnet

# firewalld (Fedora / RHEL)
sudo firewall-cmd --permanent --add-port=24000/tcp && sudo firewall-cmd --reload
```

> **Do NOT open the RPC port** (24001 / 24101). It is bound to localhost and
> should stay that way.

---

## 4. Verify Your Node

```bash
# Service running? (use timed for mainnet, timetd for testnet)
systemctl status timed
systemctl status timetd

# Blockchain info
time-cli getblockchaininfo

# Connected peers
time-cli getpeerinfo

# Wallet balance
time-cli getbalance

# Masternode status
time-cli masternodelist
```

**Watch logs:**
```bash
journalctl -u timed -f                 # mainnet live stream
journalctl -u timetd -f                # testnet live stream
journalctl -u timed -n 100 --no-pager  # mainnet last 100 lines
journalctl -u timetd -n 100 --no-pager # testnet last 100 lines
```

You should see lines like:
```
INFO 🚀 Starting TIME Coin Node v1.2.0
INFO 📁 Data directory: /root/.timecoin/testnet
INFO 🌐 Network: testnet (P2P: 0.0.0.0:24100, RPC: 127.0.0.1:24101)
INFO ✅ Registered as active masternode
```

### TUI Dashboard

TIME Coin includes a terminal dashboard that displays real-time node status.
It connects to your node's RPC and shows blockchain info, peers, masternode
status, mempool, recent blocks, and governance proposals — all in one view.

**Build and run:**
```bash
cd ~/time-masternode
bash scripts/dashboard.sh
```

Or run directly:
```bash
cargo run --bin time-dashboard --features dashboard
```

**Tabs** (switch with `1`–`5` or left/right arrow keys):

| Tab | Shows |
|-----|-------|
| **Overview** | Chain height, sync progress, wallet balance, peer count, masternode status |
| **Masternode** | Local masternode tier/uptime, full network masternode list with status |
| **Mempool** | Pending and finalized transactions, fee details, transaction inspector |
| **Blocks** | Recent blocks with height, hash, timestamp, tx count, reward |
| **Governance** | Active proposals, voting status, vote directly from the dashboard |

**Keyboard shortcuts:**
- `q` — quit
- `↑`/`↓` — scroll lists
- `Enter` — expand detail view (blocks, mempool transactions)
- `1`–`5` — jump to tab

The dashboard auto-detects whether mainnet or testnet is running by checking
which network has an active `.cookie` file. If both are running, it connects
to whichever was started most recently. RPC credentials are read automatically
from `time.conf`.

---

## 5. Masternode Configuration

### 5.1 Free Tier (default — no collateral)

The installer sets `masternode=1` in `time.conf` automatically. Your node
starts as a Free tier masternode and earns rewards to its built-in wallet
address (or your configured `reward_address`). No further action required.

### 5.2 Changing the reward address

By default, rewards go to the node's auto-generated wallet. To send rewards
to a different address (e.g., your GUI wallet):

```bash
nano ~/.timecoin/time.conf   # or testnet/time.conf
```

Add or edit:
```ini
reward_address=TIME0abc123...   # testnet
reward_address=TIME1xyz789...   # mainnet
```

Restart the service:
```bash
sudo systemctl restart timed    # mainnet
sudo systemctl restart timetd   # testnet
```

The daemon validates the address on startup. If it is malformed or on the
wrong network, it logs a warning and falls back to the local wallet address.

### 5.3 Staked Tiers (Bronze / Silver / Gold)

Staked tiers earn higher rewards and gain governance voting rights.

| Tier   | Exact Collateral | Sampling Weight | Governance Votes |
|--------|-----------------|-----------------|------------------|
| Free   | 0 TIME          | 1×              | None             |
| Bronze | 1,000 TIME      | 10×             | 1                |
| Silver | 10,000 TIME     | 100×            | 10               |
| Gold   | 100,000 TIME    | 1,000×          | 100              |

**To upgrade to a staked tier:**

1. **Generate a masternode key** (optional — auto-generated if omitted):
   ```bash
   time-cli masternode genkey
   ```

2. **Send the exact collateral amount to yourself:**
   ```bash
   time-cli sendtoaddress <your_address> 1000.0   # Bronze example
   ```

3. **Wait for 3 confirmations** (~30 minutes):
   ```bash
   time-cli listunspent
   # Note the txid and vout of the 1000.0 UTXO
   ```

4. **Add collateral info** to `masternode.conf` (same directory as `time.conf`):
   ```
   mn1 <your_public_ip>:24100 <txid> <vout>
   ```

5. **Add the key to `time.conf`** (if you generated one):
   ```ini
   masternodeprivkey=<key from step 1>
   ```

6. **Restart:**
   ```bash
   sudo systemctl restart timed    # mainnet
   sudo systemctl restart timetd   # testnet
   ```

7. **Verify:**
   ```bash
   time-cli getbalance            # shows locked collateral
   time-cli masternodelist        # shows 🔒 Locked
   time-cli listlockedcollaterals
   ```

The daemon auto-detects the tier from the collateral amount.

### 5.4 Deregistering a masternode

Set `masternode=0` in `time.conf` and restart. Collateral is automatically
unlocked and becomes spendable.

### 5.5 Changing tiers

1. Deregister (`masternode=0`, restart)
2. Create a new collateral UTXO for the target tier amount
3. Update `masternode.conf` with the new txid/vout
4. Set `masternode=1` and restart

See **[MASTERNODE_GUIDE.md](MASTERNODE_GUIDE.md)** for full operational
details (reward distribution, collateral validation, rotation, FAQ).

---

## 6. Security Hardening

### Essential

- **RPC is localhost-only** — The daemon binds RPC to `127.0.0.1` by default.
  Never change this to `0.0.0.0` unless you understand the risk (anyone with
  RPC access can drain the wallet).
- **RPC authentication** — Credentials (`rpcuser`/`rpcpassword`) are
  auto-generated in `time.conf` on first run. The CLI reads them automatically
  via the `.cookie` file. Never remove or share these credentials.
- **Wallet password** — A random password is auto-generated and stored in
  `.wallet_password` in the data directory. Back up this file alongside
  `time-wallet.dat`.
- **Firewall** — Only open the P2P port. Block everything else inbound.
- **Keep the OS updated:**
  ```bash
  sudo apt update && sudo apt upgrade -y
  ```
- **Enable automatic security updates:**
  ```bash
  sudo apt install -y unattended-upgrades
  sudo dpkg-reconfigure -plow unattended-upgrades
  ```

### Recommended

- **SSH hardening** — Disable password auth, use key-based login, change the
  default port.
- **Dedicated user** — For manual installs, run the daemon as a non-root
  service account.
- **Wallet backup** — Copy `time-wallet.dat` and `.wallet_password` to a secure
  offline location.
- **Hashed RPC credentials** — Use `rpcauth` instead of plaintext passwords:
  ```bash
  python3 scripts/rpcauth.py myuser
  # Copy the rpcauth= line to time.conf, save the password securely
  # Then remove the rpcuser/rpcpassword lines
  ```
- **RPC TLS** — Enable HTTPS for the RPC interface (required if exposing to a
  network, even a local one):
  ```ini
  rpctls=1
  # Uses auto-generated self-signed cert, or provide your own:
  #rpctlscert=/etc/timecoin/rpc.cert
  #rpctlskey=/etc/timecoin/rpc.key
  ```
- **Monitor logs** — Watch for `WARN` and `ERROR` entries:
  ```bash
  journalctl -u timed -p warning --no-pager -n 50    # mainnet
  journalctl -u timetd -p warning --no-pager -n 50   # testnet
  ```

### Add swap (low-memory servers)

```bash
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

---

## 7. Upgrading

### With update.sh (Recommended)

The `update.sh` script pulls the latest code, rebuilds, and restarts the service:

```bash
cd ~/time-masternode

# Update both networks (default)
sudo bash scripts/update.sh

# Update only testnet
sudo bash scripts/update.sh testnet

# Update only mainnet
sudo bash scripts/update.sh mainnet
```

### With the install script

Re-running the install script on an existing installation upgrades in place
(existing config files are preserved):

```bash
cd time-masternode
git pull origin main
sudo ./scripts/install-masternode.sh mainnet   # or testnet
```

### Manual upgrade

```bash
sudo systemctl stop timed    # mainnet
sudo systemctl stop timetd   # testnet (if running)

# Back up wallet first!
cp ~/.timecoin/time-wallet.dat ~/time-wallet-backup.dat

cd ~/time-masternode
git pull origin main
cargo build --release --bin timed --bin time-cli

sudo cp target/release/timed /usr/local/bin/
sudo cp target/release/time-cli /usr/local/bin/

sudo systemctl start timed
sudo systemctl start timetd   # if testnet was running
systemctl status timed
```

---

## 8. Uninstalling

### With the uninstall script

```bash
cd time-masternode
sudo ./scripts/uninstall-masternode.sh mainnet   # or testnet
```

### Manual uninstall

```bash
# Mainnet
sudo systemctl stop timed
sudo systemctl disable timed
sudo rm /etc/systemd/system/timed.service

# Testnet (if installed)
sudo systemctl stop timetd
sudo systemctl disable timetd
sudo rm /etc/systemd/system/timetd.service

sudo systemctl daemon-reload
sudo rm /usr/local/bin/timed /usr/local/bin/time-cli

# ⚠️ DANGER — this deletes your wallet and all blockchain data
# Back up time-wallet.dat FIRST!
rm -rf ~/.timecoin/
```

---

## 9. Directory Layout

### Mainnet (`~/.timecoin/`)

```
~/.timecoin/
├── time.conf          # Daemon configuration
├── masternode.conf    # Collateral configuration
├── time-wallet.dat    # Wallet (BACK THIS UP)
├── blockchain/        # Blockchain database
├── blocks/            # Block storage
├── peers/             # Peer cache
└── registry/          # Masternode registry
```

### Testnet (`~/.timecoin/testnet/`)

Same structure, nested under the `testnet/` subdirectory.

### Binaries

```
/usr/local/bin/timed       # Daemon
/usr/local/bin/time-cli    # CLI tool
```

---

## 10. Configuration Reference

### `time.conf` (key=value format)

```ini
# ─── Network ─────────────────────────────────────────────────
# Uncomment for testnet
#testnet=1

# Accept incoming connections
listen=1
server=1

# RPC bind address (keep as 127.0.0.1 for security)
rpcbind=127.0.0.1

# RPC authentication (auto-generated on first run — do not remove)
rpcuser=<auto-generated>
rpcpassword=<auto-generated>

# Hashed credentials (alternative to plaintext — use scripts/rpcauth.py)
#rpcauth=user:salt$hash

# TLS encryption for RPC (optional, recommended if exposing to network)
#rpctls=1
#rpctlscert=/path/to/cert.pem
#rpctlskey=/path/to/key.pem

# ─── Masternode ──────────────────────────────────────────────
# Enable masternode mode (0=off, 1=on)
masternode=1

# Masternode private key (optional, auto-generated from wallet if omitted)
#masternodeprivkey=<key from: time-cli masternode genkey>

# Reward payout address (optional, defaults to wallet address)
#reward_address=<TIME address>

# Public IP (auto-detected if omitted)
#externalip=1.2.3.4

# ─── Peers ───────────────────────────────────────────────────
#addnode=seed1.time-coin.io
#addnode=seed2.time-coin.io

# ─── Logging ─────────────────────────────────────────────────
# Options: trace, debug, info, warn, error
debug=info

# ─── Storage ─────────────────────────────────────────────────
txindex=1
```

### `masternode.conf` (collateral entries)

```
# alias  IP:port  collateral_txid  collateral_vout
mn1 1.2.3.4:24000 abc123...def456 0
```

Only needed for staked tiers (Bronze/Silver/Gold). Free tier nodes do not
need a `masternode.conf` entry.

See **[NETWORK_CONFIG.md](NETWORK_CONFIG.md)** for the full configuration
reference.

---

## 11. Troubleshooting

### Service won't start

```bash
journalctl -u timed -n 50 --no-pager
```

| Symptom | Fix |
|---------|-----|
| Port already in use | `sudo lsof -i :24000` → kill the process |
| Permission denied | `sudo chown -R $USER:$USER ~/.timecoin/` |
| Missing `libssl` | `sudo apt install -y libssl-dev` |

### Build failures

| Error | Fix |
|-------|-----|
| `linker 'cc' not found` | `sudo apt install build-essential` |
| `openssl-sys` build failed | `sudo apt install libssl-dev pkg-config` |
| `NASM not found` | `sudo apt install nasm` |

### No peers connecting

1. Check firewall: `sudo ufw status`
2. Test port externally: `nc -zv <your_ip> 24000`
3. Add seed nodes to `time.conf`:
   ```ini
   addnode=seed1.time-coin.io
   addnode=seed2.time-coin.io
   ```
4. Restart: `sudo systemctl restart timed`

### RPC not responding

Wait 10–15 seconds after startup for initialization, then:
```bash
curl -s http://localhost:24101/rpc \
  -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockchaininfo","params":[],"id":1}'
```

### Corrupted database

```bash
sudo systemctl stop timed
cp -r ~/.timecoin/blockchain ~/.timecoin/blockchain.backup
rm -rf ~/.timecoin/blockchain/*
sudo systemctl start timed    # re-syncs from peers
```

### Database Reset

#### Automatic Schema Migration

As of commit `09d5619`, nodes **automatically migrate old-schema blocks** on
startup. In most cases a database reset is no longer necessary:

- ✅ Nodes sync genesis from peers running old code
- ✅ Schema-mismatch `io error:` crashes no longer occur on startup
- ✅ Migration runs before any blockchain operations

You will see log output similar to:
```
🔄 Checking for old-schema blocks that need migration...
✅ Migrated block 0 from old schema
✅ Schema migration complete: 2 blocks migrated
```

#### When to Still Reset

| Situation | Action |
|-----------|--------|
| Migration fails repeatedly | Reset — indicates severe corruption |
| Blocks have invalid data | Reset — migration cannot fix logical errors |
| Want a clean sync | Reset is faster than migrating thousands of blocks |
| Testing / debugging | Reset to a known-good state |

#### Option 1 — Let Automatic Migration Handle It (Recommended)

```bash
cd /path/to/time-masternode
git pull
cargo build --release
sudo systemctl restart timed.service
journalctl -u timed.service -f | grep -i migrat   # verify migration
```

#### Option 2 — Manual Database Reset

**Testnet:**
```bash
sudo systemctl stop timed.service
rm -rf /root/.timecoin/testnet/db/blocks
# Optional full reset: rm -rf /root/.timecoin/testnet/db/*
sudo systemctl start timed.service
journalctl -u timed.service -f
```

**Mainnet:**
```bash
sudo systemctl stop timed.service
rm -rf /root/.timecoin/mainnet/db/blocks
sudo systemctl start timed.service
journalctl -u timed.service -f
```

#### What Happens After a Reset

1. Node starts with an empty database.
2. **Genesis sync** — waits 10 s for peer connections, then requests block 0
   from peers.
3. **Genesis generation (fallback)** — if no network genesis exists, waits 45 s,
   elects the masternode with the lowest address as leader, and the leader
   generates and broadcasts a dynamic genesis block.
4. **Block sync** — requests missing blocks from peers and begins participating
   in block production.

#### Post-Reset Troubleshooting

| Symptom | Fix |
|---------|-----|
| `io error:` still occurring | Verify deletion: `ls -la /root/.timecoin/testnet/db/` · Check disk space: `df -h` · Try `rm -rf /root/.timecoin/testnet/db/*` |
| Nodes stuck at height 0 | Restart one leader node first; let it generate genesis; then restart others |
| Genesis hash mismatch | Incompatible blockchains — all nodes must reset together, or one node keeps its DB and others sync from it |

### Service management quick reference

```bash
# Mainnet (timed)
systemctl status timed
sudo systemctl start timed
sudo systemctl stop timed
sudo systemctl restart timed
journalctl -u timed -f
journalctl -u timed -n 100 --no-pager

# Testnet (timetd)
systemctl status timetd
sudo systemctl start timetd
sudo systemctl stop timetd
sudo systemctl restart timetd
journalctl -u timetd -f
journalctl -u timetd -n 100 --no-pager
```

---

## 12. Portal Registration (Peer Whitelisting)

After your node is up and synced, register it at the TIME Coin Masternode
Portal to be added to the official peer whitelist.

👉 **[https://time-coin.io/dashboard.html](https://time-coin.io/dashboard.html)**

### Why register?

When your node is approved through the portal its IP is published in the
official peer registry (`https://time-coin.io/api/peers`). This unlocks two
concrete benefits:

| Benefit | What it means |
|---------|---------------|
| **Priority inbound connections** | Other nodes and wallets preferentially connect to whitelisted peers when discovering the network. Your node will receive more consistent inbound connections. |
| **Whitelist exemption** | Whitelisted IPs are permanently exempt from rate-limiting and automatic bans. This prevents your node from being cut off during high-traffic periods or after a brief network hiccup. |

Nodes that are *not* registered may still connect and earn rewards, but they
are treated as untrusted peers and are subject to the same rate-limiting and
ban logic as any unknown IP.

### How it works

The registration process has three steps, mirroring what you'll see on the
portal:

1. **Submit** — Log in and fill in your node's public IP, port, tier
   (Free / Bronze / Silver / Gold), and network (mainnet or testnet).
   An optional label helps you identify the node in your dashboard.

2. **Review** — The TIME Coin team reviews your submission, typically within
   **24 hours**. You can track the status (Pending / Approved / Rejected) in
   the "My Masternodes" section of the dashboard.

3. **Activated** — Once approved, your IP is added to the live peer list.
   The `addwhitelist` RPC command verifies against this list before whitelisting
   any IP, so approval is required before other operators can whitelist you.

### Step-by-step

1. Open **[https://time-coin.io/login.html](https://time-coin.io/login.html)**
   and sign in (or create an account) with your email, or via Google.

2. You'll land on the **Masternode Dashboard**. In the *Submit Your Masternode*
   form, enter:
   - **IP address** — your server's public IPv4 address (the same one used for
     the P2P port)
   - **Port** — `24000` for mainnet, `24100` for testnet (pre-filled
     automatically when you select the network)
   - **Tier** — Free, Bronze, Silver, or Gold
   - **Network** — Mainnet or Testnet
   - **Label** *(optional)* — a friendly name, e.g. `vps-fra-01`

3. Click **Submit for Whitelisting** and wait for the review email.

4. Once the status badge changes to **✓ Approved**, your node is live in the
   registry and will begin receiving priority connections on the next peer
   discovery cycle (within a few minutes).

> **Finding your public IP:**
> ```bash
> curl -s https://ifconfig.me
> # or
> curl -s https://api.ipify.org
> ```

### Checking your whitelist status

After approval you can confirm your IP is in the registry:

```bash
# Mainnet
curl -s https://time-coin.io/api/peers | python3 -m json.tool

# Testnet
curl -s https://time-coin.io/api/testnet/peers | python3 -m json.tool
```

Your IP should appear in the returned JSON array.

---

## Further Reading

- **[MASTERNODE_GUIDE.md](MASTERNODE_GUIDE.md)** — Tiers, collateral, rewards, deregistration, FAQ
- **[CLI_GUIDE.md](CLI_GUIDE.md)** — Full command reference
- **[NETWORK_CONFIG.md](NETWORK_CONFIG.md)** — Advanced network configuration
- **[COMPREHENSIVE_SECURITY_AUDIT.md](COMPREHENSIVE_SECURITY_AUDIT.md)** — Security analysis

---

**Version**: 1.2.0
**Last Updated**: 2026-03-24
