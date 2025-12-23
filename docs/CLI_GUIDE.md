# 🔧 time-cli - Bitcoin-like RPC Client

## ✨ Overview

`time-cli` is a command-line tool for interacting with the TIME Coin daemon (`timed`) using Bitcoin-compatible RPC commands.

---

## 🚀 Quick Start

```bash
# Build
cargo build --release

# Basic usage
./target/release/time-cli get-blockchain-info

# With custom RPC URL
./target/release/time-cli --rpc-url http://192.168.1.100:24101 get-network-info
```

---

## 📋 Available Commands

### Blockchain Information

#### Get Blockchain Info
```bash
time-cli get-blockchain-info
```
Returns general blockchain information including chain, blocks, consensus type, and finality.

#### Get Block Count
```bash
time-cli get-block-count
```
Returns the current block height.

#### Get Block
```bash
time-cli get-block 1
```
Returns information about a specific block by height.

---

### Network Information

#### Get Network Info
```bash
time-cli get-network-info
```
Returns network information including version, protocol, and connections.

#### Get Peer Info
```bash
time-cli get-peer-info
```
Returns information about connected peers.

---

### UTXO & Transactions

#### Get UTXO Set Info
```bash
time-cli get-tx-out-set-info
```
Returns statistics about the UTXO set.

#### Get Transaction
```bash
time-cli get-transaction <txid>
```
Returns information about a specific transaction.

#### Get Raw Transaction
```bash
time-cli get-raw-transaction <txid>
time-cli get-raw-transaction <txid> --verbose
```
Returns raw transaction data.

#### Send Raw Transaction
```bash
time-cli send-raw-transaction <hex>
```
Broadcasts a raw transaction to the network.

#### List Unspent
```bash
time-cli list-unspent
time-cli list-unspent 6 9999
```
Lists unspent transaction outputs.

---

### Masternode Operations

#### List Masternodes
```bash
time-cli masternode-list
```
Returns list of all masternodes with their status.

#### Masternode Status
```bash
time-cli masternode-status
```
Returns status of this node's masternode (if configured).

---

### Consensus Information

#### Get Consensus Info
```bash
time-cli get-consensus-info
```
Returns information about the Avalanche consensus:
- Type (Avalanche)
- Number of masternodes
- Quorum requirements
- Finality time

---

### Wallet Operations

#### Get Balance
```bash
time-cli get-balance
```
Returns wallet balance.

#### Validate Address
```bash
time-cli validate-address <address>
```
Validates a TIME Coin address.

---

### Daemon Control

#### Get Uptime
```bash
time-cli uptime
```
Returns daemon uptime in seconds.

#### Stop Daemon
```bash
time-cli stop
```
Stops the daemon gracefully.

---

### Memory Pool

#### Get Mempool Info
```bash
time-cli get-mempool-info
```
Returns memory pool statistics.

#### Get Raw Mempool
```bash
time-cli get-raw-mempool
time-cli get-raw-mempool --verbose
```
Returns list of transactions in the memory pool.

---

## 🔧 Configuration

### Default RPC URL
```
http://127.0.0.1:24101
```

### Custom RPC URL
```bash
time-cli --rpc-url http://node.example.com:24101 get-block-count
```

Or set environment variable:
```bash
export TIME_RPC_URL=http://node.example.com:24101
time-cli get-block-count
```

---

## 📊 Output Format

All commands return JSON output:

```json
{
  "chain": "main",
  "blocks": 1,
  "consensus": "Avalanche",
  "instant_finality": true
}
```

---

## 💡 Usage Examples

### Check if node is running
```bash
time-cli uptime
```

### Get consensus status
```bash
time-cli get-consensus-info
```

### List all masternodes
```bash
time-cli masternode-list
```

### Get blockchain info
```bash
time-cli get-blockchain-info
```

### Check network connections
```bash
time-cli get-network-info
```

---

## 🔗 Integration Examples

