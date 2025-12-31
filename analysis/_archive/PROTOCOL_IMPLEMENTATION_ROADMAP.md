# TIME Coin Protocol Implementation Roadmap

**Focus:** Implementation ONLY (no code removal)  
**Date:** December 23, 2024  
**Goal:** Complete working TIME Coin protocol  

---

## Current Status

✅ **DONE:**
- Avalanche consensus integrated into transaction processing
- Transaction validation with UTXO locking
- RPC: send_raw_transaction() and create_raw_transaction()
- Masternode tier system (Free, Bronze, Silver, Gold)
- Heartbeat attestation system
- Phase 1: AVS Snapshots - Masternode voting power captured
- Phase 2a: Vote Infrastructure - Query rounds and peer communication
- Phase 2b: Avalanche Query Round Execution (partial)
- Phase 2c: Vote Tallying (partial)
- Phase 2d: Snowball Finalization (partial)
- TSDC checkpointing framework (partial)

❌ **CRITICAL - IN PROGRESS:**
- [ ] Complete Avalanche query round execution with peer sampling
- [ ] Complete vote generation and broadcasting
- [ ] Complete vote accumulation and finality checks
- [ ] TSDC block production (10-minute blocks)
- [ ] VRF-based leader selection
- [ ] Block broadcasting to network
- [ ] UTXO archival after block inclusion

---

## Implementation Priority

### 🔴 CRITICAL PATH (Must Have)

#### 1. TSDC Block Production Engine
**Files:** `src/tsdc.rs`, `src/main.rs`
**What:** Produce blocks every 10 minutes via TSDC
**Effort:** 2-3 days

**Components:**
- [ ] Slot timer (10-minute intervals)
- [ ] VRF-based leader selection
- [ ] Block assembly from finalized transactions
- [ ] Block broadcasting to network
- [ ] Block validation by all nodes
- [ ] UTXO state transition to Archived

**Protocol Spec Reference:**
```
From TIMECOIN_PROTOCOL_V5.md:

"10-minute mark arrives"
"Leader packages pool into Block N"
"State moves to Archived"
"Block is appended to chain; rewards are paid"
```

**Dependency:** ⚠️ Blocks other features
**Impact:** Enables checkpointing and reward distribution

---

#### 2. Peer Voting for Avalanche
**Files:** `src/network/server.rs`, `src/consensus.rs`
**What:** Peers vote on transaction validity
**Effort:** 1-2 days

**Components:**
- [ ] Receive preference requests from peers
- [ ] Send our preference (Accept/Reject)
- [ ] Update Snowball state with votes
- [ ] Handle vote tallying in query rounds
- [ ] Broadcast updated preference to network

**Protocol Spec Reference:**
```
"Ask for their preferred state (Valid/Invalid) regarding Tx"
"If ≥ α peers respond Valid → Increment confidence"
```

**Dependency:** Blocks finality (currently simulated)
**Impact:** Real consensus instead of simulation

---

#### 3. Block Persistence
**Files:** `src/blockchain.rs`, `src/storage.rs`
**What:** Save blocks to disk
**Effort:** 1 day

**Components:**
- [ ] Serialize block to storage
- [ ] Maintain block chain index
- [ ] Load blocks on startup
- [ ] Verify chain integrity

**Dependency:** Needed after TSDC
**Impact:** Persistent ledger

---

### 🟡 HIGH PRIORITY (Important)

#### 4. Reward Distribution
**Files:** `src/blockchain.rs`, `src/masternode_registry.rs`
**What:** Pay block rewards to leader
**Effort:** 1 day

**Components:**
- [ ] Calculate block reward (e.g., 10 TIME per block)
- [ ] Send to leader wallet
- [ ] Track reward history
- [ ] Distribute to masternodes (future)

**Dependency:** After TSDC
**Impact:** Economic incentives

---

#### 5. Network Synchronization
**Files:** `src/network/client.rs`, `src/network/server.rs`
**What:** New nodes catch up to network
**Effort:** 2 days

**Components:**
- [ ] Request blocks from peers
- [ ] Validate downloaded blocks
- [ ] Update local state
- [ ] Get current finalized transactions

**Dependency:** After block persistence
**Impact:** Node bootstrapping

---

### 🟢 MEDIUM PRIORITY (Nice to Have)

#### 6. RPC Methods
**Files:** `src/rpc/handler.rs`
**What:** Additional query methods
**Effort:** 1 day per method

**Methods:**
- `getbestblockhash()` - Latest block hash
- `getblockhash(height)` - Block at height
- `getblockheader(hash)` - Block details
- `gettxconfirmations(txid)` - How many blocks confirm tx

**Dependency:** After block persistence
**Impact:** Better RPC API

---

## Implementation Plan

### Week 1 (This Week)

**Priority Order:**
1. **TSDC Block Production** (Days 1-2, 2-3 days)
   - Implement slot timer
   - Implement VRF leader selection
   - Implement block assembly
   - Test block creation

2. **Peer Voting** (Days 3-4, 1-2 days)
   - Implement network voting
   - Update Snowball with peer votes
   - Test finality with actual peers

3. **Block Persistence** (Day 5, 1 day)
   - Save blocks to disk
   - Load on startup
   - Verify chain

### Week 2 (Next Week)

4. **Reward Distribution** (1 day)
5. **Network Sync** (2 days)
6. **RPC Enhancements** (1 day)

---

## What NOT to Do (Focus)

❌ Don't remove dead code
❌ Don't refactor networking
❌ Don't add new consensus algorithms
❌ Don't optimize performance yet

✅ DO focus on:
- Protocol implementation
- Making TSDC work
- Making Avalanche real (with voting)
- Making blocks work
- Making rewards work

---

## Success Criteria

After implementation:

1. ✅ Transactions finalize via Avalanche (<1 second)
2. ✅ Blocks produced every 10 minutes via TSDC
3. ✅ Blocks contain all finalized transactions
4. ✅ Blocks are persistent (survive node restart)
5. ✅ New nodes can sync from network
6. ✅ Block rewards paid to leader
7. ✅ Full TIME Coin protocol working

---

## Next Step: Start TSDC Implementation

Ready to begin with **TSDC Block Production Engine**?

The code structure is already in place in `src/tsdc.rs`. We just need to:
1. Start the TSDC consensus engine in main.rs
2. Wire the 10-minute timer
3. Implement VRF-based leader selection
4. Create the block assembly loop

Estimated: 2-3 days to have working 10-minute block production.

