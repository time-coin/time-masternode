# TimeCoin Complete System Flow

**Last Updated**: 2026-04-12
**Codebase Version**: Based on actual code analysis (v1.4.34, commit ec4b37e)

---

## 1. Node Startup Sequence (main.rs)

### 1.1 Initialization Order

1. **Parse CLI args** — config path, listen addr, masternode flag, verbose, demo, generate-config
2. **Print hostname banner** — node identity display
3. **Determine network type** — Mainnet or Testnet from config
4. **Setup logging** — tracing-subscriber with systemd detection, hostname prefix
5. **Load config** — `time.conf` (key=value) + `masternode.conf` (collateral entries); legacy TOML still loads but deprecated
6. **Load wallet** — AES-256-GCM encrypted Ed25519 key; `masternodeprivkey` from time.conf takes priority over wallet key for signing
7. **Build `masternode_info`** — if `masternode=1` in time.conf:
   - Resolve reward address from `reward_address` config or local wallet
   - Parse collateral txid/vout from masternode.conf
   - Tier: `tier=auto` defers to UTXO lookup; explicit `tier=silver` etc. skips lookup
   - External IP via curl to ipify.org (overrides config if mismatch)
8. **Open sled databases**:
   - `{data_dir}/db/` — main UTXO + block storage (SledUtxoStorage)
   - `{data_dir}/db/peers` — peer manager storage
   - `{data_dir}/db/registry` — masternode registry
   - `{data_dir}/db/txindex` — transaction index
   - `{data_dir}/db/ai_state` — AI subsystem state
9. **Initialize UTXOStateManager** — loads UTXO states from sled (3,000+ entries typical)
10. **Tier auto-detection** — if `tier=auto` with collateral: call `utxo_mgr.get_utxo(outpoint)` to read collateral amount; if UTXO absent (spent/archived), logs warning and falls back to Free tier placeholder (resolved in step 17 below)
11. **Release stale collateral locks** — compare saved local outpoint vs current config; release if changed
12. **Initialize PeerManager** — loads 100+ peers from disk, fetches from bootstrap API
13. **Initialize MasternodeRegistry** — loads ~100+ masternodes from disk registry sled; sets peer manager reference
14. **Initialize ConsensusEngine** — with masternode registry and UTXO manager
15. **Initialize AI System** — all 7 AI modules with shared sled DB
16. **Enable AI transaction validation** — on consensus engine
17. **Initialize Blockchain** — block storage, consensus, registry, UTXO, network type; builds transaction index; verifies chain integrity and continuity; runs UTXO repair scan
18. **Rebuild collateral locks** — restore in-memory collateral locks from registry; UTXOs in non-Unspent state are skipped (cleaned up by periodic `cleanup_invalid_collaterals` after 3 blocks)
19. **Recover tier from on-disk registry** *(v1.4.34+)* — if auto-detection fell back to Free, check disk registry for a matching higher-tier entry with the same collateral outpoint; recover Silver/Bronze/Gold if found
20. **Register local masternode** — `registry.register(mn)` with the resolved tier; set `RegistrationSource::OnChain(height)` for paid tiers
21. **Set consensus identity** — wire masternode signing key into consensus engine
22. **Initialize TimeSync** — NTP-based time synchronization
23. **Start PeerConnectionRegistry** — connection tracking (DashMap-based, lock-free)
24. **Start ConnectionManager** — manages outbound connection lifecycle state machine
25. **Start block production task** — event-driven + 1-second interval polling; catches up on sync
26. **Start status report task** — 60-second interval with AI reporting
27. **Start cleanup task** — 10-minute interval for memory management
28. **Start RPC server** — HTTP/HTTPS JSON-RPC on port 24001/24101
29. **Start NetworkServer** — inbound TCP listener; attack enforcement loop every 30s; collateral audit every 5 min
30. **Start NetworkClient** — outbound peer connections with adaptive reconnection
31. **Announce masternode after sync** — after chain is current, broadcast V4 announcement to all peers
32. **Wait for shutdown** — Ctrl+C signal
33. **Flush sled to disk** — critical: prevents block corruption on restart

### 1.2 Genesis Block Handling

- If no genesis exists: create with `Blockchain::create_genesis_block()`
- Genesis timestamp is network-type specific (Mainnet vs Testnet)
- Genesis block: height 0, `previous_hash = [0; 32]`
- Validated on startup: hash check, height 0 verification, genesis compatibility check with peers via `BlockHashResponse`

