# Copilot Instructions for TIME Coin Protocol

## Critical Constraints

### Mainnet Consensus Compatibility

**This project is deployed on mainnet. Never make changes that break consensus compatibility with existing nodes.**

Consensus-breaking changes (forbidden without a coordinated network upgrade):
- Changing the bincode serialization of any on-chain type (`Transaction`, `TxOutput`, `Block`, `SpecialTransactionData`, etc.)
- Adding or removing fields from structs included in blocks
- Changing block validation rules
- Changing how masternode reward addresses are derived from on-chain state
- Changing how UTXOs are spent or validated

Safe (non-consensus) changes:
- Mempool/relay policy, application-layer registry logic, RPC/CLI/TUI, gossip/peer-discovery, logging, metrics, config

When in doubt: if two nodes on different versions could disagree on whether a block is valid, it's a consensus change.

### Line Endings

This repo enforces **LF line endings** via `.gitattributes` (`* text=auto eol=lf`). Always use `\n`, never `\r\n`. On Windows, editors and tools may default to CRLF — be explicit. Incorrect line endings produce `warning: CRLF will be replaced by LF` on every commit.

## Build, Test, and Lint

### Building
```bash
# Minimum Rust version: 1.75 (see rust-version in Cargo.toml)

# Default: format, check, and lint (run these instead of cargo build)
cargo fmt && cargo check && cargo clippy

# Release build (optimized, fat LTO, ~1 minute)
cargo build --release

# Faster release for CI / local testing (~3x faster than release, thin LTO)
cargo build --profile release-fast

# Build specific binary
cargo build --release --bin timed
cargo build --release --bin time-cli
cargo build --release --bin time-dashboard
```

### Testing
```bash
# Run all unit tests
cargo test

# Run specific test
cargo test test_name

# Run integration tests
./scripts/test.sh

# Run specific integration test file
cargo test --test edge_cases
cargo test --test consensus_security

# Available integration test files (tests/):
#   consensus_security, edge_cases, finalized_transaction_protection,
#   fork_resolution, multi_node_consensus, security_audit,
#   stress_tests, timeproof_conflict_detection
# Manual/scripted integration tests: tests/integration/

# Run tests with output
cargo test -- --nocapture

# Run benchmarks
cargo bench
```

### Linting and Formatting
```bash
# Format code (run before commits)
cargo fmt

# Check formatting without making changes
cargo fmt -- --check

# Lint with Clippy (must pass with no warnings)
cargo clippy

# Aggressive linting
cargo clippy -- -D warnings
```

### Known Pre-existing Test Failures

8 tests fail before any changes and are unrelated to normal development:
- `consensus::tests::test_initiate_consensus`
- `consensus::tests::test_validator_management`
- `consensus::tests::test_vote_submission`
- `consensus::tests::test_timevote_init`
- `network::secure_transport::tests::test_config_creation`
- `network::secure_transport::tests::test_tls_transport`
- `network::tls::tests::test_create_self_signed_config`
- `network::tls::tests::test_tls_handshake`

### Running
```bash
# Start daemon (testnet by default)
./target/release/timed

# Start with specific config
./target/release/timed --conf /path/to/time.conf

# CLI commands (defaults to mainnet port 24001; use --testnet for testnet port 24101)
./target/release/time-cli getblockchaininfo
./target/release/time-cli --testnet getblockchaininfo
./target/release/time-cli getbalance
./target/release/time-cli masternodelist

# Launch monitoring dashboard (auto-detects network; --testnet reverses priority)
./target/release/time-dashboard
./target/release/time-dashboard --testnet
```

## High-Level Architecture

### Consensus System (Two-Layer)

TIME Coin implements a unique dual-layer consensus:

1. **TimeVote Protocol (Transaction Layer)**: Real-time transaction finalization in <1 second via stake-weighted voting among masternodes. Transactions achieve deterministic finality BEFORE block inclusion. This is NOT a traditional BFT committee—it's leaderless with progressive vote accumulation requiring 67% stake threshold (drops to 51% after 30s liveness stall).

