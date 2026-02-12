# TIME Coin Wallet Commands

## Sending TIME Coins

### `sendtoaddress`

Send TIME to a specified address.

```bash
# Syntax
time-cli sendtoaddress <address> <amount>

# Example: Send 10 TIME to an address
time-cli sendtoaddress TIME0abc123def456... 10.0

# Example: Send 0.5 TIME
time-cli sendtoaddress TIME0xyz789... 0.5

# Example: Deduct fee from the send amount (sends slightly less than 10)
time-cli sendtoaddress TIME0abc123def456... 10.0 --subtract-fee
```

**Parameters:**
- `address` - The recipient's TIME Coin address
- `amount` - The amount to send in TIME (supports decimal values)
- `--subtract-fee` - (Optional) Deduct fee from the send amount instead of adding it on top

**Returns:**
- Transaction ID (txid) on success

**How it works:**
1. Finds sufficient UTXOs in your wallet to cover the amount + fee
2. Creates a transaction with inputs from your UTXOs
3. Creates output to recipient address
4. Creates change output back to your wallet (if applicable)
5. Broadcasts transaction to the network
6. Consensus engine validates and processes the transaction
7. Network achieves instant finality (<1 second) via TimeVote Protocol
8. Returns the transaction ID

**Fee:**
- 0.1% of input UTXO value (minimum 0.00001 TIME)
- Fee is added on top of the send amount by default
- Use `--subtract-fee` to deduct the fee from the send amount

**Example Output:**
```json
"a1b2c3d4e5f6789012345678901234567890abcdef123456789012345678901234"
```

## Checking Balance

### `getbalance`

Get your wallet's balance.

```bash
time-cli getbalance
```

**Returns:**
```json
{
  "balance": 100.0,
  "pending": 0.0
}
```

## Listing Unspent Outputs

### `listunspent`

List all unspent transaction outputs (UTXOs) in your wallet.

```bash
# List all UTXOs
time-cli listunspent

# With minimum confirmations
time-cli listunspent 1

# With min and max confirmations
time-cli listunspent 1 9999999
```

**Returns:**
```json
[
  {
    "txid": "abc123...",
    "vout": 0,
    "address": "TIME0...",
    "amount": 50.0,
    "confirmations": 10
  }
]
```

## Transaction Details

### `gettransaction`

Get detailed information about a transaction.

```bash
time-cli gettransaction <txid>
```

## Address Validation

### `validateaddress`

Validate a TIME Coin address.

```bash
time-cli validateaddress TIME0abc123...
```

**Returns:**
```json
{
  "isvalid": true,
  "address": "TIME0abc123...",
  "network": "testnet"
}
```

## Network Information

Check the status of your node:

```bash
# Get blockchain info
time-cli getblockchaininfo

# Get network info
time-cli getnetworkinfo

# Get peer connections
time-cli getpeerinfo

# Check if node is a masternode
time-cli masternodestatus
```

## Notes

- All amounts are in TIME (the base unit)
- Transactions achieve instant finality via TimeVote consensus
- Minimum transaction fee: 0.00001 TIME (0.1% of input value)
- UTXOs are locked during transaction processing
- Rejected transactions will unlock UTXOs automatically
- Testnet addresses start with TIME0
- Mainnet addresses start with TIME1
