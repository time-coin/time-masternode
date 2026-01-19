# TimeCoin Fee Collection Mechanism

## Overview
Transaction fees ARE being collected and added to block rewards. This document explains how the fee collection system works.

## Fee Flow: Step-by-Step

### 1. Transaction Submission
**Location:** `src/consensus.rs:1343-1437` (validate_transaction)

When a user submits a transaction:
```rust
// Fee calculation
let input_sum: u64 = sum of all input UTXOs
let output_sum: u64 = sum of all outputs
let fee = input_sum - output_sum

// Fee validation
if fee < MIN_TX_FEE (1,000 satoshis) {
    reject
}
if fee < output_sum / 1000 (0.1% proportional) {
    reject
}
```

**Example Transaction:**
- Input: 1,000,000 satoshis (0.01 TIME)
- Output: 998,000 satoshis
- **Fee: 2,000 satoshis** (0.2% - meets both minimums)

---

### 2. Transaction Pool Storage
**Location:** `src/transaction_pool.rs:100-136` (add_transaction)

Transaction entry includes fee:
```rust
pub struct TransactionEntry {
    pub tx: Transaction,
    pub fee: u64,              // ← Fee stored here
    pub size: usize,
    pub added_at: Instant,
    pub submitter: Option<String>,
}
```

Stored in mempool:
- **Pending pool**: Transactions awaiting finalization
- **Finalized pool**: Transactions ready for block inclusion

---

### 3. Fee Accumulation
**Location:** `src/transaction_pool.rs:236-238` (get_total_fees)

When block is produced, total fees calculated:
```rust
pub fn get_total_fees(&self) -> u64 {
    self.finalized
        .iter()
        .map(|e| e.value().fee)
        .sum()
}
```

This sums ALL fees from finalized transactions ready for the block.

---

### 4. Block Reward Calculation
**Location:** `src/blockchain.rs:1526-1532` (produce_block)

```rust
// Get finalized transactions
let finalized_txs = self.consensus.get_finalized_transactions_for_block();

// Calculate total fees from all finalized transactions
let total_fees = self.consensus.tx_pool.get_total_fees();

// Add fees to base block reward
let base_reward = BLOCK_REWARD_SATOSHIS; // 100 TIME = 10,000,000,000 satoshis
let total_reward = base_reward + total_fees;

// Distribute to masternodes
let rewards = self.calculate_rewards_with_amount(&masternodes, total_reward);
```

**Example Block Reward:**
- Base reward: 10,000,000,000 satoshis (100 TIME)
- Collected fees: 50,000 satoshis (0.0005 TIME from user transactions)
- **Total reward: 10,000,050,000 satoshis (100.0005 TIME)**

---

### 5. Coinbase Transaction Creation
**Location:** `src/blockchain.rs:1555-1565`

```rust
let coinbase = Transaction {
    version: 1,
    inputs: vec![],  // No inputs - creates new coins
    outputs: vec![TxOutput {
        value: total_reward,  // ← Base reward + ALL fees
        script_pubkey: script,
    }],
};
```

The coinbase creates **base_reward + total_fees** worth of new TIME.

---

### 6. Reward Distribution
**Location:** `src/blockchain.rs:1297-1342` (calculate_rewards_with_amount)

```rust
// For each masternode, calculate proportional share
for (i, mn) in masternodes.iter().enumerate() {
    let gross_share = if i == masternodes.len() - 1 {
        // Last masternode gets remainder (prevents rounding errors)
        total_reward - distributed
    } else {
        (total_reward * mn.masternode.tier.reward_weight()) / total_weight
    };

    // Deduct 0.1% fee from masternode reward
    let fee = gross_share / 1000; // 0.1% = 1/1000
    let net_share = gross_share.saturating_sub(fee);

    rewards.push((mn.masternode.address.clone(), net_share));
}
```

**Example with 6 masternodes (all Bronze tier):**
- Total reward: 10,000,050,000 satoshis (100.0005 TIME)
- Per masternode (gross): 1,666,675,000 satoshis (16.666675 TIME)
- 0.1% fee deducted: 1,666,675 satoshis (0.0166667 TIME)
- Per masternode (net): 1,665,008,325 satoshis (16.650083 TIME)

**Fee destination:** The 0.1% fee from each masternode reward is NOT distributed - it's effectively burned, creating slight deflation.

---

### 7. Reward Distribution Transaction
**Location:** `src/blockchain.rs:1567-1600`

```rust
let reward_distribution = Transaction {
    version: 1,
    inputs: vec![TransactionInput {
        previous_output: OutPoint {
            txid: coinbase.txid(),  // Spends coinbase
            vout: 0,
        },
        script_sig: vec![],
        sequence: 0xFFFFFFFF,
    }],
    outputs: rewards  // One output per masternode
        .iter()
        .map(|(address, amount)| TxOutput {
            value: *amount,
            script_pubkey: address.as_bytes().to_vec(),
        })
        .collect(),
};
```

This transaction:
- **Spends** the coinbase output (total_reward)
- **Distributes** to masternodes after 0.1% fee deduction
- **Difference** between input and outputs = burned fee

---

### 8. Block Structure
**Location:** `src/blockchain.rs:1645-1650`

