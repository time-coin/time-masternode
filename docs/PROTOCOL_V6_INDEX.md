# TIME Coin Protocol V6 – Documentation Index

**Status:** ✅ Implementation-Ready  
**Last Updated:** December 23, 2025

---

## Document Map

### 📘 Primary Specification
**File:** `TIMECOIN_PROTOCOL_V6.md` (32 KB, 807 lines)  
**Purpose:** Complete normative specification  
**Audience:** Protocol engineers, security reviewers, reference implementations

**Structure:**
- §1–§15: Original architecture (stable)
- §16–§27: **NEW** – Implementation guidance (concrete algorithms, formats, procedures)

**Key additions:**
- §16: Cryptographic bindings (BLAKE3, ECVRF, Ed25519, bech32m)
- §17: Transaction and staking UTXO details (OP_STAKE semantics)
- §18: Network transport (QUIC, bincode, framing)
- §19: Genesis and bootstrap procedure
- §20: Clock synchronization (NTP, ±10s tolerance)
- §21: Light client / SPV support (optional)
- §22: Error recovery and edge cases
- §23: Address format and wallet integration
- §24: Mempool management
- §25: Economic model (fair launch, logarithmic rewards)
- §26: Implementation checklist
- §27: Test vectors framework

---

### 🛠️ Implementation Guide
**File:** `IMPLEMENTATION_ADDENDUM.md` (10.2 KB)  
**Purpose:** Rationale for design decisions, development schedule, testing strategy  
**Audience:** Developers, architects, QA

**Sections:**
- Summary of changes to V6.md
- Critical implementation decisions with rationale
- 5-phase development schedule (12 weeks baseline)
- Testing strategy (unit, integration, testnet)
- Operational readiness checklist
- Open community questions

**Use:** Reference alongside V6.md during implementation.

---

### 📋 Quick Reference
**File:** `QUICK_REFERENCE.md` (5.8 KB)  
**Purpose:** One-page lookup for all parameters, formats, algorithms  
**Audience:** Developers, operators, auditors (quick validation)

**Contents:**
- Cryptography stack (BLAKE3, Ed25519, ECVRF, bech32m)
- Transaction serialization format
- Staking script (OP_STAKE)
- Network parameters (QUIC, bincode, peer limits)
- Consensus parameters (k=20, α=14, β_local=20)
- Masternode tiers and weights
- Reward formula
- RPC API signature
- Bootstrap procedure
- Validation checklist

**Use:** Print and tape to monitor for quick reference during coding.

---

### 📊 Update Summary
**File:** `V6_UPDATE_SUMMARY.md` (9.5 KB)  
**Purpose:** High-level summary of all changes, mapping analysis → implementation  
**Audience:** Project managers, protocol leads, community

**Contents:**
- Overview: 348 → 807 lines (+132%)
- 12 new normative sections table
- Key decisions finalized (with impact)
- Analysis recommendations → implementation tracking
- Breaking down "⚠️ Underspecified" issues
- Missing components → addressed
- Validation checklist (all 8 issues closed)

**Use:** Executive summary for stakeholders.

---

## Reading Paths

### 👨‍💻 Developer (Implementing the Protocol)
1. Start: `QUICK_REFERENCE.md` (5 min)
   - Get familiar with parameters and formats
2. Main: `TIMECOIN_PROTOCOL_V6.md` §16–§27 (45 min)
   - Understand cryptography, network, bootstrap
3. Details: `IMPLEMENTATION_ADDENDUM.md` (30 min)
   - Follow 5-phase schedule, testing strategy
4. Reference: `QUICK_REFERENCE.md` (ongoing)
   - Lookup format details, RPC signatures

**Time investment:** ~90 minutes for complete understanding

---

### 🔐 Security Auditor
1. Start: `V6_UPDATE_SUMMARY.md` (10 min)
   - High-level overview of changes
2. Main: `TIMECOIN_PROTOCOL_V6.md` §22 (§22.1, §22.2) (30 min)
   - Error recovery, conflict resolution
3. Deep dive: `TIMECOIN_PROTOCOL_V6.md` §16, §8 (45 min)
   - Cryptographic bindings, VFP safety
4. Test framework: `TIMECOIN_PROTOCOL_V6.md` §27 (20 min)
   - Validation vectors

**Audit focus areas:**
- Cryptographic bindings (§16)
- VFP conflict detection (§8.7, §22.1)
- Network partition recovery (§22.2)
- Staking script (§17.2)

**Time investment:** ~2–3 hours per focus area

---

### 👥 Community / Governance
1. Start: `V6_UPDATE_SUMMARY.md` (10 min)
   - What changed and why
2. Main: `IMPLEMENTATION_ADDENDUM.md` "Open Questions" section (5 min)
   - Community decisions needed
3. Reference: `QUICK_REFERENCE.md` (5 min)
   - Parameter overview

**Decisions needed:**
- Pre-mine amount (if any)
- Reward cap desired (hard cap vs logarithmic)
- Block size limit (2 MB sufficient?)
- Fee market mechanism (dynamic vs median)

**Time investment:** ~20 minutes to form opinion

---

### 📚 Historical / Reference
**Files:**
- `TIMECOIN_PROTOCOL_V5.md` – Previous version (for comparison)
- `TIMECOIN_PROTOCOL.md` – Original specification (archive)

---

## Key Changes from Analysis

