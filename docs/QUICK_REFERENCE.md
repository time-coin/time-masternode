# TIME Coin Protocol V6 – Quick Reference

**TL;DR for developers:** Concrete algorithms, formats, and parameters.

---

## Cryptography Stack

```yaml
Hash:       BLAKE3-256
Signature:  Ed25519
VRF:        ECVRF-Edwards25519-SHA512-TAI (RFC 9381)
Address:    bech32m (BIP 350)
Memo:       ECDH (X25519) + AES-256-GCM (encrypted per-transaction memos)
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
[encrypted_memo: Option<bytes>]   # ECDH-encrypted memo (see below)

txid = BLAKE3(serialized_bytes)
```

### Encrypted Memo Format

Optional field. When present, encrypted using ECDH key exchange so only sender and recipient can decrypt.

```
Wire format (encrypted_memo bytes):
[0]       version byte (0x01)
[1..33]   sender's Ed25519 public key (32 bytes)
[33..65]  recipient's Ed25519 public key (32 bytes)
[65..77]  AES-GCM nonce (12 bytes)
[77..]    AES-GCM ciphertext + 16-byte auth tag

Key derivation:
  1. Ed25519 signing key → X25519 secret (via SHA-512 of seed)
  2. Ed25519 public key → X25519 public (Edwards → Montgomery birational map)
  3. shared_secret = X25519(our_secret, their_public)
  4. aes_key = SHA-256(shared_secret || "TIME-memo-v1")
  5. Encrypt/decrypt with AES-256-GCM

Max plaintext: 256 bytes
```

### Payment Request URI

```
timecoin:<ADDRESS>?amount=<TIME>&pubkey=<HEX>[&memo=<TEXT>][&label=<TEXT>]

Example:
  timecoin:TIME0AsqaMhk...?amount=50&pubkey=a1b2c3...&memo=Invoice%20%2342

Parameters:
  address  — Recipient's TIME address (required, in URI path)
  amount   — Requested amount in TIME (required)
  pubkey   — Recipient's Ed25519 public key, hex-encoded (enables encrypted memos)
  memo     — Payment description, URL-encoded (optional)
  label    — Requester label/name, URL-encoded (optional)
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
Transport:     TCP (plain, TLS planned)
Serialization: bincode (internal), JSON (RPC)
Framing:       [length: u32_be] [type: u8] [payload]
Max message:   4 MB
Max peers:     50 (configurable outbound)
MAX_INBOUND:   100 (overload redirect at 70%)
FULL_MESH_THRESHOLD: 50 masternodes (full-mesh below, pyramid above)
MAX_SUBNET_CONNECTS_PER_MIN: 20  # per /24 prefix; exceeded → drop before TLS
PRE_HANDSHAKE_TIMEOUT_SECS:  10  # ghost connection OOM protection
Port:          24000 (mainnet), 24100 (testnet)
Storage:       Sled embedded database
Bootstrap:     Configured in time.conf (addnode=)
bansubnet:     Static CIDR bans configurable in time.conf (bansubnet=x.x.x.0/24)
```

---

## Consensus Parameters

```yaml
TimeVote:
  k:               20          # sample size
  α:               14          # success threshold
  Q_finality:      67%         # finality threshold (% of AVS weight), liveness fallback to 51% after 30s
  POLL_TIMEOUT:    200 ms
  
TimeLock:
  BLOCK_INTERVAL:  600 s       # 10 minutes
  LEADER_TIMEOUT_SECS: 5 s    # offline leader skipped every 5s (was 10s in v1.2)
  FREE_TIER_VRF_ATTEMPT: 3    # Free tier gets VRF boost after attempt 3 (15s deadlock, was 6/60s)
  SLOT_GRACE:      30 s        # accept blocks in [slot-30, slot+30]
  FUTURE_TOLERANCE: 5 s        # reject blocks > 5s in future
  CATCH_UP_PRESTART: 1         # leader_attempt starts at 1 when blocks_behind > 50

AVS:
  HEARTBEAT_PERIOD: 60 s
  HEARTBEAT_TTL:    180 s
  WITNESS_MIN:      3           # minimum witness attestations

Fork Resolution:
  Rule 1:          Longer chain wins (higher height)
  Rule 2:          Lower hash wins (lexicographic tiebreaker)
```

---

## Masternode Tiers

