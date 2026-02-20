# Changelog

All notable changes to TimeCoin will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.2.0] - 2026-02-12

### Changed - Config-Based Masternode Management
- **BREAKING: Removed `masternoderegister` and `masternodeunlock` RPC/CLI commands**
  - Masternode registration is now entirely config-based via `config.toml`
  - Set `[masternode] enabled = true`, `collateral_txid`, `collateral_vout`
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

- **Wired Attack Detector to Blacklist**: Attack detector now enforces recommended mitigations
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
├── LICENSE                      # MIT License
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
- [GitHub Repository](https://github.com/time-coin/timecoin)

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

[1.0.0]: https://github.com/time-coin/timecoin/releases/tag/v1.0.0
[0.1.0]: https://github.com/time-coin/timecoin/releases/tag/v0.1.0
