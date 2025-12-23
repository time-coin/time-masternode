# TIME Coin Protocol V6 – Implementation Addendum

**Document:** `IMPLEMENTATION_ADDENDUM.md`  
**Version:** 1.0  
**Date:** December 2025  
**Status:** Implementation guidance (Informative)

---

## Overview

This addendum consolidates concrete implementation decisions required to move from the protocol specification (V6.md) to working code. It addresses the gaps identified in the architectural analysis and provides normative guidance on:

1. Cryptographic algorithm selection
2. Message formats and serialization
3. Network transport
4. Bootstrap and genesis procedures
5. Error handling and recovery
6. Economics and mempool

---

## Summary of Changes to V6.md

The following sections were added to **TIMECOIN_PROTOCOL_V6.md**:

| Section | Purpose | Status |
|---------|---------|--------|
| §16 | Cryptographic Bindings | NORMATIVE – fixes hash/VRF/serialization |
| §17 | Transaction and Staking UTXO Details | NORMATIVE – script semantics, formats |
| §18 | Network Transport Layer | NORMATIVE – QUIC/TCP, framing, peer discovery |
| §19 | Genesis Block and Initial State | NORMATIVE – bootstrap procedure |
| §20 | Clock Synchronization | NORMATIVE – NTP requirements |
| §21 | Light Client and SPV Support | OPTIONAL – merkle proofs for wallets |
| §22 | Error Recovery and Edge Cases | NORMATIVE – conflict resolution, network partition |
| §23 | Address Format and Wallet Integration | NORMATIVE – bech32m, RPC API |
| §24 | Mempool Management | NORMATIVE – size limits, eviction, fee estimation |
| §25 | Economic Model | NORMATIVE – reward schedule, supply |
| §26 | Implementation Checklist | — pre-mainnet verification |
| §27 | Test Vectors | — validation framework |

---

## Critical Implementation Decisions

### 1. Cryptographic Primitives (§16)

**Decision:** Pin concrete algorithms to prevent replay attacks and ensure compatibility.

```yaml
HASH_FUNCTION: BLAKE3-256
  - Rationale: Modern, fast, compact
  - Used in: txid, block_hash, tx_hash_commitment, VRF input
  
VRF_SCHEME: ECVRF-EDWARDS25519-SHA512-TAI (RFC 9381)
  - Rationale: Deterministic, verifiable, resistant to chosen-input
  - Used in: TSDC sortition (§9.2)
  
SIGNATURE_SCHEME: Ed25519
  - Used in: all signed objects (heartbeats, votes, finality proofs)
```

**Implementation Priority:** Must be finalized before writing any cryptographic code.

### 2. Transaction Serialization (§16.3)

**Decision:** Canonical format prevents signature verification failures.

```
Format: version || input_count || inputs[] || output_count || outputs[] || lock_time
- All integers: little-endian
- Dynamic-length arrays: varint length prefix
- Fields: fixed order, no padding
```

**Example Rust code:**
```rust
fn serialize_tx(tx: &Transaction) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&tx.version.to_le_bytes());
    // ... serialize each field in order
    buf
}

fn tx_hash(tx: &Transaction) -> Hash256 {
    blake3::hash(&serialize_tx(tx))
}
```

### 3. Staking Script Semantics (§17.2)

**Decision:** Explicit staking outputs enable AVS membership via on-chain collateral.

```
Staking output: OP_STAKE <tier> <pubkey> <unlock_height>
Spending: <signature> must be from <pubkey> and unlock_height ≤ current_height
```

**Example workflow:**
1. Operator creates staking transaction output: 100,000 TIME locked by OP_STAKE
2. Staking becomes mature after being archived in a checkpoint block
3. Operator runs masternode; stake weight determines AVS voting power
4. To withdraw, operator spends the staking output after unlock_height

### 4. Network Protocol (§18)

**Decision:** QUIC as primary transport, TCP fallback, bincode serialization.

```yaml
TRANSPORT: QUIC v1 (RFC 9000)
FALLBACK: TCP + Noise Protocol (optional)
SERIALIZATION: bincode v1 (internal), protobuf v3 (RPC APIs)
FRAMING: 4-byte big-endian length prefix + payload
MAX_MESSAGE_SIZE: 4 MB
MAX_PEERS: 125 (inbound + outbound combined)
```

**Rationale:**
- QUIC: modern, multiplexing, TLS 1.3, congestion control
- bincode: compact, deterministic, suitable for consensus
- protobuf: forward-compatible for external APIs (wallets, explorers)

### 5. Genesis and Bootstrap (§19)

**Decision:** Solve chicken-egg problem via pre-agreed initial AVS + on-chain staking.

**Bootstrap sequence:**
1. Network launches with hardcoded genesis block
2. Genesis specifies initial AVS set (e.g., 10 founders)
3. Each initial validator stakes on-chain in block 0 or 1
4. Once staking is archived, AVS membership enforced by heartbeat+witness
5. New masternodes join via on-chain staking + achieving AVS quorum attestation

**Mainnet genesis (example):**
```json
{
  "chain_id": 1,
  "timestamp": 1704067200,  // Jan 1, 2024 00:00 UTC
  "initial_utxos": [...],
  "initial_avs": [
    { "mn_id": "...", "pubkey": "...", "tier_weight": 100 }
  ]
}
```

### 6. Clock Synchronization (§20)

**Decision:** NTP mandatory; TSDC slot alignment within ±10s tolerance.

```yaml
CLOCK_SYNC: NTP v4 required
MAX_CLOCK_DRIFT: ±10 seconds
SLOT_GRACE_PERIOD: 30 seconds  # accept blocks in [slot-30s, slot+30s]
FUTURE_BLOCK_TOLERANCE: 5 seconds  # reject blocks > 5s in future
```

