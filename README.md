# TIME Coin Protocol Node

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)

A high-performance implementation of the TIME Coin Protocol with instant finality and BFT consensus.

## 🚀 Features

- **Instant Finality**: <3 second transaction confirmation via BFT consensus
- **UTXO State Machine**: Advanced state tracking (Unspent → Locked → SpentPending → SpentFinalized → Confirmed)
- **Masternode Tiers**: Free, Bronze, Silver, Gold tiers with weighted rewards
- **Deterministic Blocks**: 10-minute block generation (52,560 blocks/year)
- **Dual Network Support**: Mainnet and Testnet configurations
- **Real-time RPC API**: Bitcoin-compatible JSON-RPC interface
- **P2P Networking**: Peer discovery and gossip protocol
- **Persistent Storage**: Sled-based blockchain storage

## 📋 Requirements

- Rust 1.70 or higher
- 2GB RAM minimum
- 10GB disk space for full node

## 🛠️ Installation

### From Source

```bash
git clone https://github.com/yourusername/timecoin.git
cd timecoin
cargo build --release
```

### Binaries

The compiled binaries will be in `target/release/`:
- `timed` - TIME Coin daemon
- `time-cli` - Command-line interface

## 🚀 Quick Start

### Run a Full Node (Testnet)

```bash
# Start the daemon
./target/release/timed --network testnet

# Or use the default (mainnet)
./target/release/timed
```

### Run as a Masternode

Edit `config.toml`:

```toml
[masternode]
enabled = true
tier = "Free"  # Free, Bronze, Silver, or Gold
wallet_address = "your_wallet_address_here"
```

Then start:

```bash
./target/release/timed
```

## 💻 CLI Usage

```bash
# Get blockchain info
./target/release/time-cli getblockchaininfo

# Get block count
./target/release/time-cli getblockcount

# List masternodes
./target/release/time-cli listmasternodes

# Get network info
./target/release/time-cli getnetworkinfo

# Get consensus info
./target/release/time-cli getconsensusinfo

# Check uptime
./target/release/time-cli uptime
```

## 🌐 Network Ports

### Mainnet
- P2P: 24100
- RPC: 24101

### Testnet
- P2P: 24200
- RPC: 24201

## 📁 Directory Structure

```
timecoin/
├── src/
│   ├── main.rs              # Entry point
│   ├── config.rs            # Configuration management
│   ├── types.rs             # Core types
│   ├── consensus.rs         # BFT consensus
│   ├── utxo_manager.rs      # UTXO state machine
│   ├── blockchain.rs        # Blockchain storage
│   ├── masternode_registry.rs # Masternode tracking
│   ├── heartbeat_attestation.rs # Uptime verification
│   ├── block/               # Block generation & validation
│   ├── network/             # P2P networking
│   └── rpc/                 # RPC server
├── docs/                    # 📚 Complete documentation
│   └── TIMECOIN_PROTOCOL.md # Full protocol specification
├── analysis/                # Implementation notes
├── config.toml              # Default config
├── config.mainnet.toml      # Mainnet config
└── Cargo.toml               # Dependencies
```

## 📚 Documentation

For complete protocol documentation, see **[docs/TIMECOIN_PROTOCOL.md](docs/TIMECOIN_PROTOCOL.md)**

