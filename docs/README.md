# TimeCoin Documentation

User-facing documentation and technical specifications for the TimeCoin protocol.

## Core Documentation

### Getting Started
- **INTEGRATION_QUICKSTART.md** - Quick start guide for integration
- **LINUX_INSTALLATION.md** - Linux installation instructions
- **NETWORK_CONFIG.md** - Network configuration guide

### Protocol Specifications
- **HEARTBEAT_ATTESTATION.md** - Peer-verified heartbeat attestation system
- **INSTANT_FINALITY.md** - Instant transaction finality via BFT consensus
- **MASTERNODE_TIERS.md** - Masternode tier system (Free/Bronze/Silver/Gold)
- **REWARD_DISTRIBUTION.md** - Block reward distribution algorithm
- **FEES.md** - Transaction fee structure

### Implementation Guides
- **IMPLEMENTATION.md** - Implementation overview
- **P2P_NETWORK_BEST_PRACTICES.md** - P2P network best practices
- **RUST_P2P_GUIDELINES.md** - Rust P2P implementation guidelines

## Document Organization

### User Documentation (`/docs`)
This folder contains:
- ✅ User-facing feature documentation
- ✅ Protocol specifications
- ✅ Installation and configuration guides
- ✅ API and RPC documentation
- ✅ Best practices and guidelines

### Implementation Analysis (`/analysis`)
For implementation details, architectural decisions, and status reports, see `/analysis/`:
- Implementation progress tracking
- Architectural decision records (ADRs)
- Build and deployment notes
- Gap analysis and issue tracking

## Key Features

### Instant Finality
TimeCoin uses Byzantine Fault Tolerant (BFT) consensus for instant transaction finality:
- Transaction finalization in <3 seconds
- 2/3 masternode quorum required
- No need for multiple confirmations

### Peer-Attested Uptime
Masternode uptime is cryptographically verified:
- Ed25519 signed heartbeats every 60 seconds
- Minimum 3 independent witness attestations required
- Prevents Sybil attacks and uptime fraud
- See `HEARTBEAT_ATTESTATION.md` for full details

### Tiered Masternodes
Four tiers with different requirements and rewards:
- **Free**: No collateral, limited voting
- **Bronze**: 1,000 TIME collateral, full voting
- **Silver**: 10,000 TIME, 10x rewards
- **Gold**: 100,000 TIME, 100x rewards

## Contributing

For development documentation and implementation status, see `/analysis/README.md`.

## License

MIT License - See LICENSE file for details.
