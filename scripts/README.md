# Installation Scripts

This directory contains scripts for installing, managing, and uninstalling TIME Coin masternodes on Linux systems.

---

## 📦 Scripts

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

**Usage**:
```bash
# Make executable
chmod +x scripts/install-masternode.sh

# Run as root
sudo ./scripts/install-masternode.sh
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

**Warning**: This will remove everything except blockchain data in `/var/lib/timecoin`.

---

## 📂 Installation Layout

After running `install-masternode.sh`, files will be organized as:

```
/usr/local/bin/
├── timed              # Main daemon
└── time-cli           # CLI tool

/etc/timecoin/
└── config.toml        # Configuration file

/var/lib/timecoin/     # Blockchain data (preserved on uninstall)
├── blockchain/
└── wallets/

/var/log/timecoin/     # Log files
└── timed.log

/etc/systemd/system/
└── timed.service      # Systemd service file
```

---

## 🚀 Quick Start

### 1. Install
```bash
# Clone repository
git clone https://github.com/yourusername/timecoin.git
cd timecoin

# Run installer
sudo ./scripts/install-masternode.sh
```

### 2. Configure
```bash
# Edit configuration
sudo nano /etc/timecoin/config.toml

# Restart service to apply changes
sudo systemctl restart timed
```

### 3. Create Wallet
```bash
# Create new wallet
time-cli wallet create

# Check balance
time-cli wallet balance <your-address>
```

### 4. Monitor
```bash
# Check service status
systemctl status timed

# View logs
journalctl -u timed -f

# Check blockchain height
time-cli node info
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
sudo nano /etc/timecoin/config.toml
# Then restart: sudo systemctl restart timed
```

### Check Disk Usage
```bash
du -sh /var/lib/timecoin
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
- **Firewall**: Configures UFW to allow only P2P port (9333)
- **Resource Limits**: Prevents resource exhaustion

---

## 🐛 Troubleshooting

### Service Won't Start
```bash
# Check logs for errors
journalctl -u timed -n 50 --no-pager

# Check config syntax
timed --config /etc/timecoin/config.toml --check-config

# Verify permissions
ls -la /etc/timecoin/
ls -la /var/lib/timecoin/
```

### Build Fails
```bash
# Ensure dependencies installed
sudo apt-get install build-essential pkg-config libssl-dev nasm

# Check Rust version
rustc --version

# Try manual build
cd /path/to/timecoin
cargo build --release
```

### Port Already in Use
```bash
# Check what's using port 9333
sudo lsof -i :9333

# Kill conflicting process or change port in config
sudo nano /etc/timecoin/config.toml
```

### Firewall Blocking Connections
```bash
# Check UFW status
sudo ufw status

# Allow P2P port
sudo ufw allow 9333/tcp

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

Key configuration options in `/etc/timecoin/config.toml`:

```toml
[network]
# P2P listening address
listen_addr = "0.0.0.0:9333"

# RPC listening address (local only for security)
rpc_addr = "127.0.0.1:9334"

# Seed nodes to connect to
seed_nodes = [
    "seed1.time-coin.io:9333",
    "seed2.time-coin.io:9333"
]

[blockchain]
# Data directory
data_dir = "/var/lib/timecoin"

[logging]
# Log level: trace, debug, info, warn, error
level = "info"

# Log directory
log_dir = "/var/log/timecoin"

[masternode]
# Your masternode reward address
reward_address = "TIME_YOUR_ADDRESS_HERE"

# Masternode tier (1, 2, or 3)
tier = 1
```

---

## 🔄 Upgrading

To upgrade to a new version:

```bash
# Stop service
sudo systemctl stop timed

# Pull latest code
cd /path/to/timecoin
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
- **Tier 1**: 100 TIME
- **Tier 2**: 500 TIME
- **Tier 3**: 1000 TIME

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

**Last Updated**: 2025-12-11
