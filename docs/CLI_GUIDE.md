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
- `masternodelist` / `masternode list` - Table format
- `masternodestatus` / `masternode status` - Formatted status
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

#### List Transactions
```bash
time-cli listtransactions
time-cli listtransactions 20
```
Lists recent wallet transactions (default 10, max specified by count argument). Each entry includes `txid`, `category` (send/receive/consolidation), `amount`, `confirmations`, and `time`. If the transaction contains an encrypted memo that this wallet can decrypt, a `"memo"` field is included in the output.

Block reward distributions include an encrypted `"Block Reward"` memo (self-send encrypted, visible only to the block-producing node).

Example output entry with memo:
```json
{
  "txid": "7ce5821a2faf...",
  "category": "consolidation",
  "amount": -2.59,
  "confirmations": 5,
  "time": 1710441600,
  "memo": "UTXO Consolidation"
}
```

---

### Masternode Operations

#### Generate Masternode Key
```bash
time-cli masternode genkey
```
Generates a new masternode private key (base58check-encoded Ed25519). Add the output to `masternodeprivkey=` in `time.conf`.

#### List Masternodes
```bash
time-cli masternode list
```
Returns list of all masternodes with their status, tier, and collateral lock status.

**Output includes:**
- Address
- Tier (Free, Bronze, Silver, Gold)
- Active status
- Uptime
- Collateral status (🔒 Locked or Legacy)

#### Masternode Status
```bash
time-cli masternode status
```
Returns status of this node's masternode (if configured).

> **Note:** Masternode registration and deregistration are managed via `time.conf` and `masternode.conf`. See the [Masternode Guide](MASTERNODE_GUIDE.md) for details.

> **Backward compatibility:** `masternodelist` and `masternodestatus` are still accepted as aliases.

#### List Locked Collaterals
```bash
time-cli listlockedcollaterals
```
Lists all currently locked collaterals with masternode details.

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

### Governance

On-chain governance allows Bronze/Silver/Gold masternodes to submit proposals and vote on protocol changes. See [GOVERNANCE.md](GOVERNANCE.md) for the full reference.

#### Submit a Proposal

```bash
# Treasury disbursement
time-cli submitproposal treasury <recipient_address> <amount_TIME> "<description>"

# Fee schedule change
time-cli submitproposal feeschedule <new_min_fee_TIME> '<[{"upper":100,"rate_bps":100},...]>'
```

Returns `{"proposal_id":"<64-hex>"}`. The proposal is broadcast to all peers immediately. Requires an unlocked wallet and an active Bronze/Silver/Gold masternode.

#### Vote on a Proposal

```bash
time-cli voteproposal <proposal_id> yes
time-cli voteproposal <proposal_id> no
```

Votes are stake-weighted (Bronze=1, Silver=10, Gold=100). A proposal passes when YES weight ≥ 67% of total active governance weight at the end of the 1,008-block voting window (~1 week).

#### List Proposals

```bash
# All proposals
time-cli listproposals

# Filter by status: active, passed, failed, executed
time-cli listproposals active
```

#### Get Proposal Detail

```bash
time-cli getproposal <proposal_id>
```

