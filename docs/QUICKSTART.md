# Quick Start Guide - TIME Coin Testnet Deployment

**Last Updated:** December 23, 2024  
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

### 2. Testnet Configuration

Create `config.toml`:

```toml
[node]
network = "testnet"
data_dir = "./data_testnet"
log_level = "info"

[network]
p2p_bind = "0.0.0.0:24100"
rpc_bind = "127.0.0.1:24101"
max_peers = 50
enable_peer_discovery = true
bootstrap_peers = [
    "seed1.time-coin.io:24100",
    "seed2.time-coin.io:24100"
]

[masternode]
enabled = false  # Set to true if running a masternode

[block]
block_time_seconds = 600  # 10 minutes
```

### 3. Run Node

```bash
# Testnet
./target/release/timed --config config.toml

# Expected output
2024-12-23T03:00:00Z  INFO  timecoin: Starting TIME Coin Node v0.1.0
2024-12-23T03:00:00Z  INFO  consensus: Initializing Avalanche + TSDC consensus
2024-12-23T03:00:00Z  INFO  network: Starting network server on 0.0.0.0:24100
2024-12-23T03:00:00Z  INFO  network: Starting network client
2024-12-23T03:00:00Z  INFO  network: 🔌 Connecting to peers...
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

### 1. Generate Wallet Address

```bash
# Create a wallet (requires TIME Coin wallet software)
# For testing, use a test address like:
wallet_address = "tcoin1q2wndu3zk0l0w6hlmlxl7l4c3q0aql5p0r9rqe"
```

### 2. Configure Masternode

Edit `config.toml`:

```toml
[masternode]
enabled = true
tier = "Free"  # Free, Bronze, Silver, or Gold
wallet_address = "your_wallet_address_here"
```

### 3. Run as Masternode

```bash
./target/release/timed --config config.toml

# Expected output
2024-12-23T03:00:00Z  INFO  masternode: 🎯 Registered as Free tier masternode
2024-12-23T03:00:00Z  INFO  masternode: Broadcasting masternode announcement to peers
```

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

```bash
# Real-time logs
tail -f timecoin.log

# Grep for errors
grep "ERROR\|WARN" timecoin.log
```

---

## 🔗 Multi-Node Network Setup

### Node 1 (Seed Node)

```toml
[node]
network = "testnet"
data_dir = "./data_node1"

[network]
p2p_bind = "0.0.0.0:24100"
rpc_bind = "127.0.0.1:24101"
external_address = "192.168.1.100:24100"  # Your IP
bootstrap_peers = []  # No peers to connect to
```

Run:
```bash
./target/release/timed --config config_node1.toml
```

### Node 2-N (Regular Nodes)

```toml
[node]
network = "testnet"
data_dir = "./data_node2"

[network]
p2p_bind = "0.0.0.0:24102"
rpc_bind = "127.0.0.1:24103"
bootstrap_peers = ["192.168.1.100:24100"]  # Connect to Node 1
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
[node]
log_level = "warn"  # Options: debug, info, warn, error
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

*Generated: December 23, 2024*  
*For latest updates, see [CHANGELOG.md](CHANGELOG.md)*
