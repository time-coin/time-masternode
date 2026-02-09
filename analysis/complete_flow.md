# TimeCoin Complete System Flow

**Last Updated**: 2026-02-09
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
11. **Initialize AI System** - All 9 AI modules with shared sled Db
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
23. **Spawn block production task** - 10-minute interval TimeLock consensus
24. **Spawn status report task** - 60-second interval with AI reporting
25. **Spawn cleanup task** - 10-minute interval for memory management
26. **Start RPC server** - HTTP JSON-RPC interface
27. **Start NetworkServer** - Inbound peer connections
28. **Start NetworkClient** - Outbound peer connections with adaptive reconnection
29. **Wait for shutdown** - Ctrl+C signal
30. **Flush sled to disk** - Critical: prevents block corruption

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
  - Fallback leader rotation uses `TSDC-leader-selection-v2` input on timeout

### 2.2 Two-Phase Commit (2PC)

**Phase 1: Propose**
1. Leader assembles block from transaction pool
2. Broadcasts `TimeLockBlockProposal { block }` to all peers
3. Validators verify: valid transactions, correct previous hash, valid merkle root

**Phase 2a: Prepare Votes**
1. Validators send `TimeVotePrepare { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PREPARE"`
3. Votes accumulate by validator count (not weighted)
4. Threshold: >50% of validator count (simple majority)

**Phase 2b: Precommit Votes**
1. After prepare threshold met, send `TimeVotePrecommit { block_hash, voter_id, signature }`
2. Ed25519 signature over `block_hash + voter_id + "PRECOMMIT"`
3. Threshold: >50% of validator count (simple majority)
4. Block is finalized after precommit threshold

**Fallback**: If no votes received (early network, single node), block is added directly if validator count < 3.

### 2.3 Block Structure

```
Block {
    header: BlockHeader {
        height: u64,
        timestamp: i64,
        previous_hash: [u8; 32],
        merkle_root: [u8; 32],
        difficulty: u64,
        nonce: u64,
        version: u32,
    },
    transactions: Vec<Transaction>,
}
```

### 2.4 Block Storage

- Key format: `block_{height}` (new) or `block:{height}` (legacy)
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

### 3.3 Transaction Pool

- Max pool size: 100MB (configurable)
- Pressure levels: Normal (0-60%), Warning (60-80%), Critical (80-90%), Emergency (90%+)
- Priority scoring based on: fee rate, age, tx type
- Eviction: lowest priority first when pool is full
- Rejected tx cleanup: after 1 hour

### 3.4 UTXO Management

- UTXOStateManager tracks all unspent transaction outputs
- States: `Unspent`, `Locked` (masternode collateral), `SpentPending` (in-flight TX with vote tracking), `SpentFinalized` (TX finalized with votes), `Confirmed` (included in block)
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

Hierarchical fork resolution with masternode authority:
1. **Masternode authority tiers** (primary): Gold > Silver > Bronze > WhitelistedFree > Free
2. **Chain work** comparison (secondary)
3. **Chain height** comparison (tertiary)
4. **Deterministic hash tiebreaker** (final): lower block hash wins
- Validations: timestamp, merkle root, signatures, chain continuity
- Finalized transaction protection: reject forks that would reverse finalized txs

---

## 5. AI System Architecture

### 5.1 Overview

TimeCoin integrates a centralized AI system (`AISystem` struct in `src/ai/mod.rs`) that provides intelligent decision-making across all node subsystems. This is a core value proposition differentiating TimeCoin from other cryptocurrencies.

### 5.2 AI Modules (12 modules)

