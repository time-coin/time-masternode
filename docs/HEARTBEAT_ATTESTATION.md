# Heartbeat Attestation System

## Overview

TimeCoin implements a **peer-verified heartbeat attestation system** to prevent uptime fraud and ensure trustless masternode reputation. This document explains how it works and why it's secure.

## The Problem

In traditional masternode systems, nodes self-report their uptime. This creates vulnerabilities:

1. **Sybil Attacks**: Attacker spins up 100 fake nodes claiming months of uptime
2. **Timestamp Manipulation**: Nodes can fake heartbeat timestamps
3. **Collusion**: Small groups can vouch for each other without real network participation
4. **No Accountability**: Claims can't be independently verified

## Our Solution: Cryptographic Peer Attestation

### Core Components

#### 1. Signed Heartbeats

Every 60 seconds, masternodes broadcast a **cryptographically signed heartbeat**:

```rust
pub struct SignedHeartbeat {
    pub masternode_address: String,
    pub sequence_number: u64,        // Monotonically increasing
    pub timestamp: i64,               // Unix timestamp
    pub masternode_pubkey: VerifyingKey,
    pub signature: Signature,         // Ed25519 signature
}
```

**Security Properties:**
- **Non-forgeable**: Signed with Ed25519, quantum-resistant signature scheme
- **Ordered**: Sequence numbers prevent replay attacks
- **Timestamped**: Real-time proof (within 3-minute validity window)
- **Self-verifying**: Anyone can verify the signature matches the public key

#### 2. Witness Attestations

When a masternode receives a heartbeat, it creates an **attestation**:

```rust
pub struct WitnessAttestation {
    pub heartbeat_hash: [u8; 32],     // Hash of the heartbeat being attested
    pub witness_address: String,
    pub witness_pubkey: VerifyingKey,
    pub witness_timestamp: i64,       // When witness saw the heartbeat
    pub signature: Signature,         // Witness's signature
}
```

**Security Properties:**
- **Independent Verification**: Each witness independently verifies and signs
- **Timestamped**: Proves when the witness observed the heartbeat
- **Traceable**: Witness identity is cryptographically bound to attestation
- **Publicly Auditable**: All attestations are broadcast to the network

#### 3. Quorum Requirement

A heartbeat is **verified** only when it has:
- ✅ Valid masternode signature
- ✅ At least **3 independent witness attestations**
- ✅ All attestations have valid signatures
- ✅ Timestamp within validity window (3 minutes)

## Attack Resistance

### Sybil Attack Prevention

**Attack:** Spin up 100 fake nodes claiming historical uptime

**Defense:**
1. New nodes start with sequence number 1 (no history)
2. Uptime = number of verified heartbeats (requires 3+ witnesses per heartbeat)
3. Cannot fake witness signatures (cryptographically impossible)
4. Building reputation takes real time (≥60 seconds per verified heartbeat)

**Result:** An attacker would need to control ≥25% of the network to provide fake attestations, and even then, honest nodes would detect the fraud.

### Timestamp Manipulation

**Attack:** Claim heartbeats from the past or future

**Defense:**
1. Each heartbeat has a 3-minute validity window
2. Witnesses timestamp when *they* saw the heartbeat
3. Sequence numbers must be monotonically increasing
4. Old sequence numbers are rejected

**Result:** Cannot fake historical uptime. Time is enforced by consensus.

### Collusion Attack

**Attack:** 5 masternodes collude to attest each other's fake heartbeats

