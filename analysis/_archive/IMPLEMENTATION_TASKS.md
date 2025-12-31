# Critical Implementation Tasks - Code Changes Required

## TASK 1: Fix Fork Resolution (CRITICAL - 4 hours)

### Current Code (BROKEN - Simulation Only)
**File**: `src/blockchain.rs:2185-2255`

```rust
// BROKEN: This doesn't actually query peers
async fn query_fork_consensus_multi_peer(
    &self,
    fork_height: u64,
    _peer_block_hash: Hash256,
    _our_block_hash: Option<Hash256>,
) -> Result<ForkConsensus, String> {
    // ...setup code...
    
    // FAKE: Just simulate responses
    responses = peers_to_query;
    peer_block_votes = (peers_to_query * 2 / 3) + 1;  // Simulated!
    
    // This returns a guess, not actual consensus
    Ok(ForkConsensus::PeerConsensus)
}
```

### What Needs to Happen

1. **Query Peers for Block Hash**
```rust
// Build query message
let query = NetworkMessage::QueryBlock {
    height: fork_height,
    timestamp: SystemTime::now(),
};

// Send to 7 random peers in parallel
let mut query_futures = Vec::new();
for peer in peers_to_query {
    let query = query.clone();
    query_futures.push(async move {
        // Send and wait for response (with timeout)
        peer.send(query).await
    });
}

// Collect responses with 5-second timeout
let responses = tokio::time::timeout(
    Duration::from_secs(5),
    futures::future::join_all(query_futures)
).await?;
```

2. **Count Votes for Each Hash**
```rust
let mut vote_counts: HashMap<Hash256, usize> = HashMap::new();

for response in responses {
    if let Some(hash) = response.block_hash {
        *vote_counts.entry(hash).or_insert(0) += 1;
    }
}

// Find winner with most votes
let (winner_hash, winner_count) = vote_counts
    .iter()
    .max_by_key(|(_, count)| *count)
    .ok_or("No peer responses")?;

tracing::info!(
    "🗳️  Fork consensus: {} peers voted for hash {:?}",
    winner_count,
    winner_hash
);
```

3. **Verify Byzantine Quorum (2/3 + 1)**
```rust
let quorum_size = (peers_to_query * 2 / 3) + 1;

if winner_count >= quorum_size {
    // Safe to trust this fork
    if *winner_hash == peer_block_hash {
        Ok(ForkConsensus::PeerConsensus)
    } else if Some(*winner_hash) == our_block_hash {
        Ok(ForkConsensus::OurConsensus)
    } else {
        Ok(ForkConsensus::UnknownConsensus)
    }
} else {
    // Not enough consensus - must wait
    Ok(ForkConsensus::InsufficientPeers)
}
```

### Test Required
```bash
# 1. Start 3 masternodes with network partition:
#    - Nodes A,B on one side
#    - Node C on other side
#    - Both create blocks at height 100

# 2. Heal partition and verify:
#    - Nodes query each other
#    - C's block gets 2 votes (A+B)
#    - Network converges to majority
#    - No permanent divergence

# Expected result: All nodes agree on consensus
```

---

## TASK 2: Test Consensus Timeouts (CRITICAL - 3 hours)

### Current Code Status
- ✅ Timeout exists in `src/consensus.rs:600-650`
- ❌ Never actually tested
- ❌ May not work under network conditions

### Test Scenario

**File to Create**: `tests/consensus_timeout_test.rs`

```rust
#[tokio::test]
async fn test_consensus_timeout_triggers_view_change() {
    // Setup: 3 masternodes
    let (node_a, node_b, node_c) = setup_three_node_network().await;
    
    // Node A is leader for view 0
    assert_eq!(node_a.current_view(), 0);
    
    // Simulate leader failing - stop sending blocks
    node_a.pause_block_production().await;
    
    // Wait for timeout (30 seconds)
    let start = SystemTime::now();
    
    // Verify nodes B and C trigger view change
    tokio::time::sleep(Duration::from_secs(31)).await;
    
    assert_eq!(node_b.current_view(), 1, "B should change to view 1");
    assert_eq!(node_c.current_view(), 1, "C should change to view 1");
    
    let elapsed = start.elapsed().unwrap();
    assert!(
        elapsed.as_secs() < 35,
        "View change should happen within timeout + margin"
    );
    
    // Resume A and verify it catches up
    node_a.resume_block_production().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(node_a.current_view(), 1, "A should catch up to view 1");
}

#[tokio::test]
async fn test_byzantine_leader_timeout() {
    // Setup: 7 masternodes
    let nodes = setup_seven_node_network().await;
    
    // Node 0 is leader - send malformed blocks
    let leader = &nodes[0];
    leader.send_invalid_block().await;  // Byzantine behavior
    
    // Other 6 nodes should all timeout
    tokio::time::sleep(Duration::from_secs(31)).await;
    
    // All should be in view 1 (new leader)
    for (i, node) in nodes.iter().enumerate().skip(1) {
        assert_eq!(node.current_view(), 1, "Node {} should timeout", i);
    }
    
    // Verify view 1 leader can make progress
    tokio::time::sleep(Duration::from_secs(5)).await;
    
    let block_height = nodes[1].chain_height().await;
    assert!(block_height > 0, "New leader should produce blocks");
}
```

