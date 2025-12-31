# Consensus Migration: BFT to Avalanche + TSDC

## Current Status

### ✅ Completed
1. Removed BFT/Consensus module imports from `main.rs`
2. Updated `app_context.rs` to use `AvalancheSampler` instead of `ConsensusEngine` and `BFTConsensus`
3. Removed BFT block processor task from main.rs
4. Updated RPC server initialization to use Avalanche sampler

### ⚠️ In Progress
- Removing BFT/Consensus references from `blockchain.rs` (major refactor needed)

### ❌ TODO

1. **Update blockchain.rs**
   - Replace `ConsensusEngine` with `AvalancheSampler`
   - Remove all `BFTConsensus` references
   - Remove `process_bft_committed_blocks()` method
   - Remove `set_bft_consensus()` method
   - Update `new()` constructor signature
   - Update block production logic to use Avalanche finality instead of BFT voting

2. **Enhance avalanche_consensus.rs**
   - Implement Snowball algorithm properly
   - Add sampling query/response handlers
   - Implement confidence tracking
   - Add finality threshold logic (β = 20 consecutive successes)
   - Add peer weight-based sampling

3. **Create tsdc_consensus.rs** (New Module)
   - Time-Scheduled Deterministic Consensus
   - VRF-based leader selection
   - 10-minute block scheduling
   - Deterministic block content ordering

4. **Update RPC server (rpc/server.rs)**
   - Change signature from `consensus_engine` to `avalanche_sampler`
   - Update query methods to use Avalanche finality instead of BFT votes

5. **Update Network Message Handlers**
   - Handle `SampleQuery` messages
   - Handle `SampleResponse` messages
   - Remove BFT message handlers

6. **Update Tests**
   - Remove BFT consensus tests
   - Add Avalanche consensus tests
   - Add TSDC scheduling tests

## Architecture Overview

```
Transaction Flow (Avalanche):
├─ Broadcast → Masternodes
├─ Lock UTXO (local)
├─ Snowball Sampling:
│  ├─ Query k random peers (stake-weighted)
│  ├─ Tally responses (need ≥α agree)
│  ├─ Increment confidence if majority agrees
│  └─ Repeat until confidence ≥ β
├─ Finalization (instant, <1s typically)
└─ Move to Finalized Pool

Block Creation (TSDC):
├─ 10-minute schedule
├─ VRF-based leader selection
├─ Leader packages finalized transactions
├─ Deterministic ordering (lexicographic by TXID)
└─ Broadcast to network

No longer used:
- BFT voting rounds
- Global quorum calculations
- Vote aggregation messages
```

## Files to Delete
- `src/bft_consensus.rs` - DELETED (BFT implementation)
- `src/consensus.rs` - DELETED (old consensus engine)

## Files to Create
- `src/avalanche_consensus.rs` - Enhanced with full Snowball (needs completion)
- `src/tsdc_consensus.rs` - Time-Scheduled Deterministic Consensus (new)

## Files to Modify
- `src/blockchain.rs` - Replace consensus references
- `src/rpc/server.rs` - Update method signatures  
- `src/network/server.rs` - Add Avalanche message handlers
- `src/network/message.rs` - Add Avalanche message types

## Priority
1. **CRITICAL**: Update `blockchain.rs` - blocks all compilation
2. **HIGH**: Implement `tsdc_consensus.rs` - needed for block scheduling
3. **HIGH**: Enhance `avalanche_consensus.rs` - needed for transaction finality
4. **MEDIUM**: Update RPC and network handlers
5. **LOW**: Add tests and documentation

## Notes
- Avalanche provides instant finality for transactions
- TSDC provides deterministic block scheduling every 10 minutes
- No global voting or quorum requirements
- Stake-weighted sampling provides Sybil resistance
- Pre-block finality means state is immutable before block creation

