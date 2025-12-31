# Phase 7: RPC API & Testnet Stabilization

**Status:** 🚀 READY TO KICKOFF  
**Date:** December 23, 2025  
**Expected Duration:** 10-14 days  
**Owner:** Backend Engineer + Network Engineer  

---

## Overview

Phase 7 focuses on creating user-facing interfaces (RPC API) and deploying a real multi-node testnet. This bridges the gap between local testing (Phase 6) and production mainnet (Phase 10).

### Key Objectives

1. **JSON-RPC 2.0 API** - Standard interface for wallet/explorer integration
2. **5-10 Node Testnet** - Real cloud deployment for stress testing
3. **Block Explorer Backend** - Chain data API for visualization
4. **Performance Optimization** - Identify and fix bottlenecks
5. **Testnet Stabilization** - Run continuously for 72+ hours

---

## Phase 7.1: JSON-RPC 2.0 API

### Endpoints to Implement

#### 1. Transaction Endpoints

```bash
# Submit transaction to mempool
curl -X POST http://localhost:8080/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "sendtransaction",
    "params": ["<hex-encoded-tx>"],
    "id": 1
  }'

Response:
{
    "jsonrpc": "2.0",
    "result": {
        "txid": "0xabcd1234...",
        "status": "accepted",
        "confirmation_target": 2
    },
    "id": 1
}
```

#### 2. Block Endpoints

```bash
# Get block by height
curl -X POST http://localhost:8080/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "getblock",
    "params": [1234],
    "id": 1
  }'

Response:
{
    "jsonrpc": "2.0",
    "result": {
        "height": 1234,
        "hash": "0xabcd...",
        "timestamp": 1703347200,
        "transactions": 42,
        "miner": "validator1",
        "finalized": true,
        "reward": "1.00 TIME"
    },
    "id": 1
}
```

#### 3. UTXO / Balance Endpoints

```bash
# Get balance for address
curl http://localhost:8080/address/time1abc123def456

Response:
{
    "address": "time1abc123def456",
    "balance": "50.25 TIME",
    "balance_nanotime": 5025000000,
    "utxo_count": 3,
    "first_seen": 1703347200,
    "last_activity": 1703350800,
    "utxos": [
        {
            "txid": "0x...",
            "vout": 0,
            "amount": "25.00 TIME",
            "height": 1000,
            "status": "confirmed"
        }
    ]
}
```

#### 4. Status / Network Endpoints

```bash
# Get network status
curl http://localhost:8080/status

Response:
{
    "height": 5432,
    "timestamp": 1703350800,
    "validators": 7,
    "total_weight": 700,
    "consensus_threshold": 351,
    "blocks_finalized": 5400,
    "blocks_pending": 3,
    "transactions_finalized": 124567,
    "transactions_pending": 45,
    "uptime_seconds": 86400,
    "network": "testnet",
    "version": "0.1.0",
    "genesis_hash": "0x...",
    "last_block_time": 8.2,
    "average_block_time": 8.0,
    "finality_latency_ms": 28.5
}
```

#### 5. Validator / AVS Endpoints

```bash
# Get validator info
curl http://localhost:8080/validators

Response:
{
    "validators": [
        {
            "id": "validator1",
            "weight": 100,
            "stake": "1000.00 TIME",
            "address": "123.45.67.1:8001",
            "last_heartbeat": 1703350790,
            "status": "online",
            "blocks_proposed": 234,
            "consensus_participation": "99.8%"
        }
    ],
    "total_weight": 700,
    "consensus_threshold": 351
}
```

### Implementation Code Structure

