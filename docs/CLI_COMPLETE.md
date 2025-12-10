# ✅ time-cli - RPC Client Complete!

## 🎉 What You Got

A **Bitcoin-compatible RPC client** for interacting with your TIME Coin daemon!

---

## 📦 Two Binaries Built

1. **`timed`** - The daemon (server)
2. **`time-cli`** - The CLI client

Both are in `target/release/`

---

## 🚀 Quick Test

### 1. Start the daemon
```bash
./target/release/timed
```

### 2. In another terminal, use the CLI
```bash
# Get blockchain info
./target/release/time-cli get-blockchain-info

# Get uptime
./target/release/time-cli uptime

# List masternodes
./target/release/time-cli masternode-list

# Get consensus info
./target/release/time-cli get-consensus-info
```

---

## 📋 Key Commands

```bash
# Blockchain
time-cli get-blockchain-info
time-cli get-block-count

# Network
time-cli get-network-info
time-cli get-peer-info

# Masternodes
time-cli masternode-list
time-cli masternode-status

# Consensus
time-cli get-consensus-info

# Daemon
time-cli uptime
time-cli stop
```

---

## 🎯 Example Output

```bash
$ time-cli get-consensus-info
```

```json
{
  "type": "BFT",
  "masternodes": 3,
  "quorum": 2
}
```

---

## 🔧 Features

- ✅ **20+ RPC commands** (Bitcoin-compatible)
- ✅ **JSON output** (easy to parse)
- ✅ **Custom RPC URL** support
- ✅ **TIME-specific** commands (consensus, masternodes)
- ✅ **Error handling** with clear messages
- ✅ **Help system** (--help on any command)

---

## 📚 Documentation

See **CLI_GUIDE.md** for:
- Complete command reference
- Usage examples
- Integration examples (bash, python)
- Error handling
- Advanced usage

---

## 💡 Usage Pattern

```bash
# Pattern
time-cli [OPTIONS] <COMMAND> [ARGS]

# Examples
time-cli get-block-count
time-cli --rpc-url http://node2:24101 get-network-info
time-cli get-transaction abc123 --verbose
```

---

## 🌐 RPC Server

The daemon automatically starts an RPC server on:
- **Address**: `127.0.0.1:24101`
- **Protocol**: JSON-RPC 2.0
- **Format**: Line-delimited JSON

You'll see in the daemon output:
```
✅ RPC server listening on 127.0.0.1:24101
```

---

## 🎉 Complete Setup

Your TIME Coin node now has:
1. ✅ **Daemon** (`timed`) - BFT consensus, UTXO management
2. ✅ **P2P Network** - Port 24100
3. ✅ **RPC Server** - Port 24101  
4. ✅ **CLI Client** (`time-cli`) - Bitcoin-like commands

**Full blockchain node with professional tooling!** 🚀

---

## 🔥 Try It Now

```bash
# Terminal 1: Start daemon
cargo run --release --bin timed

# Terminal 2: Use CLI
cargo run --release --bin time-cli -- get-blockchain-info
cargo run --release --bin time-cli -- masternode-list
cargo run --release --bin time-cli -- uptime
```

---

**Congratulations! You now have a complete Bitcoin-like CLI for your TIME Coin node!** 🎊
