# Changelog

All notable changes to TimeCoin will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.5.6] - 2026-05-10

### Fixed

- **Outbound connections never announced masternode tier**: The V3/V4 masternode
  announcement was only sent in `server.rs` on inbound handshake completion. Nodes
  that dialled *out* to a peer never announced themselves, so peers saw them as
  unknown Free-tier nodes regardless of their actual collateral. Fixed by mirroring
  the announcement block into `peer_connection.rs` `run_message_loop_unified` for
  outbound connections. (Bug affected Gold-tier LW-London; commits `d9a4369`)

- **Collateral-Churn guard blocking legitimate tier upgrades**: When a masternode
  upgraded from Silver to Gold by updating `masternode.conf` without spending the old
  Silver UTXO, the Collateral-Churn Case A guard on remote nodes always blocked the
  new Gold announcement — because the old UTXO still existed on-chain. Fixed: Case A
  now checks `prefetched_utxo_addr == masternode.wallet_address` (ownership of the
  *new* UTXO verified) before blocking. Legitimate same-owner upgrades are allowed;
  hijack attempts (different wallet address) still produce `CollateralRewardRedirect`.
  (Commit `31ac714`)

- **Startup log displayed IP instead of wallet address**: Two `main.rs` log lines used
  `mn.address` (node IP) where `mn.wallet_address` was intended, making it impossible
  to verify the reward address at a glance on startup. Cosmetic only; no functional
  effect. (Commit `d9a4369`)

### Added

- **Signed `MasternodeUnlock` — Dash `ProUpRevTx`-style collateral deregistration**:
  Commenting out a masternode's entry in `masternode.conf` and restarting now
  broadcasts a cryptographically signed revoke message to the network. Remote peers
  verify the Ed25519 signature against the stored `public_key` for that node, then
  unregister it and release the collateral lock — without requiring the operator to
  spend the collateral UTXO. Unsigned revokes are still accepted from a direct TCP
  peer whose source IP matches the node IP (operator-local trust for observer nodes
  without a `masternodeprivkey`). Signature scheme:
  `Ed25519("TIME_COLLATERAL_REVOKE:<address>:<txid>:<vout>:<timestamp>")`.
  (Commit `8e13246`)

## [1.5.5] - 2026-05-09

### Fixed

- **`initialblockdownload` flag stuck `true` after rollback**: Nodes that
  completed a rollback (fork resolution or revert-to-genesis) and then
  successfully re-synced to the chain tip could permanently report
  `initialblockdownload: true` in `getblockchaininfo`, causing wallets to
  exclude them from the synced-peer list despite being at 100% progress.

  Root cause: rollback functions set `is_syncing = true` to activate the sync
  gate in `message_handler` during the rollback window, then called
  `sync_from_peers()`. The function's concurrent-sync guard checked that same
  flag and returned immediately — without ever installing the RAII guard that
  clears it on exit. The flag stayed set forever.

  Fix: replaced the `is_syncing.load()` concurrency check in `sync_from_peers()`
  with `try_lock()` on a new dedicated `sync_in_progress_lock` mutex (same
  pattern as `block_processing_lock` / `fork_resolution_lock`). The `is_syncing`
  RAII guard is now installed at the very top of the function, covering all
  early-return paths.

## [1.4.34] - 2026-04-04

### Security — Masternode Registration & Collateral

- **Forged `CollateralUnlock` blocked**: `validate_collateral_unlock` now verifies the `owner_pubkey` against the collateral UTXO's actual on-chain address (`utxo.address`) rather than the registry entry's stored key. An attacker who gossip-squatted a masternode slot could previously have their own key stored in the registry, blocking the real owner from submitting a legitimate unlock. Ground truth is always the UTXO, not the registry.

