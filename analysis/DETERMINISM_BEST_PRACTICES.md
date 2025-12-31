# Best Practices for Deterministic Blockchain Operations

## Genesis Block Generation

### Critical Requirements for Determinism

All nodes MUST generate identical genesis blocks. The hash depends on:

1. **Fixed Timestamp** (from template)
   - Testnet: `1764547200` (2025-12-01 00:00:00 UTC)
   - Mainnet: `1767225600` (2026-01-01 00:00:00 UTC)
   - Source: `genesis.{testnet,mainnet}.json`

2. **Sorted Masternode List**
   ```rust
   // ALWAYS sort before genesis generation
   masternodes.sort_by(|a, b| a.address.cmp(&b.address));
   ```

3. **Deterministic Leader Selection**
   ```rust
   // Leader is first masternode after sorting
   let leader = masternodes.first().map(|mn| mn.address.clone());
   ```

4. **Consistent Tier Counts**
   - Count masternodes by tier from the sorted list
   - Order doesn't affect counts, but list must be complete

5. **Deterministic Reward Distribution**
   - Rewards calculated proportionally by tier weight
   - Rewards maintain sorted order (by address)
   - Last masternode (alphabetically) gets remainder

### Common Pitfalls

❌ **DON'T**:
- Pass unsorted masternode lists to genesis generation
- Generate genesis on multiple nodes simultaneously
- Modify timestamps or leader manually
- Include different masternode sets on different nodes

✅ **DO**:
- Designate ONE genesis leader node
- Sort masternodes by address before generation
- Use network template for fixed parameters
- Coordinate genesis creation timing

## Transaction Ordering

### For Merkle Root Calculation

Transactions affect the `merkle_root` in the block header. For determinism:

1. **Coinbase Transaction**
   - Always first in block
   - Has empty inputs and outputs for genesis
   - Timestamp matches genesis timestamp

2. **User Transactions**
   - Should be sorted by some deterministic criteria
   - Common: Sort by TXID (transaction hash)
   - Alternative: Sort by fee (descending), then TXID

   ```rust
   // Sort transactions deterministically
   transactions.sort_by(|a, b| {
       // Coinbase always first
       if a.inputs.is_empty() { return std::cmp::Ordering::Less; }
       if b.inputs.is_empty() { return std::cmp::Ordering::Greater; }
       // Then by txid
       a.txid().cmp(&b.txid())
   });
   ```

3. **Merkle Root Calculation**
   ```rust
   let hashes: Vec<Hash256> = txs.iter().map(|tx| tx.txid()).collect();
   let merkle_root = build_merkle_root(hashes);
   ```
   - Order of `hashes` directly affects merkle root
   - Must use same transaction ordering on all nodes

### Serialization Considerations

When serializing blocks for network transmission:

```rust
// Block struct serialization includes ALL fields
#[derive(Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,           // Order matters!
    pub masternode_rewards: Vec<(String, u64)>,   // Order matters!
    pub time_attestations: Vec<TimeAttestation>,  // Order matters!
}
```

**All vectors must be sorted deterministically**:
- `transactions`: Coinbase first, then by TXID
- `masternode_rewards`: By address (alphabetically)
- `time_attestations`: By masternode address (alphabetically)

## Masternode Rewards

### Calculation Rules

1. **Weight-Based Distribution**
   ```rust
   Free tier:   1x weight
   Bronze tier: 2x weight  
   Silver tier: 3x weight
   Gold tier:   4x weight
   ```

2. **Proportional Shares**
   ```rust
   share = (total_reward × masternode_weight) / total_weight
   ```

3. **Remainder Handling**
   - Last masternode (alphabetically) gets remainder
   - Avoids rounding errors
   - Ensures sum equals total reward exactly

4. **Sorting**
   - Input masternodes MUST be pre-sorted by address
   - Output rewards maintain sorted order
   - Debug builds assert sorted input

### Example

```rust
// 2 masternodes: Free + Bronze
// Total weight: 1 + 2 = 3
// Total reward: 10,000,000,000 satoshis

let mn1 = GenesisMasternode {
    address: "TIME0aaa",  // Alphabetically first
    tier: MasternodeTier::Free,  // 1x weight
};

let mn2 = GenesisMasternode {
    address: "TIME0bbb",  // Alphabetically last
    tier: MasternodeTier::Bronze,  // 2x weight
};

// After sorting by address (already sorted in this case):
// mn1 share = (10B × 1) / 3 = 3,333,333,333
// mn2 share = 10B - 3,333,333,333 = 6,666,666,667 (gets remainder)

// Result:
masternode_rewards = vec![
    ("TIME0aaa", 3_333_333_333),
    ("TIME0bbb", 6_666_666_667),
];
```

