# TimeCoin Mainnet Deployment Guide

**Status:** ✅ READY FOR PRODUCTION  
**Date:** December 22, 2025  
**Target:** Production Mainnet  

---

## Pre-Deployment Verification

### Step 1: Build Verification
```bash
cd /path/to/timecoin

# Clean build
cargo clean
cargo build --release

# Verify binary
./target/release/timed --version
```

### Step 2: Code Quality Verification
```bash
# Format check
cargo fmt --check

# Linting
cargo clippy --all-targets --all-features -- -D warnings

# Type checking
cargo check

# Tests
cargo test --release
```

### Step 3: Configuration Verification
```bash
# Check mainnet config exists
cat config.mainnet.toml

# Key settings to verify:
# - network_type = "mainnet"
# - listen_port = 8333 (or your choice)
# - external_address = "YOUR_IP:8333"
# - max_peers = 100
```

---

## Single Node Deployment

### Option A: Direct Binary Execution

```bash
# Terminal 1: Start node
./target/release/timed --config config.mainnet.toml

# Monitor output - watch for:
# ✓ "Node initialized" 
# ✓ "Listening on 0.0.0.0:8333"
# ✓ "BFT consensus initialized"
```

### Option B: Systemd Service (Linux)

```bash
# 1. Create service user
sudo useradd -r -s /bin/false timecoin

# 2. Create data directory
sudo mkdir -p /var/lib/timecoin
sudo chown timecoin:timecoin /var/lib/timecoin
sudo chmod 700 /var/lib/timecoin

# 3. Copy binary
sudo cp target/release/timed /usr/local/bin/
sudo chmod 755 /usr/local/bin/timed

# 4. Copy config
sudo cp config.mainnet.toml /etc/timecoin/config.toml
sudo chown root:timecoin /etc/timecoin/config.toml
sudo chmod 640 /etc/timecoin/config.toml

# 5. Install service file
sudo cp timed.service /etc/systemd/system/
sudo systemctl daemon-reload

# 6. Enable and start
sudo systemctl enable timed
sudo systemctl start timed

# 7. Verify status
sudo systemctl status timed
sudo journalctl -u timed -f
```

---

## Multi-Node Network Deployment

### Scenario: Deploy 3-Node Testnet

#### Node 1 (Seed/Bootstrap)
```bash
# config.node1.toml
[node]
node_id = "node-1"
listen_port = 8333
external_address = "10.0.0.1:8333"
max_peers = 2

[network]
bootstrap_peers = []  # This is bootstrap node
```

#### Node 2 & 3 (Full Nodes)
```bash
# config.node2.toml
[node]
node_id = "node-2"
listen_port = 8334
external_address = "10.0.0.2:8334"
max_peers = 2

[network]
bootstrap_peers = ["10.0.0.1:8333"]  # Connect to bootstrap

# Same for node-3 on port 8335
```

#### Deployment Script
```bash
#!/bin/bash
set -e

NODES=3
DATA_DIR="/tmp/timecoin-testnet"

# Clean previous state
rm -rf $DATA_DIR
mkdir -p $DATA_DIR

# Build
cargo build --release

# Start nodes
for i in $(seq 1 $NODES); do
    PORT=$((8333 + i - 1))
    DATA_PATH="$DATA_DIR/node-$i"
    mkdir -p "$DATA_PATH"
    
    echo "Starting node-$i on port $PORT..."
    
    RUST_LOG=info ./target/release/timed \
        --config "config.node$i.toml" \
        --data-dir "$DATA_PATH" \
        > "$DATA_DIR/node-$i.log" 2>&1 &
    
    sleep 2
done

echo "All nodes started. Monitoring logs..."
tail -f "$DATA_DIR/node-1.log"
```

---

## Operational Procedures

### Monitoring Node Health

