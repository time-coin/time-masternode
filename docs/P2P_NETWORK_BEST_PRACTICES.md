# Peer-to-Peer Network Best Practices

## Overview
This document outlines best practices for maintaining a stable and efficient P2P network for TIME Coin masternodes.

## Connection Management

### 1. Single Connection Per Peer
**Rule**: Maintain exactly ONE connection to each unique IP address.

**Why**:
- Prevents resource waste
- Eliminates duplicate message processing
- Reduces network overhead
- Simplifies connection state tracking

**Implementation**:
- Track active connections by IP in a HashMap
- Check before creating new outbound connections
- Reject duplicate inbound connections from same IP
- Use connection manager to enforce single connection rule

### 2. Persistent Connections
**Rule**: Keep connections alive indefinitely once established.

**Why**:
- Transactions can arrive at any time
- Block production happens every 10 minutes
- Reconnection delays can cause missed transactions
- Network stability requires consistent peer availability

**Implementation**:
- Use TCP keepalive to detect dead connections
- Set SO_KEEPALIVE socket option
- Monitor connection health with heartbeats
- Only close on explicit errors or shutdown

### 3. Fast Reconnection
**Rule**: Reconnect quickly but intelligently after disconnection.

**Why**:
- Minimize transaction loss window
- Maintain consensus participation
- Quick recovery from temporary network issues

**Implementation**:
- Initial retry: 5 seconds
- Exponential backoff: 5s → 10s → 30s → 60s → 300s (max)
- Reset backoff on successful connection
- Give up after 10 consecutive failures

### 4. Connection Deduplication
**Rule**: Prevent multiple simultaneous connection attempts to the same peer.

**Why**:
- Wastes resources on duplicate attempts
- Can overwhelm peer with connection requests
- Creates log spam and confusion

**Implementation**:
```rust
// Track in-flight connection attempts
if connecting_to.contains(ip) {
    return; // Already trying to connect
}
connecting_to.insert(ip);
```

## Message Handling

### 5. Transaction Synchronization
**Rule**: Request missed transactions immediately upon connection.

**Why**:
- Transactions can arrive during disconnection
- Mempool must stay synchronized across network
- Prevents missing transactions in next block

**Implementation**:
- Send GetMempool message on connection
- Peer responds with all pending transactions
- Merge into local mempool (deduplication)

### 6. Block Synchronization
**Rule**: Check peer height and sync blocks every 5 minutes.

**Why**:
- Blocks are produced every 10 minutes
- 5-minute interval catches new blocks quickly
- Detects and resolves forks
- Ensures all nodes stay synchronized

**Implementation**:
- Send GetHeight message to all peers
- If peer height > our height, request missing blocks
- Process blocks in order
- Log height status for monitoring

### 7. Message Deduplication
**Rule**: Never process the same message twice.

**Why**:
- Prevents double-spending
- Avoids duplicate block processing
- Reduces CPU waste

**Implementation**:
- Track message IDs (transaction hashes, block hashes)
- Check before processing
- Clean up old IDs periodically (keep last 1000)

## Network Discovery

### 8. Peer Discovery Strategy
**Rule**: Use multiple discovery methods with fallback.

**Why**:
- Single point of failure is risky
- Network should be self-healing
- Improves decentralization

**Methods** (in order):
1. **Seed peers from config** - hardcoded reliable nodes
2. **API discovery** - centralized discovery server (time-coin.io/api/peers)
3. **Peer exchange** - ask connected peers for their peer list
4. **Cached peers** - previously connected peers saved to disk

### 9. Peer Quality Tracking
**Rule**: Track and prioritize reliable peers.

**Why**:
- Some peers are more stable than others
- Prefer peers with good uptime
- Avoid repeatedly connecting to bad peers

**Metrics to Track**:
- Connection success rate
- Uptime percentage
- Response time
- Block sync speed
- Number of disconnections

### 10. Peer Limits
**Rule**: Maintain 8-50 peer connections.

**Why**:
- Minimum 8 ensures redundancy
- Maximum 50 prevents resource exhaustion
- Balance between connectivity and overhead

**Implementation**:
- Close lowest quality connections when at limit
- Prioritize masternode connections
- Keep connections to geographically diverse peers

## Security

### 11. Protocol Version Checking
**Rule**: Reject connections from incompatible protocol versions.

**Why**:
- Prevents protocol confusion
- Ensures all nodes speak same language
- Allows clean protocol upgrades

**Implementation**:
```rust
const MAGIC_BYTES: [u8; 4] = [84, 73, 77, 69]; // "TIME"
if magic != MAGIC_BYTES {
    reject_connection();
}
```

### 12. IP Blacklisting
**Rule**: Automatically ban IPs that misbehave.

**Why**:
- Prevents spam attacks
- Stops protocol version confusion spam
- Protects against malicious nodes

**Violations**:
- Wrong protocol magic bytes (3 strikes)
- Excessive connection attempts (>5/minute)
- Invalid messages (3 strikes)
- Ban duration: 5 minutes (temporary), 24 hours (persistent)

