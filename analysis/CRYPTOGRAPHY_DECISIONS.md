# Cryptography Decisions for TIME Coin Protocol v6

## Your Question
> "Why can't we just use Ed25519?"

Great question! Let me clarify the distinction between signature schemes and VRFs.

---

## Ed25519: Signature Scheme

**Ed25519** is an **EdDSA signature scheme** for:
- **Signing**: Message → Signature (with private key)
- **Verification**: Signature → Valid/Invalid (with public key)
- **Properties**: Deterministic, unforgeable, verifiable

**Cannot do**:
- ❌ Generate random-looking but reproducible values from a secret input
- ❌ Prove "this random value came from my private key"
- ❌ Prevent someone from simulating the output without your private key

---

## VRF: Verifiable Random Function

**VRF (Verifiable Random Function)** is a cryptographic primitive for:
- **Generation**: Secret key + Input → Random output + Proof
- **Verification**: Public key + Input + Proof → Valid/Invalid

**Key properties**:
- ✅ **Deterministic**: Same input always produces same output
- ✅ **Unpredictable**: Output looks random without the proof
- ✅ **Verifiable**: Anyone can verify proof without the private key
- ✅ **Collision-resistant**: Infeasible to find two different inputs producing same output

---

## Why TSDC Block Sortition Needs VRF

### The Problem: Leader Election

In TSDC (Time-Scheduled Deterministic Consensus), we need to elect a leader for each slot:

```
Slot 100: Who produces the block?
  → Must be deterministic (everyone agrees who it is)
  → Must be unpredictable (can't predict leaders in advance)
  → Must be verifiable (can't fake the election)
```

### With Ed25519 (Doesn't Work)

```
Input: previous_hash || slot_number
Signature: node_privkey.sign(input)
Output: 64-byte signature

Problem: Signature is deterministic for a node, but:
  - It's too long (64 bytes) for sortition
  - It's not unpredictable (known once you sign it)
  - Any node can simulate it if they steal the signature
  - Doesn't create a "lottery" effect
```

### With VRF (Works!)

```
Input: previous_hash || slot_number
VRF Proof: (vrf_output, proof_bytes)
Output: 32-byte random-looking value

Properties:
  ✅ Deterministic: node[i] always produces same output for same slot
  ✅ Unpredictable: looks random, no way to predict before computation
  ✅ Verifiable: peers can verify proof matches node's public key
  ✅ Sortition: Compare all outputs, smallest wins = lottery effect
```

---

## Recommended Crypto Stack

### Option 1: Full VRF (Spec-Compliant)

```yaml
HASH: BLAKE3 (fast, secure, modern)
SIGNATURES: Ed25519 (for transactions, voting)
VRF: ECVRF-Edwards25519-SHA512-TAI (RFC 9381)
TX_SERIALIZATION: Bincode or custom little-endian format

Why this:
- Ed25519 for normal signing (transactions, finality votes)
- ECVRF for deterministic leader election (TSDC)
- BLAKE3 for fast hashing everywhere
- Consistent use of Edwards25519 curve
```

### Option 2: Simplified (Faster Implementation)

```yaml
HASH: BLAKE3 (for everything)
SIGNATURES: Ed25519 (transactions, voting)
SORTITION: BLAKE3(privkey || prev_hash || slot_time)
           → Take lowest 256 bits as VRF-like output

Why this:
- No new cryptographic primitive to implement
- BLAKE3 is Merkle tree friendly (good for signatures)
- Fast and already in Rust ecosystem
- Sufficient for testnet (weaker than real VRF but okay for development)

Security Note: This is NOT a true VRF (doesn't have formal proofs),
but works for internal sortition within a trusted network
```

### Option 3: Skip Sortition (Minimal MVP)

```yaml
HASH: BLAKE3
SIGNATURES: Ed25519
LEADER: Fixed round-robin (validator 0, 1, 2, ...)

Why:
- Simplest to implement
- Good for initial testing
- No randomness needed
- Can upgrade to VRF later

When to use:
- Unit tests
- Initial network bringup
- Testnet without adversaries
```

---

## Current Implementation Status

### What We Have ✅
- Ed25519 for transaction signatures
- BLAKE3 for hashing
- Avalanche consensus fully implemented
- TSDC slot timing

