# Cryptography Stack Rationale

**Question:** Why three algorithms (BLAKE3, Ed25519, ECVRF)? Can't we just use Ed25519?

**Short Answer:** They solve different problems. Ed25519 alone is insufficient.

---

## The Three Algorithms and Their Roles

### 1. **Ed25519** (Signature Scheme)
```
Use case: Sign and verify messages
Problem solved: Authentication & non-repudiation
```

**What it does:**
- Signs transactions, heartbeats, finality votes
- Proves a message came from a specific private key
- Example: `FinalityVote.signature = Ed25519_Sign(voter_privkey, vote_data)`

**Why it alone isn't enough:**
- Ed25519 doesn't produce a **unique, deterministic hash** of data
- You need a hash to create transaction IDs, block hashes, etc.
- Signatures don't work as content-addressable identifiers

**Example of why this matters:**
```
Two ways to represent the same transaction:
  TX(format_A) → different bytes → Ed25519 signs different messages → different sigs
  TX(format_B) → different bytes → but it's the same transaction!

Without a canonical hash:
  - Users can't agree on a transaction ID
  - Network gets confused about what was finalized
```

---

### 2. **BLAKE3** (Hash Function)
```
Use case: Compute deterministic, content-addressable hashes
Problem solved: Transaction IDs, block hashes, merkle roots
```

**What it does:**
- Computes `txid = BLAKE3(canonical_tx_bytes)`
- Creates immutable content addresses
- Fast, secure, modern alternative to SHA-256

**Why Ed25519 can't do this:**
```
Ed25519 is for signing, not hashing arbitrary data.
BLAKE3 is for hashing.

Different tools for different jobs:
  • Ed25519: "This transaction is signed by Alice"
  • BLAKE3: "This transaction has ID abc123..."
```

**Why BLAKE3 over SHA-256d?**
- Faster (parallel hashing)
- Simpler (not double-hash)
- Same security level (256-bit output)
- Modern standard

---

### 3. **ECVRF** (Verifiable Random Function)
```
Use case: Deterministic but unpredictable sortition for TSDC block production
Problem solved: Fair, verifiable block leader selection
```

**What it does:**
- Each masternode computes: `score = VRF(privkey, prev_block_hash || slot_time || chain_id)`
- Score is deterministic (same input → same output)
- But unpredictable (only privkey holder can compute it first)
- Verifiable (anyone with pubkey can check the proof)

**Example:**
```
Masternode A with privkey_A:
  score_A = VRF(privkey_A, input) = 0x123abc...
  
Masternode B with privkey_B:
  score_B = VRF(privkey_B, input) = 0x456def...

Canonical leader = min(score_A, score_B) = Masternode B
(lowest score wins)
```

**Why you need VRF (not just Ed25519 or BLAKE3):**

| Requirement | Ed25519 | BLAKE3 | VRF |
|---|---|---|---|
| Deterministic? | ✗ | ✓ | ✓ |
| Unpredictable (before reveal)? | ✗ | ✗ | ✓ |
| Verifiable by anyone? | ✓ | ✗ | ✓ |
| Creates sortition ranking? | ✗ | Can't | ✓ |

**Why BLAKE3 alone doesn't work for TSDC:**
```
If block leader = lowest_hash(privkey || input):
  • Everyone can compute hash(input) → predictable!
  • Adversary can forge a privkey with low hash
  • No security

VRF fixes this:
  • Binds output to a private key
  • Unpredictable until the holder reveals it
  • Cryptographically proven (VRF proof)
```

**Why Ed25519 doesn't replace VRF:**
```
Ed25519 signatures are not sortition-ready:
  • Not designed for deterministic output comparison
  • Signatures don't create a numeric ordering
  • No proof of "lowest score"

VRF is purpose-built for:
  • Deterministic output
  • Numeric ordering
  • Verifiable randomness
```

---

## Real-World Example: What Breaks If You Use Just Ed25519?

### Scenario: TSDC Block Production with Only Ed25519

```rust
// WRONG: Using Ed25519 to elect block producers
fn elect_leader(nodes: &[Masternode], slot: u64) {
    let input = format!("{}", slot);
    for node in nodes {
        let signature = node.ed25519_sign(&input);
        println!("Node {} signature: {:?}", node.id, signature);
    }
    // Problem: How do you compare signatures to pick the leader?
    // Signatures are bytes, but they're not deterministic numbers for sorting!
}
```

**Issues:**
1. **Signature size varies:** Ed25519 signatures are 64 bytes, hard to compare numerically
2. **Not designed for ranking:** Ed25519 assumes binary (valid/invalid), not "lower is better"
3. **Every signer produces different data:** Can't create a fair ordering
4. **Adversarial manipulation:** A node might create many privkeys until one produces a "good-looking" signature

### Scenario: Correct Approach with VRF

```rust
// CORRECT: Using VRF to elect block producers
fn elect_leader(nodes: &[Masternode], slot: u64) {
    let input = format!("{}", slot);
    let mut scores = vec![];
    
    for node in nodes {
        let (vrf_output, vrf_proof) = node.vrf_prove(&input);
        
        // Can verify the proof
        assert!(node.pubkey.vrf_verify(&input, &vrf_output, &vrf_proof));
        
        // vrf_output is a numeric value—can be compared
        scores.push((node.id, vrf_output));
    }
    
    scores.sort_by_key(|(_id, output)| output);
    let leader = scores[0].0;
    println!("Block leader: {} (score: {})", leader, scores[0].1);
}
```

