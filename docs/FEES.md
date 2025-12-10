# TIME Coin Transaction Fees

## Fee Structure

TIME Coin uses a simple percentage-based fee model similar to Bitcoin, but with lower rates to encourage adoption.

### Fee Rate

- **Base Fee**: 0.1% of transaction amount
- **Minimum**: Enforced during transaction validation
- **Maximum**: No cap (user can add extra for priority)

### Examples

| Amount Sent | Minimum Fee (0.1%) | In TIME |
|-------------|-------------------|---------|
| 100 TIME    | 0.1 TIME         | 0.1     |
| 1,000 TIME  | 1 TIME           | 1.0     |
| 10,000 TIME | 10 TIME          | 10.0    |

### Fee Distribution

All transaction fees are added to the block reward and distributed to active masternodes proportionally by their tier weight:

- **Free Tier**: 0.1x weight
- **Bronze**: 1x weight  
- **Silver**: 10x weight
- **Gold**: 100x weight

### Implementation

Fees are calculated as:
```
fee = inputs - outputs
min_required_fee = outputs * 0.001  // 0.1%
```

Transactions with insufficient fees are rejected during validation.

### Benefits

1. **Predictable**: Users know exactly what they'll pay
2. **Fair**: Proportional to value transferred
3. **Incentivized**: Rewards masternodes for transaction processing
4. **Spam Protection**: Prevents dust attacks while keeping costs low
