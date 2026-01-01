# Fork Resolution System

## Overview

Timecoin uses an AI-powered fork resolution system to make intelligent decisions about competing blockchain forks. All fork resolution decisions are gated through the AI Fork Resolver, which provides confidence-scored recommendations with detailed reasoning.

**Last Updated:** 2026-01-01  
**Version:** 6.0 (AI-Integrated)

---

## Key Components

### 1. AI Fork Resolver (`src/ai/fork_resolver.rs`)

The central decision-making engine for all fork scenarios.

**Primary Decision Rule:** **Longest Valid Chain Wins**

```rust
pub async fn resolve_fork(&self, params: ForkResolutionParams) -> ForkResolution
```

#### Input Parameters

```rust
pub struct ForkResolutionParams {
    pub our_height: u64,              // Current local chain height
    pub our_chain_work: u128,         // Cumulative proof-of-work
    pub peer_height: u64,             // Peer's claimed height
    pub peer_chain_work: u128,        // Peer's cumulative work
    pub peer_ip: String,              // Peer identifier
    pub supporting_peers: Vec<...>,   // Network consensus info
    pub common_ancestor: u64,         // Fork point
    pub peer_tip_timestamp: Option<i64>, // For future-block validation
}
```

#### Output Decision

```rust
pub struct ForkResolution {
    pub accept_peer_chain: bool,    // true = reorganize, false = keep ours
    pub confidence: f64,            // 0.0 to 1.0
    pub reasoning: Vec<String>,     // Human-readable explanations
    pub risk_level: RiskLevel,      // Low/Medium/High/Critical
}
```

#### Decision Logic

**Step 1: Future Block Validation**
```
IF peer_tip_timestamp > current_time + tolerance:
  → REJECT with confidence 1.0, risk HIGH
  → Reason: "Peer's tip block is in the future"
```

**Step 2: Height Comparison**
```
IF peer_height > our_height:
  → ACCEPT peer chain
  → Confidence: 0.6 + min(height_diff * 0.1, 0.4)
  → Range: 0.6 to 1.0

ELSE IF peer_height == our_height:
  → REJECT (tie - keep ours)
  → Confidence: 0.5

ELSE:
  → REJECT (our chain longer)
  → Confidence: 0.6 + min(height_diff * 0.1, 0.4)
```

**Step 3: Risk Assessment**
```
Height Difference > 100 blocks  → HIGH risk
Height Difference > 10 blocks   → MEDIUM risk
Height Difference ≤ 10 blocks   → LOW risk
```

#### Learning & Tracking

**Fork History Database** (`ai_fork_history`)
- Records every fork decision
- Tracks outcomes: CorrectChoice | WrongChoice | NetworkSplit
- Stores up to 1,000 most recent fork events
- Persisted across node restarts

**Peer Reliability Database** (`ai_peer_fork_reliability`)
```rust
struct PeerForkReliability {
    correct_forks: u32,           // Times peer was on correct chain
    incorrect_forks: u32,         // Times peer was on wrong chain
    network_splits_caused: u32,   // Times peer caused network split
    last_updated: u64,
}
```

---

## Fork Resolution Flow

### Peer Connection Flow (`src/network/peer_connection.rs`)

```
┌─────────────────────────────────────────────────────────────┐
│ 1. RECEIVE BLOCKS                                           │
│    NetworkMessage::Blocks arrives from peer                 │
│    Extract: start_height, end_height, block_count          │
└────────────────┬────────────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. EARLY FORK DETECTION (Lines 717-770)                    │
│                                                              │
│    IF first block hash ≠ our block hash:                    │
│      🔀 FORK DETECTED!                                      │
│                                                              │
│      IF peer has longer chain:                              │
│        ┌─────────────────────────────────────┐             │
│        │ 🤖 AI DECISION POINT #1              │             │
│        │ should_investigate_fork()            │             │
│        │                                      │             │
│        │ Returns: (bool, String)             │             │
│        │ - true: Request more blocks         │             │
│        │ - false: Skip this fork             │             │
│        └─────────────────────────────────────┘             │
└────────────────┬────────────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. FIND COMMON ANCESTOR (Lines 773-813)                    │
│                                                              │
│    Loop through received blocks:                            │
│      - Compare each block hash with ours                    │
│      - Track last matching block = common_ancestor          │
│      - Identify first mismatch height                       │
│                                                              │
│    Result:                                                   │
│      common_ancestor: Option<u64>                           │
└────────────────┬────────────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. PREPARE REORGANIZATION (Lines 814-840)                  │
│                                                              │
│    IF common_ancestor found AND peer chain longer:          │
│      - Collect blocks after ancestor                        │
│      - Sort by height                                       │
│      - Verify no gaps in sequence                           │
│      - Ensure starts at ancestor + 1                        │
└────────────────┬────────────────────────────────────────────┘
                 │
                 ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. FINAL AI DECISION (Lines 841-880)                       │
│                                                              │
│    ┌─────────────────────────────────────┐                 │
│    │ 🤖 AI DECISION POINT #2              │                 │
│    │ should_accept_fork()                 │                 │
│    │                                      │                 │
│    │ Full block data available            │                 │
│    │ Returns: Result<bool, String>       │                 │
│    └─────────────────────────────────────┘                 │
│                                                              │
│    IF ACCEPT:                                               │
│      → blockchain.reorganize_to_chain(ancestor, blocks)     │
│      → Update UTXO set                                      │
│      → Update chain state                                   │
│      → ✅ Success!                                          │
│                                                              │
│    IF REJECT:                                               │
│      → Keep our chain                                       │
│      → Log decision & reasoning                             │
│      → ❌ Fork rejected                                     │
└─────────────────────────────────────────────────────────────┘
```

