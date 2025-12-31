# Dead Code Action Plan

**Created:** 2025-12-24  
**Goal:** Systematically address all dead code warnings (~51 items)  
**Approach:** Integrate useful code, delete truly unused code

---

## Overview

Dead code falls into 3 categories:
1. **FALSE POSITIVES** - Used in binary but flagged in library
2. **SHOULD INTEGRATE** - Useful code not yet wired in
3. **SHOULD DELETE** - Superseded or unnecessary code

---

## Category 1: FALSE POSITIVES (No Action Needed)

These items ARE used in `src/main.rs` (binary) but appear unused when checking the library.

| File | Item | Status |
|------|------|--------|
| `main.rs:80` | `fn main()` | ✅ Entry point |
| `main.rs:1196` | `fn setup_logging()` | ✅ Called in main |
| `main.rs:1255` | `struct CustomTimer` | ✅ Used for tracing |
| `shutdown.rs` | `ShutdownManager` | ✅ Used in main.rs:128 |

**Action:** Add `#![allow(dead_code)]` comment explaining these are binary-only.

---

## Category 2: SHOULD INTEGRATE (High Value)

### 2.1 IP Blacklisting → Wire to Server
**File:** `src/network/blacklist.rs`  
**Items:** `is_blacklisted()`, `record_violation()`, `add_temp_ban()`, `add_permanent_ban()`, `cleanup()`

**Current State:** `IPBlacklist` struct exists but never instantiated in server.

**Integration Steps:**
```
□ Step 1: In src/network/server.rs, add blacklist field to NetworkServer
□ Step 2: In handle_peer(), call is_blacklisted() before accepting connection
□ Step 3: On protocol violation, call record_violation()
□ Step 4: Spawn cleanup task to periodically call cleanup()
```

**Effort:** 30 minutes  
**Priority:** HIGH (security feature)

---

### 2.2 Rate Limiting → Wire to Server
**File:** `src/network/rate_limiter.rs`  
**Items:** `RateLimiter::new()`, `check()`

**Current State:** RateLimiter exists but never instantiated.

**Integration Steps:**
```
□ Step 1: In NetworkServer, add rate_limiter field
□ Step 2: In message handler, call rate_limiter.check() before processing
□ Step 3: If rate limited, log warning and skip message
```

**Effort:** 20 minutes  
**Priority:** HIGH (DoS protection)

---

### 2.3 ECVRF → Wire to Block Sortition
**File:** `src/crypto/ecvrf.rs`  
**Items:** `verify()`, `proof_to_hash()`, `InvalidProof`, `VerificationFailed`, `InvalidKey`

**Current State:** VRF evaluation is implemented but verification not used.

**Integration Steps:**
```
□ Step 1: In block production (main.rs), replace SHA256-based leader selection with ECVRF
□ Step 2: Block producer calls ECVRF::evaluate() to generate VRF proof
□ Step 3: Block header includes VRF output and proof
□ Step 4: Validators call ECVRF::verify() when receiving blocks
□ Step 5: Use proof_to_hash() to compute deterministic sortition weight
```

**Effort:** 2-3 hours  
**Priority:** MEDIUM (improves randomness security)

**Note:** Currently using SHA256(prev_hash + height) which is deterministic but predictable. ECVRF adds unpredictability.

---

### 2.4 Heartbeat Attestation → Wire to Monitoring
**File:** `src/heartbeat_attestation.rs`  
**Items:** `set_local_identity()`, `create_heartbeat()`, `get_next_sequence()`, `get_verified_heartbeats()`, `get_stats()`, `AttestationStats`

**Current State:** HeartbeatAttestationSystem exists but local identity never set.

**Integration Steps:**
```
□ Step 1: In main.rs masternode setup, call set_local_identity() with signing key
□ Step 2: Replace current heartbeat creation with create_heartbeat()
□ Step 3: Wire get_stats() to RPC endpoint for monitoring
□ Step 4: Wire get_verified_heartbeats() to masternode uptime tracking
```

**Effort:** 1 hour  
**Priority:** MEDIUM (improves attestation system)

---

### 2.5 NetworkType Methods → Wire to Config
**File:** `src/network_type.rs`  
**Items:** `magic_bytes()`, `default_p2p_port()`, `default_rpc_port()`, `address_prefix()`

**Current State:** Methods exist but config uses hardcoded values.

**Integration Steps:**
```
□ Step 1: In config.rs, use NetworkType::default_p2p_port() instead of hardcoded
□ Step 2: In config.rs, use NetworkType::default_rpc_port() instead of hardcoded
□ Step 3: In address generation, use NetworkType::address_prefix()
□ Step 4: In protocol handshake, use NetworkType::magic_bytes()
```

**Effort:** 30 minutes  
**Priority:** LOW (cleanup/consistency)

---

## Category 3: SHOULD DELETE (Low Value / Superseded)

### 3.1 PeerManager Module
**File:** `src/peer_manager.rs`  
**Items:** All constants and methods

**Reason:** Fully superseded by `PeerConnectionRegistry` which handles:
- Peer tracking (via DashMap)
- Connection state (via ConnectionState)
- Message routing (via send_to_peer)
- Peer discovery (via should_connect_to logic)

