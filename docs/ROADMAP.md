# TIME Coin Development Roadmap

**Version:** 1.0  
**Updated:** December 23, 2025  
**Status:** Protocol V6 Complete → Implementation Phase 1 Ready

---

## Executive Summary

The TIME Coin Protocol V6 specification is **implementation-ready**. This roadmap outlines a **5-phase, 12-week baseline** development plan to move from specification to a public testnet.

- **Phase 1 (Weeks 1–2):** Core cryptographic primitives
- **Phase 2 (Weeks 3–5):** Consensus layer (Avalanche + VFP + TSDC)
- **Phase 3 (Weeks 6–8):** Network layer (QUIC, peer discovery)
- **Phase 4 (Weeks 9–10):** Storage and archival
- **Phase 5 (Weeks 11–12):** APIs and public testnet

**Target Testnet Launch:** Week 13 (end of Phase 5)  
**Target Mainnet Launch:** Q2 2025 (after 8-week testnet run + security audit)

---

## Overview: What's Complete vs. What's Next

### ✅ Complete (December 2025)

| Category | Deliverable | Status |
|----------|-------------|--------|
| **Protocol Spec** | TIMECOIN_PROTOCOL_V6.md (27 sections) | ✅ Complete |
| **Crypto Bindings** | BLAKE3, Ed25519, ECVRF (RFC 9381) pinned | ✅ Specified |
| **Transaction Format** | Canonical serialization defined | ✅ Specified |
| **Staking Script** | OP_STAKE semantics | ✅ Specified |
| **Network Transport** | QUIC v1, bincode serialization | ✅ Specified |
| **Genesis Block** | Bootstrap procedure defined | ✅ Specified |
| **Address Format** | bech32m (BIP 350) | ✅ Specified |
| **Mempool Rules** | 300MB, eviction policy | ✅ Specified |
| **Economics** | Fair launch, logarithmic rewards | ✅ Specified |
| **Error Recovery** | Conflict handling, network partitions | ✅ Specified |

### 🟨 Next: Implementation (Jan–Mar 2025)

| Phase | Duration | Deliverable | Owner |
|-------|----------|-------------|-------|
| **Phase 1** | Weeks 1–2 | Crypto test vectors | Lead Dev |
| **Phase 2** | Weeks 3–5 | Consensus network (3+ nodes) | Consensus Eng. |
| **Phase 3** | Weeks 6–8 | P2P network (10+ nodes) | Network Eng. |
| **Phase 4** | Weeks 9–10 | Block production, archival | Storage Eng. |
| **Phase 5** | Weeks 11–12 | Testnet launch, APIs | Full Team |

---

## Phase 1: Core Cryptography & Serialization (Weeks 1–2)

**Goal:** Implement all cryptographic primitives and transaction serialization.

### Objectives

1. **BLAKE3 Hashing**
   - Hash transactions → txid
   - Hash blocks → block_hash
   - Merkle root computation
   - VRF input hashing

2. **Ed25519 Signatures**
   - Sign/verify finality votes
   - Sign/verify heartbeats
   - Sign/verify transactions
   - Keypair generation

3. **ECVRF (RFC 9381)**
   - VRF proof generation
   - VRF proof verification
   - Output comparison (lowest score selection)

4. **bech32m Address Encoding**
   - Encode addresses (mainnet: time1, testnet: timet)
   - Decode and validate

5. **Canonical Transaction Serialization**
   - Serialize TX (version || inputs || outputs || lock_time)
   - Deserialize and round-trip validation
   - Hash stability test

### Deliverables

- [ ] `src/crypto/blake3.rs` - BLAKE3 hashing
- [ ] `src/crypto/ed25519.rs` - Ed25519 signing
- [ ] `src/crypto/ecvrf.rs` - ECVRF implementation
- [ ] `src/crypto/address.rs` - bech32m encoding
- [ ] `src/serialization/tx.rs` - Canonical TX format
- [ ] Test vectors file (§27, TIMECOIN_PROTOCOL_V6.md)
- [ ] All tests passing: `cargo test crypto::*`

### Success Criteria

- ✅ BLAKE3 hashes match reference vectors
- ✅ Ed25519 signatures verify independently
- ✅ ECVRF proofs verify independently
- ✅ bech32m addresses round-trip correctly
- ✅ Transaction serialization: `serialize(deserialize(tx)) == tx`
- ✅ Zero test failures

### Estimated Effort

- 1–2 developers, full-time
- 10 days with thorough testing

### Dependencies

- None (foundational)

### Blockers

- None

---