---

## 2. Block Production Flow

### 2.1 TimeLock Leader Selection

- **Block interval**: 600 seconds (10 minutes)
- **Leader selection**: VRF (ECVRF, RFC 9381)
  - Input: `BLAKE3("TIMECOIN_VRF_V2" || height_le_bytes || previous_hash)`
  - Each masternode evaluates VRF proof; single leader per slot — highest VRF output wins
  - Fallback leader rotation uses `TimeLock-leader-selection-v2` input on timeout
- VRF proof, output hash, and score stored in `BlockHeader.vrf_proof`, `.vrf_output`, `.vrf_score`

### 2.2 Block Production Loop (Event-Driven + Interval)

The main loop uses `tokio::select!` with 4 branches:
1. **Shutdown signal** — graceful exit
2. **Catchup trigger** — immediate wake for behind-chain scenarios
3. **`block_added_signal.notified()`** — event-driven wake when any block arrives (sync, consensus, or own production)
4. **`interval.tick()`** — 1-second fallback polling

### 2.3 Two-Phase Commit (TimeLock Block Voting)

**Phase 1: Propose**
1. Leader assembles block from transaction pool
2. Signs block with `producer_signature` (Ed25519 over all header fields)
3. Broadcasts `TimeLockBlockProposal { block }` to all peers
4. Validators verify: valid transactions, correct previous hash, valid merkle root, valid producer signature

**Phase 2a: Prepare Votes**
1. Validators send `TimeVotePrepare { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PREPARE"`
3. Threshold: >50% of active validator count (simple majority)

**Phase 2b: Precommit Votes**
1. After prepare threshold met: `TimeVotePrecommit { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PRECOMMIT"`
3. Threshold: >50% of active validator count; block finalized after precommit threshold

**Production gating**: Block is only added to chain when **2/3 (67%) weighted stake** agrees (aligned with TimeVote finality).

**Fallback**: If fewer than 3 validators present (early network/single node), block added directly without voting.

### 2.4 Block Structure

```
Block {
    header: BlockHeader {
        version: u32,
        height: u64,
        previous_hash: Hash256,
        merkle_root: Hash256,
        timestamp: i64,
        block_reward: u64,
        leader: String,              // IP of block producer
        attestation_root: Hash256,   // deprecated, zeroed in new blocks
        masternode_tiers: MasternodeTierCounts { free, bronze, silver, gold },
        vrf_proof: Vec<u8>,          // ECVRF proof (~64 bytes)
        vrf_output: Hash256,         // deterministic randomness
        vrf_score: u64,              // derived from output for chain comparison
        active_masternodes_bitmap: Vec<u8>,  // 1 bit per masternode (sorted by address)
        liveness_recovery: Option<bool>,     // §7.6: true if block resolved stalled txs
        producer_signature: Vec<u8>, // Ed25519 over all header fields
        total_fees: u64,             // sum of tx fees in this block (satoshis)
    },
    transactions: Vec<Transaction>,
    masternode_rewards: Vec<(String, u64)>,   // (address, amount_satoshis)
    consensus_participants_bitmap: Vec<u8>,   // who voted on this block
    liveness_recovery: Option<bool>,
}
```

**Note**: `difficulty` and `nonce` fields shown in older docs no longer exist — TimeCoin uses VRF sortition, not PoW.

### 2.5 Block Storage

- Key format: `block_{height}` (padded decimal)
- Height stored as: `chain_height` key (bincode-serialized u64)
- Tip tracked via: `tip_height` (little-endian u64)
- Each write calls `db.flush()` with readback verification
- Two-tier block cache: hot (~50 deserialized) + warm (~500 serialized) — 10–50x faster reads
- Compression: currently forced OFF (zstd disabled for debugging)

---

## 3. Transaction Flow

### 3.1 Transaction Structure

```
Transaction {
    version: u32,
    inputs: Vec<TxInput>,
    outputs: Vec<TxOutput>,
    lock_time: u32,
    timestamp: i64,
    special_data: Option<SpecialTransactionData>,  // None for regular txs
    encrypted_memo: Option<Vec<u8>>,               // ECDH-encrypted, excluded from txid hash
}

SpecialTransactionData:
  MasternodeReg        — register/update masternode (references collateral, doesn't spend it)
  MasternodePayoutUpdate — change reward address
  CollateralUnlock     — release collateral back to spendable balance
```