Returns full proposal detail including current vote tally, yes/total weight, and quorum percentage.

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
time-cli sendtoaddress <address> <amount> --subtract-fee
time-cli sendtoaddress <address> <amount> --memo "Payment for invoice #42"
```
Send TIME to an address. Fee is tiered (1% for amounts under 100 TIME, 0.5% under 1,000 TIME, 0.25% under 10,000 TIME, 0.1% above), with a flat minimum of 0.01 TIME. Added on top by default. Use `--subtract-fee` to deduct the fee from the send amount instead.

**Minimum send amount: 1 TIME.** Amounts below 1 TIME are rejected at the protocol level (the 0.01 TIME flat fee would represent ≥1% of the amount). Self-sends (UTXO consolidation) are exempt from this minimum.

**Options:**
- `--subtract-fee` — Deduct fee from the send amount (recipient gets amount minus fee)
- `--nowait` — Return TXID immediately without waiting for finality
- `--memo <text>` — Attach an encrypted memo (max 256 chars). The memo is encrypted using ECDH (X25519) + AES-256-GCM so that only the sender and recipient can read it. Other nodes see only ciphertext on-chain.

**Memo notes:** The recipient must have at least one prior on-chain transaction for their public key to be known. If the key is unavailable, the transaction sends without a memo. Memos appear in `listtransactions` output when decryptable.

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

#### Request Payment
```bash
time-cli request-payment 50.0
time-cli request-payment 50.0 --memo "Invoice #42" --label "Alice's Shop"
```
Generate a payment request URI that you can share via email, text, or QR code. The URI includes your address, public key (for encrypted memos), requested amount, and an optional description.

Example output:
```
timecoin:TIME0AsqaMhk...?amount=50&pubkey=a1b2c3...&memo=Invoice%20%2342&label=Alice%27s%20Shop
```

The payer's wallet will automatically cache your public key, enabling encrypted memo support for this and future transactions.

#### Pay a Payment Request
```bash
time-cli pay-request "timecoin:TIME0AsqaMhk...?amount=50&pubkey=a1b2c3...&memo=Invoice%20%2342"
time-cli pay-request "timecoin:TIME0AsqaMhk...?amount=50&pubkey=a1b2c3..." --memo "Custom note"
```
Parse a payment request URI and send the specified amount with an encrypted memo. If the URI contains a memo, it is used automatically. Use `--memo` to override with your own message.

#### Wallet Notes

- All amounts are in TIME (the base unit)
- Transactions achieve instant finality via TimeVote consensus
- Minimum transaction fee: 0.01 TIME (flat floor; tiered % applies for larger amounts)
- Minimum send amount: 1 TIME (non-self-sends only)
- UTXOs are locked during transaction processing; rejected transactions unlock UTXOs automatically
- Testnet addresses start with `TIME0`; mainnet addresses start with `TIME1`

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

### Chain Maintenance & Recovery

These commands are for operators who need to repair state or recover from a fork. All run against the **live daemon** via RPC.

#### Reindex (UTXO + Transaction Index)
```bash
time-cli reindex
```
Rebuilds the UTXO set and transaction index by replaying all blocks from genesis. Use this to fix stale balances after chain corruption. Runs synchronously — the CLI waits for completion.

```bash
time-cli reindextransactions
```
Rebuilds only the transaction index in the background (returns immediately).

#### Deep Fork Recovery
If a node is stuck on a minority fork more than 100 blocks deep, normal reorg logic is blocked by the finality guard. Use the two-step recovery sequence:

```bash
# Step 1 — clear the BFT finality lock
time-cli resetfinalitylock 0

# Step 2 — roll back to genesis and resync from whitelisted peers
time-cli resyncfromwhitelist 0
```

`resyncfromwhitelist` bypasses the MAX_REORG_DEPTH (100-block) limit and re-downloads the canonical chain from trusted peers. Requires at least one whitelisted peer to be connected.

The `update.sh` script wraps this as a single command:
```bash
sudo ./scripts/update.sh resync           # both networks
sudo ./scripts/update.sh resync mainnet   # mainnet only
```

#### Full Chain Reset
```bash
time-cli rollbacktoblock0
```
**Danger.** Deletes all blocks above genesis, clears UTXOs, and resets chain height to 0. The node will re-download the entire chain from peers on restart. Use only when `resyncfromwhitelist` fails (e.g. no whitelisted peers reachable).

#### Rollback to Height
```bash
time-cli rollbacktoheight <height>
```
**Danger.** Rolls back the chain to a specific height (max 100 blocks, enforced by the finality guard). Use `resyncfromwhitelist` for deeper rollbacks.

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
Mainnet: http://127.0.0.1:24001 (default)
Testnet: http://127.0.0.1:24101 (use --testnet flag)
```

### Network Selection
```bash
# Mainnet (default)
time-cli getblockcount

# Testnet
time-cli --testnet getblockcount
```

### Custom RPC URL
```bash
time-cli --rpc-url http://node.example.com:24001 getblockcount
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
time-cli masternode list
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
| N/A | `time-cli masternode genkey` | TIME-specific |
| N/A | `time-cli masternode list` | TIME-specific |

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