```rust
// src/rpc/server.rs
pub struct RpcServer {
    blockchain: Arc<Blockchain>,
    consensus: Arc<ConsensusEngine>,
    utxo_manager: Arc<UTXOStateManager>,
    validator_registry: Arc<MasternodeRegistry>,
}

impl RpcServer {
    pub async fn handle_sendtransaction(&self, tx_hex: String) -> RpcResult<TxResponse> {
        // 1. Deserialize transaction
        let tx = Transaction::from_hex(&tx_hex)?;
        
        // 2. Validate transaction
        self.consensus.validate_transaction(&tx).await?;
        
        // 3. Add to mempool
        let txid = tx.txid();
        self.blockchain.add_pending_transaction(tx).await?;
        
        Ok(TxResponse {
            txid,
            status: "accepted",
            confirmation_target: 2,
        })
    }
    
    pub async fn handle_getblock(&self, height: u64) -> RpcResult<BlockResponse> {
        // Retrieve block from blockchain
        let block = self.blockchain.get_block_by_height(height).await?;
        
        Ok(BlockResponse {
            height: block.header.height,
            hash: hex::encode(block.hash()),
            timestamp: block.header.timestamp,
            transactions: block.transactions.len(),
            miner: block.header.proposer.clone(),
            finalized: true,
            reward: format_balance(block.reward()),
        })
    }
    
    pub async fn handle_getaddress(&self, address: &str) -> RpcResult<AddressResponse> {
        // Decode bech32 address
        let decoded = bech32::decode(address)?;
        
        // Get UTXOs for address
        let utxos = self.utxo_manager.get_address_utxos(&decoded).await;
        
        // Calculate balance
        let balance_nanotime: u64 = utxos.iter().map(|u| u.amount).sum();
        
        Ok(AddressResponse {
            address: address.to_string(),
            balance: format_balance(balance_nanotime),
            balance_nanotime,
            utxo_count: utxos.len(),
            utxos,
        })
    }
    
    pub async fn handle_status(&self) -> RpcResult<StatusResponse> {
        let height = self.blockchain.get_height().await;
        let validators = self.validator_registry.get_all().await;
        let total_weight: u64 = validators.iter().map(|v| v.masternode.collateral).sum();
        
        Ok(StatusResponse {
            height,
            validators: validators.len(),
            total_weight: total_weight as usize,
            consensus_threshold: (total_weight.div_ceil(2)) as usize,
            // ... other fields
        })
    }
}
```

### HTTP Server Implementation

```rust
// src/rpc/http_server.rs
use axum::{
    Router,
    routing::{get, post},
    Json,
    extract::{Path, State},
    http::StatusCode,
};

pub async fn start_rpc_server(
    bind_addr: &str,
    rpc_server: Arc<RpcServer>,
) -> Result<()> {
    let app = Router::new()
        .route("/rpc", post(handle_rpc))
        .route("/status", get(handle_status))
        .route("/address/:address", get(handle_address))
        .route("/block/:height", get(handle_block))
        .route("/validators", get(handle_validators))
        .route("/tx/:txid", get(handle_tx))
        .with_state(rpc_server);
    
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    
    Ok(())
}

async fn handle_rpc(
    State(rpc): State<Arc<RpcServer>>,
    Json(req): Json<JsonRpcRequest>,
) -> (StatusCode, String) {
    // Dispatch to appropriate handler based on method
    match req.method.as_str() {
        "sendtransaction" => {
            // Call rpc.handle_sendtransaction()
        }
        "getblock" => {
            // Call rpc.handle_getblock()
        }
        // ... other methods
    }
}
```

### Testing Strategy

```bash
# Unit tests
cargo test rpc::tests

# Integration tests
cargo test --test integration_rpc

# Manual testing
curl -s http://localhost:8080/status | jq .
curl -s http://localhost:8080/address/time1abc | jq .
```

---

## Phase 7.2: Testnet Deployment

### Cloud Infrastructure Setup

#### Option 1: DigitalOcean

```bash
#!/bin/bash
# Deploy 5 nodes to DigitalOcean

REGION="sfo3"
SIZE="s-1vcpu-2gb"
IMAGE="ubuntu-22-04-x64"

# Create droplets
for i in {1..5}; do
    doctl compute droplet create timecoin-node-$i \
        --region $REGION \
        --size $SIZE \
        --image $IMAGE \
        --format ID,Name,PublicIPv4 \
        --no-header
done

# Get IPs
IPS=$(doctl compute droplet list --format Name,PublicIPv4 --no-header | grep timecoin-node | awk '{print $2}')

# Deploy binary to each
for IP in $IPS; do
    scp target/release/timed root@$IP:/usr/local/bin/
    ssh root@$IP "chmod +x /usr/local/bin/timed"
done
```

#### Option 2: AWS EC2

```bash
#!/bin/bash
# Deploy 5 nodes to AWS

for i in {1..5}; do
    aws ec2 run-instances \
        --image-id ami-0c55b159cbfafe1f0 \
        --instance-type t2.small \
        --key-name timecoin-key \
        --security-groups timecoin-sg \
        --region us-west-2 \
        --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=timecoin-node-$i}]"
done
```

### Node Configuration