There is no `TransactionType` enum. Transaction type is inferred from `special_data` presence and block context (coinbase/masternode reward outputs are created by the block producer, not submitted as user transactions).

### 3.2 Transaction Processing

1. **Receive**: `TransactionBroadcast` message from peer
2. **Dedup**: Check `SeenTransactions` / `DedupFilter` (bloom-filter-like)
3. **AI Attack Detection**: record transaction for double-spend tracking
4. **Consensus Processing**: `ConsensusEngine::process_transaction()`
   - Validate against UTXO set
   - AI transaction validation (spam/dust detection)
   - Add to pending pool
5. **Gossip**: broadcast to other connected peers
6. **TimeVote Finality**: instant finality via TimeVote consensus (see §7)

### 3.3 Transaction Pool

- Max pool size: 100MB (configurable)
- Pressure levels: Normal (0–60%), Warning (60–80%), Critical (80–90%), Emergency (90%+)
- Priority scoring: fee rate, age, tx type
- Eviction: lowest priority first when pool is full
- Rejected tx cleanup: after 1 hour
- Mempool sync on connect: `MempoolSyncRequest` / `MempoolSyncResponse` exchanges pending + finalized txs with fees

### 3.4 UTXO Management

UTXOStateManager tracks all transaction outputs with a 5-state machine:

| State | Meaning |
|-------|---------|
| `Unspent` | Available for spending |
| `Locked { txid, locked_at }` | Reserved for a transaction (UTXO locked, tx not yet voted) |
| `SpentPending { txid, ... }` | TimeVote in progress; tracks vote counts |
| `SpentFinalized { txid, ... }` | 67% votes achieved (or 51% after 30s liveness fallback); irreversible |
| `Archived { txid, ... }` | Included in a block |

Finality occurs at `SpentPending → SpentFinalized`, not at block inclusion. Collateral UTXOs use a separate `LockedCollateral` mechanism that persists across restarts.

---

## 4. Network Protocol

### 4.1 P2P Transport

- TCP with bincode serialization + length-prefix framing (`wire.rs`)
- TLS auto-detect on same port: `0x16` first byte → TLS handshake; `AcceptAnyCertVerifier` (message-level Ed25519 auth)
- Default ports: Mainnet P2P 24000 (RPC 24001, WS 24002), Testnet 24100 (RPC 24101, WS 24102)
- Max peers: configurable (default ~50)
- Ping/pong heartbeat: 30-second interval, 90-second timeout (300s deadline in `peer_connection.rs`)
- Rate limiting in `server.rs`: per-message-type limits; `check_rate_limit!` macro with per-connection dedup (logs once per 60s, escalates to `record_severe_violation` after 10 drops)

### 4.2 Message Types (by category)

**Handshake / Identity**: `Handshake`, `Ack`, `Version`

**Health Check**: `Ping`, `Pong`

**Block Sync**: `GetBlockHeight`, `BlockHeightResponse`, `GetChainTip`, `ChainTipResponse`, `GetBlocks`, `BlocksResponse`, `GetBlockRange`, `BlockRangeResponse`, `GetBlockHash`, `BlockHashResponse`, `BlockRequest`, `BlockInventory`, `BlockResponse`, `BlockAnnouncement`

**Genesis**: `GetGenesisHash`, `GenesisHashResponse`, `RequestGenesis`, `GenesisAnnouncement`

**Transactions**: `TransactionBroadcast`, `TransactionFinalized`, `GetPendingTransactions`, `PendingTransactionsResponse`

**Mempool Sync**: `MempoolSyncRequest`, `MempoolSyncResponse`

**Peer Exchange**: `GetPeers`, `PeersResponse` (legacy), `PeerExchange` (load-aware, with tier and connection_count)

**Masternode**: `MasternodeAnnouncement` (V1, legacy), `MasternodeAnnouncementV2` (with collateral outpoint), `MasternodeAnnouncementV3` (with certificate + started_at), `MasternodeAnnouncementV4` (+ collateral ownership proof), `MasternodeInactive`, `MasternodeUnlock`, `GetMasternodes`, `MasternodesResponse`, `GetLockedCollaterals`, `LockedCollateralsResponse`, `ConnectivityWarning`