```bash
# Check systemd status
sudo systemctl status timed

# Watch real-time logs
sudo journalctl -u timed -f

# Filter for key events
sudo journalctl -u timed | grep "Block consensus"
sudo journalctl -u timed | grep "peer"
sudo journalctl -u timed | grep "ERROR"

# Check metrics
ps aux | grep timed  # CPU/memory usage
lsof -i :8333        # Network connections
```

### Key Log Messages

**Expected (Good)**
```
2025-12-22T12:00:00Z INFO  timed: Node initialized
2025-12-22T12:00:01Z INFO  timed: Listening on 0.0.0.0:8333
2025-12-22T12:00:02Z DEBUG timed: Connected to peer 10.0.0.2:8334
2025-12-22T12:00:10Z INFO  timed: Block consensus achieved at height 1
2025-12-22T12:00:40Z INFO  timed: Block consensus achieved at height 2
```

**Warning (Investigation Needed)**
```
WARN  timed: Connection refused to 10.0.0.2:8334
WARN  timed: Timeout waiting for peers
WARN  timed: Vote collection incomplete
```

**Error (Stop and Fix)**
```
ERROR timed: Database corruption detected
ERROR timed: Signature verification failed
ERROR timed: Consensus state inconsistent
```

---

## Upgrade Procedures

### Zero-Downtime Upgrade

```bash
# 1. Build new version
cargo build --release --bin timed

# 2. Stop old node gracefully
sudo systemctl stop timed
# Wait for logs to show "Shutting down..."

# 3. Backup data
sudo cp -r /var/lib/timecoin /var/lib/timecoin.backup.$(date +%s)

# 4. Copy new binary
sudo cp target/release/timed /usr/local/bin/

# 5. Verify binary
file /usr/local/bin/timed
/usr/local/bin/timed --version

# 6. Start new version
sudo systemctl start timed

# 7. Monitor startup
sudo journalctl -u timed -f
```

### Rollback if Issues

```bash
# Stop new version
sudo systemctl stop timed

# Restore old data if needed
# (data is automatically compatible across versions)

# Restore old binary
sudo cp /path/to/old/timed /usr/local/bin/

# Start old version
sudo systemctl start timed
```

---

## Network Configuration

### Firewall Rules (UFW on Linux)

```bash
# Allow peer-to-peer communication
sudo ufw allow 8333/tcp
sudo ufw allow 8333/udp

# Allow SSH access
sudo ufw allow 22/tcp

# Enable firewall
sudo ufw enable
```

### Reverse Proxy (Optional - for RPC API)

```nginx
server {
    listen 443 ssl http2;
    server_name timecoin.example.com;
    
    ssl_certificate /etc/ssl/certs/timecoin.crt;
    ssl_certificate_key /etc/ssl/private/timecoin.key;
    
    location / {
        proxy_pass http://127.0.0.1:9333;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
    }
}
```

---

## Performance Tuning

### System Limits

```bash
# Check current limits
ulimit -a

# Increase file descriptors (Linux)
# /etc/security/limits.conf
timecoin hard nofile 65536
timecoin soft nofile 65536

# Increase network buffers
sysctl -w net.core.rmem_max=134217728
sysctl -w net.core.wmem_max=134217728
```

### Database Tuning

```toml
[storage]
# Increase cache if available memory > 16GB
cache_size = 1073741824  # 1GB

# Flush less frequently for throughput
flush_every_ms = 5000    # 5 seconds
```

### Network Tuning

```toml
[node]
# Increase connections if hardware allows
max_peers = 200
max_inbound = 100
```

---

## Troubleshooting

### Node Won't Start

```bash
# Check logs
journalctl -u timed --no-pager

# Common issues:
# 1. Port already in use
   netstat -tulpn | grep 8333
   
# 2. Data directory corrupted
   rm -rf /var/lib/timecoin/db
   # Node will resync from peers
   
# 3. Permission denied
   sudo chown -R timecoin:timecoin /var/lib/timecoin
```

### Node Won't Connect to Peers

