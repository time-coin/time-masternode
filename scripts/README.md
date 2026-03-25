# Installation and Configuration Scripts

This directory contains scripts for installing, managing, configuring, and uninstalling TIME Coin masternodes on Linux and Windows systems.

---

## 📦 Scripts

### configure-masternode.sh / configure-masternode.bat
Interactive configuration tool for masternode setup.

**Features**:
- ✅ Interactive prompts for all masternode settings
- ✅ Validates all inputs (addresses, txids, vouts)
- ✅ Writes time.conf and masternode.conf in user's data directory
- ✅ Creates backup before making changes
- ✅ Provides next steps after configuration
- ✅ Cross-platform (Linux/macOS via .sh, Windows via .bat)
- ✅ Supports both mainnet and testnet

**Config File Locations**:
- **Linux/macOS Mainnet**: `~/.timecoin/time.conf` + `~/.timecoin/masternode.conf`
- **Linux/macOS Testnet**: `~/.timecoin/testnet/time.conf` + `~/.timecoin/testnet/masternode.conf`
- **Windows Mainnet**: `%APPDATA%\timecoin\time.conf` + `%APPDATA%\timecoin\masternode.conf`
- **Windows Testnet**: `%APPDATA%\timecoin\testnet\time.conf` + `%APPDATA%\timecoin\testnet\masternode.conf`

**Usage (Linux/macOS)**:
```bash
# Make executable
chmod +x scripts/configure-masternode.sh

# Configure mainnet (default)
./scripts/configure-masternode.sh

# Explicitly specify mainnet
./scripts/configure-masternode.sh mainnet

# Configure testnet
./scripts/configure-masternode.sh testnet
```

**Usage (Windows)**:
```cmd
# Configure mainnet (default)
scripts\configure-masternode.bat

# Explicitly specify mainnet
scripts\configure-masternode.bat mainnet

# Configure testnet
scripts\configure-masternode.bat testnet
```

**What it configures**:
1. Enable/disable masternode
2. Masternode tier (Free/Bronze/Silver/Gold)
3. Reward address
4. Collateral UTXO (txid and vout) - optional

**Example Session**:
```
Step 1: Enable Masternode
Do you want to enable masternode functionality? (y/n)
> y

Step 2: Select Masternode Tier
Available tiers:
  - Free:   No collateral (basic rewards, no governance voting)
  - Bronze: 1,000 TIME collateral (10x rewards, governance voting)
  - Silver: 10,000 TIME collateral (100x rewards, governance voting)
  - Gold:   100,000 TIME collateral (1000x rewards, governance voting)

Enter tier (free/bronze/silver/gold):
> bronze

Step 3: Reward Address
Enter your TIME address where you want to receive rewards:
> TIME1abc123...

Step 4: Collateral Information
Enter collateral transaction ID (txid):
> abc123def456... (or leave empty to configure later)

Configuration saved successfully!
```

---

### install-masternode.sh
Automated installation script for fresh Linux machines.

**Features**:
- ✅ Detects and installs all dependencies
- ✅ Installs Rust toolchain if needed
- ✅ Builds binaries from source
- ✅ Creates system user and directories
- ✅ Installs binaries to `/usr/local/bin`
- ✅ Creates systemd service
- ✅ Configures firewall (if UFW present)
- ✅ Security hardening
- ✅ Supports mainnet and testnet

**Usage**:
```bash
# Make executable
chmod +x scripts/install-masternode.sh

# Install for mainnet (default)
sudo ./scripts/install-masternode.sh mainnet

# Install for testnet
sudo ./scripts/install-masternode.sh testnet
```

**Requirements**:
- Ubuntu 20.04+ or Debian 10+ (recommended)
- Root access (sudo)
- Internet connection
- ~2GB free disk space

---

### uninstall-masternode.sh
Clean removal of TIME Coin installation.

**Features**:
- ✅ Stops and disables service
- ✅ Removes binaries and configuration
- ✅ Removes service user
- ⚠️ Preserves blockchain data (optional removal)

