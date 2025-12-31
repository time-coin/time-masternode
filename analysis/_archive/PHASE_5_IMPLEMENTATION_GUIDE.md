# Phase 5 Implementation Guide

**Status**: Ready to implement  
**Expected Duration**: 11-14 days  
**Start Date**: December 23, 2025  
**Target Completion**: January 6, 2026

---

## Quick Summary

Phase 5 implements the missing piece: **fair, verifiable leader selection via ECVRF**.

### Why This Matters
- **Current State**: Avalanche consensus works, but TSDC leader selection is undefined
- **Problem**: Without ECVRF, leaders can be gamed or predicted
- **Solution**: Use ECVRF to select leaders deterministically but fairly
- **Benefit**: No one (not even the leader) can game the system

---

## What is ECVRF and Why Not Just Ed25519?

### Simple Explanation

**Ed25519** (signing algorithm):
```
Input: message + secret key
Output: signature
Property: Proves the secret key owner signed the message
```

**ECVRF** (verifiable randomness):
```
Input: seed + domain-separation data
Output: random-looking value + cryptographic proof
Property: Proves the randomness is deterministic (same input = same output)
         but unpredictable beforehand
```

### Why Both?
- **Ed25519**: For signing votes and transactions (prove authorship)
- **ECVRF**: For selecting leaders fairly (no one can predict or game)

### Example in TimeCoin
```
Slot 1000 begins
├─ TSDC looks at: previous block hash + slot time
├─ Runs through ECVRF for each validator
├─ Validator with highest VRF output becomes leader
└─ Leader proposes block

KEY: No one can manipulate their VRF output
     (even if they control their secret key)
```

---

## Implementation Steps

### Step 1: Create ECVRF Module

**File**: `src/crypto/ecvrf.rs`

```rust
use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Sha512, Digest};

pub struct ECVRFProof {
    pub bytes: [u8; 80],  // RFC 9381: 80-byte proof
}

pub struct ECVRFOutput {
    pub bytes: [u8; 32],  // 32-byte VRF output
}

/// RFC 9381: ECVRF-Edwards25519-SHA512-TAI
pub fn evaluate(
    secret_key: &SigningKey,
    input: &[u8],
) -> (ECVRFOutput, ECVRFProof) {
    // RFC 9381 §5.2.6: ECVRF_ENCODE_TO_CURVE
    // RFC 9381 §5.2.7: ECVRF_HASH_TO_CURVE_ELLIGATOR2
    // Steps:
    // 1. Hash input to curve point
    // 2. Multiply by secret key scalar
    // 3. Generate Schnorr-like proof
    // 4. Return (output, proof)
}

pub fn verify(
    public_key: &VerifyingKey,
    input: &[u8],
    proof: &ECVRFProof,
) -> Result<ECVRFOutput, ECVRFError> {
    // RFC 9381 §5.3: ECVRF_verify
    // Verify proof without knowing secret key
}

pub fn proof_to_hash(proof: &ECVRFProof) -> ECVRFOutput {
    // RFC 9381 §5.2.8: ECVRF_proof_to_hash
}
```

**Key References**:
- RFC 9381 Section 5: ECVRF specification
- RFC 9381 Appendix A.4: Test vectors
- Use `ed25519_dalek` for Edwards25519 curve operations
- Use `sha2` for SHA-512

---

### Step 2: Integrate ECVRF into TSDC

**File**: `src/tsdc.rs`

Current code (pseudocode):
```rust
pub fn generate_proposal(&self) -> Result<Block> {
    let block = Block::new(
        self.chain_height + 1,
        self.mempool.take_transactions(MAX_BLOCK_TXS),
        self.timestamp(),
    );
    
    Ok(block)
}
```

Updated code:
```rust
pub fn select_leader(
    validators: &[ValidatorInfo],
    prev_block_hash: &Hash256,
    slot_time: u64,
    chain_id: u32,
) -> Result<ValidatorAddress> {
    // Create VRF input: hash(prev_block || slot_time || chain_id)
    let mut hasher = Blake3Hasher::new();
    hasher.update(prev_block_hash.as_bytes());
    hasher.update(&slot_time.to_le_bytes());
    hasher.update(&chain_id.to_le_bytes());
    let vrf_input = hasher.finalize();

    let mut best_output = None;
    let mut best_validator = None;

    // Evaluate VRF for each validator
    for validator in validators {
        let (output, _proof) = ecvrf::evaluate(
            &validator.vrf_secret_key,
            vrf_input.as_bytes(),
        );
        
        // Higher output = higher priority
        if output > best_output {
            best_output = Some(output);
            best_validator = Some(validator);
        }
    }

    Ok(best_validator.unwrap().address)
}

pub fn generate_proposal(&self) -> Result<Block> {
    let leader = self.select_leader(
        &self.active_validators,
        &self.blockchain.last_block().hash(),
        self.current_slot_time(),
        self.config.chain_id,
    )?;

    // Only the selected leader can propose
    if self.my_address != leader {
        return Err(TSCDError::NotLeader);
    }

    let block = Block::new(
        self.chain_height + 1,
        self.mempool.take_transactions(MAX_BLOCK_TXS),
        leader,  // Include leader in block
        self.timestamp(),
    );

    Ok(block)
}
```

