# Network Directory Consolidation Plan

**Status:** In Progress (Incomplete)

## Current Issues

### 1. **Missing Module Definitions**
- `server.rs` imports `use crate::network::peer_state::PeerStateManager;` 
- But `peer_state.rs` doesn't exist
- `PeerStateManager` is actually defined in `peer_connection.rs`
- **Import Error**: This breaks the build

### 2. **Incomplete Import in server.rs**
- Line 5: `use crate::network::peer_state::PeerStateManager;`
- Should be: `use crate::network::peer_connection::PeerStateManager;`

### 3. **Missing connection_manager Module**
- `server.rs` line 32 uses: `pub connection_manager: Arc<crate::network::connection_manager::ConnectionManager>`
- But `connection_manager.rs` doesn't exist
- Related functionality is in `connection_state.rs`

## Files Needing Consolidation

### Group 1: Security & Transport (Related by Function)
- **tls.rs** - TLS configuration for encryption
- **signed_message.rs** - Message signing/verification  
- **secure_transport.rs** - Combines both TLS + signing (marked with TODO: "Remove once integrated")

**Action:** Merge into `tls.rs` as security module

### Group 2: Connection Management (Duplicate/Overlapping)
- **connection_state.rs** - Connection state machine
- **peer_connection.rs** - Peer connection handler + PeerStateManager
- **peer_connection_registry.rs** - Registry of peer connections

**Current Issue:** These are trying to be split but imports are broken

### Group 3: Networking Core (Coordination)
- **client.rs** - Network client functionality
- **server.rs** - Network server functionality
- **peer_manager.rs** (in root src/) - Peer management

### Group 4: Utility/Filtering
- **rate_limiter.rs** - Rate limiting
- **blacklist.rs** - IP blacklisting
- **dedup_filter.rs** - Deduplication

## Recommended Actions

### Immediate (To Fix Build)
1. Fix import in `server.rs`: Change `peer_state` → `peer_connection`
2. Either:
   - Option A: Create stub `connection_manager.rs` 
   - Option B: Use `connection_state.rs` directly

### Short Term (Complete Consolidation)
1. Merge `signed_message.rs` into `tls.rs` → `security.rs`
2. Remove `secure_transport.rs` (not integrated)
3. Consolidate connection management (state machine + peer connection)

### Files Currently Working
- ✅ `message.rs` - Message types (no consolidation needed)
- ✅ `state_sync.rs` - State synchronization (no consolidation needed)
- ✅ `dedup_filter.rs` - Deduplication (no consolidation needed)
- ✅ `rate_limiter.rs` - Rate limiting (no consolidation needed)
- ✅ `blacklist.rs` - Blacklisting (no consolidation needed)

## Impact of Consolidation
- **Reduce files** from 14 to ~8-9
- **Fix broken imports**
- **Improve code organization**
- **Reduce redundancy** (no duplicate connection tracking)
