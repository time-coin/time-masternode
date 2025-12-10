# TIME Coin Masternode Tiers

TIME Coin uses a tiered masternode system to balance participation rewards with governance responsibility.

## Tier Comparison

| Tier | Collateral | Block Rewards | Governance Voting | Reward Weight |
|------|-----------|---------------|-------------------|---------------|
| **Free** | 0 TIME | ✅ Yes | ❌ No | 1x |
| **Bronze** | 1,000 TIME | ✅ Yes | ✅ Yes | 1x |
| **Silver** | 10,000 TIME | ✅ Yes | ✅ Yes | 10x |
| **Gold** | 100,000 TIME | ✅ Yes | ✅ Yes | 100x |

## Free Tier

The **Free Tier** allows anyone to run a masternode and receive block rewards without any collateral requirement.

### Benefits
- ✅ **No collateral required** - Run a node for free
- ✅ **Receive block rewards** - Get paid for securing the network
- ✅ **Participate in consensus** - Help validate transactions

### Limitations
- ❌ **Cannot vote on governance** - No voting rights for protocol changes
- ⚠️ **Lower rewards** - 1x weight (same as Bronze but no collateral)

### Use Cases
- Testing and development
- Running a personal node
- Contributing to network security without capital investment
- Community participation

## Bronze Tier

**Collateral:** 1,000 TIME

### Benefits
- ✅ Block rewards (1x weight)
- ✅ Governance voting rights
- ✅ Full masternode status

## Silver Tier

**Collateral:** 10,000 TIME

### Benefits
- ✅ Block rewards (10x weight)
- ✅ Governance voting rights
- ✅ Higher reward potential

## Gold Tier

**Collateral:** 100,000 TIME

### Benefits
- ✅ Block rewards (100x weight)
- ✅ Governance voting rights
- ✅ Maximum reward potential

## Running a Masternode

### Free Tier Setup

1. **Edit config.toml:**
```toml
[masternode]
enabled = true
wallet_address = "YOUR_TIME_ADDRESS"
tier = "free"
```

2. **Start the node:**
```bash
./timed
```

That's it! Your Free tier masternode is now running.

### Paid Tier Setup (Bronze/Silver/Gold)

1. **Lock collateral** (coming soon - requires governance approval)
2. **Edit config.toml:**
```toml
[masternode]
enabled = true
wallet_address = "YOUR_TIME_ADDRESS"
collateral_txid = "YOUR_COLLATERAL_TX"
tier = "bronze"  # or "silver", "gold"
```

3. **Start the node:**
```bash
./timed
```

## Reward Distribution

Block rewards are distributed based on tier weights:

```
Total Block Reward = 100 TIME
Masternode Pool = 30% = 30 TIME

Example with 100 masternodes:
- 90 Free tier (90 × 1 = 90 weight)
- 5 Bronze (5 × 1 = 5 weight)
- 3 Silver (3 × 10 = 30 weight)
- 2 Gold (2 × 100 = 200 weight)

Total weight = 90 + 5 + 30 + 200 = 325

Each Free node: (30 × 1) / 325 = ~0.092 TIME
Each Bronze node: (30 × 1) / 325 = ~0.092 TIME
Each Silver node: (30 × 10) / 325 = ~0.923 TIME
Each Gold node: (30 × 100) / 325 = ~9.231 TIME
```

## Governance Participation

Only Bronze, Silver, and Gold tier masternodes can vote on:
- Protocol upgrades
- Treasury spending proposals
- Economic parameter changes
- Network governance decisions

Free tier nodes help secure the network but don't participate in governance to prevent Sybil attacks (someone running many free nodes to control voting).

## Upgrading Tiers

To upgrade from Free → Bronze/Silver/Gold:
1. Acquire the required collateral
2. Lock collateral in a special transaction
3. Update your config.toml
4. Restart your node

## FAQ

**Q: Why does Free tier exist?**
A: To lower the barrier to entry and encourage broad network participation. More nodes = more decentralization and security.

**Q: Can I run multiple Free tier nodes?**
A: Yes, but each needs a unique wallet address and IP address.

**Q: Do Free tier nodes help with instant finality?**
A: Yes! Free tier nodes participate in BFT consensus for transaction validation.

**Q: When will collateral locking be available?**
A: Collateral locking and tier upgrades will be enabled via governance vote after mainnet launch.

---

For more information, see the [Technical Specification](./TECHNICAL_SPECIFICATION.md).
