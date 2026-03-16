# Quick Start Guide — Developer Reference

**Last Updated:** March 3, 2026  
**Status:** ✅ Production Ready

> **Deploying to a server?** See
> **[LINUX_INSTALLATION.md](LINUX_INSTALLATION.md)** for the full
> step-by-step production installation guide.

This document is for **developers** who want to build, test, and run TIME Coin
nodes locally.

---

## 📥 Prerequisites

- Rust 1.75+
- 2 GB RAM minimum
- 10 GB disk space

---

## 🚀 Build & Run

### 1. Build from source

```bash
git clone https://github.com/time-coin/time-masternode.git
cd time-masternode
cargo build --release
```

### 2. Run a node

```bash
# Testnet (default if time.conf has testnet=1 or no config exists)
./target/release/timed

# Specify a config file
./target/release/timed --conf path/to/time.conf
```

### 3. Verify the node

```bash
# CLI
./target/release/time-cli getblockchaininfo
./target/release/time-cli getbalance
./target/release/time-cli masternodelist

# Raw RPC
curl http://localhost:24101/rpc \
  -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockchaininfo","params":[],"id":1}'
```

---

## 🧪 Testing

```bash
cargo test               # unit tests
cargo test -- --nocapture # with stdout
cargo test --test edge_cases  # specific integration test
./scripts/test.sh        # full integration suite
cargo bench              # benchmarks
```

### Linting

```bash
cargo fmt                # format code
cargo fmt -- --check     # check only
cargo clippy             # lint
cargo clippy -- -D warnings  # strict
```

---

## 🖥️ Local Multi-Node Network

### Node 1 (seed)

Create `node1/time.conf`:
```ini
testnet=1
listen=1
server=1
masternode=1
externalip=192.168.1.100
debug=info
txindex=1
```

```bash
./target/release/timed --conf node1/time.conf
```

### Node 2–N

Create `node2/time.conf`:
```ini
testnet=1
listen=1
server=1
masternode=1
addnode=192.168.1.100
debug=info
txindex=1
```

```bash
./target/release/timed --conf node2/time.conf
```

### Verify connectivity

```bash
./target/release/time-cli --testnet getpeerinfo
```

---

## 📊 Monitoring

### Dashboard

```bash
./target/release/time-dashboard            # auto-detect network
./target/release/time-dashboard --testnet  # force testnet
```

### Logs

```bash
# If running as systemd service (mainnet=timed, testnet=timetd)
journalctl -u timed -f
journalctl -u timetd -f

# Filter for warnings/errors
journalctl -u timed -p warning --no-pager -n 50
journalctl -u timetd -p warning --no-pager -n 50
```

---

## 🔐 Security Checklist

- [ ] Firewall configured (P2P port only)
- [ ] RPC bound to localhost (not 0.0.0.0)
- [ ] Wallet backed up (`time-wallet.dat`)
- [ ] No secrets in version control
- [ ] Message signing enabled
- [ ] Rate limiting enabled

---

## 📚 Further Reading

- **[LINUX_INSTALLATION.md](LINUX_INSTALLATION.md)** — Production deployment guide
- **[MASTERNODE_GUIDE.md](MASTERNODE_GUIDE.md)** — Masternode operations
- **[CLI_GUIDE.md](CLI_GUIDE.md)** — Full command reference
- **[TIMECOIN_PROTOCOL.md](TIMECOIN_PROTOCOL.md)** — Protocol specification
- **[NETWORK_CONFIG.md](NETWORK_CONFIG.md)** — Advanced configuration

---

*Last Updated: March 3, 2026*