**Advantages:**
1. **Deterministic:** Same node, same slot → same score
2. **Unpredictable:** Can't compute score before privkey holder reveals it
3. **Verifiable:** Anyone can check the proof
4. **Rankable:** Numeric output allows sorting (lowest wins)
5. **Sybil-resistant:** Can't game the system by creating many identities

---

## Why Not Use One Algorithm for All Three Roles?

| Use Case | Why Not... | Why Need... |
|----------|-----------|-----------|
| **Sign votes** | Can't use BLAKE3 (not a sig scheme) or VRF (not designed for sigs) | **Ed25519** |
| **Hash transactions** | Can't use Ed25519 (not a hash func) or VRF (outputs are large, meant for ranking) | **BLAKE3** |
| **Elect block leaders** | Can't use Ed25519 (not sortition-ready) or BLAKE3 (predictable/anyone can compute) | **ECVRF** |

---

## Comparison with Bitcoin/Ethereum

### Bitcoin Stack
```
Hash:      SHA-256d (SHA-256 twice)
Signature: ECDSA (secp256k1)
Random:    Proof-of-Work (compute-hard hashing)
```

**Why TIME Coin is different:**
- TIME Coin uses **stake-weighted consensus** (not PoW)
- Needs **VRF for fair leader sortition** (Bitcoin uses longest chain)
- Uses **modern BLAKE3** instead of SHA-256d

### Ethereum Stack
```
Hash:      Keccak-256 (not standard SHA-3)
Signature: ECDSA (secp256k1)
Random:    Beacon chain VRF (RANDAO + BLS)
```

**Why TIME Coin matches Ethereum's approach:**
- Ethereum also recognized: **signatures + hashing + VRF are three different needs**
- Ethereum uses RANDAO (similar to VRF) for validator randomness
- TIME Coin's stack is simpler and more standard (RFC 9381)

---

## Implementation Implications

### Cryptography Dependency Chart

```
Transaction Flow:
  1. Create TX → BLAKE3_hash(tx_bytes) → txid
  2. Sign TX → Ed25519_sign(tx, privkey) → signature
  3. Include in VFP → Ed25519_verify(voter_pubkey, signature)

Block Production Flow:
  1. Compute slot → VRF(privkey, input) → (score, proof)
  2. Verify score → VRF_verify(pubkey, input, score, proof)
  3. Compare scores → min(all_scores) → select leader
```

### Code Organization

```rust
// Three separate modules
mod hash {
    use blake3;  // BLAKE3-256
    pub fn tx_hash(tx: &Transaction) -> Hash256 { ... }
}

mod sign {
    use ed25519_dalek;  // Ed25519
    pub fn sign_vote(vote: &FinalityVote, sk: &SecretKey) -> Signature { ... }
}

mod vrf {
    use ecvrf;  // RFC 9381
    pub fn prove_vrf(sk: &VrfSecretKey, input: &[u8]) -> (Output, Proof) { ... }
}
```

---

## Could We Simplify to Two Algorithms?

### Option 1: Remove BLAKE3, use Ed25519 for hashing?
**No.** Ed25519 is a signature scheme, not a hash function.
- Produces 64-byte signatures, not content addresses
- No cryptographic binding to arbitrary data
- Would require wrapper functions (defeats simplicity)

### Option 2: Remove VRF, use BLAKE3 for leader election?
**No.** BLAKE3 is deterministic but not private:
```
block_leader = argmin(BLAKE3(pubkey || slot))
// Anyone can compute this before the slot!
// Adversary can simulate all validators' scores.
// No privacy advantage to having a private key.
```

VRF is essential because:
- Privkey holder computes the output first (temporal advantage)
- Others can verify it happened correctly (verifiable)
- Unpredictable to everyone else (fairness)

### Option 3: Remove Ed25519, use BLAKE3 + HMAC for signatures?
**No.** BLAKE3 + HMAC is weaker:
- No asymmetric cryptography (can't prove sender publicly)
- Shared secrets are harder to manage at scale
- Ed25519 is battle-tested, standard, and faster

---

## Summary Table

| Algorithm | Role | Why Needed | Alternative? |
|-----------|------|-----------|--------------|
| **BLAKE3** | Hash (txid, blocks, commitments) | Content-addressable identifiers | SHA-256d (slower, less modern) |
| **Ed25519** | Sign messages (votes, txs, heartbeats) | Asymmetric authentication | ECDSA secp256k1 (larger keys) |
| **ECVRF** | Deterministic randomness for block election | Stake-weighted fair leader sortition | None (VRF is purpose-built) |

**Bottom line:** Each algorithm is irreplaceable. Trying to combine them would either:
1. Weaken security (fewer specialized tools)
2. Increase complexity (custom wrappers)
3. Slow down performance (forcing tools to do what they're not designed for)

---

## Answer to Your Original Question

> "Why not just use Ed25519?"

**Because Ed25519 only solves one problem: signing and verifying messages.**

```
TIME Coin needs to solve THREE different problems:
  ✓ Sign messages              → Ed25519
  ✓ Hash transactions          → BLAKE3
  ✓ Elect block leaders fairly → ECVRF

Using Ed25519 for all three would be like:
  "Why not just use a hammer for building?"
  → You still need a screwdriver and a saw.
```

---
