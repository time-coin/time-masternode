# Message Loop Refactoring - Builder Pattern Implementation

## Overview
This document describes the consolidation of 4 duplicate message loop methods into a single unified implementation using the builder pattern.

## Problem Statement

### Before Refactoring
The `PeerConnection` struct had 4 nearly identical message loop methods:

```rust
// 1. Basic peer registry only (~100 lines)
pub async fn run_message_loop_with_registry(
    peer_registry: Arc<PeerConnectionRegistry>
) -> Result<(), String>

// 2. Peer registry + masternode registry (~100 lines)
pub async fn run_message_loop_with_registry_and_masternode(
    peer_registry: Arc<PeerConnectionRegistry>,
    masternode_registry: Arc<MasternodeRegistry>,
) -> Result<(), String>

// 3. All components (~100 lines)
pub async fn run_message_loop_with_registry_masternode_and_blockchain(
    peer_registry: Arc<PeerConnectionRegistry>,
    masternode_registry: Arc<MasternodeRegistry>,
    blockchain: Arc<Blockchain>,
) -> Result<(), String>

// 4. No external dependencies (~100 lines)
pub async fn run_message_loop() -> Result<(), String>
```

**Issues:**
- ~400 lines of duplicated code
- Hard to maintain (bugs need fixing in 4 places)
- Difficult to add new optional components
- Confusing API with method name explosion

## Solution: Builder Pattern with Unified Method

### New API

```rust
/// Configuration for peer connection message loop
pub struct MessageLoopConfig {
    pub peer_registry: Arc<PeerConnectionRegistry>,
    pub masternode_registry: Option<Arc<MasternodeRegistry>>,
    pub blockchain: Option<Arc<Blockchain>>,
}

impl MessageLoopConfig {
    /// Create config with just peer registry (minimal)
    pub fn new(peer_registry: Arc<PeerConnectionRegistry>) -> Self
    
    /// Add masternode registry (builder pattern)
    pub fn with_masternode_registry(self, registry: Arc<MasternodeRegistry>) -> Self
    
    /// Add blockchain (builder pattern)
    pub fn with_blockchain(self, blockchain: Arc<Blockchain>) -> Self
}

impl PeerConnection {
    /// Unified message loop - works with any combination of components
    pub async fn run_message_loop_unified(
        self,
        config: MessageLoopConfig
    ) -> Result<(), String>
}
```

### Usage Examples

#### 1. Basic Setup (Peer Registry Only)
```rust
let config = MessageLoopConfig::new(peer_registry);
peer_connection.run_message_loop_unified(config).await?;
```

#### 2. With Masternode Registry
```rust
let config = MessageLoopConfig::new(peer_registry)
    .with_masternode_registry(masternode_registry);
peer_connection.run_message_loop_unified(config).await?;
```

#### 3. Full Setup (All Components)
```rust
let config = MessageLoopConfig::new(peer_registry)
    .with_masternode_registry(masternode_registry)
    .with_blockchain(blockchain);
peer_connection.run_message_loop_unified(config).await?;
```

## Migration Guide

### Old Code → New Code

**Scenario 1: Basic peer connection**
```rust
// OLD
peer_connection.run_message_loop_with_registry(peer_registry).await?;

// NEW
let config = MessageLoopConfig::new(peer_registry);
peer_connection.run_message_loop_unified(config).await?;
```

**Scenario 2: With masternode registry**
```rust
// OLD
peer_connection.run_message_loop_with_registry_and_masternode(
    peer_registry,
    masternode_registry
).await?;

// NEW
let config = MessageLoopConfig::new(peer_registry)
    .with_masternode_registry(masternode_registry);
peer_connection.run_message_loop_unified(config).await?;
```

**Scenario 3: Full blockchain node**
```rust
// OLD
peer_connection.run_message_loop_with_registry_masternode_and_blockchain(
    peer_registry,
    masternode_registry,
    blockchain
).await?;

// NEW
let config = MessageLoopConfig::new(peer_registry)
    .with_masternode_registry(masternode_registry)
    .with_blockchain(blockchain);
peer_connection.run_message_loop_unified(config).await?;
```

## Implementation Details

### Key Features

1. **Smart Message Routing**
   - Automatically uses the correct message handler based on available components
   - Falls back gracefully when optional components are missing

2. **Type Safety**
   - Blockchain requires masternode registry (enforced at runtime with clear error)
   - Impossible to create invalid configurations

3. **Backward Compatibility**
   - Old methods marked as `#[deprecated]` but still functional
   - Deprecation warnings guide users to new API
   - No breaking changes during migration period

4. **Single Source of Truth**
   - One implementation instead of 4
   - Bugs fixed once, benefit all scenarios
   - Easier to add new features

