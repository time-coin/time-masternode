# TimeCoin - Quick Reference Guide

## Current Architecture

### Consensus Layers
1. **Avalanche** - Transaction finality (5-15 seconds)
2. **TSDC** - Block production (every 1 hour)

### Transaction Lifecycle
```
Avalanche finalizes transaction in seconds
  ↓
Included in next TSDC block (at 1-hour boundary)
  ↓
Block broadcast to network
```

## Consensus Status

| Mechanism | Status | Location |
|-----------|--------|----------|
| **Avalanche** | ✅ Ready to activate | `src/avalanche_tx_handler.rs` |
| **TSDC** | ✅ Implemented | `src/tsdc.rs` |
| **BFT** | ❌ Deprecated | `src/consensus.rs` (legacy) |

## Block Time
- **1 hour** (3600 seconds)
- Status updates: Every 5 minutes

## Key Files

### Core Consensus
- `src/avalanche_consensus.rs` - Avalanche protocol
- `src/avalanche_tx_handler.rs` - Transaction handler (NEW)
- `src/tsdc.rs` - TSDC block production
- `src/blockchain.rs` - Block validation

### Network
- `src/network/server.rs` - P2P server
- `src/network/message.rs` - Message types
- `src/rpc/handler.rs` - JSON-RPC API

## Documentation

### Design Documents
- `CONSENSUS_MECHANISM_STATUS.md` - BFT vs Avalanche comparison
- `AVALANCHE_ACTIVATION.md` - Integration guide
- `NO_CATCHUP_SYSTEM.md` - Why no emergency catchup needed
- `TRANSACTION_FLOW.md` - How transactions work

### Implementation Docs
- `TSDC_IMPLEMENTATION.md` - TSDC details
- `AVALANCHE_ACTIVATION_COMPLETE.md` - What was done

## API Quick Ref

### Submit Transaction (Current - BFT)
```rust
consensus_engine.submit_transaction(tx).await?
```

### Submit Transaction (New - Avalanche)
```rust
avalanche_handler.submit_transaction(tx).await?
```

### Check Finality
```rust
consensus_engine.avalanche.is_finalized(&txid)
```

## Network Messages

### BFT (Legacy)
- `TransactionBroadcast` - Broadcast transaction
- `TransactionVote` - Vote on transaction
- `TransactionFinalized` - Announce finality
- `TransactionRejected` - Announce rejection

### Avalanche (New)
- `AvalancheQuery` - Query validator preference
- `AvalancheQueryResponse` - Validator's preference
- (Replaces explicit voting)

## Configuration

### Avalanche
```rust
AvalancheConfig {
    sample_size: 20,         // Query 20 validators per round (k)
    finality_confidence: 15, // 15 consecutive confirms (β)
    query_timeout_ms: 2000,  // 2 second timeout
    max_rounds: 100,         // Max 100 rounds
}
```

### TSDC
```
Block time: 3600 seconds (1 hour)
Leader selection: VRF (Verifiable Random Function)
Finality: After Avalanche consensus + block production
```

## Masternode Tiers

| Tier | Weight | Purpose |
|------|--------|---------|
| Gold | 100x | Preferred block producers |
| Silver | 10x | Regular producers |
| Bronze | 1x | Backup producers |
| Free | 1x | Community participation |

## Transaction Fees

- **Minimum**: 1,000 satoshis (0.00001 TIME)
- **Calculation**: inputs - outputs
- **Usage**: Spam prevention (future prioritization)

## UTXO States

```
Unspent
  ↓ (transaction submitted)
SpentPending (being voted on in Avalanche)
  ↓ (Avalanche finalization)
Spent (confirmed, in block)
  ↓ (after N confirmations)
SpentFinalized (permanent)
```

## Testing

### Check Build
```bash
cargo check
```

### Format Code
```bash
cargo fmt
```

### Lint Check
```bash
cargo clippy
```

### Build Release
```bash
cargo build --release
```

## Troubleshooting

### "No masternodes available"
- Need at least 3 active masternodes for consensus
- Check masternode registration

### "Avalanche consensus timeout"
- Transaction rejected after 100 rounds
- Check validator availability
- May indicate network partition

### "Transaction not in pending pool"
- Transaction already finalized or rejected
- Check transaction ID

## Performance Targets

| Metric | Target |
|--------|--------|
| **Tx Finality Time** | < 1 second (Avalanche) |
| **Block Production** | Once per hour (TSDC) |
| **Validators Sampled** | 20 per round |
| **Consensus Rounds** | 15 for finality |
| **Max TPS** | Thousands (Avalanche) |

## Node Roles

### Validator/Masternode
- Registers with masternode registry
- Participates in Avalanche voting
- Produces TSDC blocks (if elected)
- Earns rewards

### Full Node
- Validates blocks
- Syncs blockchain
- Retransmits transactions
- No rewards

### Light Client
- Tracks state
- Submits transactions
- Minimal storage

---

## What's New vs Old

### Old (BFT-era)
- ❌ Catchup mode for emergency block generation
- ❌ 2/3 quorum voting on every transaction
- ❌ Limited to ~30 validators
- ❌ Finality depends on block time

### New (Avalanche + TSDC)
- ✅ No catchup needed - TSDC is deterministic
- ✅ Random sampling of validators
- ✅ Scales to 1000s of validators
- ✅ Instant finality (seconds, not blocks)

---

**Last Updated**: 2025-12-23
**Status**: Ready for integration and testing