### No Common Ancestor Flow

```
┌─────────────────────────────────────────────────────────────┐
│ DEEP FORK SCENARIO (Lines 889-928)                         │
│                                                              │
│ No common ancestor in received blocks                       │
│                                                              │
│ Action: Search backwards for common ancestor                │
│   1. Calculate search_start = current_start - 100          │
│   2. Request GetBlocks(search_start, peer_height + 1)      │
│   3. Repeat until ancestor found                            │
│                                                              │
│ IF search_start reaches 0 (genesis):                        │
│   🚨 CRITICAL ERROR: Genesis blocks don't match            │
│   → Different network detected                              │
│   → Disconnect peer immediately                             │
└─────────────────────────────────────────────────────────────┘
```

### Whitelisted Peer Recovery Flow

```
┌─────────────────────────────────────────────────────────────┐
│ FORK DURING SEQUENTIAL BLOCK APPLICATION (Lines 996-1045)  │
│                                                              │
│ IF peer is whitelisted AND fork detected:                   │
│   ┌─────────────────────────────────────┐                  │
│   │ 🤖 AI DECISION POINT #3              │                  │
│   │ should_investigate_fork()            │                  │
│   │                                      │                  │
│   │ Prevents endless fork loops          │                  │
│   │ Returns: (bool, String)             │                  │
│   └─────────────────────────────────────┘                  │
│                                                              │
│   IF ACCEPT:                                                │
│     → Request blocks around fork point                      │
│     → Clear loop tracking (fresh start)                     │
│                                                              │
│   IF REJECT:                                                │
│     → Skip fork investigation                               │
│     → Clear tracking (intentional skip)                     │
│                                                              │
│ ELSE (non-whitelisted):                                     │
│   Track fork loop count                                     │
│   IF excessive loops (>5):                                  │
│     → Disconnect peer                                       │
│     → Prevent resource exhaustion                           │
└─────────────────────────────────────────────────────────────┘
```

---

## Server Fork Resolution (`src/network/server.rs`)

### Scenario 1: Previous Hash Mismatch

```
┌─────────────────────────────────────────────────────────────┐
│ BLOCK DOESN'T LINK TO OUR CHAIN (Lines 1069-1130)          │
│                                                              │
│ Incoming block's previous_hash ≠ our chain tip hash         │
│                                                              │
│ IF peer has longer chain:                                   │
│   🤖 should_accept_fork(blocks, end_height, peer.addr)      │
│                                                              │
│   IF ACCEPT:                                                │
│     → Use previous_hash point as common_ancestor            │
│     → Request missing blocks if needed                      │
│     → reorganize_to_chain(ancestor, reorg_blocks)           │
│                                                              │
│   IF REJECT:                                                │
│     → Keep our chain                                        │
│     → Continue with next peer                               │
└─────────────────────────────────────────────────────────────┘
```

### Scenario 2: Iterative Hash Comparison

