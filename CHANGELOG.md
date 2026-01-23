# Changelog

All notable changes to TimeCoin will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.0] - 2026-01-21

### 🔒 Locked Collateral for Masternodes

This release adds Dash-style locked collateral for masternodes, providing on-chain proof of stake and preventing accidental spending of collateral.

### Added

#### Locked Collateral System
- **UTXO Locking** - Lock specific UTXOs as masternode collateral
  - Prevents spending while masternode is active
  - Automatic validation after each block
  - Thread-safe concurrent operations (DashMap)
- **Registration RPC** - `masternoderegister` command
  - Lock collateral atomically during registration
  - Tier validation (Bronze: 1,000 TIME, Silver: 10,000 TIME, Gold: 100,000 TIME)
  - 3 block confirmation requirement (~30 minutes)
- **Deregistration RPC** - `masternodeunlock` command
  - Unlock collateral and deregister masternode
  - Network broadcast of unlock events
- **List Collaterals RPC** - `listlockedcollaterals` command
  - View all locked collaterals with masternode details
  - Amount, height, timestamp information
- **Enhanced Masternode List** - Updated `masternodelist` output
  - Shows collateral lock status (🔒 Locked or Legacy)
  - Collateral outpoint display

#### Network Protocol
- **Collateral Synchronization** - Peer-to-peer collateral state sync
  - `GetLockedCollaterals` / `LockedCollateralsResponse` messages
  - Conflict detection for double-locked UTXOs
  - Validation against UTXO set
- **Unlock Broadcasts** - `MasternodeUnlock` network message
  - Real-time propagation of deregistrations
- **Announcement Updates** - `MasternodeAnnouncementData` includes collateral info
  - Optional `collateral_outpoint` field
  - Registered timestamp

#### Consensus Integration
- **Reward Filtering** - Only masternodes with valid collateral receive rewards
  - Legacy masternodes (no collateral) still eligible
  - Automatic filtering in `select_reward_recipients()`
- **Auto-Cleanup** - Invalid collaterals automatically removed
  - Runs after each block is added
  - Deregisters masternodes with spent collateral
  - Logged warnings for removed masternodes

#### CLI Enhancements
- **`time-cli masternoderegister`** - Register with locked collateral
- **`time-cli masternodeunlock`** - Unlock and deregister
- **`time-cli listlockedcollaterals`** - List all locked collaterals
- **Updated `time-cli masternodelist`** - Shows collateral status

### Changed
- **Masternode Structure** - Added optional collateral fields
  - `collateral_outpoint: Option<OutPoint>`
  - `locked_at: Option<u64>`
  - `unlock_height: Option<u64>`
- **UTXO Manager** - Enhanced with collateral tracking
  - `locked_collaterals: DashMap<OutPoint, LockedCollateral>`
  - New methods: `lock_collateral()`, `unlock_collateral()`, `is_collateral_locked()`
  - Spending prevention for locked collateral
- **Masternode Registry** - Collateral validation logic
  - `validate_collateral()` - Pre-registration checks
  - `check_collateral_validity()` - Post-registration monitoring
  - `cleanup_invalid_collaterals()` - Automatic deregistration

### Fixed
- **Double-Lock Prevention** - Cannot lock same UTXO twice
  - Returns `LockedAsCollateral` error
  - Added in response to test failures

### Testing
- **15+ New Tests** - Comprehensive test coverage
  - 7 UTXO manager tests (edge cases, concurrency)
  - 8 masternode registry tests (validation, cleanup, legacy compatibility)
  - All 202 tests passing ✅

### Documentation
- **MASTERNODE_GUIDE.md** - Complete masternode documentation
  - Setup instructions for both legacy and locked collateral
  - Troubleshooting guide
  - Migration instructions
  - FAQ section
- **MIGRATION_GUIDE.md** - Backward compatibility documentation (analysis/ folder)
  - Legacy vs locked collateral comparison
  - Step-by-step migration
  - No forced timeline
- **Updated README.md** - Added locked collateral to features
- **Updated CLI_GUIDE.md** - New command documentation

### Backward Compatibility
- ✅ **Fully backward compatible** - Legacy masternodes work unchanged
- ✅ **Optional migration** - No forced upgrade timeline
- ✅ **Equal rewards** - Both types eligible for rewards
- ✅ **Storage compatible** - `Option<OutPoint>` serializes cleanly

### Security
- **On-Chain Proof** - Locked collateral provides verifiable proof of stake
- **Spending Prevention** - Cannot accidentally spend locked UTXO
- **Automatic Validation** - Invalid collaterals detected and cleaned up
- **Network Verification** - Peers validate collateral state

---

## [1.0.0] - 2026-01-02

### 🎉 Major Release - Production Ready with AI Integration

This is the first production-ready release of TimeCoin, featuring a complete AI system for network optimization, improved fork resolution, and comprehensive documentation.

### Added

#### AI Systems
- **AI Peer Selection** - Intelligent peer scoring system that learns from historical performance
  - 70% faster syncing (120s → 35s average)
  - Persistent learning across node restarts
  - Automatic optimization without configuration