**UTXO**: `UTXOStateQuery`, `UTXOStateResponse`, `UTXOStateUpdate`, `UTXOStateNotification`, `GetUTXOSet`, `UTXOSetResponse`, `GetUTXOStateHash`, `UTXOStateHashResponse`

**Consensus Query**: `ConsensusQuery`, `ConsensusQueryResponse`, `GetChainWork`, `ChainWorkResponse`, `GetChainWorkAt`, `ChainWorkAtResponse`

**TimeLock Block Voting**: `TimeLockBlockProposal`, `TimeVotePrepare`, `TimeVotePrecommit`

**TimeVote Finality**: `TimeVoteRequest`, `TimeVoteResponse`, `TimeVoteBroadcast`, `TimeProofBroadcast`

**Liveness Fallback (§7.6)**: `LivenessAlert`, `FinalityProposal`, `FallbackVote`

**Gossip**: `MasternodeStatusGossip`

**Fork**: `ForkAlert`

**Governance**: `GovernanceProposal`, `GovernanceVote`, `GetGovernanceState`, `GovernanceStateResponse`

**Payment Relay**: `PaymentRequestRelay`, `PaymentRequestCancelled`, `PaymentRequestResponse`, `PaymentRequestViewed`

**Deprecated (kept for compat)**: `TransactionVoteRequest/Response`, `FinalityVoteRequest/Response/Broadcast`

**Catchall**: `UnknownMessage` (for future protocol versions)

### 4.3 Sync Flow

1. Node starts → checks height vs peers
2. If behind: `sync_from_peers(None)`
3. `sync_from_peers()`:
   - Gets connected peers from peer registry
   - Requests blocks from `current_height + 1` up to peer's height
   - Processes blocks sequentially; stops at first missing block (no gap tolerance)
4. **SyncCoordinator** prevents storms: rate-limits requests, tracks active syncs, prevents duplicate range requests

### 4.4 Fork Resolution

Hierarchical:
1. **Masternode authority tiers** (primary): Gold > Silver > Bronze > WhitelistedFree > Free
2. **Chain work** comparison (secondary)
3. **Chain height** comparison (tertiary)
4. **Deterministic hash tiebreaker** (final): lower block hash wins

Additional guarantees:
- Finalized transaction protection: forks reversing finalized txs are rejected
- Genesis compatibility check: `BlockHashResponse` at height 0 must match before syncing
- Fork consensus requires 2/3 weighted stake threshold

---

## 5. AI System Architecture

### 5.1 Overview

`AISystem` struct in `src/ai/mod.rs` aggregates 7 modules initialized from a shared sled DB.

### 5.2 AI Modules

| Module | Purpose | Data Source |
|--------|---------|-------------|
| **AnomalyDetector** | Z-score statistical anomaly detection | All network messages, block additions |
| **AttackDetector** | Detect sybil/eclipse/fork-bombing/timing attacks; auto-enforce bans | Invalid messages, transaction patterns, peer behavior |
| **AdaptiveReconnectionAI** | Learn optimal peer reconnection strategies | Connection successes/failures, session durations |
| **AIPeerSelector** | Score and rank peers by reliability/latency | Peer response times, sync success rates |
| **PredictiveSync** | Predict next block timing for prefetch | Block arrival times and intervals |
| **NetworkOptimizer** | Connection/bandwidth optimization | Peer metrics, network health |
| **AIMetricsCollector** | Aggregate dashboard of all AI metrics | All other AI modules |
| **ConsensusHealthMonitor** | Track peer agreement ratios, fork detection | Wired directly in `Blockchain` struct |
| **ForkResolver** | Longest-chain-wins fork resolution | Wired directly in `Blockchain` struct |
| **AITransactionValidator** | Spam/dust detection | Wired via `ConsensusEngine` |

**Removed (Feb 2026):** `TransactionAnalyzer` (results never queried), `ResourceManager` (methods never called).

### 5.3 Attack Enforcement (server.rs)

Enforcement runs on two loops:
- **Every 30 seconds**: checks `AttackDetector::get_recent_attacks(300s)` → applies `BlockPeer`, `RateLimitPeer`, or `AlertOperator`
- **Every 5 minutes**: collateral audit — scans registry for on-chain anchor mismatches and evicts squatters

`record_violation` actions:
- 3 violations → 1-minute temp ban (WARN logged)
- 5 violations → 5-minute temp ban (WARN logged)
- 10 violations → permanent ban (WARN logged)
- Counts 1, 2, 4, 6–9 are DEBUG only (no ban)

