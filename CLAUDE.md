# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Line Endings

This repo enforces **LF line endings** via `.gitattributes` (`* text=auto eol=lf`). When writing or editing any file, always use LF (`\n`), never CRLF (`\r\n`). On Windows, the Write tool may default to CRLF — be explicit. Failing to do so produces `warning: CRLF will be replaced by LF` noise on every commit.

## Build & Development Commands

```bash
# Build (debug)
cargo build

# Build (release — uses fat LTO, takes longer)
cargo build --release

# Run the daemon (debug build)
cargo run -- --testnet

# Run with verbose logging
cargo run -- --testnet --verbose

# Generate default config files and exit
cargo run -- --generate-config

# Run all tests
cargo test

# Run a single test by name
cargo test <test_name>

# Run a specific test file
cargo test --test fork_resolution

# Run with output shown (tests that print)
cargo test -- --nocapture

# Run benchmarks
cargo bench

# Check without building
cargo check

# Lint
cargo clippy

# Run the CLI tool
cargo run --bin time-cli -- --help

# Run the TUI dashboard
cargo run --bin time-dashboard
```

## Architecture Overview

This is **TIME Coin** (`timed`), a Rust blockchain daemon for a masternode-based proof-of-stake network. The binary is `timed`; `src/lib.rs` simply `include!`s `main.rs` so that integration tests can access all modules.

### Core Subsystems

**Blockchain & Consensus** (`src/blockchain.rs`, `src/consensus.rs`)
- `Blockchain` owns the sled database, block storage (zstd-compressed), UTXO set, and reorg logic.
- `ConsensusEngine` implements the **TimeVote** protocol — stake-weighted BFT voting for instant finality. Validators are sampled by VRF sortition using `SamplingWeight` (distinct from `GovernanceWeight` to prevent mix-ups).
- Finality proofs are assembled in `src/finality_proof.rs` and stored on-chain.
- Block production uses ECVRF (`src/block/vrf.rs`) for verifiable randomness in leader selection.

**Network Layer** (`src/network/`)
- `server.rs` — inbound TCP listener; handles TLS auto-detect (0x16 byte peek) and spawns per-connection tasks.
- `peer_connection.rs` — unified message loop (`run_message_loop_unified`), ping/pong liveness, fork resolution state machine.
- `peer_connection_registry.rs` — lock-free registry (DashMap) of active peers; uses channel-based writers (`PeerWriterTx = mpsc::UnboundedSender<Vec<u8>>`) to avoid TLS stream split issues.
- `connection_manager.rs` — tracks Connecting/Connected/Disconnected states; prevents duplicate outbound dials.
- `client.rs` — outbound connection logic; three-phase startup: (1) pyramid-topology masternode connections, (2) regular peer slots, (3) 30s periodic rediscovery loop. **Note: currently unused** — connection management is done directly in `main.rs`.
- `tls.rs` — rustls-based TLS; uses `AcceptAnyCertVerifier` (self-signed certs, message-level auth via Ed25519).
- `wire.rs` / `secure_transport.rs` — framing and signed-message transport.

**Masternode System** (`src/masternode_registry.rs`, `src/types.rs`)
- Four tiers: **Gold → Silver → Bronze → Free** (collateral-based, stored in `MasternodeTier`).
- Registry uses gossip-based liveness: a masternode is "active" when ≥3 peers have reported it recently (within 5 min).
- Network topology mirrors the tier pyramid: Gold nodes form a full mesh; lower tiers connect upward.
- Collateral UTXOs require 3 confirmations before masternode activation.

**AI Subsystem** (`src/ai/`)
- `AISystem` aggregates 7 modules initialized from a shared sled DB.
- Key modules: `AdaptiveReconnectionAI` (exponential backoff with per-peer learning), `AIPeerSelector` (scores peers for sync), `AttackDetector` (sybil/eclipse/fork-bomb detection), `AnomalyDetector` (z-score on network events).
- The AI peer selector picks best sync peers and logs them as `🤖 [AI] Selected best peer`.

**RPC Server** (`src/rpc/`)
- JSON-RPC 2.0 over HTTP/HTTPS on port 24001 (mainnet) / 24101 (testnet).
- Auto-detects TLS vs plain HTTP on the same port (0x16 byte peek).
- WebSocket notifications on port 24002/24102 (`rpc/websocket.rs`).

**Wallet** (`src/wallet.rs`)
- AES-256-GCM encryption with Argon2 key derivation.
- Ed25519 signing keys; `zeroize` used for secure memory cleanup.
- Encrypted memo support via X25519 ECDH.

**Storage** (`src/storage.rs`)
- `sled` embedded key-value store for blocks, UTXOs, and AI model state.
- Block serialization uses zstd compression (magic prefix `ZSTD`) with transparent legacy fallback.
- UTXOs are held in `InMemoryUtxoStorage` (HashMap + RwLock) at runtime for fast access.

### Configuration & Data Directories

- **Primary format**: `time.conf` (Dash-style `key=value`)
- **Legacy**: `config.toml` (auto-migrated)
- Data dirs: `~/.timecoin/` (Linux/Mac), `%APPDATA%\timecoin\` (Windows)
- Testnet uses `~/.timecoin/testnet/` subdirectory
- Config priority: `--conf` flag → `time.conf` in data dir → legacy TOML → CWD fallback

### Key Ports

| Network  | P2P   | RPC   | WebSocket |
|----------|-------|-------|-----------|
| Mainnet  | 24000 | 24001 | 24002     |
| Testnet  | 24100 | 24101 | 24102     |

### Important Design Constraints

- `src/lib.rs` uses `include!("./main.rs")` so integration tests in `tests/` can access all modules. All `pub mod` declarations live in `main.rs`.
- The tokio runtime is pinned to **4 worker threads minimum** (`#[tokio::main(worker_threads = 4)]`) to prevent sled I/O from starving network tasks on single-CPU VPS hosts.
- TLS close_notify errors from rustls are intentionally suppressed in `rpc/server.rs` — they are benign noise from HTTP clients that don't send a proper TLS shutdown.
- `#[allow(dead_code)]` is widespread because many items used only by the binary appear as "dead code" in library builds.
- Connection deduplication: both `ConnectionManager` (outbound state machine) and `PeerConnectionRegistry` (active session registry) must agree on a peer's state — always update both.
