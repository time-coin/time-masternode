# Catchup Block Consensus - Design Document

**Date**: 2026-01-08 (Updated)  
**Issue**: Catchup blocks produced by single leader without consensus validation cause network forks  
**Solution**: Multi-masternode Avalanche consensus for catchup block validation  
**Consensus Model**: Avalanche simple majority (>50%), NOT BFT

---

## Problem Statement

### Current Catchup Block System

1. **TSDC selects a single leader** deterministically based on height
2. **Leader produces catchup blocks alone**
3. **Other nodes passively wait** for leader's blocks
4. **No validation** that leader's blocks are correct
5. **If leader produces bad blocks**, entire network accepts them → **FORK**

### Evidence of Problem

From production logs:
```
🎯 SELECTED AS CATCHUP LEADER for height 5511
📦 Catchup progress: 10/50 blocks (height: 5520)
✅ Catchup complete: produced 50 blocks, height now: 5560
```

But then other nodes:
```
❌ Block 5511 previous_hash mismatch
🔀 Fork detected at height 5511
❌❌❌ REORGANIZATION FAILED ❌❌❌
```

**Root Cause**: The catchup leader produced blocks that don't match what other nodes expected. This could be due to:
- Different UTXO state
- Different mempool transactions  
- Database corruption
- Software bugs
- Timing issues

---

## Solution: Catchup Block Consensus (Avalanche Model)

### Design Principles

1. **Multi-masternode agreement**: Simple majority (>50%) using Avalanche consensus
2. **Proposer-validator model**: Leader proposes, all masternodes validate
3. **Deterministic consensus**: All nodes agree on validator set  
4. **Fast consensus**: 30-60 second rounds using Avalanche sampling
5. **Fallback to sync**: If consensus fails, try syncing from peers

### Architecture

```
                    ┌─────────────────────────────────────┐
                    │  Catchup Block Consensus Protocol   │
                    │      (Avalanche Simple Majority)    │
                    └─────────────────────────────────────┘
                                   │
                    ┌──────────────┴───────────────┐
                    │                              │
             ┌──────▼────────┐          ┌─────────▼────────┐
             │  Phase 1:     │          │   Phase 2:       │
             │  Proposal     │          │   Validation     │
             └──────┬────────┘          └─────────┬────────┘
                    │                              │
      ┌─────────────┴───────────┐    ┌────────────┴────────────┐
      │  Leader Selection       │    │  All Masternodes        │
      │  (TSDC deterministic)   │    │  Validate               │
      └─────────────┬───────────┘    └────────────┬────────────┘
                    │                              │
      ┌─────────────▼───────────┐    ┌────────────▼────────────┐
      │  Leader produces block  │    │  Masternodes check:     │
      │  - Height N             │    │  - Previous hash        │
      │  - Timestamp            │    │  - Merkle root          │
      │  - Transactions         │    │  - Rewards              │
      │  - Masternode rewards   │    │  - Signatures           │
      └─────────────┬───────────┘    │  - Timestamp            │
                    │                └────────────┬────────────┘
                    │                              │
      ┌─────────────▼─────────────────────────────▼────────┐
      │      Phase 3: Avalanche Consensus Decision          │
      │                                                      │
      │  IF >50% masternodes APPROVE → Accept block         │
      │  IF ≤50% masternodes APPROVE → Reject, try backup   │
      │  IF backup fails → Sync from peers                  │
      └──────────────────────────────────────────────────────┘
```

### Components

#### 1. Catchup Block Proposal

```rust
pub struct CatchupBlockProposal {
    pub height: u64,
    pub block: Block,
    pub proposer: String,  // Masternode address
    pub signature: Vec<u8>,
    pub timestamp: i64,
}
```

**Leader's Role**:
- Produce catchup block for height N
- Sign the block with masternode key
- Broadcast proposal to ALL masternodes
- Wait for validator responses (timeout: 30s)

#### 2. All Masternodes Participate

**Unlike original design with committee**: ALL active masternodes vote, not just a selected committee.

```rust
pub fn get_catchup_validators(height: u64) -> Result<Vec<MasternodeInfo>, String> {
    // All active masternodes participate in Avalanche consensus
    let all_masternodes = self.list_active().await;
    
    // Sort deterministically for consistent ordering
    let mut sorted = all_masternodes;
    sorted.sort_by(|a, b| a.address.cmp(&b.address));
    
    Ok(sorted)
}
```

**Why all masternodes?**
- Avalanche works best with full network participation
- Simple majority (>50%) requires knowing total population
- No need for sampling in catchup scenario (not high-frequency)

#### 3. Validator Checks

