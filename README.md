# TIME Coin Protocol Node

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)

A high-performance implementation of the TIME Coin Protocol v5 with sub-second instant finality via Avalanche consensus and deterministic block checkpointing.

## 🚀 Features

- **Instant Finality**: <1 second transaction confirmation via Avalanche Snowball consensus
- **Deterministic Checkpointing**: 10-minute blocks with TSDC (Time-Scheduled Deterministic Consensus)
- **Leaderless Consensus**: No BFT voting rounds or global committees
- **Stake-Weighted Sampling**: Sybil resistance via collateral-based peer selection
- **UTXO State Machine**: Advanced state tracking (Unspent → Locked → Sampling → Finalized → Archived)
- **Masternode Tiers**: Free, Bronze, Silver, Gold tiers with weighted sampling power
- **Dual Network Support**: Mainnet and Testnet configurations
- **Real-time RPC API**: Bitcoin-compatible JSON-RPC interface
- **P2P Networking**: Peer discovery and gossip protocol
- **Persistent Storage**: Sled-based blockchain storage

## ✅ Build Status

- **Compilation**: ✅ COMPLETE (Zero errors)
- **Latest Build**: December 23, 2024
- **Build Time**: ~1 minute (release profile)
- **Network Modules**: ✅ Consolidated and optimized
  - Lock-free connection management (DashMap)
  - Bootstrap peer discovery
  - Secure P2P networking
  
See [analysis/COMPILATION_COMPLETE_QUICK_REFERENCE.md](analysis/COMPILATION_COMPLETE_QUICK_REFERENCE.md) for detailed build information.

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
│   ├── consensus.rs         # Avalanche Snowball + TSDC consensus
│   ├── utxo_manager.rs      # UTXO state machine
│   ├── blockchain.rs        # Blockchain storage
│   ├── masternode_registry.rs # Masternode tracking
│   ├── heartbeat_attestation.rs # Uptime verification
│   ├── block/               # Block generation & validation
│   ├── network/             # P2P networking
│   │   ├── connection_manager.rs   # Lock-free peer connection tracking (NEW)
│   │   ├── peer_discovery.rs       # Bootstrap peer service (NEW)
│   │   ├── peer_connection.rs      # Peer connection handler
│   │   ├── peer_connection_registry.rs # Peer registry & messaging
│   │   ├── client.rs        # Network client
│   │   ├── server.rs        # Network server
│   │   ├── message.rs       # Network messages
│   │   ├── state_sync.rs    # State synchronization
│   │   ├── blacklist.rs     # IP blacklisting
│   │   ├── rate_limiter.rs  # Rate limiting
│   │   ├── dedup_filter.rs  # Message deduplication
│   │   ├── tls.rs           # TLS encryption
│   │   ├── signed_message.rs # Message signing
│   │   └── secure_transport.rs # Secure transport layer
│   └── rpc/                 # RPC server
├── docs/                    # 📚 Complete documentation
│   └── TIMECOIN_PROTOCOL_V5.md # Protocol v5 specification (Avalanche + TSDC)
├── analysis/                # Implementation notes & analysis
├── config.toml              # Default config (testnet)
├── config.mainnet.toml      # Mainnet config
├── COMPILATION_COMPLETE.md  # Build status & quick reference
└── Cargo.toml               # Dependencies
```

## 📚 Documentation

For complete protocol documentation, see **[docs/TIMECOIN_PROTOCOL_V5.md](docs/TIMECOIN_PROTOCOL_V5.md)**

**Additional Resources:**
- **[docs/NETWORK_ARCHITECTURE.md](docs/NETWORK_ARCHITECTURE.md)** - Network layer design
- **[docs/INDEX.md](docs/INDEX.md)** - Complete documentation index
- **[analysis/CHANGELOG_DEC_23_2024.md](analysis/CHANGELOG_DEC_23_2024.md)** - Recent changes

Key topics in protocol documentation:
- **[Protocol Overview](docs/TIMECOIN_PROTOCOL_V5.md#overview)** - Hybrid Avalanche + TSDC architecture
- **[Protocol Architecture](docs/TIMECOIN_PROTOCOL_V5.md#protocol-architecture)** - Real-time and epoch-time layers
- **[Avalanche Consensus](docs/TIMECOIN_PROTOCOL_V5.md#avalanche-consensus-instant-finality)** - Sub-second instant finality via Snowball
- **[TSDC (Time-Scheduled Deterministic Consensus)](docs/TIMECOIN_PROTOCOL_V5.md#time-scheduled-deterministic-consensus-tsdc)** - 10-minute deterministic block checkpointing
- **[Masternode System](docs/TIMECOIN_PROTOCOL_V5.md#masternode-system)** - Stake-weighted sampling tiers
- **[Security Model](docs/TIMECOIN_PROTOCOL_V5.md#security-model)** - Safety and liveness guarantees

## 🏗️ Architecture

### UTXO State Machine

```
Unspent → Locked → Sampling → Finalized → Archived
```

Transactions achieve finality during the Sampling phase via Avalanche Snowball, before block inclusion.

### Consensus Mechanism

**Two-Layer Design:**
1. **Avalanche Layer (Real-Time)**: Transactions finalize in <1 second via stake-weighted peer sampling with Snowball protocol
2. **TSDC Layer (Deterministic)**: Blocks created every 10 minutes via VRF-based leader selection

No global committees, no voting rounds, no BFT stalls.

### Masternode Tiers

| Tier   | Collateral | Sampling Weight | Reward Share |
|--------|-----------|-----------------|--------------|
| Free   | 0 TIME    | 1x              | ✅           |
| Bronze | 1,000     | 10x             | ✅           |
| Silver | 10,000    | 100x            | ✅           |
| Gold   | 100,000   | 1,000x          | ✅           |

*Sampling weight determines probability of being queried during Avalanche consensus. Free tier enables zero-barrier participation with Sybil resistance via stake weighting.*

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
- [x] Avalanche consensus engine (Snowball)
- [x] Time-Scheduled Deterministic Consensus (TSDC)
- [x] Stake-weighted sampling
- [x] Deterministic block production
- [x] Masternode tier system
- [x] RPC API
- [x] P2P networking
- [x] Testnet/Mainnet support
- [x] CLI tool
- [x] Peer discovery
- [x] Persistent storage (Sled)
- [ ] Heartbeat attestation (witness signatures)
- [ ] WebSocket API
- [ ] Block explorer
- [ ] Signature verification
- [ ] Mobile wallet support
- [ ] Hardware wallet integration
- [ ] Multi-signature support

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