```rust
Block {
    header: BlockHeader {
        height: next_height,
        block_reward: total_reward,  // ← Includes fees
        // ...
    },
    transactions: vec![
        coinbase,              // Creates total_reward (base + fees)
        reward_distribution,   // Distributes ~99.9% to masternodes
        ...finalized_txs       // User transactions (already paid fees)
    ],
    masternode_rewards: rewards,  // Metadata for validation
}
```

---

### 9. Fee Cleanup After Block
**Location:** `src/blockchain.rs:1791-1792`

```rust
// After block is added to chain
self.consensus.clear_finalized_transactions();
```

This clears the finalized pool, removing transactions that were included in the block. Their fees have been collected and distributed.

---

## Fee Validation

### During Block Validation
**Location:** `src/blockchain.rs:2400-2425`

```rust
// Verify distributed amount is ~99.9% of block_reward
let total_distributed: u64 = reward_dist.outputs.iter().map(|o| o.value).sum();
let expected_total = block.header.block_reward;

// Calculate expected fee (0.1% of block reward)
let expected_fee = expected_total / 1000;
let expected_distributed = expected_total.saturating_sub(expected_fee);

// Allow tolerance for rounding
let tolerance = expected_fee / 100;
let lower_bound = expected_distributed.saturating_sub(tolerance);
let upper_bound = expected_total;

if total_distributed < lower_bound || total_distributed > upper_bound {
    return Err("Invalid reward distribution");
}
```

This ensures:
- Fees are being collected (total > base_reward if there are transactions)
- Distribution matches expected ~99.9% after masternode fee deduction
- No inflation attacks (can't claim more than block_reward)

---

## Fee Economics

### Per Transaction
- **Minimum absolute:** 1,000 satoshis (0.00001 TIME)
- **Minimum proportional:** 0.1% of transaction amount
- **Actual fee:** Higher of the two minimums

### Per Block
- **Base reward:** 100 TIME (10,000,000,000 satoshis)
- **Transaction fees:** Sum of all finalized transaction fees
- **Total reward:** Base + fees
- **Masternode fee:** 0.1% of gross share per masternode
- **Net distribution:** ~99.9% of total reward
- **Burned:** ~0.1% of total reward (masternode fees)

### Example Block with 100 Transactions

**Assumptions:**
- 100 user transactions included
- Average transaction: 0.01 TIME with 0.1% fee = 1,000 satoshis fee
- Total transaction fees: 100,000 satoshis (0.001 TIME)
- 6 masternodes (all Bronze tier)

**Calculations:**
1. **Base reward:** 10,000,000,000 satoshis (100 TIME)
2. **Collected fees:** 100,000 satoshis (0.001 TIME)
3. **Total reward:** 10,000,100,000 satoshis (100.001 TIME)
4. **Per masternode (gross):** 1,666,683,333 satoshis (16.6668333 TIME)
5. **Masternode fee (0.1%):** 1,666,683 satoshis (0.0166668 TIME)
6. **Per masternode (net):** 1,665,016,650 satoshis (16.6501665 TIME)
7. **Total distributed:** 9,990,099,900 satoshis (99.900999 TIME)
8. **Burned in masternode fees:** 10,000,100 satoshis (0.100001 TIME)

**Result:**
- Users paid 0.001 TIME in transaction fees
- Masternodes received 100.001 TIME gross, 99.9 TIME net
- 0.1% of block reward burned (0.100001 TIME)
- Effective reward: 99.9 TIME distributed, 0.1 TIME deflationary burn

---

## Verification

### Check Fee Collection is Working

1. **Query mempool fees:**
```rust
let total_fees = consensus.tx_pool.get_total_fees();
println!("Pending fees: {} satoshis", total_fees);
```

2. **Check block reward includes fees:**
```rust
let block = blockchain.get_block(height)?;
let base_reward = BLOCK_REWARD_SATOSHIS;
let fee_bonus = block.header.block_reward - base_reward;
println!("Block {} included {} satoshis in fees", height, fee_bonus);
```

3. **Verify distribution:**
```rust
let coinbase = &block.transactions[0];
let reward_dist = &block.transactions[1];
let total_distributed: u64 = reward_dist.outputs.iter().map(|o| o.value).sum();
let fee_burned = coinbase.outputs[0].value - total_distributed;
println!("Fees burned in masternode fee: {} satoshis", fee_burned);
```

---

## Summary

✅ **Transaction fees ARE collected and added to block rewards**

**Flow:**
1. Users pay fees (input_sum - output_sum)
2. Fees tracked in transaction pool entries
3. When block produced, all finalized transaction fees summed
4. Total reward = base_reward + collected_fees
5. Coinbase creates total_reward
6. Reward distribution transaction distributes ~99.9% to masternodes
7. 0.1% burned as masternode fee (slight deflation)
8. Finalized pool cleared after block inclusion

**Economics:**
- Minimum fee: max(1,000 satoshis, 0.1% of transaction)
- Masternodes benefit from both base reward AND user transaction fees
- 0.1% of block reward burned (creates deflationary pressure)
- More transactions = higher masternode rewards

**Security:**
- Validation ensures distributed amount matches block_reward ±tolerance
- Cannot inflate supply beyond base_reward + actual_fees
- Double-counting prevented by validation

---

**Document Version:** 1.0  
**Last Updated:** January 19, 2026