## IP Address Handling

### Masternode Addresses

Masternode addresses should be:
1. **Normalized format**: IP only, no port
   ```rust
   let ip_only = address.split(':').next().unwrap_or(address);
   ```

2. **Consistent across network**:
   - Use external/public IP, not internal
   - Strip port numbers before sorting
   - Use same address in announcements and genesis

3. **Sorted alphabetically** (as strings):
   ```rust
   masternodes.sort_by(|a, b| a.address.cmp(&b.address));
   ```

### Port Handling

- Default port: 24100 (testnet), 24101 (mainnet)
- Store IP without port in masternode address
- Add port when connecting: `format!("{}:{}", address, port)`

## Time Attestations

For deterministic attestation roots:

1. **Sort by masternode address**
   ```rust
   time_attestations.sort_by(|a, b| 
       a.masternode_address.cmp(&b.masternode_address)
   );
   ```

2. **Merkle root calculation**
   ```rust
   let hashes: Vec<Hash256> = attestations.iter().map(|att| {
       let mut hasher = Sha256::new();
       hasher.update(att.masternode_address.as_bytes());
       hasher.update(att.sequence_number.to_le_bytes());
       hasher.update(att.heartbeat_timestamp.to_le_bytes());
       hasher.finalize().into()
   }).collect();
   
   let attestation_root = build_merkle_root(hashes);
   ```

3. **Witness records within attestations**
   - Also sort by witness address
   - Ensures deterministic attestation hashing

## Testing Determinism

### Unit Tests

```rust
#[test]
fn test_deterministic_generation() {
    let mut input1 = create_test_data();
    let mut input2 = create_test_data_different_order();
    
    // Sort both
    input1.sort_by(|a, b| a.key.cmp(&b.key));
    input2.sort_by(|a, b| a.key.cmp(&b.key));
    
    let result1 = generate(input1);
    let result2 = generate(input2);
    
    // Should be identical
    assert_eq!(result1.hash(), result2.hash());
    assert_eq!(result1, result2);
}
```

### Integration Tests

```bash
# Generate genesis on multiple nodes with same input
node1: cargo run -- genesis --masternodes "1.2.3.4,5.6.7.8"
node2: cargo run -- genesis --masternodes "5.6.7.8,1.2.3.4"

# Compare hashes (should match)
node1: grep "Genesis hash" /var/log/timed.log
node2: grep "Genesis hash" /var/log/timed.log
```

## Debugging Non-Determinism

If genesis blocks don't match:

1. **Check logs for detailed comparison**
   ```bash
   journalctl -u timed | grep "Genesis block mismatch" -A 20
   ```

2. **Compare components**:
   - Timestamp: Should match template exactly
   - Previous hash: Should be all zeros
   - Merkle root: Check transaction ordering
   - Leader: Check masternode sorting
   - Tier counts: Verify masternode list completeness
   - Masternode rewards: Check sorting and calculation

3. **Verify inputs**:
   ```rust
   // Log sorted masternodes
   for mn in &masternodes {
       tracing::info!("MN: {} (tier: {:?})", mn.address, mn.tier);
   }
   
   // Verify sorting
   for i in 1..masternodes.len() {
       assert!(masternodes[i-1].address <= masternodes[i].address);
   }
   ```

4. **Compare serialization**:
   ```rust
   let bytes1 = bincode::serialize(&block1)?;
   let bytes2 = bincode::serialize(&block2)?;
   
   if bytes1 != bytes2 {
       // Find first difference
       for (i, (b1, b2)) in bytes1.iter().zip(bytes2.iter()).enumerate() {
           if b1 != b2 {
               tracing::error!("First difference at byte {}: {} vs {}", i, b1, b2);
               break;
           }
       }
   }
   ```

## Summary

**Golden Rules for Determinism:**

1. ✅ Always sort by address (masternodes, rewards, attestations)
2. ✅ Use fixed timestamps from templates
3. ✅ Derive leader from sorted masternode list
4. ✅ Coordinate genesis creation (one leader node)
5. ✅ Normalize IP addresses (strip ports)
6. ✅ Sort transactions deterministically
7. ✅ Test with different input orders
8. ✅ Log detailed comparisons on mismatch
9. ✅ Use debug assertions to catch errors early
10. ✅ Document sorting requirements in code comments