2. **TimeLock Protocol (Block Layer)**: Deterministic block production every 600 seconds using VRF sortition for fair producer selection. Blocks archive already-finalized transactions. TimeGuard fallback ensures bounded liveness (max 11.3 min recovery).

**Critical Flow**: TX submission → UTXO locking → TimeVote broadcast → Vote collection → Finalization (67% threshold, falls back to 51% after 30s stall) → TimeProof assembly → Block inclusion → Archival on chain.

### UTXO State Machine

Five states (not the typical 2), defined in `src/types.rs` as `UTXOState`:
- `Unspent` → `Locked` (UTXO reserved for a transaction) → `SpentPending` (TimeVote voting active, tracks vote counts) → `SpentFinalized` (67% votes achieved, or 51% after 30s liveness fallback) → `Archived` (included in block)

Transactions finalize during SpentPending→SpentFinalized transition, not block inclusion. This enables instant finality.

### Network Architecture

- **Connection Management**: Lock-free DashMap (not RwLock) for O(1) peer lookups and zero lock contention
- **Message Flow**: `NetworkMessage` → `message_handler.rs` → route to consensus/blockchain/sync
- **Peer Management**: `PeerConnectionRegistry` is the single source of truth for all peer operations
- **Initialization Order Critical**: Network server must start BEFORE consensus engine callback wiring

### Module Map

| Module | Purpose |
|---|---|
| `src/consensus.rs` | TimeVote consensus, `ConsensusEngine`, fee calculation, UTXO locking |
| `src/timevote.rs` | `TimeVoteHandler` — transaction-to-finality event pipeline |
| `src/timelock.rs` | VRF-based leader selection, 600s slot scheduling, fork choice |
| `src/blockchain.rs` | Block storage, validation, fork resolution, reward distribution |
| `src/utxo_manager.rs` | 5-state UTXO machine, collateral lock tracking |
| `src/masternode_registry.rs` | 4-tier registry, stake weighting, reward calculation |
| `src/finality_proof.rs` | TimeProof assembly, progressive vote accumulation |
| `src/types.rs` | All core types: `Hash256`, `UTXO`, `UTXOState`, `SamplingWeight`, `GovernanceWeight` |
| `src/constants.rs` | Protocol constants (with spec section references) |
| `src/network/` | P2P layer — server, client, peer registry, message handler, TLS |
| `src/ai/` | 7 optimization modules (peer scoring, anomaly detection, etc.) |
| `src/rpc/` | JSON-RPC 2.0 server, WebSocket notifications |
| `src/bin/time-cli.rs` | Bitcoin-compatible CLI tool |
| `src/bin/time-dashboard.rs` | TUI dashboard (5 tabs, 2s auto-refresh) |

### AI Optimization Modules

Located in `src/ai/`, integrated throughout consensus and network layers:
- `peer_selector.rs`: Peer scoring for optimized sync
- `fork_resolver.rs`: Fork resolution — **simplified to longest-chain rule** (not ML-based; multi-factor AI scoring was removed in v1.2.0)
- `anomaly_detector.rs`: Real-time security monitoring
- `predictive_sync.rs`: Block arrival prediction
- `transaction_validator.rs`, `network_optimizer.rs`, `consensus_health.rs`, `attack_detector.rs`, `adaptive_reconnection.rs`: supporting modules

### Critical Broadcast Bug (Fixed in v1.1.0)

The consensus engine requires a broadcast callback to send TimeVote requests. This MUST be wired in `main.rs` after network initialization:
```rust
consensus.set_broadcast_callback(Arc::new(move |msg: NetworkMessage| {
    let registry = registry_clone.clone();
    tokio::spawn(async move { registry.broadcast(&msg).await; });
}));
```

