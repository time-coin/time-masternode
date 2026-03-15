# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 1.2.x   | :white_check_mark: |
| 1.1.x   | :white_check_mark: |
| < 1.1   | :x:                |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Instead, please report them via email to: **security@time-coin.io**

### What to Include

When reporting a vulnerability, please include:

- Type of vulnerability
- Full paths of source file(s) related to the vulnerability
- Location of the affected source code (tag/branch/commit)
- Step-by-step instructions to reproduce the issue
- Proof-of-concept or exploit code (if possible)
- Impact of the vulnerability
- Suggested fix (if you have one)

### Response Timeline

- **Initial Response**: Within 48 hours
- **Status Update**: Within 7 days
- **Fix Timeline**: Depends on severity
  - Critical: 1-7 days
  - High: 7-30 days
  - Medium: 30-90 days
  - Low: Next release cycle

## Security Measures

### Current Protections

TimeCoin implements several security measures:

1. **Cryptographic Security**
   - Ed25519 signatures for transactions and consensus votes (RFC 8032)
   - BLAKE3 hashing for blocks and Merkle trees
   - ECVRF (RFC 9381) for deterministic block producer sortition
   - AES-256-GCM wallet encryption with Argon2 key derivation
   - Secure random number generation (OsRng)

2. **RPC Security**
   - Binds to `127.0.0.1` by default (localhost only)
   - HTTP Basic Auth with auto-generated credentials
   - `rpcauth` hashed credentials (Bitcoin Core-compatible HMAC-SHA256)
   - Optional TLS encryption (`rpctls=1` in time.conf)
   - Per-IP rate limiting (100 req/s)
   - `.cookie` file for CLI tool authentication

3. **Network Security**
   - IP blacklisting for malicious peers
   - Per-peer, per-message-type rate limiting
   - Peer reputation scoring
   - Whitelist for trusted nodes
   - Message timestamp validation (5-minute window)

4. **Consensus Security**
   - TimeVote 51% stake-weighted finality
   - Ed25519 vote signature verification (unsigned votes rejected)
   - VRF-based deterministic block producer selection
   - Fork resolution via longest-chain rule
   - Chain reorganization depth limits

5. **Wallet Security**
   - Auto-generated random wallet password (32 chars)
   - Password stored in `.wallet_password` (owner-read-only permissions)
   - Legacy wallets auto-migrated to secure passwords on first load
   - AES-256-GCM encryption with Argon2 KDF

6. **Input Validation**
   - Transaction signature verification
   - Block header validation
   - Merkle tree verification
   - Amount overflow checks
   - Reward address network validation (prevents testnet/mainnet mismatch)

### Implemented (v1.2.x)

- **TLS encryption**: Both P2P and RPC support TLS (rustls, self-signed certs, `AcceptAnyCertVerifier` — message-level auth via Ed25519 signatures). Enable with `rpctls=1` in `time.conf`.
- **Encrypted transaction memos**: ECDH (X25519) + AES-256-GCM; only sender/recipient can decrypt

### Planned Security Enhancements

- Hardware wallet support
- Multi-signature transactions
- `walletpassphrase` / `encryptwallet` RPC commands
- Formal verification of critical code paths

## Security Best Practices

### For Node Operators

1. **System Security**
   - Keep operating system updated
   - Use firewall to restrict access (allow only P2P port)
   - Enable automatic security updates
   - Use strong SSH keys (if remote access)

2. **Node Configuration**
   - Never change `rpcbind` from `127.0.0.1` unless behind a VPN
   - Do not disable RPC authentication
   - Use `rpcauth` hashed credentials instead of plaintext where possible
   - Enable `rpctls=1` if RPC may traverse a network
   - Keep auto-generated `rpcuser`/`rpcpassword` in `time.conf`
   - Enable logging and monitor for anomalies
   - Protect `time.conf` and `.wallet_password` file permissions

3. **Network Security**
   - Use trusted peers when possible
   - Monitor for unusual peer behavior
   - Keep node software updated
   - Use VPN for sensitive deployments

4. **Key Management**
   - Never share private keys or wallet password files
   - Use hardware wallets for large amounts
   - Back up wallet file (`time-wallet.dat`) and password file securely
   - Use different keys for different purposes

### For Developers

1. **Code Security**
   - Follow secure coding practices
   - Run `cargo clippy` regularly
   - Run `scripts/security-check.sh` before releases
   - Review code for common vulnerabilities

2. **Testing**
   - Write tests for edge cases
   - Test error conditions
   - Security-focused integration tests in `tests/security_audit.rs`
   - Use sanitizers during development

3. **Dependencies**
   - Keep dependencies updated
   - Run `cargo audit` regularly
   - Use `deny.toml` policy for license and advisory checks
   - Minimize dependency count