```rust
pub enum CatchupValidation {
    Approve(CatchupApproval),
    Reject(CatchupRejection),
}

pub struct CatchupApproval {
    pub height: u64,
    pub block_hash: [u8; 32],
    pub validator: String,
    pub signature: Vec<u8>,
    pub timestamp: i64,
}

pub struct CatchupRejection {
    pub height: u64,
    pub validator: String,
    pub reason: String,
    pub timestamp: i64,
}
```

**Validation Steps**:
1. **Block structure valid**: Proper header, transactions, rewards
2. **Previous hash matches**: Links to our chain correctly
3. **Timestamp valid**: Not in future, matches schedule
4. **Merkle root matches**: Transactions hash correctly
5. **Rewards correct**: Masternode rewards properly distributed
6. **Signature valid**: Proposer signed correctly
7. **UTXO consistency**: Transactions spend valid UTXOs

**Validator Response**:
- `APPROVE` + signature if all checks pass
- `REJECT` + reason if any check fails
- No response if validator is offline (timeout after 30s)

#### 4. Consensus Threshold (Avalanche Simple Majority)

```rust
pub fn check_catchup_consensus(
    approvals: &[CatchupApproval],
    rejections: &[CatchupRejection],
    total_masternodes: usize,
) -> CatchupConsensusResult {
    // Avalanche consensus: need >50% of total masternodes
    let threshold = (total_masternodes / 2) + 1;  // Simple majority
    
    if approvals.len() >= threshold {
        CatchupConsensusResult::Accepted {
            approvals: approvals.len(),
            threshold,
            percentage: (approvals.len() * 100) / total_masternodes,
        }
    } else if rejections.len() >= threshold {
        // Majority rejected
        CatchupConsensusResult::Rejected {
            rejections: rejections.len(),
            threshold,
            percentage: (rejections.len() * 100) / total_masternodes,
        }
    } else {
        // Waiting for more responses or network split
        CatchupConsensusResult::Pending {
            approvals: approvals.len(),
            rejections: rejections.len(),
            threshold,
        }
    }
}
```

**Key Difference from BFT**:
- ✅ **Simple majority**: >50% (e.g., 3 of 5, 4 of 7, 5 of 9)
- ❌ **NOT 2/3 supermajority**: That's BFT (e.g., 4 of 5, 5 of 7, 6 of 9)

### Message Protocol

#### CatchupProposal Message
```rust
NetworkMessage::CatchupProposal {
    proposal: CatchupBlockProposal,
}
```

Broadcast by leader to all masternodes.

#### CatchupValidation Message
```rust
NetworkMessage::CatchupValidation {
    validation: CatchupValidation,  // Approve or Reject
}
```

Sent by validators to leader (and potentially broadcast to all).

#### CatchupConsensus Message
```rust
NetworkMessage::CatchupConsensus {
    height: u64,
    block: Block,
    approvals: Vec<CatchupApproval>,
    result: CatchupConsensusResult,
}
```

Broadcast by leader when consensus reached. Contains proof of consensus (validator signatures).

---

## Implementation Plan

### Phase 1: Data Structures (1-2 hours)

1. **Add to `src/network/message.rs`**:
   ```rust
   pub enum NetworkMessage {
       // ... existing messages ...
       
       /// Catchup block proposal from leader
       CatchupProposal(CatchupBlockProposal),
       
       /// Validation response from committee member
       CatchupValidation {
           height: u64,
           validation: CatchupValidation,
       },
       
       /// Consensus result with proofs
       CatchupConsensus {
           height: u64,
           block: Block,
           approvals: Vec<CatchupApproval>,
       },
   }
   ```

2. **Create `src/consensus/catchup_consensus.rs`**:
   - `CatchupBlockProposal` struct
   - `CatchupValidation` enum
   - `CatchupApproval` / `CatchupRejection` structs
   - `CatchupConsensusResult` enum
   - `CatchupConsensusState` tracker

### Phase 2: Committee Selection (2 hours)

1. **Add to `src/tsdc.rs`**:
   ```rust
   pub async fn select_catchup_validators(
       &self,
       height: u64,
       committee_size: usize,
   ) -> Result<Vec<MasternodeInfo>, TSCDError>
   ```

2. **Ensure deterministic selection**:
   - Sort by address
   - Use height-based hash for selection
   - All nodes select same committee

### Phase 3: Validator Logic (3-4 hours)

1. **Add to `src/blockchain.rs`**:
   ```rust
   pub async fn validate_catchup_proposal(
       &self,
       proposal: &CatchupBlockProposal,
   ) -> Result<CatchupValidation, String>
   ```

2. **Validation checks**:
   - Previous hash continuity
   - Timestamp validity
   - Merkle root correctness
   - UTXO consistency
   - Signature verification

