# Merkle Root Mismatch Fix - Block 3733

## Problem
All nodes were stuck at different heights, rejecting block 3733 and subsequent blocks due to merkle root validation failures:
- Error: `"Block 3733 merkle root mismatch: computed 737ba88a165c29be, header 8a9e29a738058fa5"`

## Root Cause
The blockchain had an **inconsistent hashing mechanism**:

1. **Network transmission**: Blocks are sent over the network using **JSON serialization** (`serde_json`)
2. **Merkle root computation**: Transaction hashes were computed using **bincode serialization**

This created a critical mismatch:
- When a node **produces** a block, it serializes transactions with bincode → computes merkle root
- When another node **receives** the block via JSON → deserializes with JSON → recomputes merkle root with bincode
- **Bincode produces different bytes after JSON round-trip** → different transaction hashes → different merkle roots
- Result: Receiving nodes reject the block as invalid

## Solution
Changed both transaction hashing and merkle root computation to use **canonical JSON serialization**:

### Files Modified:
1. **`src/block/types.rs`** - `Block::compute_merkle_root()`
   - Changed from `bincode::serialize(tx)` to `serde_json::to_string(tx)`
   - Ensures merkle root is computed consistently regardless of transmission method

2. **`src/types.rs`** - `Transaction::txid()`
   - Changed from `bincode::serialize(self)` to `serde_json::to_string(self)`
   - Ensures transaction IDs are consistent across JSON transmission

## Why This Works
- **JSON is the wire format**: All network messages use JSON serialization
- **Canonical representation**: JSON provides a consistent text representation across serialization/deserialization
- **Deterministic hashing**: Using the same format (JSON) for both transmission and hashing ensures consistent merkle roots

## Impact
- **Forward compatible**: All new blocks will have consistent merkle roots
- **Existing blocks**: Nodes may need to resync from genesis to recompute old block hashes with the new method
- **Network convergence**: All nodes will now agree on merkle roots for the same block data

## Verification
After deploying this fix:
1. Stop all nodes
2. Clear blockchain data (or let them resync from genesis)  
3. Restart nodes with the updated binary
4. Verify nodes can now sync past block 3733

## Technical Details
The fix changes the hashing from:
```rust
// OLD - Inconsistent after JSON transmission
let bytes = bincode::serialize(tx).unwrap_or_default();
Sha256::digest(&bytes).into()
```

To:
```rust
// NEW - Consistent with network format
let json = serde_json::to_string(tx).unwrap_or_default();
Sha256::digest(json.as_bytes()).into()
```

This ensures that `hash(original_tx) == hash(json_roundtrip(original_tx))`.