| Module | Purpose | Data Source |
|--------|---------|-------------|
| **AnomalyDetector** | Z-score statistical anomaly detection on events | All network messages, block additions |
| **AttackDetector** | Detect sybil, eclipse, fork bombing, timing attacks | Invalid messages, transaction patterns, peer behavior |
| **AdaptiveReconnectionAI** | Learn optimal peer reconnection strategies | Connection successes/failures, session durations |
| **AIPeerSelector** | Score and rank peers by reliability/latency | Peer response times, sync success rates |
| **TransactionAnalyzer** | Predict load patterns, recommend fees | Transaction batches per block |
| **PredictiveSync** | Predict next block timing for prefetch | Block arrival times and intervals |
| **NetworkOptimizer** | Connection/bandwidth optimization suggestions | Peer metrics, network health |
| **ResourceManager** | CPU/memory/disk monitoring and allocation | System resource usage |
| **AIMetricsCollector** | Aggregate dashboard of all AI subsystem metrics | All other AI modules |
| **ConsensusHealthMonitor** | Track peer agreement ratios, fork detection | Wired directly in Blockchain struct |
| **ForkResolver** | Multi-factor fork resolution with AI scoring | Wired directly in Blockchain struct |
| **AITransactionValidator** | Spam/dust detection on incoming transactions | Wired via ConsensusEngine |

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
                 │  (9 modules)    │
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
- **Blocks added (blockchain.rs)**: Height + timestamp → predictive sync, tx count → transaction analyzer
- **Block received (message handler)**: Block timing → predictive sync
- **Peer connections**: Success/failure/session duration → adaptive reconnection AI

### 5.5 Periodic Tasks

- **Every 60 seconds**: Status report logs node state
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

- CoinbaseReward for block producer
- MasternodeReward distributed to active masternodes
- Reward eligibility based on active status and uptime

---

## 7. Consensus Mechanisms

### 7.1 Hybrid Consensus

TimeCoin uses a hybrid consensus combining two mechanisms:

1. **TSDC/TimeLock** - Block production (who creates the next block)
   - VRF-based leader selection per 10-minute slot
   - Deterministic but unpredictable leader election

2. **TimeVote** - Transaction and block finality (is this block accepted?)
   - Two-phase commit: Prepare → Precommit
   - Weight-based voting (masternode collateral = vote weight)
   - Instant finality: once precommit threshold met, block is final
   - No rollback of finalized blocks (critical security property)

### 7.2 Finality

- **Instant finality** (<10 seconds typically)
- Once a block receives 2/3 (67%) weighted stake agreement, it's finalized
- Prepare/Precommit phases use >50% validator count; production gating uses 2/3 weighted stake
- Finalized transactions are protected during fork resolution
- No probabilistic finality (unlike Bitcoin's 6-confirmation rule)

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

---

## 9. RPC Interface

HTTP JSON-RPC server for node interaction.

### Key Endpoints

- `getblockcount` - Current chain height
- `getblock` - Block by height or hash
- `gettransaction` - Transaction by ID
- `getbalance` - Address balance
- `sendrawtransaction` - Submit transaction
- `getpeerinfo` - Connected peer information
- `getmininginfo` - Block production status
- `getmasternodelist` - Active masternodes
- `validateaddress` - Address validation

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
| Finality threshold | 2/3 (67%) weight | Weighted stake agreement for block production |
| Ping interval | 30s | Peer heartbeat |
| Pong timeout | 90s (300s in peer_connection.rs) | Max time without pong |
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
│   ├── attack_detector.rs     # Sybil/eclipse/fork bombing detection
│   ├── adaptive_reconnection.rs # Smart peer reconnection delays
│   ├── consensus_health.rs    # Network consensus health monitoring
│   ├── fork_resolver.rs       # Longest-chain-wins fork resolution
│   ├── metrics_dashboard.rs   # AI metrics aggregation dashboard
│   ├── network_optimizer.rs   # Connection/bandwidth optimization
│   ├── peer_selector.rs       # AI-powered peer scoring
│   ├── predictive_sync.rs     # Block timing prediction
│   ├── resource_manager.rs    # System resource monitoring
│   ├── transaction_analyzer.rs # Load prediction, fee recommendations
│   └── transaction_validator.rs # AI spam/dust detection
├── block/                     # Block types and validation
├── blockchain.rs              # Core blockchain logic (~7800 lines)
├── consensus.rs               # ConsensusEngine, TimeVote, TimeLock
├── network/
│   ├── message.rs             # All P2P message types
│   ├── message_handler.rs     # Message dispatch and handling
│   ├── server.rs              # Inbound connection handling
│   ├── client.rs              # Outbound connection management
│   ├── peer_connection.rs     # Per-peer connection lifecycle
│   ├── peer_connection_registry.rs # Global peer tracking
│   ├── sync_coordinator.rs    # Sync storm prevention
│   ├── fork_resolver.rs       # Network-level fork state machine
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
