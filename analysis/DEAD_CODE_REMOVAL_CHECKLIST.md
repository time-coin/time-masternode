# Dead Code Removal Action Plan

**Status:** UPDATED - Most items now integrated!  
**Last Updated:** 2025-12-24  
**Previous Estimate:** 35+ items to delete  
**Current Status:** Most items are now actively used

---

## ✅ Items Now INTEGRATED (Do NOT Delete)

The following items from the original checklist are now actively used in the codebase:

### PeerConnectionRegistry Methods - ALL USED ✅
- `should_connect_to()` - Used in client.rs for connection direction
- `mark_connecting()` - Used in client.rs, server.rs
- `is_connected()` - Used in client.rs, server.rs
- `mark_inbound()` - Used in server.rs for inbound connections
- `register_peer()` - Used in server.rs after handshake
- `send_to_peer()` - Used throughout for message routing (40+ call sites)
- `get_connected_peers()` - Used in blockchain.rs, main.rs for sync

### ConnectionManager Methods - ALL USED ✅
- `mark_connected()` - Used in client.rs:572
- `is_connected()` - Used throughout client.rs
- `mark_connecting()` - Used throughout client.rs
- `mark_inbound()` - Used in server.rs:334
- `mark_disconnected()` - Used in client.rs
- `is_reconnecting()` - Used in client.rs
- `clear_reconnecting()` - Used in client.rs

### TransactionPool Methods - ALL USED ✅
- `finalize_transaction()` - Used in avalanche.rs:197, consensus.rs:1554
- `reject_transaction()` - Used in consensus.rs:1566
- `is_pending()` - Used in server.rs:840
- `get_all_pending()` - Used in avalanche.rs:318
- `get_pending()` - Used in avalanche.rs:236, server.rs:840
- `is_finalized()` - Used in avalanche.rs:152, consensus.rs

### MasternodeRegistry Methods - ALL USED ✅
- `get_local_address()` - Keep for future use
- `broadcast_message()` - Used in server.rs:798, 811

### Blockchain Constants - ALL USED ✅
- `MAX_REORG_DEPTH` - Used in blockchain.rs:652
- `ALERT_REORG_DEPTH` - Used in blockchain.rs:659

---

## 🔄 Items Still Unused (Review Before Deleting)

### 1. Configuration Utilities (Keep for CLI)
- **File:** `src/config.rs`
- `get_data_dir()`, `get_network_data_dir()` - Future CLI integration
- `load_from_file()`, `default()`, `save_to_file()` - Config persistence
- **Action:** Keep for planned CLI improvements

### 2. RPC Alternative Implementation (Keep as Scaffolding)
- **File:** `src/rpc/server.rs`, `src/rpc/handler.rs`
- `RpcServer`, `RpcHandler`, `RpcRequest`, `RpcResponse`, `RpcError`
- **Action:** Keep as alternative RPC implementation option

### 3. Network Blacklist/Rate Limiter (SHOULD INTEGRATE)
- **File:** `src/network/blacklist.rs`
- `is_blacklisted()`, `record_violation()`, `add_temp_ban()`, `cleanup()`
- **File:** `src/network/rate_limiter.rs`
- `RateLimiter::new()`, `check()`
- **Action:** Wire to server.rs for security

### 4. ECVRF Functions (SHOULD INTEGRATE)
- **File:** `src/crypto/ecvrf.rs`
- `verify()`, `proof_to_hash()`
- **Action:** Wire to block producer VRF sortition

### 5. Heartbeat Attestation Methods (SHOULD INTEGRATE)
- **File:** `src/heartbeat_attestation.rs`
- `set_local_identity()`, `create_heartbeat()`, `get_stats()`
- **Action:** Wire to monitoring/RPC

### 6. NetworkClient Alternative (Keep as Scaffolding)
- **File:** `src/network/client.rs`
- `NetworkClient`, `spawn_connection_task`, `maintain_peer_connection`
- **Action:** Keep as alternative implementation

### 7. PeerManager (Superseded - Consider Removal)
- **File:** `src/peer_manager.rs`
- All methods superseded by `PeerConnectionRegistry`
- **Action:** Consider removal after confirming no dependencies

---

## Recommended Actions

### ✅ Do NOT Delete (Now Used)
1. All PeerConnectionRegistry methods listed above
2. All ConnectionManager methods listed above
3. All TransactionPool methods listed above
4. MasternodeRegistry broadcast methods
5. Blockchain reorg constants

### 🔧 Should Integrate (Priority)
1. `network/blacklist.rs` → Wire to server.rs connection handling
2. `network/rate_limiter.rs` → Wire to server.rs message handling
3. `crypto/ecvrf.rs` verify/proof_to_hash → Wire to block sortition
4. `heartbeat_attestation.rs` stats → Wire to RPC

### 📦 Keep as Scaffolding
1. RPC alternative implementation
2. NetworkClient alternative implementation
3. Config persistence utilities

### ❓ Review for Removal
1. `peer_manager.rs` - Fully superseded?

---

## Verification

After any changes, run:
```bash
cargo check  # Verify no new errors
cargo fmt    # Format code
cargo clippy # Check for warnings
```

---

## Summary

**Original Plan:** Delete 35+ items  
**Updated Status:** Most items now integrated and actively used!

The codebase has progressed significantly since the original analysis. The network layer, connection management, transaction pool, and masternode registry are now fully wired together.

**Remaining dead code (~51 warnings) is primarily:**
- Alternative implementations (RPC, NetworkClient)
- Config utilities for future CLI
- ECVRF functions awaiting VRF sortition integration
- Heartbeat stats awaiting RPC integration

---

*Updated 2025-12-24 to reflect integration progress.*
