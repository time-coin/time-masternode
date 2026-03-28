# TIME Coin Protocol Masternode
## Next-Generation Cryptocurrency with Instant Finality

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)
![Protocol](https://img.shields.io/badge/protocol-v6.2-green.svg)
![Version](https://img.shields.io/badge/version-1.2.3-brightgreen.svg)
![Security](https://img.shields.io/badge/security-audited-success.svg)

**TIME Coin** is a next-generation cryptocurrency built from the ground up in Rust, featuring AI-powered optimizations and sub-second transaction finality.

**Protocol v6.2** implements:
- **TimeVote**: Stake-weighted consensus with <1 second finality
- **TimeProof**: Verifiable finality proofs for light clients
- **TimeLock**: Deterministic checkpointing every 10 minutes
- **TimeGuard**: Bounded liveness guarantees (max 11.3 min recovery)
- **AI Integration**: Machine learning-based network optimization

## 🚀 Key Features

### ⚡ Performance
- **Sub-Second Finality**: <1s transaction confirmation (vs Bitcoin's 10+ minutes)
- **Deterministic Block Timing**: 600-second slots via VRF sortition (no mining variance)
- **Bounded Liveness**: TimeGuard guarantees recovery within 11.3 minutes worst-case
- **Scalable Consensus**: No global committees or BFT voting rounds

### 🔒 Security & Trust
- **TimeProof Finality**: Cryptographic proof of transaction finality
  - Verifiable by light clients without full blockchain
  - Objective finality (not probabilistic like Bitcoin)
- **Stake-Weighted Consensus**: Sybil resistance via collateral-based voting
- **Locked Collateral System**: Dash-style on-chain proof of stake
  - Prevents accidental spending
  - Automatic validation and cleanup
- **Year 2106 Safe**: 64-bit timestamps (Bitcoin needs migration)

### 🤖 AI-Powered Optimizations
- **Intelligent Peer Selection**: 70% faster sync via ML-based peer scoring
- **Multi-Factor Fork Resolution**: Automated conflict resolution
- **Anomaly Detection**: Real-time security monitoring
- **Predictive Sync**: Learns network patterns for optimal performance

### 🏗️ Architecture
- **UTXO Model**: Advanced state machine (Unspent → Locked → Voting → Finalized → Archived)
- **Masternode Tiers**: Free/Bronze/Silver/Gold with weighted voting power
- **Light Client Support**: Merkle proofs and SPV verification
- **Dual Networks**: Separate Mainnet and Testnet configurations
- **Modern Crypto**: Ed25519 signatures, BLAKE3 hashing, ECVRF sortition

### 🔒 Encrypted Memos & Payment Requests
- **Encrypted transaction memos**: Optional per-transaction messages encrypted with ECDH (Ed25519 → X25519) + AES-256-GCM — only sender and recipient can decrypt; other nodes see only ciphertext on-chain
- **Payment request URIs**: `timecoin:ADDRESS?amount=X&pubkey=HEX&memo=TEXT&label=NAME` — shareable via email, text, or QR code; solves public-key discovery for new addresses
- **CLI integration**: `--memo` flag on `sendtoaddress`, `request-payment` / `pay-request` commands

### 🔌 Developer-Friendly
- **JSON-RPC 2.0 API**: Bitcoin-compatible interface
- **Comprehensive Documentation**: Full protocol specification and guides
- **Rust Implementation**: Memory-safe, high-performance codebase
- **Embedded Storage**: Sled database with AVS snapshots

## ✅ Status

### Protocol & Implementation
- **Protocol**: ✅ **v6.2 COMPLETE** ([full specification](docs/TIMECOIN_PROTOCOL.md))
  - TimeVote, TimeProof, TimeLock, TimeGuard fully implemented
  - All 27 normative sections complete
  - Liveness Fallback (§7.6) fully operational as of v6.2
  - Security audit completed (January 2026)
- **Implementation**: ✅ **v1.2.0 PRODUCTION** (February 2026)
  - Core consensus: TimeVote + TimeLock ✅
  - Liveness fallback: TimeGuard Protocol ✅
  - Network layer: P2P + RPC ✅
  - AI systems: 7 optimization modules ✅
  - Storage: Sled database + snapshots ✅

### Security Audit Summary
- **Date**: January 2026
- **Scope**: Core consensus, network layer, cryptography
- **Critical Issues**: 0 found
- **High Priority**: 3 addressed (VRF grinding, clock sync, vote signatures)
- **Status**: ✅ Production-ready
- **Full Report**: [docs/COMPREHENSIVE_SECURITY_AUDIT.md](docs/COMPREHENSIVE_SECURITY_AUDIT.md)

### Build Status
- **Compilation**: ✅ Zero errors, zero warnings
- **Tests**: ✅ All unit and integration tests passing
- **Build Time**: ~1 minute (release profile)
- **Binary Size**: ~15MB (optimized)


## 📋 Requirements

- Rust 1.75 or higher
- 2GB RAM minimum
- 10GB disk space for full node

## 🛠️ Installation

### From Source

```bash
git clone https://github.com/time-coin/time-masternode.git
cd time-masternode
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

The automated install script handles configuration, systemd, and firewall setup:

```bash
sudo ./scripts/install-masternode.sh testnet   # or mainnet
```

By default the node starts as a **Free tier** masternode (no collateral) and
earns rewards immediately. Staked tiers (Bronze/Silver/Gold) are available for
higher rewards and governance voting.

See **[docs/LINUX_INSTALLATION.md](docs/LINUX_INSTALLATION.md)** for the
complete step-by-step guide, or **[docs/MASTERNODE_GUIDE.md](docs/MASTERNODE_GUIDE.md)**
for operational details (tiers, collateral, rewards, deregistration).

## 💻 CLI Usage

### Command Line Interface (time-cli)

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

# List all locked collaterals
./target/release/time-cli listlockedcollaterals

# Get network info
./target/release/time-cli getnetworkinfo

# Get consensus info
./target/release/time-cli getconsensusinfo

# Use testnet (default is mainnet)
./target/release/time-cli --testnet getblockchaininfo

# Check uptime
./target/release/time-cli uptime

# Generate a payment request URI (shareable via email/QR)
./target/release/time-cli request-payment 50.0 --memo "Invoice #42"

# Pay a payment request URI
./target/release/time-cli pay-request "timecoin:TIME0...?amount=50&pubkey=..."

# Send with an encrypted memo
./target/release/time-cli sendtoaddress <address> <amount> --memo "Payment note"
```

### Masternode Dashboard (time-dashboard)

An interactive terminal UI for real-time masternode monitoring:

```bash
# Launch dashboard (auto-detects mainnet/testnet)
./target/release/time-dashboard

# Force testnet
./target/release/time-dashboard --testnet

# Connect to remote node
./target/release/time-dashboard http://192.168.1.100:24001
```

**Dashboard Features:**
- 📊 **Overview Tab**: Blockchain status, wallet balance, consensus info
- 🌐 **Network Tab**: Connected peers with ping times and direction
- 🖥️ **Masternode Tab**: Tier, status, collateral, and address
- 💾 **Mempool Tab**: Transaction count and memory usage
- ⚡ **Auto-refresh**: Updates every 2 seconds
- ⌨️ **Navigation**: Tab/Arrow keys to switch, 'r' to refresh, 'q' to quit

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
time-masternode/
├── src/
│   ├── main.rs              # Entry point + all pub mod declarations
│   ├── lib.rs               # include!(main.rs) for integration test access
│   ├── config.rs            # Configuration management (time.conf + legacy TOML)
│   ├── constants.rs         # Protocol constants (block time, rewards, limits)
│   ├── types.rs             # Core types (Block, Transaction, UTXO, etc.)
│   ├── consensus.rs         # TimeVote + TimeLock consensus engine
│   ├── timevote.rs          # TimeVote protocol handler bridge
│   ├── timelock.rs          # TimeLock block production
│   ├── blockchain.rs        # Blockchain storage, validation, reorg logic
│   ├── blockchain_validation.rs # Block validation rules (extracted)
│   ├── blockchain_error.rs  # Typed blockchain error variants
│   ├── storage.rs           # Sled DB abstraction, zstd compression
│   ├── utxo_manager.rs      # UTXO state machine
│   ├── transaction_pool.rs  # Mempool management
│   ├── tx_index.rs          # Transaction index for history queries
│   ├── masternode_registry.rs  # Masternode tracking & gossip liveness
│   ├── masternode_authority.rs # Collateral slashing & authority checks
│   ├── masternode_certificate.rs # Masternode certificate generation
│   ├── finality_proof.rs    # TimeProof (Verifiable Finality)
│   ├── wallet.rs            # Wallet (AES-256-GCM + Argon2 + Ed25519)
│   ├── memo.rs              # Encrypted memo (ECDH X25519 + AES-256-GCM)
│   ├── address.rs           # Address encoding/decoding
│   ├── peer_manager.rs      # High-level peer management
│   ├── block_cache.rs       # In-memory recent block cache
│   ├── time_sync.rs         # Network time synchronization
│   ├── state_notifier.rs    # State change notifications
│   ├── shutdown.rs          # Graceful shutdown handler
│   ├── error.rs             # Error types
│   ├── network_type.rs      # Mainnet/Testnet enum
│   ├── ai/                  # 🤖 AI Systems
│   │   ├── adaptive_reconnection.rs # Exponential backoff with per-peer learning
│   │   ├── anomaly_detector.rs      # Z-score anomaly detection
│   │   ├── attack_detector.rs       # Sybil/eclipse/fork-bomb detection
│   │   ├── consensus_health.rs      # Consensus health monitoring
│   │   ├── fork_resolver.rs         # Multi-factor fork resolution
│   │   ├── metrics_dashboard.rs     # AI metrics aggregation
│   │   ├── network_optimizer.rs     # Dynamic network tuning
│   │   ├── peer_selector.rs         # AI-powered peer selection
│   │   ├── predictive_sync.rs       # Block arrival prediction
│   │   └── transaction_validator.rs # AI validation rules
│   ├── block/               # Block generation & types
│   │   ├── types.rs         # Block & BlockHeader structures
│   │   ├── generator.rs     # Block production
│   │   ├── genesis.rs       # Genesis block
│   │   └── vrf.rs           # ECVRF implementation
│   ├── crypto/              # Cryptographic primitives
│   │   └── ecvrf.rs         # EC-VRF (RFC 9381)
│   ├── network/             # P2P networking
│   │   ├── server.rs        # Inbound TCP listener (TLS auto-detect)
│   │   ├── client.rs        # Outbound connection manager (currently unused)
│   │   ├── message.rs       # Network message types
│   │   ├── message_handler.rs          # Message processing logic
│   │   ├── peer_connection.rs          # Per-peer message loop & ping/pong
│   │   ├── peer_connection_registry.rs # Lock-free active peer registry
│   │   ├── connection_manager.rs       # Outbound connection state machine
│   │   ├── sync_coordinator.rs         # Chain sync coordination
│   │   ├── peer_discovery.rs           # Bootstrap peer service
│   │   ├── peer_scoring.rs             # Peer reputation system
│   │   ├── partition_detector.rs       # Network partition detection
│   │   ├── blacklist.rs                # IP blacklisting
│   │   ├── rate_limiter.rs             # Rate limiting
│   │   ├── dedup_filter.rs             # Message deduplication
│   │   ├── block_cache.rs              # Block propagation cache
│   │   ├── tls.rs                      # rustls TLS (self-signed, Ed25519 auth)
│   │   ├── signed_message.rs           # Ed25519 message signing
│   │   ├── wire.rs                     # Message framing
│   │   └── secure_transport.rs         # Signed transport layer
│   ├── rpc/                 # JSON-RPC server
│   │   ├── server.rs        # HTTP/HTTPS RPC server (TLS auto-detect)
│   │   ├── handler.rs       # RPC method implementations
│   │   └── websocket.rs     # WebSocket notification server
│   └── bin/
│       ├── time-cli.rs      # CLI tool
│       └── time-dashboard.rs # TUI dashboard
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
├── CHANGELOG.md             # Version history
├── CONTRIBUTING.md          # Contribution guidelines
├── Cargo.toml               # Rust dependencies
├── Cargo.lock               # Locked dependency versions
├── build.rs                 # Build script
├── Dockerfile               # Docker container definition
├── timed.service            # systemd service file
└── LICENSE                  # Business Source License 1.1
```

## 📚 Documentation

**[→ Complete Documentation Index](docs/INDEX.md)** (Read this first!)

### Essential Reading
- **[INDEX.md](docs/INDEX.md)** - Documentation roadmap (START HERE)
- **[TIMECOIN_PROTOCOL.md](docs/TIMECOIN_PROTOCOL.md)** - Protocol v6.1 specification (§1–§27)
- **[QUICKSTART.md](docs/QUICKSTART.md)** - 5-minute deployment guide
- **[MASTERNODE_GUIDE.md](docs/MASTERNODE_GUIDE.md)** - Complete masternode setup

### Technical Deep Dives
- **[IMPLEMENTATION_DETAILS.md](docs/IMPLEMENTATION_DETAILS.md)** - Technical implementation spec
- **[AI_SYSTEM.md](docs/AI_SYSTEM.md)** - AI optimization systems
- **[COMPREHENSIVE_SECURITY_AUDIT.md](docs/COMPREHENSIVE_SECURITY_AUDIT.md)** - Security analysis
- **[ARCHITECTURE_OVERVIEW.md](docs/ARCHITECTURE_OVERVIEW.md)** - System architecture
- **[NETWORK_ARCHITECTURE.md](docs/NETWORK_ARCHITECTURE.md)** - P2P design

### Reference Guides
- **[CLI_GUIDE.md](docs/CLI_GUIDE.md)** - Command-line reference
- **[WALLET_COMMANDS.md](docs/WALLET_COMMANDS.md)** - Wallet operations
- **[QUICK_REFERENCE.md](docs/QUICK_REFERENCE.md)** - One-page parameter lookup
- **[CRYPTOGRAPHY_RATIONALE.md](docs/CRYPTOGRAPHY_RATIONALE.md)** - Cryptography explained

### Developer Resources
- **[INTEGRATION_QUICKSTART.md](docs/INTEGRATION_QUICKSTART.md)** - Integration guide
- **[RUST_P2P_GUIDELINES.md](docs/RUST_P2P_GUIDELINES.md)** - P2P best practices
- **[NETWORK_CONFIG.md](docs/NETWORK_CONFIG.md)** - Network configuration

## 🏗️ Architecture

### How TIME Coin Differs from Bitcoin

| Feature | Bitcoin | TIME Coin |
|---------|---------|-----------|
| **Finality Time** | 10+ min (probabilistic) | <1 second (deterministic) |
| **Block Production** | PoW mining (random) | VRF sortition (deterministic) |
| **Finality Model** | Longest chain | TimeProof signatures |
| **Light Clients** | SPV (trust assumptions) | TimeProof verification |
| **Consensus** | Nakamoto consensus | TimeVote (stake-weighted) |
| **Year 2106 Safe** | ⚠️ Needs migration | ✅ Native 64-bit |
| **Energy Usage** | High (PoW) | Low (PoS) |
| **Block Timing** | Variable (0-60+ min) | Fixed (600s slots) |

### UTXO State Machine

```
Unspent → Locked → Voting → Finalized → Archived
           ↓         ↓          ↓
        Staking   TimeVote  TimeProof  (in block)
```

Transactions achieve **deterministic finality** during the Voting phase via TimeVote Protocol, *before* block inclusion.

### Two-Layer Consensus

1. **TimeVote Protocol (Real-Time Layer)**
   - Transactions finalize in <1 second
   - Stake-weighted voting among masternodes
   - Progressive TimeProof assembly
   - 51% threshold for finality

2. **TimeLock Layer (Archival Layer)**
   - Deterministic blocks every 600 seconds
   - VRF-based sortition (fair producer selection)
   - Finalized transactions archived on-chain
   - TimeGuard fallback for bounded liveness

**Key Innovation**: Leaderless consensus with no BFT voting rounds, no global committees, and guaranteed recovery within 11.3 minutes.

### Masternode Tiers

| Tier   | Collateral | Sampling Weight | Reward Share |
|--------|-----------|-----------------|--------------|
| Free   | 0 TIME    | 1x              | ✅           |
| Bronze | 1,000     | 10x             | ✅           |
| Silver | 10,000    | 100x            | ✅           |
| Gold   | 100,000   | 1,000x          | ✅           |

*Sampling weight determines probability of being queried during TimeVote consensus. Free tier enables zero-barrier participation with Sybil resistance via stake weighting.*

### Block Rewards

- **Base Reward**: 100 TIME per block. Distribution depends on whether paid-tier nodes are present:

**Tier-based mode** (at least one Bronze/Silver/Gold node):
  - 5 TIME → treasury
  - 30 TIME + fees → block producer (leader bonus)
  - 65 TIME → per-tier pools (Gold 25 / Silver 18 / Bronze 14 / Free 8)
  - Paid tier pools each award their full amount to a single fairness-rotation winner
  - Free tier splits equally among up to 25 recipients (no minimum per-node threshold)

**All-Free mode** (no paid-tier nodes present):
  - 5 TIME → treasury
  - 95 TIME + fees → Free pool, split equally among up to 25 Free nodes (sorted by fairness bonus)
  - No separate leader bonus; the block producer is one of the 25 pool recipients

- **Fairness bonus**: Nodes that haven't recently received a reward get a linearly increasing selection weight (`blocks_without_reward / 10`)

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

Configuration files live in `~/.timecoin/` (mainnet) or `~/.timecoin/testnet/` (testnet):

- **`time.conf`** — Daemon settings (network, masternode, logging)
- **`masternode.conf`** — Collateral entries for staked tiers

The install script generates both files with sensible defaults. See
**[docs/LINUX_INSTALLATION.md](docs/LINUX_INSTALLATION.md#10-configuration-reference)**
for the full configuration reference.

## 🛣️ Development Status

**Current Status:** ✅ **v1.2.3 Production Release** (March 2026)

### ✅ Completed Features

#### Protocol & Consensus
- ✅ TimeVote Protocol (sub-second finality)
- ✅ TimeProof (verifiable finality proofs)
- ✅ TimeLock (600s deterministic blocks)
- ✅ TimeGuard (bounded liveness recovery)
- ✅ VRF sortition (RFC 9381 ECVRF)
- ✅ Stake-weighted voting with 51% threshold
- ✅ UTXO state machine (5-state lifecycle)

#### Security & Cryptography
- ✅ Ed25519 signatures (RFC 8032)
- ✅ BLAKE3 hashing
- ✅ Message signing and verification
- ✅ Locked collateral system (Dash-style)
- ✅ Year 2106 safe (64-bit timestamps)
- ✅ Security audit completed

#### Network Layer
- ✅ TCP P2P transport with Ed25519 signing
- ✅ Peer discovery and gossip protocol
- ✅ Connection management (DashMap)
- ✅ Rate limiting and blacklisting
- ✅ Message deduplication
- ✅ State synchronization

#### AI Optimization (v1.0.0+)
- ✅ **Peer Selection**: 70% faster sync
- ✅ **Fork Resolution**: Multi-factor scoring
- ✅ **Anomaly Detection**: Real-time security
- ✅ **Predictive Sync**: Pattern learning
- ✅ **Transaction Analysis**: Pattern recognition
- ✅ **Network Optimizer**: Dynamic tuning
- ✅ **Resource Manager**: Allocation optimization

#### Storage & APIs
- ✅ Sled embedded database
- ✅ AVS snapshot system
- ✅ JSON-RPC 2.0 API (Bitcoin-compatible)
- ✅ CLI tools (timed, time-cli)
- ✅ Enhanced wallet balance display
- ✅ Mainnet/Testnet separation

### 🔮 Future Roadmap

**v1.2.x** (completed):
- [x] Config-based masternode management (auto-registration from time.conf + masternode.conf)
- [x] Network-aware CLI and dashboard (--testnet flag)
- [x] TLS encryption for P2P and RPC (rustls, self-signed certs)
- [x] Encrypted transaction memos (ECDH X25519 + AES-256-GCM)
- [x] Payment request URIs (`timecoin:` scheme)
- [x] Transaction index for full history queries
- [x] Fork resolution fixes (deadlock, infinite loop, batch fetching)

**v2.0.0** (Q3-Q4 2026):
- [ ] Hardware wallet support (Ledger, Trezor)
- [ ] Multi-signature transactions
- [ ] Post-quantum cryptography migration path
- [ ] Mobile wallet SDKs (iOS, Android)
- [ ] Smart contract layer (researching design)

**v3.0.0** (2027):
- [ ] Cross-chain bridges
- [ ] Privacy enhancements (optional privacy layer)
- [ ] Sharding for horizontal scaling
- [ ] Light client improvements

See [CHANGELOG.md](CHANGELOG.md) for version history.

## 🤝 Contributing

Contributions are welcome! Please read our [Contributing Guide](CONTRIBUTING.md) first.

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## 📜 License

This project is licensed under the Business Source License 1.1 - see the [LICENSE](LICENSE) file for details. The license converts to Apache License 2.0 four years after each release.

## 🔗 Links

- Website: [https://time-coin.io](https://time-coin.io)
- Documentation: [Docs](https://github.com/time-coin/time-masternode/blob/main/docs/INDEX.md)
- Block Explorer: Coming Soon
- Discord: Coming soon

## 📊 Performance Benchmarks

*(Tested on: AMD Ryzen 9 5950X, 64GB RAM, NVMe SSD)*

| Metric | Value | Comparison |
|--------|-------|------------|
| **Transaction Finality** | <1 second | Bitcoin: 10+ minutes |
| **Block Production** | 600s deterministic | Bitcoin: 0-60+ min variable |
| **Sync Speed (AI-optimized)** | 2,500 blocks/sec | 70% faster than baseline |
| **Mempool Processing** | 10,000 tx/sec | Limited by disk I/O |
| **RPC Latency** | <10ms | Local queries |
| **Peer Discovery** | <5 seconds | Cold start |
| **Memory Usage** | ~200MB | Full node (pruned) |
| **Storage Growth** | ~50MB/day | At 1,000 tx/day |

*Note: Benchmarks vary based on hardware, network conditions, and masternode count.*

## 📞 Support

- **GitHub Issues**: [Report bugs or request features](https://github.com/time-coin/time-masternode/issues)
- **Documentation**: [Complete docs](https://github.com/time-coin/time-masternode/blob/main/docs/INDEX.md)
- **Discord**: Coming soon
- **Email**: support@time-coin.io

## 🔐 Security

- **Security Audit**: Completed January 2026 ([full report](docs/COMPREHENSIVE_SECURITY_AUDIT.md))
- **Responsible Disclosure**: Report security issues to security@time-coin.io
- **Bug Bounty**: Coming soon (post-mainnet launch)

## ⚠️ Disclaimer

TIME Coin is production-ready software that has undergone security audits. However:
- Cryptocurrency investments carry risk
- Always test on testnet before mainnet deployment
- Keep your private keys secure
- Review the code and documentation before use
- No warranty is provided (see Business Source License 1.1)

---

Made with ❤️ by the TIME Coin community
