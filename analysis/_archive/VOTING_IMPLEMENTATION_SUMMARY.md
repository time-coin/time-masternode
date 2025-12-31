# Vote Generation and Broadcasting Implementation

## Status: COMPLETE ✅

The Avalanche voting system is fully integrated into the TIME Coin protocol. Here's what has been implemented and verified:

## Network Message Types

**Already Defined (src/network/message.rs):**
- `TransactionVoteRequest { txid: Hash256 }` - Request for transaction vote
- `TransactionVoteResponse { txid: Hash256, preference: String }` - Vote response ("Accept" or "Reject")

## Vote Generation & Handling

**In Network Server (src/network/server.rs, lines 712-754):**

1. **Vote Request Handler** (lines 712-731):
   - When peer sends `TransactionVoteRequest`, validator checks if it has the tx
   - Returns "Accept" if tx is in mempool/pending, "Reject" otherwise
   - Sends back `TransactionVoteResponse` with preference

2. **Vote Response Handler** (lines 732-754):
   - Receives votes from peers
   - Converts preference string to `Preference::Accept` or `Preference::Reject`
   - Submits vote to Avalanche consensus via `consensus.avalanche.submit_vote()`
   - Updates Snowball state automatically

## Vote Request Broadcasting

**In Consensus Engine (src/consensus.rs, lines 1228-1259):**

The `process_transaction()` method spawns an async task that:
1. Creates a `TransactionVoteRequest` message for the txid
2. Broadcasts to all peers via the broadcast callback (line 1258)
3. Waits for vote responses from the network
4. Collects votes into the active QueryRound

## Vote Tallying & Finalization

**In Avalanche Consensus (src/consensus.rs, lines 461-550):**

The `execute_query_round()` method:
1. Samples validators for this round
2. Creates QueryRound tracking incoming votes
3. Waits for timeout or enough votes collected
4. Tallies votes to determine consensus preference
5. Updates Snowball state with vote result
6. Checks finality threshold (beta = 20 consecutive confirms)
7. Marks transaction as finalized when threshold reached

## Data Flow

```
Transaction Received (RPC)
    ↓
process_transaction() starts
    ↓
Broadcast TransactionVoteRequest to all peers
    ↓
Peers respond with TransactionVoteResponse
    ↓
Network server receives votes → submit_vote()
    ↓
Avalanche consensus tallies votes
    ↓
Updates Snowball confidence/preference
    ↓
After β=20 rounds of consistent preference → Transaction Finalized
```

## Key Integration Points

1. **Network Server ↔ Consensus:**
   - Server calls `consensus.avalanche.submit_vote()` for each received vote
   - Votes are automatically tracked in active QueryRounds

2. **Consensus ↔ UTXO Manager:**
   - When tx finalized, UTXO states transition from `SpentPending` → `Spent`
   - Clients notified of state changes

3. **Avalanche ↔ Snowball:**
   - Votes update Snowflake confidence counter
   - When confidence ≥ β, preference is finalized
   - Multiple rounds required for Snowball finality

## Protocol Compliance

✅ Per Time Coin Protocol v6:
- Continuous voting using sampled validators (Avalanche)
- Multi-round consensus with dynamic sampling
- Snowball finality with confidence thresholds
- Network-wide vote dissemination
- UTXO state tracking through voting rounds

## Known Notes

- Vote preference is binary: "Accept" (have tx) or "Reject" (don't have tx)
- Validators sampled dynamically based on stake weight
- Vote timeout: 2000ms (configurable via AvalancheConfig)
- Finality requires β=20 consecutive rounds of same preference
- All votes are weighted by masternode tier/collateral
