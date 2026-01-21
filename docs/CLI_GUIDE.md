# 🔧 time-cli - Bitcoin-like RPC Client

## ✨ Overview

`time-cli` is a command-line tool for interacting with the TIME Coin daemon (`timed`) using Bitcoin-compatible RPC commands.

---

## 🚀 Quick Start

```bash
# Build
cargo build --release

# Basic usage (pretty JSON output by default)
./target/release/time-cli getblockchaininfo

# Compact JSON output (single line)
./target/release/time-cli --compact getblockchaininfo

# Human-readable output
./target/release/time-cli --human getblockchaininfo

# With custom RPC URL
./target/release/time-cli --rpc-url http://192.168.1.100:24101 getnetworkinfo
```

---

## 📊 Output Formats

TIME CLI supports three output formats:

### 1. Pretty JSON (Default)
```bash
time-cli getblockchaininfo
```
Returns formatted JSON (like Bitcoin Core):
```json
{
  "chain": "main",
  "blocks": 1,
  "consensus": "TimeVote",
  "instant_finality": true
}
```

### 2. Compact JSON
```bash
time-cli --compact getblockchaininfo
```
Returns single-line JSON for scripting:
```json
{"chain":"main","blocks":1,"consensus":"TimeVote","instant_finality":true}
```

### 3. Human-Readable
```bash
time-cli --human getblockchaininfo
```
Returns formatted text output:
```
Blockchain Information:
  Chain:            main
  Blocks:           1
  Consensus:        TimeVote
  Instant Finality: true
```

**Supported for --human flag:**
- `getblockchaininfo` - Formatted info
- `getblockcount` - Simple height display
- `getbalance` - Balance with TIME label
- `listunspent` - Table format
- `masternodelist` - Table format
- `masternodestatus` - Formatted status
- `getpeerinfo` - Table format
- `uptime` - Days/hours/minutes/seconds format
- All other commands default to pretty JSON

---

## 📋 Available Commands

### Blockchain Information

#### Get Blockchain Info
```bash
time-cli getblockchaininfo
```
Returns general blockchain information including chain, blocks, consensus type, and finality.

#### Get Block Count
```bash
time-cli getblockcount
```
Returns the current block height.

#### Get Block
```bash
time-cli getblock 1
```
Returns information about a specific block by height.

---

### Network Information

#### Get Network Info
```bash
time-cli getnetworkinfo
```
Returns network information including version, protocol, and connections.

#### Get Peer Info
```bash
time-cli getpeerinfo
```
Returns information about connected peers.

---

### UTXO & Transactions

#### Get UTXO Set Info
```bash
time-cli gettxoutsetinfo
```
Returns statistics about the UTXO set.

#### Get Transaction
```bash
time-cli gettransaction <txid>
```
Returns information about a specific transaction.

#### Get Raw Transaction
```bash
time-cli getrawtransaction <txid>
time-cli getrawtransaction <txid> --verbose
```
Returns raw transaction data.

#### Send Raw Transaction
```bash
time-cli sendrawtransaction <hex>
```
Broadcasts a raw transaction to the network.

#### List Unspent
```bash
time-cli listunspent
time-cli listunspent 6 9999
```
Lists unspent transaction outputs.

---

### Masternode Operations

#### List Masternodes
```bash
time-cli masternodelist
```
Returns list of all masternodes with their status.

#### Masternode Status
```bash
time-cli masternodestatus
```
Returns status of this node's masternode (if configured).

---

### Consensus Information

#### Get Consensus Info
```bash
time-cli getconsensusinfo
```
Returns information about the TimeVote consensus:
- Type (TimeVote)
- Number of masternodes
- Quorum requirements
- Finality time

---

### Wallet Operations

#### Get Balance
```bash
time-cli getbalance
```
Returns wallet balance.

#### Send to Address
```bash
time-cli sendtoaddress <address> <amount>
```
Send TIME to an address.

#### Validate Address
```bash
time-cli validateaddress <address>
```
Validates a TIME Coin address.

#### Merge UTXOs
```bash
time-cli mergeutxos
time-cli mergeutxos --min-count 5 --max-count 50
```
Merge multiple UTXOs into one to reduce UTXO set size.

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
time-cli getmempoolinfo
```
Returns memory pool statistics.

#### Get Raw Mempool
```bash
time-cli getrawmempool
time-cli getrawmempool --verbose
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
time-cli --rpc-url http://node.example.com:24101 getblockcount
```

Or set environment variable:
```bash
export TIME_RPC_URL=http://node.example.com:24101
time-cli getblockcount
```

### Output Format Options

```bash
# Pretty JSON (default) - matches Bitcoin Core
time-cli getbalance

# Compact JSON - single line for scripts
time-cli --compact getbalance

# Human-readable - formatted text output
time-cli --human getbalance
```

---

## 📊 Output Format

All commands return JSON output by default (matching Bitcoin Core):

```json
{
  "chain": "main",
  "blocks": 1,
  "consensus": "TimeVote",
  "instant_finality": true
}
```

**Format Options:**
- Default: Pretty JSON (formatted, multiple lines)
- `--compact`: Single-line JSON for scripting
- `--human`: Human-readable formatted text

---

## 💡 Usage Examples

### Check if node is running
```bash
time-cli uptime
```

### Get consensus status
```bash
time-cli getconsensusinfo
```

### List all masternodes
```bash
time-cli masternodelist
```

### Get blockchain info
```bash
time-cli getblockchaininfo
```

### Check network connections
```bash
time-cli getnetworkinfo
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
BLOCKS=$(time-cli getblockcount)
echo "  Blocks: $BLOCKS"

# Get masternode count
MN_COUNT=$(time-cli masternodelist | jq '. | length')
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
info = rpc_call('getblockchaininfo')
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
| `bitcoin-cli getblockchaininfo` | `time-cli getblockchaininfo` | Identical |
| `bitcoin-cli getblockcount` | `time-cli getblockcount` | Identical |
| `bitcoin-cli getnetworkinfo` | `time-cli getnetworkinfo` | Identical |
| `bitcoin-cli getpeerinfo` | `time-cli getpeerinfo` | Identical |
| `bitcoin-cli gettransaction` | `time-cli gettransaction` | Identical |
| `bitcoin-cli sendrawtransaction` | `time-cli sendrawtransaction` | Identical |
| `bitcoin-cli listunspent` | `time-cli listunspent` | Identical |
| `bitcoin-cli sendtoaddress` | `time-cli sendtoaddress` | Identical |
| `bitcoin-cli stop` | `time-cli stop` | Identical |
| N/A | `time-cli getconsensusinfo` | TIME-specific |
| N/A | `time-cli masternodelist` | TIME-specific |

---

## ⚙️ Advanced Usage

### Chaining Commands
```bash
# Get block count and save to file
time-cli getblockcount > block_height.txt

# Pretty print JSON
time-cli getblockchaininfo | jq .

# Extract specific field
time-cli getconsensusinfo | jq -r '.masternodes'
```

### Monitoring Script
```bash
#!/bin/bash
while true; do
    clear
    echo "=== TIME Coin Node Monitor ==="
    echo "Uptime:      $(time-cli uptime) seconds"
    echo "Blocks:      $(time-cli getblockcount)"
    echo "Peers:       $(time-cli getpeerinfo | jq 'length')"
    echo "Masternodes: $(time-cli masternodelist | jq 'length')"
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
./target/release/time-cli getblockchaininfo
```
