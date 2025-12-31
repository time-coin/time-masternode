# TODO: Complete PeerConnectionRegistry Integration

**Priority**: HIGH  
**Status**: PARTIAL - Parameter passing done, message loop refactor needed  
**Created**: 2024-12-15

## Problem

Currently, we have TWO ways of sending messages to peers:
1. **Direct writer** - Each connection loop writes directly to its TCP stream
2. **PeerConnectionRegistry** - Exists but not used, designed for request/response pattern

This causes issues:
- `query_peer_block_hash()` creates NEW connections instead of using existing ones
- Violates "single connection per peer" principle
- Can't do proper request/response pattern for queries

## Current State

✅ **DONE**:
- PeerConnectionRegistry exists with `send_and_await_response()` method
- Parameter passing infrastructure added (peer_registry passed through NetworkClient/Server)

❌ **NOT DONE**:
- Connections not registered in PeerConnectionRegistry
- Message loops still write directly to TCP streams
- Blockchain doesn't use peer_registry for queries

## What Needs to Be Done

### Phase 1: Register Connections (CRITICAL)

**File**: `src/network/client.rs` - `maintain_peer_connection()`

After handshake completes (around line 530):
```rust
// Register writer with PeerConnectionRegistry
peer_registry.register_peer(ip.to_string(), writer).await;
tracing::debug!("📝 Registered {} in PeerConnectionRegistry", ip);
```

**File**: `src/network/server.rs` - `handle_connection()`

After handshake (around line 240):
```rust
// Register writer with PeerConnectionRegistry  
peer_registry.register_peer(peer_ip.to_string(), writer).await;
```

### Phase 2: Replace Direct Writes with Registry

**In**: `src/network/client.rs`

Replace all instances of:
```rust
let msg_json = serde_json::to_string(&message)?;
writer.write_all(format!("{}\n", msg_json).as_bytes()).await?;
writer.flush().await?;
```

With:
```rust
peer_registry.send_to_peer(ip, message).await?;
```

**Locations to update**:
- Initial sync messages (GetBlockHeight, GetPendingTransactions, etc.) - lines ~576-640
- Periodic heartbeat messages - lines ~650-695  
- Response messages (Ack, BlockHashResponse, etc.) - lines ~726, 773, 805, 900, 989

**In**: `src/network/server.rs`

Similar replacements for server-side message sending.

### Phase 3: Handle Response Routing

**File**: `src/network/client.rs` and `src/network/server.rs`

When receiving `BlockHashResponse` or other response messages:
```rust
NetworkMessage::BlockHashResponse { height, hash } => {
    // Route to any waiting query
    peer_registry.handle_response(ip, NetworkMessage::BlockHashResponse { height, hash }).await;
    // Also handle locally if needed
}
```

### Phase 4: Update Blockchain Queries

**File**: `src/blockchain.rs` - `query_peer_block_hash()`

Replace entire function body:
```rust
async fn query_peer_block_hash(
    &self,
    peer_ip: &str,
    height: u64,
) -> Result<Option<[u8; 32]>, String> {
    // Use peer registry if available
    if let Some(peer_reg) = &self.peer_registry {
        let message = NetworkMessage::GetBlockHash(height);
        match peer_reg.send_and_await_response(peer_ip, message, 5).await {
            Ok(NetworkMessage::BlockHashResponse { height: _, hash }) => {
                return Ok(hash);
            }
            Ok(_) => return Err("Unexpected response".to_string()),
            Err(e) => return Err(e),
        }
    }
    
    // Fallback: create new connection (for backward compatibility during migration)
    // TODO: Remove this fallback once all peers use registry
    // ... existing code ...
}
```

### Phase 5: Cleanup

Once all paths use PeerConnectionRegistry:
- Remove all direct `writer.write_all()` calls from loops
- Remove fallback connection creation in `query_peer_block_hash()`  
- Update tests to use registry pattern

## Benefits After Completion

✅ Single connection per peer (no duplicate connections)  
✅ Proper request/response pattern for queries  
✅ No "invalid socket address" errors from queries  
✅ Better connection management  
✅ Can implement timeouts and retries properly  
✅ Foundation for future features (streaming, multiplexing, etc.)

## Estimated Effort

- **Phase 1**: 30 minutes (register connections)
- **Phase 2**: 2 hours (replace all direct writes)  
- **Phase 3**: 30 minutes (response routing)
- **Phase 4**: 30 minutes (blockchain queries)
- **Phase 5**: 30 minutes (cleanup)

**Total**: ~4 hours of focused work

## Testing Plan

1. Start node with changes
2. Verify connections are registered (check logs for "📝 Registered")
3. Trigger fork consensus check
4. Verify queries use existing connections (no new connection attempts)
5. Check for "invalid socket address" errors (should be none)
6. Verify block syncing works correctly

## Related Files

- `src/network/peer_connection_registry.rs` - Registry implementation
- `src/network/client.rs` - Outbound connections
- `src/network/server.rs` - Inbound connections  
- `src/blockchain.rs` - Query implementation
- `src/main.rs` - Initialization

## Notes

- The PeerConnectionRegistry code is already written and works
- This is purely an integration/refactor task
- Can be done incrementally (phase by phase)
- Backward compatibility can be maintained during migration

---

**Next Action**: Implement Phase 1 - register connections in both client and server
