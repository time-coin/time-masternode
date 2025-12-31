# Time-Scheduled Deterministic Consensus (TSDC) Protocol

## Protocol Overview

**TSDC** is a clean, compact consensus mechanism combining time-scheduled leader election with fast finality. It guarantees deterministic leader selection per slot, single-round finality via 2/3+ stake agreement, and seamless handling of faulty leaders.

---

## Entities and Definitions

### Validators
- Set: `V = {v1, v2, …, vn}`, each validator vi has stake `wi ≥ 0`
- Total stake: `W = Σ wi`
- Honest validators: `VH ⊆ V` with `Σ(wi for vi ∈ VH) > 2/3 * W`

### Time Slots
- Time divided into discrete slots: `S0, S1, S2, …`
- Slot interval: `Δ` (e.g., 5 seconds)
- Slot Sk spans: `[k*Δ, (k+1)*Δ)`

### State Variables
```rust
last_finalized_block: Block
  - The highest block with ≥2/3 finality signatures
  - Immutable once finalized

chain_head: Block
  - Highest block seen by the validator
  - May change based on fork choice rule

pending_finality_signatures: HashMap<BlockHash, Vec<Signature>>
  - Signatures collected toward finality
```

### Cryptographic Primitives
```
VRF(seed: [u8; 32], secret_key: SecretKey) → (proof: [u8; 64], output: [u8; 32])
  - Verifiable Random Function for leader selection

SIGN(secret_key: SecretKey, message: &[u8]) → Signature
  - Digital signature (e.g., Ed25519)

AGG_SIG(signatures: Vec<Signature>) → AggregateSignature
  - Aggregate multiple signatures (e.g., BLS, threshold signing)

VERIFY_VRF(proof, output, validator_id, public_key) → bool
  - Verify VRF proof authenticity
```

---

## Leader Selection

### Epoch Randomness
- **Epoch**: A fixed number of slots (e.g., 100 slots = 500 seconds)
- **Epoch Randomness R**: Determined at epoch start
  - Option 1: Hash of all VRF outputs from previous epoch
  - Option 2: Hash of finalized block at epoch boundary
  - Option 3: Beacon chain randomness (if available)

### Leader Election

For slot Sk in epoch Ei:
```
leader_input = Hash(R || Sk)
leader_index = argmin(VRF(leader_input, vi.secret_key)) over all validators vi

L(Sk) = V[leader_index]
```

**Properties:**
- Deterministic: Same L(Sk) everywhere given R
- Weighted by stake: Higher stake → more frequent selection
- Tie-breaking: Lexicographic ordering by validator ID

### Validation
Before accepting a PREPARE message from a claimed leader:
```
1. Parse VRF proof from block header
2. Recompute: expected_leader = argmin(VRF(...)) 
3. Verify: block.leader_vrf_proof matches expected_leader
4. If verification fails: reject block, penalize peer
```

---

## Block Production Phase

### Leader Responsibilities (at slot Sk)

```
1. Collect transactions from mempool
   - Validate each transaction (nonce, signature, balance)
   - Order by fee rate (max fee first)
   - Cap total block size at MAX_BLOCK_SIZE

2. Build block Bk:
   parent = chain_head
   slot = Sk
   timestamp = k * Δ
   transactions = [selected transactions]
   prev_block_hash = hash(parent)
   leader_vrf_proof = proof from VRF(leader_input, leader_key)
   leader_signature = SIGN(leader_sk, Bk)

3. Broadcast PREPARE(Bk) to all validators
   - Message: {block: Bk, leader_id: leader_index}
```

### Validator Verification (upon receiving PREPARE)

```
validate_prepare(msg: PREPARE) → Result<(), ValidationError> {
  block = msg.block
  
  1. Structural validation:
     - block.slot matches current or recent slot
     - block.parent exists and is valid
     - block.transactions are non-empty or empty (valid)
     - block.timestamp = block.slot * Δ
  
  2. Leader eligibility:
     - Recompute expected_leader from block.leader_vrf_proof
     - Verify block.leader_id == expected_leader
     - Verify VRF proof signature
  
  3. Transaction validity:
     - All transactions have valid signatures
     - No double-spending within block
     - Fee > 0 for each transaction
  
  4. Parent validity:
     - parent_block is known (either finalized or chain_head)
     - parent_block.slot < block.slot
     - parent has no conflicting children already finalized
  
  If all checks pass:
    add block to pending_blocks
    broadcast PRECOMMIT(block.hash, SIGN(validator_sk, block.hash))
}
```