## Phase 2: Consensus Layer (Weeks 3–5)

**Goal:** Implement Avalanche Snowball, VFP, and TSDC with 3-node integration test.

### Objectives

#### 2.1 Avalanche Snowball State Machine

- [ ] Local state: `status[tx]`, `confidence[tx]`, `counter[tx]`
- [ ] Responder voting logic (Valid/Invalid/Unknown)
- [ ] Polling loop: sample k nodes, collect votes, update confidence
- [ ] Local acceptance threshold: `confidence >= β_local`
- [ ] Conflict detection and rejection

**Reference:** TIMECOIN_PROTOCOL_V6.md §7

#### 2.2 Verifiable Finality Proofs (VFP)

- [ ] FinalityVote structure and signing
- [ ] VFP assembly: threshold accumulation
- [ ] VFP validation: signature checks, threshold verification
- [ ] AVS snapshot retention (7 days)
- [ ] Global finalization: set `status[tx] = GloballyFinalized` on threshold

**Reference:** TIMECOIN_PROTOCOL_V6.md §8

#### 2.3 Active Validator Set (AVS)

- [ ] Heartbeat signing and broadcast
- [ ] Witness attestation collection (minimum 3 witnesses)
- [ ] AVS membership determination: heartbeat_ttl + witness minimum
- [ ] AVS snapshot by slot

**Reference:** TIMECOIN_PROTOCOL_V6.md §5.4

#### 2.4 TSDC Block Production

- [ ] VRF sortition: compute score for each AVS member
- [ ] Canonical leader selection: lowest score
- [ ] Block assembly from finalized transactions
- [ ] Block validation: VRF proof, entry sorting, no conflicts
- [ ] Block archival: update UTXO set, distribute rewards

**Reference:** TIMECOIN_PROTOCOL_V6.md §9

### Deliverables

- [ ] `src/consensus/snowball.rs` - Avalanche Snowball state machine
- [ ] `src/consensus/vfp.rs` - Verifiable Finality Proofs
- [ ] `src/consensus/avs.rs` - Active Validator Set management
- [ ] `src/consensus/tsdc.rs` - TSDC block production
- [ ] `tests/consensus_3node.rs` - 3-node integration test
- [ ] Test vectors for state transitions (§27)

### Success Criteria

- ✅ 3-node network produces blocks every 10 minutes
- ✅ Transactions finalize in <1 second
- ✅ VFP threshold: 67% of AVS weight
- ✅ Zero consensus failures on 100-block test run
- ✅ All consensus tests pass

### Estimated Effort

- 2–3 developers, full-time
- 15 days

### Dependencies

- Phase 1 (crypto primitives)

### Blockers

- Clock synchronization requirements (need NTP running)

---

## Phase 3: Network Layer (Weeks 6–8)

**Goal:** Implement QUIC transport, peer discovery, and message handlers for a 10-node network.

### Objectives

#### 3.1 QUIC Transport

- [ ] QUIC v1 (RFC 9000) implementation
- [ ] Connection multiplexing
- [ ] Message framing: 4-byte BE length prefix
- [ ] Max message size: 4 MB
- [ ] Connection limits: MAX_PEERS = 125

**Reference:** TIMECOIN_PROTOCOL_V6.md §18.1–§18.2

#### 3.2 Message Serialization

- [ ] bincode v1.0 for consensus messages
- [ ] Serialization for all message types (§11)
- [ ] Deserialization with error handling

**Reference:** TIMECOIN_PROTOCOL_V6.md §18.3

#### 3.3 Peer Discovery

- [ ] Bootstrap peer list (hardcoded + DNS seeds)
- [ ] Peer exchange protocol (PeerListRequest/Response)
- [ ] Connection retry logic with exponential backoff
- [ ] Peer blacklisting for misbehavior

**Reference:** TIMECOIN_PROTOCOL_V6.md §18.4

#### 3.4 Message Handlers

- [ ] TxBroadcast: receive and relay transactions
- [ ] SampleQuery/Response: Avalanche polling
- [ ] VfpGossip: receive and accumulate VFP votes
- [ ] BlockBroadcast: receive and validate blocks
- [ ] Heartbeat/Attestation: receive and validate membership

**Reference:** TIMECOIN_PROTOCOL_V6.md §11.1

#### 3.5 Network Testing

- [ ] 10-node network startup and peer discovery
- [ ] Message relay validation
- [ ] Latency and bandwidth measurements

### Deliverables

