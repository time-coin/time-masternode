# TimeCoin Comprehensive Security Audit
## Analysis of All Known Cryptocurrency Attack Vectors

**Date:** January 23, 2026  
**Version:** 1.4  
**Audit Scope:** Full system security analysis against known cryptocurrency vulnerabilities + Bitcoin development insights  
**Last Verification:** April 7, 2026  
**Last Updated:** April 7, 2026 — Section 14 added (April 2026 mainnet attack findings)

---

## Executive Summary

This document provides a comprehensive security analysis of TimeCoin against all major known cryptocurrency attack vectors. The analysis covers consensus, network, transaction, and cryptographic layers, with insights from Bitcoin development community best practices.

**Overall Security Rating: 🟢 STRONG** (with recommended enhancements)

### Key Findings
- ✅ **22 attack vectors fully mitigated** (+6 from April 2026 mainnet findings)
- ⚠️ **4 attack vectors with recommended enhancements**
- ❌ **0 critical vulnerabilities**
- 🟢 **Already 2106-safe** (ahead of Bitcoin's uint32 → uint64 migration)

### Recommended Enhancements (Non-Critical)
1. **VRF grinding resistance**: Add unpredictable entropy (e.g., last_finalized_tx_hash) to VRF input
2. **Vote signature completeness**: Require signatures on both Accept AND Reject votes for full audit trail
3. **Clock drift tracking**: Monitor producer timestamp accuracy over time
4. **Light client design**: Include AVS snapshot commitments in block headers when light clients are implemented

---

## 1. CONSENSUS-LAYER ATTACKS

### 1.1 ✅ 67% Attack (Supermajority Attack)
**Status:** **STRONGLY MITIGATED**

**Attack:** Attacker controls >67% of network resources to rewrite history.

**TimeCoin Protection:**
- **67% BFT-safe finality**: Requires 67%+ of active validator stake to finalize (tolerates up to 33% Byzantine)
- **Stake-weighted voting**: Must acquire supermajority of TIME collateral (expensive)
- **Cryptographic finality proofs**: Finalized blocks have verifiable signatures from 67%+ stake
- **Cannot rewrite finalized blocks**: Once TimeProof assembled, block is immutable
- **Liveness fallback**: If stalled >30s at 67%, threshold drops to 51% to prevent deadlock

**Attack Cost:** Would require acquiring >67% of all staked TIME coins (hundreds of millions in market cap)

**Code References:**
- `src/consensus.rs` - Finality weight threshold Q_finality = 67% (configurable)
- `src/block/types.rs:44-59` - TimeAttestation with witness signatures

---

### 1.2 ✅ Long-Range Attack
**Status:** **MITIGATED**

**Attack:** Attacker uses old private keys to rewrite chain history from genesis.

**TimeCoin Protection:**
- **Checkpoints**: Hardcoded checkpoints in genesis prevent rewriting past certain heights
- **Finality proofs**: Historical TimeProofs make fork detection easy
- **Social consensus**: New nodes bootstrap from trusted checkpoints
- **AVS snapshots**: Validator set captured per slot prevents historical manipulation

**Code References:**
- `src/blockchain.rs:1732-1733` - Checkpoint validation
- `src/block/genesis.rs` - Dynamic genesis generation and verification

---

### 1.3 ✅ Nothing-at-Stake Attack / Vote Equivocation
**Status:** **MOSTLY MITIGATED - ENHANCEMENT RECOMMENDED**

**Attack:** Validators vote on multiple forks simultaneously (no cost to voting).

**TimeCoin Protection:**
- **Single chain finalization**: TimeVote protocol finalizes one chain at a time
- **Fork choice rule**: Prefer chain with most finalized blocks (TimeProofs)
- **BFT consensus**: Requires 67% to finalize, can't finalize conflicting blocks
- **Deterministic leader selection**: All honest nodes agree on next block producer
- **Signature binding**: Votes sign specific block_hash + slot, can't reuse

**⚠️ Enhancement - Vote Signature Completeness:**
Current implementation: Accept votes are signed, Reject votes may not be signed.

**Recommendation for stronger Byzantine fault tolerance:**
```rust
// Require signatures on ALL votes (Accept AND Reject)
pub struct VoteResponse {
    decision: Accept | Reject,
    signature: Signature,  // REQUIRED for both (not just Accept)
    voter_mn_id: String,
    voter_weight: u64,
}
```

**Benefits:**
- Creates cryptographic audit trail of all voting decisions
- Prevents validators from denying they rejected a block
- Enables detection and proof of equivocation (voting for conflicting blocks)
- Strengthens BFT security model

**Code References:**
- `src/consensus.rs:1064-1097` - Prepare vote generation
- `src/consensus.rs:1111-1148` - Precommit vote with block_hash binding

---

### 1.4 ✅ Selfish Mining
**Status:** **MITIGATED**

**Attack:** Miner withholds valid blocks to gain advantage on next block.

**TimeCoin Protection:**
- **Time-scheduled slots**: Blocks produced at fixed 10-minute intervals
- **Deterministic leader selection**: Next leader is known, can't "race" for advantage
- **No PoW mining**: Block production isn't competitive (no mining reward advantage)
- **Immediate broadcast**: Blocks must be broadcast for voting (can't hide)
- **TimeVote finality**: Must accumulate votes from 67% stake to finalize

**Code References:**
- `src/timelock.rs` - Deterministic slot-based leader selection (TimeLock protocol)
- `src/block/vrf.rs` - VRF leader sortition
- `src/main.rs:1326-1440` - Block production and immediate broadcast

---

### 1.5 ⚠️ Stake Grinding / VRF Manipulation
**Status:** **MOSTLY MITIGATED - ENHANCEMENT RECOMMENDED**

**Attack:** Manipulate randomness source to predict/influence future leader selection.

**TimeCoin Protection:**
- ✅ **VRF-based leader selection**: ECVRF (Elliptic Curve Verifiable Random Function) implemented
- ✅ **Cryptographic randomness**: VRF output unpredictable without knowing private key
- ✅ **Verifiable fairness**: VRF proof allows anyone to verify leader selection was fair
- ✅ **Chain head dependency**: VRF input includes previous block hash
- ✅ **No manipulation**: Cannot predict VRF output without producing valid block first

**⚠️ Potential Enhancement - VRF Pre-computation:**
Current VRF input: `prev_block_hash || slot_time || chain_id`
- `slot_time` is predictable (wall clock), allowing pre-computation of future slots
- Attacker with many masternodes could pre-compute winning slots days in advance

**Recommended Enhancement:**
```rust
// Add unpredictable entropy to VRF input
vrf_input = H(
    prev_block_hash ||           // Unpredictable
    slot_time ||                 // Predictable
    chain_id ||                  // Fixed
    last_finalized_tx_hash       // ADD: Recent unpredictable entropy
)
```
This limits pre-computation to 1-2 slots ahead while maintaining determinism.

**Code References:**
- `src/block/vrf.rs` - VRF input construction (ECVRF per RFC 9381)
- Uses ed25519-dalek for ECVRF implementation

---

### 1.6 ✅ Timestamping Attacks
**Status:** **MITIGATED (2106-SAFE)**

**Attack:** Manipulate block timestamps to gain consensus advantage.

**TimeCoin Protection:**
- **Timestamp validation**: Blocks rejected if timestamp too far in past/future
- **Tolerance window**: ±5 seconds future (`TIMESTAMP_TOLERANCE_SECS = 5` in `src/constants.rs`); blocks claiming a future timestamp are rejected immediately
- **Deterministic slot times**: Block timestamps expected at slot_time = genesis + (slot × 600); blocks produced more than 30s early are rejected for recent heights
- **Verification**: Nodes reject blocks with timestamps deviating from expected slot time
- 🟢 **2106-safe**: Uses `i64` timestamps throughout (no uint32 overflow issues like Bitcoin)

**Code References:**
- `src/blockchain.rs` - `validate_block()` (future-timestamp rejection) and `add_block()` (past-timestamp gating)
- `src/constants.rs:63` - `TIMESTAMP_TOLERANCE_SECS = 5`
- `src/timelock.rs` - Slot time calculation (genesis + height × 600)
- `src/block/types.rs` - Block header timestamp field

**Limits:** Timestamps can vary within ±10 minutes, but doesn't affect consensus security.

**⚠️ Future Enhancement - Clock Drift Tracking:**
Consider tracking producer timestamp accuracy over time:
```rust
// Track persistent clock drift per producer
producer_drift_history: HashMap<MnId, Vec<i64>>
// Penalize producers with consistent >3s average drift
```

---

### 1.7 ✅ Eclipse Attack on Consensus
**Status:** **MITIGATED**

**Attack:** Isolate a node to show them a fake chain.

**TimeCoin Protection:**
- **Multiple peer sources**: API discovery + bootstrap peers + peer exchange
- **Peer diversity**: Epsilon-greedy selection (90% best, 10% random)
- **Chain tip comparison**: Queries multiple peers for chain head
- **Fork detection**: AI-powered consensus health monitoring
- **Masternode connections**: Reserved slots for whitelisted masternodes

**Code References:**
- `src/ai/peer_selector.rs` - Epsilon-greedy peer diversity and multi-dimensional scoring
- `src/network/peer_scoring.rs` - Per-peer quality scoring
- `src/main.rs:1331-1380` - Multi-peer chain tip verification

---

## 2. NETWORK-LAYER ATTACKS

### 2.1 ✅ Sybil Attack
**Status:** **STRONGLY MITIGATED**

**Attack:** Create many fake identities to overwhelm network.

**TimeCoin Protection:**
- **Connection limits**: Max 3 connections per IP address
- **Rate limiting**: 10 new connections per minute
- **Behavioral scoring**: Anomaly detection tracks peer behavior
- **Masternode collateral**: Block production requires stake (1,000-100,000 TIME)
- **IP-based reputation**: Persistent peer quality tracking
- **Automatic banning**: Malicious peers banned after 3-10 violations

**Code References:**
- `src/network/connection_manager.rs:232-242` - Per-IP connection limits
- `src/ai/anomaly_detector.rs` - Z-score anomaly detection on network events
- `src/ai/attack_detector.rs` - Sybil/eclipse/fork bombing detection with auto-ban enforcement
- `src/masternode_registry.rs` - Tier collateral requirements

---

### 2.1a ✅ Collateral Anchor Squatting
**Status:** **MITIGATED** (commit `6e6d14e` — 2026-04-06)

**Attack:** Attacker monitors the mempool for a new collateral UTXO (e.g. a Silver
send-to-self from `188.166.243.108`), then gossips a `MasternodeAnnouncement` claiming
that TXID before the legitimate node can announce itself. The attacker's IP is anchored
first and the legitimate owner is permanently locked out.

**Root cause:** Gossip announcements are self-reported — any node can claim any UTXO
outpoint, and the first-claim anchor in sled was permanent. The `wallet_address` field
was unverifiable because it came from the announcement message itself.

**TimeCoin Protection (V4 collateral proof):**
- On startup, the masternode daemon signs `"TIME_COLLATERAL_CLAIM:<txid>:<vout>"` with
  `masternodeprivkey` (from `time.conf`) and broadcasts `MasternodeAnnouncementV4`
  with the signature in `collateral_proof`.
- When a conflict is detected (another IP holds the collateral lock), two conditions
  are tested:
  1. The proof signature verifies against the announcing node's own `public_key` over
     the exact UTXO outpoint — binding this masternode key to this UTXO.
  2. `reward_address == utxo.address` — the announced reward address matches the
     on-chain (immutable) address of the collateral UTXO. Since operators configure
     `reward_address` in `time.conf` to the same address as the collateral UTXO
     output address, this is always true for the legitimate owner.
- If both conditions pass: squatter evicted (lock released, registry entry removed),
  legitimate owner registered.
- **No GUI wallet changes required** — `masternodeprivkey` and the outpoint in
  `masternode.conf` are sufficient. The proof is generated and broadcast automatically.

**Attack economics under the new scheme:**
- To pass condition (1): attacker needs the victim's `masternodeprivkey` — not feasible.
- To pass condition (2) with their own key: attacker sets `reward_address = victim's address`,
  meaning all rewards go to the victim's wallet. No financial upside.
- V4-vs-V4 race: attacker must continuously re-squat (every 60 s) while donating
  all rewards to the victim — economically irrational.

**Code References:**
- `src/network/message_handler.rs` - `handle_masternode_announcement` conflict resolution
- `src/main.rs` - V4 announcement signing in announcement task

---

### 2.2 ✅ DDoS (Distributed Denial of Service)
**Status:** **STRONGLY MITIGATED**

**Attack:** Flood network with requests to exhaust resources.

**TimeCoin Protection:**
- **Per-peer rate limits**:
  - TX: 50/sec
  - Blocks: 10/sec
  - UTXO queries: 100/sec
  - Votes: 100/sec
  - Ping: 2/10sec
- **Memory hard caps**: 50,000 rate limit entries (~2.4MB max)
- **Connection limits**: 125 total connections (100 inbound, 25 outbound)
- **Emergency cleanup**: Automatic entry eviction when approaching limits
- **Graduated banning**: Auto-ban after repeated violations

**Code References:**
- `src/network/rate_limiter.rs:35-60` - Per-message rate limits
- `src/network/rate_limiter.rs:173-201` - Memory protection

---

### 2.3 ✅ Eclipse Attack (Network Isolation)
**Status:** **MITIGATED**

**Attack:** Surround node with attacker-controlled peers to isolate from network.

**TimeCoin Protection:**
- **Diverse peer selection**: AI-based scoring on 5+ dimensions
- **Random exploration**: 10% of connections try new peers
- **Multiple connection sources**: API + bootstrap + peer exchange
- **Masternode slots**: 50/125 slots reserved for whitelisted nodes
- **Connection diversity**: Separate inbound/outbound limits

**Code References:**
- `src/ai/peer_selector.rs` - Multi-dimensional peer scoring
- `src/network/peer_scoring.rs` - Per-peer quality metrics
- `src/network/connection_manager.rs:178-202` - Connection slot management

---

### 2.4 ✅ BGP Hijacking / Routing Attacks
**Status:** **MITIGATED (TLS ENABLED BY DEFAULT)**

**Attack:** Hijack network routes to intercept/modify traffic.

**TimeCoin Protection:**
- ✅ **Cryptographic message authentication**: Ed25519 signatures on all consensus messages
- ✅ **Block hash verification**: Tampering detected via SHA256 hashes
- ✅ **P2P redundancy**: Multiple peer connections reduce single-point failure
- ✅ **TLS integrated and enabled by default**: `enable_tls = true` in `SecurityConfig`; server wraps accepted connections via `tls.accept_server()`, client connects via `set_tls_config()`
- ✅ **Self-signed P2P certificates**: Auto-generated on first start via `TlsConfig::new_self_signed()`. Custom certs supported via `tls=` / `tlscert=` / `tlskey=` config keys.

**Current Status:** TLS is active on mainnet by default. Disable with `tls=0` in `time.conf` (for isolated testnets only).

**Code References:**
- `src/network/tls.rs` - TLS implementation (rustls)
- `src/network/server.rs:542-587` - TLS accept on inbound connections
- `src/main.rs:4460-4464` - TLS config wiring at daemon startup
- `src/config.rs:629` - `enable_tls: true` default

---

### 2.5 ✅ Message Replay Attacks
**Status:** **STRONGLY MITIGATED**

**Attack:** Replay old network messages to cause confusion.

**TimeCoin Protection:**
- **Dual-window Bloom filters**: Time-windowed deduplication
  - Blocks: 5-minute rotation
  - Transactions: 10-minute rotation
- **Atomic rotation**: Prevents race conditions during filter refresh
- **Chain-ID binding**: Messages bound to specific chain (mainnet/testnet)
- **Slot-time binding**: Finality votes expire after slot
- **Memory-efficient**: ~125KB per 10k items

**Code References:**
- `src/network/dedup_filter.rs:43-88` - Dual-window deduplication
- `src/types.rs:268-298` - Chain-ID in signed messages

---

### 2.6 ⚠️ Light Client Security
**Status:** **TRUST MODEL NEEDS SPECIFICATION**

**Attack:** Malicious full node provides fake data to light client.

**Current Status:** Light client implementation not yet specified in protocol.

**Future Consideration - AVS Snapshot Verification:**
When light clients are implemented, they will need to verify AVS snapshots used in TimeProof validation.

**Recommended Approach:**
```rust
// Include AVS snapshot commitment in block headers
pub struct BlockHeader {
    // ... existing fields ...
    avs_snapshot_root: Hash256,  // Merkle root of AVS composition
}
```

**Benefits:**
- Light clients can cryptographically verify AVS snapshots
- Query multiple nodes and compare against header commitment
- No trust assumption on individual full nodes (except for availability)
- Prevents fake TimeProof attacks

**Priority:** 🟡 MEDIUM - Address when light client protocol is designed

**Code References:**
- Protocol Specification §21 (if exists) - Light client design
- `src/block/types.rs` - BlockHeader structure

---

## 3. TRANSACTION-LAYER ATTACKS

### 3.1 ✅ Double-Spend Attack
**Status:** **STRONGLY MITIGATED**

**Attack:** Spend same UTXO twice in different transactions.

**TimeCoin Protection:**
- **UTXO locking**: Atomic lock with 10-minute timeout
- **State machine**: Unspent → Locked → Confirmed → SpentFinalized
- **Lock conflict detection**: Second transaction automatically rejected
- **Mempool deduplication**: Same transaction can't enter mempool twice
- **Block validation**: Checks for double-spends within block

**Code References:**
- `src/utxo_manager.rs:179-227` - Atomic UTXO locking
- `src/network/message_handler.rs:2272-2284` - Pre-vote double-spend check

---

### 3.2 ✅ Transaction Malleability
**Status:** **NOT APPLICABLE (DESIGN PREVENTS)**

**Attack:** Modify transaction ID without invalidating signature.

**TimeCoin Protection:**
- **Ed25519 signatures**: Fixed 64-byte signatures (not malleable)
- **Signature covers entire TX**: Signs `SHA256(txid || input_index || outputs_hash)`
- **TXID = SHA256(tx)**: Any modification changes TXID and breaks signature
- **No script malleability**: Simple script_pubkey (no complex opcodes)

**Code References:**
- `src/consensus.rs:1439-1466` - Signature message creation
- `src/transaction.rs:112-123` - TXID calculation

---

### 3.3 ✅ Fee Sniping / Replace-by-Fee (RBF) Attacks
**Status:** **NOT APPLICABLE (NO RBF)**

**Attack:** Replace low-fee transaction with higher-fee version to double-spend.

**TimeCoin Protection:**
- **No RBF support**: First valid transaction locks UTXOs
- **Locked UTXOs can't be respent**: Second transaction rejected immediately
- **Mempool immutability**: Once in mempool, transaction can't be replaced
- **Minimum fees enforced**: 1,000 satoshis absolute + 0.1% proportional

**Code References:**
- `src/utxo_manager.rs:179-227` - UTXO locking prevents replacement
- `src/consensus.rs:1396-1416` - Fee validation

---

### 3.4 ✅ Dust Attacks
**Status:** **MITIGATED**

**Attack:** Create many tiny UTXOs to bloat UTXO set.

**TimeCoin Protection:**
- **Dust threshold**: 546 satoshi minimum output (0.00000546 TIME)
- **Proportional fees**: 0.1% fee makes dust transactions expensive
- **Economic infeasibility**: Spamming dust costs 0.1% per transaction
- **Mempool limits**: 100MB cap + LRU eviction

**Code References:**
- `src/consensus.rs:1386-1393` - Dust rejection
- `src/consensus.rs:1408-1416` - Proportional fee requirement

---

### 3.5 ✅ Front-Running
**Status:** **LIMITED (INHERENT TO TRANSPARENT MEMPOOLS)**

**Attack:** See pending transaction and submit competing transaction with higher fee.

**TimeCoin Protection:**
- ⚠️ **Mempool visible**: Pending transactions broadcast to network
- ✅ **UTXO locking**: First transaction to lock UTXO wins
- ✅ **No RBF**: Can't replace transaction with higher-fee version
- ✅ **Deterministic block inclusion**: Leader can't easily exclude transactions
- ✅ **10-minute blocks**: Less time-sensitive than fast chains

**Inherent Limitation:** Transparent mempool allows MEV (Miner Extractable Value).

**Potential Enhancement:** Add private mempool or commit-reveal schemes for sensitive transactions.

**Code References:**
- `src/transaction_pool.rs:169-193` - Mempool transaction management

---

### 3.6 ✅ Signature Forgery
**Status:** **CRYPTOGRAPHICALLY IMPOSSIBLE**

**Attack:** Forge valid signatures to spend others' UTXOs.

**TimeCoin Protection:**
- **Ed25519 cryptography**: Industry-standard, 128-bit security level
- **Full signature verification**: Every input signature checked
- **Public key in UTXO**: script_pubkey contains 32-byte Ed25519 public key
- **Message binding**: Signature covers txid + input_index + outputs_hash

**Code References:**
- `src/consensus.rs:1468-1538` - Ed25519 signature verification
- Dependencies: `ed25519-dalek = "2.1.1"` (audited library)

---

## 4. BLOCK PRODUCTION ATTACKS

### 4.1 ✅ JUST FIXED: Invalid Block Consensus
**Status:** **FIXED (January 19, 2026)**

**Attack:** Propose blocks with invalid transactions/UTXOs to disrupt network.

**Previous Vulnerability:** Nodes voted on blocks before validating transactions.

**Current Protection (NEW):**
- ✅ **Pre-vote validation**: All blocks validated BEFORE voting
- ✅ **Transaction signature checks**: Every TX verified before vote
- ✅ **UTXO existence checks**: Inputs must exist before vote
- ✅ **Block reward validation**: Coinbase + distribution checked before vote
- ✅ **Double-spend detection**: Within-block conflicts detected before vote
- ✅ **Merkle root validation**: Validated before vote

**Code References:**
- `src/network/message_handler.rs:2187-2291` - Pre-vote validation (NEW)
- `src/network/message_handler.rs:2293-2362` - Block reward structure validation (NEW)

---

### 4.2 ✅ Block Withholding
**Status:** **MITIGATED**

**Attack:** Leader produces block but doesn't broadcast to gain advantage.

**TimeCoin Protection:**
- **Deterministic slots**: Next leader known, no advantage to withholding
- **Voting required**: Must broadcast to accumulate votes for finalization
- **Backup leaders**: If primary offline, backup leader triggers after 5 seconds
- **Liveness timeout**: After 30 seconds, TimeGuard protocol forces resolution
- **No mining rewards**: Can't "mine ahead" like PoW

**Code References:**
- `src/main.rs:1326-1440` - Block production and broadcast
- `src/timelock.rs` - Backup leader fallback (TimeGuard protocol)

---

### 4.3 ✅ JUST FIXED: Double Block Rewards
**Status:** **FIXED (January 19, 2026)**

**Attack:** Claim block rewards multiple times per block.

**Previous Vulnerability:** Block rewards added as both metadata AND transaction outputs.

**Current Protection (NEW):**
- ✅ **Single reward source**: Only reward_distribution transaction creates UTXOs
- ✅ **Validation**: Coinbase must create exactly BLOCK_REWARD_SATOSHIS
- ✅ **Distribution validation**: Outputs must match masternode_rewards metadata
- ✅ **No duplicate UTXOs**: masternode_rewards array is metadata only
- ✅ **Total amount check**: Distributed amount must equal block_reward

**Code References:**
- `src/blockchain.rs:2285-2429` - Block reward validation (NEW)
- `src/blockchain.rs:2160-2250` - UTXO processing (masternode_rewards not processed)

---

## 5. CRYPTOGRAPHIC ATTACKS

### 5.1 ✅ Hash Collision Attacks
**Status:** **CRYPTOGRAPHICALLY SECURE**

**Attack:** Find two inputs that produce same hash to forge blocks/transactions.

**TimeCoin Protection:**
- **SHA256 everywhere**: 2^256 hash space (collision-resistant)
- **Ed25519 hashing**: SHA512 internally (stronger than SHA256)
- **Merkle tree integrity**: Would require 2^256 operations to forge
- **Block hash binding**: Signatures cover block_hash (collision would break chain)

**Code References:**
- `src/block/types.rs:101-111` - Block hash calculation (SHA256)
- `src/transaction.rs:112-123` - TXID calculation (SHA256)

---

### 5.2 ✅ Quantum Computing Attacks
**Status:** **VULNERABLE TO FUTURE QUANTUM (INDUSTRY STANDARD)**

**Attack:** Use quantum computer to break Ed25519 signatures.

**Current Status:**
- ⚠️ **Ed25519 vulnerable to Shor's algorithm** (theoretical quantum attack)
- ⚠️ **SHA256 partially vulnerable** to Grover's algorithm (reduces security to 128-bit)
- ✅ **No quantum computers capable yet** (estimated 10-20 years away)

**Industry Context:** Bitcoin, Ethereum, and most cryptocurrencies use similar algorithms.

**Recommendation:** Monitor post-quantum cryptography research (e.g., NIST PQC finalists).

**Future Upgrade Path:** 
- Implement hybrid signatures (Ed25519 + Dilithium/SPHINCS+)
- Add post-quantum hash function (SHA3-256)

---

### 5.3 ✅ Replay Attacks (Cross-Chain)
**Status:** **MITIGATED**

**Attack:** Replay mainnet transaction on testnet or vice versa.

**TimeCoin Protection:**
- **Chain-ID binding**: Signatures include chain_id (mainnet=1, testnet=2, devnet=3)
- **Different networks**: Separate genesis blocks, different ports
- **Signature domain separation**: VRF and finality votes include chain_id

**Code References:**
- `src/types.rs:268-298` - Chain-ID in SignedMessage
- `time.conf` (mainnet) vs `time.conf` (testnet) with `testnet=1` - Separate configurations

---

## 6. GOVERNANCE & SOCIAL ATTACKS

### 6.1 ✅ Governance Capture
**Status:** **PARTIALLY MITIGATED**

**Attack:** Wealthy entity buys stake to control governance votes.

**TimeCoin Protection:**
- **Tier collateral requirements**: Minimum 1,000 TIME for Bronze tier voting
- **Stake-weighted voting**: Proportional to collateral (prevents Sybil)
- **Uptime requirements**: Must maintain 90%+ uptime to vote
- **Health AI monitoring**: Unhealthy nodes excluded from governance
- ⚠️ **Plutocracy risk**: Whales with Gold tier (100,000 TIME) have 100x vote weight

**Recommendation:** Consider quadratic voting or voting caps to limit whale influence.

**Code References:**
- `src/masternode_registry.rs:228-257` - Tier collateral requirements

---

### 6.2 ✅ Bribery / Vote Buying
**Status:** **MONITORING ONLY (HARD TO PREVENT)**

**Attack:** Bribe validators to vote certain way.

**Inherent Limitation:** Off-chain coordination difficult to prevent technically.

**TimeCoin Protections:**
- **Anonymous voting**: Votes signed but voter identity in pseudonymous (address-based)
- **Verifiable finality**: All votes public, can audit for suspicious patterns
- **Stake slashing potential**: Future upgrade could slash malicious voters

**Recommendation:** Implement reputation system and stake slashing for provable misbehavior.

---

## 7. ECONOMIC ATTACKS

### 7.1 ✅ Inflation Attacks
**Status:** **IMPOSSIBLE**

**Attack:** Create TIME coins from nothing.

**TimeCoin Protection:**
- **Fixed block rewards**: 100 TIME per block, enforced in validation
- **Transaction balance check**: input_sum ≥ output_sum strictly enforced
- **No minting outside blocks**: Only coinbase can create new TIME
- **Block reward validation**: Enforced in both add_block() and pre-vote validation
- **UTXO set integrity**: Can calculate total supply by summing UTXO set

**Code References:**
- `src/consensus.rs:1418-1423` - Input ≥ output check
- `src/blockchain.rs:2285-2429` - Block reward validation

---

### 7.2 ✅ Deflationary Attacks (Lost Coins)
**Status:** **NOT AN ATTACK (ECONOMIC FEATURE)**

**Observation:** Coins sent to unspendable addresses are effectively burned.

**TimeCoin Behavior:**
- Lost coins remain in UTXO set but never spent
- Effective supply decreases over time (deflationary pressure)
- Not exploitable (attacker loses coins)

---

## 8. IMPLEMENTATION-LEVEL VULNERABILITIES

### 8.1 ✅ Memory Exhaustion
**Status:** **MITIGATED**

**Attack:** Exhaust node memory with large data structures.

**TimeCoin Protection:**
- **Mempool cap**: 100MB total, 10,000 transaction limit
- **Rate limiter cap**: 50,000 entries (~2.4MB)
- **Block size limit**: 4MB maximum
- **Transaction size limit**: 1MB maximum
- **Automatic eviction**: LRU policy at 80% capacity
- **Bloom filter sizes**: Fixed at initialization (125KB per 10k items)

**Code References:**
- `src/transaction_pool.rs:169-193` - Mempool size limits
- `src/network/rate_limiter.rs:173-201` - Rate limiter memory protection

---

### 8.2 ✅ Deadlocks / Race Conditions
**Status:** **MITIGATED (RUST SAFETY)**

**Attack:** Trigger deadlocks to halt node.

**TimeCoin Protection:**
- **Rust borrow checker**: Prevents data races at compile time
- **Lock-free data structures**: DashMap for concurrent access
- **Atomic operations**: AtomicU64 for height, lock counts
- **Tokio async runtime**: Prevents blocking I/O deadlocks
- **Timeout mechanisms**: All network operations have timeouts

**Code References:**
- Language-level: Rust's type system prevents most concurrency bugs

---

### 8.3 ✅ Integer Overflow
**Status:** **PROTECTED (RUST DEBUG CHECKS)**

**Attack:** Overflow arithmetic to manipulate values.

**TimeCoin Protection:**
- **Rust overflow checks**: Debug builds panic on overflow
- **Saturating arithmetic**: Uses `.saturating_sub()` and `.saturating_add()` where appropriate
- **Checked arithmetic**: Uses `.checked_add()` for critical paths
- **u64 for amounts**: 18.4 quintillion satoshis max (far exceeds supply)

**Code References:**
- `src/blockchain.rs:1398` - `saturating_sub()` for fees
- `src/consensus.rs:1397` - Checked arithmetic in validation

---

## 9. AI-SPECIFIC ATTACKS (TIMECOIN-UNIQUE)

### 9.1 ✅ AI Consensus Health Manipulation
**Status:** **MONITORED**

**Attack:** Manipulate AI health predictions to exclude honest nodes.

**TimeCoin Protection:**
- **Multi-factor health scoring**: Response validity, fork attempts, request rate, timing
- **Weight distribution**: 40% validity, 30% forks, 20% rate, 10% timing
- **Threshold-based**: Requires 0.7+ score for "anomalous" classification
- **Gradual banning**: 3-10 violations before permanent ban
- **Whitelist bypass**: Masternodes can whitelist to skip AI checks

**Limitation:** AI model could have false positives/negatives.

**Recommendation:** Regular model retraining and adversarial testing.

**Code References:**
- `src/ai/anomaly_detector.rs` - Z-score anomaly detection
- `src/ai/attack_detector.rs` - Attack pattern detection with mitigation enforcement
- `src/ai/consensus_health.rs` - Consensus health monitoring and prediction

---

## 10. SUPPLY CHAIN & DEPENDENCY ATTACKS

### 10.1 ⚠️ Dependency Vulnerabilities
**Status:** **REQUIRES REGULAR AUDITING**

**Risk:** Vulnerabilities in third-party libraries (e.g., ed25519-dalek, tokio, sled).

**TimeCoin Protection:**
- ✅ **Rust's cargo ecosystem**: Cryptographically verified dependencies
- ✅ **Well-audited libraries**: Using mainstream crates (tokio, serde, ed25519-dalek)
- ⚠️ **Manual review needed**: Should regularly audit dependencies

**Recommendation:**
- Run `cargo audit` regularly
- Subscribe to RustSec advisories
- Consider cargo-deny for policy enforcement

**Code References:**
- `Cargo.toml` - All dependencies listed

---

## 11. RPC API ATTACKS

*Added: March 2026 — covers the RPC attack surface not addressed in the original audit.*

### 11.1 ✅ FIXED — RPC Exposed to Public Internet
**Status:** **FIXED in v1.2.0**

**Risk:** RPC server was bound to `0.0.0.0`, allowing any internet host to execute wallet-draining commands (`sendtoaddress`, `sendfrom`, `sendrawtransaction`).

**Fix Applied:**
- ✅ **Default bind changed to `127.0.0.1`** in both code (`config.rs`) and install script
- ✅ **Install script no longer sets `rpcallowip=0.0.0.0/0`**

**Code References:**
- `src/config.rs` — `RpcConfig` default `listen_address`
- `scripts/install-masternode.sh` — config generation

### 11.2 ✅ FIXED — No RPC Authentication
**Status:** **FIXED in v1.2.0**

**Risk:** RPC server accepted all requests without any authentication. Any local process could drain the wallet.

**Fix Applied:**
- ✅ **HTTP Basic Auth** with `rpcuser`/`rpcpassword` in `time.conf`
- ✅ **Auto-generated credentials** on first run (16-char user, 32-char password)
- ✅ **`.cookie` file** written for CLI tool authentication (owner-read-only)
- ✅ **`time-cli` reads `.cookie`** automatically (also accepts `--rpcuser`/`--rpcpassword`)
- ✅ **Existing configs auto-upgraded** with generated credentials

**Code References:**
- `src/rpc/server.rs` — Basic Auth checking in `handle_connection()`
- `src/config.rs` — `RpcConfig.rpcuser`/`rpcpassword`, `write_rpc_cookie()`
- `src/bin/time-cli.rs` — `read_cookie_file()`, `read_conf_credentials()`

### 11.3 ✅ FIXED — No RPC Rate Limiting
**Status:** **FIXED in v1.2.0**

**Risk:** No rate limiting on RPC allowed resource exhaustion via rapid-fire requests.

**Fix Applied:**
- ✅ **Per-IP rate limiter** (100 requests/second) in RPC server
- ✅ **429 Too Many Requests** response when exceeded
- ✅ **Automatic cleanup** of stale entries every 60 seconds

**Code References:**
- `src/rpc/server.rs` — `RpcRateLimiter` struct

### 11.4 ✅ FIXED — CORS Allows All Origins
**Status:** **FIXED in v1.2.0**

**Risk:** Default `allow_origins: ["*"]` could enable cross-origin attacks if RPC were exposed.

**Fix Applied:**
- ✅ **Default restricted** to `["http://localhost", "http://127.0.0.1"]`

**Code References:**
- `src/config.rs` — `RpcConfig` default `allow_origins`

### 11.5 ✅ FIXED — RPC Credentials Stored in Plaintext
**Status:** **FIXED in v1.2.0**

**Risk:** `rpcuser`/`rpcpassword` stored in cleartext in `time.conf` could be read by any process with file access.

**Fix Applied:**
- ✅ **`rpcauth` hashed credentials** — Bitcoin Core-compatible format `rpcauth=user:salt$hash`
- ✅ **HMAC-SHA256** verification (password never stored, only hash)
- ✅ **Multiple `rpcauth` entries** supported for multi-user setups
- ✅ **Generator script** at `scripts/rpcauth.py` for creating hashed credentials
- ✅ **Backward compatible** — plaintext `rpcuser`/`rpcpassword` still works for simplicity

**Code References:**
- `src/rpc/server.rs` — `RpcAuthenticator` with `RpcAuthEntry` hashed credential support
- `src/config.rs` — `RpcConfig.rpcauth` field, parsed from `time.conf`
- `scripts/rpcauth.py` — HMAC-SHA256 credential generator

### 11.6 ✅ FIXED — No TLS for RPC
**Status:** **FIXED in v1.2.0**

**Risk:** RPC over plain HTTP exposes credentials and wallet commands to interception on the local machine or network.

**Fix Applied:**
- ✅ **Optional TLS** via `rpctls=1` in `time.conf`
- ✅ **Auto-generated self-signed certificate** when no cert/key files specified
- ✅ **Custom certificate support** via `rpctlscert`/`rpctlskey` config options
- ✅ **Reuses existing TLS infrastructure** from `src/network/tls.rs` (rustls)
- ✅ **Graceful fallback** — if TLS init fails, server falls back to plain HTTP with warning

**Code References:**
- `src/rpc/server.rs` — `set_tls()`, TLS accept in `run()` loop
- `src/main.rs` — TLS config loading and `TlsConfig` integration
- `src/config.rs` — `rpctls`, `rpctlscert`, `rpctlskey` fields

---

## 12. WALLET SECURITY

*Added: March 2026*

### 12.1 ✅ FIXED — Hardcoded Default Wallet Password
**Status:** **FIXED in v1.2.0**

**Risk:** Wallet encrypted with AES-256-GCM (strong) but default password was hardcoded as `"timecoin"` — trivially guessable.

**Fix Applied:**
- ✅ **Auto-generated 32-char random password** on first wallet creation
- ✅ **Password stored in `.wallet_password`** (owner-read-only permissions)
- ✅ **Legacy wallet migration**: existing wallets re-encrypted with new password on first load
- ✅ **Hardcoded password removed** from production path

**Code References:**
- `src/wallet.rs` — `WalletManager::resolve_password()`, `save_password_file()`

---

## 13. MESSAGE SIGNING ENFORCEMENT

*Added: March 2026*

### 13.1 ✅ FIXED — Unsigned Vote Acceptance
**Status:** **FIXED in v1.2.0**

**Risk:** TimeVote messages accepted empty signatures for "backward compatibility" and also accepted votes from unknown/unregistered masternodes, allowing vote forgery.

**Fix Applied:**
- ✅ **Empty signatures rejected** with warning log
- ✅ **Unknown voter votes rejected** (must be registered masternode)

**Remaining Work:**
- ⚠️ General network messages (Ping, BlockAnnouncement, TransactionBroadcast, etc.) still unsigned — `SignedMessage` wrapper exists but is not used in the wire protocol
- **Recommendation:** Introduce a protocol version bump that requires `SignedMessage` wrapping for all message types

**Code References:**
- `src/network/message_handler.rs` — `verify_vote_signature()`
- `src/network/signed_message.rs` — `SignedMessage` struct (available but unused for non-vote messages)

---

## 14. APRIL 2026 MAINNET ATTACK FINDINGS

*Added: April 7, 2026 — Observations from live mainnet attacks across LW-Michigan2, LW-Arizona, and DO-Singapore nodes.*

---

### 14.1 ✅ FIXED — Sybil Subnet Attack (`154.217.246.0/24`)
**Status:** **FIXED in v1.4.34** (commit d8ac235)

**Attack:** A coordinated Sybil network of 15+ IP addresses from the `154.217.246.0/24` subnet repeatedly sent `MasternodeAnnouncement` messages claiming ownership of legitimate masternodes' collateral UTXOs. The attack generated 200–300 "Registry conflict" log lines per 2-second window with **zero penalty** — pure noise that masked real events in the log and consumed CPU/IO.

**Observed Behavior:**
- IPs involved: `154.217.246.19`, `.33`, `.34`, `.48`, `.67`, `.86`, `.105`, `.111`, `.130`, `.181`, `.187`, `.194`
- All claiming outpoints anchored to legitimate nodes (e.g., `96d12d31...`, `45d22fd2...`, `0d16a18c...`)
- `154.217.246.33` additionally operated as a pre-handshake prober (sending data before completing the handshake)
- Free-tier nodes from the subnet performed rapid IP migrations to steal collateral, cycling between registrations

**Root Cause:** The "Registry conflict" code path (`message_handler.rs` around the `can_evict == false` branch) had no rate limiting and recorded zero violations — attackers could spam it indefinitely at no cost.

**Fix Applied:**
- ✅ **Rate-limited WARN** to once per 5 minutes per peer IP (prevents log flooding)
- ✅ **`record_violation()` on every rejection** — peer gets auto-banned after repeated attempts (3 violations → 1 min, 5 → 5 min, 10 → permanent)
- ✅ **Coordinated /24 Sybil auto-detection** — if ≥5 unique IPs from the same /24 subnet claim the same collateral outpoint within 60 seconds, the entire /24 is automatically subnet-banned
- ✅ **`bansubnet=` config option** — operators can statically ban entire CIDR ranges in `time.conf` (e.g., `bansubnet=154.217.246.0/24`); bans are enforced at the TCP accept level before any handshake

**Code References:**
- `src/network/message_handler.rs` — Registry conflict path with rate limiting, violation recording, and Sybil auto-detection
- `src/network/blacklist.rs` — `add_subnet_ban()`, `in_banned_subnet()`, `subnet_ban_count()`
- `src/config.rs` — `blacklisted_subnets` field; `bansubnet=` parser
- `src/network/server.rs` — Subnet ban enforcement at TCP accept; `new_with_blacklist()` subnet init loop

---

### 14.2 ✅ FIXED — Pre-Handshake Prober Attack
**Status:** **FIXED in v1.4.34** (commit 948041f)

**Attack:** Nodes (notably `154.217.246.33`, `43.129.27.42`, `8.218.124.20`, `39.174.152.101`, `104.28.165.55`) connected every ~30 seconds and sent protocol data **before completing the handshake** (before the Handshake message exchange). This is a probing/fingerprinting technique and a resource exhaustion vector.

**Observed Behavior (live logs):**
```
⚠️  154.217.246.33:59680 sent message before handshake - closing connection
⚠️  Violation #5 from 154.217.246.33: Sent message before completing handshake
🚫 Auto-banned 154.217.246.33 for 5 minutes (5 violations)
```

**Fix Applied:**
- ✅ **`blacklist.record_violation()` called immediately** on every pre-handshake message (in addition to AI detection)
- ✅ **Auto-banned** after 5 violations (5-minute ban); permanent after 10

**Code References:**
- `src/network/server.rs` — Pre-handshake violation handler (`record_violation()` on early message)
- `src/ai/attack_detector.rs` — `record_pre_handshake_violation()`; ≥10 violations → `BlockPeer`

---

### 14.3 ✅ FIXED — Collateral Hijack / Free-Tier Squatting Attack
**Status:** **FIXED in v1.4.34** (commits 948041f, d8ac235)

**Attack:** Two variants were observed:

**Variant A — Free-tier squatting on paid-tier collateral:** Nodes (`154.217.246.187`, `47.79.37.107`, `154.217.246.67`) repeatedly attempted to claim UTXOs belonging to paid Silver/Bronze masternodes (`165.84.215.117`, `64.91.224.76`) via gossip announcements. These attempts generated "Free-tier claim rejected" messages at 20+ per second with no per-IP penalty.

**Variant B — Direct collateral hijack:** `47.79.39.125` (DO-Singapore) and `69.167.169.81` attempted to claim Silver-tier collateral UTXOs (`a579a134...`, `0d16a18c...`) belonging to other nodes, with no matching UTXO address proof.

**Fix Applied:**
- ✅ **`record_severe_violation()` on `CollateralAlreadyLocked`** — covers both free-tier rejection and hijack rejection paths (both return this error). First severe violation → 1-hour ban; second → permanent ban.
- ✅ **`record_collateral_spoof_attempt()` in AI detector** — feeds attack pattern into coordinated detection

**Code References:**
- `src/network/message_handler.rs` — `CollateralAlreadyLocked` handlers for paid-tier and free-tier paths (lines ~3776 and ~3857)
- `src/ai/attack_detector.rs` — `record_collateral_spoof_attempt()`
- `src/masternode_registry.rs` — `Free-tier claim rejected` and `Collateral hijack rejected` paths (both return `CollateralAlreadyLocked`)

---

### 14.4 ✅ FIXED — GossipEvictionStorm Attack
**Status:** **FIXED in v1.3.x** (pre-existing fix, observed working correctly in April 2026 logs)

**Attack:** `69.167.168.176` triggered a `GossipEvictionStorm` — rapidly broadcasting masternode announcements to evict legitimate nodes from the registry by cycling through their collateral outpoints. This is a variant of the V4 eviction abuse.

**Observed Behavior (live logs):**
```
Peer 69.167.168.176 is blacklisted: Temporarily banned for 3416s: SEVERE: GossipEvictionStorm
[Outbound] REJECTING message from blacklisted peer 69.167.168.176
```
The node kept attempting to reconnect but was correctly rejected every time.

**Fix Status:** Existing AI eviction storm detector is working. The 3416-second ban correctly persisted across reconnection attempts.

**Code References:**
- `src/ai/attack_detector.rs` — `GossipEvictionStorm` attack type
- `src/network/server.rs` — Blacklist check at TCP accept and outbound message receipt

---

### 14.5 ✅ FIXED — Oversized Frame (Memory Exhaustion) Attack
**Status:** **FIXED in v1.4.34** (commit pending in server.rs)

**Attack:** A peer sent a TCP frame with a 4-byte length header claiming a body size of **2,823,396,163 bytes (~2.8 GB)**. Since only 4 bytes need to be sent (the length prefix), this is a trivially cheap attack. The previous code caught the oversize and disconnected the peer, but recorded **zero violation** — the attacker could reconnect and repeat indefinitely at no cost.

**Observed (live log, line 373):**
```
Connection from 188.166.243.108:60880 ended: Frame too large: 2823396163 bytes (max: 8388608)
```

**Fix Applied:**
- ✅ **`blacklist.record_violation()` on "Frame too large" error** — attacker is penalized: 3 oversized frames → 1-minute ban, 5 → 5-minute ban, 10 → permanent ban

**Note:** In the observed instance, `188.166.243.108` is a legitimate node running an outdated binary with a serialization bug (not a malicious attacker). The fix is still correct: whitelisted IPs bypass the blacklist check, so the operator's own node will not be penalized.

**Code References:**
- `src/network/server.rs` — `Err(e)` branch in message read loop; `record_violation()` when `e.contains("Frame too large")`
- `src/network/wire.rs` — `read_message()`: frame size check against `MAX_FRAME_SIZE` (8 MB)

---

### 14.6 ✅ FIXED — UTXO Lock Flood Attack
**Status:** **FIXED in v1.4.33** (commit 3c8bc59)

**Attack:** A peer sent an abnormally high number of `UTXOStateUpdate(Locked)` messages for the same transaction — far exceeding the number of inputs any legitimate transaction would have. This is a resource exhaustion attack targeting the UTXO manager lock/unlock machinery.

**Attack Source:** `47.79.39.125`, `188.166.243.108` (old binary bug)

**Fix Applied:**
- ✅ **Per-connection per-TX UTXO lock counter** — max 50 lock messages per TX per connection
- ✅ **AI auto-ban** via `record_utxo_lock_flood()` when threshold exceeded
- ✅ **"Applied UTXO lock" logs downgraded** INFO → DEBUG to reduce log noise from legitimate traffic

**Code References:**
- `src/network/server.rs` — `peer_tx_lock_counts` HashMap; `MAX_UTXO_LOCKS_PER_TX = 50`
- `src/ai/attack_detector.rs` — `UtxoLockFlood` attack type; `record_utxo_lock_flood()`

---

### 14.7 ✅ FIXED — V4 Eviction Oscillation / IP Cycling (Free-tier Re-squatting)
**Status:** **FIXED in v1.4.34** (commits `1b9bf31`, `a028b52`, `651799c`)

**Attack:** After a legitimate node uses a V4 collateral proof to evict a free-tier squatter, the squatter immediately re-registers via "free-tier IP migration" from a different IP in the same Sybil subnet. This creates an oscillation loop:

1. `154.217.246.19` squats on `96d12d31...` (registered to `188.166.243.108`)
2. `188.166.243.108` presents V4 proof → evicts squatter ✅
3. V4 eviction storm cooldown prevents rapid re-eviction
4. `154.217.246.19` re-migrates collateral from yet another squatter
5. Repeat

**Observed (live log):**
```
✅ V4 collateral proof verified: evicting squatter 154.217.246.19 → 188.166.243.108 for 96d12d31...
🛡️ V4 eviction storm blocked for 96d12d31... (154.217.246.19 → 154.217.246.194) — cooldown active
🔄 Free-tier IP migration: 96d12d31... moving from 154.217.246.19 to 69.167.169.81
```

**Current Mitigation:** Subnet ban of `154.217.246.0/24` (auto-triggered or manually via `bansubnet=`) stops the migration at the TCP level. With v1.4.34, these IPs accumulate violations faster and reach permanent ban sooner.

**Fixes Applied (v1.4.34):**
- ✅ **`MIGRATION_COOLDOWN_SECS` raised 60s → 300s** — reduces cycling frequency by 5×; attacker can no longer flip on every block slot
- ✅ **Back-and-forth cycling detection** — new `collateral_migration_from` field tracks the source IP of the last accepted migration per outpoint. If the incoming IP matches the previous-from IP within `CYCLING_LOCKOUT_SECS = 600`, the migration is rejected as AV3.
- ✅ **`record_synchronized_disconnect()`** — if ≥5 masternodes from the same /24 disconnect within 30s, the specific offending IP is blocked (not the whole subnet, to avoid collateral damage to legitimate cloud-hosted nodes)

**Code References:**
- `src/masternode_registry.rs` — `MIGRATION_COOLDOWN_SECS = 300`; `collateral_migration_from` field; cycling detection before `collateral_migration_times.insert()`
- `src/ai/attack_detector.rs` — `record_synchronized_disconnect()`; `SynchronizedCycling` attack type
- `src/network/server.rs` — `record_synchronized_disconnect()` called in `handle_peer` cleanup after `mark_inactive_on_disconnect`

---

### 14.8 ✅ FIXED — Ghost Connection OOM / Distributed SNI Flood
**Status:** **FIXED in v1.4.34** (commits `2778693`, `1affdfc`, `a028b52`)

**Attack:** A coordinated botnet sends ~10 TLS connections per second from distributed IPs. Each connection presents the victim node's own IP address as the TLS SNI hostname (e.g., `35302e32382e3130342e3530` = hex-encoded ASCII `50.28.104.50`). Each connection completes TLS successfully (rustls warns but proceeds) then never sends a Handshake message. The 10-second pre-handshake timeout holds every connection as a live tokio future.

**Crash Mechanism:** 10 connections/sec × 10s hold = ~100 concurrent futures × ~200KB TLS state = ~20MB RAM consumed every 10 seconds — growing until the kernel OOM-killer fires (~12 minutes).

**Compound effect (three vectors firing simultaneously):**
1. SNI ghost flood consuming ~20MB/10s
2. PHASE3 outbound loop wasting 15 tokio tasks on banned IPs every 30s (see 14.9)
3. Coordinated disconnect storm: 7–10 nodes from `154.217.246.x` disconnecting simultaneously every ~60s, triggering reconnect storms

**Observed:** All nodes (Michigan, Arizona) crashing every ~12 minutes. Watchdog restarted each node 8–9 times per session.

**Fixes Applied:**
- ✅ **`timed.service` memory limits** — `MemoryMax=3G`, `MemoryHigh=2G`, `LimitNPROC=8192`. Hard ceiling prevents OOM from killing other system processes; systemd restarts if limit is breached
- ✅ **Per-/24 subnet accept rate limiter** — >20 connections/min from any single /24 prefix are dropped before TLS. Implemented as `DashMap<String, VecDeque<Instant>>` in the TCP accept loop, before `can_accept_inbound()`. Non-whitelisted IPs only — trusted nodes bypass the limit.
- ✅ **`record_tls_failure()` AI hook** — rate-limit rejections feed the AI attack detector; ≥5 from same IP in 60s → `BlockPeer`
- ✅ **Watchdog RPC timeout** — `mn-watchdog.sh` wraps all `time-cli` calls with `timeout "$RPC_TIMEOUT"` (default 8s), preventing 60s stalls when the daemon is dead. `FAIL_THRESHOLD` default changed 1→3 to avoid restart thrashing on transient RPC errors.

**Code References:**
- `src/network/server.rs` — `subnet_accept_rate: Arc<DashMap<String, VecDeque<Instant>>>` in `run()`; subnet prefix check with `MAX_SUBNET_CONNECTS_PER_MIN = 20`
- `src/ai/attack_detector.rs` — `record_tls_failure()`; `TlsFlood` attack type; `tls_failure_times` sliding-window field
- `scripts/mn-watchdog.sh` — `--rpc-timeout` flag; `FAIL_THRESHOLD` default
- `timed.service` — `MemoryMax`, `MemoryHigh`, `LimitNPROC`

---

### 14.9 ✅ FIXED — PHASE3 Reconnect Loop to Banned Peers
**Status:** **FIXED in v1.4.34** (commit `a028b52`)

**Attack:** The PHASE3 outbound connection loop (`client.rs`) iterates all registered masternodes and peers every 30 seconds. The `should_skip()` closure only checked the static config `blacklisted_peers` set — it did **not** check `res.ip_blacklist`, the live `Arc<RwLock<IPBlacklist>>` that holds subnet bans applied by the AI enforcement loop. As a result, the PHASE3 loop opened full TCP + TLS handshakes to all ~15 IPs on the banned `154.217.246.0/24` subnet on every 30-second cycle, consuming tokio tasks and TLS memory:

```
[PHASE3-MN] Connected to peer: 154.217.246.34:24000
[PHASE3-MN] REJECTING message from blacklisted peer 154.217.246.34: Subnet banned
```

During the ghost connection OOM this contributed ~15 extra concurrent futures every 30 seconds.

**Fix Applied:**
- ✅ Both PHASE3-MN and PHASE3-PEER loops check `ip_blacklist.write().await.is_blacklisted()` before `mark_connecting`. Banned IPs are skipped at zero cost — no socket opened, no TLS round-trip, no tokio task spawned.

**Code References:**
- `src/network/client.rs` — PHASE3-MN loop (~line 438); PHASE3-PEER loop (~line 519); `ip_blacklist.write().await.is_blacklisted()` check before `mark_connecting`

---

### 14.10 ✅ FIXED — IP Cycling / Collateral Migration Back-and-Forth
**Status:** **FIXED in v1.4.34** (commit `a028b52`)

**Attack:** Four attacker-controlled collateral outpoints were cycling between IP pairs on an exact 60-second cadence — matching the old `MIGRATION_COOLDOWN_SECS = 60`. Each cycle triggered UTXOManager stale-collateral unlocks, sled registry writes, and peer gossip re-broadcasts. With 4 outpoints cycling synchronously the registry received sustained write pressure and legitimate nodes flickered between active/inactive reward eligibility states.

**Observed IP pairs:**
| Outpoint prefix | Pair |
|-----------------|------|
| `50911bd...` | `154.217.246.34` ↔ `124.70.167.62` |
| `f52a81...`  | `154.217.246.111` ↔ `154.217.246.86` |
| `926b2f...`  | `133.18.180.117` ↔ `43.119.35.195` |
| `95f1b8...`  | `69.167.169.81` ↔ `47.82.236.153` |

**Fixes Applied:**
- ✅ **Cooldown raised 60s → 300s** — each cycling event now costs the attacker 5× longer; the 4-outpoint synchronized cycle now fires at most every 5 minutes instead of every minute
- ✅ **Back-and-forth detection (600s lockout)** — `collateral_migration_from` field tracks last-from IP per outpoint; incoming migration rejected with `InvalidCollateral` if it matches the previous-from IP within 600 seconds
- ✅ **Subnet disconnect detection** — coordinated simultaneous disconnects from the same /24 trigger `BlockPeer` for the specific offending IP

**Note on subnet-wide bans:** The synchronized disconnect detector emits `BlockPeer` for the specific IP, not `BanSubnet` for the whole /24. This is intentional — cloud providers like Alibaba Cloud host both attacker and legitimate nodes on the same prefix. Operators who are certain a subnet is wholly hostile can add explicit `bansubnet=x.x.x.0/24` entries to `time.conf`.

**Code References:**
- `src/masternode_registry.rs` — `MIGRATION_COOLDOWN_SECS = 300`; `collateral_migration_from: Arc<DashMap<String, String>>`; cycling detection block before `collateral_migration_times.insert()`
- `src/ai/attack_detector.rs` — `record_synchronized_disconnect()`; `subnet_disconnects` sliding-window field; `SynchronizedCycling` → `BlockPeer`
- `src/network/server.rs` — `record_synchronized_disconnect()` call in `handle_peer` cleanup (gated on `handshake_done`)

---

### 14.11 ✅ FIXED — Reconnection-Storm Tokio Thread Starvation
**Status:** **FIXED in v1.4.35** (commit `22e056a`, April 8, 2026)

**Attack / Failure Mode:** When 40+ masternodes disconnect and reconnect simultaneously (due to a mass eviction from the Free-tier subnet flood, attacker-triggered disconnect storm, or a network partition resolving), each inbound reconnection spawns a TLS I/O bridge task plus a message-processing loop task. During the resulting burst:

- Each arriving message acquires a write lock on the **shared** `Arc<RwLock<RateLimiter>>` (one lock for ALL inbound connections)
- Ping/pong bursts from 15+ peers at ~3 pings/sec each = 45+ write-lock acquisitions per second on one mutex
- Fork-resolution state-machine runs concurrent `GetBlocks` cycles against the same peers
- All of the above on a 4-worker tokio runtime (pinned for sled-on-VPS safety)

Result: tokio worker threads saturate → RPC JSON-RPC handler never gets scheduled → `masternodestatus` RPC times out in 3s → watchdog calls it "de-registration" and restarts the node → **restart every ~10 minutes**.

**Observed (April 8 watchdog log):**
```
16:20:24 🔁 De-registration detected after 2 consecutive checks — restarting timed (restart #12)
16:35:39 🔁 De-registration detected after 2 consecutive checks — restarting timed (restart #13)
16:46:00 🔁 De-registration detected after 2 consecutive checks — restarting timed (restart #14)
... (5 more restarts at ~10 min intervals)
```

**Root Cause (two parts):**
1. **Shared rate-limiter mutex** — `Arc<RwLock<RateLimiter>>` is shared across ALL inbound peer connections; write-lock contention under load directly starves the RPC server task
2. **Watchdog does not distinguish RPC busy from RPC dead** — any 3-second RPC timeout is treated as de-registration

**Fixes Applied:**
- ✅ **`fork_resolution_blocked_until` cooldown** (commit `92737ad`) — stops the deep-fetch busy-loop from restarting every 15s on finality lock; removes the biggest single contributor to tokio saturation
- ✅ **`MIN_PEERS_FINALITY_OVERRIDE` lowered 5 → 2** (commit `92737ad`) — node escapes minority fork within 60s instead of being permanently stuck
- ✅ **Watchdog activity-check before restart** (commit `f3e6229`) — checks `journalctl` for recent log activity before treating RPC timeout as de-registration (see section 14.12)
- ✅ **Per-connection rate limiter** (commit `22e056a`) — `handle_peer()` now creates a local `RateLimiter::new()` that shadows the shared parameter; each peer's rate checks are fully independent with zero cross-peer lock contention
- ✅ **Pre-channel message gate** (commit `22e056a`) — TLS and plaintext I/O bridge tasks count raw messages per second before forwarding to the processing channel; soft limit 200/s (silent drop), hard limit 500/s (error → `record_violation()` + `record_message_flood()` → disconnect)
- ✅ **Ping flood escalation** (commit `22e056a`) — `ping_excess_streak` counter escalates 3 consecutive rate-limit exceedances to `record_violation()` + `record_ping_flood()`; peer is disconnected on ban threshold
- ✅ **`PingFlood` / `MessageFlood` in `AttackDetector`** (commit `22e056a`) — new `AttackType` variants with sliding-window detection methods; feed the 30s enforcement loop → `IPBlacklist` auto-ban

**Code References:**
- `src/blockchain.rs` — `fork_resolution_blocked_until`, `MIN_PEERS_FINALITY_OVERRIDE`, `longer_chain_escape`
- `src/network/server.rs` — per-connection `RateLimiter::new()` shadow; pre-channel gate in TLS+plaintext bridge tasks; `ping_excess_streak` escalation
- `src/ai/attack_detector.rs` — `PingFlood`, `MessageFlood` variants; `record_ping_flood()`, `record_message_flood()`
- `scripts/mn-watchdog.sh` — `daemon_recently_active()` check before restart

---

### 14.12 ✅ FIXED — Watchdog False-Restart on RPC Timeout
**Status:** **FIXED in watchdog v1.1** (April 8, 2026)

**Attack / Failure Mode:** The masternode watchdog script (`mn-watchdog.sh`) treated any `masternodestatus` RPC timeout as "de-registration detected" and restarted `timed` after 2 consecutive failures (a ~6-second window at the 3-second RPC timeout default). A node legitimately busy processing fork resolution or a reconnection storm is alive and healthy, but its tokio RPC handler thread is temporarily starved — the daemon is not dead or de-registered.

**Consequence:** Nodes restarted every ~10 minutes even though they had caught up to the canonical chain and were operating normally. Each restart reset the masternode registration startup sequence, causing a ~60s window of ineligibility for block production rewards on every cycle.

**Attack amplification:** An attacker who can trigger a brief reconnection storm (by coordinating 40+ Free-tier nodes to disconnect/reconnect simultaneously — AV3/AV25) can induce continuous watchdog restarts without ever directly attacking consensus.

**Fix Applied:**
- ✅ **`daemon_recently_active()` function** — checks `journalctl -u timed` for log entries within the last `DAEMON_ACTIVE_SECS` seconds (default: 90s). If the daemon has logged recently, it is alive and busy — not dead or de-registered.
- ✅ **Separate busy-streak counter** — RPC timeouts while the daemon is logging increment `rpc_busy_streak`; restart is suppressed until `rpc_busy_streak >= RPC_BUSY_MAX` (default: 10, i.e. ~90s of continuous unresponsiveness with recent log activity). Silent failures still escalate normally via `fail_streak`.
- ✅ **Raised default `RPC_TIMEOUT`** from 3s → 8s — gives the tokio RPC handler more time to respond during load spikes
- ✅ **Raised default `FAIL_THRESHOLD`** from 2 → 3 — three consecutive confirmed failures before restart
- ✅ **Accurate log message** — distinguishes "node busy (RPC timeout but daemon logging)" from "de-registration detected (RPC returned not-active status)"

**Code References:**
- `scripts/mn-watchdog.sh` — `daemon_recently_active()`, `rpc_busy_streak`, `RPC_BUSY_MAX`, `DAEMON_ACTIVE_SECS`

---

## SUMMARY TABLE: ATTACK SURFACE ANALYSIS

| Attack Vector | Mitigation Status | Risk Level | Notes |
|---------------|-------------------|------------|-------|
| **67% Attack** | ✅ Strong | 🟢 Low | Requires 67% stake (economically prohibitive) |
| **Long-Range Attack** | ✅ Mitigated | 🟢 Low | Checkpoints prevent history rewrite |
| **Nothing-at-Stake** | ✅ N/A | 🟢 Low | BFT consensus prevents multi-voting |
| **Selfish Mining** | ✅ Mitigated | 🟢 Low | Deterministic slots, no mining advantage |
| **Stake Grinding** | ✅ Mitigated | 🟢 Low | VRF-based leader selection implemented |
| **Timestamp Attacks** | ✅ Mitigated | 🟢 Low | ±5s future tolerance, slot-time validation |
| **Eclipse (Consensus)** | ✅ Mitigated | 🟢 Low | Multi-peer verification, fork detection |
| **Sybil Attack** | ✅ Strong | 🟢 Low | Connection limits + stake requirements + /24 subnet auto-ban |
| **DDoS** | ✅ Strong | 🟢 Low | Comprehensive rate limiting |
| **Eclipse (Network)** | ✅ Mitigated | 🟢 Low | Diverse peer selection, masternode slots |
| **BGP Hijacking** | ✅ Mitigated | 🟢 Low | TLS enabled by default on P2P connections |
| **Message Replay** | ✅ Strong | 🟢 Low | Time-windowed Bloom filters |
| **Double-Spend** | ✅ Strong | 🟢 Low | Atomic UTXO locking |
| **TX Malleability** | ✅ N/A | 🟢 Low | Ed25519 prevents malleability |
| **Fee Sniping/RBF** | ✅ N/A | 🟢 Low | No RBF support, UTXO locking |
| **Dust Attacks** | ✅ Mitigated | 🟢 Low | 546 satoshi minimum + proportional fees |
| **Front-Running** | ⚠️ Limited | 🟡 Medium | Transparent mempool allows MEV |
| **Signature Forgery** | ✅ Impossible | 🟢 Low | Ed25519 cryptographically secure |
| **Invalid Block Consensus** | ✅ Fixed | 🟢 Low | Pre-vote validation (Jan 19, 2026) |
| **Block Withholding** | ✅ Mitigated | 🟢 Low | Deterministic slots, liveness timeout |
| **Collateral Hijack** | ✅ Fixed | 🟢 Low | V4 proof required; violations auto-ban attacker (Apr 2026) |
| **Sybil /24 Subnet Attack** | ✅ Fixed | 🟢 Low | Auto-banned on 5+ IPs same outpoint in 60s (Apr 2026) |
| **Pre-Handshake Prober** | ✅ Fixed | 🟢 Low | Immediate violation + AI ban (Apr 2026) |
| **GossipEvictionStorm** | ✅ Fixed | 🟢 Low | AI detection + timed ban, confirmed working live |
| **UTXO Lock Flood** | ✅ Fixed | 🟢 Low | 50-lock/TX cap per connection + AI auto-ban (Apr 2026) |
| **Oversized Frame (DoS)** | ✅ Fixed | 🟢 Low | Frame >8MB → disconnect + violation (Apr 2026) |
| **V4 Eviction Oscillation** | ⚠️ Partial | 🟡 Medium | Subnet ban helps; post-eviction re-registration delay recommended |
| **Double Block Rewards** | ✅ Fixed | 🟢 Low | Strict validation (Jan 19, 2026) |
| **Hash Collision** | ✅ Secure | 🟢 Low | SHA256 collision-resistant |
| **Quantum Computing** | ⚠️ Future Risk | 🟡 Medium | Industry-standard, 10-20 year horizon |
| **Cross-Chain Replay** | ✅ Mitigated | 🟢 Low | Chain-ID binding |
| **Governance Capture** | ⚠️ Partial | 🟡 Medium | Plutocracy risk (whale dominance) |
| **Bribery/Vote Buying** | ⚠️ Monitoring | 🟡 Medium | Hard to prevent technically |
| **Inflation** | ✅ Impossible | 🟢 Low | Strict supply enforcement |
| **Memory Exhaustion** | ✅ Mitigated | 🟢 Low | Caps on all data structures |
| **Deadlocks** | ✅ Mitigated | 🟢 Low | Rust type system prevents |
| **Integer Overflow** | ✅ Protected | 🟢 Low | Rust overflow checks |
| **AI Health Manipulation** | ✅ Monitored | 🟢 Low | Multi-factor scoring |
| **Dependency Vulnerabilities** | ⚠️ Requires Audit | 🟡 Medium | Need regular cargo audit |
| **RPC Public Exposure** | ✅ Fixed | 🟢 Low | Bound to 127.0.0.1 (v1.2.0) |
| **RPC Authentication** | ✅ Fixed | 🟢 Low | HTTP Basic Auth + rpcauth hashed credentials (v1.2.0) |
| **RPC Rate Limiting** | ✅ Fixed | 🟢 Low | Per-IP 100 req/s (v1.2.0) |
| **CORS Policy** | ✅ Fixed | 🟢 Low | Restricted to localhost (v1.2.0) |
| **Wallet Default Password** | ✅ Fixed | 🟢 Low | Auto-generated 32-char password (v1.2.0) |
| **Unsigned Vote Acceptance** | ✅ Fixed | 🟢 Low | Empty signatures rejected (v1.2.0) |
| **General Message Signing** | ⚠️ Not Enforced | 🟡 Medium | SignedMessage exists but unused for non-votes |
| **Reconnection Storm → Tokio Starvation** | ✅ Fixed | 🟢 Low | Per-connection rate-limiter, pre-channel gate, ping flood escalation (22e056a) |
| **Watchdog False-Restart via RPC Timeout** | ✅ Fixed | 🟢 Low | `daemon_recently_active()` check added; watchdog v1.1 (Apr 2026) |
| **Ping Flood (no escalation)** | ✅ Fixed | 🟢 Low | `ping_excess_streak` → `record_ping_flood()` → blacklist (22e056a) |
| **Pre-channel Message Flood** | ✅ Fixed | 🟢 Low | Soft 200/s + hard 500/s gate in TLS and plaintext I/O bridge tasks (22e056a) |

---

## APPENDIX: IMPLEMENTATION VERIFICATION LOG

**Verification Date:** January 23, 2026  
**Method:** Code inspection and grep analysis

### Verified Implementations

**1. Pre-vote Block Validation**
- **Location:** `src/network/message_handler.rs`
- **Method:** `validate_block_before_vote()`
- **Status:** ✅ Active and functioning
- **Evidence:** Validation occurs before TimeVote generation

**2. Block Reward Validation**
- **Location:** `src/blockchain.rs` lines 2312-2341
- **Method:** `validate_block_rewards()`
- **Features:**
  - Coinbase amount validation
  - Fee accumulation from previous block
  - Dual-ledger mechanism (coinbase + reward_distribution)
  - Total distributed amount range checks
- **Status:** ✅ Comprehensive implementation

**3. Rate Limiting**
- **Location:** `src/network/rate_limiter.rs`
- **Implementation:**
  - MAX_RATE_LIMIT_ENTRIES: 50,000 (memory protection)
  - Per-message type limits (TX: 50/sec, Votes: 100/sec, Blocks: 10/sec)
  - Emergency cleanup mechanisms
  - 10-second regular cleanup cycle
- **Status:** ✅ Mature production implementation

**4. UTXO Locking**
- **Location:** `src/utxo_manager.rs` lines 100-170
- **Features:**
  - Lock timeout: 600 seconds (10 minutes)
  - Collateral locking via DashMap
  - State machine: Locked → SpentFinalized → SpentPending
  - Prevents spending of collateral-locked UTXOs (line 156-158)
- **Status:** ✅ Robust implementation

**5. TLS Implementation**
- **Locations:**
  - `src/network/tls.rs` (TLS configuration)
  - `src/network/secure_transport.rs` (Combined TLS + signature layer)
- **Features:**
  - Rustls-based implementation
  - Self-signed certificates for P2P
  - Client and server configs
  - Message signing + encryption combined
- **Status:** ⚠️ Code complete but marked "TODO: Remove once integrated into server/client"
- **Action Required:** Integration into ConnectionManager

**6. VRF Leader Selection**
- **Location:** `src/tsdc.rs`
- **Method:** `select_leader_for_slot()`
- **Implementation:**
  - ECVRF (Elliptic Curve Verifiable Random Function)
  - ED25519 signing keys for VRF computation
  - Deterministic slot-based selection
  - VRF proof verification
- **Status:** ✅ Fully implemented

### Verification Summary

| Feature | Code Status | Integration Status | Priority |
|---------|-------------|-------------------|----------|
| Pre-vote validation | ✅ Complete | ✅ Integrated | N/A |
| Block reward validation | ✅ Complete | ✅ Integrated | N/A |
| Rate limiting | ✅ Complete | ✅ Integrated | N/A |
| UTXO locking | ✅ Complete | ✅ Integrated | N/A |
| VRF leader selection | ✅ Complete | ✅ Integrated | N/A |
| TLS/encryption | ✅ Complete | ⚠️ Pending | 🔴 High |

**Overall Code Quality:** 🟢 Excellent - All claimed features verified in codebase

---

## PRIORITY RECOMMENDATIONS

### 🔴 HIGH PRIORITY
1. **COMPLETED ✅:** Pre-vote block validation (Fixed Jan 19, 2026)
2. **COMPLETED ✅:** Block reward validation (Fixed Jan 19, 2026)
3. **COMPLETED ✅:** VRF for leader selection (Implemented Jan 2026)

### 🟡 MEDIUM PRIORITY
4. **COMPLETED ✅:** TLS Integration (enabled by default, commit `c69f159`)
   - TLS active on all P2P connections via `enable_tls = true` (config default)
   - Eliminates BGP hijacking and MITM attack vectors

5. **Implement Stake Slashing**
   - Penalize provable misbehavior (double signing, invalid blocks)
   - Deterrent against bribery/collusion
   - Estimated effort: 2-3 weeks
   - **Priority increased:** Should be next major security feature

6. **Regular Dependency Audits**
   - Run `cargo audit` before each release
   - Monitor RustSec advisories
   - Consider `cargo-deny` for automated policy enforcement
   - **Status:** Should be added to CI/CD pipeline

### 🟢 LOW PRIORITY (FUTURE ENHANCEMENTS)
6. **Post-Quantum Cryptography**
   - Add hybrid signatures (Ed25519 + PQC)
   - 10-20 year timeline before quantum threat
   - Monitor NIST PQC standardization

7. **Quadratic Voting for Governance**
   - Reduce whale voting power
   - More democratic governance
   - Requires economic analysis

8. **Private Mempool / Commit-Reveal**
   - Reduce MEV/front-running
   - Add complexity, tradeoffs needed
   - Research threshold encryption schemes

---

## CONCLUSION

TimeCoin demonstrates **strong security posture** against the vast majority of known cryptocurrency attacks. The hybrid Proof-of-Stake + BFT consensus model, combined with comprehensive network and transaction-layer protections, creates a resilient system.

**Key Strengths:**
- ✅ 67% BFT-safe finality threshold prevents consensus attacks
- ✅ VRF-based leader selection eliminates stake grinding
- ✅ Multi-layer network protections (rate limiting, anomaly detection, deduplication)
- ✅ Cryptographically secure transaction validation
- ✅ Recent security fixes (pre-vote validation, block reward validation)
- ✅ TLS implementation complete (awaiting integration)

**Implementation Progress Since v1.0:**
- ✅ VRF leader selection added
- ✅ TLS/secure transport layer implemented
- ⚠️ TLS integration pending (final step)

**Recommended Next Steps:**
1. **Immediate:** Complete TLS integration into network stack (3-5 days)
2. **Short-term:** Add cargo audit to CI/CD pipeline
3. **Medium-term:** Implement stake slashing for validator misbehavior
4. **Long-term:** Monitor post-quantum cryptography developments

**Overall Assessment:** 🟢 **PRODUCTION-READY** with one remaining integration task (TLS) for optimal security hardening.

---

## LIVE MAINNET ATTACK VECTORS (April 2026)

The following attack vectors were observed and exploited against the live mainnet network in April 2026.
All have been investigated, root-caused, and fixed. Fixes shipped in commits `89bd02d`–`2d842f6` plus the
inbound V4 context fix.

---

### AV-6 ✅ Pre-Handshake Deregistration Attack
**Status:** **FIXED** (commit `89bd02d`)

**Attack:** Adversary opens a TCP connection to a node and immediately closes it before sending the
handshake (`Version`/`Verack`). The disconnect event fires and `mark_inactive_on_disconnect()` is
called with the connecting IP. If that IP matches a registered masternode's address, the masternode
entry is marked inactive/removed.

**Root cause:** `server.rs` called `mark_inactive_on_disconnect` unconditionally on TCP close, without
checking whether the handshake had completed.

**Fix:** Added `handshake_done: bool` tracking per connection. `mark_inactive_on_disconnect` is now
gated: only called when `handshake_done = true`. Pre-handshake closes trigger a `WARN` but do not
touch the masternode registry.

**Evidence from logs:**
```
WARN ⚠️  47.82.240.104:34204 sent message before handshake - closing connection (not blacklisting)
INFO 🔌 Peer 47.82.240.104:34204 disconnected
```

---

### AV-7 ✅ Genesis Isolation via Timeout
**Status:** **FIXED** (commit `12e4fb1`)

**Attack:** Older nodes that don't implement `GetGenesisHash` time out when queried. The node marks
every peer that times out as "incompatible" with a 300-second re-check ban. On a large network with
many older nodes, all peers become simultaneously marked incompatible → `get_compatible_peers()`
returns empty → sync coordinator and fork detection stall completely. The node effectively isolates
itself from the network.

**Root cause:** `verify_genesis_compatibility()` in `peer_connection_registry.rs` treated timeout, I/O
errors, and unexpected response types identically to an explicit hash mismatch, all leading to
`mark_incompatible`.

**Fix:** Only an explicit hash mismatch (received a valid `GenesisHashResponse` with a different hash)
marks a peer as incompatible. Timeout, I/O error, or unrecognized response type now returns `true`
(assume compatible) with a debug log. Peers are only isolated when their genesis hash is provably wrong.

---

### AV-8 ✅ Free-Tier Collateral Squatting via Gossip Migration
**Status:** **FIXED** (commit `12e4fb1`)

**Attack:** A paid-tier masternode (Tier 1/2/3) is registered with a specific collateral outpoint.
An adversary registers as Free-tier with the same IP but different collateral. Then, the adversary
sends a gossip `MasternodeAnnouncementV3` with a new IP and the paid-tier node's collateral outpoint.
Free-tier nodes allow gossip IP migration without proof, so the paid collateral gets "migrated" to the
attacker's chosen IP, ejecting the legitimate owner.

**Root cause:** `register_internal()` in `masternode_registry.rs` allowed Free-tier migration to
overwrite any existing holder of an outpoint, including paid-tier holders.

**Fix:** Added a check: if the current holder of a collateral outpoint is a paid tier (Tier 1/2/3),
Free-tier gossip migration is blocked. The attacker receives a `🛡️` warning log and the migration is
rejected. Only an on-chain `MasternodeReg` transaction can claim a paid-tier collateral.

---

### AV-9 ✅ Startup Squatter Eviction Race
**Status:** **FIXED** (commits `73275c7`, `99e3718`)

**Attack:** The local node starts up and attempts to register its collateral. If an attacker already
squatted the outpoint (e.g., by gossip before the node restarted), `register_masternode()` returns
`CollateralAlreadyLocked`. The original code logged an error and exited the registration path,
leaving the local node unregistered. The attacker's squatter entry remained, and the dashboard showed
"Node is not configured as a masternode."

**Root cause:** `main.rs` treated `CollateralAlreadyLocked` as fatal, not as "evict squatter and
proceed."

**Fix:** On `CollateralAlreadyLocked`, the startup path now:
1. Calls `find_holder_of_outpoint()` to identify the squatter's IP.
2. Calls `unregister(squatter_ip)` to evict it.
3. Re-runs `register_masternode()` (succeeds now that lock is released).
4. Runs the full post-registration setup: `lock_local_collateral()`, reward address registration,
   `set_local_masternode()`, `mark_reachable()`.

This ensures the local node is fully operational even after a cold-start squatter attack.

---

### AV-10 ✅ V4 Eviction Storm (Per-Outpoint Denial of Service)
**Status:** **FIXED** (commit `2d842f6`)

**Attack:** Adversaries with a valid V4 collateral proof for a target outpoint repeatedly send
`MasternodeAnnouncementV4` messages, evicting the current holder. Because `unregister()` clears the
`collateral_anchor` sled key, each eviction resets the outpoint to claimable, allowing the next
eviction cycle immediately. With three coordinated attacker IPs, 435 V4 eviction events were observed
in 2 seconds against a single collateral outpoint (`926b2fb0:0`).

**Root cause (Path 1):** In `handle_masternode_announcement()`, the UTXOManager lock conflict path
set `can_evict = true` unconditionally for Tier 1 (V4 proof), without any rate limiting.

**Root cause (Path 2):** The registry-only eviction path (no UTXOManager lock present) had the same
unconditional `can_evict = true`.

**Fix:**
- Added `V4_EVICTION_COOLDOWN_SECS = 60` per-outpoint cooldown map (`v4_eviction_cooldown()`).
- Both eviction paths now check: if a V4 eviction occurred within the last 60 seconds for this
  outpoint, reject and log a storm warning (rate-limited to once per 30s per outpoint).
- Rate-limited storm warning logs prevent log flooding under sustained attack.

---

### AV-11 ✅ V4 Eviction of Local Node
**Status:** **FIXED** (commit `2d842f6`)

**Attack:** The local node's own collateral outpoint is targeted by an attacker who obtains (or
forges) a V4 collateral proof. The V4 eviction path removes the local node from the masternode
registry. `get_local_masternode()` then returns `None`, causing all RPC calls that depend on it to
return error `-4` ("Node is not configured as a masternode"). The dashboard shows the node as
deregistered while it is still running, causing operators to believe the node is broken and potentially
restart or reconfigure it (which the attacker hopes will disrupt service further).

**Root cause:** Both V4 eviction paths lacked a guard against evicting the node's own IP.

**Fix:** Both eviction paths now check `is_local_node` (comparing the eviction candidate IP against
`context.node_masternode_address`). If the candidate is the local node, eviction is blocked and a
`WARN` is logged:
```
WARN 🛡️ [Inbound] Blocking V4 eviction attempt against local node by <attacker_ip>
```

---

### AV-12 ✅ Inbound V4 Announcement Missing UTXO Manager
**Status:** **FIXED** (server.rs explicit V4 handler)

**Attack:** A paid-tier node sends a `MasternodeAnnouncementV4` message to a peer inbound connection.
Because `server.rs` had no explicit case for `NetworkMessage::MasternodeAnnouncementV4`, the message
fell through to the `_ =>` fallback handler, which creates a `MessageContext::minimal()` without
`utxo_manager`. The announcement handler requires `utxo_manager` to verify and lock the collateral;
without it, the message is silently dropped with:
```
WARN ⚠️ [Inbound] Cannot verify collateral for <ip> — no UTXO manager available
```
This prevented legitimate V4 masternodes from registering on nodes they connected to inbound, leaving
them invisible to part of the network and ineligible for rewards from those nodes.

**Root cause:** `server.rs` explicitly handled `MasternodeAnnouncementV2` and `V3` with UTXO manager
wiring, but omitted `V4`. The fallback `_ =>` handler never sets `utxo_manager`.

**Fix:** Added an explicit `NetworkMessage::MasternodeAnnouncementV4` match arm in `server.rs`
(inbound message loop) that mirrors the V2/V3 handlers: normalises the address from the TCP peer
address, sets `context.utxo_manager` and `context.peer_manager`, and delegates to the unified
`MessageHandler`.

---

**Document Version:** 1.3
**Last Updated:** April 7, 2026
**Changes from v1.2:**
- Updated 14.7 (V4 Eviction Oscillation) status from ⚠️ ONGOING to ✅ FIXED
- Added 14.8: Ghost Connection OOM / Distributed SNI Flood (AV22) — fixed in v1.4.34
- Added 14.9: PHASE3 Reconnect Loop to Banned Peers (AV23) — fixed in v1.4.34
- Added 14.10: IP Cycling / Collateral Migration Back-and-Forth (AV24) — fixed in v1.4.34

**Next Review:** Quarterly or after major protocol changes
