# TIME Coin Wallet Commands

## Sending TIME Coins

### `sendtoaddress`

Send TIME to a specified address.

```bash
# Syntax
time-cli send-to-address <address> <amount>

# Example: Send 10 TIME to an address
time-cli send-to-address TIME0abc123def456... 10.0

# Example: Send 0.5 TIME
time-cli send-to-address TIME0xyz789... 0.5
```

**Parameters:**
- `address` - The recipient's TIME Coin address (TIME0... for testnet, TIME1... for mainnet)
- `amount` - The amount to send in TIME (supports decimal values)

**Returns:**
- Transaction ID (txid) on success

**How it works:**
1. Finds sufficient UTXOs in your wallet to cover the amount + fee
2. Creates a transaction with inputs from your UTXOs
3. Creates output to recipient address
4. Creates change output back to your wallet (if applicable)
5. Broadcasts transaction to the network
6. Consensus engine validates and processes the transaction
7. Network achieves instant finality (<3 seconds) via BFT voting
8. Returns the transaction ID

**Fee:**
- Fixed fee: 0.00001 TIME per transaction

**Example Output:**
```json
"a1b2c3d4e5f6789012345678901234567890abcdef123456789012345678901234"
```

## Checking Balance

### `get-balance`

Get your wallet's balance.

```bash
time-cli get-balance
```

**Returns:**
```json
{
  "balance": 100.0,
  "pending": 0.0
}
```

## Listing Unspent Outputs

### `list-unspent`

List all unspent transaction outputs (UTXOs) in your wallet.

```bash
# List all UTXOs
time-cli list-unspent

# With minimum confirmations
time-cli list-unspent 1

# With min and max confirmations
time-cli list-unspent 1 9999999
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

### `get-transaction`

Get detailed information about a transaction.

```bash
time-cli get-transaction <txid>
```

## Address Validation

### `validate-address`

Validate a TIME Coin address.

```bash
time-cli validate-address TIME0abc123...
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
time-cli get-blockchain-info

# Get network info
time-cli get-network-info

# Get peer connections
time-cli get-peer-info

# Check if node is a masternode
time-cli masternode-status
```

## Notes

- All amounts are in TIME (the base unit)
- Transactions achieve instant finality via BFT consensus
- Minimum transaction fee: 0.00001 TIME
- UTXOs are locked during transaction processing
- Rejected transactions will unlock UTXOs automatically
- Testnet addresses start with TIME0
- Mainnet addresses start with TIME1