- [ ] `src/network/quic.rs` - QUIC transport
- [ ] `src/network/serialization.rs` - bincode/protobuf handlers
- [ ] `src/network/peer_discovery.rs` - Enhanced peer discovery
- [ ] `src/network/message_handlers.rs` - Consensus message handlers
- [ ] `tests/network_10node.rs` - 10-node integration test
- [ ] Peer list configuration (DNS seeds for testnet)

### Success Criteria

- ✅ 10 nodes discover each other automatically
- ✅ Messages propagate with <100ms latency
- ✅ Bandwidth usage < 1 MB/s at full load
- ✅ Zero message corruption
- ✅ Graceful handling of peer disconnections

### Estimated Effort

- 2–3 developers, full-time
- 15 days

### Dependencies

- Phase 2 (consensus messages to send)

### Blockers

- None (can prototype with TCP fallback)

---

## Phase 4: Storage & Archival (Weeks 9–10)

**Goal:** Implement persistent storage, block archival, and mempool management.

### Objectives

#### 4.1 UTXO Database

- [ ] RocksDB with proper indexing
- [ ] UTXO set queries by outpoint
- [ ] Atomic writes: block archival
- [ ] Snapshot consistency

#### 4.2 Block Archive

- [ ] Indexed block storage by height
- [ ] Header-only queries
- [ ] Block body retrieval
- [ ] Chain reorg support (rollback up to finality boundary)

#### 4.3 AVS Snapshot Retention

- [ ] Store AVS membership by slot (7 days)
- [ ] Snapshot queries for VFP validation
- [ ] Cleanup of old snapshots

#### 4.4 Mempool Management

- [ ] Mempool size limit: 300 MB
- [ ] Eviction policy: lowest_fee_rate_first
- [ ] Transaction expiry: 72 hours
- [ ] Orphan transaction pool with retry logic

**Reference:** TIMECOIN_PROTOCOL_V6.md §24

#### 4.5 Block Production Integration

- [ ] Fetch finalized transactions from FinalizedPool
- [ ] Assemble blocks with available VFPs
- [ ] Reward calculation and distribution
- [ ] Archival on block acceptance

**Reference:** TIMECOIN_PROTOCOL_V6.md §10

### Deliverables

- [ ] `src/storage/utxo_db.rs` - UTXO database
- [ ] `src/storage/block_archive.rs` - Block storage
- [ ] `src/storage/avs_snapshots.rs` - Snapshot retention
- [ ] `src/mempool/manager.rs` - Enhanced mempool with eviction
- [ ] `src/block/producer.rs` - Block assembly and rewards
- [ ] Integration test: 100-block production run

### Success Criteria

- ✅ UTXO state consistent after block archival
- ✅ Can query any historical block
- ✅ Mempool eviction triggered at 300 MB
- ✅ AVS snapshots available for 7 days
- ✅ 100 blocks produced without data corruption

### Estimated Effort

- 2 developers, full-time
- 10 days

### Dependencies

- Phase 2 (consensus produces finalized transactions)
- Phase 3 (network delivers blocks)

### Blockers

- RocksDB version compatibility

---

## Phase 5: APIs & Public Testnet (Weeks 11–12)

**Goal:** Deploy a public testnet with RPC API, faucet, and block explorer.

### Objectives

#### 5.1 JSON-RPC 2.0 API

- [ ] `sendtransaction(tx_hex)` - Broadcast transaction
- [ ] `gettransaction(txid)` - Query transaction status
- [ ] `getbalance(address)` - Query address balance
- [ ] `getblockinfo(height)` - Query block
- [ ] `getblockcount()` - Current block height
- [ ] `listmasternodes()` - Active validator list
- [ ] Error handling and rate limiting

**Reference:** TIMECOIN_PROTOCOL_V6.md §23.3

#### 5.2 Testnet Bootstrap

- [ ] Genesis block (testnet chain_id = 2)
- [ ] 3–5 bootstrap nodes (DNS seeds)
- [ ] Network parameters (testnet ports, etc.)
- [ ] Documentation for joining testnet

**Reference:** TIMECOIN_PROTOCOL_V6.md §19

#### 5.3 Faucet

- [ ] REST API for testnet TIME distribution
- [ ] Rate limiting (e.g., 100 TIME per address per day)
- [ ] Wallet integration

#### 5.4 Block Explorer Schema

- [ ] Transaction database (searchable by txid)
- [ ] Block database (searchable by height/hash)
- [ ] Address balances
- [ ] Masternode list with stats

#### 5.5 Documentation

- [ ] Testnet setup guide
- [ ] Wallet integration guide
- [ ] Masternode operator runbook
- [ ] RPC API reference

#### 5.6 Monitoring & Metrics