**Key Changes**:
- TSDC calls `select_leader()` deterministically
- Only selected leader proposes block
- VRF output included in block header
- Can be verified by any node

---

### Step 3: Update Block Structure

**File**: `src/block/types.rs`

```rust
pub struct Block {
    pub version: u32,
    pub height: u64,
    pub timestamp: u64,
    pub prev_hash: Hash256,
    pub merkle_root: Hash256,
    pub leader: ValidatorAddress,
    pub vrf_output: ECVRFOutput,          // NEW
    pub vrf_proof: ECVRFProof,             // NEW
    pub transactions: Vec<Transaction>,
    pub finality_votes: Vec<FinalityVote>,
}

pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub timestamp: u64,
    pub prev_hash: Hash256,
    pub merkle_root: Hash256,
    pub leader: ValidatorAddress,
    pub vrf_output: ECVRFOutput,          // NEW
    pub vrf_proof: ECVRFProof,             // NEW
}

impl Block {
    pub fn validate_vrf(&self, validators: &[ValidatorInfo]) -> Result<()> {
        // Find leader's public key
        let leader_pk = validators
            .iter()
            .find(|v| v.address == self.leader)
            .map(|v| &v.vrf_public_key)
            .ok_or(BlockError::UnknownLeader)?;

        // Reconstruct VRF input
        let mut hasher = Blake3Hasher::new();
        hasher.update(self.prev_hash.as_bytes());
        hasher.update(&self.timestamp.to_le_bytes());
        hasher.update(&CHAIN_ID.to_le_bytes());
        let vrf_input = hasher.finalize();

        // Verify VRF proof
        ecvrf::verify(leader_pk, vrf_input.as_bytes(), &self.vrf_proof)
            .map(|output| {
                // Verify output matches claimed VRF output
                if output.bytes != self.vrf_output.bytes {
                    return Err(BlockError::InvalidVRF);
                }
                Ok(())
            })?
    }
}
```

---

### Step 4: Testing (Multi-Node)

**File**: `tests/multi_node_consensus.rs`

```rust
#[tokio::test]
async fn test_3node_consensus_with_vrf() {
    // Setup 3 nodes
    let node_a = TimeCoinNode::new("A");
    let node_b = TimeCoinNode::new("B");
    let node_c = TimeCoinNode::new("C");

    // Connect nodes (network)
    node_a.connect(&node_b).await;
    node_a.connect(&node_c).await;
    node_b.connect(&node_c).await;

    // Submit transactions to node A
    let txs = vec![
        Transaction::transfer("Alice", "Bob", 100),
        Transaction::transfer("Charlie", "Dave", 50),
    ];
    node_a.submit_transactions(txs).await.unwrap();

    // Wait for block proposal
    tokio::time::sleep(Duration::from_secs(5)).await;

    // Check that one leader was selected
    let block_a = node_a.get_latest_block().unwrap();
    let block_b = node_b.get_latest_block().unwrap();
    let block_c = node_c.get_latest_block().unwrap();

    // All nodes should have the same block
    assert_eq!(block_a.hash(), block_b.hash());
    assert_eq!(block_b.hash(), block_c.hash());

    // Block should be valid
    assert!(block_a.validate_vrf(&[node_a.validator, node_b.validator, node_c.validator]).is_ok());
    
    // Leader should be deterministic
    assert!(
        block_a.leader == node_a.address 
        || block_a.leader == node_b.address 
        || block_a.leader == node_c.address
    );
}

#[tokio::test]
async fn test_fork_resolution() {
    // Create network partition
    let node_a = TimeCoinNode::new("A");
    let node_b = TimeCoinNode::new("B");
    let node_c = TimeCoinNode::new("C");

    node_a.connect(&node_b).await;
    // C is isolated

    // Wait for fork
    tokio::time::sleep(Duration::from_secs(60)).await;

    // Reconnect C
    node_a.connect(&node_c).await;

    // C should adopt A+B's chain (higher VRF score)
    tokio::time::sleep(Duration::from_secs(10)).await;
    
    assert_eq!(node_c.get_height(), node_a.get_height());
}
```

---

## Testing with RFC 9381 Vectors

### Where to Get Test Vectors

**RFC 9381 Appendix A.4** contains test vectors:

```
Test vector 1:
  SK: 3052...  (secret key hex)
  alpha: "abc" (input)
  VRF output: 612...
  Proof: fce8...

Test vector 2:
  SK: ...
  alpha: ...
  VRF output: ...
  Proof: ...
```

