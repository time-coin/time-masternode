# TIME Coin Node - Quick Reference

## 🚀 Quick Start

### Run with Default Config
```bash
cargo run --release
```

### Generate Configuration File
```bash
cargo run --release -- --generate-config
```

### Run with Custom Config
```bash
cargo run --release -- --config my-config.toml
```

### Run with Custom Port
```bash
cargo run --release -- --listen-addr 0.0.0.0:9999
```

### Enable Verbose Logging
```bash
cargo run --release -- --verbose
```

## 🐳 Docker

### Build Image
```bash
docker build -t timecoin-node .
```

### Run Container
```bash
docker run -d \
  --name timecoin \
  -p 24100:24100 \
  -p 24101:24101 \
  -v $(pwd)/data:/app/data \
  timecoin-node
```

### View Logs
```bash
docker logs -f timecoin
```

### Stop Container
```bash
docker stop timecoin
```

## 🔧 Systemd Service (Linux)

### Install
```bash
# Build release binary first
cargo build --release

# Run installation script
sudo bash install.sh
```

### Manage Service
```bash
# Start
sudo systemctl start timecoin-node

# Stop
sudo systemctl stop timecoin-node

# Restart
sudo systemctl restart timecoin-node

# Status
sudo systemctl status timecoin-node

# Enable auto-start
sudo systemctl enable timecoin-node

# Disable auto-start
sudo systemctl disable timecoin-node
```

### View Logs
```bash
# Follow logs
sudo journalctl -u timecoin-node -f

# Last 100 lines
sudo journalctl -u timecoin-node -n 100

# Today's logs
sudo journalctl -u timecoin-node --since today
```

## ⚙️ Configuration

### Storage Backends
```toml
# In-memory (default, testing only)
[storage]
backend = "memory"

# Persistent (production)
[storage]
backend = "sled"
data_dir = "./data"
```

### Network Settings
```toml
[network]
listen_address = "0.0.0.0:24100"
max_peers = 50
```

### Logging Levels
```toml
[logging]
level = "info"  # trace, debug, info, warn, error
format = "pretty"  # pretty or json
```

## 📊 Monitoring

### Check Node Status
```bash
# Process running?
ps aux | grep time-coin-node

# Listening ports
ss -tlnp | grep time-coin

# Resource usage
top -p $(pgrep time-coin-node)
```

### Network Connectivity
```bash
# Test P2P port
nc -zv localhost 24100

# Test RPC port
nc -zv localhost 24101
```

## 🧪 Development

### Run Tests
```bash
cargo test
```

### Run with Debug Logging
```bash
RUST_LOG=debug cargo run
```

### Format Code
```bash
cargo fmt
```

### Lint Code
```bash
cargo clippy --all-targets
```

### Check Compilation
```bash
cargo check
```

## 🔐 Security

### Firewall Rules (UFW)
```bash
# Allow P2P
sudo ufw allow 24100/tcp

# Allow RPC (localhost only recommended)
sudo ufw allow from 127.0.0.1 to any port 24101
```

### Firewall Rules (iptables)
```bash
# Allow P2P
sudo iptables -A INPUT -p tcp --dport 24100 -j ACCEPT

# Allow RPC from localhost
sudo iptables -A INPUT -p tcp -s 127.0.0.1 --dport 24101 -j ACCEPT
```

## 🐛 Troubleshooting

### Port Already in Use
```bash
# Find process using port
lsof -i :24100

# Kill process
kill -9 <PID>
```

### Permission Denied
```bash
# Check ownership
ls -la /opt/timecoin

# Fix permissions
sudo chown -R timecoin:timecoin /opt/timecoin
```

### Storage Errors
```bash
# Clear data (WARNING: deletes all data)
rm -rf ./data/*

# Check disk space
df -h
```

## 📁 File Locations

### Development
- Binary: `./target/release/time-coin-node`
- Config: `./config.toml`
- Data: `./data/`

### Production (systemd)
- Binary: `/usr/local/bin/time-coin-node`
- Config: `/etc/timecoin/config.toml`
- Data: `/opt/timecoin/data/`
- Logs: `/var/log/timecoin/`

## 🔄 Upgrade Procedure

```bash
# Stop service
sudo systemctl stop timecoin-node

# Backup data
sudo cp -r /opt/timecoin/data /opt/timecoin/data.backup

# Build new version
git pull
cargo build --release

# Install new binary
sudo cp ./target/release/time-coin-node /usr/local/bin/

# Start service
sudo systemctl start timecoin-node

# Check status
sudo systemctl status timecoin-node
```

## 💾 Backup & Restore

### Backup
```bash
# Create backup
tar -czf timecoin-backup-$(date +%Y%m%d).tar.gz \
  /opt/timecoin/data \
  /etc/timecoin/config.toml

# Verify backup
tar -tzf timecoin-backup-*.tar.gz
```

### Restore
```bash
# Stop service
sudo systemctl stop timecoin-node

# Restore data
sudo tar -xzf timecoin-backup-*.tar.gz -C /

# Start service
sudo systemctl start timecoin-node
```

## 🌐 Network Testing

### Test from Another Machine
```bash
# Install netcat
sudo apt install netcat

# Test P2P connection
nc -zv <node-ip> 24100

# Test with telnet
telnet <node-ip> 24100
```

## 📈 Performance Tuning

### Increase File Limits
```bash
# Edit limits
sudo nano /etc/security/limits.conf

# Add these lines
timecoin soft nofile 65536
timecoin hard nofile 65536
```

### Kernel Parameters
```bash
# Edit sysctl
sudo nano /etc/sysctl.conf

# Add these lines
net.core.somaxconn = 4096
net.ipv4.tcp_max_syn_backlog = 8192
```

---

**For full documentation, see README.md**
