# Transaction Submission Flow in TimeCoin

## Overview

When a transaction is submitted to a masternode in TimeCoin, it goes through Avalanche consensus for instant finality. The transaction is finalized in seconds, not at block time.

## Detailed Flow

### Step 1: Transaction Submission
```
Client submits transaction to masternode via RPC/P2P
↓
consensus_engine.submit_transaction(tx)
```

### Step 2: Atomic Validation & Locking
```
lock_and_validate_transaction(tx):
  ├─ Check transaction format
  ├─ Verify signatures (Ed25519)
  ├─ Verify inputs exist and are unspent
  ├─ Check for double-spending attempt
  ├─ Verify total input >= total output + fee
  ├─ Atomically lock all inputs (prevent double-spend)
  └─ Return: OK or Error

If validation fails:
  └─ Reject transaction immediately
     Return error to client
```

### Step 3: Network Broadcast
```
broadcast(NetworkMessage::TransactionBroadcast(tx))
  ├─ Send to all connected peers
  └─ Each peer receives tx and starts voting
```

### Step 4: Add to Transaction Pool
```
process_transaction(tx):
  ├─ Mark all inputs as "SpentPending" in UTXO state
  │  (prevents double-spending by other transactions)
  │
  ├─ Notify clients of state change
  │  (via state_notifier for real-time updates)
  │
  ├─ Broadcast UTXO state updates to peers
  │  (NetworkMessage::UTXOStateUpdate)
  │
  ├─ Calculate transaction fee
  │
  ├─ Add to mempool (TransactionPool)
  │  (tracks as "pending" transaction)
  │
  └─ If this node is a masternode:
       ├─ Auto-vote YES for transaction
       └─ Create and broadcast vote to network
```

### Step 5: Avalanche Consensus (Parallel Process)

**Voting happens independently in background:**

```
On other masternodes receiving the transaction:
  ├─ Validate transaction locally
  ├─ If valid: vote YES
  ├─ Broadcast vote to network
  └─ Track vote count

Finalization happens when:
  ├─ Transaction receives 2/3+ masternode votes, OR
  └─ Consensus reached via Avalanche protocol
```

### Step 6: Transaction Finalization

```
When 2/3+ votes received:
  ├─ Mark transaction as FINALIZED
  ├─ Update all inputs to "Spent" (permanent)
  ├─ Update outputs to "Unspent" 
  ├─ Create finality proof
  ├─ Notify clients: "Transaction Finalized"
  └─ Remove from pending pool
     (add to confirmed transactions)
```

### Step 7: Block Inclusion

```
When TSDC block producer is elected (every 1 hour):
  ├─ Collect all finalized transactions
  ├─ Package into new block
  ├─ Distribute rewards to masternodes
  ├─ Broadcast block to network
  └─ Validators add block to chain

Note: Transactions are already finalized BEFORE block time
      Blocks just create permanent, ordered record
```

## Timeline

```
T+0s    : Transaction submitted to masternode
T+0-5s  : Validation, broadcast, voting begins
T+5-10s : Consensus reached (2/3+ votes)
T+10s   : Transaction FINALIZED ✓
          (clients can see: tx confirmed, outputs spendable)

Later (on 1-hour boundary):
T+1h    : Transaction included in TSDC block
          (creates permanent record)
```

## Key Points

### Finality is NOT Block-Based
- ❌ NOT waiting for next block
- ✅ Instant finality via Avalanche voting (5-10 seconds)
- Blocks are just historical records

### Inputs are Protected
```
State progression:
  Unspent → SpentPending (when tx submitted)
          → Spent (when tx finalized)
          → Included in Block (permanent record)
```

### No Emergency Block Generation
- Blocks are produced on TSDC schedule (every 1 hour)
- Transactions don't wait for blocks to finalize
- If node is behind on blocks, it syncs from peers
- Finalized transactions are never lost

### Fees
- Minimum: 1,000 satoshis (0.00001 TIME)
- Calculated as: inputs - outputs
- Used for:
  - Spam prevention
  - May be used for prioritization in future
  - Added to block rewards

## What Masternode Does

When a masternode receives a transaction:

1. **Validate** - Check it's well-formed and inputs are real
2. **Vote** - Broadcast YES vote if valid
3. **Track** - Store in mempool until finalized
4. **Finalize** - Mark as confirmed when consensus reached
5. **Include** - Add to next TSDC block (when elected)
6. **Broadcast** - Forward to peers for distribution

## UTXO State Machine

```
┌─────────┐
│ Unspent │  (Available to spend)
└────┬────┘
     │ transaction submitted
     ▼
┌─────────────┐
│SpentPending │  (Locked, being voted on)
└────┬────────┘
     │ 2/3+ votes received (finalization)
     ▼
┌──────────┐
│  Spent   │  (Confirmed, cannot double-spend)
└────┬─────┘
     │ included in block
     ▼
┌──────────────┐
│ Spent (Final)│  (Permanent record)
└──────────────┘
```

## Error Cases

**Invalid Signature**
- Rejected immediately
- Error: "Signature verification failed"

**Double-Spending**
- Rejected immediately
- Error: "Input already locked" or "Input already spent"

**Insufficient Balance**
- Rejected immediately
- Error: "Input sum < output sum + fee"

**Mempool Full**
- Rejected immediately
- Error: "Mempool full: X transactions (max Y)"

**Invalid UTXO**
- Rejected immediately
- Error: "UTXO not found" or "UTXO already spent"

## Consensus Requirements

- **Minimum Masternodes**: 3 (for votes to have meaning)
- **Finality**: 2/3 + 1 masternodes must vote YES
- **Timeout**: Votes tracked until transaction finalized
- **Broadcast**: Votes gossipped to all nodes (Avalanche style)

## Transaction Tracking for Clients

Clients can query:
```
is_transaction_finalized(txid)
  → Returns: true/false

get_transaction_confirmations(txid)
  → Returns: 0 (pending) or 1+ (finalized & in blocks)

get_utxo_state(outpoint)
  → Returns: Unspent | SpentPending | Spent | Finalized
```

## Summary

**The key insight**: TimeCoin uses **Avalanche for finality**, **TSDC for history**.

- Transactions are **finalized in seconds** (Avalanche voting)
- Blocks are **produced hourly** (TSDC schedule)
- Finality is **instant and permanent**
- Blocks are just **append-only historical record**

This is fundamentally different from traditional blockchains where finality = next block. Here, finality = consensus vote, block = record keeping.