### 13. Rate Limiting
**Rule**: Limit inbound connections per IP.

**Why**:
- Prevents connection flood attacks
- Ensures fair resource distribution
- Protects against DoS

**Limits**:
- 1 connection per IP
- 10 connection attempts per IP per minute
- 100 messages per minute per peer

## Logging and Monitoring

### 14. Connection Logging
**Rule**: Log all connection state changes with hostname.

**Why**:
- Troubleshoot network issues
- Monitor network health
- Detect connection patterns

**Log Format**:
```
2025-12-10 22:08:23 [LW-Michigan] INFO ✓ Connected to peer: 50.28.104.50
2025-12-10 22:08:45 [LW-Michigan] WARN Connection to 50.28.104.50 lost
2025-12-10 22:08:50 [LW-Michigan] INFO Reconnected to 50.28.104.50
```

### 15. Status Reporting
**Rule**: Report network status every 5 minutes.

**Why**:
- Monitor health at a glance
- Detect issues proactively
- Track network growth

**Report Contents**:
```
📊 Status: Height=1424, Active Masternodes=5, Connected Peers=4
```

### 16. Avoid Log Spam
**Rule**: Consolidate redundant log messages.

**Why**:
- Makes logs readable
- Easier to spot real issues
- Reduces disk I/O

**Anti-patterns**:
- ❌ Logging connection closure 3 times
- ❌ Logging same ban multiple times
- ❌ Excessive debug output in production

## Performance

### 17. Async I/O
**Rule**: Use async/await for all network operations.

**Why**:
- Handle many connections efficiently
- Non-blocking I/O
- Better resource utilization

**Implementation**:
- Use tokio runtime
- spawn tasks for each connection
- Use channels for cross-task communication

### 18. Message Batching
**Rule**: Batch multiple small messages when possible.

**Why**:
- Reduces network overhead
- Fewer TCP packets
- Better throughput

**Examples**:
- Send multiple transactions in one message
- Batch block requests

### 19. Connection Pooling
**Rule**: Reuse connections, don't create/destroy constantly.

**Why**:
- TCP handshake is expensive
- Connection setup has overhead
- Persistent connections are faster

## High Availability

### 20. Automatic Failover
**Rule**: If connection fails, immediately try next best peer.

**Why**:
- Maintains network connectivity
- No manual intervention needed
- Quick recovery from failures

### 21. Geographic Diversity
**Rule**: Connect to peers in different regions/networks.

**Why**:
- Resilient to regional outages
- Protects against network partitions
- Improves global latency distribution

### 22. Health Checks
**Rule**: Periodically verify peer responsiveness.

**Why**:
- Detect dead but not-yet-closed connections
- Identify slow/unresponsive peers
- Trigger reconnection before complete failure

**Implementation**:
- Send ping every 30 seconds
- Expect pong within 5 seconds
- Close connection if 3 pings timeout

## Consensus Specific

### 23. Masternode Peer Priority
**Rule**: Always maintain connections to all active masternodes.

**Why**:
- Block production requires masternode consensus
- Missing masternode connection = failed block production
- Critical for network operation

### 24. Transaction Propagation
**Rule**: Forward new transactions to ALL peers immediately.

**Why**:
- Fast mempool synchronization
- Ensures transactions reach block producers
- Reduces confirmation time

### 25. Block Propagation
**Rule**: Forward new blocks to ALL peers immediately.

**Why**:
- Critical for consensus
- Prevents forks
- Keeps network synchronized

## Testing and Validation

### 26. Connection Resilience Testing
**Rule**: Regularly test network under adverse conditions.

**Tests**:
- Random peer disconnections
- Network partitions
- High latency simulation
- Packet loss simulation

### 27. Load Testing
**Rule**: Test with realistic and peak loads.

**Scenarios**:
- 100+ concurrent connections
- 1000 transactions per second
- Network with 100+ masternodes

## Configuration

### 28. Configurable Parameters
**Rule**: Make network parameters configurable.

**Tunable Settings**:
```toml
[network]
max_peers = 50
min_peers = 8
connection_timeout = 30
reconnect_delay = 5
sync_interval = 300
heartbeat_interval = 30
```

### 29. Feature Flags
**Rule**: Use feature flags for experimental features.

**Why**:
- Safe testing in production
- Gradual rollout
- Easy rollback

## Summary Checklist

✅ Single connection per peer
✅ Persistent connections with fast reconnect
✅ Transaction sync on connect
✅ Exponential backoff for failures
✅ IP blacklisting for misbehavior
✅ Rate limiting for security
✅ Async I/O for performance
✅ Connection deduplication
✅ Health checks and monitoring
✅ Masternode peer priority
✅ Geographic diversity
✅ Comprehensive logging
✅ Configurable parameters

## References

- Bitcoin P2P network protocol
- Ethereum DevP2P specification
- Kadmelia DHT protocol
- TIME Coin consensus documentation
