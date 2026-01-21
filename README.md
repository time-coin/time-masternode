# TIME Coin Protocol Node

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)
![Protocol](https://img.shields.io/badge/protocol-v6-green.svg)
![Version](https://img.shields.io/badge/version-1.1.0-brightgreen.svg)

A high-performance implementation of the TIME Coin Protocol v6 with sub-second instant finality via Avalanche consensus, Verifiable Finality Proofs (VFP), deterministic block checkpointing, and integrated AI optimization systems.

## 🚀 Features

- **Instant Finality**: <1 second transaction confirmation via Avalanche Snowball consensus
- **Verifiable Finality Proofs**: Objective proof of transaction finality usable by all nodes and light clients
- **Deterministic Checkpointing**: 10-minute blocks with TSDC (Time-Scheduled Deterministic Consensus)
- **AI-Powered Peer Selection**: Machine learning-based peer scoring for optimal sync performance
  - Learns peer reliability from historical performance
  - Persistent knowledge across restarts
  - Automatic optimization without configuration
- **Leaderless Consensus**: No BFT voting rounds or global committees
- **Stake-Weighted Sampling**: Sybil resistance via collateral-based peer selection
- **UTXO State Machine**: Advanced state tracking (Unspent → Locked → Sampling → Finalized → Archived)
- **Masternode Tiers**: Free, Bronze, Silver, Gold tiers with weighted sampling power
- **🔒 Locked Collateral**: Dash-style masternode collateral with on-chain proof of stake
  - Prevents accidental spending of collateral
  - Automatic validation and cleanup
  - Full backward compatibility with legacy masternodes
- **Dual Network Support**: Mainnet and Testnet configurations
- **Real-time RPC API**: JSON-RPC 2.0 interface for wallets and services
- **P2P Networking**: TCP transport with peer discovery and gossip protocol (TLS support planned)
- **Persistent Storage**: Sled embedded database for blockchain storage with AVS (Active Validator Set) snapshots
- **Light Client Support**: Merkle proofs and block headers for SPV wallets

## ✅ Status

**Protocol Specification**: ✅ **V6 COMPLETE** (Implementation-Ready)
- All 8 "underspecified" issues resolved
- All 6 "missing components" specified
- 12 new normative sections (§16–§27)
- See [docs/TIMECOIN_PROTOCOL.md](docs/TIMECOIN_PROTOCOL.md)

**Implementation**: ✅ **PHASE 6 COMPLETE** (RPC API & Testnet Next)
- ✅ Phase 4: Pure Avalanche Consensus COMPLETE (Dec 23, 2025)
- ✅ Phase 5: ECVRF RFC 9381 & Multi-node COMPLETE (Dec 23, 2025)
- ✅ Phase 6: Network Integration & Testnet COMPLETE (Dec 23, 2025)
  - Network vote handlers fully implemented
  - Consensus voting working (prepare + precommit)
  - Finalization callbacks complete
  - 3-node testing procedures documented
  - Cloud testnet deployment ready
- 🚀 Phase 7: RPC API & Testnet Stabilization READY

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


## 📋 Requirements

- Rust 1.75 or higher
- 2GB RAM minimum
- 10GB disk space for full node

## 🛠️ Installation

### From Source

```bash
git clone https://github.com/time-coin/timecoin.git
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

For complete deployment guide, see **[docs/QUICKSTART.md](docs/QUICKSTART.md)**

### Run as a Masternode

#### Quick Configuration

Use the interactive configuration script:

```bash
# Linux/macOS - Configure mainnet (default)
./scripts/configure-masternode.sh

# Configure testnet
./scripts/configure-masternode.sh testnet

# Windows - Configure mainnet (default)
scripts\configure-masternode.bat

# Configure testnet
scripts\configure-masternode.bat testnet
```

#### Manual Configuration

Edit `~/.timecoin/config.toml` (or `~/.timecoin/testnet/config.toml` for testnet):

```toml
[masternode]
enabled = true
tier = "bronze"  # free, bronze, silver, or gold
reward_address = "TIMEyouraddresshere"
collateral_txid = ""  # Set after creating collateral
collateral_vout = 0
```

#### Register with Locked Collateral

```bash
# 1. Create collateral UTXO
time-cli sendtoaddress <your_address> 1000.0  # For Bronze

# 2. Wait for confirmations (30 minutes)
time-cli listunspent

# 3. Register masternode
time-cli masternoderegister \
  --tier bronze \
  --collateral-txid <txid> \
  --vout 0 \
  --reward-address <your_address>

# 4. Verify
time-cli getbalance  # Shows locked collateral
time-cli masternodelist  # Shows 🔒 Locked
```

See **[docs/MASTERNODE_GUIDE.md](docs/MASTERNODE_GUIDE.md)** for complete setup guide.

## 💻 CLI Usage

```bash
# Get blockchain info
./target/release/time-cli getblockchaininfo

# Get block count
./target/release/time-cli getblockcount

# Check wallet balance (shows total/locked/available)
./target/release/time-cli getbalance

# List unspent outputs (node-specific)
./target/release/time-cli listunspent

# List masternodes
./target/release/time-cli masternodelist

# Register masternode with locked collateral
./target/release/time-cli masternoderegister \
  --tier bronze \
  --collateral-txid <txid> \
  --vout <vout> \
  --reward-address <address>

# List all locked collaterals
./target/release/time-cli listlockedcollaterals

# Unlock masternode collateral
./target/release/time-cli masternodeunlock

# Get network info
./target/release/time-cli getnetworkinfo

# Get consensus info
./target/release/time-cli getconsensusinfo

# Check uptime
./target/release/time-cli uptime
```

See **[docs/CLI_GUIDE.md](docs/CLI_GUIDE.md)** for complete command reference.

## 🌐 Network Ports

### Mainnet
- P2P: 24000
- RPC: 24001

### Testnet
- P2P: 24100
- RPC: 24101

## 📁 Directory Structure

```
timecoin/
├── src/
│   ├── main.rs              # Entry point
│   ├── lib.rs               # Library exports
│   ├── config.rs            # Configuration management
│   ├── types.rs             # Core types (Block, Transaction, UTXO, etc.)
│   ├── consensus.rs         # Avalanche Snowball + TSDC consensus
│   ├── avalanche.rs         # Avalanche protocol implementation
│   ├── tsdc.rs              # Time-Scheduled Deterministic Consensus
│   ├── blockchain.rs        # Blockchain storage and validation
│   ├── storage.rs           # Sled database abstraction layer
│   ├── utxo_manager.rs      # UTXO state machine
│   ├── transaction_pool.rs  # Mempool management
│   ├── masternode_registry.rs # Masternode tracking
│   ├── heartbeat_attestation.rs # Uptime verification
│   ├── finality_proof.rs    # VFP (Verifiable Finality Proofs)
│   ├── wallet.rs            # Wallet functionality
│   ├── address.rs           # Address encoding/decoding
│   ├── peer_manager.rs      # High-level peer management
│   ├── time_sync.rs         # Network time synchronization
│   ├── state_notifier.rs    # State change notifications
│   ├── shutdown.rs          # Graceful shutdown handler
│   ├── error.rs             # Error types
│   ├── network_type.rs      # Mainnet/Testnet enum
│   ├── ai/                  # 🤖 AI Systems (NEW in v1.0.0)
│   │   ├── mod.rs
│   │   ├── peer_selector.rs     # AI-powered peer selection
│   │   ├── fork_resolver.rs     # Multi-factor fork resolution
│   │   ├── anomaly_detector.rs  # Security anomaly detection
│   │   ├── predictive_sync.rs   # Block arrival prediction
│   │   ├── transaction_analyzer.rs  # Transaction pattern analysis
│   │   ├── transaction_validator.rs # AI validation rules
│   │   ├── network_optimizer.rs     # Dynamic network tuning
│   │   └── resource_manager.rs      # Resource allocation
│   ├── block/               # Block generation & validation
│   │   ├── mod.rs
│   │   ├── types.rs         # Block structures
│   │   ├── producer.rs      # Block production
│   │   ├── validator.rs     # Block validation
│   │   └── merkle.rs        # Merkle tree implementation
│   ├── crypto/              # Cryptographic primitives
│   │   ├── mod.rs
│   │   ├── keys.rs          # Ed25519 key management
│   │   ├── vrf.rs           # ECVRF implementation
│   │   └── hash.rs          # BLAKE3 hashing
│   ├── network/             # P2P networking
│   │   ├── mod.rs
│   │   ├── server.rs        # TCP server
│   │   ├── client.rs        # Network client
│   │   ├── message.rs       # Network message types
│   │   ├── message_handler.rs   # Message processing logic
│   │   ├── peer_connection.rs   # Individual peer connection
│   │   ├── peer_connection_registry.rs # Peer registry & messaging
│   │   ├── connection_manager.rs    # Lock-free connection tracking
│   │   ├── connection_state.rs      # Connection state machine
│   │   ├── peer_discovery.rs        # Bootstrap peer service
│   │   ├── peer_scoring.rs          # Peer reputation system
│   │   ├── state_sync.rs    # State synchronization
│   │   ├── blacklist.rs     # IP blacklisting
│   │   ├── rate_limiter.rs  # Rate limiting
│   │   ├── dedup_filter.rs  # Message deduplication
│   │   ├── anomaly_detection.rs # Network anomaly detection
│   │   ├── fee_prediction.rs    # AI fee estimation
│   │   ├── block_optimization.rs # Block propagation optimization
│   │   ├── tls.rs           # TLS encryption (infrastructure ready)
│   │   ├── signed_message.rs    # Ed25519 message signing
│   │   └── secure_transport.rs  # Secure transport layer (future)
│   ├── rpc/                 # JSON-RPC server
│   │   ├── mod.rs
│   │   ├── server.rs        # RPC HTTP server
│   │   └── methods.rs       # RPC method handlers
│   └── bin/
│       ├── timed.rs         # Main daemon binary
│       └── time-cli.rs      # CLI tool binary
├── docs/                    # 📚 Complete documentation
│   ├── INDEX.md             # Documentation index (START HERE)
│   ├── TIMECOIN_PROTOCOL.md # Protocol v6 specification
│   ├── AI_SYSTEM.md         # AI system documentation (NEW)
│   ├── IMPLEMENTATION_DETAILS.md # Technical implementation spec (NEW)
│   ├── QUICKSTART.md        # Quick deployment guide
│   ├── QUICK_REFERENCE.md   # One-page parameter reference
│   ├── ARCHITECTURE_OVERVIEW.md # System architecture
│   ├── NETWORK_ARCHITECTURE.md  # P2P design
│   ├── CLI_GUIDE.md         # Command-line reference
│   ├── WALLET_COMMANDS.md   # Wallet operations
│   ├── CRYPTOGRAPHY_RATIONALE.md # Crypto choices explained
│   ├── LINUX_INSTALLATION.md    # Linux setup guide
│   ├── INTEGRATION_QUICKSTART.md # Integration guide
│   ├── RUST_P2P_GUIDELINES.md   # P2P best practices
│   ├── P2P_NETWORK_BEST_PRACTICES.md # Network patterns
│   ├── NETWORK_CONFIG.md    # Network configuration
│   └── _archive_protocol/   # Archived protocol versions
├── analysis/                # Implementation notes & analysis
│   └── (development notes, not for production use)
├── scripts/                 # Utility scripts
│   └── (deployment and maintenance scripts)
├── tests/                   # Integration tests
│   └── (test suites)
├── config.toml              # Default config (testnet)
├── config.mainnet.toml      # Mainnet configuration
├── genesis.testnet.json     # Testnet genesis block
├── genesis.mainnet.json     # Mainnet genesis block
├── CHANGELOG.md             # Version history
├── CONTRIBUTING.md          # Contribution guidelines
├── Cargo.toml               # Rust dependencies
├── Cargo.lock               # Locked dependency versions
├── build.rs                 # Build script
├── Dockerfile               # Docker container definition
├── timed.service            # systemd service file
└── LICENSE                  # MIT License
```

## 📚 Documentation

**[→ Complete Documentation Index](docs/INDEX.md)** (Read this first!)

### Core Documentation
- **[INDEX.md](docs/INDEX.md)** - Documentation roadmap (START HERE)
- **[TIMECOIN_PROTOCOL.md](docs/TIMECOIN_PROTOCOL.md)** - Protocol v6 specification (§1–§27)
- **[MASTERNODE_GUIDE.md](docs/MASTERNODE_GUIDE.md)** - Complete masternode setup guide
- **[AI_SYSTEM.md](docs/AI_SYSTEM.md)** - AI optimization systems (v1.0.0)
- **[IMPLEMENTATION_DETAILS.md](docs/IMPLEMENTATION_DETAILS.md)** - Technical implementation spec

### Getting Started
- **[QUICKSTART.md](docs/QUICKSTART.md)** - Quick deployment guide
- **[CLI_GUIDE.md](docs/CLI_GUIDE.md)** - Command-line reference
- **[INTEGRATION_QUICKSTART.md](docs/INTEGRATION_QUICKSTART.md)** - Integration guide

### Reference
- **[QUICK_REFERENCE.md](docs/QUICK_REFERENCE.md)** - One-page parameter lookup
- **[WALLET_COMMANDS.md](docs/WALLET_COMMANDS.md)** - Wallet operations
- **[CRYPTOGRAPHY_RATIONALE.md](docs/CRYPTOGRAPHY_RATIONALE.md)** - Crypto choices explained

### Architecture
- **[ARCHITECTURE_OVERVIEW.md](docs/ARCHITECTURE_OVERVIEW.md)** - System architecture
- **[NETWORK_ARCHITECTURE.md](docs/NETWORK_ARCHITECTURE.md)** - P2P design
- **[RUST_P2P_GUIDELINES.md](docs/RUST_P2P_GUIDELINES.md)** - P2P implementation best practices

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

See [docs/TIMECOIN_PROTOCOL.md#253-reward-distribution](docs/TIMECOIN_PROTOCOL.md#253-reward-distribution) for detailed examples.

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

## 🛣️ Development Status

**Current Status:** ✅ **v1.1.0 Production Release** (January 2026)

### ✅ Completed (v1.1.0)

#### Core Implementation
- ✅ BLAKE3 hashing, Ed25519 signing, ECVRF sortition
- ✅ Avalanche Snowball consensus
- ✅ TSDC (Time-Scheduled Deterministic Consensus)
- ✅ Verifiable Finality Proofs (VFP)
- ✅ UTXO state machine with archival
- ✅ Masternode registry with tiered system
- ✅ **Locked Collateral System (Dash-style)** - NEW in v1.1.0
  - Prevents accidental spending
  - On-chain proof of stake
  - Automatic validation and cleanup
  - Full backward compatibility
- ✅ Heartbeat attestation and uptime tracking
- ✅ **Enhanced Wallet Balance Display** - NEW in v1.1.0
  - Shows total/locked/available breakdown
  - Clear collateral visibility

#### Network Layer
- ✅ TCP P2P transport with message signing
- ✅ Peer discovery and connection management
- ✅ Block propagation and state synchronization
- ✅ Rate limiting and blacklist protection
- ✅ Message deduplication

#### AI Systems (NEW in v1.0.0)
- ✅ AI-powered peer selection (70% faster sync)
- ✅ Transaction fee prediction (80% savings)
- ✅ Multi-factor fork resolution
- ✅ Anomaly detection and security monitoring
- ✅ Predictive sync optimization
- ✅ Transaction pattern analysis
- ✅ Dynamic network optimization

#### Storage & APIs
- ✅ Sled embedded database
- ✅ JSON-RPC 2.0 API
- ✅ CLI tools (timed, time-cli)
- ✅ Mainnet and Testnet support

### 🔮 Future Roadmap (v1.2+)

**v1.2.0** (Q1 2026):
- [ ] Hot/cold wallet separation
- [ ] Masternode key auto-generation
- [ ] Enhanced monitoring dashboard
- [ ] Auto-registration from config.toml

**v2.0.0** (Q2 2026):
- [ ] Hardware wallet support
- [ ] Multi-signature transactions
- [ ] Advanced smart contract templates
- [ ] Mobile wallet SDKs

See [CHANGELOG.md](CHANGELOG.md) for detailed version history and [docs/ARCHITECTURE_OVERVIEW.md](docs/ARCHITECTURE_OVERVIEW.md) for technical architecture.

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
- Documentation: [Docs](https://github.com/time-coin/timecoin/blob/main/docs/INDEX.md)
- Block Explorer: Coming Soon
- Discord: Coming soon

## 📞 Support

- GitHub Issues: [Report a bug](https://github.com/time-coin/timecoin/issues)
- Discord: Join our community server
- Email: support@time-coin.io

## ⚠️ Disclaimer

This is experimental software. Use at your own risk. Always test on testnet first.

---

Made with ❤️ by the TIME Coin community