**Operator checklist:**
- [ ] Run `ntpd` or systemd-timesyncd
- [ ] Verify clock offset: `ntpq -p` or `timedatectl status`
- [ ] Target accuracy: ±1 second

### 7. Address Format (§23)

**Decision:** bech32m (BIP 350) for human-readable, typo-resistant addresses.

```
Mainnet: time1<base32_payload>
Testnet: timet<base32_payload>
```

**Example:** `time1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx`

**Implementation:**
```rust
let address = bech32::encode("time1", pubkey_hash)?;
```

### 8. Economic Model (§25)

**Decision:** Fair launch, logarithmic rewards, no hard cap.

```yaml
INITIAL_SUPPLY: 0  # (or specify pre-mine for foundation)
REWARD_PER_BLOCK: R = 100 * (1 + ln(|AVS|))
  Example:
    |AVS| = 10  → R ≈ 330 TIME
    |AVS| = 100 → R ≈ 561 TIME
    |AVS| = 1000 → R ≈ 791 TIME

PAYOUT_SPLIT:
  - Producer: 10% of (R + fees)
  - AVS validators: 90% of (R + fees) proportional to weight w

HALVING: None (logarithmic growth)
```

**⚠️ Governance decision:** Current spec has no hard supply cap. Community discussion recommended.

---

## Implementation Phases

### Phase 1: Core Infrastructure (Weeks 1–2)
- [ ] Implement BLAKE3 hashing
- [ ] Implement Ed25519 signing/verification
- [ ] Implement ECVRF (RFC 9381)
- [ ] Define and test canonical transaction serialization
- [ ] Implement UTXO data structures

**Deliverable:** Test vectors for all crypto operations

### Phase 2: Consensus Layer (Weeks 3–5)
- [ ] Avalanche Snowball state machine
- [ ] VFP generation and validation
- [ ] AVS membership (heartbeats, witnesses)
- [ ] TSDC block production and validation

**Deliverable:** Consensus integration tests (3+ nodes)

### Phase 3: Network Layer (Weeks 6–8)
- [ ] QUIC/TCP transport layer
- [ ] Message serialization (bincode)
- [ ] Peer discovery and bootstrap
- [ ] Message handlers for §11 types

**Deliverable:** P2P network tests (10+ nodes)

### Phase 4: Storage and Archival (Weeks 9–10)
- [ ] UTXO database (RocksDB or similar)
- [ ] Block archive (indexed by height)
- [ ] AVS snapshot retention (7 days)
- [ ] Mempool with eviction policy

**Deliverable:** Integration test with block production

### Phase 5: Client APIs (Weeks 11–12)
- [ ] JSON-RPC endpoint (sendtransaction, gettransaction, getbalance)
- [ ] Wallet integration
- [ ] Block explorer schema
- [ ] Testnet deployment

**Deliverable:** Testnet live (public peer list, faucet)

---

## Testing Strategy

### Unit Tests
```rust
#[cfg(test)]
mod tests {
    #[test]
    fn test_canonical_tx_serialization() {
        // Verify txid matches known vector
    }
    
    #[test]
    fn test_vrf_determinism() {
        // Same input → same output
    }
    
    #[test]
    fn test_finality_vote_signature() {
        // Signature verifies with correct pubkey
    }
}
```

### Integration Tests
- 3-node network: transaction finality in < 1s
- 10-node network: block production every 600s
- Network partition: recovery and canonical chain selection
- Double-spend detection: conflicting VFPs logged

### Testnet
- Public bootstrap nodes
- Web faucet for testnet TIME
- Block explorer
- 72+ hour run-in period before mainnet

---

## Operational Considerations

### Mainnet Readiness Checklist

Before launch:
- [ ] Cryptographic test vectors validated by external auditor
- [ ] Consensus tests passed on 100+ node testnet
- [ ] Network partition recovery tested
- [ ] Performance benchmark: TPS, latency, bandwidth
- [ ] Security audit (external firm)
- [ ] Operator documentation and runbooks
- [ ] Incident response plan

### Monitoring
```
Key metrics:
  - Block production time (should be 600s ± grace period)
  - Finalized transaction count per second
  - VFP assembly time
  - Mempool size and eviction rate
  - Peer count and churn
  - AVS membership changes
```

### Upgrades
The protocol includes no in-band upgrade mechanism. Upgrades MUST be coordinated off-chain (e.g., via governance vote) and deployed to a new chain_id.

---

## Open Questions for Community

1. **Pre-mine:** Should there be an initial supply reserved for the foundation? (Currently: 0)
2. **Reward cap:** Logarithmic rewards have no hard cap. Is a hard cap desired? (Currently: no cap)
3. **Block size:** Is 2 MB sufficient for the target use case?
4. **Fee market:** Should fees be dynamic (EIP-1559 style) or simple median-based?
5. **Storage:** 7-day AVS snapshot retention – sufficient? Scale to 7 days?

---

## References

- RFC 9381: ECVRF – Elliptic Curve Verifiable Random Function
- RFC 9000: QUIC – A UDP-Based Multiplexed and Secure Transport
- BIP 350: bech32m – Format for Encoding Bitcoin Addresses
- BLAKE3 specification: https://blake3.io
- Avalanche consensus: https://arxiv.org/abs/1906.08936

---

## Document Approval

| Role | Name | Date | Signature |
|------|------|------|-----------|
| Protocol Lead | TBD | — | — |
| Lead Developer | TBD | — | — |
| Security Review | TBD | — | — |

---
