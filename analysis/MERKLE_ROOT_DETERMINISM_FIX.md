# Merkle Root Determinism Fix

## Problem
Network synchronization was failing with merkle root mismatches. The logs showed:
- Every node computed a different merkle root for the same block
- Block 1 had different computed roots from different nodes:
  - Node A: `ff623eeee1a9bd02` (header: `d0c9270274d46ace`)
  - Node B: `2d7efa2bb5cad8ec` (header: `d0c9270274d46ace`)  
  - Node C: `5ee1895e91efbc38` (header: `440d85f4e171d815`)

## Root Cause
The `Block::compute_merkle_root()` function was non-deterministic because:
1. It used JSON serialization to hash transactions
2. JSON field ordering is not guaranteed to be consistent
3. Different nodes could serialize the same transaction with different field orders
4. This resulted in different hashes for identical transactions

## Solution
**Sort transactions by txid before computing merkle root:**

```rust
pub fn compute_merkle_root(&self) -> Hash256 {
    if self.transactions.is_empty() {
        return [0u8; 32];
    }

    // Hash each transaction using txid() for consistency
    // Sort by txid to ensure deterministic ordering
    let mut hashes: Vec<(Hash256, Hash256)> = self
        .transactions
        .iter()
        .map(|tx| {
            let txid = tx.txid();
            (txid, txid) // (sort_key, hash)
        })
        .collect();
    
    // Sort by txid to ensure deterministic merkle root
    hashes.sort_by(|a, b| a.0.cmp(&b.0));
    
    // Extract hashes and build merkle tree
    let mut hashes: Vec<Hash256> = hashes.into_iter().map(|(_, hash)| hash).collect();
    // ... merkle tree construction ...
}
```

## Benefits
1. **Deterministic**: All nodes compute identical merkle roots for the same transactions
2. **Canonical**: Transaction order is always sorted by txid
3. **Network-safe**: Works correctly regardless of how blocks are transmitted (JSON/bincode)
4. **Verifiable**: Any node can independently verify merkle roots

## Testing
- Code compiles without warnings
- Existing tests pass
- Ready for deployment

## Deployment Notes
After deploying this fix:
1. Nodes should restart and resync from genesis
2. All nodes will now compute identical merkle roots
3. Network synchronization should proceed normally
4. Monitor logs for "merkle root mismatch" errors (should be eliminated)
