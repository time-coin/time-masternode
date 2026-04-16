# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Versioning

The version for this repo is defined in **`Cargo.toml`** (`version = "x.y.z"`). When bumping the version:

1. Update `Cargo.toml` here
2. Update **`~/projects/time-website/js/config.js`** — this is the single source of truth for the version numbers displayed on the public website (`nodeVersion`, `devNotice`, `progressInfo`). The website does not auto-read Cargo.toml; it must be updated manually.



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

## Line Endings

This repo enforces **LF line endings** via `.gitattributes` (`* text=auto eol=lf`). Always use `\n`, never `\r\n`. On Windows, editors and tools may default to CRLF — be explicit. Incorrect line endings produce `warning: CRLF will be replaced by LF` on every commit.

## Build, Test, and Lint

### Building
```bash
# Default: format, check, and lint (run these instead of cargo build)
cargo fmt && cargo check && cargo clippy

# Debug build
cargo build

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

## Architecture Overview

This is **TIME Coin** (`timed`), a Rust blockchain daemon for a masternode-based proof-of-stake network. The binary is `timed`; `src/lib.rs` simply `include!`s `main.rs` so that integration tests can access all modules.

### Consensus System (Two-Layer)

1. **TimeVote Protocol (Transaction Layer)**: Real-time transaction finalization in <1 second via stake-weighted voting among masternodes. Transactions achieve deterministic finality BEFORE block inclusion. Leaderless with progressive vote accumulation requiring 67% stake threshold (drops to 51% after 30s liveness stall).

2. **TimeLock Protocol (Block Layer)**: Deterministic block production every 600 seconds using VRF sortition for fair producer selection. Blocks archive already-finalized transactions. TimeGuard fallback ensures bounded liveness (max 11.3 min recovery).

**Critical Flow**: TX submission → UTXO locking → TimeVote broadcast → Vote collection → Finalization (67% threshold, falls back to 51% after 30s stall) → TimeProof assembly → Block inclusion → Archival on chain.

### UTXO State Machine

Five states (not the typical 2), defined in `src/types.rs` as `UTXOState`:
- `Unspent` → `Locked` (UTXO reserved for a transaction) → `SpentPending` (TimeVote voting active, tracks vote counts) → `SpentFinalized` (67% votes achieved, or 51% after 30s liveness fallback) → `Archived` (included in block)

Transactions finalize during SpentPending→SpentFinalized transition, not block inclusion. This enables instant finality.

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
| `src/wallet.rs` | AES-256-GCM encryption, Ed25519 keys, encrypted memos via X25519 ECDH |
| `src/storage.rs` | sled KV store, zstd-compressed blocks (magic prefix `ZSTD`), `InMemoryUtxoStorage` |
| `src/network/` | P2P layer — server, client, peer registry, message handler, TLS |
| `src/ai/` | 7 optimization modules (peer scoring, anomaly detection, attack detection) |
| `src/rpc/` | JSON-RPC 2.0 server, WebSocket notifications |
| `src/bin/time-cli.rs` | Bitcoin-compatible CLI tool |
| `src/bin/time-dashboard.rs` | TUI dashboard (5 tabs, 2s auto-refresh) |

### Network Layer (`src/network/`)

- `server.rs` — inbound TCP listener; TLS auto-detect (0x16 byte peek), per-connection tasks.
- `peer_connection.rs` — unified message loop (`run_message_loop_unified`), ping/pong liveness, fork resolution state machine.
- `peer_connection_registry.rs` — lock-free registry (DashMap) of active peers; channel-based writers (`PeerWriterTx = mpsc::UnboundedSender<Vec<u8>>`).
- `connection_manager.rs` — Connecting/Connected/Disconnected state machine; prevents duplicate outbound dials.
- `client.rs` — outbound connection logic; three-phase startup. **Currently unused** — connection management is done directly in `main.rs`.
- `tls.rs` — rustls with `AcceptAnyCertVerifier` (self-signed certs, message-level auth via Ed25519).
- `wire.rs` / `secure_transport.rs` — framing and signed-message transport.
- `message_handler.rs` — routes `NetworkMessage` to consensus/blockchain/sync.

### Masternode System

- Four tiers: **Gold → Silver → Bronze → Free** (collateral-based, stored in `MasternodeTier`).
- Collateral: Free=0, Bronze=1,000 TIME, Silver=10,000 TIME, Gold=100,000 TIME; tier auto-detected from amount.
- Sampling weights (TimeVote stake): Free=1, Bronze=10, Silver=100, Gold=1,000.
- Registry uses gossip-based liveness: active when ≥3 peers reported it within 5 min.
- Network topology mirrors tier pyramid: Gold full mesh; lower tiers connect upward.
- Collateral UTXOs require 3 confirmations before activation.
- **Registration is config-based** (v1.2.0+): Set `masternode=1` in `time.conf` and collateral in `masternode.conf`; daemon auto-registers on startup. There are no `masternoderegister`/`masternodeunlock` RPC or CLI commands.
- Cleanup: Set `enabled = false` in `[masternode]` section of `masternode.conf` and restart to deregister.

### AI Subsystem (`src/ai/`)

- `AISystem` aggregates 7 modules initialized from a shared sled DB.
- Key modules: `AdaptiveReconnectionAI` (exponential backoff with per-peer learning), `AIPeerSelector` (scores peers for sync), `AttackDetector` (sybil/eclipse/fork-bomb detection), `AnomalyDetector` (z-score on network events).
- `fork_resolver.rs` uses longest-chain rule (multi-factor AI scoring removed in v1.2.0).

### RPC Server (`src/rpc/`)

- JSON-RPC 2.0 over HTTP/HTTPS on port 24001 (mainnet) / 24101 (testnet).
- Auto-detects TLS vs plain HTTP on same port (0x16 byte peek).
- WebSocket notifications on port 24002/24102.

### Configuration & Data Directories

- **Config files**: `time.conf` (key=value daemon settings) + `masternode.conf` (collateral entries: `alias IP:port txid vout`). Legacy `config.toml` still loads but is deprecated.
- Data dirs: `~/.timecoin/` (Linux/Mac), `%APPDATA%\timecoin\` (Windows)
- Testnet uses `~/.timecoin/testnet/` subdirectory
- Config priority: `--conf` flag → `time.conf` in data dir → legacy TOML → CWD fallback
- Genesis blocks generated dynamically when masternodes register (no JSON files).

### Key Ports

| Network  | P2P   | RPC   | WebSocket |
|----------|-------|-------|-----------|
| Mainnet  | 24000 | 24001 | 24002     |
| Testnet  | 24100 | 24101 | 24102     |

## Key Conventions

### Important Design Constraints

- `src/lib.rs` uses `include!("./main.rs")` so integration tests can access all modules. All `pub mod` declarations live in `main.rs`. This is intentional — expect widespread `#[allow(dead_code)]` in library builds.
- Tokio runtime pinned to **4 worker threads minimum** (`#[tokio::main(worker_threads = 4)]`) to prevent sled I/O from starving network tasks on single-CPU VPS hosts.
- TLS close_notify errors from rustls are intentionally suppressed in `rpc/server.rs` — benign noise from HTTP clients.
- Connection deduplication: both `ConnectionManager` (outbound state machine) and `PeerConnectionRegistry` (active session registry) must agree on a peer's state — always update both.

