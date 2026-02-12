# TIME Coin - Linux Installation Guide

**Version**: 0.2.0  
**Last Updated**: 2026-01-02

---

## Table of Contents

1. [System Requirements](#system-requirements)
2. [Quick Start](#quick-start)
3. [Installation Methods](#installation-methods)
4. [Post-Installation](#post-installation)
5. [Network Configuration](#network-configuration)
6. [Troubleshooting](#troubleshooting)

---

## System Requirements

### Minimum Requirements
- **OS**: Ubuntu 20.04+, Debian 10+, or compatible Linux distribution
- **CPU**: 2 cores
- **RAM**: 2GB
- **Storage**: 10GB free disk space
- **Network**: Stable internet connection

### Recommended for Masternode
- **OS**: Ubuntu 22.04 LTS (recommended)
- **CPU**: 4+ cores
- **RAM**: 4GB+
- **Storage**: 50GB+ SSD
- **Network**: Static IP or domain name

---

## Quick Start

### For Mainnet (Production)

```bash
# Clone the repository
git clone https://github.com/time-coin/timecoin.git
cd timecoin

# Run the installer
chmod +x scripts/install-masternode.sh
sudo ./scripts/install-masternode.sh mainnet

# Check status
systemctl status timed
```

### For Testnet (Development/Testing)

```bash
# Clone the repository
git clone https://github.com/time-coin/timecoin.git
cd timecoin

# Run the installer
chmod +x scripts/install-masternode.sh
sudo ./scripts/install-masternode.sh testnet

# Check status
systemctl status timed
```

---

## Installation Methods

### Method 1: Automated Installation (Recommended)

The automated installation script handles everything for you:
- Installs all dependencies
- Installs Rust toolchain
- Builds binaries from source
- Creates system user and directories
- Configures systemd service
- Sets up firewall rules

**Steps**:

1. **Download the project**:
   ```bash
   git clone https://github.com/time-coin/timecoin.git
   cd timecoin
   ```

2. **Run the installer**:
   ```bash
   # For mainnet
   sudo ./scripts/install-masternode.sh mainnet
   
   # For testnet
   sudo ./scripts/install-masternode.sh testnet
   ```

3. **Follow the prompts**:
   - Installer will ask to start the service
   - Answer `y` to start immediately

**What gets installed**:
- Binaries: `/usr/local/bin/timed`, `/usr/local/bin/time-cli`
- Config: `/root/.timecoin/config.toml` (mainnet) or `/root/.timecoin/testnet/config.toml` (testnet)
- Data: `/root/.timecoin/` (mainnet) or `/root/.timecoin/testnet/` (testnet)
- Service: `/etc/systemd/system/timed.service`

---

### Method 2: Manual Installation

For users who want more control over the installation process.

#### Step 1: Install Dependencies

**Ubuntu/Debian**:
```bash
sudo apt update
sudo apt install -y curl git build-essential libssl-dev pkg-config nasm
```

**Fedora/RHEL**:
```bash
sudo dnf install -y curl git gcc openssl-devel pkgconfig nasm
```

#### Step 2: Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
rustc --version
```

#### Step 3: Build TIME Coin

```bash
# Clone repository
git clone https://github.com/time-coin/timecoin.git
cd timecoin

# Build release binaries
cargo build --release

# Binaries will be in target/release/
ls -lh target/release/timed target/release/time-cli
```

#### Step 4: Install Binaries

```bash
# Copy binaries to system path
sudo cp target/release/timed /usr/local/bin/
sudo cp target/release/time-cli /usr/local/bin/

# Set permissions
sudo chmod +x /usr/local/bin/timed
sudo chmod +x /usr/local/bin/time-cli

# Verify installation
timed --version
time-cli --version
```

#### Step 5: Create Configuration

**For Mainnet**:
```bash
# Create data directory
mkdir -p ~/.timecoin

# Copy config file
cp config.mainnet.toml ~/.timecoin/config.toml

# Edit configuration
nano ~/.timecoin/config.toml
```

**For Testnet**:
```bash
# Create data directory
mkdir -p ~/.timecoin/testnet

# Copy config file
cp config.toml ~/.timecoin/testnet/config.toml

# Edit configuration
nano ~/.timecoin/testnet/config.toml
```

#### Step 6: Create Systemd Service (Optional)

**For Mainnet**:
```bash
sudo tee /etc/systemd/system/timed.service > /dev/null <<EOF
[Unit]
Description=TIME Coin Daemon
After=network.target

[Service]
Type=simple
User=$USER
WorkingDirectory=/home/$USER/.timecoin
ExecStart=/usr/local/bin/timed --config /home/$USER/.timecoin/config.toml
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF
```

**For Testnet**:
```bash
sudo tee /etc/systemd/system/timed.service > /dev/null <<EOF
[Unit]
Description=TIME Coin Daemon (Testnet)
After=network.target

[Service]
Type=simple
User=$USER
WorkingDirectory=/home/$USER/.timecoin/testnet
ExecStart=/usr/local/bin/timed --config /home/$USER/.timecoin/testnet/config.toml
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF
```

**Enable and start service**:
```bash
sudo systemctl daemon-reload
sudo systemctl enable timed
sudo systemctl start timed
sudo systemctl status timed
```

---

## Post-Installation

### 1. Verify Installation

```bash
# Check if service is running
systemctl status timed

# View logs
journalctl -u timed -f

# Check node info
time-cli node info
```

### 2. Create Wallet

```bash
# Create new wallet
time-cli wallet create

# Save the output securely!
# It will display your address and private key
```

**⚠️ IMPORTANT**: Save your private key securely! You cannot recover it later.

### 3. Check Wallet Balance

```bash
time-cli wallet balance <your-address>
```

### 4. Configure Masternode (Optional)

Edit your config file:
```bash
# Mainnet
sudo nano /root/.timecoin/config.toml

# Testnet
sudo nano /root/.timecoin/testnet/config.toml
```

Update the masternode section:
```toml
[masternode]
enabled = true
tier = "free"  # Options: free, bronze, silver, gold
```

Restart the service:
```bash
sudo systemctl restart timed
```

---

## Network Configuration

### Port Configuration

TIME Coin uses different ports for mainnet and testnet:

| Network | P2P Port | RPC Port | Purpose |
|---------|----------|----------|---------|
| Mainnet | 24000    | 24001    | Production network |
| Testnet | 24100    | 24101    | Development/testing |

### Firewall Configuration

**Using UFW (Ubuntu)**:

```bash
# For mainnet
sudo ufw allow 24000/tcp comment 'TIME Coin P2P (mainnet)'

# For testnet
sudo ufw allow 24100/tcp comment 'TIME Coin P2P (testnet)'

# Enable firewall
sudo ufw enable
sudo ufw status
```

**Using firewalld (RHEL/CentOS)**:

```bash
# For mainnet
sudo firewall-cmd --permanent --add-port=24000/tcp
sudo firewall-cmd --reload

# For testnet
sudo firewall-cmd --permanent --add-port=24100/tcp
sudo firewall-cmd --reload
```

**Manual iptables**:

```bash
# For mainnet
sudo iptables -A INPUT -p tcp --dport 24000 -j ACCEPT

# For testnet
sudo iptables -A INPUT -p tcp --dport 24100 -j ACCEPT

# Save rules
sudo iptables-save | sudo tee /etc/iptables/rules.v4
```

### External Address Configuration

If you're running a masternode, configure your external address:

```toml
[network]
external_address = "your-public-ip:24000"  # Or your domain
```

To find your public IP:
```bash
curl ifconfig.me
```

---

## Troubleshooting

### Service Won't Start

**Check logs**:
```bash
journalctl -u timed -n 50 --no-pager
```

**Common issues**:

1. **Port already in use**:
   ```bash
   # Check what's using the port
   sudo lsof -i :24000
   
   # Kill the process if needed
   sudo kill -9 <PID>
   ```

2. **Permission issues**:
   ```bash
   # Check ownership
   ls -la /root/.timecoin/
   
   # Fix ownership
   sudo chown -R timecoin:timecoin /root/.timecoin/
   ```

3. **Missing dependencies**:
   ```bash
   # Reinstall dependencies
   sudo apt install -y libssl-dev pkg-config
   ```

### Build Failures

**Error: "linker `cc` not found"**:
```bash
sudo apt install build-essential
```

**Error: "failed to run custom build command for `openssl-sys`"**:
```bash
sudo apt install libssl-dev pkg-config
```

**Error: "NASM not found"**:
```bash
sudo apt install nasm
```

### Network Issues

**No peers connected**:

1. Check firewall:
   ```bash
   sudo ufw status
   ```

2. Check if port is open:
   ```bash
   nc -zv your-ip 24000
   ```

3. Check external address:
   ```toml
   [network]
   external_address = "your-public-ip:24000"
   ```

4. Check bootstrap peers in config:
   ```toml
   [network]
   bootstrap_peers = [
       "seed1.time-coin.io:24000",
       "seed2.time-coin.io:24000",
   ]
   ```

### Database Issues

**Corrupted database**:
```bash
# Stop service
sudo systemctl stop timed

# Backup data
cp -r ~/.timecoin/blockchain ~/.timecoin/blockchain.backup

# Remove corrupted database
rm -rf ~/.timecoin/blockchain/*

# Start service (will resync)
sudo systemctl start timed
```

### High Memory Usage

**Check memory usage**:
```bash
free -h
htop
```

**Add swap if needed**:
```bash
sudo fallocate -l 2G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile

# Make permanent
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

### Service Management

**View logs in real-time**:
```bash
journalctl -u timed -f
```

**View last 100 lines**:
```bash
journalctl -u timed -n 100 --no-pager
```

**Restart service**:
```bash
sudo systemctl restart timed
```

**Stop service**:
```bash
sudo systemctl stop timed
```

**Check service status**:
```bash
systemctl status timed
```

---

## Upgrading

### Upgrading with Installation Script

```bash
cd timecoin
git pull origin main
sudo ./scripts/install-masternode.sh mainnet
```

The script will detect existing installation and upgrade.

### Manual Upgrade

```bash
# Stop service
sudo systemctl stop timed

# Backup wallet
cp ~/.timecoin/time-wallet.dat ~/time-wallet-backup.dat

# Pull latest code
cd timecoin
git pull origin main

# Rebuild
cargo build --release

# Install new binaries
sudo cp target/release/timed /usr/local/bin/
sudo cp target/release/time-cli /usr/local/bin/

# Start service
sudo systemctl start timed

# Check status
systemctl status timed
```

---

## Uninstalling

### Using Uninstall Script

```bash
cd timecoin
sudo ./scripts/uninstall-masternode.sh mainnet
```

### Manual Uninstall

```bash
# Stop and disable service
sudo systemctl stop timed
sudo systemctl disable timed

# Remove binaries
sudo rm /usr/local/bin/timed
sudo rm /usr/local/bin/time-cli

# Remove service file
sudo rm /etc/systemd/system/timed.service
sudo systemctl daemon-reload

# Remove data (CAUTION: This deletes your wallet!)
# Make sure you have backup of your private key!
rm -rf ~/.timecoin/

# Remove user
sudo userdel timecoin
```

---

## Directory Structure

After installation, your files will be organized as:

### Mainnet
```
/usr/local/bin/
├── timed              # Daemon
└── time-cli           # CLI tool

/root/.timecoin/       # Data directory
├── config.toml        # Configuration
├── time-wallet.dat    # Wallet
├── blockchain/        # Blockchain database
├── blocks/            # Block storage
├── peers/             # Peer cache
└── registry/          # Masternode registry
```

### Testnet
```
/root/.timecoin/testnet/
├── config.toml        # Testnet config
├── time-wallet.dat    # Testnet wallet
├── blockchain/        # Testnet blockchain
├── blocks/            # Testnet blocks
├── peers/             # Testnet peer cache
└── registry/          # Testnet registry
```

---

## Configuration Reference

### Essential Configuration Options

```toml
[node]
network = "mainnet"  # or "testnet"

[network]
listen_address = "0.0.0.0"
external_address = ""  # Set your public IP or domain
max_peers = 50

[rpc]
enabled = true
listen_address = "127.0.0.1"  # Localhost only for security

[storage]
backend = "sled"
data_dir = ""  # Leave empty for automatic
cache_size_mb = 512

[masternode]
enabled = false
tier = "free"
collateral_txid = ""
collateral_vout = 0
```

See [Network Configuration Guide](./NETWORK_CONFIG.md) for full reference.

---

## Security Best Practices

1. **Never expose RPC port to the internet**
   - Keep `rpc.listen_address = "127.0.0.1"`

2. **Secure your private key**
   - Store in encrypted file
   - Never commit to version control
   - Keep offline backup

3. **Keep system updated**
   ```bash
   sudo apt update && sudo apt upgrade -y
   ```

4. **Use firewall**
   - Only open necessary ports (P2P only)
   - Block RPC port from external access

5. **Run as non-root user** (for manual installations)
   - Create dedicated user for the service
   - Don't run as root unless necessary

6. **Enable automatic security updates**
   ```bash
   sudo apt install unattended-upgrades
   sudo dpkg-reconfigure unattended-upgrades
   ```

---

## Support

- **Documentation**: [https://github.com/time-coin/timecoin/tree/main/docs](../README.md)
- **Issues**: [GitHub Issues](https://github.com/time-coin/timecoin/issues)
- **Community**: [time-coin.io](https://time-coin.io)

---

## Additional Resources

- [Installation Scripts README](../scripts/README.md)
- [CLI Guide](../CLI_GUIDE.md)
- [Wallet Commands](../WALLET_COMMANDS.md)
- [Contributing Guide](../CONTRIBUTING.md)

---

**Last Updated**: 2025-12-11  
**Version**: 0.1.0