Without this, consensus engine cannot communicate with the network and transactions never finalize network-wide.

## Key Conventions

### Module Organization

- **Binary entry points**: `src/main.rs` (daemon), `src/bin/time-cli.rs`, `src/bin/time-dashboard.rs`
- **Library re-export trick**: `src/lib.rs` uses `include!("./main.rs")` so integration tests can access all modules. All `pub mod` declarations live in `main.rs`. This is intentional — expect widespread `#[allow(dead_code)]` warnings in library builds.
- **Module structure**: Each major component (network, consensus, ai, block, crypto) has its own directory with `mod.rs`
- **Tokio runtime**: Pinned to 4 worker threads minimum (`#[tokio::main(worker_threads = 4)]`) to prevent sled I/O from starving network tasks on single-CPU VPS hosts.

### Async/Sync Patterns

- **Network operations**: Always async (tokio runtime)
- **Storage operations**: Sled is sync, wrap in `tokio::spawn_blocking` for CPU-intensive work
- **Consensus engine**: Internally async but exposed API is sync-friendly with channels
- **Lock-free where possible**: Use DashMap, AtomicBool, Arc for shared state (avoid RwLock in hot paths)

### Type-Safe Weight Wrappers

`types.rs` defines two distinct newtype wrappers to prevent accidental interchange:
- `SamplingWeight(u64)`: Used for VRF sortition and TimeVote stake weighting
- `GovernanceWeight(u64)`: Used for on-chain governance proposals

Never use raw `u64` for stake values — use the appropriate wrapper.

### Error Handling

- Use `thiserror` for error types
- Network errors: Continue operation, blacklist bad peers
- Consensus errors: Log and investigate (indicates protocol issue)
- Storage errors: Fatal (cannot continue without state)

### Network Module Rules

1. **All peer registration goes through `PeerConnectionRegistry`** (never bypass)
2. **Rate limiting happens BEFORE message processing** (in server.rs)
3. **Message validation order**: Check signature → Check timestamp → Process content
4. **Use `ConnectionManager` for connection state** (not manual tracking)
5. **Never block tokio threads**: Use `spawn_blocking` for crypto operations like signature verification

### Masternode Collateral System

- Collateral UTXOs are **locked on-chain** (Dash-style), not just tracked off-chain
- States: `Locked` UTXO state prevents accidental spending
- **Registration is config-based** (v1.2.0+): Set `masternode=1` in `time.conf` and collateral in `masternode.conf`; daemon auto-registers on startup. There are no `masternoderegister`/`masternodeunlock` RPC or CLI commands.
- Tier is auto-detected from collateral amount (exactly 1000/10000/100000 TIME)
- Collateral locks persist across daemon restarts (rebuilt from known masternodes on startup via `rebuild_collateral_locks()`)
- Cleanup: Set `enabled = false` in the `[masternode]` section of `masternode.conf` and restart to deregister

### Transaction Pool Management

Two separate pools:
1. **Pending pool** (`transaction_pool.rs`): Unfinalized transactions
2. **Finalized pool** (in `ConsensusEngine`): Transactions with 67% TimeVote approval (51% fallback)

**Critical**: Finalized pool must NOT clear on every block add. Only clear transactions that are actually included in the added block (use `clear_finalized_txs(txids)` not `clear_finalized_transactions()`).

### Cryptography