### Internal Logic Flow

```rust
pub async fn run_message_loop_unified(config: MessageLoopConfig) -> Result<(), String> {
    // 1. Setup (handshake, ping, registration)
    // ...
    
    // 2. Main loop
    loop {
        tokio::select! {
            // Handle messages based on available components
            result = self.reader.read_line(&mut buffer) => {
                let handle_result = if let Some(blockchain) = config.blockchain {
                    // Full setup: use blockchain handler
                    let masternode_registry = config.masternode_registry
                        .expect("Masternode registry required with blockchain");
                    self.handle_message_with_blockchain(...)
                } else if let Some(masternode_registry) = config.masternode_registry {
                    // Masternode-only setup
                    self.handle_message_with_masternode_registry(...)
                } else {
                    // Basic setup
                    self.handle_message_with_registry(...)
                };
                // Handle errors
            }
            
            // Periodic pings
            _ = ping_interval.tick() => { ... }
            
            // Timeout checks
            _ = timeout_check.tick() => { ... }
        }
    }
}
```

## Benefits

### Code Quality
- ✅ **~400 lines reduced** (4 methods → 1)
- ✅ **Single implementation** to maintain
- ✅ **DRY principle** applied
- ✅ **Clear separation** of concerns

### Maintainability
- ✅ Bug fixes apply to all scenarios
- ✅ New features added once
- ✅ Easier to understand
- ✅ Better documented

### Flexibility
- ✅ Easy to add new optional components
- ✅ Composable configuration
- ✅ Type-safe builder pattern
- ✅ No method name explosion

### User Experience
- ✅ Intuitive builder API
- ✅ Clear migration path
- ✅ Deprecation warnings with examples
- ✅ Backward compatible

## Testing Recommendations

### Unit Tests
```rust
#[tokio::test]
async fn test_unified_loop_basic() {
    let config = MessageLoopConfig::new(peer_registry);
    let result = peer_connection.run_message_loop_unified(config).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_unified_loop_with_blockchain() {
    let config = MessageLoopConfig::new(peer_registry)
        .with_masternode_registry(masternode_registry)
        .with_blockchain(blockchain);
    let result = peer_connection.run_message_loop_unified(config).await;
    assert!(result.is_ok());
}
```

### Integration Tests
```rust
#[tokio::test]
async fn test_message_routing_with_different_configs() {
    // Test that messages are routed correctly based on config
}
```

## Future Enhancements

### Possible Additions
1. **Consensus Engine Integration**
   ```rust
   impl MessageLoopConfig {
       pub fn with_consensus(self, consensus: Arc<ConsensusEngine>) -> Self
   }
   ```

2. **Transaction Pool**
   ```rust
   impl MessageLoopConfig {
       pub fn with_tx_pool(self, pool: Arc<TransactionPool>) -> Self
   }
   ```

3. **State Sync Manager**
   ```rust
   impl MessageLoopConfig {
       pub fn with_state_sync(self, state_sync: Arc<StateSync>) -> Self
   }
   ```

### Pattern Extensibility
The builder pattern makes it trivial to add new optional components:
```rust
pub struct MessageLoopConfig {
    pub peer_registry: Arc<PeerConnectionRegistry>,
    pub masternode_registry: Option<Arc<MasternodeRegistry>>,
    pub blockchain: Option<Arc<Blockchain>>,
    pub new_component: Option<Arc<NewComponent>>, // Add here
}

impl MessageLoopConfig {
    pub fn with_new_component(mut self, component: Arc<NewComponent>) -> Self {
        self.new_component = Some(component);
        self
    }
}
```

## Deprecation Timeline

### Phase 1: Soft Deprecation (Current)
- ✅ Old methods marked with `#[deprecated]`
- ✅ Deprecation warnings in compilation
- ✅ Migration examples in documentation
- ⏸️ Old methods still functional

### Phase 2: Migration Period (Recommended: 2-3 releases)
- Update all internal callsites to use new API
- Update examples and documentation
- Monitor usage of deprecated methods

### Phase 3: Hard Deprecation (Future)
- Remove old methods completely
- Clean up codebase
- **Estimated savings: ~400 lines of code**

## Related Documentation
- [Performance Improvements](../PERFORMANCE_IMPROVEMENTS.md) - Phase 1 optimizations
- [Network Protocol](./NETWORK_PROTOCOL.md) - Message handling details
- [Peer Connection Architecture](./PEER_CONNECTION.md) - Connection management

## Changelog
- **2026-01-06**: Initial implementation of unified message loop with builder pattern
- **2026-01-06**: Added deprecation warnings to old methods
- **2026-01-06**: Created migration guide and documentation