Key topics covered:
- **[Core Architecture](docs/TIMECOIN_PROTOCOL.md#core-architecture)** - System components and data structures
- **[UTXO State Machine](docs/TIMECOIN_PROTOCOL.md#utxo-state-machine)** - 6-state transaction lifecycle
- **[Instant Finality](docs/TIMECOIN_PROTOCOL.md#instant-finality)** - Sub-3-second settlement
- **[BFT Consensus](docs/TIMECOIN_PROTOCOL.md#bft-consensus)** - Byzantine fault tolerance
- **[Masternode System](docs/TIMECOIN_PROTOCOL.md#masternode-system)** - Tier structure and requirements
- **[Heartbeat Attestation](docs/TIMECOIN_PROTOCOL.md#heartbeat-attestation)** - Peer-verified uptime
- **[Block Production](docs/TIMECOIN_PROTOCOL.md#block-production)** - Deterministic generation
- **[Reward Distribution](docs/TIMECOIN_PROTOCOL.md#reward-distribution)** - Economic model
- **[Network Protocol](docs/TIMECOIN_PROTOCOL.md#network-protocol)** - P2P messaging
- **[Security Model](docs/TIMECOIN_PROTOCOL.md#security-model)** - Threat analysis

## 🏗️ Architecture

### UTXO State Machine

```
Unspent → Locked → SpentPending → SpentFinalized → Confirmed
```

### BFT Consensus

- Quorum: ⌈2n/3⌉ of masternodes
- Vote aggregation in parallel
- Instant finality on quorum reached

### Masternode Tiers

| Tier   | Collateral | Reward Weight | Block Rewards | Governance |
|--------|-----------|---------------|---------------|------------|
| Free   | 0 TIME    | 100           | ✅            | ❌         |
| Bronze | 1,000     | 1,000         | ✅            | ✅         |
| Silver | 10,000    | 10,000        | ✅            | ✅         |
| Gold   | 100,000   | 100,000       | ✅            | ✅         |

*Free tier enables zero-barrier participation. Governance voting requires collateral to prevent Sybil attacks.*

*Free tier enables zero-barrier participation. Governance voting requires collateral to prevent Sybil attacks.*

### Block Rewards

- **Base Reward**: 100 × (1 + ln(n)) TIME per block
  - Scales logarithmically with masternode count
  - Example: 10 nodes = ~330 TIME, 100 nodes = ~560 TIME
- **Distribution**: Proportional to masternode weight
- **Transaction Fees**: Added to block reward
- **All rewards** distributed to masternodes (no treasury/governance allocations)

See [docs/TIMECOIN_PROTOCOL.md#reward-distribution](docs/TIMECOIN_PROTOCOL.md#reward-distribution) for detailed examples.

## 🧪 Testing

```bash
# Run unit tests
cargo test

# Run integration tests
./test.sh

# Format code
cargo fmt

# Lint
cargo clippy
```

## 📝 Configuration

Create `config.toml`:

```toml
[node]
network = "mainnet"  # or "testnet"
data_dir = "./data"
log_level = "info"

[network]
p2p_bind = "0.0.0.0:24100"
rpc_bind = "127.0.0.1:24101"
max_peers = 50

[masternode]
enabled = false
tier = "Free"
wallet_address = ""

[consensus]
min_confirmations = 1
finality_timeout = 3000  # milliseconds
```

## 🛣️ Roadmap

- [x] Core UTXO state machine
- [x] BFT consensus engine
- [x] Deterministic block production
- [x] Masternode tier system
- [x] RPC API
- [x] P2P networking
- [x] Testnet/Mainnet support
- [x] CLI tool
- [x] Peer discovery
- [x] Persistent storage (Sled)
- [ ] WebSocket API
- [ ] Block explorer
- [ ] Signature verification
- [ ] Mobile wallet support
- [ ] Hardware wallet integration
- [ ] Multi-signature support
- [ ] Smart contract layer

## 🤝 Contributing

Contributions are welcome! Please read our [Contributing Guide](CONTRIBUTING.md) first.

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 📜 License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## 🔗 Links

- Website: [https://time-coin.io](https://time-coin.io)
- Documentation: [https://docs.time-coin.io](https://docs.time-coin.io)
- Block Explorer: [https://explorer.time-coin.io](https://explorer.time-coin.io)
- Discord: [https://discord.gg/timecoin](https://discord.gg/timecoin)

## 📞 Support

- GitHub Issues: [Report a bug](https://github.com/yourusername/timecoin/issues)
- Discord: Join our community server
- Email: support@time-coin.io

## ⚠️ Disclaimer

This is experimental software. Use at your own risk. Always test on testnet first.

---

Made with ❤️ by the TIME Coin community