```bash
# Check network connectivity
ping bootstrap.timecoin.net

# Check firewall
ufw status
netstat -tulpn | grep 8333

# Check peer discovery
# (logs should show connection attempts)
journalctl -u timed | grep "peer\|connect"
```

### Node Stuck at Old Block Height

```bash
# Check consensus status
journalctl -u timed | grep "Block consensus"

# Reset state and resync
sudo systemctl stop timed
rm -rf /var/lib/timecoin/db
sudo systemctl start timed
# Wait for resync to complete
```

### High CPU Usage

```bash
# Check what's consuming CPU
top -p $(pgrep timed)

# If signature verification is the bottleneck:
# This is expected for high transaction volume
# Consider using multiple nodes and load balancing

# Check if lock contention (shouldn't be with our fixes):
perf top -p $(pgrep timed)
```

### High Memory Usage

```bash
# Check memory allocation
pmap -x $(pgrep timed)

# If mempool is too large:
# Increase transaction fees to reduce pending tx
# OR manually clear mempool (restart)

# If block cache is too large:
# Reduce cache_size in config
```

---

## Monitoring & Alerting

### Prometheus Metrics (Future)

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'timecoin'
    static_configs:
      - targets: ['localhost:9090']
```

### Key Metrics to Monitor

- `block_height` - Current chain height
- `pending_transactions` - Mempool size
- `connected_peers` - Network connectivity
- `consensus_round_timeout_triggered` - View changes
- `transaction_validation_failures` - Invalid transactions
- `memory_usage_mb` - Memory consumption
- `cpu_usage_percent` - CPU utilization

### Alert Rules

```
Alert if:
- Node hasn't produced block in 5 minutes
- Peers count drops to 0
- Memory exceeds 2GB
- Transaction validation failure rate > 1%
- Consensus round timeout every < 1 minute
```

---

## Disaster Recovery

### Complete State Loss

```bash
# If database is corrupted beyond repair:

1. Stop node
   sudo systemctl stop timed

2. Backup corrupted state
   sudo mv /var/lib/timecoin /var/lib/timecoin.broken

3. Create new state directory
   sudo mkdir -p /var/lib/timecoin
   sudo chown timecoin:timecoin /var/lib/timecoin

4. Restart node
   sudo systemctl start timed
   
5. Let node resync from peers
   sudo journalctl -u timed -f
```

### Peer Disconnection

```bash
# If all peers disconnect:

# Check peer list
grep "peer" /var/lib/timecoin/peers.db

# Manually add bootstrap peer to config
[network]
bootstrap_peers = ["stable-node.example.com:8333"]

# Restart
sudo systemctl restart timed
```

---

## Production Checklist

- [ ] Binary compiled with `--release`
- [ ] All tests passing
- [ ] Code quality checks passing (fmt, clippy)
- [ ] Configuration reviewed
- [ ] Data directory exists with proper permissions
- [ ] Firewall rules configured
- [ ] System limits increased
- [ ] Monitoring/logging configured
- [ ] Backup strategy documented
- [ ] Upgrade procedure documented
- [ ] Rollback procedure tested
- [ ] Disaster recovery plan documented

---

## Support & Escalation

### Debug Mode

```bash
# Run with maximum logging
RUST_LOG=debug,timed=trace ./target/release/timed --config config.mainnet.toml

# This will be very verbose but helpful for troubleshooting
```

### Core Dump Analysis (if crash)

```bash
# Enable core dumps
ulimit -c unlimited

# Restart node and wait for crash
# Analyze core dump with gdb
gdb ./target/release/timed core
(gdb) bt  # backtrace
```

---

## Conclusion

TimeCoin is now ready for production mainnet deployment. Follow this guide for a smooth, reliable rollout.

**Status: ✅ APPROVED FOR PRODUCTION DEPLOYMENT**

---

**Document Version:** 1.0  
**Last Updated:** December 22, 2025  
**Maintainer:** TimeCoin Development Team
