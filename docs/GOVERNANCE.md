# TIME Coin On-Chain Governance

On-chain governance allows the masternode network to collectively change protocol parameters and disburse treasury funds without requiring a hard fork or operator intervention. Proposals are submitted, voted on, and executed entirely through the consensus layer.

---

## How It Works

### 1. Submit a Proposal

Any active **Bronze, Silver, or Gold** masternode can submit a proposal via the `submitproposal` RPC. Free-tier nodes cannot submit or vote.

```bash
# Treasury disbursement
time-cli submitproposal treasury <recipient_address> <amount_TIME> "<description>"

# Fee schedule change
time-cli submitproposal feeschedule <new_min_fee_TIME> '<[{"upper":100,"rate_bps":100},{"upper":1000,"rate_bps":50}]>'
```

The proposal is broadcast to all peers immediately and stored on-chain.

### 2. Voting Window

After a proposal is submitted, a **1,008-block voting window** opens (~1 week at 10 min/block). During this window, active Bronze/Silver/Gold masternodes cast YES or NO votes:

```bash
time-cli voteproposal <proposal_id> yes
time-cli voteproposal <proposal_id> no
```

Votes are signed with the masternode's Ed25519 key and gossiped to all peers. A duplicate vote (same node, same decision) is silently ignored; a changed vote replaces the previous one.

### 3. Execution

At the block where the voting window closes, `timed` tallies all YES votes by governance weight. If YES weight ≥ **67% of total active governance weight**, the proposal passes and executes automatically in that same block. No further action is needed.

If quorum is not met, the proposal is marked Failed and has no effect.

---

## Governance Weights

Votes are not equal — they are weighted by the voter's tier collateral:

| Tier | Governance Weight | Notes |
|------|------------------:|-------|
| Free | 0 (cannot vote) | No stake, no governance access |
| Bronze | 1 | 1,000 TIME collateral |
| Silver | 10 | 10,000 TIME collateral |
| Gold | 100 | 100,000 TIME collateral |

**Example:** 5 Gold nodes (500 weight) + 10 Silver nodes (100 weight) + 20 Bronze nodes (20 weight) = 620 total weight. A proposal needs at least ⌈620 × 0.67⌉ = 416 YES weight to pass.

---

## Proposal Types

### TreasurySpend

Disburses funds from the on-chain treasury to a recipient address.

```
Parameters:
  recipient   — TIME address to receive funds
  amount      — amount in TIME (must be ≤ current treasury balance)
  description — reason (max 256 bytes)
```

The treasury accumulates **5 TIME per block** from block rewards. Treasury balance is visible in `getblockchaininfo`. Funds are released atomically in the block where the proposal executes — there is no delay between passage and disbursement.

### FeeScheduleChange

Replaces the active fee schedule (minimum fee + tiered rates).

```
Parameters:
  new_min_fee   — new minimum fee in TIME
  new_tiers     — JSON array of {upper_bound_TIME, rate_basis_points}
                  tiers must be ordered ascending by upper_bound
```

The new schedule takes effect immediately in the same block the proposal executes and applies to all subsequent transactions.

---

## RPC Reference

### `submitproposal`

Submit a governance proposal.

```bash
# Treasury spend
time-cli submitproposal treasury TIME1xyz... 500 "Fund exchange listing"

# Fee schedule change (example: lower fees)
time-cli submitproposal feeschedule 0.005 '[{"upper":100,"rate_bps":50},{"upper":1000,"rate_bps":25},{"upper":10000,"rate_bps":10},{"upper":999999999,"rate_bps":5}]'
```

**Returns:** `{ "proposal_id": "<64-hex>" }`

**Errors:**
- Node is not an active Bronze/Silver/Gold masternode
- Wallet not unlocked
- TreasurySpend amount exceeds treasury balance
- Description exceeds 256 bytes
- Fee tiers not ordered ascending

---

### `voteproposal`

Cast a YES or NO vote on an active proposal.

```bash
time-cli voteproposal <proposal_id> yes
time-cli voteproposal <proposal_id> no
```

**Returns:** `{ "recorded": true }` (or `false` if duplicate)

**Errors:**
- Node is not an active Bronze/Silver/Gold masternode
- Proposal not found or not in Active status
- Voting window has closed

---

### `listproposals`

List all known governance proposals.

```bash
# All proposals
time-cli listproposals

# Filter by status: active, passed, failed, executed
time-cli listproposals active
```

**Returns:** Array of proposal objects:

```json
[
  {
    "id": "a1b2c3d4...",
    "type": "TreasurySpend",
    "submitter": "192.168.1.10:24000",
    "submit_height": 1000,
    "vote_end_height": 2008,
    "status": "Active",
    "payload": {
      "recipient": "TIME1xyz...",
      "amount": 500.0,
      "description": "Fund exchange listing"
    }
  }
]
```

---

### `getproposal`

Get full detail for a single proposal, including vote tally.

```bash
time-cli getproposal <proposal_id>
```

**Returns:**

```json
{
  "id": "a1b2c3d4...",
  "type": "TreasurySpend",
  "status": "Active",
  "submit_height": 1000,
  "vote_end_height": 2008,
  "yes_weight": 210,
  "total_weight": 620,
  "quorum_pct": 33.87,
  "quorum_required_pct": 67.0,
  "votes": [
    { "voter": "10.0.0.1:24000", "approve": true, "weight": 100, "height": 1050 },
    { "voter": "10.0.0.2:24000", "approve": false, "weight": 10, "height": 1060 }
  ],
  "payload": { ... }
}
```

---

## Lifecycle States

```
Submitted → Active (voting window open)
                │
      vote_end_height reached
                │
         ┌──────┴──────┐
      yes ≥ 67%     yes < 67%
         │               │
       Passed          Failed
         │
    Executed in same block
```

A proposal cannot be cancelled once submitted. If it fails, a corrected version can be resubmitted.

---

## Security Properties

- **Ed25519-signed** — every proposal and vote is signed by the submitter's/voter's masternode key; the network rejects any unsigned or invalid-signature message
- **Idempotent gossip** — duplicate proposals and votes are silently dropped; the network converges to the same state regardless of arrival order
- **Sled-persistent** — proposals and votes survive daemon restarts; state is reloaded at startup
- **Atomic execution** — TreasurySpend and FeeScheduleChange execute atomically in the same block they pass; there is no window between "passed" and "applied"
- **Quorum requires stake** — 67% of *governance weight* (not node count) prevents a swarm of Bronze nodes from overriding Gold consensus

---

## Parameters

| Parameter | Value |
|-----------|------:|
| Voting period | 1,008 blocks (~1 week) |
| Quorum threshold | 67% of total active governance weight |
| Eligible voters | Bronze, Silver, Gold masternodes |
| Treasury accumulation | 5 TIME per block |
| Max proposal description | 256 bytes |
| Proposal storage | sled DB (`gov_proposal_<id>`) |
| Vote storage | sled DB (`gov_vote_<id>_<address>`) |