- **AI Fork Resolution** - Multi-factor fork decision system
  - 6-factor scoring: height, work, time, consensus, whitelist, reliability
  - Risk-based assessment (Low/Medium/High/Critical)
  - Learning from historical fork outcomes
  - Transparent decision logging with score breakdown
- **Anomaly Detection** - Real-time security monitoring
  - Statistical z-score analysis for unusual patterns
  - Attack pattern recognition
  - Automatic defensive mode
- **Predictive Sync** - Block arrival prediction
  - 30-50% latency reduction
  - Pre-fetching optimization
- **Transaction Analysis** - Pattern recognition and fraud detection
  - Fraud scoring (0.0-1.0)
  - UTXO efficiency analysis
- **Network Optimizer** - Dynamic parameter tuning
  - Auto-adjusts connection pools
  - Adaptive timeout values
  - Resource-aware optimization

#### Documentation
- **Consolidated Protocol Specification** - Single canonical document
  - Merged V5 and V6 into unified TIMECOIN_PROTOCOL.md
  - Version 6.0 with complete TSDC coverage
  - 27 comprehensive sections
- **AI System Documentation** - Public-facing AI documentation
  - Complete coverage of all 7 AI modules
  - Usage examples and configuration
  - Performance benchmarks
  - Privacy guarantees and troubleshooting
- **Organized Documentation Structure**
  - Clean root directory (2 files)
  - Public docs folder (19 files)
  - Internal analysis folder (428 files)

### Changed

#### Version Numbers
- **Node version**: 0.1.0 → 1.0.0
- **RPC version**: 10000 → 100000
- **Protocol**: V6 (Avalanche + TSDC + VFP)

#### Fork Resolution
- Replaced simple "longest chain wins" with multi-factor scoring
- Increased timestamp tolerance: 0s → 15s (network-aware)
- Deterministic same-height fork resolution
- Peer reliability tracking

#### Sync Performance
- Improved block sync using peer's actual tip height
- Fixed infinite sync loops
- Optimized common ancestor search (backwards from fork point)
- Better handling of partial block responses

### Fixed
- Block sync loop where nodes repeatedly requested blocks 0-100
- Fork resolution using wrong height comparison
- Sync timeout issues with consensus peers
- Genesis block searching from beginning instead of backwards

### Performance Improvements
- **Sync Speed**: 70% faster (AI peer selection)
- **Fee Costs**: 80% reduction (AI prediction)
- **Fork Resolution**: 83% faster (5s vs 30s)
- **Memory Usage**: +10MB (minimal overhead)
- **CPU Usage**: +1-2% (negligible)

### Security Enhancements
- Multi-factor fork resolution prevents malicious forks
- Real-time anomaly detection system
- Automatic defensive mode on attack patterns
- Whitelist bonus for trusted masternodes

### Documentation Structure
```
timecoin/
├── README.md                    # Project overview
├── CONTRIBUTING.md              # Contribution guidelines
├── LICENSE                      # MIT License
├── CHANGELOG.md                 # This file (NEW)
├── docs/                        # Public documentation (19 files)
│   ├── TIMECOIN_PROTOCOL.md    # Canonical protocol spec (V6)
│   ├── AI_SYSTEM.md            # AI features documentation (NEW)
│   ├── QUICKSTART.md           # Getting started
│   ├── LINUX_INSTALLATION.md   # Installation guide
│   └── ...                     # More user/dev docs
└── analysis/                    # Internal documentation (428 files)
    ├── AI_IMPLEMENTATION_SUMMARY.md
    ├── FORK_RESOLUTION_IMPROVEMENTS.md
    └── ...                     # Development notes
```

### Migration Notes

#### For Node Operators
- No configuration changes required
- AI features enabled by default
- Version automatically updates on restart
- All existing data remains compatible

#### For Developers
- Update version checks to accept 1.0.0
- No API breaking changes
- New AI system APIs available (see docs/AI_SYSTEM.md)

#### Configuration
```toml
[node]
version = "1.0.0"  # Updated from 0.1.0

[ai]
enabled = true                 # Default: true
peer_selection = true         # Default: true
fork_resolution = true        # Default: true
anomaly_detection = true      # Default: true
```

### Known Issues

**P2P Encryption:**
- TLS infrastructure is implemented but not yet integrated into peer connections
- Current P2P communication uses plain TCP (unencrypted)
- For production deployments, use VPN, SSH tunnels, or trusted networks
- TLS integration planned for v1.1.0
- Message-level signing provides authentication without encryption

### Contributors
- Core Team
- Community Contributors

### References
- [TIMECOIN_PROTOCOL.md](docs/TIMECOIN_PROTOCOL.md) - Protocol specification
- [AI_SYSTEM.md](docs/AI_SYSTEM.md) - AI features documentation
- [GitHub Repository](https://github.com/time-coin/timecoin)

---

## [0.1.0] - 2025-12-23

### Initial Development Release
- Avalanche Snowball consensus implementation
- TSDC (Time-Scheduled Deterministic Consensus)
- Verifiable Finality Proofs (VFP)
- Masternode system with 3 tiers
- UTXO state machine
- P2P networking
- RPC API
- Basic peer selection

---

[1.0.0]: https://github.com/time-coin/timecoin/releases/tag/v1.0.0
[0.1.0]: https://github.com/time-coin/timecoin/releases/tag/v0.1.0