**Verification:**
```
□ Confirm no imports of peer_manager in other files (except mod declaration)
□ Confirm PeerConnectionRegistry handles all peer management
□ Delete file and remove from mod.rs
```

**Effort:** 15 minutes  
**Priority:** LOW (code cleanup)

---

### 3.2 PeerDiscovery Struct
**File:** `src/network/peer_discovery.rs`  
**Items:** `PeerDiscovery`, `DiscoveredPeer`, `fetch_peers_with_fallback()`

**Reason:** Discovery is handled inline in main.rs and client.rs using the API directly.

**Options:**
- **Option A:** Delete entire file (15 min)
- **Option B:** Integrate fetch_peers_with_fallback into client.rs for cleaner code (30 min)

**Priority:** LOW

---

### 3.3 Alternative Network Client
**File:** `src/network/client.rs`  
**Items:** `NetworkClient`, `spawn_connection_task`, `maintain_peer_connection`

**Reason:** Connection management is done directly in main.rs with more control.

**Options:**
- **Option A:** Delete (may break things, verify first)
- **Option B:** Keep as scaffolding for future refactor
- **Option C:** Refactor main.rs to use NetworkClient (large effort)

**Recommendation:** Keep for now, mark with comment explaining it's alternative impl.

**Priority:** LOW

---

### 3.4 Alternative RPC Implementation
**File:** `src/rpc/server.rs`, `src/rpc/handler.rs`  
**Items:** `RpcServer`, `RpcHandler`, `RpcRequest`, `RpcResponse`, `RpcError`

**Reason:** RPC is handled via axum in main.rs with different structure.

**Options:**
- **Option A:** Delete both files
- **Option B:** Keep as alternative implementation option

**Recommendation:** Keep - provides a simpler RPC structure if axum is ever removed.

**Priority:** LOW

---

### 3.5 Dedup Filter (Alternative)
**File:** `src/network/dedup_filter.rs`  
**Items:** `BloomFilter`, `DeduplicationFilter`

**Reason:** Alternative to current inline deduplication. Current code uses HashSet.

**Options:**
- **Option A:** Delete
- **Option B:** Integrate for memory-efficient deduplication

**Recommendation:** Keep - Bloom filter is more memory efficient for high-volume dedup.

**Priority:** LOW

---

### 3.6 Config Utilities
**File:** `src/config.rs`  
**Items:** `get_data_dir()`, `get_network_data_dir()`, `load_from_file()`, `default()`, `save_to_file()`

**Reason:** Config is currently loaded inline in main.rs.

**Options:**
- **Option A:** Wire these to main.rs for cleaner config handling
- **Option B:** Keep for future CLI improvements

**Recommendation:** Keep for future use.

**Priority:** LOW

---

### 3.7 Type Alias
**File:** `src/block/consensus.rs`  
**Item:** `type DeterministicConsensus = AvalancheBlockConsensus`

**Reason:** Unused type alias, only `AvalancheBlockConsensus` is used directly.

**Action:** Delete line 55.

**Effort:** 1 minute  
**Priority:** LOW

---

### 3.8 Storage Alternative
**File:** `src/storage.rs`  
**Item:** `SledUtxoStorage::new()`

**File:** `src/utxo_manager.rs`  
**Item:** `new_with_storage()`

**Reason:** Alternative storage constructors not used.

**Recommendation:** Keep for future storage backend options.

**Priority:** LOW

---

## Implementation Order

### Phase 1: Quick Wins (30 minutes total)
1. ✅ Delete `DeterministicConsensus` type alias (1 min)
2. Add `#[allow(dead_code)]` comments for false positives (5 min)
3. Delete `peer_manager.rs` after verification (15 min)

### Phase 2: Security Integrations (1 hour total)
4. Wire `IPBlacklist` to server.rs (30 min)
5. Wire `RateLimiter` to server.rs (20 min)

### Phase 3: Feature Integrations (2-3 hours total)
6. Wire `HeartbeatAttestationSystem` properly (1 hour)
7. Wire `NetworkType` methods to config (30 min)
8. Wire `ECVRF` to block sortition (2 hours) - OPTIONAL for now

### Phase 4: Cleanup (30 minutes)
9. Decide on PeerDiscovery (keep/delete)
10. Add comments to alternative implementations explaining their purpose

---

## Verification After Each Change

```bash
cargo check   # No new errors
cargo clippy  # Check for new warnings  
cargo test    # All tests pass
cargo fmt     # Code formatted
```

---

## Expected Results

| Metric | Before | After Phase 1 | After Phase 2 | After All |
|--------|--------|---------------|---------------|-----------|
| Dead code warnings | ~51 | ~45 | ~35 | ~20 |
| Security features | Partial | Partial | Full | Full |
| Code clarity | Good | Better | Better | Best |

---

## Notes

- Items marked "Keep for future use" should have comments explaining their purpose
- Alternative implementations provide flexibility for future refactoring
- ECVRF integration is optional but improves security significantly
- False positives are due to Rust checking lib vs bin separately

---

*This plan addresses all 51 dead code warnings systematically.*
