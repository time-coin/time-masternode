# Genesis Block Mismatch Fix - Deterministic Generation

## Problem
Node stuck at height 0, unable to sync with peers. All blocks being skipped with warnings:
```
⚠️  [Outbound] All 101 blocks skipped from 50.28.104.50:57542 - potential fork at height 0 (failed attempts: 10)
```

## Root Cause
**Non-deterministic genesis block generation**. Different nodes generate different genesis blocks even with the same set of masternodes, causing hash mismatches.

### Why Genesis Blocks Were Non-Deterministic

The genesis block hash is calculated from the `BlockHeader`, which includes:
```rust
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub previous_hash: Hash256,
    pub merkle_root: Hash256,          // ← Calculated from transactions
    pub timestamp: i64,                 // ← From genesis template
    pub block_reward: u64,              // ← From genesis template
    pub leader: String,                 // ← From sorted masternode list
    pub attestation_root: Hash256,
    pub masternode_tiers: MasternodeTierCounts,  // ← Counts by tier
}
```

Although `masternode_rewards` are not directly part of the hash, they affect the overall block comparison when nodes validate received blocks via serialization.

**Issues that caused non-determinism:**
1. ✅ **Already Fixed**: Masternodes sorted by address before generation (blockchain.rs:401)
2. ✅ **Already Fixed**: Leader derived from sorted list (blockchain.rs:404)
3. ✅ **Now Fixed**: Masternode rewards maintain sorted order from input
4. ✅ **Now Fixed**: Debug assertion validates sorted input

## Solution Applied

### 1. Enhanced Genesis Mismatch Logging (`src/blockchain.rs`)
Added detailed diagnostics showing exactly what differs between genesis blocks:
```rust
tracing::error!(
    "🚫 Genesis block mismatch detected!\n\
     Our genesis: {}\n\
     - timestamp: {}\n\
     - merkle_root: {}\n\
     - masternodes: {}\n\
     Peer genesis: {}\n\
     - timestamp: {}\n\
     - merkle_root: {}\n\
     - masternodes: {}",
    // ... full comparison
);
```

### 2. Ensured Deterministic Reward Distribution (`src/block/genesis.rs`)
- Added documentation requiring pre-sorted input
- Added debug assertion to validate sorted input
- Rewards maintain sorted order from sorted input
- Remainder handling: last masternode alphabetically gets rounding remainder

### 3. Added Comprehensive Tests
- `test_genesis_deterministic`: Verifies same input produces same hash
- `test_genesis_deterministic_with_sorting`: Verifies different input orders produce same result after sorting
- `test_tier_reward_distribution`: Now properly sorts input before testing

## Best Practices for Deterministic Genesis

### ✅ DO:
1. **Always sort masternodes by address** before calling `generate_with_masternodes()`
   ```rust
   genesis_masternodes.sort_by(|a, b| a.address.cmp(&b.address));
   ```

2. **Use consistent leader selection** - derive from sorted list
   ```rust
   let leader = genesis_masternodes.first().map(|mn| mn.address.clone());
   ```

3. **Use fixed genesis timestamp** from template (network-specific)
   - Testnet: 1764547200 (2025-12-01 00:00:00 UTC)
   - Mainnet: 1767225600 (2026-01-01 00:00:00 UTC)

4. **Ensure all nodes use same network type** (testnet vs mainnet)

5. **Coordinate genesis creation** - designate one "genesis leader" node

### ❌ DON'T:
1. Don't pass unsorted masternode lists to genesis generation
2. Don't override leader with arbitrary values
3. Don't modify genesis timestamp
4. Don't create genesis independently on multiple nodes simultaneously

## Genesis Block Components That MUST Match

For two genesis blocks to have the same hash, ALL of these must be identical:

| Component | How Determined | Critical? |
|-----------|---------------|-----------|
| `timestamp` | Network template | ✅ YES - Part of header hash |
| `previous_hash` | Always 0x00...00 | ✅ YES - Part of header hash |
| `merkle_root` | Coinbase tx hash | ✅ YES - Part of header hash |
| `block_reward` | Network template | ✅ YES - Part of header hash |
| `leader` | First sorted masternode | ✅ YES - Part of header hash |
| `masternode_tiers` | Count by tier | ✅ YES - Part of header hash |
| `masternode_rewards` | Sorted, weight-based | ⚠️ Not in hash, but affects block comparison |
| `transactions` | Coinbase only | ⚠️ Affects merkle_root |

## Resolution Steps for Affected Nodes

### Option A: Delete Local Genesis and Resync (Recommended)
```bash
sudo systemctl stop timed
sudo mv /var/lib/timed/blockchain.db /var/lib/timed/blockchain.db.backup
sudo systemctl start timed
```

The node will sync the correct genesis from network peers.

### Option B: Coordinate Network-Wide Genesis Reset
If all nodes have mismatched genesis:
1. Stop all nodes
2. Clear blockchain data on all nodes  
3. Designate ONE genesis leader node
4. Leader node generates genesis
5. Other nodes sync from leader

### Verify Genesis After Sync
```bash
# Check logs for genesis creation/sync
sudo journalctl -u timed | grep -i genesis

# Verify current height
curl http://localhost:24100/chain/height

# Check for mismatch errors
sudo journalctl -u timed | grep "Genesis block mismatch"
```

## Testing Determinism

Run the test suite to verify deterministic generation:
```bash
cargo test --lib genesis -- --nocapture
```

All 5 tests should pass:
- ✅ `test_genesis_with_masternodes`
- ✅ `test_genesis_deterministic`  
- ✅ `test_genesis_deterministic_with_sorting`
- ✅ `test_genesis_verification`
- ✅ `test_tier_reward_distribution`

## Files Modified
1. `src/blockchain.rs` (lines 1300-1347): Enhanced genesis mismatch logging
2. `src/block/genesis.rs`:
   - Lines 181-198: Added sorted input validation
   - Lines 250-283: Documented reward calculation requirements
   - Lines 401-502: Added determinism tests and fixed existing tests

## Prevention Going Forward

The code now has safeguards:
- **Debug assertion**: Panics if unsorted input detected (debug builds)
- **Comprehensive tests**: Verify determinism with various input orders
- **Clear documentation**: Function comments specify requirements
- **Detailed logging**: Genesis mismatches show full comparison

For production, always:
1. Use the same genesis template file across all nodes
2. Coordinate genesis creation with a designated leader
3. Bootstrap new nodes from existing network
4. Monitor logs for "Genesis block mismatch" errors