### Async/Sync Patterns

- **Network operations**: Always async (tokio runtime).
- **Storage operations**: Sled is sync, wrap in `tokio::spawn_blocking` for CPU-intensive work.
- **Consensus engine**: Internally async but exposed API is sync-friendly with channels.
- **Lock-free where possible**: Use DashMap, AtomicBool, Arc for shared state (avoid RwLock in hot paths).

### Type-Safe Weight Wrappers

`types.rs` defines two distinct newtype wrappers to prevent accidental interchange:
- `SamplingWeight(u64)`: Used for VRF sortition and TimeVote stake weighting.
- `GovernanceWeight(u64)`: Used for on-chain governance proposals.

Never use raw `u64` for stake values — use the appropriate wrapper.

### Transaction Pool Management

Two separate pools:
1. **Pending pool** (`transaction_pool.rs`): Unfinalized transactions.
2. **Finalized pool** (in `ConsensusEngine`): Transactions with 67% TimeVote approval (51% fallback).

**Critical**: Finalized pool must NOT clear on every block add. Only clear transactions actually included in the added block (use `clear_finalized_txs(txids)` not `clear_finalized_transactions()`).

### Network Module Rules

1. **All peer registration goes through `PeerConnectionRegistry`** (never bypass).
2. **Rate limiting happens BEFORE message processing** (in server.rs).
3. **Message validation order**: Check signature → Check timestamp → Process content.
4. **Use `ConnectionManager` for connection state** (not manual tracking).
5. **Never block tokio threads**: Use `spawn_blocking` for crypto operations like signature verification.

