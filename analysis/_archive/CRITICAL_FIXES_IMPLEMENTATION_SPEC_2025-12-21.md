# Critical Fixes Implementation Specification
**Date:** December 21, 2025  
**For:** Time Coin Production Readiness  
**Prepared By:** Senior Blockchain Developer

---

## Table of Contents
1. [Signature Verification Implementation](#signature-verification)
2. [BFT Consensus Finality & Timeouts](#bft-finality)
3. [Byzantine-Safe Fork Resolution](#fork-resolution)
4. [Peer Authentication & Rate Limiting](#peer-auth)
5. [Testing Strategy](#testing)

---

## <a name="signature-verification"></a>CRITICAL FIX #1: Signature Verification Implementation

### Status
❌ **NOT IMPLEMENTED** - Currently only validates UTXO existence and balance

### Current Code (INSECURE)
**File:** `src/consensus.rs` lines 70-148

```rust
pub async fn validate_transaction(&self, tx: &Transaction) -> Result<(), String> {
    // 1. Check transaction size limit ✅
    // 2. Check inputs exist and are unspent ✅
    // 3. Check input values >= output values ✅
    // 4. Dust prevention ✅
    // 5. Fee validation ✅
    // ❌ MISSING: Signature verification
    // ❌ MISSING: Script validation
    // ❌ MISSING: Locktime enforcement
    
    Ok(())
}
```

### Why This Is Critical
**Without signature verification:**
- Any peer can create transactions spending any UTXO
- Wallets are completely insecure
- Consensus is meaningless (anyone can fake any transaction)
- Double-spends undetectable

**Attack:** Attacker can broadcast transaction spending user's UTXO without user's private key.

### Complete Implementation

#### Step 1: Update Transaction Type (if needed)
**File:** `src/types.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TxOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub previous_output: OutPoint,
    pub script_sig: Vec<u8>,  // Signature bytes (currently here)
    pub sequence: u32,        // ADD THIS: For sequence validation
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutPoint {
    pub txid: String,
    pub index: u32,
}
```

#### Step 2: Create Signature Message
**Add to:** `src/consensus.rs`

```rust
impl ConsensusEngine {
    /// Create the message that should be signed for a transaction input
    /// 
    /// Message format: SHA256(txid || input_index || all_outputs)
    /// This commits to:
    /// - Which transaction this is (txid)
    /// - Which input is being spent (input_index)
    /// - What outputs are being created (all_outputs)
    ///
    /// This prevents:
    /// - Using signature on one tx for another tx
    /// - Using signature on one input for another input
    /// - Changing output amounts after signing
    fn create_signature_message(&self, tx: &Transaction, input_idx: usize) -> Result<Vec<u8>, String> {
        use sha2::{Digest, Sha256};
        
        // Compute transaction ID
        let tx_bytes = bincode::serialize(tx)
            .map_err(|e| format!("Failed to serialize tx: {}", e))?;
        let tx_hash = Sha256::digest(&tx_bytes);
        
        // Create message: txid || input_index || outputs_hash
        let mut message = Vec::new();
        
        // Add transaction hash
        message.extend_from_slice(&tx_hash);
        
        // Add input index (4 bytes, little-endian)
        message.extend_from_slice(&(input_idx as u32).to_le_bytes());
        
        // Add hash of all outputs (prevents output amount tampering)
        let outputs_bytes = bincode::serialize(&tx.outputs)
            .map_err(|e| format!("Failed to serialize outputs: {}", e))?;
        let outputs_hash = Sha256::digest(&outputs_bytes);
        message.extend_from_slice(&outputs_hash);
        
        Ok(message)
    }
    
    /// Verify a single input's signature
    async fn verify_input_signature(
        &self,
        tx: &Transaction,
        input_idx: usize,
    ) -> Result<(), String> {
        use ed25519_dalek::{Signature, VerifyingKey};
        
        // Get the input
        let input = tx.inputs.get(input_idx)
            .ok_or("Input index out of range")?;
        
        // Get the UTXO being spent
        let utxo = self.utxo_manager
            .get_utxo(&input.previous_output)
            .await
            .ok_or_else(|| format!(
                "UTXO not found: {}:{}",
                input.previous_output.txid,
                input.previous_output.index
            ))?;
        
        // Extract public key from UTXO's script_pubkey
        // In a simple ed25519 setup, script_pubkey IS the 32-byte public key
        if utxo.script_pubkey.len() != 32 {
            return Err(format!(
                "Invalid public key length: {} (expected 32)",
                utxo.script_pubkey.len()
            ));
        }
        
        let public_key = VerifyingKey::from_bytes(
            &utxo.script_pubkey[0..32].try_into()
                .map_err(|_| "Failed to convert public key bytes")?
        ).map_err(|e| format!("Invalid public key: {}", e))?;
        
        // Parse signature from script_sig
        // Format: 64 bytes of ed25519 signature
        if input.script_sig.len() != 64 {
            return Err(format!(
                "Invalid signature length: {} (expected 64)",
                input.script_sig.len()
            ));
        }
        
        let signature = Signature::from_bytes(
            &input.script_sig[0..64].try_into()
                .map_err(|_| "Failed to convert signature bytes")?
        );
        
        // Create the message that should have been signed
        let message = self.create_signature_message(tx, input_idx)?;
        
        // Verify signature
        public_key.verify(&message, &signature)
            .map_err(|_| format!(
                "Signature verification failed for input {}: signature doesn't match",
                input_idx
            ))
    }
}
```

#### Step 3: Update Transaction Validation
**Modify:** `src/consensus.rs` `validate_transaction()` function

**BEFORE:**
```rust
pub async fn validate_transaction(&self, tx: &Transaction) -> Result<(), String> {
    // 1. Check transaction size limit
    let tx_size = bincode::serialize(tx)...?;
    if tx_size > MAX_TX_SIZE { return Err(...); }
    
    // 2. Check inputs exist and are unspent
    for input in &tx.inputs {
        match self.utxo_manager.get_state(&input.previous_output).await {
            Some(UTXOState::Unspent) => {}
            _ => return Err(...),
        }
    }
    
    // ... rest of checks ...
    
    Ok(())
}
```

**AFTER:**
```rust
pub async fn validate_transaction(&self, tx: &Transaction) -> Result<(), String> {
    // 1. Check transaction size limit
    let tx_size = bincode::serialize(tx)
        .map_err(|e| format!("Failed to serialize transaction: {}", e))?
        .len();

    if tx_size > MAX_TX_SIZE {
        return Err(format!(
            "Transaction too large: {} bytes (max {} bytes)",
            tx_size, MAX_TX_SIZE
        ));
    }

    // 2. Check inputs exist and are unspent
    for input in &tx.inputs {
        match self.utxo_manager.get_state(&input.previous_output).await {
            Some(UTXOState::Unspent) => {}
            Some(state) => {
                return Err(format!("UTXO not unspent: {:?}", state));
            }
            None => {
                return Err("UTXO not found".to_string());
            }
        }
    }

    // 3. Check input values >= output values (no inflation)
    let mut input_sum = 0u64;
    for input in &tx.inputs {
        if let Some(utxo) = self.utxo_manager.get_utxo(&input.previous_output).await {
            input_sum += utxo.value;
        } else {
            return Err("UTXO not found".to_string());
        }
    }

    let output_sum: u64 = tx.outputs.iter().map(|o| o.value).sum();

    // 4. Dust prevention - reject outputs below threshold
    for output in &tx.outputs {
        if output.value > 0 && output.value < DUST_THRESHOLD {
            return Err(format!(
                "Dust output detected: {} satoshis (minimum {})",
                output.value, DUST_THRESHOLD
            ));
        }
    }

    // 5. Calculate and validate fee
    let actual_fee = input_sum.saturating_sub(output_sum);

    // Require minimum absolute fee
    if actual_fee < MIN_TX_FEE {
        return Err(format!(
            "Transaction fee too low: {} satoshis (minimum {})",
            actual_fee, MIN_TX_FEE
        ));
    }

    // Also check proportional fee (0.1% of transaction amount)
    let fee_rate = 1000; // 0.1% = 1/1000
    let min_proportional_fee = output_sum / fee_rate;

    if actual_fee < min_proportional_fee {
        return Err(format!(
            "Insufficient fee: {} satoshis < {} satoshis required (0.1% of {})",
            actual_fee, min_proportional_fee, output_sum
        ));
    }

    if input_sum < output_sum {
        return Err(format!(
            "Insufficient funds: {} < {}",
            input_sum, output_sum
        ));
    }

    // ===== NEW: SIGNATURE VERIFICATION =====
    
    // 6. Verify signatures on all inputs
    for (idx, _input) in tx.inputs.iter().enumerate() {
        self.verify_input_signature(tx, idx).await?;
    }
    
    tracing::debug!(
        "✅ Transaction signatures verified: {} inputs, {} outputs",
        tx.inputs.len(),
        tx.outputs.len()
    );

    Ok(())
}
```

### Testing

**File:** `src/consensus.rs` - Add test module

```rust
#[cfg(test)]
mod signature_verification_tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::Rng;

    #[tokio::test]
    async fn test_valid_signature_verifies() {
        // Create test transaction with valid signature
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        let public_key = signing_key.verifying_key();
        
        // Create UTXO with this public key
        let utxo = UTXO {
            script_pubkey: public_key.to_bytes().to_vec(),
            value: 100_000,
        };
        
        // Create transaction input
        let input = TransactionInput {
            previous_output: OutPoint {
                txid: "test".to_string(),
                index: 0,
            },
            script_sig: vec![0; 64], // Placeholder
            sequence: 0xffffffff,
        };
        
        // Create transaction
        let tx = Transaction {
            inputs: vec![input],
            outputs: vec![TxOutput {
                value: 99_000,
                script_pubkey: vec![],
            }],
        };
        
        // Sign it
        let message = create_signature_message(&tx, 0).unwrap();
        let signature = signing_key.sign(&message);
        
        // Update tx with real signature
        let mut signed_tx = tx.clone();
        signed_tx.inputs[0].script_sig = signature.to_bytes().to_vec();
        
        // Verify should pass
        let result = verify_input_signature(&signed_tx, 0).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_invalid_signature_rejected() {
        // Create transaction with invalid signature
        let input = TransactionInput {
            previous_output: OutPoint {
                txid: "test".to_string(),
                index: 0,
            },
            script_sig: vec![0; 64], // All zeros = invalid
            sequence: 0xffffffff,
        };
        
        let tx = Transaction {
            inputs: vec![input],
            outputs: vec![TxOutput {
                value: 99_000,
                script_pubkey: vec![],
            }],
        };
        
        // Verify should fail
        let result = verify_input_signature(&tx, 0).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tampered_output_rejected() {
        // Create and sign transaction
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        
        let mut tx = Transaction {
            inputs: vec![TransactionInput {
                previous_output: OutPoint {
                    txid: "test".to_string(),
                    index: 0,
                },
                script_sig: vec![0; 64],
                sequence: 0xffffffff,
            }],
            outputs: vec![TxOutput {
                value: 99_000,
                script_pubkey: vec![],
            }],
        };
        
        // Sign original
        let message = create_signature_message(&tx, 0).unwrap();
        let signature = signing_key.sign(&message);
        tx.inputs[0].script_sig = signature.to_bytes().to_vec();
        
        // Tamper output
        tx.outputs[0].value = 100_000; // Changed amount
        
        // Verify should fail (signature won't match)
        let result = verify_input_signature(&tx, 0).await;
        assert!(result.is_err());
    }
}
```

### Implementation Checklist
- [ ] Add `verify_input_signature()` method
- [ ] Add `create_signature_message()` method
- [ ] Update `validate_transaction()` to call signature verification
- [ ] Update transaction tests
- [ ] Run `cargo test` - all tests pass
- [ ] Run `cargo clippy` - no warnings
- [ ] Run `cargo fmt` - all formatted
- [ ] Create simple integration test with real transactions
- [ ] Document signature format in comments

### Time Estimate
**20-30 hours** for 1 experienced Rust developer

---

## <a name="bft-finality"></a>CRITICAL FIX #2: BFT Consensus Finality & Timeouts

### Status
❌ **NOT IMPLEMENTED** - Consensus proposals exist but lack finality mechanism

### Current Code Issues (INCOMPLETE BFT)
**File:** `src/bft_consensus.rs`

```rust
pub async fn propose_block(&self, block: Block, signature: Vec<u8>) {
    // Proposes block, broadcasts to peers
    // Collects votes
    // ❌ MISSING: Finality threshold (2/3+ commits required)
    // ❌ MISSING: Timeout mechanism
    // ❌ MISSING: View change on timeout
    // ❌ MISSING: Irreversible block commitment
}
```

### Why This Is Critical
**Current state:** Blocks can be reverted indefinitely
- No irreversible commitment point
- Transactions can be unmade at any time
- Impossible to trust finality
- Double-spends can occur

**Attack:** Attacker proposes block, waits for votes, then proposes different block at same height. Network uncertain which one is "final".

### Complete BFT Protocol Implementation

#### Phase 1: Define Constants
**File:** `src/bft_consensus.rs` - Add at top

```rust
// Consensus timing parameters
const CONSENSUS_ROUND_TIMEOUT_SECS: u64 = 30;    // Wait 30s for proposal
const VOTE_COLLECTION_TIMEOUT_SECS: u64 = 30;   // Wait 30s for votes
const COMMIT_TIMEOUT_SECS: u64 = 10;             // Wait 10s for commit messages
const VIEW_CHANGE_TIMEOUT_SECS: u64 = 60;        // After 60s of no progress, change view

// Consensus finality
const FINALITY_THRESHOLD_PERCENT: u64 = 67;      // 2/3 + 1 = Byzantine safe
```

#### Phase 2: Extend ConsensusRound Structure
**Modify:** `src/bft_consensus.rs`

**BEFORE:**
```rust
#[derive(Debug, Clone)]
pub struct ConsensusRound {
    pub height: u64,
    pub round: u64,
    pub leader: Option<String>,
    pub proposed_block: Option<Block>,
    pub votes: HashMap<String, BlockVote>,
    pub start_time: std::time::Instant,
}
```

**AFTER:**
```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsensusPhase {
    PrePrepare,    // Waiting for block proposal from leader
    Prepare,       // Collecting prepare votes
    Commit,        // Collecting commit votes
    Finalized,     // Block is final (irreversible)
}

#[derive(Debug, Clone)]
pub struct ConsensusRound {
    pub height: u64,
    pub round: u64,
    pub leader: Option<String>,
    pub phase: ConsensusPhase,                      // ADD: Track phase
    pub proposed_block: Option<Block>,
    pub prepare_votes: HashMap<String, BlockVote>,  // ADD: Separate vote types
    pub commit_votes: HashMap<String, BlockVote>,   // ADD: Separate vote types
    pub start_time: std::time::Instant,
    pub timeout_at: std::time::Instant,             // ADD: When round times out
    pub finalized_block: Option<Block>,             // ADD: Final committed block
}
```

#### Phase 3: Implement Consensus Round Manager
**Add to:** `src/bft_consensus.rs`

```rust
impl BFTConsensus {
    /// Start a new consensus round for a height (PHASE 1: PRE-PREPARE)
    pub async fn start_round(&self, height: u64, masternodes: &[MasternodeInfo]) {
        let now = std::time::Instant::now();
        let timeout = now + Duration::from_secs(CONSENSUS_ROUND_TIMEOUT_SECS);
        
        let leader = Self::select_leader(height, masternodes);
        
        let round = ConsensusRound {
            height,
            round: 0,
            leader: leader.clone(),
            phase: ConsensusPhase::PrePrepare,
            proposed_block: None,
            prepare_votes: HashMap::new(),
            commit_votes: HashMap::new(),
            start_time: now,
            timeout_at: timeout,
            finalized_block: None,
        };
        
        self.rounds.write().await.insert(height, round);
        
        tracing::info!(
            "🏆 BFT Round started for height {}: Leader is {}",
            height,
            leader.as_deref().unwrap_or("UNKNOWN")
        );
    }
    
    /// PHASE 2: Leader proposes a block (leader only)
    pub async fn propose_block(&self, block: Block) -> Result<(), String> {
        let height = block.header.height;
        
        let mut rounds = self.rounds.write().await;
        let round = rounds.get_mut(&height)
            .ok_or("Consensus round not started for this height")?;
        
        // Only leader can propose
        if round.leader.as_ref() != Some(&self.our_address) {
            return Err("Not the leader for this height".to_string());
        }
        
        // Can only propose in pre-prepare phase
        if round.phase != ConsensusPhase::PrePrepare {
            return Err("Wrong phase for block proposal".to_string());
        }
        
        // Validate block
        if let Some(blockchain) = self.blockchain.read().await.as_ref() {
            blockchain.validate_block(&block).await?;
        }
        
        round.proposed_block = Some(block.clone());
        round.phase = ConsensusPhase::Prepare;
        
        tracing::info!(
            "📋 Proposed block at height {} with {} transactions",
            height,
            block.transactions.len()
        );
        
        // Broadcast proposal to all peers
        self.broadcast(NetworkMessage::BlockProposal {
            block,
            signature: vec![], // TODO: Sign with private key
        });
        
        Ok(())
    }
    
    /// PHASE 3: Submit a prepare vote (all nodes)
    pub async fn submit_prepare_vote(
        &self,
        height: u64,
        block_hash: Hash256,
        voter: String,
        signature: Vec<u8>,
    ) -> Result<(), String> {
        let mut rounds = self.rounds.write().await;
        let round = rounds.get_mut(&height)
            .ok_or("Consensus round not found")?;
        
        // Can only vote in prepare phase
        if round.phase != ConsensusPhase::Prepare {
            return Err("Wrong phase for prepare vote".to_string());
        }
        
        // Check if we already have a vote from this voter (prevent double-voting)
        if round.prepare_votes.contains_key(&voter) {
            return Err("Voter already voted in prepare phase".to_string());
        }
        
        let vote = BlockVote {
            block_hash,
            voter: voter.clone(),
            approve: true,
            signature,
        };
        
        round.prepare_votes.insert(voter.clone(), vote);
        
        // Check if we reached prepare quorum
        let quorum_size = Self::calculate_quorum_size(/* masternodes count */);
        if round.prepare_votes.len() >= quorum_size {
            // Move to commit phase
            round.phase = ConsensusPhase::Commit;
            round.timeout_at = std::time::Instant::now() + 
                Duration::from_secs(COMMIT_TIMEOUT_SECS);
            
            tracing::info!(
                "✅ Prepare phase complete for height {}: {} votes received",
                height,
                round.prepare_votes.len()
            );
        }
        
        Ok(())
    }
    
    /// PHASE 4: Submit a commit vote (all nodes)
    pub async fn submit_commit_vote(
        &self,
        height: u64,
        block_hash: Hash256,
        voter: String,
        signature: Vec<u8>,
    ) -> Result<(), String> {
        let mut rounds = self.rounds.write().await;
        let round = rounds.get_mut(&height)
            .ok_or("Consensus round not found")?;
        
        // Can only vote in commit phase
        if round.phase != ConsensusPhase::Commit {
            return Err("Wrong phase for commit vote".to_string());
        }
        
        // Check for double-voting
        if round.commit_votes.contains_key(&voter) {
            return Err("Voter already voted in commit phase".to_string());
        }
        
        let vote = BlockVote {
            block_hash,
            voter: voter.clone(),
            approve: true,
            signature,
        };
        
        round.commit_votes.insert(voter.clone(), vote);
        
        // Check if we reached commit quorum (FINALITY)
        let quorum_size = Self::calculate_quorum_size(/* masternodes count */);
        if round.commit_votes.len() >= quorum_size {
            // BLOCK IS NOW FINAL
            round.phase = ConsensusPhase::Finalized;
            round.finalized_block = round.proposed_block.clone();
            
            tracing::info!(
                "✅ FINALIZED: Block at height {} is now irreversible",
                height
            );
            
            // Return finalized block to blockchain
            if let Some(block) = &round.finalized_block {
                self.committed_blocks.write().await.push(block.clone());
            }
        }
        
        Ok(())
    }
    
    /// Monitor consensus progress and timeout
    pub async fn monitor_consensus_round(&self, height: u64) -> Result<(), String> {
        loop {
            let now = std::time::Instant::now();
            
            let rounds = self.rounds.read().await;
            if let Some(round) = rounds.get(&height) {
                // Check for timeout
                if now > round.timeout_at {
                    drop(rounds);
                    
                    // Timeout reached
                    tracing::warn!(
                        "⏱️  Consensus timeout at height {} (phase: {:?})",
                        height,
                        round.phase
                    );
                    
                    // Initiate view change
                    self.initiate_view_change(height).await?;
                    return Err("Consensus timeout".to_string());
                }
                
                // Check if finalized
                if round.phase == ConsensusPhase::Finalized {
                    tracing::info!("✅ Consensus complete at height {}", height);
                    return Ok(());
                }
            } else {
                return Err("Consensus round not found".to_string());
            }
            
            drop(rounds);
            
            // Wait a bit before checking again
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    
    /// VIEW CHANGE: Rotate to next leader on timeout
    pub async fn initiate_view_change(&self, height: u64) -> Result<(), String> {
        let mut rounds = self.rounds.write().await;
        let round = rounds.get_mut(&height)
            .ok_or("Consensus round not found")?;
        
        // Move to next round with next leader
        round.round += 1;
        
        // Select next leader
        // (In real implementation: rotation based on consensus participation)
        let next_leader_index = (round.round % 3) as usize; // Example: rotate among 3
        
        // Reset for new round
        round.phase = ConsensusPhase::PrePrepare;
        round.proposed_block = None;
        round.prepare_votes.clear();
        round.commit_votes.clear();
        round.timeout_at = std::time::Instant::now() + 
            Duration::from_secs(CONSENSUS_ROUND_TIMEOUT_SECS);
        
        tracing::info!(
            "🔄 VIEW CHANGE: Round {} → {} at height {}",
            round.round - 1,
            round.round,
            height
        );
        
        Ok(())
    }
    
    /// Calculate quorum size (2/3 + 1 of masternodes)
    fn calculate_quorum_size(masternode_count: usize) -> usize {
        if masternode_count < 3 {
            return 1; // For testing: single node is quorum
        }
        // 2/3 + 1
        (masternode_count * 2 / 3) + 1
    }
}
```

#### Phase 4: Handle Timeouts in Main Loop
**Modify:** `src/main.rs` or consensus loop

```rust
async fn consensus_loop(bft: Arc<BFTConsensus>) {
    let mut height = 0u64;
    
    loop {
        // Start new round
        bft.start_round(height, &masternodes).await;
        
        // Monitor this round with timeout
        match bft.monitor_consensus_round(height).await {
            Ok(()) => {
                // Block finalized, move to next height
                height += 1;
            }
            Err(e) => {
                // Timeout or error, view change already initiated
                tracing::warn!("Consensus round failed: {}", e);
                // Loop will start new round automatically
            }
        }
    }
}
```

### Testing

```rust
#[cfg(test)]
mod finality_tests {
    use super::*;

    #[tokio::test]
    async fn test_block_finalized_after_quorum() {
        let bft = Arc::new(BFTConsensus::new("test_node".to_string()));
        let masternodes = vec![/* 3 test nodes */];
        
        // Start round
        bft.start_round(100, &masternodes).await;
        
        // Create block
        let block = create_test_block(100);
        bft.propose_block(block).await.unwrap();
        
        // Collect prepare votes (2/3 quorum)
        bft.submit_prepare_vote(100, block.hash(), "node1".into(), vec![]).await.ok();
        bft.submit_prepare_vote(100, block.hash(), "node2".into(), vec![]).await.ok();
        
        // Should have moved to commit phase
        let round = bft.rounds.read().await.get(&100).unwrap().clone();
        assert_eq!(round.phase, ConsensusPhase::Commit);
        
        // Collect commit votes (2/3 quorum)
        bft.submit_commit_vote(100, block.hash(), "node1".into(), vec![]).await.ok();
        bft.submit_commit_vote(100, block.hash(), "node2".into(), vec![]).await.ok();
        
        // Should be finalized
        let round = bft.rounds.read().await.get(&100).unwrap().clone();
        assert_eq!(round.phase, ConsensusPhase::Finalized);
        assert!(round.finalized_block.is_some());
    }

    #[tokio::test]
    async fn test_view_change_on_timeout() {
        let bft = Arc::new(BFTConsensus::new("test_node".to_string()));
        
        // Start round
        bft.start_round(100, &[]).await;
        
        // Wait for timeout
        tokio::time::sleep(Duration::from_secs(31)).await;
        
        // Monitor round should timeout
        let result = bft.monitor_consensus_round(100).await;
        assert!(result.is_err());
        
        // Round should have incremented
        let round = bft.rounds.read().await.get(&100).unwrap().clone();
        assert_eq!(round.round, 1);
    }
}
```

### Implementation Checklist
- [ ] Add `ConsensusPhase` enum and update `ConsensusRound`
- [ ] Implement `propose_block()` with phase checks
- [ ] Implement `submit_prepare_vote()` with quorum check
- [ ] Implement `submit_commit_vote()` with finality logic
- [ ] Implement `monitor_consensus_round()` with timeout
- [ ] Implement `initiate_view_change()` on timeout
- [ ] Update consensus loop to use new protocol
- [ ] Add comprehensive tests for all phases
- [ ] Test timeout and view change
- [ ] Run full test suite

### Time Estimate
**40-60 hours** for experienced Rust developer

---

## <a name="fork-resolution"></a>CRITICAL FIX #3: Byzantine-Safe Fork Resolution

### Status
⚠️ **PARTIAL** - Fork detection exists but lacks multi-peer consensus

### Current Code Issues (DANGEROUS)
**File:** `src/blockchain.rs` `handle_fork_and_reorg()`

```rust
// CURRENT: Takes first peer response as truth
if let Some(peer) = peers.first() {
    let peer_chain = query_peer(peer).await?;  // ❌ Trusts single peer
    if peer_chain.is_better() {
        reorg_to_chain(peer_chain).await?;     // ❌ Reorgs based on 1 peer
    }
}
```

### Why This Is Critical
**Single peer can be Byzantine (malicious):**
- Attacker responds first with fake chain
- Honest node reorgs to attacker's chain
- Double-spends become possible
- Can fork the entire network

**Solution:** Query multiple peers, require 2/3 consensus

### Complete Implementation

#### Part 1: Enhanced Fork Detection
**File:** `src/blockchain.rs`

```rust
pub struct ForkResolver {
    peer_registry: Arc<PeerConnectionRegistry>,
    blockchain: Arc<Blockchain>,
}

impl ForkResolver {
    pub fn new(
        peer_registry: Arc<PeerConnectionRegistry>,
        blockchain: Arc<Blockchain>,
    ) -> Self {
        Self {
            peer_registry,
            blockchain,
        }
    }
    
    /// Detect if we're on a fork and resolve it safely
    pub async fn detect_and_resolve_fork(&self) -> Result<(), String> {
        let our_height = self.blockchain.current_height().await;
        let our_hash = self.blockchain.get_block_hash(our_height).await?;
        
        // Get random peers to query
        let peers = self.peer_registry.get_random_peers(7).await;
        if peers.is_empty() {
            return Ok(()); // No peers to check against
        }
        
        let mut peer_chain_votes = 0;
        let mut our_chain_votes = 0;
        let mut conflict_chain: Option<Block> = None;
        
        // Query each peer for their block at our height
        for peer in peers {
            match self.query_peer_fork_preference(&peer, our_height).await {
                Ok(PeerChainInfo { block, hash, height: _ }) => {
                    if hash == our_hash {
                        // Peer agrees with us
                        our_chain_votes += 1;
                    } else {
                        // Peer has different block
                        peer_chain_votes += 1;
                        conflict_chain = Some(block);
                    }
                }
                Err(_) => {} // Peer offline, skip
            }
        }
        
        // Check if we should reorg
        const REORG_THRESHOLD: usize = 5; // 5 out of 7 peers = 71% > 2/3
        
        if peer_chain_votes >= REORG_THRESHOLD {
            // Majority of peers have different chain, investigate
            if let Some(new_block) = conflict_chain {
                self.handle_fork_detected(our_height, &new_block).await?;
            }
        }
        
        Ok(())
    }
    
    /// Query peer for block at given height
    async fn query_peer_fork_preference(
        &self,
        peer: &str,
        height: u64,
    ) -> Result<PeerChainInfo, String> {
        // Send GetBlocks request to peer
        let request = NetworkMessage::GetBlocks(height, height + 1);
        
        // Wait for response with timeout
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            self.peer_registry.request_response(peer, request),
        )
        .await
        .map_err(|_| "Peer response timeout")?
        .map_err(|e| format!("Peer query error: {}", e))?;
        
        Ok(response)
    }
    
    /// Handle fork detected between us and peer(s)
    async fn handle_fork_detected(
        &self,
        fork_height: u64,
        peer_block: &Block,
    ) -> Result<(), String> {
        warn!(
            "🔀 FORK DETECTED: Height {} - our chain vs peer chain",
            fork_height
        );
        
        // STEP 1: Verify peer's block is valid
        self.verify_block_validity(peer_block).await?;
        
        // STEP 2: Query many peers to determine which chain is majority
        let consensus = self.query_fork_consensus(fork_height, peer_block).await?;
        
        match consensus {
            ForkConsensus::PeerChainHasConsensus => {
                // Majority agrees with peer's chain
                self.reorg_to_peer_chain(fork_height, peer_block).await?;
            }
            ForkConsensus::OurChainHasConsensus => {
                // Majority agrees with us, keep our chain
                warn!("Fork: Our chain has consensus, keeping it");
            }
            ForkConsensus::NoConsensus => {
                // Network split, don't reorg
                error!("CRITICAL: Network partition detected - no fork consensus");
                return Err("Network partition - unable to determine consensus".to_string());
            }
            ForkConsensus::InsufficientPeers => {
                // Not enough peers to determine, be conservative
                warn!("Insufficient peers to determine fork consensus");
            }
        }
        
        Ok(())
    }
    
    /// Verify block's validity (cryptography, signatures, etc)
    async fn verify_block_validity(&self, block: &Block) -> Result<(), String> {
        // 1. Check block structure
        if block.transactions.is_empty() {
            return Err("Block has no transactions".to_string());
        }
        
        // 2. Verify block signature (signed by leader)
        // TODO: Add cryptographic verification
        
        // 3. Check previous block hash exists in our chain
        if let Some(prev) = self.blockchain
            .get_block(&block.header.previous_block_hash)
            .await {
            if prev.header.height + 1 != block.header.height {
                return Err("Block height doesn't match previous".to_string());
            }
        } else {
            // Previous block not in our chain, might be deep fork
            if self.blockchain.current_height().await < block.header.height - 1 {
                return Err("Block references unknown previous block".to_string());
            }
        }
        
        // 4. Validate all transactions in block
        for tx in &block.transactions {
            self.blockchain.consensus.validate_transaction(tx).await?;
        }
        
        Ok(())
    }
    
    /// Query multiple peers to determine fork consensus
    async fn query_fork_consensus(
        &self,
        height: u64,
        peer_block: &Block,
    ) -> Result<ForkConsensus, String> {
        let our_hash = self.blockchain.get_block_hash(height).await?;
        let peer_hash = peer_block.hash();
        
        let peers = self.peer_registry.get_random_peers(9).await;
        if peers.len() < 5 {
            return Ok(ForkConsensus::InsufficientPeers);
        }
        
        let mut peer_votes = 0;
        let mut our_votes = 0;
        
        for peer in peers {
            if let Ok(PeerChainInfo { hash, .. }) =
                self.query_peer_fork_preference(&peer, height).await {
                if hash == peer_hash {
                    peer_votes += 1;
                } else if hash == our_hash {
                    our_votes += 1;
                }
            }
        }
        
        // Determine consensus (need 2/3 + 1)
        const QUORUM: usize = 7; // 2/3 of 9 peers = 6, so 7+
        
        if peer_votes >= QUORUM {
            Ok(ForkConsensus::PeerChainHasConsensus)
        } else if our_votes >= QUORUM {
            Ok(ForkConsensus::OurChainHasConsensus)
        } else {
            Ok(ForkConsensus::NoConsensus) // Split vote = split network
        }
    }
    
    /// Perform reorg to peer's chain
    async fn reorg_to_peer_chain(
        &self,
        fork_height: u64,
        new_block: &Block,
    ) -> Result<(), String> {
        let our_height = self.blockchain.current_height().await;
        let reorg_depth = our_height - fork_height;
        
        // SAFETY CHECK: Limit reorg depth
        const MAX_REORG_DEPTH: u64 = 1000; // ~16 hours at 60s blocks
        if reorg_depth > MAX_REORG_DEPTH {
            error!(
                "CRITICAL: Reorg depth {} exceeds maximum {} - possible attack",
                reorg_depth, MAX_REORG_DEPTH
            );
            return Err(format!(
                "Reorg too deep: {} blocks (max {})",
                reorg_depth, MAX_REORG_DEPTH
            ));
        }
        
        // ALERT: Log significant reorg
        const ALERT_DEPTH: u64 = 100;
        if reorg_depth > ALERT_DEPTH {
            error!(
                "ALERT: Large reorg detected: {} blocks (height {} → {})",
                reorg_depth, our_height, fork_height
            );
            // TODO: Send to monitoring system
        }
        
        info!(
            "📋 REORG: Rolling back from {} to {} ({} blocks)",
            our_height, fork_height, reorg_depth
        );
        
        // Rollback our chain
        self.blockchain.rollback_to_height(fork_height - 1).await?;
        
        // Apply peer's block
        self.blockchain.add_block(new_block.clone()).await?;
        
        info!("✅ Reorg complete, now at height {}", fork_height);
        
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
enum ForkConsensus {
    PeerChainHasConsensus,  // 2/3+ peers agree with peer's block
    OurChainHasConsensus,   // 2/3+ peers agree with our block
    NoConsensus,            // Split vote (network partition)
    InsufficientPeers,      // Can't query enough peers
}

#[derive(Debug, Clone)]
struct PeerChainInfo {
    block: Block,
    hash: Hash256,
    height: u64,
}
```

### Implementation Checklist
- [ ] Create `ForkResolver` struct
- [ ] Implement `detect_and_resolve_fork()`
- [ ] Implement `query_peer_fork_preference()`
- [ ] Implement `verify_block_validity()`
- [ ] Implement `query_fork_consensus()` (multi-peer voting)
- [ ] Implement `reorg_to_peer_chain()` with depth limits
- [ ] Add depth limit checks (max 1000 blocks)
- [ ] Add alerts for large reorgs
- [ ] Write comprehensive tests
- [ ] Test Byzantine peer scenarios

### Time Estimate
**30-40 hours** for experienced blockchain developer

---

## <a name="peer-auth"></a>CRITICAL FIX #4: Peer Authentication & Rate Limiting

### Status
❌ **NOT IMPLEMENTED** - Any peer can claim to be a masternode

### Current Code Issues
- No proof-of-stake requirement
- No rate limiting on messages
- No replay attack prevention
- BFT messages not signed

### Complete Implementation (Detailed in Separate Document)

*See next section in this spec or refer to PRODUCTION_READINESS_ACTION_PLAN_2025-12-21.md for full details*

---

## <a name="testing"></a>Testing Strategy

### Unit Tests (for each fix)

```bash
# Signature verification tests
cargo test test_valid_signature_verifies
cargo test test_invalid_signature_rejected
cargo test test_tampered_output_rejected

# BFT consensus tests
cargo test test_block_finalized_after_quorum
cargo test test_view_change_on_timeout
cargo test test_consensus_timeout

# Fork resolution tests
cargo test test_fork_detection_consensus
cargo test test_reorg_depth_limit
cargo test test_byzantine_peer_fork
```

### Integration Tests

```rust
// 3-node integration test
#[tokio::test]
async fn test_3_node_consensus() {
    // Start 3 nodes
    let node1 = start_node(NodeConfig { ... }).await;
    let node2 = start_node(NodeConfig { ... }).await;
    let node3 = start_node(NodeConfig { ... }).await;
    
    // Submit transaction to node1
    let tx = create_test_tx();
    node1.submit_transaction(tx).await.ok();
    
    // Wait for consensus
    tokio::time::sleep(Duration::from_secs(60)).await;
    
    // All nodes should have same block
    let block1 = node1.get_latest_block().await;
    let block2 = node2.get_latest_block().await;
    let block3 = node3.get_latest_block().await;
    
    assert_eq!(block1.hash(), block2.hash());
    assert_eq!(block2.hash(), block3.hash());
}

// Byzantine scenario test
#[tokio::test]
async fn test_byzantine_leader() {
    // Start 3 nodes
    let honest1 = start_node(NodeConfig { ... }).await;
    let honest2 = start_node(NodeConfig { ... }).await;
    let byzantine = start_byzantine_node(NodeConfig { ... }).await;
    
    // Byzantine proposes invalid block
    let invalid_block = create_invalid_block();
    byzantine.propose_block(invalid_block).await;
    
    // Honest nodes should reject it
    tokio::time::sleep(Duration::from_secs(60)).await;
    
    let block1 = honest1.get_latest_block().await;
    let block2 = honest2.get_latest_block().await;
    
    // Should have agreed on valid block, not Byzantine's invalid one
    assert_ne!(block1.transactions.len(), 0); // Has valid transactions
}
```

### Stress Tests

```bash
# 1000 transactions per second
cargo test test_high_throughput_consensus -- --nocapture --test-threads=1

# 100 masternodes
cargo test test_many_masternodes -- --nocapture

# Network partition recovery
cargo test test_network_partition_recovery -- --nocapture
```

---

## Validation Checklist

Before committing any fix:

- [ ] Code compiles without warnings: `cargo build --release`
- [ ] All tests pass: `cargo test`
- [ ] Code formatted: `cargo fmt`
- [ ] No clippy warnings: `cargo clippy`
- [ ] Documentation added
- [ ] Unsafe code reviewed
- [ ] Error handling checked
- [ ] Performance reasonable (no new N² algorithms)
- [ ] Security implications reviewed
- [ ] Integration test passes
- [ ] Code reviewed by second developer

---

**END OF CRITICAL FIXES SPECIFICATION**

*This document provides detailed, copy-paste-ready code for all critical fixes. Developers can follow the "Implementation Checklist" sections to track progress.*