## Known Security Considerations

### Current Limitations

1. **P2P TLS**: Implemented and available; not enforced as mandatory (nodes negotiate TLS on connect)
2. **Eclipse Attacks**: Partial mitigation through peer diversity and reputation
3. **Sybil Attacks**: Mitigated by masternode collateral requirements and whitelisting
4. **51% Attacks**: TimeVote finality requires 51% stake threshold

### Attack Vectors Being Monitored

- Long-range attacks
- Time manipulation attacks
- Network partition attacks
- Double-spend attempts
- VRF grinding attacks

## Threat Analysis

### UTXO Creation Attack Vectors

**Attack Scenario:** Can a malicious node add itself to the network and present
invalid UTXOs to "create coins"?

#### Protections Against Invalid Transactions

Transaction validation in `consensus.rs` enforces:

- **Input existence** — All inputs must reference existing, unspent UTXOs
- **Ownership proof** — Every input must carry a valid Ed25519 signature from the
  UTXO owner; signatures bind `txid + input_index + outputs_hash` to prevent
  tampering
- **No inflation** — Input sum must be ≥ output sum
- **Dust prevention** — Outputs below the dust threshold are rejected
- **Minimum fees** — Both absolute (`MIN_TX_FEE`) and proportional (0.1%) fees
  are required

A malicious node **cannot** spend UTXOs it does not own or reference
non-existent inputs — the signature check fails unconditionally.

#### Triple-Layer Block Reward Validation

Block reward manipulation is prevented by three sequential checks in
`blockchain.rs`:

1. **Fee calculation from previous block** — Each transaction's input sum is
   traced back through the UTXO set; fees are computed as `inputs − outputs`.
2. **Reward verification** — `expected_reward = BASE_REWARD (100 TIME) + fees`.
   The block is rejected if its claimed reward differs by even one satoshi.
3. **Distribution verification** — `total_distributed` across all reward outputs
   must equal `block_reward` (within rounding tolerance).

Example: an attacker claims 1,000 TIME reward when actual fees total 2 TIME.
Nodes calculate `100 + 2 = 102 TIME` expected and reject the block.

#### Additional Attack Vectors Ruled Out

| Vector | Status |
|--------|--------|
| Create UTXOs from nothing | ❌ Impossible — all UTXOs originate from coinbase or existing UTXOs |
| Steal coins | ❌ Impossible — Ed25519 signature covers the entire transaction |
| Inflate supply | ❌ Impossible — transaction validation enforces `input_sum ≥ output_sum` |
| Double-spend | ❌ Impossible — UTXOs are marked spent on first use; subsequent attempts fail |

### Vote-Before-Validate Gap (Open Vulnerability)

**Location:** `network/message_handler.rs` — `handle_tsdc_block_proposal`

**Severity:** HIGH — consensus-level issue that can cause network splits or DoS.

When a node receives a block proposal it currently:
1. Checks the block height ✅
2. Caches the block ✅
3. **Votes on it immediately** — before validating transactions or UTXOs ❌

A malicious masternode can propose a block containing invalid transactions
(non-existent inputs, forged signatures, inflated rewards). Honest nodes vote
before validation; if the block accumulates >50% votes it enters the
finalization path. `blockchain.add_block()` then rejects it, but the network
has already consumed voting bandwidth and may have diverged.

**Impact:**
- Network splits if subsets of nodes add or reject the invalid block
- Consensus failures when a finalized block cannot be appended to the chain
- DoS amplification — one invalid proposal triggers voting across all nodes

**Recommended fix:** add a `validate_block_before_vote()` call in
`handle_tsdc_block_proposal` that verifies block structure, reward amounts,
all transaction inputs, all signatures, and absence of intra-block
double-spends **before** casting a vote. See `docs/SECURITY_ANALYSIS.md` (now
merged here) for a code sketch.

---

## Vulnerability Disclosure

We follow responsible disclosure practices:

1. Report received and acknowledged
2. Vulnerability confirmed and assessed
3. Fix developed and tested
4. Security advisory prepared
5. Fix released
6. Public disclosure (after patch deployment)

## Security Audits

- Internal code reviews: Ongoing
- Comprehensive security analysis: See `docs/COMPREHENSIVE_SECURITY_AUDIT.md`
- Automated scanning: `scripts/security-check.sh` (cargo-audit + cargo-deny + clippy)
- Bug bounty program: Under consideration

## Contact

- Security Email: security@time-coin.io
- General Contact: info@time-coin.io
- GitHub: https://github.com/time-coin/time-masternode

## Acknowledgments

We appreciate the security research community and will acknowledge researchers who responsibly disclose vulnerabilities (with their permission).

---

*This security policy is subject to updates. Last updated: March 2026*