| Issue | Resolution | §Location |
|-------|-----------|-----------|
| Crypto bindings undefined | Pinned BLAKE3, ECVRF, bech32m | §16 |
| Staking script missing | Defined OP_STAKE semantics, unlock, maturation | §17.2 |
| TX format unspecified | Canonical serialization with field order | §16.3, §17 |
| Network transport missing | QUIC primary, TCP fallback, bincode | §18 |
| Genesis procedure absent | Bootstrap sequence, initial_avs | §19 |
| Clock sync undefined | NTP required, ±10s max drift, grace period | §20 |
| Light client missing | Block headers, Merkle proofs | §21 |
| Error handling incomplete | Conflicting VFP detection, network partition recovery | §22 |
| Address format missing | bech32m (BIP 350) | §23 |
| Mempool vague | 300 MB max, eviction policy, 72h expiry | §24 |
| Economics incomplete | Fair launch, R = 100 * (1 + ln(\|AVS\|)) | §25 |

---

## Document Maintenance

### Version Control
- All documents in `docs/` are version-controlled
- Changes tracked via git commits
- Pull requests required for modifications

### Update Procedure
1. Identify needed change (protocol, implementation, clarification)
2. Create branch: `docs/protocol-v6-<topic>`
3. Update relevant sections (mark changes with [v6.1] tags if needed)
4. Update table of contents and cross-references
5. Pull request → review → merge

### Stability Policy
- **§1–§15 (Original):** Freeze after v6.0 launch; major changes require v7.0
- **§16–§27 (New):** May be refined in v6.1, v6.2 (backward compatible)
- **Addendum:** Living document; updates don't require version bump

---

## References & Standards

| Spec | Role | Citation |
|------|------|----------|
| RFC 9381 | ECVRF construction | ECVRF-Edwards25519-SHA512-TAI |
| RFC 9000 | Transport protocol | QUIC v1 |
| BIP 350 | Address encoding | bech32m |
| Avalanche | Consensus | https://arxiv.org/abs/1906.08936 |
| BLAKE3 | Hash function | https://blake3.io |
| Noise Protocol | Transport encryption (fallback) | https://noiseprotocol.org |

---

## Implementation Status

### Phase 1: Core Infrastructure
**Goal:** Cryptography, serialization, data structures  
**Duration:** Weeks 1–2  
**Deliverable:** Passing test vectors for all crypto operations

**Checklist:**
- [ ] BLAKE3 hashing
- [ ] Ed25519 signing/verification
- [ ] ECVRF (RFC 9381)
- [ ] bech32m address encoding
- [ ] Canonical TX serialization
- [ ] UTXO structures
- [ ] Test vectors: §27

---

### Phase 2: Consensus Layer
**Goal:** Snowball, VFP, AVS membership, TSDC  
**Duration:** Weeks 3–5  
**Deliverable:** Consensus integration tests (3+ nodes)

**Checklist:**
- [ ] Avalanche Snowball state machine
- [ ] VFP generation and validation
- [ ] Heartbeat + witness attestation
- [ ] TSDC block production
- [ ] Conflict detection (§22.1)

---

### Phase 3: Network Layer
**Goal:** P2P transport, peer discovery, bootstrap  
**Duration:** Weeks 6–8  
**Deliverable:** P2P tests (10+ nodes)

**Checklist:**
- [ ] QUIC/TCP transport
- [ ] bincode serialization
- [ ] Message handlers (§11)
- [ ] Peer discovery (§18.4)
- [ ] Bootstrap nodes

---

### Phase 4: Storage and Archival
**Goal:** Persistent state, block archive, mempool  
**Duration:** Weeks 9–10  
**Deliverable:** Block production integration test

**Checklist:**
- [ ] UTXO database
- [ ] Block archive
- [ ] AVS snapshots (7-day retention)
- [ ] Mempool (§24)
- [ ] Eviction policy

---

### Phase 5: Client APIs and Testnet
**Goal:** RPC, wallet integration, public testnet  
**Duration:** Weeks 11–12  
**Deliverable:** Testnet live

**Checklist:**
- [ ] JSON-RPC 2.0 API (§23)
- [ ] Wallet integration
- [ ] Block explorer
- [ ] Testnet bootstrap nodes
- [ ] Faucet for testnet TIME

---

## Pre-Mainnet Validation

**14-item checklist** (§26 in V6.md):
- [ ] Crypto test vectors validated externally
- [ ] TX format tested (serialization round-trip)
- [ ] Staking script deployed and functional
- [ ] Network transport live (QUIC working)
- [ ] Peer discovery automatic
- [ ] Genesis block format and init procedure
- [ ] Clock sync verified (NTP on all nodes)
- [ ] Mempool eviction functional
- [ ] Conflicting VFP detection and logging
- [ ] Network partition recovery tested
- [ ] Address format and RPC API standardized
- [ ] Reward calculation verified
- [ ] Block size/entry limits enforced
- [ ] Test vectors created and passing

---

## Contact & Questions

### Protocol Questions
- File issue: `https://github.com/timecoin-protocol/timecoin/issues`
- Tag: `protocol`, `v6`

### Implementation Questions
- Reference: `IMPLEMENTATION_ADDENDUM.md`
- Tag: `implementation`, `phase-N`

### Community Feedback
- Use discussion forum (to be established)
- Open questions (see `IMPLEMENTATION_ADDENDUM.md`)

---

## Summary

**TIME Coin Protocol V6 is now implementation-ready.**

- ✅ All gaps from analysis addressed
- ✅ Concrete algorithms, formats, parameters specified
- ✅ 5-phase development schedule provided
- ✅ Test framework and checklist defined
- ✅ Community questions surfaced

**Start building with §16–§27 of TIMECOIN_PROTOCOL_V6.md and IMPLEMENTATION_ADDENDUM.md.**

---
