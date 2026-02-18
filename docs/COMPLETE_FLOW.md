# TimeCoin Complete System Flow

**Last Updated**: 2026-02-16
**Codebase Version**: Based on actual code analysis, not design docs

---

## 1. Node Startup Sequence (main.rs)

### 1.1 Initialization Order

1. **Parse CLI args** - config path, listen addr, masternode flag, verbose, demo, generate-config
2. **Print hostname banner** - Node identity display
3. **Determine network type** - Mainnet or Testnet from config
4. **Setup logging** - tracing-subscriber with systemd detection, hostname prefix
5. **Open sled databases**:
   - `{data_dir}/db/peers` - Peer manager storage
   - `{data_dir}/db/registry` - Masternode registry
   - `{data_dir}/db/blocks` - Block storage (with `flush_every_ms(None)`, `Mode::LowSpace`)
   - `{data_dir}/db/txindex` - Transaction index
6. **Initialize UTXO storage** - InMemoryUtxoStorage (or SledUtxoStorage)
7. **Initialize UTXOStateManager** - Loads UTXO states from storage
8. **Initialize PeerManager** - Peer discovery and tracking
9. **Initialize MasternodeRegistry** - With peer manager reference
10. **Initialize ConsensusEngine** - With masternode registry and UTXO manager
11. **Initialize AI System** - All 7 AI modules in AISystem struct (+ 3 wired separately) with shared sled Db
12. **Enable AI Transaction Validation** - On consensus engine
13. **Initialize Blockchain** - With block storage, consensus, registry, UTXO, network type
14. **Set AI System on Blockchain** - For intelligent decision recording
15. **Configure block compression** - Currently forced OFF
16. **Initialize Transaction Index** - For O(1) tx lookups
17. **Verify chain height integrity** - Fix inconsistencies from crashes
18. **Validate genesis block** - Create if needed, verify if exists
19. **Initialize TimeSync** - NTP-based time synchronization
20. **Start PeerConnectionRegistry** - Connection tracking
21. **Start ConnectionManager** - Manages connection lifecycle
22. **Start PeerStateManager** - Peer state machine
23. **Wire AI System on NetworkServer** - For attack enforcement
24. **Spawn block production task** - Event-driven + 10-minute interval TimeLock consensus
25. **Spawn status report task** - 60-second interval with AI reporting
26. **Spawn cleanup task** - 10-minute interval for memory management
27. **Start RPC server** - HTTP JSON-RPC interface
28. **Start NetworkServer** - Inbound peer connections (with attack enforcement every 30s)
29. **Start NetworkClient** - Outbound peer connections with adaptive reconnection
30. **Wait for shutdown** - Ctrl+C signal
31. **Flush sled to disk** - Critical: prevents block corruption

### 1.2 Genesis Block Handling

- If no genesis exists: create one with `Blockchain::create_genesis_block()`
- Genesis timestamp is network-type specific (Mainnet vs Testnet)
- Genesis block has height 0, previous_hash = [0; 32]
- Genesis is validated on startup: hash check, height 0 verification

---

## 2. Block Production Flow

### 2.1 TimeLock Leader Selection

- **Block interval**: 600 seconds (10 minutes)
- **Leader selection**: VRF (Verifiable Random Function) using ECVRF
  - Input: `SHA256("TIMECOIN_VRF_V2" || height_le_bytes || previous_hash)`
  - Each masternode evaluates VRF proof
  - Single leader per slot: highest VRF output wins
  - Fallback leader rotation uses `TimeLock-leader-selection-v2` input on timeout

### 2.2 Block Production Loop (Event-Driven + Interval)

The main block production loop uses `tokio::select!` with 4 branches:
1. **Shutdown signal** — graceful exit
2. **Production trigger** — immediate wake when status check detects chain is behind
3. **`block_added_signal.notified()`** — event-driven wake when ANY block is added (from sync, consensus, or own production)
4. **`interval.tick()`** — periodic 1-second fallback polling

The event-driven wake (branch 3) reduces latency from ~1 second to near-instant when blocks arrive from peers.

### 2.2 Two-Phase Commit (2PC)

