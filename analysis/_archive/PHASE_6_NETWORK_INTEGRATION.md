# Phase 6: Network Integration & Testnet Deployment

**Status**: Starting  
**Date**: December 23, 2025  
**Goal**: Full network integration with live multi-node testnet  
**Expected Duration**: 4-6 hours  

---

## Overview

Phase 6 focuses on:
1. **Network Message Integration** - Wire network handlers to consensus logic
2. **Vote Generation Triggers** - Emit votes when blocks arrive
3. **Finalization Callbacks** - Complete block finalization with signatures
4. **Local Testing** - 3-node network on single machine
5. **Testnet Deployment** - 5+ nodes on cloud infrastructure
6. **Monitoring & Validation** - Real-time chain metrics

---

## Phase 6.1: Network Message Handlers

### What This Does
Connects network message reception to consensus voting logic.

### Implementation Checklist

- [ ] **PrepareVote Handler** (`src/network/server.rs`)
  - Validate vote signature
  - Extract block_hash, voter_id, voter_weight
  - Accumulate in consensus
  - Check if 2/3+ threshold reached
  - Log "✅ Prepare consensus reached!"

- [ ] **PrecommitVote Handler** (`src/network/server.rs`)
  - Validate vote signature
  - Extract block_hash, voter_id, voter_weight
  - Accumulate in consensus
  - Check if 2/3+ threshold reached
  - Collect signatures
  - Call finalization complete
  - Emit finalization event

- [ ] **Block Proposal Handler** (`src/network/server.rs`)
  - Validate block signature
  - Store in block cache
  - Generate local prepare vote
  - Broadcast to peers

### Code Template

```rust
// In src/network/server.rs

async fn handle_prepare_vote(&mut self, vote: PrepareVote) -> Result<()> {
    // 1. Validate signature
    vote.validate_signature(&self.validator_db)?;
    
    // 2. Accumulate vote
    self.consensus.accumulate_prepare_vote(
        vote.block_hash,
        vote.voter_id.clone(),
        self.validator_db.get_weight(&vote.voter_id)?,
    );
    
    // 3. Check threshold
    if self.consensus.check_prepare_consensus(&vote.block_hash)? {
        tracing::info!("✅ Prepare consensus reached for block {:?}", vote.block_hash);
        
        // Generate precommit vote locally
        self.generate_precommit_vote(vote.block_hash).await?;
    }
    
    Ok(())
}

async fn handle_precommit_vote(&mut self, vote: PrecommitVote) -> Result<()> {
    // 1. Validate signature
    vote.validate_signature(&self.validator_db)?;
    
    // 2. Accumulate vote
    self.consensus.accumulate_precommit_vote(
        vote.block_hash,
        vote.voter_id.clone(),
        self.validator_db.get_weight(&vote.voter_id)?,
    );
    
    // 3. Check threshold
    if self.consensus.check_precommit_consensus(&vote.block_hash)? {
        tracing::info!("✅ Precommit consensus reached for block {:?}", vote.block_hash);
        
        // Get block and signatures
        let block = self.block_cache.get(&vote.block_hash)?;
        let signatures = self.consensus.get_precommit_signatures(&vote.block_hash)?;
        
        // Finalize block
        let reward = self.tsdc.finalize_block_complete(
            block,
            signatures,
        ).await?;
        
        tracing::info!("🎉 Block finalized! Reward: {} TIME", reward / 100_000_000);
        
        // Emit finalization event
        self.event_channel.send(BlockFinalized {
            block_hash: vote.block_hash,
            reward,
        })?;
    }
    
    Ok(())
}
```

### Testing
```bash
cargo check
cargo fmt
cargo test test_network_handlers
```

---

## Phase 6.2: Vote Generation Triggers

### What This Does
Automatically generate and broadcast votes when blocks arrive.

### Implementation Checklist

- [ ] **On Block Proposal**
  - Validate block
  - Store in cache
  - Generate prepare vote
  - Broadcast prepare vote
  - Start voting timer

