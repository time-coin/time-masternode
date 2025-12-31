# TimeCoin Cryptography Design

## Question: Why ECVRF instead of plain Ed25519?

**Short Answer**: Ed25519 is for digital signatures (signing/verification). ECVRF is for verifiable random functions (deterministic randomness with proofs). You can use both—they serve different purposes.

---

## Crypto Primitives Used in TimeCoin

| Primitive | Purpose | Implementation | Where Used |
|-----------|---------|-----------------|------------|
| **Ed25519** | Digital signatures | `ed25519_dalek` crate | Transaction signing, vote attestation |
| **BLAKE3** | Cryptographic hashing | `blake3` crate | Block hashing, tx identification |
| **ECVRF** | Verifiable random selection | RFC 9381 (Edwards25519) | Leader election (TSDC) |

---

## Ed25519 (Signing)

### What it does
- **Signs transactions** with a private key
- **Verifies signatures** with a public key
- Provides **non-repudiation**: Signer cannot deny signing

### How TimeCoin uses it
```rust
// Sign a transaction
let signing_key = SigningKey::generate();
let signature = signing_key.sign(transaction_bytes);

// Verify the signature
let verifying_key = signing_key.verifying_key();
verifying_key.verify(transaction_bytes, &signature)?;
```

### Example: Transaction Signing
```
Transaction → serialize → Ed25519 sign → Signature attached to TX
```

### Limitations
- **Not random**: Same input always produces same output (deterministic)
- **Not secret**: Signature is public (anyone can verify)
- **Not for randomness**: Cannot use for leader election or shuffling

---

## ECVRF (Verifiable Random Function)

### What it does
- **Deterministic**: Given same input, always produces same "random" output
- **Verifiable**: Anyone can prove the output is correct without knowing the secret
- **Unpredictable**: Output looks random (cannot predict without secret key)

### How it works
```
Secret Key + Input → ECVRF → [Random-looking Output + Proof]
                              ↓
                     Anyone can verify proof
                     without knowing secret key
```

### Why it's essential for consensus

**Problem**: How do we select block leaders fairly in a decentralized network?

#### ❌ **Option 1: Pure randomness (impossible)**
```
Leader = random()  // ← How? No source of randomness everyone agrees on
```

#### ❌ **Option 2: Each node picks own random leader (breaks consensus)**
```
Node A: "Node X is leader"
Node B: "Node Y is leader"
// Conflict! No agreement
```

#### ✅ **Option 3: VRF-based leader election (Avalanche/TSDC approach)**
```
Leader = VRF(secret_key, prev_block_hash, slot_number)

// Everyone can verify the same node is leader
// because they can check the VRF proof
// but nobody can predict who will be leader
// (even if they know the previous block hash and slot)
```

### TimeCoin TSDC Use Case

In `src/tsdc.rs`, we use VRF to deterministically select block producers:

```rust
// TSDC leader selection
fn select_leader_for_slot(validators: Vec<Validator>, slot: u64) -> Validator {
    // Each validator computes VRF(their_secret, prev_hash, slot)
    // The validator with best (lowest) VRF output becomes leader
    
    let mut best_validator = None;
    let mut best_vrf_output = [255u8; 32]; // Worst possible
    
    for validator in validators {
        let vrf_input = hash(prev_block_hash || slot);
        let vrf_output = ecvrf_evaluate(&validator.secret_key, vrf_input);
        
        if vrf_output < best_vrf_output {
            best_vrf_output = vrf_output;
            best_validator = Some(validator);
        }
    }
    
    best_validator.unwrap()
}
```

**Why this works**:
1. **Deterministic**: Same input always produces same output
2. **Fair**: All validators independently compute, get same result
3. **Unpredictable**: Cannot predict next leader without secret key
4. **Verifiable**: Anyone can prove the leader was selected correctly

---

## Can You Use Just Ed25519?

### ❌ No, for these reasons:

#### 1. **Different cryptographic properties**
```rust
// Ed25519: Signature
ed25519_sign(message, secret_key) → Signature
// Result: Same signature for same (message, key) combo
//         But signatures are not "random-looking"
//         And result is different for different messages

// ECVRF: Random function
ecvrf_eval(message, secret_key) → Random-looking output
// Result: Can be used for fair randomness
//         Deterministic but appears random
```

#### 2. **Ed25519 outputs are not suitable for leader selection**
```rust
// If we tried to use Ed25519 as randomness:
let seed = ed25519_sign(prev_block_hash, validator_key);
// ❌ Problem: Signature structure makes poor randomness source
// ❌ Not designed for this use case
// ❌ No standard way to compare signatures for ordering
```

#### 3. **VRF proof is not a signature**
```rust
// VRF includes proof of correct computation:
ecvrf_output = {
    value: [32 bytes random-looking data],
    proof: [proof of correct computation]
}
// This proof is different from a signature
// It proves "I computed this correctly" not "I approve this"
```

---

## Current TimeCoin Setup

### Implemented
- ✅ **Ed25519** for transaction signing (`ed25519_dalek` crate)
- ✅ **BLAKE3** for block hashing
- ✅ **TSDC parameter**: VRF needed for leader selection (spec §9.5)

### To Implement
- 📋 **ECVRF (RFC 9381)**: Full VRF implementation for TSDC
  - Currently TSDC uses placeholder VRF logic
  - Needs real RFC 9381 Edwards25519 implementation
  - Consider: `vrf` crate or `zcash_vrf` for production

### Why separate implementations
```
Ed25519 (Signature):
├─ Core library: ed25519_dalek
├─ Used for: Transaction signing, vote authentication
└─ Output: 64-byte signature

ECVRF (Random Function):
├─ Core library: RFC 9381 compatible
├─ Used for: Leader election, validator sampling
└─ Output: 32-byte value + proof
```

---

## Production Recommendation

### Crypto Stack

```yaml
Hash Function:
  Algorithm: BLAKE3-256
  Why: Modern, fast, parallel-friendly
  Crate: blake3

Digital Signatures:
  Algorithm: Ed25519 (RFC 8032)
  Why: Fast, well-studied, no side channels
  Crate: ed25519_dalek

Verifiable Random Function:
  Algorithm: ECVRF-Edwards25519-SHA512-TAI (RFC 9381)
  Why: Standards-compliant, works with Ed25519 keys
  Crate: zcash-vrf or custom RFC 9381 implementation
```

### Security Properties Achieved

| Property | Mechanism |
|----------|-----------|
| **Authenticity** | Ed25519 digital signatures |
| **Integrity** | BLAKE3 hash commitments |
| **Fairness** | ECVRF deterministic randomness |
| **Finality** | Avalanche consensus + VFP |

---

## Implementation Checklist

- [x] Ed25519 signing/verification (transactions)
- [x] BLAKE3 block hashing
- [ ] Full ECVRF implementation (RFC 9381)
  - [ ] VRF evaluation
  - [ ] VRF proof generation
  - [ ] VRF proof verification
  - [ ] Integration with TSDC leader election

---

## References

| Document | Purpose |
|----------|---------|
| RFC 8032 | Ed25519 digital signature algorithm |
| RFC 9381 | ECVRF specification (Edwards25519-SHA512-TAI) |
| Zcash specs | VRF implementation reference |
| TIME-COIN Protocol v6 §4 | Cryptographic primitives |

---

## Summary

**Ed25519 alone is insufficient** because:
1. It's a signature algorithm, not a randomness source
2. TSDC requires deterministic but unpredictable leader selection
3. VRF solves the "random but verifiable" problem

**Combined approach**:
- **Ed25519**: Sign all messages (transactions, votes, proofs)
- **BLAKE3**: Hash all data (blocks, transactions, commitments)
- **ECVRF**: Select leaders fairly and verifiably

This gives you **authentication + integrity + fairness**—the three pillars of distributed consensus.

---

**Last Updated**: 2025-12-23  
**Status**: Ready for VRF implementation phase