**Phase 1: Propose**
1. Leader assembles block from transaction pool
2. Broadcasts `TimeLockBlockProposal { block }` to all peers
3. Validators verify: valid transactions, correct previous hash, valid merkle root

**Phase 2a: Prepare Votes**
1. Validators send `TimeVotePrepare { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PREPARE"`
3. Votes accumulate by validator count (simple majority)
4. Threshold: >50% of participating validator count

**Phase 2b: Precommit Votes**
1. After prepare threshold met, send `TimeVotePrecommit { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PRECOMMIT"`
3. Threshold: >50% of participating validator count
4. Block is finalized after precommit threshold

**TimeProof Finality (separate from 2PC):**
- Transactions achieve instant finality via TimeProof with **51% weighted stake** threshold
- Weight is tier-based **sampling weight**: Free=1, Bronze=10, Silver=100, Gold=1000
- Note: Sampling weight is distinct from reward weight (Free=100, Bronze=1000, Silver=10000, Gold=100000) and governance voting power (Free=0, Bronze=1, Silver=10, Gold=100)
- This is distinct from block 2PC which uses validator count

**Liveness Fallback:**
- Stall detection timeout: 30 seconds without consensus progress
- Broadcasts `LivenessAlert`, enters `FallbackResolution` state
- Up to 5 fallback rounds with 10-second round timeout
- Deterministic hash-based leader selection per fallback round: `leader = MN with min SHA256(txid || slot_index || round || mn_address)`

**Fallback**: If no votes received (early network, single node), block is added directly if validator count < 3.

### 2.3 Block Structure

```
Block {
    header: BlockHeader {
        version: u32,
        height: u64,
        previous_hash: Hash256,
        merkle_root: Hash256,
        timestamp: i64,
        block_reward: u64,
        leader: String,
        attestation_root: Hash256,
        masternode_tiers: MasternodeTierCounts,
        vrf_proof: Vec<u8>,
        vrf_output: Hash256,
        vrf_score: u64,
        active_masternodes_bitmap: Vec<u8>,
        liveness_recovery: Option<bool>,
    },
    transactions: Vec<Transaction>,
    masternode_rewards: Vec<(String, u64)>,
    time_attestations: Vec<TimeAttestation>,
    consensus_participants_bitmap: Vec<u8>,
    liveness_recovery: Option<bool>,
}
```

### 2.4 Block Storage

- Key format: `block_{height}` (current format)
- Legacy `block:{height}` format and BlockV1 schema migration still supported in code but unused (old chain deleted)
- Height stored as: `chain_height` key with bincode-serialized u64
- Tip tracked via: `tip_height` with little-endian u64 bytes
- Each block write calls `db.flush()` with immediate readback verification
- Two-tier block cache: hot (deserialized) + warm (serialized) for 10-50x faster reads

---

## 3. Transaction Flow

### 3.1 Transaction Structure

```
Transaction {
    inputs: Vec<TxInput>,
    outputs: Vec<TxOutput>,
    lock_time: u64,
    tx_type: TransactionType,
}

TransactionType: Standard, CoinbaseReward, MasternodeReward,
                 MasternodeLock, MasternodeUnlock, GovernanceVote,
                 TimeProof, SmartContract
```

### 3.2 Transaction Processing

1. **Receive**: `TransactionBroadcast` message from peer
2. **Dedup**: Check SeenTransactions filter (bloom-filter-like)
3. **AI Attack Detection**: Record transaction for double-spend tracking
4. **Consensus Processing**: `ConsensusEngine::process_transaction()`
   - Validate against UTXO set
   - AI transaction validation (spam/dust detection)
   - Add to transaction pool
5. **Gossip**: Broadcast to other connected peers
6. **TimeVote Finality**: Instant finality via TimeVote consensus

### 3.2.1 Per-Transaction State Machine (TransactionStatus)

```
Seen → Voting → Finalized → Archived
         │          ↑
         │     (accumulated_weight ≥ Q_finality, TimeProof complete)
         │
         ├→ FallbackResolution → Finalized / Rejected
         │   (stall > 30s, deterministic leader resolves)
         │
         └→ Rejected
             (conflict lost or invalid)
```