- [ ] **On Prepare Consensus**
  - Generate precommit vote
  - Broadcast precommit vote

- [ ] **Voting Timer** (if no consensus in 30s)
  - Restart with different sample of validators

### Code Template

```rust
// In src/network/server.rs

async fn on_block_proposal(&mut self, block: Block) -> Result<()> {
    tracing::debug!("📦 Received block proposal at height {}", block.height);
    
    // 1. Validate block
    self.validate_block(&block)?;
    
    // 2. Store in cache
    self.block_cache.insert(block.hash(), block.clone());
    
    // 3. Generate prepare vote
    let vote = self.consensus.generate_prepare_vote(
        block.hash(),
        self.validator_id.clone(),
        self.validator_weight,
    );
    
    // 4. Broadcast
    self.broadcast_message(NetworkMessage::PrepareVote(vote)).await?;
    
    tracing::debug!("✅ Generated prepare vote for block {:?}", block.hash());
    
    Ok(())
}

async fn on_prepare_consensus_reached(&mut self, block_hash: Hash) -> Result<()> {
    tracing::info!("Prepare consensus reached, generating precommit vote");
    
    let vote = self.consensus.generate_precommit_vote(
        block_hash,
        self.validator_id.clone(),
        self.validator_weight,
    );
    
    self.broadcast_message(NetworkMessage::PrecommitVote(vote)).await?;
    
    Ok(())
}
```

### Testing
```bash
cargo check
cargo fmt
cargo test test_vote_generation
```

---

## Phase 6.3: Local 3-Node Testing

### What This Does
Deploy and test 3 validators on a single machine.

### Setup

```bash
# Terminal 1 - Node 1 (Leader)
RUST_LOG=debug cargo run -- \
  --validator-id validator1 \
  --port 8001 \
  --peers localhost:8002,localhost:8003 \
  --stake 100

# Terminal 2 - Node 2
RUST_LOG=debug cargo run -- \
  --validator-id validator2 \
  --port 8002 \
  --peers localhost:8001,localhost:8003 \
  --stake 100

# Terminal 3 - Node 3
RUST_LOG=debug cargo run -- \
  --validator-id validator3 \
  --port 8003 \
  --peers localhost:8001,localhost:8002 \
  --stake 100
```

### Verification Checklist

- [ ] All 3 nodes start without errors
- [ ] Nodes discover each other (check logs for "peer connected")
- [ ] Node 1 proposes first block (check for "proposing block" log)
- [ ] All 3 nodes vote prepare (check for "prepare vote" in logs)
- [ ] Prepare consensus reached (check for "✅ Prepare consensus reached")
- [ ] All 3 nodes vote precommit
- [ ] Precommit consensus reached
- [ ] Block finalized with 3 signatures
- [ ] Reward calculated and distributed (check for "🎉 Block finalized")
- [ ] Transactions archived
- [ ] Chain height increments to 2, 3, 4...

### Expected Logs

```
Node 1:
[INFO] Starting validator node: validator1
[INFO] Listening on 0.0.0.0:8001
[DEBUG] Peer connected: validator2
[DEBUG] Peer connected: validator3
[DEBUG] 📦 Proposing block at height 1
[DEBUG] ✅ Generated prepare vote for block 0xabc123
[DEBUG] Prepare vote from validator2 - weight: 100/300
[DEBUG] Prepare vote from validator3 - weight: 200/300
[INFO] ✅ Prepare consensus reached!
[DEBUG] ✅ Generated precommit vote for block 0xabc123
[DEBUG] Precommit vote from validator2 - weight: 100/300
[DEBUG] Precommit vote from validator3 - weight: 200/300
[INFO] ✅ Precommit consensus reached!
[DEBUG] ⛓️  Block finalized at height 1
[DEBUG] 💰 Block 1 rewards - subsidy: 560508300 nanoTIME
[INFO] 🎉 Block finalization complete: 5.60 TIME distributed
[DEBUG] Moving to height 2...

Node 2 & 3: (similar pattern)
```