---

## Finalization Phase

### Single-Round Finality via 2/3+ Stake

```
on_receive_precommit(block_hash: Hash256, signature: Signature, from: ValidatorId) {
  1. Verify signature authenticity
  2. If block_hash not in pending_finality_signatures:
       pending_finality_signatures[block_hash] = []
  3. Append signature to pending_finality_signatures[block_hash]
  
  4. Calculate stake of signers:
       total_stake = sum(wi for all signatures received)
  
  5. If total_stake > 2/3 * W:
       finalize_block(block_hash)
       return
}

finalize_block(block_hash: Hash256) {
  block = get_block(block_hash)
  
  1. Create finality_proof = AGG_SIG(all precommit signatures)
  2. Set block.finality_proof = finality_proof
  3. Update last_finalized_block = block
  4. Prune pending_finality_signatures for older blocks
  5. Log: "✅ Block finalized at height {block.height} in {elapsed_ms}ms"
  
  // Notify upper layers of finality
  emit_finality_event(block_hash, block.height)
}
```

**Finality Time:**
- Best case: ~network propagation delay (10-100ms on WAN)
- Worst case: ~network diameter × 2 (for signature collection)
- Typical: <1 second for well-connected networks

---

## Fork Handling and Missed Leaders

### Handling Faulty/Missing Leaders

If no PREPARE received in slot Sk:
```
on_slot_timeout(Sk) {
  1. No block produced for slot Sk
  2. chain_head remains unchanged (or points to last valid block)
  3. Leader L(S(k+1)) will choose parent:
       Option A: parent = last_finalized_block
       Option B: parent = chain_head
       (Implementation-dependent; recommend Option B for liveness)
  
  4. To mark skipped slot, leader can set:
       block.prev_block_hash = hash(block at S(k-1))
       (indicates slot Sk was empty)
}
```

### Fork Choice Rule

When a validator observes multiple blocks at the same height:

```
compare_blocks(B1: Block, B2: Block) → Block {
  1. If one is finalized and the other is not:
       return finalized_block
  
  2. If both finalized or both unfinalized:
       - compare by height: higher height wins
       - if equal height, compare by slot: higher slot wins
       - if equal slot (fork!), compare by hash: lexicographically smaller wins
  
  return "preferred_block"
}
```

**Properties:**
- Deterministic everywhere
- Strongly prefers finalized blocks
- On forks, selects lexicographically smallest to break ties uniquely

---

## Message Specifications

### PREPARE Message
```rust
struct PrepareMessage {
    block: Block,
    leader_id: u32,
}

struct Block {
    height: u64,
    slot: u64,
    timestamp: u64,  // = slot * Δ
    parent_hash: Hash256,
    transactions: Vec<Transaction>,
    leader_vrf_proof: VRFProof,
    leader_signature: Signature,
    // Optional: merkle_root of transactions
}
```

### PRECOMMIT Message
```rust
struct PrecommitMessage {
    block_hash: Hash256,
    validator_id: u32,
    signature: Signature,
    // Optional: include block_height for filtering
}
```

### FINALITY Message (optional, for efficiency)
```rust
struct FinalityMessage {
    block_hash: Hash256,
    height: u64,
    finality_proof: AggregateSignature,
    signer_count: u32,
}
```

---

## State Machine

### Validator Pseudocode

```
class TSCDValidator {
  state {
    current_slot: u64
    chain_head: Block
    last_finalized_block: Block
    pending_blocks: HashMap<Hash256, Block>
    pending_finality_signatures: HashMap<Hash256, Vec<Signature>>
    is_leader: bool
  }
  
  async fn main_loop() {
    loop {
      current_slot = get_current_slot()
      
      // Check if I'm the leader for this slot
      is_leader = (get_leader(current_slot) == my_id)
      
      if is_leader {
        block = create_block(current_slot)
        broadcast(PREPARE(block))
      }
      
      // Receive and process messages
      select {
        msg = receive_prepare_with_timeout(Δ) => {
          if validate_prepare(msg) {
            pending_blocks[msg.block.hash] = msg.block
            broadcast(PRECOMMIT(msg.block.hash, sign(msg.block.hash)))
          }
        }
        msg = receive_precommit() => {
          on_receive_precommit(msg.block_hash, msg.signature, msg.validator_id)
        }
      }
      
      // Wait for next slot
      sleep_until_next_slot()
    }
  }
  
  fn on_receive_precommit(hash, sig, validator_id) {
    pending_finality_signatures[hash].append(sig)
    
    stake = calculate_stake(pending_finality_signatures[hash])
    if stake > 2/3 * total_stake {
      finalize_block(hash)
    }
  }
  
  fn finalize_block(hash) {
    block = pending_blocks[hash]
    last_finalized_block = block
    chain_head = block
    prune_old_pending_blocks()
    emit_finality_event(hash)
  }
}
```