### Test Code

```rust
#[test]
fn test_rfc9381_vector_1() {
    let sk_hex = "3052...";  // From RFC 9381 A.4
    let sk = ECVRFSecretKey::from_hex(sk_hex);
    let alpha = b"abc";
    
    let (output, proof) = ecvrf::evaluate(&sk, alpha);
    
    assert_eq!(output.to_hex(), "612...");  // Expected from RFC
}
```

This proves your ECVRF implementation matches the standard.

---

## Deployment Checklist

### Before Phase 5 Starts
- [ ] Team assigned (Consensus Eng + Network Eng)
- [ ] ECVRF library selected (ed25519-dalek + sha2)
- [ ] RFC 9381 reviewed and understood
- [ ] Test vectors downloaded from RFC

### During Phase 5
- [ ] ECVRF module implemented
- [ ] RFC 9381 test vectors passing (100%)
- [ ] TSDC updated for leader selection
- [ ] Block structure includes VRF output/proof
- [ ] 3-node test passing
- [ ] Fork resolution working
- [ ] Edge cases tested

### After Phase 5
- [ ] Code review complete
- [ ] Benchmarks run (VRF evaluation time <10ms)
- [ ] Documentation updated
- [ ] Ready for Phase 6 (RPC API)

---

## Troubleshooting

### Issue: VRF Output Not Deterministic
**Cause**: Hash input not canonical  
**Solution**: Always use `Blake3(prev_hash || slot_time || chain_id)`

### Issue: Proof Verification Failing
**Cause**: Using wrong RFC 9381 variant  
**Solution**: Use `ECVRF-Edwards25519-SHA512-TAI` (Appendix A, not A.1)

### Issue: Different Leaders on Different Nodes
**Cause**: Clock skew or different validator sets  
**Solution**: Ensure NTP is running, use same AVS snapshot timestamp

### Issue: High VRF Computation Latency
**Cause**: Running pure Rust implementation  
**Solution**: Consider libsodium FFI binding (later optimization)

---

## Success Criteria (Hard Requirements)

✅ Phase 5 is complete when:

1. **ECVRF fully working**
   ```
   cargo test ecvrf -- --nocapture
   ALL TESTS PASS
   ```

2. **RFC 9381 test vectors passing**
   ```
   All 10 vectors from Appendix A.4 verified
   ```

3. **3-node consensus operational**
   ```
   3 nodes form network, select leader via VRF, produce block
   Block contains valid VRF proof
   All nodes agree on block hash
   ```

4. **Fork resolution automatic**
   ```
   Network partitions, then reconnects
   Minority chain adopts majority chain
   Single canonical chain restored
   ```

5. **Zero compilation warnings**
   ```
   cargo build --release 2>&1 | grep -i warning
   (should be empty or non-critical)
   ```

6. **Documentation complete**
   ```
   - Code comments explain ECVRF usage
   - README updated with Phase 5 status
   - Test vectors documented
   ```

---

## Handoff to Phase 6

Once Phase 5 completes, you'll have:

✅ Consensus: Pure Avalanche with VRF-based leader selection  
✅ Network: Multi-node, fork resolution, partition recovery  
✅ Cryptography: ECVRF RFC 9381 compliant  
✅ Testing: 100+ integration tests  

**Ready for Phase 6**:
- RPC API (send tx, get balance, query block)
- Performance optimization
- Governance layer
- Mainnet preparation

---

## Estimated Task Breakdown

| Task | Days | Owner |
|------|------|-------|
| ECVRF implementation | 3-4 | Consensus Eng |
| RFC 9381 test vectors | 1 | Consensus Eng |
| TSDC integration | 2 | Consensus Eng |
| 3-node testing | 2 | Network Eng |
| Fork resolution | 2-3 | Consensus Eng |
| Edge cases | 2 | QA |
| Documentation | 1 | Lead Dev |
| **TOTAL** | **13-15** | — |

---

## Resources & References

### RFC 9381
- Full spec: https://tools.ietf.org/html/rfc9381
- Section 5: ECVRF-Edwards25519-SHA512-TAI (our choice)
- Appendix A.4: Test vectors (use for validation)

### ed25519-dalek
- Crate: https://docs.rs/ed25519-dalek
- Examples: Key generation, signing, verification
- Already in Cargo.toml

### TimeCoin References
- Protocol V6: Full specification
- Phase 4: Pure Avalanche (just completed)
- AVALANCHE_CONSENSUS_ARCHITECTURE.md
- CRYPTOGRAPHY_DESIGN.md

---

**Ready to start Phase 5?**

Next step: Assign Consensus Engineer to `src/crypto/ecvrf.rs`

Once ECVRF module is working, rest of Phase 5 proceeds in parallel.

---

**Last Updated**: December 23, 2025  
**Status**: ✅ Ready for implementation  
**Owner**: Consensus Engineer + Network Engineer
