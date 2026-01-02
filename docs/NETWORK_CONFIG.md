# TIME Coin Network Configuration

## Overview

TIME Coin supports two networks with different ports and address prefixes:

| Network | P2P Port | RPC Port | Address Prefix | Magic Bytes |
|---------|----------|----------|----------------|-------------|
| **Mainnet** | 24000 | 24001 | time1 | 0xC01D7E4D ("COLD TIME") |
| **Testnet** | 24100 | 24101 | time1 | 0x7E577E4D ("TEST TIME") |

## Configuration Files

- `config.toml` - Default testnet configuration
- `config.mainnet.toml` - Mainnet configuration template

## Network Type

The network type is specified in the `[node]` section:

```toml
[node]
network = "testnet"  # or "mainnet"
```

## Port Selection

Ports are automatically selected based on the network type:

```toml
[network]
listen_address = "0.0.0.0"  # Auto-selects network-appropriate port

[rpc]
listen_address = "127.0.0.1"  # Auto-selects network-appropriate port
```

You can also explicitly specify ports:

```toml
[network]
listen_address = "0.0.0.0:24100"  # Explicit port for testnet
```

## Address Prefixes

TIME Coin addresses use the `time1` prefix for both networks:

- **Mainnet**: `time1abc...`
- **Testnet**: `time1xyz...`

Both networks use the same address format, but transactions are network-isolated through magic bytes.

## Running Different Networks

### Testnet (Default)

```bash
./target/release/timed --config config.toml
```

Output will show:
```
📡 Network: Testnet
  └─ Magic Bytes: [126, 87, 126, 77]
  └─ Address Prefix: time1
```

### Mainnet

```bash
./target/release/timed --config config.mainnet.toml
```

Output will show:
```
📡 Network: Mainnet
  └─ Magic Bytes: [192, 29, 126, 77]
  └─ Address Prefix: time1
```

## Masternode Configuration

### Free Tier Example

```toml
[masternode]
enabled = true
wallet_address = "TIME0qz9x2k3j4c5m6n7b8v9c0x1z2a3s4d5f6g7h"  # Testnet address
tier = "free"
```

### Paid Tier Example

```toml
[masternode]
enabled = true
wallet_address = "TIME1abc123..." # Mainnet address
collateral_txid = "a1b2c3d4..."
tier = "bronze"  # or "silver", "gold"
```

## Reward Weights

Rewards are proportional to collateral:

| Tier | Collateral | Reward Weight | Can Vote |
|------|------------|---------------|----------|
| Free | 0 TIME | 1 | ❌ No |
| Bronze | 1,000 TIME | 1,000 | ✅ Yes (1x) |
| Silver | 10,000 TIME | 10,000 | ✅ Yes (10x) |
| Gold | 100,000 TIME | 100,000 | ✅ Yes (100x) |

## Network Protocol

Each network uses unique magic bytes to prevent cross-network communication:

```rust
NetworkType::Mainnet.magic_bytes() // [0xC0, 0x1D, 0x7E, 0x4D]
NetworkType::Testnet.magic_bytes() // [0x7E, 0x57, 0x7E, 0x4D]
```

Nodes on different networks will reject each other's messages.

## CLI Commands

```bash
# Generate testnet config
./target/release/timed --gen-config --config my-testnet.toml

# Run testnet node
./target/release/timed --config my-testnet.toml

# Run mainnet node
./target/release/timed --config config.mainnet.toml

# Query blockchain info (auto-detects network)
./target/release/time-cli get-blockchain-info
```

## Peer Discovery

Nodes discover peers from the API endpoint:

```toml
[network]
enable_peer_discovery = true
bootstrap_peers = [
    "seed1.time-coin.io:24100",  # Testnet
    "seed2.time-coin.io:24100",
]
```

For mainnet, use port 24000:

```toml
bootstrap_peers = [
    "seed1.time-coin.io:24000",
    "seed2.time-coin.io:24000",
]
```

## Security

- **Never** mix mainnet and testnet:
  - Testnet coins have no value
  - Address prefixes prevent accidental transfers
  - Magic bytes prevent network cross-talk

- **Always** verify the network before sending transactions:
  - Check address prefix (TIME0 vs TIME1)
  - Verify RPC port matches network
  - Check daemon output for network type

## Storage

Data directories are network-specific:

```toml
[storage]
data_dir = "./data/testnet"  # Testnet
# OR
data_dir = "./data/mainnet"  # Mainnet
```

This prevents blockchain data from being mixed between networks.

## Troubleshooting

### Wrong network connected

**Error**: Peers rejecting connections

**Solution**: Check magic bytes in daemon output match your intended network

### Port already in use

**Error**: `Failed to start network: Address already in use`

**Solution**: Either:
1. Stop other node using that port
2. Change to different port in config
3. Use different network (testnet vs mainnet uses different ports)

### Address prefix mismatch

**Error**: Invalid address format

**Solution**: Verify address starts with:
- `TIME0` for testnet
- `TIME1` for mainnet

## Best Practices

1. **Development**: Always use testnet
2. **Testing**: Use free tier masternode on testnet
3. **Production**: Use mainnet with appropriate collateral
4. **Separate Data**: Keep testnet and mainnet data directories separate
5. **Verify Network**: Always check network type before transactions

## References

- Network Protocol: `docs/NETWORK_PROTOCOL.md`
- Masternode Tiers: `docs/masternodes/TIERS.md`
- API Documentation: `docs/api/README.md`
