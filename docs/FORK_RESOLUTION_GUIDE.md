# Network Fork Resolution - Developer Quick Reference

## Overview

The TIME Coin network now has **proactive fork detection and intelligent resolution** to prevent network fragmentation.

---

## How Fork Detection Works

### 1. Periodic Chain Tip Polling (Every 60s)

```
Sync Coordinator → GetChainTip → All Connected Peers
                      ↓ (3s wait)
               ChainTipResponse (height + hash)
                      ↓
            Compare with Local Chain
                      ↓
                Fork Detected?
                ↙           ↘
              Yes            No
               ↓              ↓
          Fork Resolution   Continue
```

### 2. Fork Resolution Decision Tree

```
Fork Detected
    ↓
Consensus Height > Our Height?
    ↓ YES → Sync from consensus peer
    ↓ NO
Same Height, Different Hash?
    ↓ YES → AI Fork Resolver
              ↓
         Score Factors:
         • Height (40%)
         • Work (30%)
         • Timestamp (15%)
         • Peer Consensus (15%)
         • Whitelist Bonus (20%)
         • Peer Reliability (10%)
              ↓
         Accept or Reject
    ↓ NO
We're Ahead?
    ↓ YES → Keep our chain
    ↓ NO
Same Chain
    ↓
Continue
```

---

## Key Components

### Sync Coordinator

**Location:** `src/blockchain.rs:877`

**Runs:** Every 60 seconds

**Actions:**
1. Request chain tips from all peers
2. Check for fork via consensus
3. Sync from best masternode if behind
4. Trigger time-based sync if needed

**Configuration:**
```rust
const SYNC_COORDINATOR_INTERVAL_SECS: u64 = 60;
```

---

### Fork State Machine

**States:**

```rust
enum ForkResolutionState {
    None,                    // No fork detected
    FindingAncestor {        // Searching for common ancestor
        started_at: Instant, // Timeout: 2 minutes
        // ...
    },
    FetchingChain {          // Getting alternate chain
        started_at: Instant, // Timeout: 2 minutes
        // ...
    },
    ReadyToReorg {           // Ready to switch chains
        // ...
    },
    Reorging {               // Performing reorganization
        started_at: Instant, // Timeout: 1 minute
        // ...
    },
}
```

**Timeout Configuration:**
```rust
const FORK_RESOLUTION_TIMEOUT_SECS: u64 = 120; // 2 minutes
// Reorg timeout: 60 seconds (hardcoded in check)
```

---

### AI Fork Resolver

**Location:** `src/ai/fork_resolver.rs`

**Decision Factors:**

| Factor | Weight | Description |
|--------|--------|-------------|
| Height | 40% | Which chain is longer |
| Work | 30% | Which chain has more cumulative work |
| Timestamp | 15% | Block timestamp validity and recency |
| Peer Consensus | 15% | How many peers agree |
| Whitelist Bonus | 20% | Extra trust for masternodes |
| Peer Reliability | 10% | Historical accuracy |

**Risk Levels:**

```rust
enum RiskLevel {
    Low,      // < 5 blocks, trusted peer
    Medium,   // 5-20 blocks
    High,     // 20-100 blocks
    Critical, // > 100 blocks or timing issues
}
```

---

## Logging & Monitoring

### Important Log Messages

#### Fork Detection
```
🔀 Fork detected: consensus height 150 > our height 140 (5 peers agree)
```

#### AI Fork Resolution
```
🔀 Fork at same height 150: our hash 1a2b3c4d (2 peers) vs consensus hash 5e6f7g8h (5 peers)
   AI Resolution: ACCEPT consensus chain (confidence: 85%, risk: Low)
   • Height comparison: 0 blocks ahead (score: 0.00)
   • Peer consensus: 3 on peer chain, 2 on our chain, 0 other (score: 0.14)
   • Whitelisted peer bonus: +20.0%
```

#### Chain Tip Polling
```
🔍 Sync coordinator: Requesting chain tips from 7 peer(s)
```

#### Timeout Detection
```
⚠️  Fork resolution timed out after 125s, resetting state
```

#### Sync Initiation
```
🔀 Sync coordinator: Fork detected via consensus, syncing from 192.168.1.100
🎯 Sync coordinator: Found masternode 192.168.1.100 at height 150 (10 blocks ahead)
```

---

## API Usage

### Trigger Manual Fork Check

```rust
let blockchain: Arc<Blockchain> = /* ... */;

// Check for forks with peers
if let Some((fork_height, fork_peer)) = blockchain.compare_chain_with_peers().await {
    println!("Fork detected at height {} from peer {}", fork_height, fork_peer);
    
    // Sync will happen automatically via spawn task
}
```

### Get Fork Resolver Statistics

```rust
let blockchain: Arc<Blockchain> = /* ... */;
let resolver = &blockchain.fork_resolver;

let stats = resolver.get_statistics().await;
println!("Fork Resolution Stats:");
println!("  Total forks: {}", stats.total_forks);
println!("  Correct decisions: {}", stats.correct_decisions);
println!("  Wrong decisions: {}", stats.wrong_decisions);
println!("  Network splits: {}", stats.network_splits);
println!("  Avg peer success: {:.1}%", stats.avg_peer_success_rate * 100.0);
```

