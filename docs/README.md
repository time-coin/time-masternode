# TimeCoin Documentation

Comprehensive documentation for the TIME Coin Protocol.

## Core Documentation

### 📘 Protocol Specification
- **[TIMECOIN_PROTOCOL.md](TIMECOIN_PROTOCOL.md)** - Complete protocol specification
  - Core architecture and components
  - UTXO state machine (Unspent → Locked → Voting → Finalized → Archived)
  - TimeVote Protocol for instant finality
  - Masternode system and tiers
  - Heartbeat attestation
  - Block production and rewards
  - Network protocol and security

### 🚀 Getting Started
- **[INTEGRATION_QUICKSTART.md](INTEGRATION_QUICKSTART.md)** - Quick start guide for integration
- **[LINUX_INSTALLATION.md](LINUX_INSTALLATION.md)** - Linux installation instructions
- **[NETWORK_CONFIG.md](NETWORK_CONFIG.md)** - Network configuration guide

### 📚 Implementation Guides
- **[P2P_NETWORK_BEST_PRACTICES.md](P2P_NETWORK_BEST_PRACTICES.md)** - P2P network best practices
- **[RUST_P2P_GUIDELINES.md](RUST_P2P_GUIDELINES.md)** - Rust P2P implementation guidelines

## Key Features

### Instant Finality
- **Transaction finalization in <1 second (TimeVote)**
- Continuous quorum voting consensus
- 2/3 masternode quorum required
- No need for multiple confirmations

### UTXO State Machine
Advanced 6-state lifecycle:
```
Unspent → Locked → SpentPending → SpentFinalized → Confirmed
```

### Peer-Attested Uptime
Cryptographically verified masternode reputation:
- Ed25519 signed heartbeats every 60 seconds
- Minimum 3 independent witness attestations required
- Prevents Sybil attacks and uptime fraud

### Tiered Masternodes
Four tiers with different requirements and rewards:

| Tier | Collateral | Governance Voting | Reward Weight |
|------|-----------|-------------------|---------------|
| **Free** | 0 TIME | ❌ No | 100 |
| **Bronze** | 1,000 TIME | ✅ Yes | 1,000 |
| **Silver** | 10,000 TIME | ✅ Yes | 10,000 |
| **Gold** | 100,000 TIME | ✅ Yes | 100,000 |

## Document Organization

### User Documentation (`/docs`)
This folder contains:
- ✅ Complete protocol specification
- ✅ User-facing feature documentation
- ✅ Installation and configuration guides
- ✅ Best practices and guidelines
- ✅ API and RPC documentation

### Implementation Analysis (`/analysis`)
For implementation details, architectural decisions, and status reports, see `/analysis/`:
- Implementation progress tracking
- Architectural decision records (ADRs)
- Build and deployment notes
- Gap analysis and issue tracking
- Session summaries and bug fixes

## Quick Links

- [Protocol Overview](TIMECOIN_PROTOCOL.md#overview)
- [UTXO State Machine](TIMECOIN_PROTOCOL.md#utxo-state-machine)
- [Instant Finality](TIMECOIN_PROTOCOL.md#instant-finality)
- [TimeVote Consensus](TIMECOIN_PROTOCOL.md#timevote-consensus)
- [Masternode System](TIMECOIN_PROTOCOL.md#masternode-system)
- [Heartbeat Attestation](TIMECOIN_PROTOCOL.md#heartbeat-attestation)
- [Reward Distribution](TIMECOIN_PROTOCOL.md#reward-distribution)
- [Network Protocol](TIMECOIN_PROTOCOL.md#network-protocol)
- [Security Model](TIMECOIN_PROTOCOL.md#security-model)

## Contributing

For development documentation and implementation status, see `/analysis/README.md`.

## License

Business Source License 1.1 - See LICENSE file for details.

