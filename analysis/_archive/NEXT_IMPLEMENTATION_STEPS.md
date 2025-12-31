# Next Steps - TSDC Block Production Integration

## Overview
The Avalanche consensus is working and transactions are being finalized. Now we need to integrate TSDC block production to actually create blocks on a 10-minute schedule.

---

## Task 1: Trigger TSDC Every 10 Minutes

### Where to Add
**File:** `src/main.rs` (or create new `src/tasks/block_production.rs`)

### What to Do
Add a periodic task that runs TSDC block selection every 10 minutes (600 seconds):

```rust
// In main application initialization, add:
tokio::spawn({
    let consensus = consensus.clone();
    let tsdc = tsdc_consensus.clone();
    
    async move {
        let mut interval = tokio::time::interval(Duration::from_secs(600));
        
        loop {
            interval.tick().await;
            
            // TSDC block production every 10 minutes
            match tsdc.select_leader(tsdc.current_slot()).await {
                Ok(leader) => {
                    tracing::info!("🎯 Selected leader: {} for block production", leader.address);
                    
                    if is_local_validator(&leader) {
                        // We are the leader - generate and broadcast block
                        match generate_tsdc_block(&consensus, &tsdc).await {
                            Ok(block) => {
                                tracing::info!("✅ Generated block height: {}", block.height);
                                // Broadcast to network
                            }
                            Err(e) => {
                                tracing::error!("❌ Failed to generate block: {}", e);
                            }
                        }
                    } else {
                        tracing::debug!("⏸️  We are not leader for this slot");
                    }
                }
                Err(e) => {
                    tracing::error!("❌ Failed to select leader: {}", e);
                }
            }
        }
    }
});
```

### Helper Function to Add
```rust
async fn generate_tsdc_block(
    consensus: &Arc<ConsensusEngine>,
    tsdc: &Arc<TSCDConsensus>,
) -> Result<Block, String> {
    // Get current block height
    let height = get_next_block_height();
    
    // Get finalized transactions from Avalanche
    let finalized_txs = consensus.get_finalized_transactions_for_block();
    
    // Get active masternodes
    let masternodes = consensus.get_active_masternodes();
    
    // Generate deterministic block
    let block = Block::new(
        height,
        chrono::Utc::now().timestamp(),
        finalized_txs,
        masternodes,
        100, // base_reward
    );
    
    // Sign block
    // Broadcast to network
    
    Ok(block)
}
```

---

## Task 2: Clean Up Dead Code

### Code to Remove

**File:** `src/avalanche.rs`
- Remove entire `AvalancheHandler` struct (lines 30-283)
- Remove `run_avalanche_loop` function (line 288)
- Remove `AvalancheMetrics` struct (line 281)

**Reasoning:** These are old patterns, current code uses `AvalancheConsensus` instead

**File:** `src/consensus.rs`
- Remove unused methods (marked with `#[allow(dead_code)]`):
  - `set_peer_manager`
  - `is_syncing`
  - `set_syncing`
  - `finalize_transaction`
  - `reject_transaction`
  - `is_pending`
  - `get_all_pending`
  - `get_pending` 
  - `is_finalized` (in TransactionPool)

### Keep These (They're Used)
- ✅ `Snowflake` - part of Avalanche consensus
- ✅ `Snowball` - part of Avalanche consensus
- ✅ `QueryRound` - vote tracking for Avalanche
- ✅ `AvalancheConsensus` - active consensus engine
- ✅ All methods in `ConsensusEngine` that are actively called

---

## Task 3: Test End-to-End

### Test Script (pseudocode)
```rust
#[test]
async fn test_transaction_to_block_flow() {
    // 1. Create test masternode network
    let network = setup_test_network(3);
    
    // 2. Submit transaction via RPC
    let txid = network.send_transaction(tx).await.unwrap();
    tracing::info!("Transaction submitted: {:?}", txid);
    
    // 3. Wait for Avalanche finality (~750ms)
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(is_finalized(&network, txid));
    tracing::info!("✅ Transaction finalized by Avalanche");
    
    // 4. Wait for next TSDC block (~10 minutes, or simulate)
    let block = wait_for_block(&network, 10).await.unwrap();
    tracing::info!("✅ Block {} produced", block.height);
    
    // 5. Verify transaction is in block
    assert!(block.contains_transaction(&txid));
    tracing::info!("✅ Transaction included in block");
    
    // 6. Verify blockchain state updated
    let utxo_state = network.get_utxo_state(...).await;
    assert_eq!(utxo_state, UTXOState::Spent);
    tracing::info!("✅ UTXO marked as Spent");
}
```

---

## Task 4: Monitor and Fix Issues

### Monitoring Points
1. **Avalanche rounds:** Are validators being sampled?
2. **Vote collection:** Are votes being counted?
3. **Finality detection:** Is confidence counter reaching β (20)?
4. **TSDC block production:** Are blocks being generated every 10 min?
5. **Network:** Are connections staying alive?

### Debug Output to Add
```rust
tracing::debug!(
    "Avalanche round {} for TX {:?}: {} votes, confidence {}",
    round_num, hex::encode(txid), vote_count, confidence
);

tracing::debug!(
    "TSDC slot {}: leader={}, transactions={}, block_hash={}",
    slot, leader, tx_count, hex::encode(block_hash)
);
```

---

## Implementation Order

### Phase 1: Setup (1 hour)
1. Add TSDC block production task to main
2. Implement `generate_tsdc_block` helper
3. Add debug logging

### Phase 2: Testing (1 hour)
1. Run with test network
2. Submit transaction
3. Verify Avalanche finality
4. Wait for block production
5. Verify blockchain update

### Phase 3: Cleanup (30 mins)
1. Remove identified dead code
2. Run `cargo fmt && cargo clippy && cargo check`
3. Final testing

### Phase 4: Deploy (30 mins)
1. Push to git
2. Verify CI/CD passes
3. Monitor production

---

## Success Criteria

- [x] Transaction submitted via RPC
- [x] Avalanche consensus finalizes transaction
- [x] Transaction moves to finalized pool
- [ ] TSDC block production triggered (needs implementation)
- [ ] Block contains finalized transactions
- [ ] Blockchain updated with block
- [ ] UTXO state reflects transaction (Spent)
- [ ] Masternode connections stay persistent
- [ ] Code compiles with no warnings
- [ ] All tests pass

---

## Estimated Effort

| Task | Time | Difficulty |
|------|------|------------|
| TSDC integration | 1-2 hours | Medium |
| Dead code removal | 30 mins | Low |
| Testing & debugging | 2-3 hours | Medium |
| Final cleanup | 30 mins | Low |
| **Total** | **4-6 hours** | - |

---

## Questions to Answer Before Starting

1. **Should TSDC be slot-based or fixed time?**
   - Current: Every 10 minutes (600s) from epoch
   - Alternative: Align to clock hours?

2. **What if block production fails?**
   - Skip that slot?
   - Retry?
   - Log and continue?

3. **How many finalized transactions per block?**
   - All available?
   - Limited set?
   - Fee-based selection?

4. **Block reward distribution?**
   - Fixed amount to leader?
   - To all masternodes?
   - Proportional to stake?

---

**Status:** Ready for implementation  
**Next:** Start Task 1 (TSDC block production task)