**Usage**:
```bash
# Make executable
chmod +x scripts/uninstall-masternode.sh

# Run as root
sudo ./scripts/uninstall-masternode.sh
```

**Warning**: This will remove everything except blockchain data in `/root/.timecoin`.

---

## 📂 Installation Layout

After running `install-masternode.sh`, files will be organized as:

```
/usr/local/bin/
├── timed              # Main daemon
└── time-cli           # CLI tool

/root/.timecoin/       # Mainnet data (when using mainnet)
├── time.conf          # Daemon configuration
├── masternode.conf    # Collateral configuration
├── blockchain/        # Blockchain database
├── wallets/           # Wallet files
└── logs/              # Log files

/root/.timecoin/testnet/  # Testnet data (when using testnet)
├── time.conf          # Testnet daemon configuration
├── masternode.conf    # Testnet collateral configuration
├── blockchain/        # Testnet blockchain database
├── wallets/           # Testnet wallet files
└── logs/              # Testnet log files

/etc/systemd/system/
└── timed.service      # Systemd service file
```

**Network Configuration**:
- **Mainnet**: P2P port 24000, RPC port 24001
- **Testnet**: P2P port 24100, RPC port 24101

---

## 🚀 Quick Start

### Method 1: Using Configuration Script (Recommended)

**Step 1: Run Configuration Tool**
```bash
# Linux/macOS - testnet
./scripts/configure-masternode.sh testnet

# Windows - testnet
scripts\configure-masternode.bat testnet

# Omit argument to default to mainnet
```

**Step 2: Follow Interactive Prompts**
- Enable masternode: Yes
- Select tier: Bronze/Silver/Gold
- Enter reward address
- Enter collateral info (or skip for later)

**Step 3: Create Collateral UTXO** (if not done yet)
```bash
# Send collateral to yourself
time-cli sendtoaddress <your_address> 1000.0

# Wait 30 minutes for confirmations
time-cli listunspent
```

**Step 4: Update masternode.conf**
```
# Format: alias collateral_txid collateral_vout
mn1 <txid from step 3> 0
```

**Step 5: Restart and Verify**
```bash
sudo systemctl restart timed
time-cli masternodelist
time-cli getbalance
```

---

### Method 2: Manual Installation (Linux)

### 1. Install (Mainnet)
```bash
# Clone repository
git clone https://github.com/yourusername/time-masternode.git
cd time-masternode

# Run installer for mainnet
sudo ./scripts/install-masternode.sh mainnet
```

### 1b. Install (Testnet)
```bash
# Run installer for testnet
sudo ./scripts/install-masternode.sh testnet
```

### 2. Configure
```bash
# Edit configuration (mainnet)
sudo nano /root/.timecoin/time.conf

# Edit collateral (mainnet)
sudo nano /root/.timecoin/masternode.conf

# Edit configuration (testnet)
sudo nano /root/.timecoin/testnet/time.conf

# Restart service to apply changes
sudo systemctl restart timed
```

### 3. Get Wallet Info
```bash
# Check balance
time-cli getbalance

# Get wallet info
time-cli getwalletinfo
```

### 4. Monitor
```bash
# Check service status
systemctl status timed

# View logs
journalctl -u timed -f

# Check blockchain height
time-cli getblockcount
```

---

## 🔧 Common Tasks

### Check Service Status
```bash
systemctl status timed
```

### View Logs
```bash
# Live log streaming
journalctl -u timed -f

# Last 100 lines
journalctl -u timed -n 100

# Logs from today
journalctl -u timed --since today
```

### Restart Service
```bash
sudo systemctl restart timed
```

### Stop Service
```bash
sudo systemctl stop timed
```

### Start Service
```bash
sudo systemctl start timed
```

### Edit Configuration
```bash
sudo nano /root/.timecoin/time.conf
# Then restart: sudo systemctl restart timed
```

### Check Disk Usage
```bash
# Check disk usage
du -sh ~/.timecoin
```