### Phase 4: Leader Consensus Collection (3-4 hours)

1. **Add to catchup leader logic** (`src/main.rs`):
   - After producing block, create proposal
   - Broadcast to validator committee
   - Wait for validator responses (30s timeout)
   - Collect approvals/rejections
   - Check if consensus reached

2. **Consensus decision**:
   - If ≥2/3 approve: Broadcast with proofs, add to local chain
   - If <2/3 approve: Log failure, try backup leader or sync

### Phase 5: Message Handling (2-3 hours)

1. **Add to `src/network/peer_connection.rs`** or message handler:
   ```rust
   async fn handle_catchup_proposal(&mut self, proposal: CatchupBlockProposal) {
       // Check if we're a validator for this height
       // Validate the proposal
       // Send validation response
   }
   
   async fn handle_catchup_validation(&mut self, height: u64, validation: CatchupValidation) {
       // If we're the leader, collect validation
       // Update consensus state
   }
   
   async fn handle_catchup_consensus(&mut self, height: u64, block: Block, approvals: Vec<CatchupApproval>) {
       // Verify consensus proof (check signatures)
       // If valid, add block to chain
   }
   ```

### Phase 6: Testing (2-3 hours)

1. **Unit tests**:
   - Committee selection determinism
   - Validation logic
   - Consensus threshold calculation

2. **Integration tests**:
   - Multi-node catchup with consensus
   - Leader failure (backup leader takes over)
   - Validator disagreement scenarios

---

## Configuration

Add to `config.toml`:

```toml
[catchup_consensus]
# Enable Avalanche consensus for catchup block validation
enabled = true

# Consensus threshold (simple majority for Avalanche)
threshold_fraction = 0.51  # >50% of all masternodes

# Timeout for validator responses (seconds)
validator_timeout = 30

# Maximum rounds before falling back to sync
max_consensus_rounds = 3
```

---

## Rollout Strategy

### Stage 1: Non-Disruptive Addition
- Deploy code with `catchup_consensus.enabled = false`
- All nodes continue using current single-leader catchup
- Monitor for any issues with new code

### Stage 2: Gradual Enablement
- Enable on testnet first
- Monitor consensus success rate
- Enable on mainnet with 1-2 nodes initially
- Gradually enable on more nodes

### Stage 3: Full Deployment
- Enable on all masternodes
- Monitor for consensus failures
- Adjust timeouts/thresholds if needed

### Stage 4: Make Mandatory
- Remove single-leader fallback
- Require consensus for all catchup blocks

---

## Expected Benefits

### 1. Fork Prevention
- ✅ Bad blocks rejected by majority before acceptance
- ✅ Corrupted data detected early
- ✅ Network stays synchronized

### 2. Avalanche Consensus Properties
- ✅ Simple majority (>50%) sufficient for consensus
- ✅ Fast finality (30-60 seconds typical)
- ✅ Works with any odd number of masternodes (3, 5, 7, 9...)
- ✅ No single point of failure

### 3. Transparency
- ✅ Consensus proofs (signatures) stored with blocks
- ✅ Can audit which validators approved each block
- ✅ Can detect consistently bad actors

### 4. Network Reliability
- ✅ Backup leader selection if primary fails
- ✅ Falls back to peer sync if consensus fails
- ✅ Prevents network stalls

---

## Failure Scenarios & Handling

### Scenario 1: Leader Proposes Bad Block
**Detection**: Validators reject (fail validation checks)  
**Response**: 
- ≤50% approvals → consensus fails
- Try backup leader (attempt + 1)
- If backup also fails → fallback to peer sync

### Scenario 2: Network Split (50/50 Vote)
**Example**: 3 approve, 3 reject (6 masternodes)  
**Response**:
- Neither >50% threshold met → no consensus
- Timeout and retry with backup leader
- Backup leader may propose different block
- If still split → fallback to peer sync (indicates serious network issue)

### Scenario 3: Validators Can't Reach Leader
**Detection**: Leader doesn't receive validator responses  
**Response**:
- Timeout after 30s
- If ≤50% responses → leader tries again (broadcast proposal again)
- After 3 attempts → leader gives up, network tries backup leader

### Scenario 4: Validators Disagree (Split Vote)
**Example**: 2 approve, 2 reject, 1 offline (5 masternodes)  
**Response**:
- Not enough for >50% threshold (need 3 of 5)
- Consensus fails
- Try backup leader
- Backup leader may propose different block that majority can agree on

### Scenario 5: Network Partition
**Detection**: Leaders and validators on different network segments  
**Response**:
- Timeouts on both sides
- Each segment attempts consensus separately
- When partition heals, fork resolution mechanism handles it
- Longer chain (more work) wins

