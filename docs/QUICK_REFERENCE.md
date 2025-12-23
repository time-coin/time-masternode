# TIME Coin Protocol V6 – Quick Reference

**TL;DR for developers:** Concrete algorithms, formats, and parameters.

---

## Cryptography Stack

```yaml
Hash:       BLAKE3-256
Signature:  Ed25519
VRF:        ECVRF-Edwards25519-SHA512-TAI (RFC 9381)
Address:    bech32m (BIP 350)
```

---

## Transaction Format

```
[version: u32_le]
[input_count: varint]
  [prev_txid: Hash256]
  [prev_index: u32_le]
  [script_length: varint]
  [script: bytes]
[output_count: varint]
  [value: u64_le]
  [script_length: varint]
  [script: bytes]
[lock_time: u64_le]

txid = BLAKE3(serialized_bytes)
```

---

## Staking Script

```
Lock script:   OP_STAKE <tier: u8> <pubkey: 33B> <unlock_height: u32> <reserved: u8>
Unlock script: <signature: 64B> <witness: bytes>

Conditions:
  - signature must be valid from pubkey
  - unlock_height ≤ current_block_height
  - stake matures after being archived
```

---

## Network

```yaml
Transport:     QUIC v1 (RFC 9000) | TCP fallback
Serialization: bincode (consensus), protobuf (RPC)
Framing:       [length: u32_be] [payload]
Max message:   4 MB
Max peers:     125
Port:          18888 (mainnet), 18889 (testnet)
Bootstrap:     seed1.timecoin.dev, seed2.timecoin.dev, seed3.timecoin.dev
```

---

## Consensus Parameters

```yaml
Avalanche:
  k:               20          # sample size
  α:               14          # success threshold
  β_local:         20          # local acceptance threshold
  POLL_TIMEOUT:    200 ms

VFP:
  Q_finality:      67% of AVS weight
  
TSDC:
  BLOCK_INTERVAL:  600 s       # 10 minutes
  SLOT_GRACE:      30 s        # accept blocks in [slot-30, slot+30]
  FUTURE_TOLERANCE: 5 s        # reject blocks > 5s in future

AVS:
  HEARTBEAT_PERIOD: 60 s
  HEARTBEAT_TTL:    180 s
  WITNESS_MIN:      3           # minimum witness attestations
```

---

## Masternode Tiers

```yaml
Free:   0 TIME     → weight 1
Bronze: 1,000 TIME → weight 10
Silver: 10,000 TIME → weight 100
Gold:   100,000 TIME → weight 1,000
```

---

## Rewards (per checkpoint block)

```
R = 100 * (1 + ln(|AVS|))

Examples:
  |AVS| = 10    → R ≈ 330 TIME
  |AVS| = 100   → R ≈ 561 TIME
  |AVS| = 1,000 → R ≈ 791 TIME

Distribution:
  Producer:    10% of (R + tx_fees)
  Validators:  90% of (R + tx_fees) proportional to weight
```

---

## Address Format

```
Mainnet:  time1<bech32m_payload>
Testnet:  timet<bech32m_payload>

Example: time1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx
```

---

## Mempool

```yaml
Max size:           300 MB
Max block entries:  10,000
Max block size:     2 MB
Eviction:           lowest_fee_rate_first
TX expiry:          72 hours
Min fee:            0.001 TIME/tx
```

---

## Genesis

```yaml
Chain ID:
  Mainnet:  1
  Testnet:  2
  Devnet:   3

Bootstrap:
  1. Genesis specifies initial_avs (pre-agreed founders)
  2. Validators stake on-chain in block 0/1
  3. Staking matures (archived) → AVS membership active
  4. New validators join via on-chain staking + quorum attestation
```

---

## Clock Sync

```yaml
Requirement:       NTP v4
Max clock drift:   ±10 seconds
Slot grace period: 30 seconds
Future tolerance:  5 seconds
```

---

## RPC API (JSON-RPC 2.0)

```json
{
  "jsonrpc": "2.0",
  "method": "sendtransaction",
  "params": { "tx": "<hex>" },
  "id": 1
}

{
  "jsonrpc": "2.0",
  "method": "gettransaction",
  "params": { "txid": "<hash256>" },
  "id": 2
}

{
  "jsonrpc": "2.0",
  "method": "getbalance",
  "params": { "address": "<bech32>" },
  "id": 3
}
```

---

## Block Structure

```
Header:
  height: u64
  slot_index: u64
  slot_time: u64
  prev_block_hash: Hash256
  producer_id: Hash256
  vrf_output: [u8; 32]
  vrf_proof: bytes
  finalized_root: Hash256 (Merkle root of entries)

Body:
  entries: [
    { txid: Hash256, vfp_hash: Hash256 },
    ...
  ]
  (sorted lexicographically by txid)
```

---

## VFP (Verifiable Finality Proof)

```
FinalityVote:
  chain_id: u32
  txid: Hash256
  tx_hash_commitment: BLAKE3(canonical_tx)
  slot_index: u64
  voter_mn_id: Hash256
  voter_weight: u16
  signature: [u8; 64] (Ed25519)

VFP validation:
  1. All signatures verify
  2. All votes agree on (chain_id, txid, tx_hash_commitment, slot_index)
  3. Voters distinct
  4. Sum of weights ≥ 67% of AVS weight at slot_index
```

---

## Implementation Phases

1. **Core crypto** (weeks 1–2)
2. **Consensus** (weeks 3–5)
3. **Network** (weeks 6–8)
4. **Storage** (weeks 9–10)
5. **APIs** (weeks 11–12)

---

## Key Files

- **TIMECOIN_PROTOCOL_V6.md** – Full normative specification (§1–§27)
- **IMPLEMENTATION_ADDENDUM.md** – Implementation guidance and rationale
- **QUICK_REFERENCE.md** – This file
- **V6_UPDATE_SUMMARY.md** – Summary of changes from analysis

---

## Open Questions for Community

1. **Pre-mine:** Should there be an initial supply reserved for the foundation?
2. **Reward cap:** Logarithmic rewards have no hard cap. Is one desired?
3. **Block size:** Is 2 MB sufficient for the target use case?
4. **Fee market:** Dynamic fees (EIP-1559) or simple median-based?
5. **Storage:** 7-day AVS snapshot retention – sufficient or too much?

---

## Validation Checklist (Before Mainnet)

- [ ] Cryptographic test vectors validated externally
- [ ] Consensus tests on 100+ node testnet
- [ ] Network partition recovery demonstrated
- [ ] Performance: TPS, latency, bandwidth measured
- [ ] Security audit completed
- [ ] Operator documentation finalized
- [ ] Incident response plan in place

---

## References

- RFC 9381: ECVRF
- RFC 9000: QUIC
- BIP 350: bech32m
- BLAKE3: https://blake3.io
- Avalanche consensus: https://arxiv.org/abs/1906.08936

---