### Update Fork Outcome (Learning)

```rust
// After confirming a fork decision was correct/wrong
blockchain.fork_resolver
    .update_fork_outcome(fork_height, ForkOutcome::CorrectChoice)
    .await;

// Update peer reliability
blockchain.fork_resolver
    .update_peer_reliability(
        "192.168.1.100",
        true,  // was_correct
        false  // caused_split
    )
    .await;
```

---

## Configuration

### Adjust Sync Interval

```rust
// In blockchain.rs:877
const SYNC_COORDINATOR_INTERVAL_SECS: u64 = 60; // Change this value
```

**Recommendations:**
- **Fast networks:** 30-45 seconds
- **Normal networks:** 60 seconds (default)
- **Slow networks:** 90-120 seconds

### Adjust Fork Resolution Timeout

```rust
// In blockchain.rs:2945
const FORK_RESOLUTION_TIMEOUT_SECS: u64 = 120; // Change this value
```

**Recommendations:**
- **Fast networks:** 60-90 seconds
- **Normal networks:** 120 seconds (default)
- **Slow networks:** 180-240 seconds

### Adjust Chain Tip Wait Time

```rust
// In blockchain.rs:922
tokio::time::sleep(std::time::Duration::from_secs(3)).await;
//                                                     ^ Change this
```

**Recommendations:**
- **Local/Fast:** 1-2 seconds
- **Normal:** 3 seconds (default)
- **High latency:** 5-10 seconds

---

## Testing

### Multi-Node Fork Test

```bash
# Terminal 1
./target/release/timed --network testnet --config node1.toml

# Terminal 2
./target/release/timed --network testnet --config node2.toml

# Terminal 3
./target/release/timed --network testnet --config node3.toml

# Watch logs for fork detection and resolution
tail -f data_testnet/node1/logs/*.log | grep "Fork\|🔀\|AI Resolution"
```

### Timeout Test

```bash
# Start node
./target/release/timed --network testnet

# Watch for timeout in logs (wait 2+ minutes during fork resolution)
tail -f data_testnet/logs/*.log | grep "timeout\|timed out"
```

---

## Troubleshooting

### "Fork resolution timed out"

**Cause:** Peer didn't respond with blocks or disconnected

**Solution:**
- Timeout is intentional to prevent stalling
- Node will retry with different peer
- Check peer connectivity

### "No peer consensus data"

**Cause:** Not enough connected peers

**Solution:**
- Ensure at least 3 connected peers
- Check network configuration
- Verify bootstrap peers

### "We appear to be ahead of consensus"

**Cause:** Node produced blocks that others don't have

**Solution:**
- Usually resolves automatically
- Other nodes will sync to you
- If persists, check block validity

### "Sync coordinator sync failed"

**Cause:** Peer disconnected or sync error

**Solution:**
- Will retry next sync cycle (60s)
- Check peer reliability
- Monitor network stability

---

## Performance Tuning

### Reduce Network Traffic

Lower polling frequency:
```rust
const SYNC_COORDINATOR_INTERVAL_SECS: u64 = 120; // Every 2 minutes
```

### Increase Fork Resolution Speed

Shorten timeout:
```rust
const FORK_RESOLUTION_TIMEOUT_SECS: u64 = 60; // 1 minute
```

Reduce response wait:
```rust
tokio::time::sleep(std::time::Duration::from_secs(2)).await; // 2s instead of 3s
```

### Balance Memory vs Speed

More peer data tracking:
```rust
const MAX_FORK_HISTORY: usize = 2000; // In fork_resolver.rs
```

---

## Best Practices

### For Node Operators

1. **Maintain at least 5 connections** to well-synced peers
2. **Monitor logs** for frequent fork events (may indicate network issues)
3. **Update to latest version** for best fork resolution
4. **Whitelist trusted masternodes** for priority connections

### For Developers

1. **Don't disable sync coordinator** - critical for fork detection
2. **Use AI fork resolver statistics** to improve decisions
3. **Log fork events** to detect patterns
4. **Test fork scenarios** before mainnet deployment

---

## Related Files

- `src/blockchain.rs` - Sync coordinator, fork resolution state machine
- `src/ai/fork_resolver.rs` - AI fork resolver with scoring
- `src/network/peer_connection.rs` - ChainTipResponse handler
- `src/network/message.rs` - GetChainTip and ChainTipResponse messages
- `src/network/peer_connection_registry.rs` - Peer chain tip tracking

---

## References

- **Network Connectivity Analysis:** `analysis/NETWORK_CONNECTIVITY_ANALYSIS.md`
- **Fixes Applied:** `analysis/FIXES_APPLIED.md`
- **Protocol Specification:** `docs/TIMECOIN_PROTOCOL.md`
- **AI System Documentation:** `docs/AI_SYSTEM.md`
