# Why ECVRF? (Not Just Ed25519)

**Date:** December 23, 2025  
**Context:** Phase 5 Planning - ECVRF RFC 9381 Implementation Decision

---

## The Question

> "Can't we just use Ed25519 for everything instead of adding ECVRF?"

## The Short Answer

**No.** Ed25519 and ECVRF solve different problems:

| Feature | Ed25519 | ECVRF |
|---------|---------|-------|
| **Purpose** | Signature (proves ownership) | Random Function (proves fairness) |
| **Input** | Message + Private Key | Secret scalar + Message |
| **Output** | Signature | Random value + Proof |
| **Deterministic?** | Always same sig for same msg | YES - same input = same random output |
| **Publicly verifiable?** | Yes (via pubkey) | YES - without private key! |
| **Resistant to bias?** | No - attacker can pick msgs | YES - output non-malleable |

---

## Real-World Example

### Scenario: TIME Coin Block Leader Selection

**With ONLY Ed25519:**
```rust
// How we select the next block producer (WRONG)
let msg = format!("slot_{}", current_slot);
let signature = ed25519_sign(&validator_key, msg.as_bytes());
let leader_score = u64::from_le_bytes(&signature[0..8]);
let leader = validators[leader_score % validators.len()];
```

**Problem:** Attacker validates multiple messages until getting a signature with high score, then uses that slot.

---

**With ECVRF (CORRECT):**
```rust
// TSDC Sortition with ECVRF
let slot_hash = hash(prev_block_hash || current_slot);
let (vrf_output, vrf_proof) = ecvrf_prove(&validator_sk, &slot_hash);
let vrf_score = u64::from_le_bytes(&vrf_output[0..8]);
let leader = validators[vrf_score % validators.len()];
// Broadcast (leader_id, vrf_output, vrf_proof) to network
// Everyone verifies: vrf_verify(&validator_pk, &slot_hash, vrf_proof) → vrf_output
```

**Benefit:** 
- Validator cannot change their slot assignment by trying multiple times
- Output is deterministic (same slot hash = same VRF output always)
- Network can verify fairness without validator's private key
- Prevents MEV via "wait for favorable slot" gaming

---

## TIME Coin Use Cases for ECVRF

### 1. **TSDC Block Leader Selection** (Most Critical)

**Without ECVRF:**
- Any validator could claim "I'm the leader" this slot
- Honest validators don't know who should produce the block
- Consensus requires asking all validators (O(n²) messaging)

**With ECVRF:**
- Leader deterministically selected via VRF of previous block hash
- All validators can compute: "Block should be from validator_5"
- No ambiguity, no messaging for leader selection
- Reduces O(n²) → O(n) communication

### 2. **Avalanche Validator Sampling**

**Avalanche consensus rule:** Each node samples k validators randomly each round

**Without ECVRF:**
```rust
// WEAK - attacker can influence by controlling random seed
let seed = timestamp % 256;
let sample = random_sample_from_seed(validators, seed);
```

**With ECVRF:**
```rust
// STRONG - attacker cannot bias sample without breaking ECVRF
let seed = vrf_prove(private_key, prev_block_hash);
let sample = hash_to_indices(seed.output, validators.len());
// Attacker would need to break ECVRF to bias this
```

### 3. **Fork Resolution**

**When two blocks compete for same slot:**

**Without ECVRF:**
```rust
// Arbitrary: whoever broadcasts first wins
let canonical = if block_a.timestamp < block_b.timestamp {
    block_a
} else {
    block_b
};
// Problem: Network timing varies, no consensus
```

**With ECVRF:**
```rust
// Deterministic: compute cumulative VRF score per chain
let score_a: u64 = block_a.vrf_output.as_u64();
let score_b: u64 = block_b.vrf_output.as_u64();
let canonical = if score_a > score_b { block_a } else { block_b };
// All nodes agree: highest VRF score wins
```

---

## Cryptographic Properties

### ECVRF = VRF (Verifiable Random Function)

**Definition:** A function F where:

1. **Deterministic:** F(sk, x) always returns same y for same input
2. **Verifiable:** Anyone with pk can prove y = F(sk, x) without knowing sk
3. **Pseudorandom:** Output y looks random (computationally indistinguishable from random)
4. **Collision-resistant:** Hard to find two inputs with same output
5. **Non-interactive:** Single round operation

### Ed25519 Cannot Provide VRF

Ed25519 signature s = sign(sk, msg) **includes randomness** in generation:
- Same (sk, msg) might produce different s each time (if not deterministic)
- Even deterministic Ed25519: output is 512 bits of signature, not designed as random function
- Attacker can forge messages to search for "good" signatures

ECVRF explicitly designed to provide this property.

---

## RFC 9381: ECVRF-Edwards25519-SHA512-TAI

**We chose this specific variant because:**

| Aspect | Choice | Why |
|--------|--------|-----|
| **Elliptic Curve** | Edwards25519 | Same as Ed25519 (audited, fast, constant-time) |
| **Hash Function** | SHA512 | NIST standard, widely audited |
| **Encoding** | TAI (Test Applications Implementors) | Simpler, more interoperable |