- **Seen**: Transaction received, pending validation
- **Voting**: Actively collecting signed FinalityVotes, tracking `accumulated_weight` and `confidence`
- **FallbackResolution**: Stall detected, deterministic fallback round in progress (tracks round number and alert count)
- **Finalized**: `accumulated_weight ≥ 51%` of AVS weight, TimeProof assembled
- **Rejected**: Lost conflict resolution or deemed invalid
- **Archived**: Included in TimeLock checkpoint block

### 3.3 Transaction Pool

- Three-map structure:
  - **pending**: Transactions in consensus (Seen + Voting states) — `DashMap<Hash256, PoolEntry>`
  - **finalized**: Transactions ready for block inclusion — `DashMap<Hash256, PoolEntry>`
  - **rejected**: Previously rejected transactions with reason and timestamp — `DashMap<Hash256, (String, Instant)>`
- Max pool size: 100MB (configurable)
- Pressure levels: Normal (0-60%), Warning (60-80%), Critical (80-90%), Emergency (90%+)
- Priority scoring based on: fee rate, age, tx type
- Eviction: lowest priority first when pool is full
- Rejected tx cleanup: after 1 hour

### 3.4 UTXO Management

- UTXOStateManager tracks all unspent transaction outputs
- States: `Unspent`, `Locked` (masternode collateral), `SpentPending` (in-flight TX with vote tracking), `SpentFinalized` (TX finalized with votes), `Archived` (included in block)
- Collateral locking for masternodes with timeout cleanup (10 minutes)
- Double-spend prevention at UTXO level

---

## 4. Network Protocol

### 4.1 P2P Transport

- TCP with bincode serialization + length-prefix framing
- Connection types: Inbound (server accepts) and Outbound (client connects)
- Default ports: Mainnet 24000 (RPC 24001), Testnet 24100 (RPC 24101)
- Max peers: configurable (default ~50)
- Ping/pong heartbeat: 30-second interval, 90-second timeout (300s in peer_connection.rs)

### 4.2 Message Types (by category)

**Health Check**: Ping, Pong

**Block Sync**: GetBlocks, GetBlockHeight, GetChainTip, GetBlockRange, GetBlockHash,
BlockRequest, BlockInventory, BlockResponse, BlockAnnouncement

**Genesis**: GetGenesisHash, GenesisHashResponse, RequestGenesis, GenesisAnnouncement

**Transactions**: TransactionBroadcast

**Peer Exchange**: GetPeers, PeersResponse

**Masternode**: GetMasternodes, MasternodeAnnouncement, MasternodeInactive,
MasternodeUnlock, MasternodesResponse, GetLockedCollaterals, LockedCollateralsResponse

**UTXO**: UTXOStateQuery, UTXOStateUpdate, GetUTXOStateHash, GetUTXOSet

**Consensus Query**: ConsensusQuery, GetChainWork, GetChainWorkAt

**TimeLock Consensus**: TimeLockBlockProposal, TimeVotePrepare, TimeVotePrecommit,
FinalityVoteBroadcast

**Liveness Fallback**: LivenessAlert, FinalityProposal, FallbackVote

**Gossip**: MasternodeStatusGossip

**Fork**: ForkAlert

**Chain Sync Responses**: ChainTipResponse, BlocksResponse, BlockRangeResponse

### 4.3 Sync Flow

1. Node starts → checks current height vs expected height
2. If behind: calls `sync_from_peers(None)`
3. `sync_from_peers()`:
   - Gets connected peers from peer registry
   - Requests blocks from `current_height + 1` up to peer's height
   - Processes blocks sequentially
   - Stops at first missing block (no gap tolerance)
4. **Sync Coordinator** prevents storms:
   - Rate limits sync requests
   - Tracks active sync operations
   - Prevents duplicate sync to same height range

### 4.4 Fork Resolution

Stake-weighted longest-chain rule with tiered override:

**Chain comparison in `compare_chain_with_peers()` (blockchain.rs):**
1. **Height-first** (primary): longest chain wins
2. **Stake weight override** (within small gap): if both chains are within `MAX_STAKE_OVERRIDE_DEPTH` (2 blocks) of the tallest, stake weight becomes the primary criterion
3. **Stake tiebreaker** (same height): higher cumulative `sampling_weight()` wins
4. **Peer count** (same height + weight): more supporting peers wins
5. **Deterministic hash** (final): lexicographically lower block hash wins

**Stake override constants (`fork_resolver.rs`):**
- `MAX_STAKE_OVERRIDE_DEPTH = 2` — maximum height deficit stake can override
- `MIN_STAKE_OVERRIDE_RATIO = 2` — shorter chain needs ≥2× the taller chain's cumulative stake

**Masternode tier weights (`sampling_weight()`):**
- Free = 1, Bronze = 10, Silver = 100, Gold = 1000
- Cumulative stake = sum of all peers on that chain tip + our own weight

**`handle_fork()` decision flow (blockchain.rs):**
1. Find common ancestor via binary search
2. Security checks: reject genesis reorgs, reject depth > 500 blocks
3. Compute cumulative `our_stake_weight` and `peer_stake_weight`
4. Call `fork_resolver.resolve_fork()` which applies the three-tier logic:
   - Same height → stake tiebreaker, then hash
   - Gap ≤ 2 blocks → shorter chain wins if it has ≥2× stake
   - Gap > 2 blocks → longer chain always wins
5. Accept reorg to shorter chain only if `stake_override = true`

**Fork alert protocol (`message_handler.rs`):**
- When we're ahead: send `ForkAlert` to lagging peers (rate-limited to once per 60s per peer)
- When peer is ahead and in consensus: request blocks to sync
- On receiving `ForkAlert`: request blocks from consensus chain if behind or hash differs
- Validations: timestamp, merkle root, signatures, chain continuity
- Finalized transaction protection: reject forks that would reverse finalized txs

---

## 5. AI System Architecture

### 5.1 Overview

TimeCoin integrates a centralized AI system (`AISystem` struct in `src/ai/mod.rs`) that provides intelligent decision-making across all node subsystems. This is a core value proposition differentiating TimeCoin from other cryptocurrencies.

### 5.2 AI Modules (7 modules in AISystem + 3 wired separately)

| Module | Purpose | Data Source |
|--------|---------|-------------|
| **AnomalyDetector** | Z-score statistical anomaly detection on events | All network messages, block additions |
| **AttackDetector** | Detect sybil, eclipse, fork bombing, timing attacks; auto-enforce bans via blacklist | Invalid messages, transaction patterns, peer behavior |
| **AdaptiveReconnectionAI** | Learn optimal peer reconnection strategies | Connection successes/failures, session durations |
| **AIPeerSelector** | Score and rank peers by reliability/latency | Peer response times, sync success rates |
| **PredictiveSync** | Predict next block timing for prefetch | Block arrival times and intervals |
| **NetworkOptimizer** | Connection/bandwidth optimization, network health scoring | Peer metrics, network health |
| **AIMetricsCollector** | Aggregate dashboard of all AI subsystem metrics | All other AI modules |
| **ConsensusHealthMonitor** | Track peer agreement ratios, fork detection | Wired directly in Blockchain struct |
| **ForkResolver** | Multi-factor fork resolution with AI scoring | Wired directly in Blockchain struct |
| **AITransactionValidator** | Spam/dust detection on incoming transactions | Wired via ConsensusEngine |

**Removed modules (Feb 2026):** TransactionAnalyzer (results never queried), ResourceManager (methods never called)

### 5.3 Data Flow

```
                 ┌─────────────────┐
                 │   MessageHandler │
                 │  (all P2P msgs)  │
                 └────────┬────────┘
                          │ record events
                          ▼
                 ┌─────────────────┐
                 │    AISystem     │
                 │  (7 modules)    │
                 └────────┬────────┘
                          │
          ┌───────────────┼───────────────┐
          ▼               ▼               ▼
   AnomalyDetector  AttackDetector  PredictiveSync
   (Z-score stats)  (sybil/eclipse) (block timing)
          │               │               │
          └───────────────┼───────────────┘
                          ▼
                 ┌─────────────────┐
                 │ AIMetricsCollector│
                 │  (dashboard)     │
                 └────────┬────────┘
                          │ every 5 minutes
                          ▼
                 ┌─────────────────┐
                 │  Status Logger  │
                 │  (brief_status) │
                 └─────────────────┘
```