### Measurement Required
```
Metrics to track:
- Time from timeout to view change: target <2s
- Time from view change to first block: target <5s
- Message overhead during view change: target <1MB total
- Memory stability: target <50MB delta
```

---

## TASK 3: Verify Peer Authentication (CRITICAL - 2 hours)

### Current Code Status
- ✅ Ed25519 signature verification exists
- ⚠️ Not sure if enforced on all connections
- ⚠️ No clear evidence of mandatory validation

### Required Verification Checklist

**File**: `src/network/server.rs`

```rust
// MUST verify each of these:

// 1. Incoming connection requires signature
pub async fn handle_incoming_connection(peer: TcpStream) {
    // Step 1: Receive auth message
    let auth = peer.read_message().await?;
    
    // Step 2: CRITICAL - Verify signature
    if !verify_ed25519_signature(&auth.signature, &auth.peer_id) {
        // MUST reject - close connection
        return Err("Invalid signature");  // <- Must enforce this
    }
    
    // Step 3: Add to peer registry
    peer_registry.add(auth.peer_id).await;
}

// 2. Rate limiting must be enforced
const MAX_REQUESTS_PER_SEC: usize = 10;

struct RateLimiter {
    requests_this_sec: Vec<Instant>,
}

impl RateLimiter {
    fn check_rate_limit(&mut self) -> Result<(), String> {
        let now = Instant::now();
        
        // Remove old entries
        self.requests_this_sec.retain(|t| now.duration_since(*t) < Duration::from_secs(1));
        
        if self.requests_this_sec.len() >= MAX_REQUESTS_PER_SEC {
            return Err("Rate limit exceeded".to_string());
        }
        
        self.requests_this_sec.push(now);
        Ok(())
    }
}

// 3. Malicious peer isolation
pub async fn handle_bad_signature() {
    // Must track failed signatures
    peer.signature_failures += 1;
    
    if peer.signature_failures >= 3 {
        // Isolation: Don't send messages to this peer
        // Don't accept new connections from this peer
        peer_registry.blacklist(peer.id).await;
    }
}
```

### Test Code
```rust
#[tokio::test]
async fn test_invalid_signature_rejected() {
    let server = start_test_server().await;
    
    // Try to connect with invalid signature
    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    
    let bad_auth = AuthMessage {
        peer_id: "attacker".to_string(),
        signature: [0u8; 64],  // Invalid!
    };
    
    stream.write(serde_json::to_vec(&bad_auth).unwrap()).await.unwrap();
    
    // Server must close connection
    let response = stream.read_message().await;
    assert!(response.is_err(), "Server must reject invalid signature");
}

#[tokio::test]
async fn test_rate_limiting() {
    let peer = create_peer().await;
    
    // Send 15 messages rapidly (max is 10/sec)
    for i in 0..15 {
        let result = peer.send_message(format!("msg{}", i)).await;
        
        if i < 10 {
            assert!(result.is_ok(), "First 10 should succeed");
        } else {
            assert!(result.is_err(), "Messages 11+ should be rate limited");
        }
    }
}

#[tokio::test]
async fn test_malicious_peer_isolation() {
    let mut peer = create_peer().await;
    
    // Send 3 messages with bad signatures
    for _ in 0..3 {
        peer.send_bad_signature().await;
    }
    
    // Peer should be blacklisted
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    let result = peer.send_message("ping".to_string()).await;
    assert!(result.is_err(), "Blacklisted peer shouldn't be able to send");
}
```

---

## TASK 4: Refactor Main Function (CODE QUALITY - 6 hours)

### Current: 979 lines - TOO BIG

### Target: <200 lines using AppBuilder pattern

### Implementation Steps

**Step 1: Create `src/app/mod.rs`**
```rust
pub mod builder;
pub mod context;
pub mod shutdown;

pub use builder::AppBuilder;
pub use context::AppContext;
```