---

## Security Properties

### Safety (no conflicting finality)
**Theorem:** If >2/3 of stake is honest, at most one block per height can be finalized.

**Proof:**
- Two blocks B1, B2 at same height both finalized ⟹ both have >2/3 signatures
- Total honest stake is >2/3 ⟹ honest validators signed both
- Contradiction: honest validators don't double-sign
- QED.

### Liveness (new blocks produced)
**Theorem:** If >2/3 honest stake follows the protocol and network is eventually synchronous, blocks are produced every slot.

**Proof:**
- At each slot Sk, exactly one leader L(Sk) is elected deterministically
- If L(Sk) is honest and online, it produces a block
- Honest validators receive and precommit it
- >2/3 honest stake signs it ⟹ finality achieved
- Even if L(Sk) is offline, L(S(k+1)) can build a block with L(Sk)'s block as parent
- QED.

### Predictable Finality Time
**Claim:** Under good network conditions, finality time ≈ max(block propagation, signature aggregation).

**Rationale:**
- Block produced at slot start
- Validators receive within ~50ms (WAN)
- Signatures collected within ~100ms (good network)
- Finality verified in next ~50ms
- Total: ~200ms typical, <1s worst case

---

## Performance Characteristics

| Metric | Value | Notes |
|--------|-------|-------|
| **Blocktime** | Δ (e.g., 5s) | Fixed by slot interval |
| **Finality Time** | ~network diameter × 2 | Typically <1s |
| **Messages per block** | O(n) | n = validator count |
| **Signature aggregation** | O(n) to O(log n) | Depends on scheme |
| **Storage per block** | ~1KB + transactions | Small for finality proofs |
| **Validator CPU per slot** | O(n) verification | VRF + signature checks |

---

## Implementation Notes

### Epoch Randomness Update
```
At epoch boundary (every E slots):
  new_R = Hash(previous_finalized_block_hash || epoch_number)
```

### Handling Network Partitions
- Validators in the larger partition produce blocks normally
- Validators in smaller partitions cannot finalize (stuck at 2/3 threshold)
- On partition healing, smaller partition adopts the finalized chain

### Byzantine Resilience
- Tolerates up to 1/3 - ε malicious or offline validators
- With <1/3 attacking, safety and liveness guaranteed
- With ≥1/3 attacking, liveness may halt, safety is guaranteed

---

## Configuration Parameters

```rust
pub struct TSCDConfig {
    pub slot_duration_ms: u64,        // Δ in milliseconds (default: 5000)
    pub epoch_length_slots: u64,      // E (default: 100)
    pub finality_threshold: f64,      // 2/3 (default: 0.667)
    pub max_block_size: usize,        // bytes (default: 10MB)
    pub vrf_scheme: String,           // "vrf-ed25519" or similar
    pub signature_scheme: String,     // "ed25519", "bls", etc.
}
```

---

## References and Further Reading

1. **Avalanche** (Rocket Fuel for the Internet): https://avalabs.org/whitepaper
   - Inspired the probabilistic sampling approach; TSDC uses deterministic VRF instead

2. **Tendermint/Cosmos**: https://tendermint.com/
   - Similar round-robin leader election; TSDC uses VRF weighting by stake

3. **Ouroboros Praos** (Cardano): https://eprint.iacr.org/2017/573
   - VRF-based leader selection; TSDC adapts this for deterministic slots

4. **BLS Signatures**: https://crypto.stanford.edu/~dabo/papers/BLS.pdf
   - Recommended for O(log n) aggregate signature schemes

---

## Changelog

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2024-12-22 | Initial TSDC specification |

---

**End of Document**