### Troubleshooting

| Issue | Cause | Solution |
|-------|-------|----------|
| Nodes can't connect | Port already in use | Kill existing process: `pkill timed` |
| No blocks proposed | Leader election bug | Check VRF sorting in block proposal |
| Prepare consensus not reached | Vote accumulation bug | Check threshold calculation (2/3) |
| Precommit never reached | Prepare vote lost | Check network broadcast |
| Reward not calculated | TSDC issue | Verify finalization callback is called |

---

## Phase 6.4: Byzantine Fault Testing

### What This Does
Test network resilience when 1/3 of validators go offline.

### Setup

```bash
# Start 3-node network as above
# Let it run for 5 slots (~20 seconds)

# Then in Terminal 3 (Node 3):
# Press Ctrl+C to stop Node 3

# Continue monitoring Nodes 1 and 2
```

### Verification Checklist

- [ ] After Node 3 stops, Nodes 1-2 continue operating
- [ ] Consensus threshold adjusted to 2/3 (67%) of 200 weight
- [ ] Block proposal still occurs with Node 1 or 2 as leader
- [ ] Prepare votes from 2 validators reach consensus
- [ ] Precommit votes reach consensus
- [ ] Blocks finalize with 2 signatures
- [ ] Rewards still calculated correctly
- [ ] No chain fork

### Expected Behavior

```
Before: Need 2/3 of 300 weight = 200 weight
After:  Need 2/3 of 200 weight = 133 weight ✓

Node 1-2 combined: 200 weight > 133 required
→ Consensus continues ✅
```

---

## Phase 6.5: Testnet Deployment

### What This Does
Deploy 5-node network on cloud infrastructure.

### Prerequisites

- [ ] AWS or DigitalOcean account
- [ ] Release binary built: `cargo build --release`
- [ ] Binary size check: `ls -lh target/release/timed`

### Deployment Steps

#### Step 1: Create Cloud Instances

**DigitalOcean Example:**
```bash
# Create 5 droplets (Ubuntu 22.04, 2GB RAM)
doctl compute droplet create \
  timecoin-node-{1..5} \
  --region sfo3 \
  --size s-1vcpu-2gb \
  --image ubuntu-22-04-x64 \
  --format ID,Name,PublicIPv4 \
  --no-header
```

**Record IPs:**
```
Node 1: 123.45.67.1
Node 2: 123.45.67.2
Node 3: 123.45.67.3
Node 4: 123.45.67.4
Node 5: 123.45.67.5
```

#### Step 2: Deploy Binary

```bash
# For each node:
for IP in 123.45.67.{1..5}; do
  scp target/release/timed root@$IP:/usr/local/bin/
  ssh root@$IP "chmod +x /usr/local/bin/timed"
done
```

#### Step 3: Configure Nodes

**Node 1:**
```bash
ssh root@123.45.67.1
timed --validator-id validator1 \
  --port 8001 \
  --peers 123.45.67.2:8001,123.45.67.3:8001,123.45.67.4:8001,123.45.67.5:8001 \
  --stake 100
```

**Nodes 2-5:** (similar, adjust validator-id)

#### Step 4: Monitor Chain Growth

```bash
# Watch each node's height
watch -n 2 'for ip in 123.45.67.{1..5}; do 
  echo "Node $ip:"; 
  ssh -o StrictHostKeyChecking=no root@$ip "curl http://localhost:8001/status" | jq .height; 
done'
```

### Testnet Checklist

- [ ] All 5 nodes start
- [ ] Nodes discover each other (5x4 = 20 peer connections)
- [ ] Blocks propose every ~8 seconds
- [ ] All nodes track same height
- [ ] No chain forks
- [ ] Blocks finalize within 30 seconds
- [ ] Reward distribution working
- [ ] Chain running for >1 hour without issues
- [ ] Memory usage stable
- [ ] CPU usage <20% per node