### 5.4 Event Recording Points

- **Every P2P message**: Message type recorded to anomaly detector for traffic pattern analysis
- **Message errors**: Failed message processing → attack detector + anomaly detector
- **Transactions received**: TX ID + peer IP → attack detector (double-spend tracking)
- **Blocks added (blockchain.rs)**: Height + timestamp → predictive sync
- **Block received (message handler)**: Block timing → predictive sync
- **Peer connections**: Success/failure/session duration → adaptive reconnection AI

### 5.5 Attack Enforcement (wired in server.rs)

The AttackDetector's recommendations are now automatically enforced:
- **Every 30 seconds**: server.rs checks `get_recent_attacks(300s)` for detected threats
- **BlockPeer**: Calls `blacklist.record_violation()` → auto-escalation (3→5min, 5→1hr, 10→permanent ban)
- **RateLimitPeer**: Also calls `record_violation()` (escalates on repeat offenses)
- **AlertOperator**: Logs critical alert for operator attention
- **Whitelisted peers**: Uses `record_severe_violation()` → overrides whitelist on 2nd offense
- Banned peers are disconnected via `mark_disconnected()` in the peer registry

### 5.6 Periodic Tasks

- **Every 60 seconds**: Status report logs node state
- **Every 30 seconds**: Attack enforcement (server.rs checks recent attacks, applies bans/rate-limits)
- **Every 5 minutes**: AI metrics collection + brief AI status log
- **Every 60 minutes**: Attack detector old record cleanup

---

## 6. Masternode System

### 6.1 Masternode Requirements

- Collateral locked via MasternodeLock transaction
- Three tiers based on collateral amount
- Ed25519 public key for vote signing
- Must be reachable by peers (inbound connections)

### 6.2 Masternode Registry

- Stored in sled database (`registry` db)
- Tracks: address, reward address, tier, public key, collateral outpoint
- Active/inactive status with heartbeat gossip
- Bitmap-based efficient status tracking

### 6.3 Block Rewards

- **50/50 Split**: Total reward = 100 TIME + transaction fees per block
  - **Block Producer**: 50 TIME + all transaction fees (VRF-selected proposer)
  - **Free-Tier Participation Pool**: 50 TIME distributed to active, mature Free-tier masternodes
    - Weighted by fairness_bonus (blocks_without_reward / 10, capped at 20)
    - Minimum payout: 1 TIME per node (max 50 recipients per block)
    - If no eligible Free nodes: full 50 TIME goes to producer
- CoinbaseReward transaction for block producer
- MasternodeReward transactions for Free-tier pool recipients
- Reward eligibility based on active status and uptime

---

## 7. Consensus Mechanisms

### 7.1 Hybrid Consensus

TimeCoin uses a hybrid consensus combining two mechanisms:

1. **TimeLock** - Block production (who creates the next block)
   - VRF-based leader selection per 10-minute slot
   - Deterministic but unpredictable leader election

2. **TimeVote** - Transaction and block finality (is this block accepted?)
   - Two-phase commit: Prepare → Precommit (validator count majority)
   - TimeProof finality: 51% weighted stake threshold for transaction finality
   - Sampling weight tiers: Free=1, Bronze=10, Silver=100, Gold=1000
   - Instant finality: once threshold met, transaction/block is final
   - No rollback of finalized blocks (critical security property)

### 7.2 Finality

- **Dual threshold system:**
  - **Block 2PC (Prepare/Precommit):** >50% of participating **validator count** (simple majority)
  - **TimeProof (transaction finality):** 51% of total **weighted stake**