**Defense:**
1. Need ≥3 witnesses (makes collusion expensive)
2. Witness selection is pseudo-random (can't predict who will attest)
3. All attestations are public (fraud is detectable)
4. Higher-tier masternodes get more weight but still need attestations

**Result:** Collusion requires controlling significant network percentage and is easily detected.

### Replay Attack

**Attack:** Rebroadcast old heartbeats to inflate uptime

**Defense:**
1. Sequence numbers must be strictly increasing
2. System tracks latest verified sequence per masternode
3. Duplicate or old sequences are rejected

**Result:** Cannot reuse old heartbeats.

## Implementation Details

### Heartbeat Lifecycle

```
1. Masternode creates signed heartbeat (seq N)
   └─ Signs with Ed25519 private key
   
2. Broadcasts to network
   └─ NetworkMessage::HeartbeatBroadcast
   
3. Peers receive and validate
   ├─ Verify signature
   ├─ Check timestamp (< 3min old)
   └─ Check sequence > last verified
   
4. Peers create attestations
   └─ Sign heartbeat hash with their keys
   
5. Peers broadcast attestations
   └─ NetworkMessage::HeartbeatAttestation
   
6. System collects attestations
   └─ Once 3+ unique witnesses → VERIFIED
   
7. Verified count increments
   └─ Masternode's verified_uptime += 1
```

### Data Storage

Heartbeats are stored in-memory with a rolling window:

- **Recent History**: Last 1000 heartbeats (configurable)
- **Verified Sequences**: Persistent mapping of `address → latest_verified_seq`
- **Witness Counts**: Persistent mapping of `address → total_verified_heartbeats`

Old heartbeats are pruned but the **verified count persists forever**.

### Network Protocol

Two new message types:

1. **HeartbeatBroadcast**: Masternode announces it's online
2. **HeartbeatAttestation**: Peer confirms they witnessed the heartbeat

Both messages are gossiped across the P2P network like transactions.

## Future Enhancements

### Phase 1: Basic Attestation ✅ (Current)
- Ed25519 signatures
- 3-witness quorum
- Sequence number enforcement

### Phase 2: Temporal Stake Weight (Planned)
- Voting power proportional to verified uptime
- Linear decay for missed heartbeats
- Minimum uptime thresholds per tier

### Phase 3: Slashing (Planned)
- Masternodes that attest fake heartbeats lose collateral
- Cryptographic fraud proofs
- Automated penalty system

### Phase 4: VDF Integration (Planned)
- Combine attestations with Verifiable Delay Functions
- Heartbeats include VDF proof of computation time
- Makes time claims hardware-verifiable

## Cryptographic Primitives

- **Signature Scheme**: Ed25519 (Curve25519)
  - Fast: ~60k signatures/sec verification
  - Secure: 128-bit security level
  - Small: 64-byte signatures, 32-byte keys

- **Hash Function**: SHA-256
  - Standard, widely audited
  - Used for heartbeat fingerprinting

- **Time Source**: NTP + Consensus Time
  - System syncs with NTP servers
  - Node shuts down if clock drift > 2 minutes
  - Prevents time-based attacks

## Testing

Run the test suite:

```bash
cargo test heartbeat_attestation
```

Key tests:
- `test_heartbeat_creation_and_verification`: Signature validation
- `test_witness_attestation`: Attestation signing
- `test_attestation_system`: Full quorum verification
- `test_sequence_validation`: Replay attack prevention

## API

### Create Heartbeat (Masternode)

```rust
let heartbeat = attestation_system.create_heartbeat().await?;
network.broadcast(NetworkMessage::HeartbeatBroadcast(heartbeat)).await;
```

### Receive Heartbeat (Peer)

```rust
attestation_system.receive_heartbeat(heartbeat).await?;
// Automatically creates attestation if we're a masternode
```

### Add Attestation

```rust
attestation_system.add_attestation(attestation).await?;
```

### Query Stats

```rust
let stats = attestation_system.get_stats().await;
println!("Verified heartbeats: {}", stats.verified_heartbeats);

let uptime = attestation_system.get_verified_heartbeats("node_address").await;
println!("Node uptime: {} verified heartbeats", uptime);
```

## Configuration

Constants in `heartbeat_attestation.rs`:

```rust
const MIN_WITNESS_ATTESTATIONS: usize = 3;      // Quorum size
const MAX_HEARTBEAT_HISTORY: usize = 1000;      // Memory limit
const HEARTBEAT_VALIDITY_WINDOW: i64 = 180;     // 3 minutes
```

## Comparison to Other Systems

| System | Proof Mechanism | Attack Resistance |
|--------|----------------|-------------------|
| Bitcoin | Proof of Work | ⭐⭐⭐⭐⭐ (51% attack) |
| Ethereum | Proof of Stake | ⭐⭐⭐⭐ (33% attack) |
| Dash Masternodes | Self-reported uptime | ⭐⭐ (Trust-based) |
| **TimeCoin** | **Peer-attested time** | **⭐⭐⭐⭐** (25%+ collusion) |

## References

- Ed25519 Signature Scheme: https://ed25519.cr.yp.to/
- BFT Consensus: Castro & Liskov, OSDI 1999
- Verifiable Delay Functions: Boneh et al., CRYPTO 2018
- TIME Coin Whitepaper: (link when published)

## Questions?

See `src/heartbeat_attestation.rs` for implementation details.

For issues, open a GitHub issue with the `[attestation]` tag.