- [ ] Block production time tracking
- [ ] Finalization latency (TPS, <1s confirmation)
- [ ] Network peer count
- [ ] Mempool size

### Deliverables

- [ ] `src/rpc/api.rs` - Complete JSON-RPC 2.0 API
- [ ] Testnet genesis block
- [ ] 3 public bootstrap nodes (community-run)
- [ ] Faucet service
- [ ] Block explorer backend
- [ ] Documentation and setup guides
- [ ] Monitoring dashboard

### Success Criteria

- ✅ Testnet stable for 72+ hours
- ✅ RPC API responds in < 100ms
- ✅ Transactions finalize in < 1s
- ✅ Block production: 1 block per 600s ± 30s
- ✅ 100+ external nodes can join
- ✅ Faucet distributes testnet TIME
- ✅ Zero protocol violations detected

### Estimated Effort

- 2–3 developers + DevOps, full-time
- 10 days

### Dependencies

- All previous phases

### Blockers

- None (can continue with limited testnet if needed)

---

## Post-Testnet: Security & Mainnet (Q1–Q2 2025)

### Security Audit (4–6 weeks)

- [ ] External security audit of consensus, crypto, storage
- [ ] Fuzzing of message deserialization
- [ ] Performance testing under load
- [ ] Formal verification of critical sections

### Testnet Hardening (8 weeks min)

- [ ] Run testnet for 8+ weeks
- [ ] Monitor for consensus edge cases
- [ ] Stress test with 1000+ transactions/sec
- [ ] Network partition recovery testing
- [ ] Collect feedback from operators

### Mainnet Preparation

- [ ] Genesis block (mainnet chain_id = 1)
- [ ] Mainnet bootstrap nodes (high-availability setup)
- [ ] Operational runbooks
- [ ] Incident response procedures
- [ ] Community announcement and coordination

### Mainnet Launch

- [ ] Coordinated soft launch with initial validators
- [ ] Public node software release
- [ ] Wallet integrations live
- [ ] Block explorer live
- [ ] Community support channels active

---

## Detailed Phase Timeline

### Week 1–2: Phase 1 (Crypto)

```
Mon: BLAKE3 hashing
Tue: BLAKE3 testing & Ed25519 start
Wed: Ed25519 complete, ECVRF start
Thu: ECVRF complete, bech32m start
Fri: bech32m done, TX serialization start
Sat: TX serialization tests & integration
Sun: Test vector validation, code review
```

### Week 3–5: Phase 2 (Consensus)

```
Week 3: Snowball state machine
Week 4: VFP + AVS membership
Week 5: TSDC + 3-node integration test
```

### Week 6–8: Phase 3 (Network)

```
Week 6: QUIC transport + message serialization
Week 7: Peer discovery + message handlers
Week 8: 10-node integration test + stress testing
```

### Week 9–10: Phase 4 (Storage)

```
Week 9: RocksDB + block archive + mempool
Week 10: Integration test + 100-block run
```

### Week 11–12: Phase 5 (Testnet)

```
Week 11: RPC API + testnet bootstrap + faucet
Week 12: Block explorer + documentation + deployment
```

### Week 13+: Testnet Hardening

```
Weeks 13+: Public testnet runs, community testing, issue fixes
Weeks 17+: Security audit + mainnet preparation
```

---

## Team Structure

### Recommended Team Composition

| Role | Count | Responsibility |
|------|-------|-----------------|
| **Lead Developer** | 1 | Architecture, code review, release |
| **Consensus Engineer** | 1 | Phase 2 (Snowball, VFP, TSDC) |
| **Network Engineer** | 1 | Phase 3 (QUIC, peer discovery) |
| **Storage Engineer** | 1 | Phase 4 (RocksDB, archival) |
| **DevOps / SRE** | 1 | Testnet deployment, monitoring |
| **Security Engineer** | 1 | Code review, testing, audit coordination |
| **QA / Testing** | 1 | Test vectors, integration tests, stress testing |
| **Technical Writer** | 0.5 | Documentation, RPC API docs |

**Total: 6–7 FTE**

---

## Risk Management

### High-Risk Items

| Risk | Mitigation | Contingency |
|------|-----------|-------------|
| ECVRF not available in Rust | Use RFC 9381 reference impl | Implement from scratch (2–3 days) |
| QUIC library issues | Test early, fallback to TCP | Use TCP for Phase 3–5 (slower) |
| RocksDB performance | Benchmark early, optimize schema | Use SQLite (slower but simpler) |
| Consensus edge cases | Thorough testing, formal verification | Pivot to simpler consensus (time cost) |
| Testnet instability | Run Phase 2–4 tests extensively | 2-week slip acceptable |