```yaml
Free:   0 TIME          → sampling weight 1, no governance
Bronze: 1,000 TIME      → sampling weight 10,   governance weight 1   (exact collateral)
Silver: 10,000 TIME     → sampling weight 100,  governance weight 10  (exact collateral)
Gold:   100,000 TIME    → sampling weight 1,000 governance weight 100 (exact collateral)
```

Registry Security:
```yaml
MIGRATION_COOLDOWN_SECS:  300   # min seconds between IP migrations per outpoint (was 60)
CYCLING_LOCKOUT_SECS:     600   # lockout if back-and-forth cycling A→B→A detected
EVICTION_STORM_COOLDOWN:  60    # V4 eviction storm rate-limit window
```

---

## Masternode Operations

```yaml
Register masternode:
  1. Generate key:   time-cli masternode genkey
  2. Add to time.conf:    masternodeprivkey=<key>
  3. Add to masternode.conf:  mn1 <txid> <vout>
  4. Restart timed   # daemon auto-registers on startup

Deregister masternode / release collateral:
  1. Comment out collateral line in masternode.conf:  # mn1 <txid> <vout>
  2. Restart timed
  # Daemon broadcasts signed MasternodeUnlock gossip message
  # All peers release the collateral lock within ~15 seconds
  # No on-chain transaction required

Upgrade tier (e.g. Silver → Gold):
  1. Update masternode.conf with the new collateral txid/vout
  2. Restart timed
  # Daemon broadcasts MasternodeUnlock for old outpoint
  # Registers new collateral; tier auto-detected from amount
  # Collateral-Churn guard allows upgrade when new UTXO is owned by same wallet_address
```

---

## On-Chain Governance

```yaml
Voting period:       1,008 blocks (~1 week at 10 min/block)
Quorum threshold:    67% of total active governance weight (YES votes only)
Eligible voters:     Bronze, Silver, Gold masternodes (Free cannot vote)
Proposal types:      TreasurySpend | FeeScheduleChange
Treasury source:     5 TIME/block (accumulated in on-chain state)
Max description:     256 bytes
Storage keys:        gov_proposal_<64-hex>
                     gov_vote_<64-hex>_<voter_address>
Execution:           Atomic in the same block the voting window closes
```

---

## Rewards (per checkpoint block)

```
Base Reward = 100 TIME (fixed per block)
Total Reward = 100 TIME + transaction_fees

Distribution (§10.4):
  Block Producer:  30 TIME + fees  (VRF-selected leader bonus)
  Treasury pool:    5 TIME         (on-chain state, not a UTXO)
  Per-Tier Pools:  65 TIME total
    Gold pool:     25 TIME  (full pool → 1 VRF-selected winner)
    Silver pool:   18 TIME
    Bronze pool:   14 TIME
    Free pool:      8 TIME  (split among up to 25 recipients)

  Max 25 Free-tier recipients per block.
  Fairness bonus: blocks_without_reward / 10 (linear, unbounded).
  Empty tier's pool → block producer.
```

---

## Address Format

```
Mainnet:  TIME1<payload>   (38 chars total)
Testnet:  TIME0<payload>   (38 chars total)
```

---

## Mempool

```yaml
Max size:           300 MB
Max block entries:  10,000
Max block size:     2 MB
Eviction:           lowest_fee_rate_first
TX expiry:          72 hours
Min fee:            0.01 TIME/tx (flat floor; tiered % for larger amounts)
Min send amount:    1 TIME (non-self-sends; enforced at consensus layer)
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
    { txid: Hash256, timeproof_hash: Hash256 },
    ...
  ]
  (sorted lexicographically by txid)
```

---

## TimeProof

```
FinalityVote:
  chain_id: u32
  txid: Hash256
  tx_hash_commitment: BLAKE3(canonical_tx)
  slot_index: u64
  decision: Accept | Reject  (REQUIRED: prevents equivocation)
  voter_mn_id: Hash256
  voter_weight: u16
  signature: [u8; 64] (Ed25519)

TimeProof validation:
  1. All signatures verify (including decision field)
  2. All votes agree on (chain_id, txid, tx_hash_commitment, slot_index)
  3. All votes have decision=Accept (only Accept votes count toward finality)
  4. Voters distinct
  5. Sum of weights ≥ 67% of AVS weight at slot_index (51% under liveness fallback)
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
- RFC 8446: TLS v1.3
- BIP 350: bech32m
- BLAKE3: https://blake3.io
- TimeVote Protocol: Stake-weighted voting consensus (inspired by Avalanche research)
- Sled embedded database: https://github.com/spacejam/sled

---