---

## 🔒 Security

The installation script implements security best practices:

- **Dedicated User**: Service runs as non-privileged `timecoin` user
- **Restricted Permissions**: Config files readable only by service user
- **Systemd Hardening**: 
  - `NoNewPrivileges=true`
  - `PrivateTmp=true`
  - `ProtectSystem=strict`
  - `ProtectHome=true`
- **Firewall**: Configures UFW to allow only P2P port (24000 mainnet / 24100 testnet)
- **Resource Limits**: Prevents resource exhaustion

---

## 🐛 Troubleshooting

### Service Won't Start
```bash
# Check logs for errors
journalctl -u timed -n 50 --no-pager

# Check config syntax
timed --conf ~/.timecoin/time.conf

# Verify permissions
ls -la ~/.timecoin/
```

### Build Fails
```bash
# Ensure dependencies installed
sudo apt-get install build-essential pkg-config libssl-dev nasm

# Check Rust version
rustc --version

# Try manual build
cd /path/to/time-masternode
cargo build --release
```

### Port Already in Use
```bash
# Check what's using the P2P port
sudo lsof -i :24100

# Kill conflicting process or change port in config
sudo nano /root/.timecoin/testnet/time.conf
```

### Firewall Blocking Connections
```bash
# Check UFW status
sudo ufw status

# Allow P2P port (testnet)
sudo ufw allow 24100/tcp

# Allow P2P port (mainnet)
sudo ufw allow 24000/tcp

# Check iptables
sudo iptables -L -n
```

### High Memory Usage
```bash
# Check memory usage
free -h
htop

# Restart service
sudo systemctl restart timed

# Consider adding swap if needed
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
```

---

## 📝 Configuration Options

Key configuration options in `time.conf` (mainnet: `~/.timecoin/time.conf`, testnet: `~/.timecoin/testnet/time.conf`):

```ini
# Network (uncomment for testnet)
#testnet=1

# Accept connections
listen=1
server=1

# Masternode mode (0=off, 1=on)
masternode=1

# Masternode private key (generate with: time-cli masternode genkey)
#masternodeprivkey=<key>

# Public IP (auto-detected if omitted)
#externalip=1.2.3.4

# Peers
#addnode=seed1.time-coin.io

# Logging: trace, debug, info, warn, error
debug=info

# RPC port (mainnet=24001, testnet=24101)
#rpcport=24101

# Storage
txindex=1
```

Collateral is configured in `masternode.conf`:
```
# Format: alias collateral_txid collateral_vout
mn1 abc123...def456 0
```

---

## 🔄 Upgrading

To upgrade to a new version:

```bash
# Stop service
sudo systemctl stop timed

# Pull latest code
cd /path/to/time-masternode
git pull origin main

# Rebuild and reinstall
sudo ./scripts/install-masternode.sh

# Service will be restarted automatically
```

**Note**: The installer preserves your existing configuration and data.

---

## 📊 System Requirements

### Minimum Requirements
- **CPU**: 1 core
- **RAM**: 1GB
- **Disk**: 20GB SSD
- **Network**: 10 Mbps up/down
- **OS**: Ubuntu 20.04 or Debian 10

### Recommended Requirements
- **CPU**: 2 cores
- **RAM**: 2GB
- **Disk**: 50GB SSD
- **Network**: 100 Mbps up/down
- **OS**: Ubuntu 22.04 LTS

### Masternode Tiers
Different tiers have different collateral requirements:
- **Free**: No collateral (basic rewards)
- **Bronze**: 1,000 TIME
- **Silver**: 10,000 TIME
- **Gold**: 100,000 TIME

See [MASTERNODE_TIERS.md](../docs/MASTERNODE_TIERS.md) for details.

---

## 📞 Support

For issues or questions:

1. Check the logs: `journalctl -u timed -f`
2. Review [troubleshooting](#-troubleshooting) section above
3. Check existing GitHub issues
4. Open a new issue with:
   - Output of `journalctl -u timed -n 100`
   - Your OS version
   - Output of `systemctl status timed`

---

## 📄 License

See [LICENSE](../LICENSE) file in the repository root.

---

**Last Updated**: 2026-03-10

---

## 🛡️ Operations Scripts

### backup-node.sh
Automated backup of blockchain, wallet, and configuration.

**Usage**:
```bash
# Full backup (stops daemon for consistency, restarts after)
sudo bash scripts/backup-node.sh -n testnet

# Wallet + config only (smaller, faster)
sudo bash scripts/backup-node.sh -n testnet --wallet-only

# Hot backup (no downtime, less consistent)
sudo bash scripts/backup-node.sh -n testnet --hot

# Custom output directory
sudo bash scripts/backup-node.sh -n testnet -o /mnt/backups
```

### restore-node.sh
Restore from a backup tarball created by backup-node.sh.

**Usage**:
```bash
# Full restore (auto-detects network from filename)
sudo bash scripts/restore-node.sh /root/timecoin_backup_testnet_20260224.tar.gz

# Wallet-only restore
sudo bash scripts/restore-node.sh backup.tar.gz --wallet-only

# Skip confirmation
sudo bash scripts/restore-node.sh backup.tar.gz -y
```

### health-check.sh
Quick health probe returning exit codes for monitoring tools (cron, Nagios, uptime checks).

**Exit codes**: 0 = Healthy, 1 = Degraded, 2 = Critical

**Usage**:
```bash
# Quick check (human-readable output)
bash scripts/health-check.sh --testnet

# JSON output for automation
bash scripts/health-check.sh --testnet --json

# Quiet mode (exit code only, for cron)
bash scripts/health-check.sh --testnet --quiet

# Custom thresholds
bash scripts/health-check.sh --testnet --max-behind 10 --min-peers 3

# Cron example (alert if not healthy)
*/5 * * * * bash /root/time-masternode/scripts/health-check.sh --testnet --quiet || echo "Node unhealthy" | mail -s "ALERT" admin@example.com
```

### reindex.sh
Safely reindex the blockchain (UTXO + transaction index rebuild).

**Usage**:
```bash
# Transaction index only (fast, non-blocking)
bash scripts/reindex.sh --testnet --tx-only

# Full reindex (UTXOs + tx index, blocks until complete)
bash scripts/reindex.sh --testnet
```

### reset-chain.sh
Delete all blocks and resync from scratch. Preserves wallet, config, and peer data.
Use when a node is hopelessly behind, corrupt, or you want a clean resync.

**What is deleted**: block database, transaction index, UTXO state  
**What is kept**: `time.conf`, `masternode.conf`, `wallet.json`, peer database

**Usage**:
```bash
# Testnet reset (prompts for confirmation)
bash scripts/reset-chain.sh --testnet

# Mainnet reset
bash scripts/reset-chain.sh --mainnet

# Skip confirmation prompt (for automated use)
bash scripts/reset-chain.sh --testnet --yes
```

### node-monitor.sh
Persistent log watcher with color-coded event filtering.

**Usage**:
```bash
# Watch live (Ctrl+C to stop, shows summary)
bash scripts/node-monitor.sh

# Watch with log file output
bash scripts/node-monitor.sh -o /tmp/node_events.log

# Start from further back
bash scripts/node-monitor.sh --since "1 hour ago"
```

**Event categories**:
- 🔴 Errors, panics, crashes
- 🟡 Forks, sync issues, deregistrations
- 🟢 Block production, finalization
- 🔵 Masternode status, collateral changes

---

## 🧪 Testing Scripts

### stress_test.sh
Network stress test that sends transactions at increasing rates to measure finalization performance and find the saturation point.

**Features**:
- ✅ Count-based rate ramp: sends N transactions at each TPS level, then steps up
- ✅ Early stop: halts when >50% send failures at a rate or 10 consecutive finality timeouts
- ✅ Measures send latency (RPC round-trip) and finality time (send → confirmed)
- ✅ Per-rate breakdown with P50/P95/P99 percentiles
- ✅ CSV output for graphing and analysis
- ✅ Reports saturation point and last clean TPS rate

**Usage**:
```bash
# Default: ramp 5→50 TPS (step 5), 20 TX per step = 200 TX total
bash scripts/stress_test.sh --testnet

# Custom rate range with more samples per step
bash scripts/stress_test.sh --testnet -s 5 -m 100 -r 10 -p 30

# Fixed total count (overrides auto-calc)
bash scripts/stress_test.sh --testnet -n 500 -s 10 -m 50

# Disable early stop to run all TXs regardless
bash scripts/stress_test.sh --testnet --no-early-stop

# All options
bash scripts/stress_test.sh --help
```

**CSV Columns**: `tx_seq, txid, target_tps, actual_tps, send_time_unix, send_latency_ms, finality_time_ms, finalized, votes, accumulated_weight, confirmations, error`

---

### test.sh / test.bat
Basic smoke test: starts the daemon, runs common CLI commands, and stops the daemon.

```bash
bash scripts/test.sh
```

### test_transaction.sh
Full TimeVote transaction flow test. Sends 1 TIME to a connected masternode and monitors the entire lifecycle: broadcast → vote collection → finalization (51% threshold) → TimeProof assembly → block archival.

```bash
# Requires: time-cli in PATH, daemon running, at least one connected masternode
bash scripts/test_transaction.sh
```

### test_timevote.sh
TimeVote Protocol validation suite. Tests masternode connectivity, stake-weighted voting, finalization at 51% threshold, and TimeProof certificate structure.

```bash
bash scripts/test_timevote.sh
```

### test_critical_flow.sh
End-to-end critical transaction flow tests covering: transaction creation, broadcast, TimeVote request, finalization, TransactionFinalized broadcast, finalized pool state, block inclusion, fee calculation, finalized pool cleanup, and UTXO state transitions.

```bash
# Requires: time-cli in PATH, daemon running (v1.2.0+)
bash scripts/test_critical_flow.sh
```

### test_finalization_propagation.sh
Multi-node test verifying that transaction finalization propagates to all nodes. Configure the `NODES` array with SSH-accessible hosts, or override via env var.

```bash
# Default: single-node only
bash scripts/test_finalization_propagation.sh

# Multi-node via env var
NODES="root@node1 root@node2 root@node3" bash scripts/test_finalization_propagation.sh
```

### stability_test.sh
72-hour stability test for a 3-node local testnet. Monitors heights across nodes, logs mismatches, and produces a summary report. Assumes nodes are running on RPC ports 24111, 24121, 24131 (created by `setup_local_testnet.sh`).

```bash
bash scripts/stability_test.sh
```

### setup_local_testnet.sh
Creates a 3-node local testnet with separate data directories and configs. Prints the commands needed to start each node.

```bash
bash scripts/setup_local_testnet.sh
```

---

## 🔍 Diagnostic Scripts

### diagnose_forks.sh
Local fork diagnostic: checks daemon status, blockchain height, block hash, peer count, clock sync, P2P port, recent logs, and genesis time–based schedule comparison. Run directly on the node to investigate sync issues.

```bash
bash scripts/diagnose_forks.sh
```

### check_node_state.sh
Per-node state check: service status, blockchain info, recent fork/reorg activity, peer connectivity, and recent log messages. Outputs a health status summary.

```bash
bash scripts/check_node_state.sh
```

### diagnose_status.sh
Quick blockchain, mempool, peer, masternode, and consensus status dump using `time-cli`. Set `CLI_PATH` to specify a non-default binary location.

```bash
bash scripts/diagnose_status.sh
# or: CLI_PATH=/usr/local/bin/time-cli bash scripts/diagnose_status.sh
```

### quick_check.sh
Rapid RPC connectivity check. Tests both testnet (24101) and mainnet (24001) RPC ports and lists listening ports.

```bash
bash scripts/quick_check.sh
```