### Scenario 6: All Validators Offline
**Detection**: No responses after timeout  
**Response**:
- Leader cannot achieve consensus
- Falls back to sync from peers
- If no peers have blocks → network stalled (expected behavior)

---

## Performance Considerations

### Network Overhead
- **Messages per catchup block**: 
  - 1 proposal broadcast
  - N validator responses (N = number of active masternodes)
  - 1 consensus broadcast
  - Total: ~2 + N messages (vs 1 in current system)

- **For 5 masternodes**: ~7 messages per block (vs 1)
- **For 10 masternodes**: ~12 messages per block (vs 1)
- **Still acceptable**: Catchup blocks are rare (only when behind)

### Latency
- **Additional delay**: 30-60 seconds per block (for consensus)
- **Impact**: Catchup is slower but **safe**
- **Trade-off**: Better to be slow and correct than fast and forked

### Storage
- **Consensus proofs**: Store validator signatures with blocks
- **Size**: ~100 bytes per validator signature
  - 5 masternodes: ~500 bytes
  - 10 masternodes: ~1KB  
- **Benefit**: Can audit consensus history

---

## Monitoring & Metrics

### Key Metrics

1. **Catchup consensus success rate**: % of proposals that reach consensus (>50%)
2. **Average consensus time**: How long to get majority approvals
3. **Validator response rate**: % of validators that respond in time
4. **Rejection reasons**: Why validators reject proposals
5. **Backup leader usage**: How often primary leader fails

### Alerts

- ⚠️ **Consensus failure rate >10%**: Investigation needed
- ⚠️ **Validator consistently rejecting**: Possible bad actor or outdated software
- ⚠️ **Consensus timeout frequent**: Network connectivity issues
- 🚨 **No consensus for 5+ rounds**: Manual intervention required

### Logging

```
📋 Catchup consensus for height 5511:
   Leader: LW-Michigan2
   All Masternodes: 7 total
   Approvals: 5/7 (LW-Arizona, LW-London, LW-Texas, LW-Florida, LW-Nevada)
   Rejections: 2/7 (LW-Michigan: "previous_hash mismatch", LW-California: "UTXO invalid")
   Result: ✅ CONSENSUS REACHED (5/7 = 71% > 50% threshold)
   Time: 18.3s
```

---

## Migration Path

### Before Implementation
```rust
// Current: Single leader produces blocks
if is_catchup_leader {
    produce_block();
    broadcast_block();
}
```

### After Implementation
```rust
// New: Leader proposes, all masternodes validate using Avalanche
if is_catchup_leader {
    let block = produce_block();
    let proposal = create_proposal(block);
    broadcast_proposal(proposal);  // To ALL masternodes
    
    let validations = collect_validations(30_seconds).await;
    
    if has_majority_consensus(validations) {  // >50%
        broadcast_consensus(block, validations);
        add_block_locally(block);
    } else {
        log_consensus_failure(validations);
        // Backup leader will try
    }
} else {
    // ALL masternodes validate (Avalanche model)
    let validation = validate_proposal(proposal);
    send_validation(validation);
}
```

---

## Alternative Approaches Considered

### 1. BFT 2/3 Supermajority
**Idea**: Use 2/3 threshold like classical BFT  
**Rejected**: TimeCoin uses Avalanche (>50%), not BFT. Mixing models creates confusion.

### 2. Sampling Instead of Full Participation
**Idea**: Sample k masternodes like regular Avalanche  
**Rejected**: Catchup is infrequent enough that full participation is fine. Sampling adds complexity.

### 3. Leader Rotation Per Block
**Idea**: Different leader for each catchup block  
**Rejected**: Coordination overhead, current deterministic selection works

### 4. Peer Voting
**Idea**: All peers vote, not just committee  
**Rejected**: Too much network traffic, committee is sufficient

---

## Future Enhancements

1. **Adaptive committee size**: More validators for critical heights
2. **Reputation-based selection**: Prefer reliable validators
3. **Fast-path consensus**: Skip for uncontroversial blocks
4. **Concurrent proposals**: Multiple leaders propose in parallel
5. **Checkpoint consensus**: Extra validation at milestone heights

---

**Total Implementation Time**: 15-20 hours  
**Testing Time**: 5-8 hours  
**Documentation & Rollout**: 3-5 hours  
**Total**: ~25-30 hours (3-4 days of focused work)

---

**Priority**: HIGH - This fixes the root cause of network forks  
**Risk**: MEDIUM - Complex consensus protocol, needs thorough testing  
**Benefit**: HIGH - Prevents future network-wide fork failures