### What We Need 🔄
- VRF or sortition function for TSDC leader election
- Currently using deterministic hash-based selection (Option 2 above)
- See line 219-243 in `src/tsdc.rs`

### What We Should Add 🎯

**Short term** (Testnet):
```rust
// Current code uses this (OK for now)
fn select_leader(slot: u64) -> ValidatorInfo {
    let mut best_hash = Sha256::digest(b"genesis");
    for validator in validators {
        let hash = Sha256::digest(format!("{}{}", slot, validator.id));
        if hash < best_hash { best_hash = hash; }
    }
    // Return validator with lowest hash
}
```

**Medium term** (Mainnet):
```rust
// Switch to actual VRF
use vrf_dalek::VRF; // Or similar library

fn select_leader(slot: u64) -> ValidatorInfo {
    let vrf_input = format!("{}", slot);
    let vrf_output = node.vrf.compute(&vrf_input); // 32 bytes
    
    // All validators compute this, compare outputs, 
    // lowest VRF output wins
    let mut best_output = [255u8; 32];
    let mut leader = &validators[0];
    
    for validator in validators {
        let output = VRF::compute(validator.pubkey, &vrf_input);
        if output < best_output {
            best_output = output;
            leader = &validator;
        }
    }
}
```

---

## Why NOT Just Use Ed25519

| Feature | Ed25519 | VRF | Why VRF Needed |
|---------|---------|-----|---|
| Sign message | ✅ Yes | ❌ No | Different purpose |
| Verify signature | ✅ Yes | ❌ No | Different purpose |
| Random output | ❌ No | ✅ Yes | Needed for sortition |
| Unpredictable | ❌ No | ✅ Yes | Prevent leader prediction |
| Deterministic | ✅ Yes | ✅ Yes | Required |
| Verifiable | ✅ Yes | ✅ Yes | Required |
| Collision-resistant | ✅ Yes | ✅ Yes | Required |

**Bottom line**: Ed25519 is for **authentication** (proving who signed something). VRF is for **random oracle** (everyone agrees on a random value without trusting anyone).

---

## Cryptography Summary Table

```
╔═══════════════════════════════════════════════════════════════╗
║                    PURPOSE              │  USE THIS           ║
╠═══════════════════════════════════════════════════════════════╣
║ Transaction signatures                 │  Ed25519            ║
║ Finality vote signatures                │  Ed25519            ║
║ Transaction hashing (txid)              │  BLAKE3-256         ║
║ Block hashing                           │  BLAKE3-256         ║
║ Merkle tree commitments                 │  BLAKE3 (tree mode) ║
║ VFP (Finality Proof) hash               │  BLAKE3-256         ║
║ TSDC Leader election (sortition)        │  ECVRF-Edwards      ║
║ Slot randomness                         │  ECVRF-Edwards      ║
║ General-purpose hashing                 │  BLAKE3             ║
╚═══════════════════════════════════════════════════════════════╝
```

---

## Implementation Recommendation

### Phase 1 (Now): Get consensus working
- Use Ed25519 for all signatures ✅ (done)
- Use BLAKE3 for hashing ✅ (done)
- Use hash-based leader selection (temporary)

### Phase 2 (Before mainnet): Add VRF
- Implement ECVRF-Edwards25519-SHA512-TAI per RFC 9381
- Or use `vrf-dalek` crate if available
- Update TSDC leader selection

### Phase 3 (Production): Optimize
- Consider VRF curve options (Ristretto25519 vs Edwards25519)
- Benchmark different VRF implementations
- Consider ZK proofs for stronger sortition

---

## References

- **Ed25519**: RFC 8032 - Edwards-Curve Digital Signature Algorithm (EdDSA)
- **VRF**: RFC 9381 - ECVRF: Elliptic Curve Verifiable Random Functions
- **BLAKE3**: https://blake3.io/
- **Avalanche Sortition**: https://github.com/ava-labs/avalanchego (look for ProposerID)

---

**Tl;dr**: Ed25519 proves "I signed this." VRF proves "This random number came from my private key." Different tools, different jobs. TSDC needs VRF, not Ed25519.
