# Copilot Instructions for TIME Coin Protocol

## Build, Test, and Lint

### Building
```bash
# Debug build
cargo build

# Release build (optimized, ~1 minute)
cargo build --release

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

### Running
```bash
# Start daemon (testnet by default)
./target/release/timed

# Start with specific config
./target/release/timed --conf /path/to/time.conf

# CLI commands
./target/release/time-cli getblockchaininfo
./target/release/time-cli getbalance
./target/release/time-cli masternodelist

# Launch monitoring dashboard
./target/release/time-dashboard
```

## High-Level Architecture

### Consensus System (Two-Layer)

TIME Coin implements a unique dual-layer consensus:

1. **TimeVote Protocol (Transaction Layer)**: Real-time transaction finalization in <1 second via stake-weighted voting among masternodes. Transactions achieve deterministic finality BEFORE block inclusion. This is NOT a traditional BFT committee—it's leaderless with progressive vote accumulation requiring 51% stake threshold.

2. **TimeLock Protocol (Block Layer)**: Deterministic block production every 600 seconds using VRF sortition for fair producer selection. Blocks archive already-finalized transactions. TimeGuard fallback ensures bounded liveness (max 11.3 min recovery).

**Critical Flow**: TX submission → UTXO locking → TimeVote broadcast → Vote collection → Finalization (51% threshold) → TimeProof assembly → Block inclusion → Archival on chain.

### UTXO State Machine

Five states (not the typical 2):
- `Unspent` → `SpentPending` (locked during voting) → `Voting` (TimeVote active) → `Finalized` (51% votes achieved) → `Archived` (in block)

Transactions finalize during Voting phase, not block inclusion. This enables instant finality.

### Network Architecture

- **Connection Management**: Lock-free DashMap (not RwLock) for O(1) peer lookups and zero lock contention
- **Message Flow**: `NetworkMessage` → `message_handler.rs` → route to consensus/blockchain/sync
- **Peer Management**: `PeerConnectionRegistry` is the single source of truth for all peer operations
- **Initialization Order Critical**: Network server must start BEFORE consensus engine callback wiring

### AI Optimization Modules

8 AI systems (not just helpers, actively running):
- `peer_selector.rs`: ML-based peer scoring (70% faster sync)
- `fork_resolver.rs`: Multi-factor fork resolution with AI scoring
- `anomaly_detector.rs`: Real-time security monitoring
- `predictive_sync.rs`: Block arrival prediction
- Transaction analysis and validation
- Network optimization and resource management

Located in `src/ai/`, integrated throughout consensus and network layers.

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
- **Library re-export trick**: `src/lib.rs` includes main.rs for testability (expect "dead code" warnings)
- **Module structure**: Each major component (network, consensus, ai, block, crypto) has its own directory with `mod.rs`

### Async/Sync Patterns

- **Network operations**: Always async (tokio runtime)
- **Storage operations**: Sled is sync, wrap in `tokio::spawn_blocking` for CPU-intensive work
- **Consensus engine**: Internally async but exposed API is sync-friendly with channels
- **Lock-free where possible**: Use DashMap, AtomicBool, Arc for shared state (avoid RwLock in hot paths)

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
- Registration: Transaction creates locked UTXO → Wait 30 min confirmation → Register masternode
- Cleanup: Set `masternode=0` in time.conf and restart to deregister and unlock collateral

### Transaction Pool Management

Two separate pools:
1. **Pending pool** (`transaction_pool.rs`): Unfinalized transactions
2. **Finalized pool** (in `ConsensusEngine`): Transactions with 51% TimeVote approval

**Critical**: Finalized pool must NOT clear on every block add. Only clear transactions that are actually included in the added block (use `clear_finalized_txs(txids)` not `clear_finalized_transactions()`).

### Cryptography

- **Signatures**: Ed25519 (RFC 8032) via `ed25519-dalek`
- **Hashing**: BLAKE3 (fastest cryptographic hash)
- **VRF**: ECVRF (RFC 9381) for sortition
- **All timestamps**: 64-bit Unix time (Year 2106 safe, unlike Bitcoin's 32-bit)

### Configuration

- **Network separation**: Mainnet uses `~/.timecoin/`, Testnet uses `~/.timecoin/testnet/`
- **Genesis blocks**: Generated dynamically when masternodes register (no JSON files)
- **Port assignments**: Mainnet (24000/24001), Testnet (24100/24101)
- **Magic bytes**: Different per network for message disambiguation

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
1. Add method to `src/rpc/methods.rs`
2. Update method routing in `handle_request()`
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