### Medium-Risk Items

| Risk | Mitigation |
|------|-----------|
| Cryptographic test vector mismatches | Validate against RFC references |
| Network latency issues | Early stress testing with 10+ nodes |
| Storage database locks | Handle concurrent UTXO updates carefully |
| Community feedback late in Phase 5 | Early RFC review in weeks 1–2 |

---

## Success Metrics

### Phase 1 Success

- ✅ All crypto tests pass
- ✅ Test vectors match RFC references
- ✅ Zero security vulnerabilities in audit

### Phase 2 Success

- ✅ 3-node network stability: 99.9% uptime
- ✅ Consensus finality: <1s mean latency
- ✅ VFP threshold: exactly 67% of AVS weight

### Phase 3 Success

- ✅ 10-node network stability: 99.5% uptime
- ✅ Message propagation: <100ms p99 latency
- ✅ Peer discovery: <30s to find first peer

### Phase 4 Success

- ✅ 100-block production: zero data corruption
- ✅ Mempool eviction: triggered at 300 MB
- ✅ Block archival: UTXO consistency maintained

### Phase 5 Success

- ✅ Testnet stable for 72+ hours
- ✅ RPC API: <100ms response time
- ✅ Block production: 1 block per 600s ± 30s
- ✅ 100+ nodes can join

---

## Documentation & Knowledge Transfer

### Key Documents to Maintain

- [ ] `TIMECOIN_PROTOCOL_V6.md` - Always current with implementation
- [ ] `IMPLEMENTATION_ADDENDUM.md` - Update with design decisions
- [ ] Developer onboarding guide (add in Phase 2)
- [ ] Operator runbook (add in Phase 5)
- [ ] API documentation (auto-generated from RPC)

### Code Organization

```
src/
├── crypto/          # Phase 1 (BLAKE3, Ed25519, ECVRF, bech32m)
├── consensus/       # Phase 2 (Snowball, VFP, TSDC)
├── network/         # Phase 3 (QUIC, peer discovery)
├── storage/         # Phase 4 (RocksDB, archival)
├── rpc/             # Phase 5 (JSON-RPC API)
├── types.rs         # Shared types
└── main.rs          # Entry point
```

---

## Dependencies & Tools

### Required Libraries

| Library | Version | Purpose |
|---------|---------|---------|
| `blake3` | latest | BLAKE3 hashing |
| `ed25519-dalek` | latest | Ed25519 signing |
| `ecvrf` | TBD | ECVRF implementation |
| `quinn` | latest | QUIC transport |
| `bincode` | latest | Message serialization |
| `rocksdb` | latest | Persistent storage |
| `tokio` | latest | Async runtime |

### Development Tools

| Tool | Purpose |
|------|---------|
| `cargo test` | Unit/integration testing |
| `cargo clippy` | Linting |
| `cargo fmt` | Code formatting |
| `criterion` | Performance benchmarking |
| `proptest` | Property-based testing |

---

## Go-Live Checklist

Before mainnet launch, verify:

- [ ] All 5 phases complete and tested
- [ ] Security audit passed (no critical/high findings)
- [ ] Testnet ran for 8+ weeks without major issues
- [ ] Documentation complete and reviewed
- [ ] Operator runbooks tested
- [ ] Incident response procedures established
- [ ] Community validators identified and trained
- [ ] Genesis block parameters finalized
- [ ] Mainnet bootstrap nodes deployed and secured
- [ ] Wallet support launched
- [ ] Block explorer live and tested

---

## Contact & Updates

- **Lead:** TBD
- **Slack Channel:** #timecoin-development
- **Weekly Standups:** Monday 10 AM UTC
- **Roadmap Updates:** Monthly

---

## Appendix: Phase Dependency Graph

```
Phase 1 (Crypto)
    ↓
Phase 2 (Consensus) ← depends on Phase 1
    ↓
Phase 3 (Network) ← depends on Phase 2
    ↓
Phase 4 (Storage) ← depends on Phase 2, 3
    ↓
Phase 5 (Testnet) ← depends on Phase 3, 4
```

Sequential execution recommended for clarity, but some parallelization possible:
- Phase 3 (network) can prototype with TCP while Phase 2 finalizes
- Phase 4 (storage) can start once Phase 2 defines block format

---

## Version History

| Date | Version | Changes |
|------|---------|---------|
| 2025-12-23 | 1.0 | Initial roadmap (Protocol V6 complete) |

---