### Critical Broadcast Bug (Fixed in v1.1.0)

The consensus engine requires a broadcast callback wired in `main.rs` after network initialization:
```rust
consensus.set_broadcast_callback(Arc::new(move |msg: NetworkMessage| {
    let registry = registry_clone.clone();
    tokio::spawn(async move { registry.broadcast(&msg).await; });
}));
```
Without this, consensus cannot communicate with the network and transactions never finalize.

### Masternode Collateral Locking

- Collateral UTXOs are **locked on-chain** (Dash-style), not just tracked off-chain.
- Collateral locks persist across daemon restarts (rebuilt via `rebuild_collateral_locks()`).

### Error Handling

- Use `thiserror` for error types.
- Network errors: Continue operation, blacklist bad peers.
- Consensus errors: Log and investigate (indicates protocol issue).
- Storage errors: Fatal (cannot continue without state).

### Cryptography

- **Signatures**: Ed25519 (RFC 8032) via `ed25519-dalek`
- **Hashing**: BLAKE3 (fastest cryptographic hash)
- **VRF**: ECVRF (RFC 9381) for sortition
- **All timestamps**: 64-bit Unix time (Year 2106 safe, unlike Bitcoin's 32-bit)

### Commit Messages

- Start with a verb (Add, Fix, Update, Remove).
- Use module prefixes for scoped changes: `network: Fix connection recovery timeout`, `consensus: Implement view change mechanism`.
- Reference issues when applicable: `Fix #123: Description`.

### Logging

- Use `tracing` crate (not `log`).
- Levels: ERROR (fatal), WARN (recoverable), INFO (normal), DEBUG (dev), TRACE (verbose).
- Structured logging with fields: `tracing::info!(peer = %addr, "Connected")`.

## Common Development Tasks

### Adding a new RPC method
1. Add method implementation to `src/rpc/handler.rs`.
2. Register it in the `match request.method.as_str()` block inside `handle_request()`.
3. Add corresponding CLI command in `src/bin/time-cli.rs`.
4. Document in `docs/CLI_GUIDE.md`.

### Modifying consensus behavior
1. Read `docs/TIMECOIN_PROTOCOL.md` section first.
2. Update implementation in `src/timevote.rs` or `src/timelock.rs`.
3. Add test in `tests/consensus_security.rs`.
4. Ensure broadcast callback is maintained.

### Adding a network message type
1. Define enum variant in `src/network/message.rs`.
2. Add serialization/deserialization.
3. Add handler in `src/network/message_handler.rs`.
4. Add rate limiting rules if needed.
5. Test with integration tests.

### Working with storage
- Sled trees: `blocks`, `utxos`, `transactions`, `masternodes`, `ai_state`.
- Always use transactions for multi-key updates.
- Serialize with bincode for binary data, serde_json for human-readable.
- Check existence before deletion (sled returns Ok even if key doesn't exist).

## Known Attack Vectors

> **Maintenance rule**: Whenever a new attack is observed in live node logs, identified during a security review, or documented in `analysis/`, add it to this section immediately. Include: vector ID, name, observed behaviour, root cause, fix status, and AI detection method. This section is the authoritative reference for all future AI attack-detection work.

Full technical detail lives in `analysis/2026-04-05_POOL_DISTRIBUTION_ATTACK_VECTORS.md`. The table below is the quick-reference.

### Confirmed & Fixed (AV1–AV14, April 2026 chain-stall incident)

| ID | Name | Observed behaviour | Root cause | Status |
|----|------|--------------------|------------|--------|
| AV1 | Non-deterministic tier sort | Different nodes disagree on masternode ordering → wrong reward recipients flagged | `tier_for_wallet()` used unsorted map | Fixed: deterministic sort by collateral outpoint |
| AV2 | Reward squatter | Node claims more reward slots than its tier allows | Missing per-tier cap check | Fixed: per-tier slot enforcement |
| AV3 | Synchronized cycling / IP cycling | 8+ Free-tier nodes disconnect/reconnect simultaneously every 5s, poisoning registry bitmaps; also: single IP tries to move collateral back to itself within 600s lockout | No reconnect rate-limit per subnet; AV3 cycling path returned `InvalidCollateral` (generic) with no violation recorded | ✅ Fixed (`45bb9ba`): `IpCyclingRejected` error variant; message handler `record_violation` → escalating ban + disconnect. ✅ Fixed (`d209b70`): per-IP 30s reconnect cooldown (`free_tier_reconnect_cooldown` DashMap); 5s disconnect dedup guard; blacklist check before gossip `register()` closes ghost-registration via relay. Subnet-wide bans deliberately not implemented — honest nodes on shared cloud subnets would be caught in the blast radius. Per-IP escalating bans are the correct boundary. ✅ Fixed (`6a861f1`): AV3 detector was counting raw disconnect events per /24, not unique IPs — a single peer reconnecting after a frame error or TLS race triggered the threshold (observed false-positive victims: 158.247.220.125, 50.28.104.50, 64.91.241.10, causing block stalls). Changed `subnet_disconnects` to store `(timestamp, ip)` tuples; threshold now evaluated against unique IP count in window. |
| AV4 | Collateral squatting / gossip hijack spam | Attacker registers collateral txid already used by a legitimate node; also floods gossip with repeated claims (15+/30s) against paid-tier nodes | No uniqueness check; `CollateralAlreadyLocked` violation was recorded but connection was never terminated | ✅ Fixed (`61a24c7`): UTXO output address proves ownership → squatter evicted. ✅ Fixed (`45bb9ba`): `record_severe_violation` return value now propagated → 1h ban + immediate disconnect on first attempt |
| AV5 | Fee validation false-positive | Blocks with fee-bearing txs rejected by all validators → 20s stall per block | `validate_proposal_rewards()` hardcoded `fees=0` | Fixed: `compute_block_fees()` helper |
| AV6 | Bitmap position drift | Validator sees wrong paid recipients because node set changed between production and validation | Bitmap positions keyed on volatile IP strings | ✅ Fixed: bitmap positions keyed on permanent `slot_id` assigned at registration — stable across all network state changes, drift impossible |
| AV7 | Reward hijack | Node submits blocks paying non-participants | Misconfigured or malicious producer | ✅ Fixed: 3 violations within 1-hour window → collateral slash + deregistration; counter decays after 1h so transient fork confusion cannot permanently ban honest producers |
| AV8 | Eclipse via low peer diversity | All connected peers share same /16 prefix → node isolated | No IP-diversity enforcement | Fixed: `check_eclipse_attack()` in `AttackDetector` |
| AV9 | Gossip eviction storm | Repeated V4 MasternodeAnnounce messages force legitimate node off its own collateral | No per-outpoint eviction cooldown | Fixed: cooldown + `record_eviction_storm_attempt()` |
| AV10 | UTXO lock flood | Peer spams `UTXOStateUpdate` for a single TX, starving tokio runtime | No per-TX update count limit | Fixed: `record_utxo_lock_flood()` + block after threshold |
| AV11 | Sync loop DoS | Peer sends ≥20 identical `GetBlocks` requests in 30s | No sync request dedup/rate limit | Fixed: `record_sync_flood()` + rate-limit action |
| AV12 | Pre-handshake message flood | Data sent before Version/Verack exchange holds task slots | No pre-handshake message gate | Fixed: `record_pre_handshake_violation()` + 10s timeout |
| AV13 | TLS SNI flood | Rapid TLS connections (wrong/IP SNI) never blacklisted | `Err` branch of TLS setup never called `record_violation()` | ✅ Fixed (`d209b70`): `record_tls_violation()` with 30-failure threshold; caps at 1-hour temp ban (never permanent); resets after 1 hour so a mode-mismatch blip doesn't permanently exile a legitimate node |
| AV14 | Ghost connection exhaustion | TLS succeeds but no Handshake ever sent; task held open indefinitely | No pre-handshake deadline | Fixed: 10s `tokio::pin!` timeout |
| AV25 | Free-tier subnet flooding | 15+ Free-tier masternodes from one /24 fill registry; PHASE3 spawns task per node → OOM | No per-/24 registration or reconnect cap | Policy change: subnet registration cap removed — operators may legitimately control a subnet and run multiple Free-tier nodes. OOM prevention retained via PHASE3 task limit. Per-node misbehavior (cycling AV3, vote spam AV27/AV28, sync flood AV11) is detected and penalized individually. |
| AV26 | Multi-hop collateral pool rotation | A→B→C→D→A rotation evades back-and-forth check | `collateral_migration_from` only stores one previous IP | ✅ Fixed: sliding-window migration limit (max 3 per 30 min per outpoint) |
| AV27 | Invalid vote signature spam | Forged Ed25519 vote sigs at ~1-3/sec burn CPU | `verify_vote_signature()` returns `Ok(false)` with no violation | ✅ Fixed: `invalid_sig_vote_window` sliding window in `message_handler.rs` — 5 Ed25519 failures within 30s triggers one violation; structurally malformed votes (empty/wrong-length) still record immediately |
| AV28 | Unregistered voter spam | Votes for unregistered IDs at ~15/sec burn registry lookups | Same `Ok(false)` return, no violation | ✅ Fixed: `unregistered_vote_window` sliding window in `message_handler.rs` — 10 unregistered-voter rejections within 60s triggers one violation |
| AV29 | SNI false-flag / reputation poisoning | Attacker sets TLS SNI to victim's IP to confuse logs | Ban attribution already uses real TCP source IP | ✅ No fix needed |
| AV30 | Genesis-confirmed deadlock | `BlockHashResponse` routing bug: `handle_response()` never called → `verify_genesis_compatibility()` always times out → fork resolution permanently blocked → node stranded on minority fork | `BlockHashResponse` arm in message handler returned `Ok(None)` without forwarding to oneshot channel | ✅ Fixed: forward `BlockHashResponse` to `handle_response()`; whitelist bypass for genesis-confirmed gate |
| AV31 | Fork injection via catch-up sync | Attacker controls majority of a restarting node's connections; forked block arrives first during catch-up → node locked onto minority fork; compounds with AV30 to create indefinite fork trap | No prioritization of whitelisted peers during sync; subnet ban not enforced on `BlockResponse` messages | ✅ Fixed: genesis sync loops now ask `get_whitelisted_connected_peers()` first; subnet bans enforced on all inbound messages including `BlockResponse`; unwhitelisted peers only used as fallback if trusted set doesn't respond |
| AV32 | Gossip flood → tokio worker starvation | Flood of MasternodeAnnounce gossip causes `masternodes.write().await` + `spawn_blocking` UTXO/sled I/O inside the lock; 4 tokio workers saturate → RPC timeouts → watchdog restarts → node loses masternode registration | Sync sled I/O and nested async lock awaits inside async write lock scope | ✅ Fixed (Apr 2026, commits `06e1481`–`5a51407`): write-behind sled channel; all async state pre-fetched before write lock; free-tier disconnect via `sled_remove_bg`; zombie kick disabled pending `unregister_peer` DashMap refactor |
| AV33 | Producer pool self-award | Modified block producer self-assigns non-Free tier pool (Silver/Bronze/Gold) every block regardless of fairness rotation; validators accepted because amounts were correct but winner identity was not verified | `validate_pool_distribution` verified AMOUNTS only, not WHICH node within a tier received the pool — explicitly noted in a comment | ✅ Fixed: Step 3b in `validate_pool_distribution` now verifies fairness-rotation winner identity using on-chain `blocks_without_reward` history; bitmap drift guard prevents false positives; `tier_winner` map tracks actual recipient per tier |
| AV34 | Targeted disconnect / reward theft | Attacker floods a paid-tier node with garbage connections or spoofed RST packets to force a TCP disconnect; with the node marked inactive it is excluded from the next block's reward pool | Free-tier nodes were immediately removed on disconnect; paid-tier PHASE3 reconnect could take up to 30 s (one block slot at 600 s); no grace window kept disconnected nodes eligible | ✅ Fixed (`d209b70`): 90-second reward-eligibility grace window (`last_seen_at` + `ELIGIBILITY_GRACE_SECS`) keeps recently-disconnected paid-tier nodes in all three eligible-pool passes; paid-tier disconnect fires `priority_reconnect_notify` so PHASE3 wakes immediately (AI cooldown bypassed for Bronze+); Free-tier nodes kept in registry for 300 s grace period before stale-cleanup removes them |
| AV35 | Free-tier reward monopolisation | Modified producer excludes all but one Free-tier address from the 8 TIME pool; validator only checked total amount, not which addresses were chosen | `validate_pool_distribution` Step 3b did not apply to Free tier; fairness formula dead zone (`blocks_without_reward / 10`) let recently-paid node win every tiebreak for 9 consecutive blocks | ✅ Fixed (`d209b70`): `FAIRNESS_V2_HEIGHT = 1730` gates switch from `/10` to direct counter; Step 3b extended to Free tier — rejects blocks where a freshly-paid address (counter=0) receives rewards while other nodes with higher counters were skipped |
| AV36 | Reputation poisoning / blacklist manipulation | Attacker causes an honest node to accumulate violations and be banned from producing or receiving connections. Three sub-paths: (A) forged block proposals with victim's IP as `leader` and bad reward distribution → every validator calls `record_reward_violation(victim_ip)`; (B) relay-forwarded gossip announces with victim's IP as `masternode_ip` pointing to locked collateral → every node calls `record_severe_violation(victim_ip)`; (C) clock-drift / key-rotation triggers causing a legitimate peer to breach the AV27/AV28 thresholds | (A) `validate_proposal_rewards` recorded violations before leader identity was authenticated; (B) `CollateralAlreadyLocked` path always blamed `masternode_ip` regardless of relay; (C) inherent in sliding-window thresholds | ✅ Fixed: (A) `validate_block_before_vote` now authenticates the leader via VRF proof BEFORE calling `validate_proposal_rewards`; unauthenticated failures record a violation against the *sending peer* and pass `record_violations: false` to avoid poisoning the claimed leader. (B) Both `CollateralAlreadyLocked` paths now attribute violations to the *relay peer* (minor violation) rather than the claimed `masternode_ip` when `is_relayed = true`. (C) Mitigated by 1-hour decay on reward violations and 30s/60s sliding windows on AV27/AV28 |
| AV37 | Registration spam — slot ID exhaustion | Attacker submits hundreds of valid `MasternodeRegistration` transactions for the same IP with different wallet addresses and txids; each call to `apply_masternode_registration()` burned a new slot_id from the global counter and overwrote the sled record, exhausting the slot namespace and corrupting fairness-rotation bitmaps | Idempotency guard only matched on txid equality; a different txid for the same IP was treated as a fresh registration | ✅ Fixed (v1.4.35, commit `268eaa9`): Height-gated one-slot-per-IP rule activates at `SLOT_UNIQUENESS_FORK_HEIGHT = 200`. Before that height, legacy slot assignment is used so chain replay produces identical slot_ids on all nodes (consensus-safe). From height 200, re-registrations with a new txid reuse the IP's existing slot_id. Observed attack: `188.26.80.38` registered 49×, `50.28.104.50` 29×, `64.91.241.10` 23× at height 160 |

### AI Detection Coverage

The `AttackDetector` (`src/ai/attack_detector.rs`) detects automatically and feeds `take_pending_mitigations()` → server enforcement loop (30s tick) → `IPBlacklist`:

| `AttackType` variant | Detected by | Mitigation |
|----------------------|-------------|------------|
| `SybilAttack` | `record_peer_connect` (>10 connects/60s per IP) | `BlockPeer` |
| `EclipseAttack` | `check_eclipse_attack` (peer diversity < 50%) | `EmergencySync` |
| `ForkBombing` | `record_fork` (≥5 forks in 300s window) | `BlockPeer` |
| `TimingAttack` | `record_timestamp` (avg drift > 30s over 5 samples) | `RateLimitPeer` |
| `DoublespendAttack` | `record_conflicting_transaction` (≥2 conflicting versions) | `AlertOperator` |
| `GossipEvictionStorm` | `record_eviction_storm_attempt` | `BlockPeer` |
| `CollateralSpoofing` | `record_collateral_spoof_attempt` | `BlockPeer` |
| `SyncLoopFlooding` | `record_sync_flood` | `RateLimitPeer` |
| `UtxoLockFlood` | `record_utxo_lock_flood` | `BlockPeer` |
| `ResourceExhaustion` | `record_pre_handshake_violation` (≥3 violations) | `RateLimitPeer` / `BlockPeer` |
| `InvalidVoteSignatureSpam` | `record_invalid_vote_sig_spam` (called from `message_handler.rs` after AV27 sliding window fires) | `RateLimitPeer` |
| `UnregisteredVoterSpam` | `record_unregistered_voter_spam` (called from `message_handler.rs` after AV28 sliding window fires) | `RateLimitPeer` |

All known attack vectors have AI detection coverage. No open items.

### Adding a New Attack Vector

1. **Document it here** in the appropriate subsection.
2. **Add the `AttackType` variant** to the enum in `src/ai/attack_detector.rs`.
3. **Add a `record_*` method** with sliding-window or threshold detection logic.
4. **Choose a `MitigationAction`**: `BlockPeer`, `BanSubnet`, `RateLimitPeer`, or `AlertOperator`.
5. **Wire the hook** in `src/network/server.rs` at the relevant event site.
6. **Update the enforcement loop** in `NetworkServer::start()` if a new `MitigationAction` variant was added.
7. **Add a test** in `tests/consensus_security.rs` or `tests/security_audit.rs`.
8. **Commit** with prefix `security:` and reference the vector ID.

## Web API & Explorer

Base URL: `https://time-coin.io`

| Endpoint | Description |
|---|---|
| `/api/peers` | Mainnet peer list (JSON array of IPs) |
| `/api/testnet/peers` | Testnet peer list |
| `/api/explorer/mainnet/chain` | Chain stats |
| `/api/explorer/mainnet/blocks?limit=20` | Recent blocks |
| `/api/explorer/mainnet/blocks/{height}` | Block by height |
| `/api/explorer/mainnet/tx/{txid}` | Transaction detail |
| `/api/explorer/mainnet/address/{address}` | Address UTXOs and balance |
| `/api/explorer/mainnet/masternodes` | Masternode list |
| `/api/explorer/mainnet/search?q={query}` | Search (block/tx/address) |
| `/api/v1/nodes/peers/mainnet/full` | Full peer registry |

Replace `mainnet` with `testnet` for testnet queries. Explorer UI: `https://www.time-coin.io/explorer.html`

## Documentation References

- **Protocol spec**: `docs/TIMECOIN_PROTOCOL.md` (27 normative sections)
- **Architecture**: `docs/ARCHITECTURE_OVERVIEW.md`
- **Network layer**: `docs/NETWORK_ARCHITECTURE.md`
- **Security audit**: `docs/COMPREHENSIVE_SECURITY_AUDIT.md` (January 2026)
- **CLI guide**: `docs/CLI_GUIDE.md`
- **Quick reference**: `docs/QUICK_REFERENCE.md`