- Instant finality (<10 seconds typically)
- Finalized transactions are protected during fork resolution
- No probabilistic finality (unlike Bitcoin's 6-confirmation rule)
- **Liveness fallback:** 30s stall timeout → LivenessAlert → up to 5 fallback rounds (10s each)

---

## 8. Storage Architecture

### 8.1 Sled Databases

| Database | Path | Purpose |
|----------|------|---------|
| blocks | `{data_dir}/db/blocks` | Block storage, chain height |
| peers | `{data_dir}/db/peers` | Peer discovery data |
| registry | `{data_dir}/db/registry` | Masternode registry |
| txindex | `{data_dir}/db/txindex` | Transaction index |

### 8.2 Block Storage Configuration

- `flush_every_ms(None)` - Manual flush only (after each block write)
- `Mode::LowSpace` - Conservative writes to prevent corruption
- Explicit `db.flush()` on graceful shutdown
- Readback verification after each write

### 8.3 Caching

- **Block Cache**: Two-tier (hot + warm), configurable size
  - Hot: ~50 deserialized blocks for instant access
  - Warm: ~500 serialized blocks for fast deserialization
- **Consensus Cache**: 2/3 consensus check results, 30s TTL
- **Compatible Peers Cache**: 10s TTL on `get_compatible_peers()` result, avoids repeated lock acquisition across 15+ call sites

### 8.4 Performance Optimizations

| Optimization | Location | Impact |
|-------------|----------|--------|
| Per-connection MessageHandler | `peer_connection.rs` | Handler created once per peer connection, not per-message. Preserves rate limiter and loop-detection state across messages. |
| Compatible peers cache | `peer_connection_registry.rs` | 10s TTL cache eliminates repeated write-lock + full-scan on `get_compatible_peers()` (called 15× across codebase). |
| Skip duplicate block size check | `blockchain.rs` `add_block()` | `validate_block()` already performs `bincode::serialize()` for size check; removed duplicate serialization in `add_block()`. |
| Parallel masternode reconnection | `main.rs` | Inactive masternode reconnection attempts spawned concurrently via `tokio::spawn` instead of sequential `add_peer()` loop. |
| Global fork alert rate limiter | `message_handler.rs` | `OnceLock<DashMap<String, Instant>>` persists across MessageHandler instances; prevents fork alert spam (60s per peer). |

---

## 9. RPC Interface

HTTP JSON-RPC server for node interaction.

### Key Endpoints

**Blockchain:**
- `getblockchaininfo` - Chain height, network, genesis hash
- `getblockcount` - Current chain height
- `getblock` - Block by height or hash
- `getbestblockhash` - Tip block hash
- `getblockhash` - Hash at height
- `gettxoutsetinfo` - UTXO set statistics

**Transactions:**
- `gettransaction` - Transaction by ID
- `gettransactions` - Batch fetch multiple transactions by ID array
- `getrawtransaction` - Raw transaction data
- `sendrawtransaction` - Submit transaction
- `createrawtransaction` - Build unsigned transaction
- `decoderawtransaction` - Decode raw transaction hex
- `gettransactionfinality` - Check finality status of a transaction
- `waittransactionfinality` - Wait for transaction to reach finality

**Wallet:**
- `getbalance` - Address balance
- `listunspent` - Unspent outputs
- `getnewaddress` - Generate new address
- `getwalletinfo` - Wallet metadata
- `sendtoaddress` - Send TIME to address
- `mergeutxos` - Consolidate UTXOs
- `listreceivedbyaddress` - Received amounts per address
- `listtransactions` - Transaction history

**Network:**
- `getnetworkinfo` - Network status
- `getpeerinfo` - Connected peer information

**Masternodes:**
- `masternodelist` - Active masternodes
- `masternodestatus` - This node's masternode status
- `listlockedcollaterals` - Locked collateral UTXOs

**Consensus:**
- `getconsensusinfo` - Consensus engine state
- `gettimevotestatus` - TimeVote protocol status

**Mempool:**
- `getmempoolinfo` - Transaction pool statistics
- `getrawmempool` - Raw mempool contents

**Admin:**
- `validateaddress` - Address validation
- `getinfo` - General node information
- `uptime` - Node uptime
- `stop` - Graceful shutdown
- `reindex` / `reindextransactions` - Rebuild indexes
- `gettxindexstatus` - Transaction index status
- `getwhitelist` / `addwhitelist` / `removewhitelist` - Peer whitelist management
- `getblacklist` - View banned peers
- `cleanuplockedutxos` / `listlockedutxos` / `unlockutxo` / `unlockorphanedutxos` / `forceunlockall` - UTXO lock management

---

## 10. Shutdown Sequence

1. **Ctrl+C signal** received
2. **CancellationToken** cancelled → signals all tasks
3. **Tasks drain** with 10-second timeout
4. **Sled flush** - Critical: flush block storage to disk
5. **Process exit**

Without the sled flush, dirty pages are lost, causing block corruption ("unexpected end of file") on restart.

---

## 11. Key Constants

| Constant | Value | Description |
|----------|-------|-------------|
| Block time | 600s (10 min) | Time between blocks |
| Max block size | Configurable | Block size limit |
| Block cache size | ~500 blocks | Two-tier cache capacity |
| Tx pool max | 100MB | Transaction pool memory limit |
| Prepare/Precommit threshold | >50% count | Validator count majority for block 2PC |
| TimeProof finality threshold | 51% weight | Weighted stake for transaction finality |
| Stall timeout | 30s | Time before liveness fallback triggers |
| Fallback rounds | 5 max | Maximum fallback resolution rounds |
| Max reorg depth | 500 blocks | Maximum fork rollback depth |
| Stake override depth | 2 blocks | Max height gap for stake-weighted override |
| Stake override ratio | 2× | Shorter chain needs ≥2× taller chain's stake |
| Ping interval | 30s | Peer heartbeat |
| Pong timeout | 90s (300s in peer_connection.rs) | Max time without pong |
| Fork alert rate limit | 60s | Max fork alert frequency per peer |
| Compatible peers cache TTL | 10s | Cache duration for peer list lookups |
| Cleanup interval | 600s (10 min) | Memory cleanup cycle |
| Status interval | 60s | Status report cycle |
| AI report interval | 300s (5 min) | AI metrics collection cycle |
| Shutdown timeout | 10s | Max wait for task drain |

---

## 12. File Structure

```
src/
├── main.rs                    # Entry point, initialization, task spawning
├── ai/
│   ├── mod.rs                 # AISystem aggregator struct
│   ├── anomaly_detector.rs    # Z-score statistical anomaly detection
│   ├── attack_detector.rs     # Sybil/eclipse/fork bombing detection + enforcement
│   ├── adaptive_reconnection.rs # Smart peer reconnection delays
│   ├── consensus_health.rs    # Network consensus health monitoring
│   ├── fork_resolver.rs       # Stake-weighted fork resolution (longest chain + stake override)
│   ├── metrics_dashboard.rs   # AI metrics aggregation dashboard
│   ├── network_optimizer.rs   # Connection/bandwidth optimization
│   ├── peer_selector.rs       # AI-powered peer scoring
│   ├── predictive_sync.rs     # Block timing prediction
│   └── transaction_validator.rs # AI spam/dust detection
├── block/                     # Block types and validation
├── blockchain.rs              # Core blockchain logic (~7800 lines)
├── consensus.rs               # ConsensusEngine, TimeVote, TimeLock
├── network/
│   ├── message.rs             # All P2P message types
│   ├── message_handler.rs     # Message dispatch and handling
│   ├── server.rs              # Inbound connection handling + attack enforcement
│   ├── client.rs              # Outbound connection management
│   ├── peer_connection.rs     # Per-peer connection lifecycle
│   ├── peer_connection_registry.rs # Global peer tracking
│   ├── sync_coordinator.rs    # Sync storm prevention
│   └── peer_scoring.rs        # Peer reliability scoring
├── rpc/                       # JSON-RPC server
├── config.rs                  # Configuration loading
├── storage.rs                 # Storage backends (UTXO, Block)
├── types.rs                   # Core types (Transaction, UTXO, etc.)
├── wallet.rs                  # Wallet management
├── timelock.rs                # TimeLock VRF leader selection
├── timevote.rs                # TimeVote consensus protocol
├── masternode_registry.rs     # Masternode tracking
├── utxo_manager.rs            # UTXO state management
├── transaction_pool.rs        # Mempool with priority eviction
├── shutdown.rs                # Graceful shutdown coordination
└── constants.rs               # System-wide constants
```
