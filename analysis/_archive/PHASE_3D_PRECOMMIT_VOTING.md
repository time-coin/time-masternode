# Phase 3D: Precommit Voting - Implementation Guide

**Status:** Ready to implement  
**Estimated Duration:** 1-2 hours  
**Build Target:** cargo check + cargo fmt + cargo clippy passing

---

## Overview

Phase 3D implements Byzantine fault-tolerant consensus for block acceptance. After validators receive a block proposal, they vote on whether to accept it. Once 2/3 of validators vote to accept (by weight), the block is ready for finalization.

---

## Architecture

```
Block Proposal Received (Phase 3C)
    ↓
Validate Block (Phase 3C)
    ↓
Generate PrepareVote (Phase 3D.1)
    ├─ Sign vote with private key
    └─ Broadcast to peers
    ↓
Receive PrepareVotes (Phase 3D.2)
    ├─ Validate signatures
    ├─ Track by block_hash
    ├─ Accumulate weights
    └─ Check 2/3 threshold
    ↓
2/3 Threshold Reached (Phase 3D.3)
    ├─ Generate PrecommitVote
    └─ Broadcast to peers
    ↓
Receive PrecommitVotes (Phase 3D.4)
    ├─ Accumulate weights
    ├─ Check 2/3 threshold
    └─ Set finalize_ready = true
    ↓
Ready for Phase 3E (Block Finalization)
```

---

## Phase 3D.1: Prepare Vote Generation

### Task
When a valid block is received, generate and broadcast a prepare vote.

### Files to Modify
- `src/tsdc.rs`
- `src/consensus.rs`

### Implementation

```rust
// In src/tsdc.rs

/// Generate a prepare vote for a block
pub fn generate_prepare_vote(
    &self,
    block_hash: [u8; 32],
    slot_index: u64,
    node_id: &str,
    private_key: &str,
) -> Option<TSCDPrepareVote> {
    // Create vote struct
    let vote = TSCDPrepareVote {
        block_hash,
        slot_index,
        voter_id: node_id.to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        signature: sign_data(private_key, &block_hash)?,
    };
    
    Some(vote)
}

/// Broadcast prepare vote to all peers
pub fn broadcast_prepare_vote(&self, vote: TSCDPrepareVote) {
    let message = NetworkMessage::TSCDPrepareVote(vote);
    // Send via network layer to all peers
    // This pattern already exists from block proposal broadcasting
}
```

### Integration Point

In `src/network/server.rs`, in the `TSCDBlockProposal` handler:

```rust
async fn handle_block_proposal(&self, block: Block) {
    // Existing validation code...
    if is_valid_block(&block) {
        // [Phase 3C code already here]
        log_block_received(&block);
        
        // NEW: Phase 3D.1 - Generate prepare vote
        if let Some(vote) = self.tsdc.generate_prepare_vote(
            block.hash(),
            current_slot,
            &self.node_id,
            &self.private_key,
        ) {
            self.tsdc.broadcast_prepare_vote(vote);
            log!("✅ Sent prepare vote for block {}", block_hash_hex(&block.hash()));
        }
    }
}
```

### Logging
```
✅ Sent prepare vote for block 0x123abc...
```

### Success Criteria
- [ ] Prepare votes generated for valid blocks
- [ ] Signatures verify correctly
- [ ] Votes broadcast to network
- [ ] Code compiles

---

## Phase 3D.2: Prepare Vote Accumulation

### Task
Receive prepare votes from peers and accumulate them until 2/3 consensus.

### Files to Modify
- `src/consensus.rs` - Add vote storage
- `src/network/server.rs` - Add vote handler

### Implementation

```rust
// In src/consensus.rs - Add to AvalancheConsensus struct

pub struct PrepareVoteAccumulator {
    votes: HashMap<[u8; 32], Vec<TSCDPrepareVote>>, // block_hash -> votes
    validators: Vec<(String, u64)>, // (validator_id, weight)
    total_weight: u64,
}

impl PrepareVoteAccumulator {
    pub fn new(validators: Vec<(String, u64)>) -> Self {
        let total_weight: u64 = validators.iter().map(|(_, w)| w).sum();
        Self {
            votes: HashMap::new(),
            validators,
            total_weight,
        }
    }
    
    /// Add a prepare vote
    pub fn add_vote(&mut self, vote: TSCDPrepareVote) -> Result<(), String> {
        // Find validator weight
        let weight = self.validators
            .iter()
            .find(|(id, _)| id == &vote.voter_id)
            .map(|(_, w)| w)
            .ok_or("Unknown validator")?;
        
        // Add to accumulator
        self.votes
            .entry(vote.block_hash)
            .or_insert_with(Vec::new)
            .push(vote);
        
        Ok(())
    }
    
    /// Check if 2/3 consensus reached for block
    pub fn check_consensus(&self, block_hash: [u8; 32]) -> bool {
        let votes = match self.votes.get(&block_hash) {
            Some(v) => v,
            None => return false,
        };
        
        let weight: u64 = votes.iter()
            .filter_map(|v| {
                self.validators
                    .iter()
                    .find(|(id, _)| id == &v.voter_id)
                    .map(|(_, w)| w)
            })
            .sum();
        
        weight * 3 >= self.total_weight * 2  // 2/3 threshold
    }
}
```

