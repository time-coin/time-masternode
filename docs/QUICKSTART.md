# Quick Start Guide - TIME Coin Testnet Deployment

**Last Updated:** January 2, 2026  
**Status:** ✅ Production Ready

---

## 📥 Prerequisites

- Rust 1.70+
- 2GB RAM minimum
- 10GB disk space
- Network connectivity (P2P port 24100 for testnet)

---

## 🚀 Quick Build & Run

### 1. Build from Source

```bash
# Clone repository
git clone https://github.com/time-coin/timecoin.git
cd timecoin

# Build release binary
cargo build --release

# Verify build
ls -lh target/release/timed
```

**Expected Output:**
```
-rwxr-xr-x 1 user group 25M Dec 23 03:00 timed
```

### 2. Configuration

The configuration file is automatically created in:
- **Linux/Mac:** `~/.timecoin/config.toml` (testnet: `~/.timecoin/testnet/config.toml`)
- **Windows:** `%APPDATA%\timecoin\config.toml` (testnet: `%APPDATA%\timecoin\testnet\config.toml`)

You can also specify a custom config file with `--config` flag.

**Basic testnet config.toml:**

```toml
[node]
name = "TIME Coin Node"
version = "1.0.0"
network = "testnet"  # or "mainnet"

[network]
listen_address = "0.0.0.0"  # Auto-uses port 24100 for testnet
max_peers = 50
enable_peer_discovery = true
bootstrap_peers = []  # Add seed nodes if available

[rpc]
enabled = true
listen_address = "127.0.0.1"  # Auto-uses port 24101 for testnet

[storage]
backend = "sled"
data_dir = ""  # Auto-configured: ~/.timecoin/testnet/
cache_size_mb = 256

[consensus]
min_masternodes = 3  # Genesis generated dynamically when masternodes register

[logging]
level = "info"
format = "pretty"
output = "stdout"
file_path = "./logs/testnet-node.log"

[masternode]
enabled = false  # Set to true if running a masternode
# tier defaults to "auto" (auto-detected from collateral UTXO value)

[security]
enable_rate_limiting = true
enable_message_signing = true
```

### 3. Run Node

```bash
# Using default config location (~/.timecoin/testnet/config.toml)
./target/release/timed

# Or specify custom config
./target/release/timed --config config.toml

# Expected output
Jan 02 01:00:00 timed[12345]:  INFO 🚀 Starting TIME Coin Node v1.0.0
Jan 02 01:00:00 timed[12345]:  INFO 📁 Data directory: /home/user/.timecoin/testnet
Jan 02 01:00:00 timed[12345]:  INFO 🌐 Network: testnet (P2P: 0.0.0.0:24100, RPC: 127.0.0.1:24101)
Jan 02 01:00:00 timed[12345]:  INFO 🔌 Network server started
Jan 02 01:00:00 timed[12345]:  INFO ⚡ Consensus engine initialized
```

### 4. Verify Node is Running

```bash
# In another terminal, check RPC
curl http://localhost:24101/rpc \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockchaininfo","params":[],"id":1}'

# Expected response
{
  "jsonrpc": "2.0",
  "result": {
    "network": "testnet",
    "blocks": 0,
    "difficulty": 0,
    "headers": 0,
    "mediantime": 1703299200,
    "verificationprogress": 0.0
  },
  "id": 1
}
```

---

## 🖥️ Masternode Setup

### 1. Configure Masternode

Edit config file (`config.toml` or `~/.timecoin/testnet/config.toml`):

```toml
[masternode]
enabled = true
# tier is auto-detected from collateral UTXO value (defaults to "auto")
collateral_txid = ""  # Leave empty for free tier
collateral_vout = 0
```

**Tier Requirements:**
- **free**: No collateral, can receive rewards, cannot vote
- **bronze**: 1,000 TIME collateral (exact), voting enabled
- **silver**: 10,000 TIME collateral (exact), voting enabled  
- **gold**: 100,000 TIME collateral (exact), voting enabled

### 2. For Staked Tiers (Bronze/Silver/Gold)

```bash
# Create collateral UTXO
time-cli sendtoaddress <your_address> 1000.0  # Bronze example

# Wait for confirmations, then note the txid and vout
time-cli listunspent

# Update config.toml with collateral_txid and collateral_vout, then restart
```

### 3. Run as Masternode

```bash
./target/release/timed

# Expected output
Feb 12 01:00:00 timed[12345]:  INFO 🎯 Masternode enabled: free tier
Feb 12 01:00:00 timed[12345]:  INFO 📡 Broadcasting masternode announcement
Feb 12 01:00:00 timed[12345]:  INFO ✅ Registered as active masternode
```

To deregister: set `enabled = false` in config.toml and restart.

---

## 📊 Monitoring & Diagnostics

### Check Node Status

```bash
curl http://localhost:24101/rpc \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getnetworkinfo","params":[],"id":1}'
```