- **Gossip anchoring removed for paid tiers**: Bronze/Silver/Gold collateral outpoints can no longer be anchored via peer gossip. Only a confirmed on-chain `MasternodeReg` transaction (signed by the collateral owner's private key) may anchor a paid-tier collateral. Prevents an attacker from gossip-squatting a collateral UTXO before the real owner registers.

- **Payout address locked to collateral owner**: `MasternodeReg` transactions where `payout_address` does not equal `utxo.address` are now rejected at the mempool/relay level. Rewards must flow to the collateral owner — no redirection is possible even with a valid registration signature. (Mempool rule only; existing blocks are not affected.)

- **`CollateralUnlock` signature verification tightened**: Verifies `owner_pubkey` against the on-chain UTXO address rather than the registry's stored public key, which may have been gossip-filled by an attacker.

### Fixed — Collateral Lock Persisting After On-Chain Spend

- **Spent collateral UTXOs stuck in lock map**: When a collateral-locked UTXO was spent on-chain (e.g. via a consolidation transaction), `spend_utxo` returned `LockedAsCollateral` and silently aborted without removing the UTXO from storage. The UTXO remained in `Unspent` state locally, so `check_collateral_validity` kept returning `true` and `cleanup_invalid_collaterals` never deregistered the masternode. Fixed: `spend_utxo` now releases any collateral lock before spending — a confirmed block is authoritative and overrides application-layer locks. The masternode auto-deregisters on the next cleanup sweep.

### Added — CLI Tooling

- **`dumpprivkey` command**: Exports the Ed25519 private key from a `wallet.dat` file offline (no running daemon required). Prints address, pubkey, and privkey hex.

- **`masternodereg --privkey <hex>`**: Sign a `MasternodeReg` transaction using a raw hex private key instead of a wallet file. Enables registering from a GUI wallet machine while submitting to a remote node's RPC via `--rpc-url`.

- **`scripts/register-masternode.sh`**: End-to-end registration script for the cold-wallet-on-separate-machine workflow.

## [Unreleased]

### Fixed — Genesis Verification False Disconnect (Critical)

- **`handle_genesis_hash_response` always disconnected peers, including compatible ones**: The function had an unconditional `Err("DISCONNECT: genesis hash mismatch ...")` at the end of the function body — **outside** the `if/else` that compared hashes. When a peer's genesis matched ours, the compatible branch logged "✅ compatible" and called `reset_fork_errors`, but then fell through to the trailing `Err` which disconnected the peer with a spurious mismatch message showing *identical* hashes on both sides.

  Cascade effect observed on mainnet (April 6 2026, nodes stuck at height 753):
  - Every genesis-responding peer was immediately disconnected after passing verification
  - No peer ever reached `is_genesis_confirmed()` state
  - Fork resolution was unconditionally skipped for **all** peers: `"Skipping fork resolution — peer not genesis-confirmed (likely old code)"`
  - All nodes stuck at height 753 despite 20+ connected peers at heights 754–757

  Fix: compatible branch now calls `mark_genesis_confirmed()` and returns `Ok(None)`. `Err("DISCONNECT: ...")` is only returned when hashes genuinely differ. `mark_genesis_confirmed()` made `pub` so `message_handler` can call it directly alongside the existing `verify_genesis_compatibility` background path.

### Added — Windows Tooling

- **`scripts/install-masternode.bat`**: Automated Windows installer — checks prerequisites (Git, Rust, VS Build Tools), clones/updates the repo, builds release binaries, generates `time.conf` with random RPC credentials, adds binaries to `PATH`, and opens the P2P firewall port. Mirrors `install-masternode.sh`. Usage: `scripts\install-masternode.bat [mainnet|testnet]`.
- **`scripts/update.bat`**: Windows update script — pulls latest code, rebuilds, stops the running node, copies new binaries, and restarts. Supports `mainnet`, `testnet`, or `both` (default). Usage: `scripts\update.bat [mainnet|testnet|both]`.
- **`scripts/dashboard.bat`**: Windows TUI dashboard launcher — sets up the cargo `PATH` and runs `time-dashboard`. Usage: `scripts\dashboard.bat`.
- **`scripts/uninstall-masternode.bat`**: Windows uninstaller — stops the running process and NSSM service (if present), removes the firewall rule, and deletes binaries. Preserves blockchain data and wallet by default; prints manual `rmdir` commands for a full wipe. Usage: `scripts\uninstall-masternode.bat [mainnet|testnet]`.
- **`MASTERNODE_GUIDE.md` Windows Setup section**: New section covering automated installation (`install-masternode.bat`), manual build, running as a service (NSSM), updating, uninstalling, and firewall configuration.

### Fixed — Scripts

- **`scripts/uninstall-masternode.sh`**: Service name was hardcoded to `timed` regardless of network; now correctly uses `timetd` for testnet.
- **`scripts/uninstall-masternode.sh`**: Removed bogus `timecoin` OS user removal step — the install script runs the daemon as the invoking user, not a dedicated service account.

### Fixed — Transaction Processing During Sync

- **`TransactionFinalized` spam while syncing**: When a node was hundreds of blocks behind, peers would send `TransactionFinalized` messages referencing UTXOs created by blocks the node hadn't received yet. Every message was logged as `⚠️ Rejecting TransactionFinalized ... input not in storage`, generating noise and wasting CPU. The handler now drops `TransactionFinalized` messages immediately when `blockchain.is_syncing()` is true; peers re-broadcast once the node catches up.

### Fixed — Fork Resolution: Common Ancestor Search Always Failing

- **`BlockHashResponse` not routed to response channels**: The common ancestor search (`find_and_resolve_fork`) works by sending `GetBlockHash(height)` to a peer and awaiting the `BlockHashResponse` on a oneshot channel. However, when the response arrived in the inbound message loop, it fell through to the `_` catch-all which called `MessageHandler` — which returns `Ok(None)` and silently drops the response. The waiting channel never received anything, so every iteration timed out after 3 seconds. After 3 consecutive timeouts the search aborted with "Aborting common ancestor search: 3 consecutive failures". Fixed by explicitly matching `BlockHashResponse` in the server's inbound message loop and calling `peer_registry.handle_response()` to dispatch it to the waiting oneshot channel.

### Fixed — Message Too Large on Block Range Responses

- **`GetBlockRange` responder now enforces `MAX_BLOCKS_PER_RESPONSE` (50 blocks)**: `handle_get_block_range` previously served the full requested range with no cap. A peer (or our own fork handler) could request thousands of blocks and receive a 9 MB+ response. The responder now silently clamps `end_height` to `start_height + 49`, keeping responses under ~400 KB compressed. `MAX_FRAME_SIZE` remains at 8 MB.
- **Unbounded `GetBlockRange` in fork handler**: When `ChainWorkResponse` detected a peer with a better chain, it sent a single `GetBlockRange { start: fork_height, end: peer_height }` that could span thousands of blocks. The fork handler now caps the first batch to 50 blocks; subsequent batches are fetched by the normal sync path once the initial gap is filled.

### Fixed — Parallel Sync Deadlock (missing first chunk)

- **Sync stall at specific height**: When the peer assigned the first block chunk (e.g. 15132–15181) was unresponsive or couldn't serve that range, the parallel sync buffered hundreds of valid blocks from other peers but made no height progress. On the 30 s timeout it called `clear_pending_blocks()` — **destroying all the valid buffered blocks** — then reset `next_request_height` back to `current_height + 1` and retried with a single fallback peer. The fallback was chosen only from peers *not* already in `sync_peers`; when all connected peers were in `sync_peers` the fallback list was empty and the node gave up immediately. Two fixes applied:
  1. **Preserve the buffer on gap**: detect whether there are buffered blocks above `current_height + 1`. If yes, keep them and only re-request the missing leading range. Once the gap is filled the buffered blocks drain automatically.
  2. **Expanded fallback pool**: when a gap exists, the retry candidate list is now built from ALL connected peers minus only the specific peer(s) assigned to the failing leading chunk — instead of only peers outside `sync_peers`. This ensures a retry peer is always available even when all connected peers participated in the original sync round.

### Fixed — Network Stability (Testnet block 15162 incident)

- **Block size cap in producer**: `get_finalized_transactions_with_fees_for_block` now accumulates serialized transaction sizes and truncates the transaction set once the payload would exceed `MAX_BLOCK_ASSEMBLY_SIZE` (1.9 MB). Previously all finalized transactions were included unconditionally, allowing blocks to grow to 3+ MB and be rejected by every peer's validation check. Excess transactions remain in the finalized pool and are included in the next block.
- **Dual block size constants**: `MAX_BLOCK_SIZE` (validation cap, 4 MB) and `MAX_BLOCK_ASSEMBLY_SIZE` (producer cap, 1.9 MB) are now separate constants. Raising the validation cap allows nodes to accept legacy oversized blocks already on the majority chain while the producer cap prevents new oversized blocks from being created.
- **TLS I/O race condition**: Both inbound and outbound TLS connection paths used a single `tokio::select!` loop to interleave reads and writes on a shared `TlsStream`. When a write became ready while `read_message` was mid-read, Tokio cancelled and dropped the in-progress read future, silently discarding partially-consumed bytes from the TCP kernel buffer. The next `read_message` call picked up at the wrong frame offset, producing garbage frame lengths (e.g. 318,767,104 bytes). This fired at 30-second intervals matching `PING_INTERVAL_SECS`. Fixed by replacing the `select!` bridge with `tokio::io::split()` + two independent tasks (reader + writer), mirroring the existing non-TLS path.
- **Ping/pong rate limit no longer records banlist violations**: Excess pings from peers reconnecting due to sync failures accumulated as banlist violations, eventually triggering 1-hour bans on legitimate masternodes. Ping and pong now use a soft rate limit (`check_rate_limit_soft!`) that drops excess messages silently without recording a violation.
- **Reduced ban escalation durations**: 3rd violation ban reduced from 5 min → 1 min; 5th violation ban reduced from 1 hr → 5 min. Severe protocol violations (corrupted blocks, reorg attacks) retain 1-hour bans via `record_severe_violation`.

## [1.2.3] - 2026-03-14

### Added — Encrypted Transaction Memos
- **Native encrypted memo field**: Transactions now support an optional `encrypted_memo` field (max 256 chars plaintext) encrypted using ECDH (Ed25519 → X25519) + AES-256-GCM. Only the sender and recipient can decrypt the memo; other nodes see only ciphertext on-chain.
- **Wire format**: Version byte + sender pubkey (32B) + recipient pubkey (32B) + AES-GCM nonce (12B) + ciphertext with auth tag. Both public keys stored so either party can reconstruct the ECDH shared secret.
- **Automatic consolidation memos**: UTXO consolidation and merge transactions automatically attach an encrypted "UTXO Consolidation" or "UTXO Merge" memo.
- **CLI `--memo` flag**: `time-cli sendtoaddress <addr> <amount> --memo "text"` attaches an encrypted memo.
- **RPC memo parameter**: `sendtoaddress` and `sendfrom` RPC methods accept an optional memo as the 5th/6th parameter.
- **Wallet display**: `listtransactions` decrypts and displays memos when the wallet holds the sender or recipient key.
- **Pubkey cache**: `UTXOStateManager` maintains a cache of Ed25519 public keys observed during signature verification, enabling memo encryption for any address that has signed an on-chain transaction.

### Added — Payment Request URIs
- **`requestpayment` CLI command**: Generate a `timecoin:` URI containing the recipient's address, Ed25519 public key, requested amount, and optional memo/label. Share via email, text, or QR code.
- **`payrequest` CLI command**: Parse a payment request URI and send funds with an encrypted memo in one step. The recipient's public key is automatically cached from the URI, solving the pubkey discovery problem for new addresses.
- **URI format**: `timecoin:ADDRESS?amount=X&pubkey=HEX&memo=TEXT&label=NAME` — compatible with future GUI/mobile wallets.

### Fixed — Transaction Safety
- **Re-broadcast loop prevention**: Re-broadcast task now validates input UTXO states before re-broadcasting; evicts transactions with reverted/missing inputs instead of re-sending them.
- **TransactionFinalized handler hardening**: Rejects transactions with missing input UTXOs, prevents double-finalization, short-circuits if transaction already finalized.
- **Block production TX validation**: Validates input UTXO states before including transactions in blocks; evicts invalid ones from the pool.
- **Safe `clearstucktransactions`**: Pre-flight check verifies all input UTXOs exist before clearing; skips unsafe transactions and reports values/fees.

## [1.2.2] - 2026-03-09

### Fixed — Solo Block Production & Fork Prevention
- **Solo block production eliminated**: Longest-chain-rule path now requires `MIN_AGREEING_PEERS` (2) peers confirming they're on the same chain before allowing production. A single node can no longer produce blocks even if it believes it has the "longest chain."
- **Fallback consensus requires peer votes**: `prepare_weight > 1` required for TimeGuard fallback — the producer's own vote alone no longer counts. Previously `prepare_weight > 0` always succeeded since the producer votes for its own block.
- **Block timing enforcement**: Blocks cannot be produced before their scheduled timestamp (`genesis_ts + height × BLOCK_TIME`). `validate_block()` also rejects incoming blocks produced too early (30s clock-skew grace, only for recent blocks within 10 of chain tip).
- **Single-node attack defense**: A single peer claiming a higher height cannot stall the network. Block production is only halted when ≥2 independent peers report a plausible height ahead (within 1 block of time-based expected height). A single peer triggers background sync but does not block production.

### Fixed — Sync & Catch-Up
- **Sync loop no longer blocks catch-up production**: When all peers are at the same height and far behind target, `sync_from_peers` now requests fresh chain tips, waits 3s, and re-checks. If no peer is ahead after refresh, returns immediately to allow catch-up block production. Previously fell through to a 120s sync loop that accomplished nothing.
- **Consensus failure triggers sync**: When `check_2_3_consensus` fails in the block production loop, the node now spawns `sync_from_peers` in the background instead of just retrying production endlessly.
- **Fork alerts update chain tip cache**: `handle_fork_alert` now updates the alerting peer's chain tip with the consensus height/hash, so `sync_from_peers` can discover peers ahead of us. Previously the chain tip cache remained stale after fork alerts.

### Fixed — Fork Resolution (Bugs 1-4)
- **Bug 1 — Same-height fork detection**: `check_2_3_consensus_for_production()` no longer allows production via the "longest chain rule" escape when peers at the same height have different block hashes.
- **Bug 2 — Deterministic fork tiebreaker**: Same-height forks now use a deterministic hash comparison (lower hash wins) instead of requiring >50% weighted majority, which was impossible when N nodes were on N different chains.
- **Bug 3 — Non-consensus sync deadlock**: Lowered the non-consensus peer blocking threshold from gap > 20 to gap > 10, allowing behind nodes to sync from any peer when significantly behind.
- **Bug 4 — Fork resolution infinite loop**: `handle_fork` now requests blocks in small batches (`FORK_RESOLUTION_BATCH_SIZE=20`) with 60s stall detection. `MAX_BLOCKS_PER_RESPONSE=50` caps response size to prevent 8MB frame overflow.

### Fixed — Block Validation & Sync
- **Bitmap-based registry divergence detection**: `validate_reward_distribution` uses `active_masternodes_bitmap` set bits to determine how many masternodes were active when the block was produced. If bitmap count differs from local registry count, strict reward checks become warnings — enabling sync through historical blocks with different reward rules.
- **Max block size increased**: 1MB → 2MB (`MAX_BLOCK_SIZE`) to accommodate large blocks with many transactions.
- **`sync_from_specific_peer` fallback**: When consensus height is known but peer chain tip is missing, falls back to requesting a small batch instead of failing with "No chain tip data."
- **Fork detection patterns expanded**: `"incorrect block_reward"` and `"pool theft"` errors now trigger fork resolution instead of falling into the generic error handler.

### Changed — Faster Peer Connections
- **`PEER_WAIT_SECS`**: 15s → 5s (initial wait for peer connections at startup)
- **`GENESIS_WAIT_SECS`**: 20s → 10s (genesis block response wait)
- **`BASE_DISCOVERY_WAIT`**: 30s → 10s (discovery round backoff base, reducing exponential sequence from 30/60/90s to 10/20/30s)
- **Peer exchange broadcast**: 60s → 30s (GetMasternodes interval)
- **Health monitoring**: Start delay 120s → 30s, interval 120s → 60s
- **Peer discovery loop (PHASE 3)**: 120s → 30s (rediscovery interval)
- Best-case startup-to-connected reduced from ~43s to ~18s.

### Added
- **Remote uptime via announcements**: `started_at: u64` field added to `MasternodeAnnouncementV3`. Dashboard computes real uptime from `daemon_started_at` instead of relying on local `total_uptime`. Masternodes sorted by tier (Gold→Free) in dashboard.
- **Stale pending TX cleanup**: `cleanup_stale_pending()` runs every 10 minutes in the main loop, reverting UTXOs for transactions pending > 300s.

## [1.2.1] - 2026-03-08

### Fixed — Consensus & Block Production
- **`select_leader()` used wrong weight function**: Was using `reward_weight()` (1:5:20:60) instead of `sampling_weight()` (1:10:100:1000) per protocol §5.2/§9.2. Invisible on all-Free testnet but would have given Gold nodes 60x instead of 1000x selection probability.
- **`getblock` RPC returned wrong hash**: Was computing hash from 4 fields inline; now uses canonical `Block::hash()` (11-field SHA-256).
- **Uptime always showed 0**: `total_uptime` only accumulated on disconnect; active nodes always read 0. RPC now computes `total_uptime + (now - uptime_start)` at query time.
- **Fairness bonus cap removed**: Removed stale `.min(20)` caps from 5 locations (VRF leader selection in `main.rs`, `message_handler.rs`, `masternode_registry.rs`). The cap had already been removed from `blockchain.rs` reward selection, creating an inconsistency. Fairness bonus now grows linearly without bound (`blocks_without_reward / 10`), matching the whitepaper specification.

### Fixed — Network & Peer Management
- **Persistent reconnection to offline peers**: Three-part fix:
  1. `list_by_tier()` now filters `is_active` — PHASE 1 startup no longer dials inactive masternodes
  2. PHASE 3 eviction check moved before AI cooldown check — previously only ran during cooldown periods, so eviction never triggered when cooldown expired
  3. `PeerManager` tracks evicted IPs with 1-hour cooldown — prevents PeerExchange gossip from immediately re-adding just-evicted peers
- **Eviction threshold reduced**: `FORGET_THRESHOLD` lowered from 10 to 5 consecutive failures for faster cleanup of dead peers

### Added
- **Hardcoded testnet genesis block**: `GenesisBlock::testnet_genesis()` creates deterministic genesis with 6 masternodes, verified hash `866273...`. Fresh nodes recreate identical genesis without network bootstrapping.
- **Dynamic block production quorum**: `max(active_masternodes / 3, 3)` instead of hardcoded 3
- **Dashboard network tab enhancements**: Scrollable peer table with status indicator (●/○), direction, address, type (Masternode/Peer), height, ping columns. Color-coded rows (green=active outbound, cyan=inbound, gray=inactive). In/out breakdown in network summary.

## [1.2.0] - 2026-02-12

### Changed - Config-Based Masternode Management
- **BREAKING: Removed `masternoderegister` and `masternodeunlock` RPC/CLI commands**
  - Masternode registration is now entirely config-based (see [Unreleased] for time.conf migration)
  - Set `masternode=1` in time.conf + collateral in masternode.conf (or legacy `config.toml`)
  - Tier is auto-detected from collateral UTXO value (or set explicitly with `tier`)
  - Daemon auto-registers on startup; deregister by setting `enabled = false`
  - Eliminates security vulnerability where anyone with RPC access could deregister masternodes
- **`MasternodeUnlock` network messages are now ignored** (logged as deprecated)
  - Variant kept in `NetworkMessage` enum for bincode serialization compatibility
- **Exact collateral amounts required** (was >=, now must be exactly 1000/10000/100000 TIME)
- **CLI defaults to mainnet** (port 24001); use `--testnet` flag for testnet (port 24101)
- **Dashboard auto-detects network** (tries mainnet first); `--testnet` reverses priority

### Added
- `collateral_vout` field in `[masternode]` config section
- `--testnet` flag for `time-cli` and `time-dashboard`
- Fee breakdown documentation for collateral transactions (0.1% fee)

### Security
- Removed unauthenticated `masternodeunlock` RPC endpoint (anyone could deregister any masternode)
- Removed unsigned `MasternodeUnlock` network message handling (any peer could forge deregistration)

## [Unreleased]

### Changed — Config Migration (time.conf + masternode.conf)
- **BREAKING: Configuration migrated from config.toml to time.conf + masternode.conf** (Dash-style)
  - `time.conf`: key=value format for daemon settings (`masternode=1`, `masternodeprivkey=`, `testnet=1`, etc.)
  - `masternode.conf`: collateral entries (`alias txid vout`)
  - Legacy config.toml still supported but deprecated; daemon logs a warning
- **Legacy TOML path now also loads masternode.conf and masternodeprivkey from time.conf**
  - Existing deployments using `--config config.toml` work without changes
- **Startup output defers tier/collateral display when auto-detection is pending**
  - No longer shows misleading "Running as Free masternode / Collateral: 0 TIME" before UTXO lookup
- **Removed all user-facing references to config.toml** (observer mode hint, comments, log messages)
- **Scripts rewritten for new config system**:
  - `configure-masternode.sh/.bat` — writes time.conf + masternode.conf (was editing TOML `[masternode]` section)
  - `deploy-config.sh/.bat` — creates default time.conf + masternode.conf (was copying TOML templates)

### Added — Per-Address UTXO Index & Auto-Consolidation
- **Per-address UTXO index** (`DashMap<String, DashSet<OutPoint>>`) in UTXOStateManager
  - O(n) in address UTXOs instead of scanning the entire UTXO set
  - Maintained incrementally on add/spend/remove, built at startup from full scan
  - Used by: `get_balance`, `list_unspent`, `list_received_by_address`, `send_to_address`, `merge_utxos`
- **Auto-consolidation** in `sendtoaddress` when a transfer needs more than 5000 inputs
  - Merges smallest UTXOs into a single consolidation TX, waits for finalization, retries original send
  - Falls back to a clear error message if consolidation fails
- **MAX_TX_INPUTS limit** (5000) with OOM-prevention error message recommending smaller amounts
- **`scripts/migrate-systemd.sh`** — migrates deployed systemd service from `--config config.toml` to `--conf time.conf`

### Added — Operational Scripts
- **`scripts/backup-node.sh`** — Full/wallet-only backup with timestamped tarballs, hot backup mode, disk space check
- **`scripts/restore-node.sh`** — Restore from backup with integrity verification, safety wallet backup, confirmation prompt
- **`scripts/health-check.sh`** — Production health probe returning exit codes 0/1/2 (healthy/degraded/critical), JSON output mode, cron-friendly quiet mode
- **`scripts/reindex.sh`** — Safe blockchain reindex with full (blocking) and tx-only (background) modes, progress monitoring
- **`scripts/node-monitor.sh`** — Persistent log watcher with color-coded event categories (errors, forks, blocks, masternode events), optional log file output

### Fixed — Script Correctness
- **All scripts now use correct camelCase CLI commands** (was kebab-case `get-block-count` → `getblockcount`, etc.)
- **`setup_local_testnet.sh` rewritten** — uses `--conf`/`--datadir` flags and generates time.conf per node (was using nonexistent `--validator-id`/`--port`/`--peers` flags)
- **Server lists parameterized** — `diagnose_fork_state.sh`, `emergency_recovery.sh`, `test_finalization_propagation.sh` accept `SERVERS`/`NODES` env var override
- **`stability_test.sh`** — updated ports to match local testnet (24111/24121/24131)
- **`diagnose_status.sh`** — removed invalid positional arg from `getrawmempool`

### Removed
- **config.mainnet.toml and config.testnet.toml** template files from repository root

### Fixed — Collateral Recollateralization Race Condition
- **CRITICAL: `check_collateral_validity()` no longer falsely deregisters masternodes during recollateralization**
  - `cleanup_invalid_collaterals()` ran inside `add_block()` after `process_block_utxos()` created a new collateral UTXO but before it was locked — the masternode was incorrectly deregistered
  - Now auto-locks valid Unspent collateral UTXOs instead of declaring them invalid
- **CRITICAL: Local masternode is never auto-deregistered by `cleanup_invalid_collaterals()`**
  - Operator must explicitly disable via `enabled = false` in config
  - Prevents wallet blindness where all RPC balance/UTXO queries returned 0
- **Wallet RPCs (`getbalance`, `listunspent`) now fall back to stored `local_wallet_address`**
  - If `get_local_masternode()` returns `None` after unexpected deregistration, UTXOs are still visible

### Security — Block Reward Enforcement & Misbehavior Tracking
- **Block reward distribution is validated BEFORE voting** (was only checked during `add_block()`)
  - `validate_block_before_vote()` now calls `validate_proposal_rewards()` which runs the full pool distribution check
  - If rewards deviate beyond `GOLD_POOL_SATOSHIS` (25 TIME) the node refuses to vote
  - Block fails to reach consensus → TimeGuard fallback → next VRF producer takes over
- **Per-producer misbehavior tracking** with lifetime violation counter
  - After 3 reward violations a producer is marked as misbehaving
  - All future proposals from that producer are rejected
  - Violations are logged: `⚠️ Producer X reward violation (N/3 strikes)`
  - Threshold breach: `🚨 Producer X is MISBEHAVING, future proposals will be rejected`
- **Reward deviation tolerance capped at `GOLD_POOL_SATOSHIS` (25 TIME) per recipient**
  - Prevents modified nodes from skewing reward distribution within a tier pool
  - Total block reward is still strictly validated (100 TIME + fees)
  - Per-recipient deviations within the cap are accepted with a warning (handles masternode list divergence)

### Security — Masternode Collateral & UTXO Hardening
- **CRITICAL: Prevent duplicate collateral registration across masternodes**
  - Same UTXO could register multiple masternodes (outpoint uniqueness not enforced)
  - Added `DuplicateCollateral` error with outpoint scan in `register_internal()`
- **CRITICAL: Collateral locks now persist across daemon restarts**
  - `locked_collaterals` DashMap was in-memory only; restart cleared all locks
  - Added `rebuild_collateral_locks()` called on startup for all known masternodes
- **CRITICAL: Reject unsigned transaction inputs (empty script_sig)**
  - Transactions with empty signatures were accepted into the mempool
- **TX validation now checks collateral locks** (prevents mempool pollution with locked UTXOs)
- **Reject locking non-existent UTXOs** (Vacant entry path created phantom locks)
- **Guard `force_unlock()` against collateral-locked UTXOs**

### Security — Protocol Consensus Hardening
- **Block 2PC now uses stake weight instead of raw vote count**
  - Previously used `vote_count > effective_size / 2` (node count)
  - Free-tier Sybil attack: many zero-stake nodes could dominate block consensus
  - Now accumulates stake weight and requires >50% of total participating weight
- **Raised Q_FINALITY from 51% to 67% (BFT-safe majority)**
  - 51% threshold only tolerated 49% Byzantine; 67% tolerates up to 33%
  - Liveness fallback: threshold drops to 51% after 30s stall to prevent deadlock
  - Updated across all finality checks: consensus, finality_proof, types, timelock,
    message_handler, server
- **Fallback leader election now includes `prev_block_hash`**
  - Previously used only public values (txid, slot_index, mn_pubkey) — fully predictable
  - Adding latest block hash prevents prediction before block production, mitigating
    targeted DDoS against known fallback leaders
- **Free tier VRF weight capped below Bronze base weight**
  - Free tier with max fairness bonus (+20) reached effective weight 21, exceeding Bronze (10)
  - Capped at `Bronze.sampling_weight() - 1 = 9` for both local and total VRF calculations
- **Emergency VRF fallback requires Bronze+ tier**
  - Emergency threshold relaxation (`2^attempt` multiplier) no longer applies to Free tier
  - Maintains Sybil resistance even when legitimate VRF winners are unavailable
- **Added `SamplingWeight` and `GovernanceWeight` newtypes**
  - Type-safe wrappers prevent accidental interchange between consensus weights and
    governance voting power (10x discrepancy at Gold tier)
- **AVS witness subnet diversity requirement**
  - Liveness heartbeat witnesses must come from ≥2 distinct /16 subnets (networks ≥5 nodes)
  - Prevents targeted DDoS against a node's 3 witnesses on the same subnet
- **Added FIXME(security) for catastrophic conflict recovery mechanism**

### Fixed - Same-Height Fork Resolution Blocked by Reorg Guard
- **CRITICAL: Deterministic same-height fork resolution never completed**
  - `perform_reorg()` rejected reorgs where `new_height <= our_height`
  - `handle_fork()` correctly decided to accept peer chain via hash tiebreaker,
    but `perform_reorg()` then rejected it as "equal or shorter chain"
  - This caused infinite fork resolution loops at same height (e.g., height 10999)
  - Fix: Allow same-height reorgs (`<` instead of `<=`). The caller already
    validated acceptance via deterministic tiebreaker (lower hash wins)

### Fixed - Fork Resolution Gap When Peer Response Capped at 100 Blocks
- **Fork resolution failed with "non-contiguous blocks" when block 10998 was missing**
  - GetBlocks response capped at 100 blocks (e.g., 10898-10997), but block 10999
    arrived separately, leaving a gap at 10998
  - The start-height check passed (10997 == common_ancestor + 1) but the chain
    had a hole: blocks 10997 and 10999 present, 10998 missing
  - Fix: After start-height check, detect gaps by comparing block count to expected
    count. If gap found, store current blocks as accumulated and request missing range

### Fixed - Block Production Requires Minimum 3 Nodes In Sync
- **Block production now requires at least 2 agreeing peers (3 nodes total)**
  - Previously only checked weighted 2/3 threshold, which could be met by a single
    high-tier masternode agreeing
  - With network fragmented into 3 chains, no chain had enough peers to produce
  - Fix: Added `MIN_AGREEING_PEERS = 2` count check alongside the weight threshold
  - Also fixed unregistered peer default weight from Bronze (3) to Free (1) to match
    `compare_chain_with_peers()` and prevent phantom weight inflation

### Fixed - Incompatible Peers Poison Consensus and Fork Detection
- **CRITICAL: Incompatible peers (wrong genesis hash) diluted 2/3 consensus threshold**
  - `check_2_3_consensus_for_production()` used `get_connected_peers()` which includes
    peers marked incompatible (e.g., genesis hash mismatch `0000260000000000`)
  - These peers' weight inflated `total_weight` but they never agreed on our chain tip
  - With 2 incompatible + 3 compatible peers, the 2/3 threshold became unreachable
  - Result: Network stalled at height 10990, VRF relaxing for 8600+ seconds with no blocks produced
  - Fix: Switch to `get_compatible_peers()` in consensus check, sync peer height check,
    bootstrap scenario check, and fork detection peer counting
  - Incompatible peers are still connected (for peer discovery) but excluded from all
    consensus-critical decisions

### Fixed - Future-Timestamp Blocks Rejected During Catchup
- **CRITICAL: Catchup mode produced blocks with timestamps minutes in the future**
  - During fast catchup (>5 blocks behind), the time gate was bypassed entirely
  - This allowed producing blocks whose scheduled timestamp exceeded `now + 60s`
  - Receiving nodes correctly rejected these blocks ("too far in future")
  - Caused ~10 minute stalls as the network waited for the timestamp to arrive
  - Fix: Early time gate now applies to ALL modes — blocks are never produced when
    their scheduled timestamp exceeds `now + TIMESTAMP_TOLERANCE_SECS` (60s)
  - Catchup still runs at full speed for past-due blocks, only pauses at the frontier

### Fixed - Block Production Log Spam During Participation Recovery
- **Block production loop ran expensive masternode selection every second even when next block wasn't due**
  - Added early time gate before masternode selection — skips the entire masternode
    bitmap/fallback logic when the next block's scheduled time hasn't arrived
  - Rate-limited participation tracking failure logs to once per 60 seconds
  - Downgraded bitmap fallback messages from WARN/ERROR to DEBUG when fallback succeeds
  - Eliminates ~5 ERROR log lines per second during normal inter-block waiting periods

### Fixed - Block Reward Mismatch on Double-Spend Exclusion
- **CRITICAL: Blocks rejected by all nodes when containing double-spend transactions**
  - Block producer calculated `block_reward` (base + fees) BEFORE filtering double-spend TXs
  - After filtering, the block contained fewer TXs but the inflated `block_reward` remained
  - Validators recalculated fees from the actual block TXs and got a lower total → rejection
  - Caused all nodes to get stuck at the same height with infinite retry loops
  - Fix: Move double-spend/duplicate filtering before fee calculation so `block_reward`
    only includes fees from transactions that actually make it into the block

### Improved - UTXO Log Readability
- **OutPoint now displays as `hex_txid:vout` instead of raw byte arrays**
  - Added `Display` impl for `OutPoint` struct
  - Updated all UTXO manager log lines to use the new format

### Fixed - Fork Resolution Infinite Loop
- **CRITICAL: Fork resolution stuck in infinite retry loop when peer splits block response**
  - `handle_fork()` filtered the raw `blocks` parameter instead of the merged `all_blocks` set
  - When a peer responds in multiple TCP messages (e.g., 3 blocks + 100 blocks), the second
    `handle_fork()` call received blocks at heights ≤ common ancestor in its parameter, while
    the blocks above the ancestor were only in the accumulated/merged set
  - Filtering the wrong variable produced zero reorg candidates, triggering an infinite
    request→filter→empty→request loop
  - Fix: Filter `all_blocks` (merged set with accumulated blocks) instead of `blocks` (raw parameter)
  - Also fixed `peer_tip_block` selection to use merged block set for correct hash comparison

### Fixed - UTXO Contention Under Concurrent Load
- **`sendtoaddress` failed when multiple users sent transactions simultaneously**
  - Coin selection picked UTXOs that were `Unspent` at query time but got `Locked` by
    another concurrent transaction before `lock_and_validate_transaction` could lock them
  - Fix: On UTXO contention errors, exclude the contested outpoints and immediately
    re-select different UTXOs (up to 3 retries with growing exclusion set)
  - Transparent to callers — retries happen internally within the RPC handler

### Fixed - TimeProof Threshold Mismatch
- **TimeProof verification used 67% threshold instead of 51% (Protocol §8.3)**
  - `finality_proof.rs` correctly used 51% for local finalization checks
  - `types.rs` `TimeProof::verify()` incorrectly used 67%, causing peers to reject valid proofs
  - With total AVS weight 15: local threshold was 8, but peer verification required 10
  - Fix: Aligned `types.rs` to use 51% with `div_ceil` matching the protocol spec
- **Auto-finalized transactions broadcast under-weight TimeProofs**
  - After 5s timeout, TXs were auto-finalized and their TimeProofs broadcast regardless of weight
  - Peers rejected these with "Insufficient weight" warnings
  - Fix: Only broadcast TimeProof if accumulated weight meets 51% threshold; still finalize locally

### Removed
- **Deleted 8 obsolete scripts** from `scripts/` directory:
  - `deploy_fork_fixes.sh`, `deploy_utxo_fix.sh` — one-time deployment scripts for past bug fixes
  - `check_block_hash.sh` — investigated specific fork at block 1723 (resolved)
  - `diagnose_fork.sh` — diagnosed specific fork at heights 4388–4402 (resolved)
  - `reset-blockchain.sh`, `reset-testnet-db.sh`, `reset-testnet-nodes.sh` — one-time reset scripts
  - `cpctest.sh` — ad-hoc config copy utility with hardcoded paths

### Fixed - Script Compatibility
- **Fixed 5 transaction test scripts** with incorrect CLI command names or JSON parsing:
  - `test-wallet.sh` / `test-wallet.bat` — all commands used wrong dashed format (e.g., `get-balance` → `getbalance`)
  - `test_critical_flow.sh` — wrong masternode JSON path and version check format
  - `test_finalization_propagation.sh` — used non-existent `getmasternodes` command
  - `test_timevote.sh` — used total balance instead of available balance, replaced `bc` dependency with `awk`

### Fixed - Critical Security and Compatibility Issues

- **CRITICAL: Old Genesis Format Incompatibility**
  - Nodes with old JSON-based genesis blocks couldn't sync with network
  - Old format: empty transactions, no masternode rewards, no bitmap
  - New format: dynamic generation, leader gets 100 TIME reward, has active bitmap
  - Fix: Auto-detect old genesis on startup and clear it automatically
  - Result: Nodes seamlessly upgrade to new dynamic genesis format

- **Block Reward Validation Vulnerability (Security)**
  - Block reward validation relied on local state (`get_pending_fees()`)
  - Different nodes could have different views of correct reward
  - Attack: Malicious node could create blocks with inflated rewards (e.g., 1000 TIME vs 100 TIME)
  - Fix: Implemented cryptographic fee verification by scanning blockchain
  - Now calculates fees deterministically: `fee = inputs - outputs` for each transaction
  - Validates: `block_reward = BASE_REWARD (100 TIME) + calculated_fees`
  - Impact: **Prevents supply inflation attacks** - all nodes verify rewards identically

### Security Improvements

- **Proper Fee Calculation from Blockchain**
  - Added backward blockchain scan to verify UTXO values for fee calculation
  - Traces every satoshi back to its origin transaction
  - Rejects blocks if any UTXO cannot be found or validated
  - No arbitrary reward caps - natural limit based on actual transaction fees

- **Triple-Layer Block Reward Validation**
  1. Calculate fees from previous block's transactions (scan blockchain for UTXOs)
  2. Verify: `block_reward = BASE_REWARD + calculated_fees` (exact match required)
  3. Verify: total distributed = block_reward (existing check)
  - Result: Byzantine fault tolerant - no trust required, all cryptographically verified

### Fixed - Network & Consensus (February 9, 2026)

- **Same-Height Fork Resolution**: `spawn_sync_coordinator` now detects and resolves forks at the same height, not just when peers are ahead
- **Consensus Support Ratio**: Fixed denominator to use responding peers instead of all connected peers (2/3 of 3 responding = 67% pass, not 2/5 = 40% fail)
- **ChainTipResponse on Inbound Connections**: Server now handles `ChainTipResponse` messages from inbound peers (was silently dropped via `_ => {}` catch-all)
- **Inbound Message Dispatch**: Replaced silent `_ => {}` catch-all with `MessageHandler` delegation for unhandled message types
- **Fork Resolution Threshold**: Aligned fork resolution to use 2/3 weighted stake consensus (was >50% unweighted), matching block production threshold

### Improved - Event-Driven Block Production

- **Block Added Signal**: Added `block_added_signal` as a wake source in the main production `select!` loop
  - Loop now wakes immediately when any block is added (from peer sync, consensus, or own production)
  - Reduces catchup latency from up to 1 second to near-instant
  - 1-second interval kept as fallback for leader timeouts and chain tip refresh

### Improved - AI Attack Mitigation Enforcement

- **Wired Attack Detector to Banlist**: Attack detector now enforces recommended mitigations
  - `BlockPeer` → records violations (auto-escalating: 3→5min ban, 5→1hr, 10→permanent)
  - `RateLimitPeer` → records violations (escalates to ban on repeat offenses)
  - `AlertOperator` → logs critical alert
  - Whitelisted peers use `record_severe_violation` (overrides whitelist on 2nd offense)
  - Active peers are disconnected on ban
  - 30-second enforcement interval

### Removed - Dead Code Cleanup (~3,400 lines)

- **Deleted `src/network/fork_resolver.rs`** (-919 lines): Never called from any code path
- **Deleted `src/network/anomaly_detection.rs`**: Superseded by `ai/anomaly_detector.rs`
- **Deleted `src/network/block_optimization.rs`**: Never called
- **Deleted `src/network/connection_state.rs`** (-354 lines): Never imported outside its own module
- **Deleted `src/ai/transaction_analyzer.rs`** (-232 lines): Recorded data but no code ever queried results
- **Deleted `src/ai/resource_manager.rs`** (-191 lines): Created but no methods ever invoked
- **Deleted `src/transaction_priority.rs`** (-370 lines): `TransactionPriorityQueue` only used by unused `TransactionSelector`
- **Deleted `src/transaction_selection.rs`** (-226 lines): `TransactionSelector` never instantiated
- **Removed dead methods** from `blockchain.rs` and `ai/fork_resolver.rs`: `update_fork_outcome`, `get_fork_resolver_stats`, `ForkResolverStats`, `ForkOutcome`
- AI System reduced from 9 to 7 active modules

## [1.1.0] - 2026-01-28 - TimeVote Consensus Complete

### Fixed - Critical Transaction Flow Bugs

- **CRITICAL: Broadcast Callback Not Wired (commit c58a3ec)**
  - The consensus engine had no way to broadcast TimeVote requests to the network
  - `set_broadcast_callback()` method existed but was never called in main.rs
  - Result: Vote requests never sent, other nodes never received/finalized transactions
  - Fix: Wired up `peer_connection_registry.broadcast()` to consensus engine after network initialization
  - Impact: **This was preventing the entire TimeVote consensus system from working**

- **CRITICAL: Finalized Pool Cleared Incorrectly (commit 27b6a9f)**
  - Finalized transaction pool was cleared after EVERY block addition
  - Happened even when blocks came from other nodes and didn't contain our finalized transactions
  - Result: Locally finalized TXs lost before they could be included in locally produced blocks
  - Fix: Added `clear_finalized_txs()` to selectively clear only TXs that were in the added block
  - Extract txids from block, only remove those specific transactions from finalized pool

- **Version String Not Dynamic (commit 5d6bf8a)**
  - Version hardcoded as "1.0.0" instead of using Cargo.toml version
  - Made it impossible to distinguish nodes with new TimeVote code
  - Fix: Use `env!("CARGO_PKG_VERSION")` compile-time macro
  - Now automatically reflects version from Cargo.toml (1.1.0)

### Completed - TimeVote Transaction Consensus (Protocol v6.2 §7-8)

**Phase 1: Vote Signing & Weight Accumulation** (1 week)
- ✅ Implemented `TimeVote` structure with Ed25519 signatures
- ✅ Added `VoteDecision` enum (Accept/Reject)
- ✅ Implemented cryptographic vote signing and verification
- ✅ Added stake-weighted vote accumulation with DashMap
- ✅ Implemented 51% finality threshold calculation
- ✅ Added automatic finalization when threshold reached
- ✅ Byzantine-resistant consensus with signature verification

**Phase 2: TimeProof Assembly & Storage** (4 days)
- ✅ Implemented TimeProof assembly on finalization
- ✅ Added TimeProof verification method
- ✅ Integrated TimeProof storage into finality_proof_manager
- ✅ Added TimeProof broadcasting on finalization
- ✅ Implemented TimeProof request/response handlers
- ✅ Added offline TimeProof verification

**Phase 3: Block Production Pipeline** (2 hours - infrastructure already existed!)
- ✅ Enhanced logging for finalized TX inclusion in blocks
- ✅ Added TX validation framework before block inclusion
- ✅ Verified finalized pool cleanup after block addition
- ✅ Discovered Phase 3 was already 95% implemented
- ✅ Block production already queries `get_finalized_transactions_for_block()`
- ✅ UTXO processing and finalized TXs already included in blocks

**Transaction Flow Now Working:**
1. ✅ Transaction broadcast → pending pool
2. ✅ TimeVote requests → broadcast to all validators
3. ✅ Validators sign votes → return to submitter
4. ✅ Stake-weighted vote accumulation
5. ✅ 51% threshold → finalization (all nodes)
6. ✅ TimeProof assembly → broadcast to network
7. ✅ Block production → includes finalized TXs
8. ✅ UTXO processing → transaction archival
9. ✅ Selective finalized pool cleanup

### Technical Details

**Tier Weight System:**
- Free tier: sampling_weight = 1, reward_weight = 1
- Bronze tier: sampling_weight = 10, reward_weight = 10
- Silver tier: sampling_weight = 100, reward_weight = 100
- Gold tier: sampling_weight = 1000, reward_weight = 1000

**Auto-Finalization Fallback:**
- When validators don't respond (0 votes received within 3 seconds)
- System auto-finalizes if UTXOs are locked (double-spend protection via UTXO states)
- Transaction added to finalized pool locally
- Still requires gossip to other nodes for network-wide finalization

**Finalized Pool Management:**
- Transactions move from pending → finalized when consensus reached
- Multiple nodes can produce blocks, each queries their finalized pool
- Only TXs actually included in a block are cleared from pool
- Prevents premature clearing of transactions not yet in blocks

### Protocol Compliance

This release achieves full compliance with:
- ✅ Protocol v6.2 §6: Transaction validation
- ✅ Protocol v6.2 §7: TimeVote cryptographic voting
- ✅ Protocol v6.2 §8: TimeProof finality certificates  
- ✅ Protocol v6.2 §9: Block production with finalized transactions

### Files Modified

**Core Transaction Flow:**
- `src/consensus.rs` - TimeVote consensus, vote accumulation, finalization
- `src/transaction_pool.rs` - Finalized pool management, selective clearing
- `src/blockchain.rs` - Block production with finalized TXs, selective cleanup
- `src/timevote.rs` - TimeVote structure, signing, verification
- `src/finality_proof.rs` - TimeProof assembly, storage, broadcasting
- `src/network/server.rs` - TimeVote request/response handlers
- `src/main.rs` - Broadcast callback wiring, initialization

**RPC & Testing:**
- `src/rpc/handler.rs` - Dynamic version string from Cargo.toml
- `scripts/test_transaction.sh` - Complete Phase 1-3 flow validation

**Documentation:**
- `analysis/transaction_flow_analysis.md` - Complete flow analysis
- `analysis/phase_1_2_implementation.md` - Phase 1-2 implementation details
- `analysis/phase_3_summary.md` - Phase 3 completion summary
- `analysis/bug_fix_finalized_pool_clearing.md` - Bug #1 documentation
- `plan.md` - Implementation plan and progress tracking

### Known Limitations

- Auto-finalization fallback doesn't guarantee network-wide consensus (requires gossip)
- Free tier nodes must have TimeVote code for participation
- Version 1.0.0 nodes cannot participate in TimeVote consensus
- Network requires majority of nodes running v1.1.0 for proper operation

### Upgrade Instructions

**All Nodes Must Upgrade:**
```bash
cd ~/timecoin
git pull
cargo build --release
sudo systemctl restart timed
```

**Verify Upgrade:**
```bash
time-cli getpeerinfo | jq '.[] | {addr: .addr, version: .version, subver: .subver}'
# Should show: "version": 110000, "subver": "/timed:1.1.0/"
```

**Test Transaction Flow:**
```bash
bash scripts/test_transaction.sh
```

## [1.2.0] - 2026-01-28 - Protocol v6.2: TimeGuard Complete

### Fixed - Fork Resolution
- **Critical Bug Fix**: Fork resolution now properly handles historical block responses
  - When requesting historical blocks for fork resolution, received blocks are now routed directly to `handle_fork()`
  - Previously, blocks were re-processed through `add_block_with_fork_handling()`, triggering duplicate fork detection
  - Made `ForkResolutionState` and `fork_state` public for cross-module coordination
  - Fixes stuck fork resolution where nodes repeatedly detect the same fork without resolving it
  
- **Fork Resolution Enhancements**:
  - Preserve peer_height from FetchingChain state during block merging
  - Request missing blocks after finding common ancestor (fixes empty reorg blocks error)
  - Add detailed logging for fork resolution progress

- **Critical: Whitelisted Peer Protection**
  - Fixed bug where whitelisted masternodes could be disconnected on timeout
  - Old timeout check code path bypassed `should_disconnect()` protection
  - Whitelisted peers now NEVER disconnect regardless of missed pongs
  - Ensures persistent connections for essential network infrastructure

### Added - Liveness Fallback Protocol (§7.6 Complete Implementation)
- **Core Fallback Logic**
  - `start_stall_detection()` - Background task monitoring transactions every 5s for 30s+ stalls
  - `elect_fallback_leader()` - Deterministic hash-based leader election
  - `execute_fallback_as_leader()` - Leader workflow for broadcasting proposals
  - `start_fallback_resolution()` - Monitors FallbackResolution transactions
  - `start_fallback_timeout_monitor()` - Handles 10s round timeouts, max 5 rounds
  - `resolve_stalls_via_timelock()` - Ultimate fallback via TimeLock blocks

- **Security & Validation**
  - Equivocation detection for alerts and votes
  - Byzantine behavior detection (multiple proposals)
  - Vote weight validation (≤110% of total AVS)
  - Byzantine node flagging system

- **Monitoring & Metrics**
  - `FallbackMetrics` struct with 8 key metrics
  - Counters for activations, stalls, TimeLock resolutions
  - Comprehensive status logging

- **Block Structure**
  - Added `liveness_recovery: bool` to Block/BlockHeader
  - Backward compatible via `#[serde(default)]`

- **Testing**
  - 10+ comprehensive unit tests
  - All critical paths covered
  - Zero compilation warnings

### Changed
- **Protocol**: 6.1 → 6.2
- Updated documentation to mark §7.6 as fully implemented
- README badges updated to v6.2

### Performance
- Typical recovery: 35-45 seconds
- Worst-case: ≤11.3 minutes
- Memory: ~1KB per stalled transaction
- Byzantine tolerance: f=(n-1)/3

## [1.1.0] - 2026-01-21

### 🔒 Locked Collateral for Masternodes

This release adds Dash-style locked collateral for masternodes, providing on-chain proof of stake and preventing accidental spending of collateral.

### Added

#### Locked Collateral System
- **UTXO Locking** - Lock specific UTXOs as masternode collateral
  - Prevents spending while masternode is active
  - Automatic validation after each block
  - Thread-safe concurrent operations (DashMap)
- **Registration RPC** - `masternoderegister` command
  - Lock collateral atomically during registration
  - Tier validation (Bronze: 1,000 TIME, Silver: 10,000 TIME, Gold: 100,000 TIME)
  - 3 block confirmation requirement (~30 minutes)
- **Deregistration RPC** - `masternodeunlock` command
  - Unlock collateral and deregister masternode
  - Network broadcast of unlock events
- **List Collaterals RPC** - `listlockedcollaterals` command
  - View all locked collaterals with masternode details
  - Amount, height, timestamp information
- **Enhanced Masternode List** - Updated `masternodelist` output
  - Shows collateral lock status (🔒 Locked or Legacy)
  - Collateral outpoint display

#### Network Protocol
- **Collateral Synchronization** - Peer-to-peer collateral state sync
  - `GetLockedCollaterals` / `LockedCollateralsResponse` messages
  - Conflict detection for double-locked UTXOs
  - Validation against UTXO set
- **Unlock Broadcasts** - `MasternodeUnlock` network message
  - Real-time propagation of deregistrations
- **Announcement Updates** - `MasternodeAnnouncementData` includes collateral info
  - Optional `collateral_outpoint` field
  - Registered timestamp

#### Consensus Integration
- **Reward Filtering** - Only masternodes with valid collateral receive rewards
  - Legacy masternodes (no collateral) still eligible
  - Automatic filtering in `select_reward_recipients()`
- **Auto-Cleanup** - Invalid collaterals automatically removed
  - Runs after each block is added
  - Deregisters masternodes with spent collateral
  - Logged warnings for removed masternodes

#### CLI Enhancements
- **`time-cli masternoderegister`** - Register with locked collateral
- **`time-cli masternodeunlock`** - Unlock and deregister
- **`time-cli listlockedcollaterals`** - List all locked collaterals
- **Updated `time-cli masternodelist`** - Shows collateral status

### Changed
- **Masternode Structure** - Added optional collateral fields
  - `collateral_outpoint: Option<OutPoint>`
  - `locked_at: Option<u64>`
  - `unlock_height: Option<u64>`
- **UTXO Manager** - Enhanced with collateral tracking
  - `locked_collaterals: DashMap<OutPoint, LockedCollateral>`
  - New methods: `lock_collateral()`, `unlock_collateral()`, `is_collateral_locked()`
  - Spending prevention for locked collateral
- **Masternode Registry** - Collateral validation logic
  - `validate_collateral()` - Pre-registration checks
  - `check_collateral_validity()` - Post-registration monitoring
  - `cleanup_invalid_collaterals()` - Automatic deregistration

### Fixed
- **Double-Lock Prevention** - Cannot lock same UTXO twice
  - Returns `LockedAsCollateral` error
  - Added in response to test failures

### Testing
- **15+ New Tests** - Comprehensive test coverage
  - 7 UTXO manager tests (edge cases, concurrency)
  - 8 masternode registry tests (validation, cleanup, legacy compatibility)
  - All 202 tests passing ✅

### Documentation
- **MASTERNODE_GUIDE.md** - Complete masternode documentation
  - Setup instructions for both legacy and locked collateral
  - Troubleshooting guide
  - Migration instructions
  - FAQ section
- **MIGRATION_GUIDE.md** - Backward compatibility documentation (analysis/ folder)
  - Legacy vs locked collateral comparison
  - Step-by-step migration
  - No forced timeline
- **Updated README.md** - Added locked collateral to features
- **Updated CLI_GUIDE.md** - New command documentation

### Backward Compatibility
- ✅ **Fully backward compatible** - Legacy masternodes work unchanged
- ✅ **Optional migration** - No forced upgrade timeline
- ✅ **Equal rewards** - Both types eligible for rewards
- ✅ **Storage compatible** - `Option<OutPoint>` serializes cleanly

### Security
- **On-Chain Proof** - Locked collateral provides verifiable proof of stake
- **Spending Prevention** - Cannot accidentally spend locked UTXO
- **Automatic Validation** - Invalid collaterals detected and cleaned up
- **Network Verification** - Peers validate collateral state

---

## [1.0.0] - 2026-01-02

### 🎉 Major Release - Production Ready with AI Integration

This is the first production-ready release of TimeCoin, featuring a complete AI system for network optimization, improved fork resolution, and comprehensive documentation.

### Added

#### AI Systems
- **AI Peer Selection** - Intelligent peer scoring system that learns from historical performance
  - 70% faster syncing (120s → 35s average)
  - Persistent learning across node restarts
  - Automatic optimization without configuration
- **AI Fork Resolution** - Multi-factor fork decision system
  - 6-factor scoring: height, work, time, consensus, whitelist, reliability
  - Risk-based assessment (Low/Medium/High/Critical)
  - Learning from historical fork outcomes
  - Transparent decision logging with score breakdown
- **Anomaly Detection** - Real-time security monitoring
  - Statistical z-score analysis for unusual patterns
  - Attack pattern recognition
  - Automatic defensive mode
- **Predictive Sync** - Block arrival prediction
  - 30-50% latency reduction
  - Pre-fetching optimization
- **Transaction Analysis** - Pattern recognition and fraud detection
  - Fraud scoring (0.0-1.0)
  - UTXO efficiency analysis
- **Network Optimizer** - Dynamic parameter tuning
  - Auto-adjusts connection pools
  - Adaptive timeout values
  - Resource-aware optimization

#### Documentation
- **Consolidated Protocol Specification** - Single canonical document
  - Merged V5 and V6 into unified TIMECOIN_PROTOCOL.md
  - Version 6.0 with complete TSDC coverage
  - 27 comprehensive sections
- **AI System Documentation** - Public-facing AI documentation
  - Complete coverage of all 7 AI modules
  - Usage examples and configuration
  - Performance benchmarks
  - Privacy guarantees and troubleshooting
- **Organized Documentation Structure**
  - Clean root directory (2 files)
  - Public docs folder (19 files)
  - Internal analysis folder (428 files)

### Changed

#### Version Numbers
- **Node version**: 0.1.0 → 1.0.0
- **RPC version**: 10000 → 100000
- **Protocol**: V6.1 (TimeVote + TimeLock + TimeProof + TimeGuard)

#### Fork Resolution
- Replaced simple "longest chain wins" with multi-factor scoring
- Increased timestamp tolerance: 0s → 15s (network-aware)
- Deterministic same-height fork resolution
- Peer reliability tracking

#### Sync Performance
- Improved block sync using peer's actual tip height
- Fixed infinite sync loops
- Optimized common ancestor search (backwards from fork point)
- Better handling of partial block responses

### Fixed
- Block sync loop where nodes repeatedly requested blocks 0-100
- Fork resolution using wrong height comparison
- Sync timeout issues with consensus peers
- Genesis block searching from beginning instead of backwards

### Performance Improvements
- **Sync Speed**: 70% faster (AI peer selection)
- **Fee Costs**: 80% reduction (AI prediction)
- **Fork Resolution**: 83% faster (5s vs 30s)
- **Memory Usage**: +10MB (minimal overhead)
- **CPU Usage**: +1-2% (negligible)

### Security Enhancements
- Multi-factor fork resolution prevents malicious forks
- Real-time anomaly detection system
- Automatic defensive mode on attack patterns
- Whitelist bonus for trusted masternodes

### Documentation Structure
```
timecoin/
├── README.md                    # Project overview
├── CONTRIBUTING.md              # Contribution guidelines
├── LICENSE                      # Business Source License 1.1
├── CHANGELOG.md                 # This file (NEW)
├── docs/                        # Public documentation (19 files)
│   ├── TIMECOIN_PROTOCOL.md    # Canonical protocol spec (V6)
│   ├── AI_SYSTEM.md            # AI features documentation (NEW)
│   ├── QUICKSTART.md           # Getting started
│   ├── LINUX_INSTALLATION.md   # Installation guide
│   └── ...                     # More user/dev docs
└── analysis/                    # Internal documentation (428 files)
    ├── AI_IMPLEMENTATION_SUMMARY.md
    ├── FORK_RESOLUTION_IMPROVEMENTS.md
    └── ...                     # Development notes
```

### Migration Notes

#### For Node Operators
- No configuration changes required
- AI features enabled by default
- Version automatically updates on restart
- All existing data remains compatible

#### For Developers
- Update version checks to accept 1.0.0
- No API breaking changes
- New AI system APIs available (see docs/AI_SYSTEM.md)

#### Configuration
```toml
[node]
version = "1.0.0"  # Updated from 0.1.0

[ai]
enabled = true                 # Default: true
peer_selection = true         # Default: true
fork_resolution = true        # Default: true
anomaly_detection = true      # Default: true
```

### Known Issues

**P2P Encryption:**
- TLS infrastructure is implemented but not yet integrated into peer connections
- Current P2P communication uses plain TCP (unencrypted)
- For production deployments, use VPN, SSH tunnels, or trusted networks
- TLS integration planned for v1.1.0
- Message-level signing provides authentication without encryption

### Contributors
- Core Team
- Community Contributors

### References
- [TIMECOIN_PROTOCOL.md](docs/TIMECOIN_PROTOCOL.md) - Protocol specification
- [AI_SYSTEM.md](docs/AI_SYSTEM.md) - AI features documentation
- [GitHub Repository](https://github.com/time-coin/time-masternode)

---

## [0.1.0] - 2025-12-23

### Initial Development Release
- TimeVote consensus implementation (stake-weighted voting)
- TimeLock block production (deterministic 10-minute blocks)
- TimeProof (verifiable finality proofs)
- Masternode system with 4 tiers (Free/Bronze/Silver/Gold)
- UTXO state machine
- P2P networking
- RPC API
- Basic peer selection

---

[1.0.0]: https://github.com/time-coin/time-masternode/releases/tag/v1.0.0
[0.1.0]: https://github.com/time-coin/time-masternode/releases/tag/v0.1.0