### Integration Point

In `src/network/server.rs`, add handler:

```rust
async fn handle_prepare_vote(&self, vote: TSCDPrepareVote) {
    // Validate vote signature
    if !verify_signature(&vote.signature, &vote.voter_id) {
        log!("❌ Invalid prepare vote signature");
        return;
    }
    
    // Add to accumulator
    if let Err(e) = self.consensus.prepare_votes.add_vote(vote.clone()) {
        log!("❌ Failed to add prepare vote: {}", e);
        return;
    }
    
    log!("✅ Received prepare vote from {}", vote.voter_id);
    
    // Check for consensus
    if self.consensus.prepare_votes.check_consensus(vote.block_hash) {
        log!("✅ Prepare consensus reached! (2/3 weight)");
        // NEW: Phase 3D.3 - Generate precommit vote
    }
}
```

### Logging
```
✅ Received prepare vote from validator_2
✅ Prepare consensus reached! (2/3 weight)
```

### Success Criteria
- [ ] Prepare votes accumulated correctly
- [ ] 2/3 threshold detected
- [ ] No duplicate counting
- [ ] Weight calculation accurate

---

## Phase 3D.3: Precommit Vote Generation

### Task
Once 2/3 prepare votes are accumulated, generate and broadcast precommit vote.

### Implementation

```rust
/// Generate precommit vote after prepare consensus
pub fn generate_precommit_vote(
    &self,
    block_hash: [u8; 32],
    slot_index: u64,
    node_id: &str,
    private_key: &str,
) -> Option<TSCDPrecommitVote> {
    let vote = TSCDPrecommitVote {
        block_hash,
        slot_index,
        voter_id: node_id.to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        signature: sign_data(private_key, &block_hash)?,
    };
    
    Some(vote)
}

/// Broadcast precommit vote to peers
pub fn broadcast_precommit_vote(&self, vote: TSCDPrecommitVote) {
    let message = NetworkMessage::TSCDPrecommitVote(vote);
    // Send via network layer
}
```

### Integration Point

```rust
// In prepare consensus check handler:
if self.consensus.prepare_votes.check_consensus(vote.block_hash) {
    log!("✅ Prepare consensus reached!");
    
    // Generate precommit vote
    if let Some(precommit) = self.tsdc.generate_precommit_vote(
        vote.block_hash,
        current_slot,
        &self.node_id,
        &self.private_key,
    ) {
        self.tsdc.broadcast_precommit_vote(precommit);
        log!("✅ Sent precommit vote for block");
    }
}
```

### Logging
```
✅ Prepare consensus reached!
✅ Sent precommit vote for block 0x123abc...
```

---

## Phase 3D.4: Precommit Vote Accumulation

### Task
Accumulate precommit votes and detect when 2/3 threshold is reached.

### Implementation

```rust
// In src/consensus.rs - Similar to PrepareVoteAccumulator

pub struct PrecommitVoteAccumulator {
    votes: HashMap<[u8; 32], Vec<TSCDPrecommitVote>>,
    validators: Vec<(String, u64)>,
    total_weight: u64,
}

impl PrecommitVoteAccumulator {
    pub fn new(validators: Vec<(String, u64)>) -> Self {
        let total_weight: u64 = validators.iter().map(|(_, w)| w).sum();
        Self {
            votes: HashMap::new(),
            validators,
            total_weight,
        }
    }
    
    pub fn add_vote(&mut self, vote: TSCDPrecommitVote) -> Result<(), String> {
        let weight = self.validators
            .iter()
            .find(|(id, _)| id == &vote.voter_id)
            .map(|(_, w)| w)
            .ok_or("Unknown validator")?;
        
        self.votes
            .entry(vote.block_hash)
            .or_insert_with(Vec::new)
            .push(vote);
        
        Ok(())
    }
    
    pub fn check_consensus(&self, block_hash: [u8; 32]) -> bool {
        let votes = match self.votes.get(&block_hash) {
            Some(v) => v,
            None => return false,
        };
        
        let weight: u64 = votes.iter()
            .filter_map(|v| {
                self.validators
                    .iter()
                    .find(|(id, _)| id == &v.voter_id)
                    .map(|(_, w)| w)
            })
            .sum();
        
        weight * 3 >= self.total_weight * 2  // 2/3 threshold
    }
}
```