### Check Peer Connections

```bash
curl http://localhost:24101/rpc \
  -X POST \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getpeercount","params":[],"id":1}'
```

### View Logs

Logs are written to the location specified in config or stdout by default:

```bash
# If output = "file" in config
tail -f ./logs/testnet-node.log

# Filter for errors/warnings
grep "ERROR\|WARN" ./logs/testnet-node.log

# View in real-time with journalctl (systemd service)
journalctl -u timed -f
```

---

## 🔗 Multi-Node Network Setup

### Node 1 (Seed Node)

```toml
[node]
network = "testnet"

[network]
listen_address = "0.0.0.0"  # Port 24100
external_address = "192.168.1.100"  # Your public IP
bootstrap_peers = []

[rpc]
listen_address = "127.0.0.1"  # Port 24101

[storage]
data_dir = "./data_node1"
```

Run:
```bash
./target/release/timed --config config_node1.toml
```

### Node 2-N (Regular Nodes)

```toml
[node]
network = "testnet"

[network]
listen_address = "0.0.0.0:24102"
bootstrap_peers = ["192.168.1.100:24100"]

[rpc]
listen_address = "127.0.0.1:24103"

[storage]
data_dir = "./data_node2"
```

Run:
```bash
./target/release/timed --config config_node2.toml
```

### Verify Network

```bash
# On each node (adjust port for each node: 24101, 24103, etc.)
curl http://localhost:24101/rpc -X POST \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getnetworkinfo","params":[],"id":1}' | \
  jq '.result.peer_count'
```

---

## 🧪 Testing & Validation

### Run Unit Tests

```bash
cargo test --all
```

### Run Integration Tests

```bash
./test.sh
```

### Validate Configuration

```bash
cargo check
```

### Lint Code

```bash
cargo clippy
```

### Format Code

```bash
cargo fmt
```

---

## 🚨 Troubleshooting

### Node Won't Connect to Peers

**Cause:** Firewall blocking P2P port

**Solution:**
```bash
# On Linux, open firewall
sudo ufw allow 24100/tcp

# On macOS
sudo /usr/libexec/ApplicationFirewall/socketfilterfw \
  --setglobalstate off
```

### RPC Not Responding

**Cause:** Node not fully initialized

**Solution:** Wait 10-15 seconds and retry

```bash
sleep 15
curl http://localhost:24101/rpc ...
```

### High CPU/Memory Usage

**Cause:** Node catching up with blockchain

**Solution:** Let it sync (may take 1-2 hours for initial sync)

```bash
# Monitor progress
watch -n 5 'curl -s http://localhost:24101/rpc \
  -X POST \
  -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"method\":\"getblockcount\",\"params\":[],\"id\":1}" | \
  jq .result.blocks'
```

### Connection Refused Errors

**Cause:** Another instance running on same port

**Solution:**
```bash
# Find process
lsof -i :24100

# Kill process
kill -9 <PID>

# Or use different port
[network]
p2p_bind = "0.0.0.0:24300"
```

---

## 📈 Performance Tuning

### Increase Peer Connections

```toml
[network]
max_peers = 100  # Default: 50
```

### Increase Block Buffer

```toml
[block]
max_block_size_kb = 2048  # Default: 1024
```

### Reduce Log Verbosity

```toml
[logging]
level = "warn"  # Options: trace, debug, info, warn, error
```

---

## 🔐 Security Checklist

- [ ] Firewall configured (only allow necessary ports)
- [ ] RPC bound to localhost (not 0.0.0.0)
- [ ] Masternode wallet address is secure
- [ ] No sensitive data in logs
- [ ] TLS enabled for P2P communication
- [ ] Message signing enabled
- [ ] Rate limiting enabled

---

## 📚 Additional Resources

- **Protocol Docs**: [docs/TIMECOIN_PROTOCOL_V5.md](docs/TIMECOIN_PROTOCOL_V5.md)
- **Network Architecture**: [docs/NETWORK_ARCHITECTURE.md](docs/NETWORK_ARCHITECTURE.md)
- **Build Status**: [COMPILATION_COMPLETE.md](COMPILATION_COMPLETE.md)
- **Changelog**: [CHANGELOG.md](CHANGELOG.md)

---

## 🆘 Getting Help

- **GitHub Issues**: Report bugs and feature requests
- **Discord**: Join community (link in README)
- **Email**: support@time-coin.io
- **Documentation**: See docs/ directory

---

## 🎯 Next Steps

1. ✅ Build and run node
2. ✅ Verify it connects to network
3. ✅ Check peer connections
4. ⏳ (Optional) Register as masternode
5. ⏳ Monitor node performance
6. ⏳ Join testnet discord for updates

---

**Ready to launch! Happy validating! 🚀**

*Generated: January 2, 2026*  
*For latest updates, see [CHANGELOG.md](CHANGELOG.md)*
