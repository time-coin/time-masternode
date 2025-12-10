# TIME Coin Masternode Reward Distribution

## Overview

100% of block rewards are distributed to masternodes based on their tier weight. There are no treasury or governance allocations.

## Block Production

- **Block Time**: 10 minutes
- **Blocks Per Day**: 144
- **Blocks Per Year**: ~52,560

## Masternode Tiers

| Tier   | Collateral | Reward Weight | Governance Voting |
|--------|-----------|---------------|-------------------|
| Free   | 0 TIME    | 0.1x (100)    | ❌ No             |
| Bronze | 1,000     | 1x (1,000)    | ✅ Yes            |
| Silver | 10,000    | 10x (10,000)  | ✅ Yes            |
| Gold   | 100,000   | 100x (100,000)| ✅ Yes            |

## Reward Calculation

### Base Block Reward (Logarithmic Scaling)

The base reward per block is calculated using a logarithmic formula to account for network growth:

```
R = 100 × (1 + ln(n))
```

Where:
- `R` = Total block reward in TIME
- `n` = Total number of active masternodes
- `ln` = Natural logarithm

### Examples

- **10 masternodes**: 100 × (1 + ln(10)) ≈ **330 TIME per block**
- **100 masternodes**: 100 × (1 + ln(100)) ≈ **560 TIME per block**
- **1,000 masternodes**: 100 × (1 + ln(1,000)) ≈ **790 TIME per block**

### Distribution Formula

Each masternode receives:

```
Reward = (Total Block Reward × Node Weight) / Total Network Weight
```

### Special Case: Free Nodes Only

**If only Free tier nodes exist on the network**, they share 100% of the block reward equally (no reduced weight penalty).

This ensures the network can function even with no collateral nodes, while still incentivizing collateral-backed nodes when they join.

### Example Scenarios

#### Scenario 1: Mixed Network
- 10 Free nodes (weight: 100 each = 1,000 total)
- 5 Bronze nodes (weight: 1,000 each = 5,000 total)
- 2 Silver nodes (weight: 10,000 each = 20,000 total)
- 1 Gold node (weight: 100,000 = 100,000 total)

**Total Weight**: 126,000
**Block Reward**: ~440 TIME

- Each Free node: (440 × 100) / 126,000 ≈ **0.35 TIME**
- Each Bronze node: (440 × 1,000) / 126,000 ≈ **3.49 TIME**
- Each Silver node: (440 × 10,000) / 126,000 ≈ **34.92 TIME**
- Each Gold node: (440 × 100,000) / 126,000 ≈ **349.21 TIME**

#### Scenario 2: Only Free Nodes
- 20 Free nodes on network
- **Block Reward**: ~400 TIME
- Each node receives: 400 / 20 = **20 TIME per block**

This ensures network bootstrap and participation even with no capital investment.

## Annual Returns (APY)

Estimated APY based on consistent operation (100% uptime):

| Tier   | Collateral | Est. Annual Rewards* | Est. APY* |
|--------|-----------|---------------------|-----------|
| Free   | 0         | Variable            | N/A       |
| Bronze | 1,000     | ~183,000 TIME       | ~18,300%  |
| Silver | 10,000    | ~1,830,000 TIME     | ~18,300%  |
| Gold   | 100,000   | ~18,300,000 TIME    | ~18,300%  |

*Estimates assume stable network of 100 masternodes with mixed tiers. Actual returns vary based on total network weight and number of active nodes.

## Fair Distribution Principles

1. **No Capital Barriers**: Free nodes can participate and earn
2. **Proportional Rewards**: Higher collateral = higher absolute rewards
3. **Fair APY**: All collateral tiers earn similar % return on investment
4. **Network Security**: Higher collateral incentivizes long-term commitment
5. **Governance Rights**: Only collateral-backed nodes can vote (prevents Sybil attacks)

## Inflation Model

Total supply increases based on block rewards. With logarithmic scaling:
- New coins created: ~183,000 - 790,000 TIME per day (depending on network size)
- Annual inflation: Decreases as percentage as total supply grows
- Sustainable long-term issuance without hyperinflation

## Transaction Fees

All transaction fees are added to the base block reward and distributed to masternodes in the same proportional manner.