### Bash Script
```bash
#!/bin/bash

# Check if daemon is running
if time-cli uptime > /dev/null 2>&1; then
    echo "✓ Daemon is running"
    UPTIME=$(time-cli uptime)
    echo "  Uptime: $UPTIME seconds"
else
    echo "✗ Daemon is not running"
    exit 1
fi

# Get block count
BLOCKS=$(time-cli get-block-count)
echo "  Blocks: $BLOCKS"

# Get masternode count
MN_COUNT=$(time-cli masternode-list | jq '. | length')
echo "  Masternodes: $MN_COUNT"
```

### Python Script
```python
import subprocess
import json

def rpc_call(method):
    result = subprocess.run(
        ['time-cli', method],
        capture_output=True,
        text=True
    )
    return json.loads(result.stdout)

# Get blockchain info
info = rpc_call('get-blockchain-info')
print(f"Chain: {info['chain']}")
print(f"Blocks: {info['blocks']}")
print(f"Consensus: {info['consensus']}")
```

---

## 🚀 Help Command

```bash
time-cli --help
```

Shows all available commands and options.

```bash
time-cli <command> --help
```

Shows help for a specific command.

---

## 🎯 Comparison with Bitcoin CLI

| Bitcoin CLI | TIME CLI | Notes |
|-------------|----------|-------|
| `bitcoin-cli getblockchaininfo` | `time-cli get-blockchain-info` | Same |
| `bitcoin-cli getblockcount` | `time-cli get-block-count` | Same |
| `bitcoin-cli getnetworkinfo` | `time-cli get-network-info` | Same |
| `bitcoin-cli getpeerinfo` | `time-cli get-peer-info` | Same |
| `bitcoin-cli gettransaction` | `time-cli get-transaction` | Same |
| `bitcoin-cli sendrawtransaction` | `time-cli send-raw-transaction` | Same |
| `bitcoin-cli listunspent` | `time-cli list-unspent` | Same |
| `bitcoin-cli stop` | `time-cli stop` | Same |
| N/A | `time-cli get-consensus-info` | TIME-specific |
| N/A | `time-cli masternode-list` | TIME-specific |

---

## ⚙️ Advanced Usage

### Chaining Commands
```bash
# Get block count and save to file
time-cli get-block-count > block_height.txt

# Pretty print JSON
time-cli get-blockchain-info | jq .

# Extract specific field
time-cli get-consensus-info | jq -r '.masternodes'
```

### Monitoring Script
```bash
#!/bin/bash
while true; do
    clear
    echo "=== TIME Coin Node Monitor ==="
    echo "Uptime:      $(time-cli uptime) seconds"
    echo "Blocks:      $(time-cli get-block-count)"
    echo "Peers:       $(time-cli get-peer-info | jq 'length')"
    echo "Masternodes: $(time-cli masternode-list | jq 'length')"
    sleep 5
done
```

---

## 🔐 Security Notes

- RPC server listens on `127.0.0.1` by default (localhost only)
- For remote access, configure firewall rules carefully
- Consider using SSH tunneling for remote RPC access
- Authentication will be added in future versions

---

## 📝 Error Handling

### Connection Refused
```
Error: HTTP error: connection refused
```
**Solution**: Ensure `timed` daemon is running

### Method Not Found
```
Error: RPC error -32601: Method 'xyz' not found
```
**Solution**: Check command spelling or use `time-cli --help`

### Parse Error
```
Error: RPC error -32700: Parse error
```
**Solution**: Check JSON formatting in parameters

---

## 🎉 Features

- ✅ Bitcoin-compatible RPC interface
- ✅ Easy-to-use command-line interface
- ✅ JSON output for scripting
- ✅ Detailed error messages
- ✅ Tab completion support (with shell config)
- ✅ TIME-specific commands (consensus, masternodes)

---

## 📚 See Also

- `START.md` - How to start the daemon
- `OPERATIONS.md` - Operations guide
- `README.md` - Full documentation

---

**Start using time-cli today!** 🚀

```bash
cargo build --release
./target/release/time-cli get-blockchain-info
```