```
┌─────────────────────────────────────────────────────────────┐
│ BLOCK-BY-BLOCK FORK DETECTION (Lines 1137-1210)            │
│                                                              │
│ Loop through blocks, comparing with our chain:              │
│   - Find where hashes diverge                               │
│   - Identify common ancestor                                │
│   - Collect blocks after fork point                         │
│                                                              │
│ IF peer has longer chain:                                   │
│   🤖 should_accept_fork(reorg_blocks, end_height, peer)     │
│                                                              │
│   IF ACCEPT:                                                │
│     → reorganize_to_chain(ancestor, reorg_blocks)           │
│     → ✅ Chain reorganization successful                    │
│                                                              │
│   IF REJECT:                                                │
│     → ❌ Keep our chain                                     │
│     → Continue processing                                   │
└─────────────────────────────────────────────────────────────┘
```

---

## Catchup Block Prevention (`src/main.rs`)

**Main.rs does NOT perform fork resolution directly!**

It focuses on preventing premature catchup block generation when longer valid chains exist.

```
┌─────────────────────────────────────────────────────────────┐
│ CATCHUP DECISION LOGIC (Lines 1024-1120)                   │
│                                                              │
│ When node is behind schedule:                               │
│                                                              │
│ 1. Try blockchain.sync_from_peers()                         │
│    → Delegates to peer_connection.rs                        │
│    → Uses AI fork resolution internally                     │
│                                                              │
│ 2. IF still behind after sync:                              │
│    Query ALL peers for chain heights                        │
│    Send: GetBlocks(our_height+1, expected+100)              │
│                                                              │
│ 3. Wait 15 seconds for peer responses                       │
│    Continuously check if height increases                   │
│                                                              │
│ 4. IF ANY blocks received:                                  │
│    → Loop back to step 1 (retry sync)                       │
│    → DO NOT produce catchup blocks                          │
│                                                              │
│ 5. IF NO blocks received after 15s:                         │
│    → All peers confirmed at similar/lower height            │
│    → Safe to produce catchup blocks via TSDC consensus      │
│                                                              │
│ This prevents fork creation when:                           │
│   - Valid longer chains exist but sync incomplete           │
│   - Network propagation delays                              │
│   - Peers are responding slowly                             │
└─────────────────────────────────────────────────────────────┘
```

---

## AI Decision Points Summary

| Location | Function | Trigger | Input | Purpose |
|----------|----------|---------|-------|---------|
| **peer_connection.rs:734** | `should_investigate_fork()` | First block mismatch | fork_height, peer_height, peer_ip | Gate block requests for early forks |
| **peer_connection.rs:844** | `should_accept_fork()` | Common ancestor found | blocks[], peer_height, peer_ip | Final reorganization decision |
| **peer_connection.rs:1020** | `should_investigate_fork()` | Sequential fork (whitelisted) | fork_height, peer_height, peer_ip | Prevent endless fork loops |
| **server.rs:1084** | `should_accept_fork()` | Previous hash mismatch | blocks[], peer_height, peer_addr | Server-side fork handling |
| **server.rs:1182** | `should_accept_fork()` | Iterative fork detection | blocks[], peer_height, peer_addr | Server-side fork handling |

**All decisions include:**
- ✅ Confidence score (0.0 to 1.0)
- 📝 Detailed reasoning
- ⚠️ Risk level assessment
- 📊 Recorded in fork history

---

## Key Safety Features

### 1. Future Block Rejection
- **Zero tolerance** for blocks timestamped in the future
- Immediate rejection with HIGH risk flag
- Prevents timestamp manipulation attacks

### 2. Whitelisted Peer Protection
- Special handling for trusted peers
- AI gates investigation attempts
- Prevents resource exhaustion from fork loops
- Balances trust with validation

### 3. Deep Fork Detection
- Searches backwards up to 100 blocks per iteration
- Continues until common ancestor found or genesis reached
- Genesis mismatch = different network → disconnect

### 4. Fork Loop Prevention
- Tracks repeated fork attempts per peer
- Non-whitelisted peers: disconnect after 5+ loops
- Whitelisted peers: AI decides whether to continue

### 5. Peer Reliability Tracking
- Historical success/failure rates per peer
- Influences future AI decisions
- Network split detection and tracking

---

## Configuration

### Constants (`src/ai/fork_resolver.rs`)

```rust
const TIMESTAMP_TOLERANCE_SECS: i64 = 0;    // No future blocks allowed
const MAX_FORK_HISTORY: usize = 1000;       // Keep last 1000 fork events
```

### Catchup Wait Times (`src/main.rs`)

```rust
const PEER_RESPONSE_WAIT: u64 = 15;         // Wait 15s for peer chains
const CATCHUP_DELAY_THRESHOLD: i64 = 300;   // 5-min grace before catchup
```

---

## Logging