**RFC 9381 provides:**
- ✅ Complete test vectors (can verify implementation)
- ✅ Formal security proofs
- ✅ Multiple encoding options (compatibility)
- ✅ Reference implementation available
- ✅ Standardized (IETF standard track)

---

## Implementation Roadmap

### Phase 5 Tasks

**1. Core ECVRF (src/crypto/ecvrf.rs)**
```rust
// Proof generation
pub fn ecvrf_prove(
    secret_scalar: &[u8; 32],
    input: &[u8; 32],
) -> Result<([u8; 32], Vec<u8>), ECVRFError>

// Proof verification (no secret key needed)
pub fn ecvrf_verify(
    public_key: &PublicKey,
    input: &[u8; 32],
    proof: &[u8],
) -> Result<[u8; 32], ECVRFError>  // Returns VRF output if valid
```

**2. TSDC Leader Sortition (src/tsdc.rs)**
```rust
impl TSCDBlock {
    pub fn sortition_leader(
        prev_block_hash: &[u8; 32],
        slot_time: u64,
        validators: &[Validator],
    ) -> String {
        // Each validator computes their VRF score
        for validator in validators {
            let input = hash(prev_block_hash || slot_time.to_le_bytes());
            let (vrf_output, _proof) = ecvrf_prove(&validator.sk, &input)?;
            let score = u64::from_le_bytes(&vrf_output[0..8]);
            
            // Highest score wins (or lowest score in sortition lottery)
            // All validators compute same result -> deterministic leader
        }
    }
}
```

**3. Multi-node Testing (tests/integration/)**
- 3-node network: verify same leader elected same slot
- 5-node with network delay: verify leader deterministic despite latency
- 10-node stress test: 1000 slots, verify no leader conflicts
- Fork resolution: two blocks same slot, verify VRF score resolves

---

## Build Impact

**Current (Phase 4):**
```
Dependencies:
✅ ed25519-dalek (Ed25519 signatures)
✅ curve25519-dalek (Elliptic curve operations)
✅ sha2 (SHA256/512)
```

**Phase 5 additions:**
```
Dependencies (likely):
+ curve25519-dalek (already have)
+ sha2 (already have)
+ getrandom (randomness, for testing only)
+ Maybe: ecdlp library if implementing Elligator2
  OR: Use existing RFC 9381 implementation crate (if high quality exists)
```

**Size:** +5-15 KB binary size (ECVRF code ~500 lines Rust)

**Performance:** 
- VRF prove: ~50 scalar multiplications (~10-50 ms on modern CPU)
- VRF verify: ~40 scalar multiplications (~5-30 ms)
- Acceptable: runs once per slot (10 minute slots = negligible impact)

---

## Alternatives Considered

| Alternative | Reason Rejected |
|-------------|-----------------|
| **Plain Ed25519 only** | Cannot provide pseudorandom output; attacker can bias via message choice |
| **SHA3(secrets+time)** | Not cryptographically proven for leader sortition; time-based bias |
| **Libra's VRF** | Proprietary; RFC 9381 is standard |
| **BLS signatures** | Much heavier (pairing-based); unnecessary complexity |
| **Threshold VRF** | Requires key sharing; complexity for single-signer case |

---

## Risk Mitigation

### Implementation Risk
- ✅ RFC 9381 is IETF standard (2023)
- ✅ Multiple open-source implementations exist
- ✅ Can use audited crate (e.g., `vrf` crate on crates.io)
- ✅ Test vectors in RFC for validation

### Security Risk
- ✅ Based on Edwards25519 (battle-tested)
- ✅ NIST SHA512 hash function
- ✅ Formally proven in RFC 9381
- ⚠️ NEW in TIME Coin codebase → extra testing

### Mitigation: Phase 5 Testing Plan
```
1. RFC 9381 test vector validation (100% pass rate required)
2. Differential testing (compare against reference implementation)
3. Multi-node consensus testing (determinism verification)
4. Fuzzing of VRF inputs
5. Performance benchmarking
6. Side-channel analysis (if time permits)
```

---

## Conclusion

**ECVRF is necessary, not optional.**

| Requirement | Ed25519 Only | With ECVRF |
|-------------|--------------|-----------|
| Deterministic leader selection | ❌ | ✅ |
| Non-biasable sampling | ❌ | ✅ |
| Verifiable by network | ✅ | ✅ (stronger) |
| Resistance to MEV gaming | ❌ | ✅ |
| Standard (RFC) | N/A | ✅ RFC 9381 |

**Next steps:**
1. Implement RFC 9381 ECVRF-Edwards25519-SHA512-TAI (Phase 5.1)
2. Integrate into TSDC sortition (Phase 5.2)
3. Multi-node consensus testing (Phase 5.3-5.5)
4. Go-live with ECVRF enabled

**Estimated effort:** 3-4 days implementation + 3-5 days testing = Phase 5 (11-14 days total)