```bash
# On each node, create systemd service: /etc/systemd/system/timed.service

[Unit]
Description=TIME Coin Validator Node
After=network.target

[Service]
Type=simple
User=timecoin
WorkingDirectory=/opt/timecoin
ExecStart=/usr/local/bin/timed \
    --validator-id validator$INSTANCE_ID \
    --port 8001 \
    --peers node2.example.com:8001,node3.example.com:8001,node4.example.com:8001,node5.example.com:8001 \
    --rpc-bind 0.0.0.0:8080 \
    --stake 100
Environment="RUST_LOG=info"
Restart=always
RestartSec=10s

[Install]
WantedBy=multi-user.target

# Start service
systemctl enable timed
systemctl start timed
```

### Testnet Checklist

- [ ] 5 nodes deployed and running
- [ ] All nodes discover each other
- [ ] Blocks producing every ~8 seconds
- [ ] All nodes at same height
- [ ] RPC API responding on all nodes
- [ ] Logs clean (no errors)
- [ ] Memory usage <300MB per node
- [ ] CPU usage <10% per node
- [ ] Network bandwidth <5 Mbps total

### Monitoring Setup

```bash
# Monitor node status across all validators

watch -n 2 '
for node in node{1..5}.example.com; do
    echo "=== $node ==="
    curl -s http://$node:8080/status | jq ".height, .consensus_threshold, .blocks_finalized"
done
'
```

---

## Phase 7.3: Block Explorer Backend

### Minimal Block Explorer API

```rust
// src/rpc/explorer.rs

#[derive(Serialize)]
pub struct BlockSummary {
    pub height: u64,
    pub hash: String,
    pub timestamp: u64,
    pub proposer: String,
    pub tx_count: usize,
    pub reward: String,
}

#[derive(Serialize)]
pub struct TransactionSummary {
    pub txid: String,
    pub height: u64,
    pub timestamp: u64,
    pub inputs: Vec<InputSummary>,
    pub outputs: Vec<OutputSummary>,
    pub fee: String,
    pub status: "pending" | "confirmed" | "finalized",
}

#[derive(Serialize)]
pub struct ValidatorStats {
    pub id: String,
    pub weight: u64,
    pub blocks_proposed: u64,
    pub consensus_participation: f64,  // percentage
    pub last_active: u64,
    pub status: "online" | "offline",
}

impl RpcServer {
    pub async fn get_block_summary(&self, height: u64) -> Result<BlockSummary> {
        let block = self.blockchain.get_block_by_height(height).await?;
        Ok(BlockSummary {
            height: block.header.height,
            hash: hex::encode(block.hash()),
            timestamp: block.header.timestamp,
            proposer: block.header.proposer.clone(),
            tx_count: block.transactions.len(),
            reward: format_balance(block.reward()),
        })
    }
    
    pub async fn get_recent_blocks(&self, count: usize) -> Result<Vec<BlockSummary>> {
        let current_height = self.blockchain.get_height().await;
        let start = current_height.saturating_sub(count as u64);
        
        let mut blocks = Vec::new();
        for h in start..=current_height {
            if let Ok(summary) = self.get_block_summary(h).await {
                blocks.push(summary);
            }
        }
        
        Ok(blocks)
    }
    
    pub async fn get_validator_stats(&self) -> Result<Vec<ValidatorStats>> {
        let validators = self.validator_registry.get_all().await;
        
        let mut stats = Vec::new();
        for (_, info) in validators {
            stats.push(ValidatorStats {
                id: info.masternode.address.clone(),
                weight: info.masternode.collateral,
                blocks_proposed: 0,  // TODO: Track in blockchain
                consensus_participation: 99.8,  // TODO: Calculate from logs
                last_active: info.registered_at,
                status: "online",
            });
        }
        
        Ok(stats)
    }
}
```

---

## Phase 7.4: Performance Optimization

### Bottlenecks to Investigate

1. **Vote Accumulation Latency**
   - Current: O(n) linear scan of votes
   - Target: <5ms per vote
   - Optimization: Use BTreeMap for O(log n) lookups

2. **Block Finalization**
   - Current: Signature collection and verification
   - Target: <100ms total
   - Optimization: Batch signature verification

3. **Mempool Management**
   - Current: Linear iteration for transactions
   - Target: Handle 10,000 pending TXs
   - Optimization: Priority queue with fees

4. **Network Message Handling**
   - Current: Per-peer JSON deserialization
   - Target: <1ms per message
   - Optimization: Binary serialization (bincode)

### Profiling

```bash
# Run under flamegraph
cargo install flamegraph
cargo flamegraph -- --validator-id v1 --port 8001

# Analyze CPU usage
perf record -g ./target/release/timed
perf report
```