### Fork Detection
```
🔀 Fork detected at height 2077: our 1b7dad6690 vs peer d1ab8cd481
```

### AI Decisions
```
🤖 AI Fork Resolver: investigating fork from 1.2.3.4 - AI recommends investigating (confidence: 85%)
🤖 AI Fork Resolver: skipping fork from 5.6.7.8 - AI recommends skipping (confidence: 92%)
```

### Fork Resolution
```
🔄 Fork resolution: ACCEPT peer chain, reorganizing from height 2046 with 50 blocks (2047-2096)
✅ [Inbound] Chain reorganization successful

❌ Fork resolution: REJECT peer chain, keeping our chain
```

### Consolidated Summaries
```
🔀 [Outbound] Fork detected during block sync at height 2077 from 1.2.3.4 (70 blocks affected: 2077-2146)
```

---

## Best Practices

### For Node Operators

1. **Monitor fork events** in logs - frequent forks may indicate:
   - Network issues
   - Time synchronization problems
   - Malicious peers

2. **Check AI confidence scores** - low confidence (<0.6) suggests:
   - Close chain competition
   - Potential network split
   - Need for manual review

3. **Review peer reliability** - repeatedly wrong peers may be:
   - Poorly configured
   - On different networks
   - Malicious actors

4. **Whitelist trusted peers** carefully:
   - Only peers you control or trust
   - Different geographic locations
   - Known good operators

### For Developers

1. **Always use AI fork resolver** for decisions
   - Don't bypass with manual logic
   - Trust the confidence scores
   - Log reasoning for debugging

2. **Record fork outcomes** when known
   - Update fork history with results
   - Improves future AI decisions
   - Helps detect patterns

3. **Test fork scenarios** thoroughly
   - Genesis mismatches
   - Deep forks (>100 blocks)
   - Timestamp manipulation
   - Network splits

4. **Monitor AI statistics**
   - Track correct vs wrong decisions
   - Identify problematic peers
   - Tune parameters if needed

---

## Troubleshooting

### Issue: Constant Fork Loops

**Symptoms:**
- Same fork height repeatedly
- No progress in chain sync
- High CPU usage

**Solution:**
1. Check if peer is whitelisted (may retry indefinitely)
2. Verify AI is rejecting fork (check logs for reasoning)
3. Consider de-whitelisting or disconnecting peer
4. Check for time sync issues on either node

### Issue: Wrong Chain Selection

**Symptoms:**
- Node on minority chain
- Different blocks than network
- Isolation from other peers

**Solution:**
1. Check `get_statistics()` for fork resolver accuracy
2. Verify peer list includes trusted nodes
3. Ensure time synchronization is correct
4. Check for genesis file mismatches
5. Manual intervention: restart with `--resync`

### Issue: Premature Catchup Blocks

**Symptoms:**
- Creating blocks while peers have longer chains
- Frequent reorganizations
- Competing with network

**Solution:**
1. Verify 15-second peer wait is completing
2. Check network connectivity to peers
3. Ensure peers are responding to GetBlocks
4. Review catchup logs for "No blocks received" messages
5. May need to increase wait time in slow networks

---

## Future Enhancements

### Planned Features

1. **Machine Learning Integration**
   - Train on fork history data
   - Pattern recognition for malicious forks
   - Adaptive confidence scoring

2. **Enhanced Peer Consensus**
   - Weight decisions by supporting peer count
   - Require supermajority for large reorganizations
   - Detect coordinated attacks

3. **Checkpoint System**
   - Reject forks before certain heights
   - Community-verified checkpoints
   - Prevent deep reorganization attacks

4. **Fork Metrics Dashboard**
   - Real-time fork visualization
   - Historical accuracy graphs
   - Peer reliability rankings

---

## Related Documentation

- [Network Architecture](NETWORK_ARCHITECTURE.md)
- [TSDC Consensus](TSDC_CATCHUP_CONSENSUS.md)
- [Fork Recovery Guide](FORK_RECOVERY_GUIDE.md)
- [Protocol V6 Specification](TIMECOIN_PROTOCOL_V6.md)

---

## Change Log

### 2026-01-01 - v6.0 AI Integration
- Integrated AI fork resolver at all decision points
- Added `should_investigate_fork()` for early fork evaluation
- Removed aggressive whitelisted peer resolution
- Enhanced catchup block prevention logic
- Reduced fork logging verbosity
- Added confidence scores and risk levels to all decisions

### Previous Versions
- See [FORK_RECOVERY_GUIDE.md](FORK_RECOVERY_GUIDE.md) for historical approaches