**Step 2: Create `src/app/builder.rs`**
```rust
pub struct AppBuilder {
    config: Config,
    args: Args,
}

impl AppBuilder {
    pub fn new(config: Config, args: Args) -> Self {
        Self { config, args }
    }
    
    pub async fn build(self) -> Result<AppContext, AppError> {
        // Initialize storage
        let storage = self.init_storage().await?;
        
        // Initialize blockchain
        let blockchain = self.init_blockchain(storage.clone()).await?;
        
        // Initialize network
        let network = self.init_network().await?;
        
        // Initialize consensus
        let consensus = self.init_consensus(&blockchain).await?;
        
        Ok(AppContext {
            config: self.config,
            blockchain,
            consensus,
            network,
            // ... rest of components
        })
    }
    
    async fn init_storage(&self) -> Result<Arc<dyn UtxoStorage>, AppError> {
        match self.config.storage.backend.as_str() {
            "memory" => Ok(Arc::new(InMemoryUtxoStorage::new())),
            "sled" => {
                let storage = storage::SledUtxoStorage::new(
                    &self.config.storage.data_dir
                ).map_err(|e| AppError::Storage(e))?;
                Ok(Arc::new(storage))
            }
            _ => Err(AppError::Config("Unknown storage backend".to_string())),
        }
    }
    
    // ... other init methods
}
```

**Step 3: Create `src/app/shutdown.rs`**
```rust
use tokio_util::sync::CancellationToken;
use std::sync::Arc;

pub struct ShutdownManager {
    token: CancellationToken,
}

impl ShutdownManager {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }
    
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
    
    pub async fn wait_for_shutdown(&self) {
        tokio::signal::ctrl_c().await.ok();
        self.token.cancel();
    }
    
    pub async fn run_with_shutdown<F>(&self, mut f: F) 
    where
        F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
    {
        tokio::select! {
            _ = self.token.cancelled() => {
                tracing::info!("Shutdown signal received");
            }
            _ = f() => {
                tracing::info!("Task completed");
            }
        }
    }
}
```

**Step 4: Simplify `src/main.rs`**
```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    
    // Load config
    let config = Config::load_or_create(&args.config)?;
    
    // Setup logging
    setup_logging(&config.logging, args.verbose);
    
    // Print startup info
    print_startup_banner(&config);
    
    // Build application
    let app = AppBuilder::new(config, args)
        .build()
        .await?;
    
    // Run with graceful shutdown
    app.run().await
}

async fn print_startup_banner(config: &Config) {
    println!("🚀 TimeCoin v{}", env!("CARGO_PKG_VERSION"));
    println!("📡 Network: {:?}", config.node.network_type());
}
```

### Files to Create
- ✅ `src/app/mod.rs`
- ✅ `src/app/builder.rs`
- ✅ `src/app/context.rs` (already done as app_context.rs)
- ✅ `src/app/shutdown.rs`

### Files to Delete/Replace
- ❌ Extract from `src/main.rs`
- ❌ Remove duplicate code from main
- ❌ Update module declarations

---

## Code Review Checklist

After implementing each task, verify:

```
TASK 1: Fork Resolution
- [ ] Peer queries actually sent and received
- [ ] Byzantine quorum logic (2/3 + 1) correct
- [ ] Timeout handling (5 seconds max)
- [ ] Logging shows peer votes
- [ ] Test with 1 Byzantine peer: majority wins
- [ ] Test with 2 Byzantine peers: proper isolation

TASK 2: Consensus Timeouts
- [ ] 30-second timeout enforced
- [ ] View change triggers at all nodes
- [ ] New leader able to produce blocks
- [ ] Timeout latency <2 seconds
- [ ] Memory stable during timeout
- [ ] Works with 3, 7, 15 node networks

TASK 3: Peer Authentication
- [ ] Invalid signatures rejected on connect
- [ ] Rate limiting enforced (10 req/sec max)
- [ ] Malicious peers isolated after 3 failures
- [ ] No panic on bad signature
- [ ] Proper error logging
- [ ] Blacklist prevents reconnection

TASK 4: Main Function Refactoring
- [ ] Main.rs <200 lines
- [ ] All initialization in AppBuilder
- [ ] Graceful shutdown working
- [ ] Proper error propagation
- [ ] Logging on startup/shutdown
- [ ] No unwrap() in main path
```

---

## Summary

**Total Implementation Time**: ~15 hours

1. Fork Resolution: 4 hours
2. Timeout Testing: 3 hours
3. Auth Verification: 2 hours
4. Main Refactoring: 6 hours

**Critical for Production**: Tasks 1-3
**Important for Maintainability**: Task 4

All code changes should:
- ✅ Pass `cargo fmt`
- ✅ Pass `cargo clippy` (warnings OK, errors NO)
- ✅ Have error handling (no unwrap/panic)
- ✅ Include logging for debugging
- ✅ Include tests