`record_severe_violation`: bypasses whitelist status; used for mass flooding (≥10 rate-limit drops per connection) and collateral squatting.

### 5.4 Periodic Tasks

| Interval | Task |
|----------|------|
| 30s | Attack enforcement; blacklist enforcement loop |
| 60s | Status report logs |
| 5 min | AI metrics collection + brief AI status |
| 5 min | Collateral audit (server.rs) |
| 60 min | Attack detector old-record cleanup |

---

## 6. Masternode System

### 6.1 Masternode Tiers

| Tier | Collateral | Sampling Weight | Notes |
|------|-----------|----------------|-------|
| Free | 0 TIME | 1 | No collateral required |
| Bronze | 1,000 TIME | 10 | |
| Silver | 10,000 TIME | 100 | |
| Gold | 100,000 TIME | 1,000 | |

### 6.2 Registration Flow (v1.2.0+)

Registration is **config-based**, not transaction-based:
1. Operator sets `masternode=1` in `time.conf` + collateral entry in `masternode.conf`
2. Daemon auto-registers on startup (`main.rs` calls `registry.register()`)
3. Daemon broadcasts `MasternodeAnnouncementV4` to peers at startup (after sync) and on new peer connections
4. **On-chain anchor**: operator submits a `MasternodeReg` special transaction signed with their collateral private key. This writes `collateral_anchor:{outpoint} → IP` to the registry sled — permanent ground truth that no gossip can override.
5. To deregister: set `enabled = false` in `masternode.conf`; daemon broadcasts `MasternodeUnlock`

There are no `masternoderegister` / `masternodeunlock` RPC or CLI commands. The old `MasternodeLock` transaction type no longer drives registration.

### 6.3 Collateral Security (Anti-Squatter)

- **Anchor check** (highest priority): `collateral_anchor:{outpoint}` sled key written by on-chain `MasternodeReg` tx. Any gossip claim from a different IP is permanently rejected and the peer banned.
- **V4 proof**: Ed25519 signature over `"TIME Masternode:" + ip + ":" + txid_hex + ":" + vout` using the collateral UTXO's output address key. Proves ownership without an on-chain tx.
- **Relay-path fix** (v1.4.34): `handle_masternode_announcement` uses `announced_address` (not TCP source IP) as masternode identity. Innocent relay nodes are not banned for relaying squatter messages; only the squatter IP is targeted.
- **3 confirmations** required before collateral activates.

### 6.4 Masternode Registry

- Stored in sled (`{data_dir}/db/registry`)
- Tracks: address, reward address, tier, public key, collateral outpoint, registration source, uptime counters
- Active/inactive status via gossip liveness: active when ≥3 peers reported it within 5 min
- `cleanup_invalid_collaterals()` runs per-block: deregisters nodes with 3 consecutive failed collateral checks (~30 min grace period)
- Local masternode is never auto-deregistered (operator must disable explicitly)

### 6.5 Block Rewards

- Pool-based distribution: separate pools per tier (Gold/Silver/Bronze/Free)
- Fairness rotation: `blocks_without_reward` counter tracks each node's wait time; winner is the longest-waiting active node in the tier
- `validate_pool_distribution` verifies both amounts AND winner identity (Step 3b, fixed April 2026)

---

## 7. Consensus Mechanisms

### 7.1 Hybrid Consensus

1. **TimeLock** — block production (who creates the next block)
   - VRF-based leader selection per 600s slot
   - Deterministic but unpredictable

2. **TimeVote** — transaction finality (is this transaction accepted?)
   - Stake-weighted voting: `SamplingWeight` (Free=1, Bronze=10, Silver=100, Gold=1000)
   - Threshold: **67% of total AVS weight** (`(total_weight * 67).div_ceil(100)`)
   - Liveness fallback: if stalled >30s in `SpentPending`, threshold drops to **51%**
   - TimeProof: finality certificate assembled from votes, stored with block

### 7.2 Finality