### Handler

```rust
async fn handle_precommit_vote(&self, vote: TSCDPrecommitVote) {
    // Validate signature
    if !verify_signature(&vote.signature, &vote.voter_id) {
        return;
    }
    
    // Accumulate vote
    if let Err(e) = self.consensus.precommit_votes.add_vote(vote.clone()) {
        log!("❌ Failed to add precommit vote: {}", e);
        return;
    }
    
    log!("✅ Received precommit vote from {}", vote.voter_id);
    
    // Check for finalization
    if self.consensus.precommit_votes.check_consensus(vote.block_hash) {
        log!("✅ Precommit consensus reached! Block ready for finalization");
        self.consensus.finalize_ready = true;
    }
}
```

### Logging
```
✅ Received precommit vote from validator_3
✅ Precommit consensus reached! Block ready for finalization
```

---

## Integration Checklist

- [ ] Add PrepareVoteAccumulator to AvalancheConsensus
- [ ] Add PrecommitVoteAccumulator to AvalancheConsensus
- [ ] Implement generate_prepare_vote() in tsdc.rs
- [ ] Implement generate_precommit_vote() in tsdc.rs
- [ ] Add prepare vote handler in network/server.rs
- [ ] Add precommit vote handler in network/server.rs
- [ ] Hook prepare vote generation into block proposal handler
- [ ] Hook precommit vote generation into prepare consensus check
- [ ] Set finalize_ready flag on precommit consensus
- [ ] cargo check passes
- [ ] cargo fmt passes
- [ ] cargo clippy passes

---

## Testing Strategy

### Unit Tests
```rust
#[test]
fn test_prepare_vote_accumulation() {
    let mut acc = PrepareVoteAccumulator::new(vec![
        ("validator_1".to_string(), 100),
        ("validator_2".to_string(), 100),
        ("validator_3".to_string(), 100),
    ]);
    
    let vote = TSCDPrepareVote { /* ... */ };
    acc.add_vote(vote).unwrap();
    
    assert!(!acc.check_consensus(block_hash)); // Need 2/3 votes
}

#[test]
fn test_prepare_consensus_threshold() {
    // Add 2/3 weighted votes
    // Assert consensus reached
}
```

### Integration Test
- Create 3+ validators
- Send block proposal from leader
- All validators should broadcast prepare votes
- Check that consensus is detected
- All validators should broadcast precommit votes

---

## Estimated Implementation Time

- **3D.1 - Prepare vote generation:** 15 minutes
- **3D.2 - Prepare vote accumulation:** 20 minutes
- **3D.3 - Precommit vote generation:** 10 minutes
- **3D.4 - Precommit vote accumulation:** 20 minutes
- **Testing & integration:** 30 minutes

**Total: 1-1.5 hours**

---

## Common Pitfalls to Avoid

1. **Duplicate vote counting:** Ensure each validator's vote counted only once
2. **Weight mismatch:** Verify validator weights are current and consistent
3. **Off-by-one errors:** Check 2/3 calculation (weight * 3 >= total_weight * 2)
4. **Missing message routing:** Ensure votes are routed to correct handler
5. **Signature validation:** Always verify before accepting votes

---

## Success Metrics

After Phase 3D, you should see in logs:
```
🎯 SELECTED AS LEADER for slot 12345
📦 Proposed block at height 100 with 42 transactions
✅ Sent prepare vote for block 0x123abc...
✅ Received prepare vote from validator_2
✅ Received prepare vote from validator_3
✅ Prepare consensus reached! (2/3 weight)
✅ Sent precommit vote for block 0x123abc...
✅ Received precommit vote from validator_2
✅ Received precommit vote from validator_3
✅ Precommit consensus reached! Block ready for finalization
```

---

Next: Phase 3E - Block Finalization (PHASE_3E_FINALIZATION.md)