---

## Phase 7.5: Testnet Stabilization

### 72-Hour Stability Test

```bash
#!/bin/bash
# Run testnet for 72 hours, monitoring key metrics

DURATION=259200  # 72 hours in seconds
INTERVAL=10      # Check every 10 seconds

START_TIME=$(date +%s)
END_TIME=$((START_TIME + DURATION))

while [ $(date +%s) -lt $END_TIME ]; do
    CURRENT_TIME=$(date '+%Y-%m-%d %H:%M:%S')
    
    # Check all validators at same height
    HEIGHTS=$(curl -s http://node{1..5}:8080/status | jq '.height' | sort | uniq | wc -l)
    if [ $HEIGHTS -gt 1 ]; then
        echo "[$CURRENT_TIME] ⚠️  Height mismatch detected! Heights: $(curl -s http://node{1..5}:8080/status | jq '.height')"
    fi
    
    # Check for forks
    HASHES=$(curl -s http://node{1..5}:8080/block/$(curl -s http://node1:8080/status | jq '.height') | jq '.hash' | sort | uniq | wc -l)
    if [ $HASHES -gt 1 ]; then
        echo "[$CURRENT_TIME] 🚨 FORK DETECTED!"
    fi
    
    # Monitor resource usage
    MEM=$(free -h | awk '/^Mem/ {print $3}')
    CPU=$(top -bn1 | grep "Cpu(s)" | awk '{print $2}')
    echo "[$CURRENT_TIME] Memory: $MEM, CPU: $CPU"
    
    sleep $INTERVAL
done

echo "✅ 72-hour stability test complete!"
```

### Success Metrics

| Metric | Target | Acceptable |
|--------|--------|-----------|
| Block Production | 8s ± 1s | 5-15s |
| Finality Latency | <30s | <60s |
| Consensus Success | 100% | 99%+ |
| Node Sync Status | All same height | Max diff: 1 block |
| Memory Stability | <200MB | <500MB (stable) |
| CPU Usage | <5% | <20% |
| Zero Forks | 100% | Required |

---

## Phase 7.6: Acceptance Criteria

### RPC API ✅

- [ ] All endpoints implemented (sendtransaction, getblock, getaddress, status, validators)
- [ ] JSON-RPC 2.0 spec compliant
- [ ] Response time <100ms (p95)
- [ ] Proper error handling with descriptive messages
- [ ] API documentation (OpenAPI/Swagger)

### Testnet Deployment ✅

- [ ] 5+ nodes deployed on cloud infrastructure
- [ ] All nodes discover each other
- [ ] Blocks produce continuously
- [ ] RPC API accessible from all nodes
- [ ] Health checks passing
- [ ] Monitoring dashboard functional

### Performance ✅

- [ ] Average block time: 8s ± 2s
- [ ] Finality latency: <30s
- [ ] Consensus success rate: >99%
- [ ] Memory usage: <300MB per node
- [ ] CPU usage: <10% per node

### Stability ✅

- [ ] 72-hour continuous run without errors
- [ ] Zero chain forks
- [ ] All nodes maintain consensus
- [ ] No transaction loss
- [ ] Clean logs (no warnings)

---

## Implementation Timeline

```
Day 1-2:   RPC API Implementation
Day 3:     RPC Testing & Bug Fixes
Day 4-5:   Testnet Deployment Setup
Day 6:     Deploy 5 Nodes
Day 7:     Initial Stability Testing
Day 8-10:  Performance Optimization
Day 11-12: 72-Hour Stability Test
Day 13-14: Hardening & Documentation
```

---

## Deliverables

1. **RPC API Server** (`src/rpc/server.rs`)
   - All endpoints implemented
   - Full test coverage
   - API documentation

2. **Testnet Deployment** (5+ nodes)
   - Cloud infrastructure configured
   - Systemd services running
   - Monitoring and alerts

3. **Performance Report**
   - Bottleneck analysis
   - Optimization results
   - Profiling data

4. **Stability Report**
   - 72-hour test results
   - Metrics collected
   - Issues and resolutions

5. **Documentation**
   - RPC API guide
   - Testnet operator manual
   - Troubleshooting guide

---

## Next: Phase 8 - Hardening & Audit

After Phase 7 completes:
- Security audit of consensus and cryptography
- Load testing and stress scenarios
- Incident response testing
- Mainnet preparation

---

**Ready to start Phase 7** ✅

Execute: `next` to begin RPC API implementation