- **Instant finality** (<10 seconds typically, <1 second ideal)
- Once a transaction reaches 67% weighted stake, it transitions `SpentPending → SpentFinalized`
- Liveness fallback: after 30s stall, 51% threshold applies; `block.liveness_recovery = true`
- Finalized transactions are protected during fork resolution (no rollback)
- No probabilistic finality (unlike Bitcoin's 6-confirmation rule)

---

## 8. Storage Architecture

### 8.1 Sled Databases

| Database | Path | Purpose |
|----------|------|---------|
| main | `{data_dir}/db/` | UTXOs (SledUtxoStorage), collateral locks |
| blocks | `{data_dir}/db/blocks` | Block storage, chain height, tip |
| peers | `{data_dir}/db/peers` | Peer discovery data |
| registry | `{data_dir}/db/registry` | Masternode registry, collateral anchors |
| txindex | `{data_dir}/db/txindex` | Transaction index (6,000+ txs) |
| ai_state | `{data_dir}/db/ai_state` | AI module state (peer scores, anomaly history) |

### 8.2 Block Storage Configuration

- `flush_every_ms(None)` — manual flush only (after each block write)
- `Mode::LowSpace` — conservative writes to prevent corruption
- Explicit `db.flush()` on graceful shutdown (prevents "unexpected end of file" on restart)
- Readback verification after each write

### 8.3 Caching

- **Block Cache**: two-tier (hot + warm), configurable size
  - Hot: ~50 deserialized blocks (instant access)
  - Warm: ~500 serialized blocks (fast deserialization)
- **Consensus Cache**: 2/3 consensus check results, 30s TTL
- **sled write-behind channel**: heavy sled I/O runs outside the registry write lock (prevents tokio worker starvation, see AV32 fix)

---

## 9. RPC Interface

HTTP/HTTPS JSON-RPC 2.0 server (auto-detects TLS via `0x16` byte peek) on port 24001/24101. WebSocket notifications on 24002/24102.

### Key Endpoints

- `getblockcount` — current chain height
- `getblock` — block by height or hash
- `gettransaction` — transaction by ID
- `getbalance` — address balance
- `sendrawtransaction` — submit transaction
- `getpeerinfo` — connected peer information
- `getmininginfo` — block production status
- `getmasternodelist` — all registered masternodes
- `masternodestatus` — local masternode status with eligibility diagnosis
- `validateaddress` — address validation
- `collateralstatus` — collateral outpoint ownership and anchor check

---

## 10. Shutdown Sequence

1. **Ctrl+C signal** received
2. **CancellationToken** cancelled → signals all tasks
3. **Tasks drain** with 10-second timeout
4. **Sled flush** — critical: flushes block + UTXO storage to disk
5. **Process exit**

---

## 11. Key Constants

| Constant | Value | Description |
|----------|-------|-------------|
| Block time | 600s (10 min) | VRF slot duration |
| Finality threshold | 67% weighted stake | TimeVote finality (§8.3) |
| Liveness fallback | 51% weighted stake | After 30s stall |
| Liveness stall timeout | 30s | Time before fallback activates |
| Prepare/Precommit | >50% validator count | TimeLock block voting (not weighted) |
| Ping interval | 30s | Peer heartbeat |
| Pong timeout | 90s / 300s | `server.rs` / `peer_connection.rs` |
| Collateral miss threshold | 3 blocks | Consecutive misses before deregistration |
| Cleanup interval | 600s (10 min) | Memory cleanup cycle |
| Status interval | 60s | Status report cycle |
| AI report interval | 300s (5 min) | AI metrics collection |
| Shutdown timeout | 10s | Max task drain wait |
| Rate limit log interval | 60s | Per-connection dedup (server.rs) |
| Attack enforcement interval | 30s | server.rs blacklist sweep |
| Collateral audit interval | 5 min | server.rs anchor mismatch sweep |

---

## 12. File Structure

```
src/
├── main.rs                         # Entry point, initialization, task spawning
├── lib.rs                          # include!(main.rs) — exposes all modules to integration tests
├── types.rs                        # Core types: Transaction, UTXO, UTXOState, SamplingWeight, etc.
├── constants.rs                    # Protocol constants (with spec section references)
├── config.rs                       # Configuration loading (time.conf + masternode.conf)
├── address.rs                      # TIME address encoding/decoding (bech32-like)
├── network_type.rs                 # NetworkType enum (Mainnet/Testnet)
├── error.rs                        # Shared error types
├── shutdown.rs                     # Graceful shutdown coordination (CancellationToken)
├── state_notifier.rs               # Block-added notification channel
├── blockchain.rs                   # Core blockchain logic (~7,800 lines): block add, validation, rewards
├── blockchain_error.rs             # Blockchain-specific error types
├── blockchain_validation.rs        # Extracted block validation logic
├── consensus.rs                    # ConsensusEngine, TimeVote voting, UTXO locking
├── timevote.rs                     # TimeVoteHandler — TX-to-finality event pipeline
├── timelock.rs                     # VRF-based leader selection, 600s slot scheduling
├── finality_proof.rs               # TimeProof assembly, progressive vote accumulation
├── utxo_manager.rs                 # 5-state UTXO machine, collateral lock tracking
├── masternode_registry.rs          # 4-tier registry, stake weighting, reward calculation, anchor checks
├── masternode_authority.rs         # Authority-tier masternode management
├── masternode_certificate.rs       # Certificate verification (legacy, enforcement off)
├── transaction_pool.rs             # Mempool with priority eviction
├── tx_index.rs                     # Transaction index (O(1) lookups)
├── block_cache.rs                  # Two-tier block cache (hot + warm)
├── storage.rs                      # Storage backends: SledUtxoStorage, InMemoryUtxoStorage
├── wallet.rs                       # AES-256-GCM wallet, Ed25519 keys, X25519 ECDH memos
├── governance.rs                   # On-chain governance proposals and voting
├── peer_manager.rs                 # Peer discovery, scoring, persistence
├── time_sync.rs                    # NTP-based time synchronization
├── memo.rs                         # Encrypted memo helpers
├── http_client.rs                  # HTTP utilities (peer discovery, ipify.org)
├── ai/
│   ├── mod.rs                      # AISystem aggregator struct
│   ├── anomaly_detector.rs         # Z-score statistical anomaly detection
│   ├── attack_detector.rs          # Sybil/eclipse/fork-bombing detection + enforcement
│   ├── adaptive_reconnection.rs    # Smart peer reconnection delays
│   ├── consensus_health.rs         # Network consensus health monitoring
│   ├── fork_resolver.rs            # Longest-chain-wins fork resolution
│   ├── metrics_dashboard.rs        # AI metrics aggregation dashboard
│   ├── network_optimizer.rs        # Connection/bandwidth optimization
│   ├── peer_selector.rs            # AI-powered peer scoring
│   ├── predictive_sync.rs          # Block timing prediction
│   └── transaction_validator.rs    # AI spam/dust detection
├── bin/
│   ├── time-cli.rs                 # Bitcoin-compatible CLI tool (Bitcoin-style, mainnet port 24001)
│   └── time-dashboard.rs           # TUI dashboard (5 tabs, 2s auto-refresh)
├── block/
│   ├── types.rs                    # Block, BlockHeader, MasternodeTierCounts structs
│   ├── genesis.rs                  # Genesis block creation
│   ├── generator.rs                # Block assembly logic
│   ├── vrf.rs                      # ECVRF proof generation and verification
│   └── mod.rs
├── crypto/                         # Cryptographic primitives
├── network/
│   ├── message.rs                  # All P2P message types (enum NetworkMessage)
│   ├── message_handler.rs          # Message dispatch and handling (~5,000 lines)
│   ├── server.rs                   # Inbound connection handling + attack enforcement
│   ├── client.rs                   # Outbound connection logic (three-phase startup)
│   ├── peer_connection.rs          # Per-peer message loop, ping/pong liveness
│   ├── peer_connection_registry.rs # Lock-free registry (DashMap) of active peers
│   ├── connection_manager.rs       # Outbound state machine (Connecting/Connected/Disconnected)
│   ├── blacklist.rs                # IP ban list with violation tracking
│   ├── rate_limiter.rs             # Per-message-type rate limiting
│   ├── dedup_filter.rs             # Seen-transaction deduplication
│   ├── sync_coordinator.rs         # Sync storm prevention
│   ├── partition_detector.rs       # Network partition detection
│   ├── peer_discovery.rs           # Peer discovery helpers
│   ├── peer_scoring.rs             # Peer reliability scoring
│   ├── tls.rs                      # rustls with AcceptAnyCertVerifier
│   ├── wire.rs                     # Length-prefix framing
│   ├── secure_transport.rs         # Signed-message transport
│   ├── signed_message.rs           # Ed25519-signed P2P message wrapper
│   ├── block_cache.rs              # Network-layer block cache
│   └── mod.rs
└── rpc/                            # JSON-RPC 2.0 server, WebSocket notifications
```