### Metrics to Track

```
Block Time:         Target 8s, acceptable 5-15s
Finality Latency:   Target <30s, acceptable <60s
Consensus %:        Target 100%, acceptable 99%+
Memory (per node):  Target <200MB, acceptable <500MB
CPU (per node):     Target <5%, acceptable <20%
P2P Messages:       Track vote volume
Network Bandwidth:  Monitor if >10Mbps
```

---

## Phase 6.6: Monitoring & Observability

### Logging

Add to `Cargo.toml`:
```toml
[dependencies]
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
```

### Metrics to Log

```rust
// Block proposal
tracing::info!(
    "📦 Proposed block at height {}",
    block.height
);

// Prepare vote received
tracing::debug!(
    "vote.prepare height={} hash={:?} from={} weight={}",
    block.height,
    block.hash,
    voter_id,
    voter_weight
);

// Consensus reached
tracing::info!(
    "✅ Prepare consensus reached for block {:?}",
    block.hash
);

// Block finalized
tracing::info!(
    "🎉 Block finalized! height={} reward={} TIME",
    block.height,
    reward / 100_000_000
);
```

### Real-Time Dashboard (Optional)

Create simple HTTP endpoint:
```rust
// GET /status
{
    "height": 1234,
    "validators": 5,
    "voting_weight": 500,
    "consensus_threshold": 334,
    "blocks_finalized": 1200,
    "pending_blocks": 3,
    "uptime_seconds": 3600
}
```

---

## Phase 6.7: Acceptance Criteria

### Network Integration ✅
- [ ] Message handlers compile without errors
- [ ] Vote generation triggers work
- [ ] No panics on message reception
- [ ] All tests pass: `cargo test`

### Local Testing ✅
- [ ] 3 nodes start and connect
- [ ] Blocks propose correctly
- [ ] Voting works end-to-end
- [ ] Block finalization completes
- [ ] Rewards calculated
- [ ] Transactions archived

### Byzantine Scenario ✅
- [ ] 2 nodes consensus without 3rd
- [ ] Blocks continue to finalize
- [ ] No chain fork

### Testnet ✅
- [ ] 5 nodes running for >1 hour
- [ ] Block height increasing monotonically
- [ ] Finality consistent
- [ ] No reorgs
- [ ] Metrics healthy

---

## Timeline

```
Phase 6.1: Network Handlers     ~1 hour
Phase 6.2: Vote Triggers        ~30 minutes
Phase 6.3: Local 3-Node Test    ~45 minutes
Phase 6.4: Byzantine Test       ~15 minutes
Phase 6.5: Testnet Deploy       ~60 minutes
Phase 6.6: Monitoring           ~30 minutes
───────────────────────────────────────
Total:                           ~4.5 hours
```

---

## Success Criteria

### MVP Complete When:
1. ✅ All network handlers integrated
2. ✅ 3-node local network running
3. ✅ Blocks finalizing with correct signatures
4. ✅ Byzantine fault scenario passing
5. ✅ Testnet with 5+ nodes running for 1+ hour
6. ✅ All consensus metrics nominal
7. ✅ Zero chain forks

---

## Known Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| Network latency causing vote loss | Implement vote retransmission timer |
| Memory leak in long-running nodes | Add periodic cleanup of old blocks |
| Clock skew between nodes | Implement NTP sync checker |
| Validator disconnection | Implement peer reconnection logic |

---

## What's Next (Phase 7)

Once Phase 6 complete:
- **Phase 7A**: RPC API (wallet/explorer interaction)
- **Phase 7B**: Light client protocol
- **Phase 7C**: Block explorer
- **Phase 8**: Public testnet launch

---

**Ready to implement Phase 6.1 (Network Handlers)**

See: PHASE_6_IMPLEMENTATION.md for detailed code templates
