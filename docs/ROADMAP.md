# TimeCoin Roadmap

This document outlines the development roadmap for TimeCoin.

## Current Version: v1.2.0

---

## ✅ Completed

### Core Blockchain
- Two-layer consensus: TimeVote (instant tx finality) + TimeLock (block production)
- VRF-based leader selection with tier-weighted participation
- Five-state UTXO lifecycle: Unspent → Locked → SpentPending → SpentFinalized → Archived
- Block bitmap for on-chain participation records and reward eligibility
- Fork detection and resolution (longest-chain rule + hash tiebreaker)
- Chain rollback with undo logs
- Block catch-up acceleration (5s VRF rotation, Free tier eligible at 15s)

### Masternode System
- Four collateral tiers: Gold, Silver, Bronze, Free
- Stake-weighted TimeVote participation
- Gossip-based active status tracking (direct TCP authoritative, gossip secondary)
- Bitmap-based reward eligibility (must appear in previous block's bitmap)
- Pyramid network topology with full mesh for networks ≤ 20 nodes
- Masternode registration, deregistration, collateral locking

### Networking
- P2P with exponential backoff reconnection
- Peer eviction after 10 consecutive failures
- PeerExchange with load info and tier-aware routing
- Overload redirect (>70% capacity → redirect inbound to less-loaded peers)
- Post-handshake-only connected peer tracking
- IP blacklisting, rate limiting, dedup filter
- TLS support (optional, wired but not enforced)

### AI Modules (7 active)
- **Adaptive reconnection** — learns optimal retry timing from historical patterns
- **Anomaly detector** — Z-score statistical analysis on network events
- **Attack detector** — detects Eclipse, Sybil, Timing, DoublespendAttempt, ForkBombing, ResourceExhaustion
- **Consensus health** — monitors agreement ratios, predicts fork probability
- **Fork resolver** — deterministic rules with AI scoring for ambiguous cases
- **Peer selector** — epsilon-greedy scoring by reliability, latency, recency
- **Predictive sync** — predicts next block timing from 20-block history

### API & Tools
- 55 RPC methods (blockchain, wallet, transactions, masternode, consensus, admin)
- WebSocket server for real-time tx and UTXO finality notifications (5,000 concurrent limit)
- TUI monitoring dashboard (`time-dashboard`)
- CLI client (`time-cli`) with full RPC access
- Ed25519 wallet with AES-256-GCM encryption and Argon2 KDF

### Storage & Security
- Sled embedded database with per-domain trees
- BLAKE3 hashing, Ed25519 signatures, ECVRF sortition
- 64-bit timestamps (Year 2106 safe)
- Signature verification, rate limiting, per-IP connection limits

---

## 🔄 Phase 1 — Network Hardening (Q1–Q2 2026)

### In Progress / Near Term
- [x] **Enforce TLS for P2P** — enabled by default; self-signed certs auto-generated; `tls=0` opt-out for debugging
- [x] **Enforce TLS for RPC** — enabled by default; `rpctls=0` opt-out; `time-cli` auto-negotiates TLS
- [x] **Stub completion** — all 6 blockchain stubs implemented with real data
- [ ] **Network partition detection** — detect when a node is isolated from the rest of the network and alert/recover automatically
- [ ] **Checkpoint system** — replace placeholder checkpoints with real mainnet/testnet block hashes

### Medium Priority
- [ ] **Block explorer** — web interface for browsing blocks, transactions, and masternode status
- [ ] **IPv6 support** — bind and connect over IPv6 addresses
- [ ] **NAT traversal** — UPnP or STUN-based hole punching for nodes behind home routers
- [ ] **Parallel block validation** — validate non-conflicting transactions concurrently during sync

---

## 🗓️ Phase 2 — Governance & Ecosystem (Q2–Q3 2026)

### Governance
- [ ] **On-chain governance proposals** — masternode voting framework exists (power defined per tier); add proposal submission, voting, and execution
- [ ] **Treasury management** — allocate a portion of block rewards to a treasury governed by masternode vote
- [ ] **Fee schedule governance** — allow masternodes to adjust fee parameters via proposal votes (architecture already supports this)

### Developer Ecosystem
- [ ] **SDK** — lightweight Rust and/or Python client library for the RPC API
- [ ] **GraphQL interface** — alternative to JSON-RPC for web integrations
- [ ] **WebSocket subscription extensions** — subscribe to block events, masternode changes, and governance votes (tx notifications already implemented)
- [ ] **Testnet faucet** — public endpoint to fund testnet addresses

---

## 🗓️ Phase 3 — Scalability (Q3–Q4 2026)

- [ ] **State compression** — prune spent UTXO history after sufficient confirmations
- [ ] **DHT-based peer discovery** — reduce reliance on hardcoded seed nodes
- [ ] **Light client / SPV support** — allow wallets to verify transactions without a full node
- [ ] **Optimized proof verification** — batch Ed25519 signature verification during block validation

---

## 🗓️ Phase 4 — Advanced Features (2027)

### Smart Contracts
- [ ] Basic smart contract VM (sandboxed, TIME-native scripting)
- [ ] Contract deployment and invocation via RPC
- [ ] Developer tooling and documentation

### Privacy (Optional / Research)
- [ ] Confidential transaction amounts (Pedersen commitments)
- [ ] Zero-knowledge proof integration research

### Infrastructure
- [ ] **Mobile wallet** — iOS/Android app with TOFU (Trust On First Use) cert pinning: warn on first connect to a node, pin the self-signed cert fingerprint, transparent on subsequent connections (same model as SSH)
- [ ] **Multi-signature wallet support**
- [ ] **Atomic swaps with other chains**
- [ ] **Cross-chain bridge research**
- [ ] **Public RPC infrastructure** (`rpc.timecoin.network`) — TIME Coin-operated nodes with real domain + Let's Encrypt certs for wallets that need CA-trusted TLS without cert pinning

---

## Release Schedule

| Version | Focus | Target |
|---------|-------|--------|
| **1.2.x** | Bug fixes, testnet stability | Ongoing |
| **1.3.0** | Mandatory TLS, partition detection, stub completion | Q2 2026 |
| **1.4.0** | Block explorer, governance proposals, treasury | Q3 2026 |
| **1.5.0** | Light clients, state compression, DHT discovery | Q4 2026 |
| **2.0.0** | Smart contracts, production mainnet launch | 2027 |

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to get involved.

---

*Last updated: March 2026*