- **Signatures**: Ed25519 (RFC 8032) via `ed25519-dalek`
- **Hashing**: BLAKE3 (fastest cryptographic hash)
- **VRF**: ECVRF (RFC 9381) for sortition
- **All timestamps**: 64-bit Unix time (Year 2106 safe, unlike Bitcoin's 32-bit)

### Configuration

- **Config files**: `time.conf` (key=value daemon settings: `masternode=1`, `testnet=1`, etc.) + `masternode.conf` (collateral entries: `alias IP:port txid vout`). Legacy `config.toml` still loads but is deprecated.
- **Network separation**: Mainnet uses `~/.timecoin/`, Testnet uses `~/.timecoin/testnet/`
- **Genesis blocks**: Generated dynamically when masternodes register (no JSON files)
- **Port assignments**: Mainnet (24000/24001), Testnet (24100/24101)
- **Magic bytes**: Different per network for message disambiguation

### Commit Messages

- Start with a verb (Add, Fix, Update, Remove)
- Use module prefixes for scoped changes: `network: Fix connection recovery timeout`, `consensus: Implement view change mechanism`
- Reference issues when applicable: `Fix #123: Description`

### Logging

- Use `tracing` crate (not `log`)
- Levels: ERROR (fatal issues), WARN (recoverable), INFO (normal operation), DEBUG (development), TRACE (verbose internals)
- Structured logging with fields: `tracing::info!(peer = %addr, "Connected")`

## Documentation References

- **Protocol spec**: `docs/TIMECOIN_PROTOCOL.md` (27 normative sections)
- **Architecture**: `docs/ARCHITECTURE_OVERVIEW.md` (system design with recent bug fixes)
- **Network layer**: `docs/NETWORK_ARCHITECTURE.md` (P2P design patterns)
- **Security audit**: `docs/COMPREHENSIVE_SECURITY_AUDIT.md` (January 2026)
- **CLI guide**: `docs/CLI_GUIDE.md` (all commands)
- **Quick reference**: `docs/QUICK_REFERENCE.md` (one-page lookup)

## Common Development Tasks

### Adding a new RPC method
1. Add method implementation to `src/rpc/handler.rs`
2. Register it in the `match request.method.as_str()` block inside `handle_request()`
3. Add corresponding CLI command in `src/bin/time-cli.rs`
4. Document in `docs/CLI_GUIDE.md`

### Modifying consensus behavior
1. Read `docs/TIMECOIN_PROTOCOL.md` section first
2. Update implementation in `src/timevote.rs` or `src/timelock.rs`
3. Add test in `tests/consensus_security.rs`
4. Ensure broadcast callback is maintained

### Adding network message type
1. Define enum variant in `src/network/message.rs`
2. Add serialization/deserialization
3. Add handler in `src/network/message_handler.rs`
4. Add rate limiting rules if needed
5. Test with integration tests

### Working with storage
- Sled trees: `blocks`, `utxos`, `transactions`, `masternodes`, `ai_state`
- Always use transactions for multi-key updates
- Serialize with bincode for binary data, serde_json for human-readable
- Check existence before deletion (Sled returns Ok even if key doesn't exist)

## Known Attack Vectors

> **Maintenance rule**: Whenever a new attack is observed in live node logs, identified during a security review, or documented in `analysis/`, add it to this section immediately. Include: vector ID, name, observed behaviour, root cause, fix status, and AI detection method. This section is the authoritative reference for all future AI attack-detection work.

Full technical detail lives in `analysis/2026-04-05_POOL_DISTRIBUTION_ATTACK_VECTORS.md`. The table below is the quick-reference that every Copilot session starts with.

### Confirmed & Fixed (AV1–AV14, April 2026 chain-stall incident)

| ID | Name | Observed behaviour | Root cause | Status |
|----|------|--------------------|------------|--------|
| AV1 | Non-deterministic tier sort | Different nodes disagree on masternode ordering → wrong reward recipients flagged | `tier_for_wallet()` used unsorted map | ✅ Fixed: deterministic sort by collateral outpoint |
| AV2 | Reward squatter | Node claims more reward slots than its tier allows | Missing per-tier cap check | ✅ Fixed: per-tier slot enforcement |
| AV3 | Synchronized cycling | 8+ Free-tier nodes disconnect/reconnect simultaneously every 5s, poisoning registry bitmaps | No reconnect rate-limit per subnet | ✅ Partially mitigated (TLS blacklist); AI subnet detection pending |
| AV4 | Collateral squatting | Attacker registers collateral txid already used by a legitimate node; blocks that node from earning rewards | No uniqueness check on collateral outpoints | ✅ Fixed: outpoint uniqueness enforced at registration |
| AV5 | Fee validation false-positive | Blocks with fee-bearing txs rejected by all validators → 20 s stall per block | `validate_proposal_rewards()` hardcoded `fees=0` | ✅ Fixed (April 2026): `compute_block_fees()` helper |
| AV6 | Bitmap position drift | Validator sees wrong paid recipients because node set changed between production and validation | Bitmap positions keyed on volatile IP strings | ⚠️ Open: needs collateral-outpoint-keyed bitmaps |
| AV7 | Reward hijack | Node submits blocks paying non-participants; `record_reward_violation()` escalation | Misconfigured or malicious producer | ✅ Auto-bans: 3 violations → temp ban |
| AV8 | Eclipse via low peer diversity | All connected peers share same /16 prefix → node isolated | No IP-diversity enforcement | ✅ Fixed: `check_eclipse_attack()` in `AttackDetector` |
| AV9 | Gossip eviction storm | Repeated V4 MasternodeAnnounce messages force legitimate node off its own collateral | No per-outpoint eviction cooldown | ✅ Fixed: cooldown + `record_eviction_storm_attempt()` |
| AV10 | UTXO lock flood | Peer spams `UTXOStateUpdate` for a single TX, starving tokio runtime | No per-TX update count limit | ✅ Fixed: `record_utxo_lock_flood()` + block after threshold |
| AV11 | Sync loop DoS | Peer sends ≥20 identical `GetBlocks` requests in 30 s | No sync request dedup/rate limit | ✅ Fixed: `record_sync_flood()` + rate-limit action |
| AV12 | Pre-handshake message flood | Data sent before Version/Verack exchange holds task slots | No pre-handshake message gate | ✅ Fixed: `record_pre_handshake_violation()` + 10 s timeout |
| AV13 | TLS SNI flood | Rapid TLS connections (wrong/IP SNI) never blacklisted | `Err` branch of TLS setup never called `record_violation()` | ✅ Fixed (April 2026): violation recorded on TLS failure |
| AV14 | Ghost connection exhaustion | TLS succeeds but no Handshake ever sent; task held open indefinitely | No pre-handshake deadline | ✅ Fixed (April 2026): 10 s `tokio::pin!` timeout |
| AV25 | Free-tier subnet flooding | 15+ Free-tier masternodes from `154.217.246.0/24` fill registry (65 total); PHASE3 spawns tokio task per node → OOM crash every ~12 min | No per-/24 registration or reconnect cap | ✅ Fixed (April 2026): per-/24 registration cap (max 5) in `register()`; PHASE3 reconnect cap (max 3) in `client.rs` |
| AV26 | Multi-hop collateral pool rotation | Attackers use A→B→C→D→A rotation pool to evade AV3 back-and-forth check; each hop looks fresh because last-source IP differs | `collateral_migration_from` only stores one previous IP | ✅ Fixed (April 2026): sliding-window migration frequency limit (max 3 migrations per 30 min per outpoint) |
| AV27 | Invalid vote signature spam | Already-connected `154.217.246.x` nodes forge Ed25519 vote sigs at ~1-3/sec; node stays connected indefinitely burning CPU on failed signature verification | `verify_vote_signature()` returns `Ok(false)` with no violation recorded and no disconnect | ⚠️ Open: record violation in `verify_vote_signature()` invalid-sig path → escalating ban (3-strike) |
| AV28 | Unregistered voter spam | Attacker relays `TimeVotePrepare`/`TimeVotePrecommit` for voter IDs not in registry at ~15/sec; burns registry lookup cycles per message | Same `Ok(false)` return, no violation recorded; relay ambiguity requires higher threshold than AV27 | ⚠️ Open: rate-limit per peer (10 rejections/60s window → 1 violation) in unregistered-voter path |
| AV29 | SNI false-flag / reputation poisoning | Attacker from `154.217.246.x` sets TLS SNI to victim's own node IP (`69.167.168.176`) → logs appear to blame friendly node; confuses operator during incident response | No attack — ban attribution uses real TCP source IP from `accept()`, not SNI; confirmed correct | ✅ No fix needed: source IP attribution already correct; `getblacklist` CLI (commit `a4d7daa`) lets operator verify |

### AI Detection Coverage

The `AttackDetector` (`src/ai/attack_detector.rs`) detects the following automatically and feeds `take_pending_mitigations()` → server enforcement loop (30 s tick) → `IPBlacklist`:

| `AttackType` variant | Detected by | Mitigation |
|----------------------|-------------|------------|
| `SybilAttack` | `record_peer_connect` (>10 connects / 60 s per IP) | `BlockPeer` |
| `EclipseAttack` | `check_eclipse_attack` (peer diversity < 50%) | `EmergencySync` |
| `ForkBombing` | `record_fork` (≥5 forks in 300 s window) | `BlockPeer` |
| `TimingAttack` | `record_timestamp` (avg drift > 30 s over 5 samples) | `RateLimitPeer` |
| `DoublespendAttack` | `record_conflicting_transaction` (≥2 conflicting versions) | `AlertOperator` |
| `GossipEvictionStorm` | `record_eviction_storm_attempt` (any blocked V4 eviction) | `BlockPeer` |
| `CollateralSpoofing` | `record_collateral_spoof_attempt` | `BlockPeer` |
| `SyncLoopFlooding` | `record_sync_flood` | `RateLimitPeer` |
| `UtxoLockFlood` | `record_utxo_lock_flood` | `BlockPeer` |
| `ResourceExhaustion` | `record_pre_handshake_violation` (≥3 violations) | `RateLimitPeer` / `BlockPeer` |

**Still missing AI coverage (open work)**:
- `SynchronizedCycling` (AV3/AV25): `record_synchronized_disconnect()` is now wired for both inbound (server.rs) and outbound (client.rs spawn()); but the `BanSubnet` mitigation action is not yet in `MitigationAction` or the enforcement loop — coordinated subnet bans require manual `bansubnet=` config
- `TlsFlood` (AV13): need per-IP + per-subnet TLS failure rate tracking → `BlockPeer` / `BanSubnet`
- `InvalidVoteSignatureSpam` (AV27): `verify_vote_signature()` invalid-sig path needs `blacklist.record_violation(peer_ip, "invalid vote signature")` → 3-strike escalating ban
- `UnregisteredVoterSpam` (AV28): unregistered-voter path in `verify_vote_signature()` needs per-peer sliding-window counter → record violation after 10 rejections/60s
- `BanSubnet(String)` variant not yet in `MitigationAction` or enforcement loop

### Adding a New Attack Vector

When you discover a new attack (from logs, audit, or live incident):

1. **Document it here** in the "Confirmed & Fixed" or a new "Active / Under Investigation" subsection.
2. **Add the `AttackType` variant** to the enum in `src/ai/attack_detector.rs`.
3. **Add a `record_*` method** with sliding-window or threshold detection logic.
4. **Choose a `MitigationAction`**: `BlockPeer` (single IP), `BanSubnet` (coordinated cluster), `RateLimitPeer` (nuisance), `AlertOperator` (ambiguous/high-value).
5. **Wire the hook** in `src/network/server.rs` at the relevant event site (TLS error, disconnect, invalid message, etc.).
6. **Update the enforcement loop** in `NetworkServer::start()` if a new `MitigationAction` variant was added.
7. **Add a test** in `tests/consensus_security.rs` or `tests/security_audit.rs`.
8. **Commit** with prefix `security:` and reference the vector ID (e.g. `security: add AV15 detection for X`).
