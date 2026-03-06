# 📚 TIME Coin Documentation Index

**Last Updated:** March 6, 2026  
**Status:** ✅ Production Ready (v1.2.0)

---

## 🚀 Getting Started

### New to TIME Coin?
1. Read **[README.md](../README.md)** — Project overview and features
2. Follow **[LINUX_INSTALLATION.md](LINUX_INSTALLATION.md)** — Full installation guide (fresh Linux → running masternode)
3. Read **[CLI_GUIDE.md](CLI_GUIDE.md)** — Command-line interface

### Quick Paths

| I want to...                     | Read this                                                  |
|----------------------------------|------------------------------------------------------------|
| Deploy a masternode on Linux     | [LINUX_INSTALLATION.md](LINUX_INSTALLATION.md)             |
| Manage masternode operations     | [MASTERNODE_GUIDE.md](MASTERNODE_GUIDE.md)                 |
| Build & test locally (developer) | [QUICKSTART.md](QUICKSTART.md)                             |
| Use the CLI                      | [CLI_GUIDE.md](CLI_GUIDE.md)                               |
| Understand the protocol          | [TIMECOIN_PROTOCOL.md](TIMECOIN_PROTOCOL.md)               |
| Review security                  | [COMPREHENSIVE_SECURITY_AUDIT.md](COMPREHENSIVE_SECURITY_AUDIT.md) |
| Configure networking             | [NETWORK_ARCHITECTURE.md](NETWORK_ARCHITECTURE.md)         |
| Contribute code                  | [CONTRIBUTING.md](CONTRIBUTING.md)                         |

---

## 📖 Document Guide

### Installation & Operations

| Document | Purpose | Audience |
|----------|---------|----------|
| [LINUX_INSTALLATION.md](LINUX_INSTALLATION.md) | Step-by-step Linux installation, configuration, security hardening, upgrading, troubleshooting, database reset | Node Operators |
| [MASTERNODE_GUIDE.md](MASTERNODE_GUIDE.md) | Masternode tiers, collateral, rewards, reward rotation, block producer selection, deregistration, FAQ | Masternode Operators |
| [QUICKSTART.md](QUICKSTART.md) | Build, test, and run nodes locally; multi-node setup | Developers |
| [CLI_GUIDE.md](CLI_GUIDE.md) | Full command reference for `time-cli`, wallet operations | All Users |

### Protocol & Architecture

| Document | Purpose | Audience |
|----------|---------|----------|
| [TIMECOIN_PROTOCOL.md](TIMECOIN_PROTOCOL.md) | Complete protocol specification (§1–§27) + appendices: fee collection mechanism, cryptography rationale | Developers, Researchers |
| [ARCHITECTURE_OVERVIEW.md](ARCHITECTURE_OVERVIEW.md) | System architecture, complete transaction/consensus flow, TimeProof conflict detection | All Developers |
| [NETWORK_ARCHITECTURE.md](NETWORK_ARCHITECTURE.md) | P2P network layer design, network configuration reference, integration guide | Network Developers |

### Security

| Document | Purpose | Audience |
|----------|---------|----------|
| [COMPREHENSIVE_SECURITY_AUDIT.md](COMPREHENSIVE_SECURITY_AUDIT.md) | Full attack-vector analysis (30+ vectors) | Security Reviewers |
| [SECURITY.md](SECURITY.md) | Vulnerability reporting policy, threat analysis, UTXO attack vectors | Security Researchers |

### Reference

| Document | Purpose | Audience |
|----------|---------|----------|
| [QUICK_REFERENCE.md](QUICK_REFERENCE.md) | One-page parameter lookup | All Users |
| [PRE_MAINNET_CHECKLIST.md](PRE_MAINNET_CHECKLIST.md) | Implementation status checklist | Developers |

### Project Management

| Document | Purpose | Audience |
|----------|---------|----------|
| [CONTRIBUTING.md](CONTRIBUTING.md) | Development guidelines, commit conventions | Contributors |
| [ROADMAP.md](ROADMAP.md) | Development roadmap | All |
| [../CHANGELOG.md](../CHANGELOG.md) | Version history | All |

---

## 🎯 Recommended Reading Order

### For Node Operators
1. [LINUX_INSTALLATION.md](LINUX_INSTALLATION.md) — Install and run
2. [MASTERNODE_GUIDE.md](MASTERNODE_GUIDE.md) — Manage your masternode
3. [CLI_GUIDE.md](CLI_GUIDE.md) — Day-to-day commands

### For Developers
1. [QUICKSTART.md](QUICKSTART.md) — Build, test, run locally
2. [ARCHITECTURE_OVERVIEW.md](ARCHITECTURE_OVERVIEW.md) — System design and flows
3. [TIMECOIN_PROTOCOL.md](TIMECOIN_PROTOCOL.md) — Protocol specification
4. [CONTRIBUTING.md](CONTRIBUTING.md) — How to contribute

### For Security Reviewers
1. [COMPREHENSIVE_SECURITY_AUDIT.md](COMPREHENSIVE_SECURITY_AUDIT.md) — Attack analysis
2. [SECURITY.md](SECURITY.md) — Threat analysis and policy
3. [TIMECOIN_PROTOCOL.md](TIMECOIN_PROTOCOL.md) — Protocol details and cryptography rationale (Appendix B)

---

## 📊 Quick Facts

| Category | Detail |
|----------|--------|
| Protocol Version | v6.2 |
| Software Version | v1.2.0 |
| Consensus | TimeVote (real-time) + TimeLock (archival) |
| Finality | <1 second (deterministic) |
| Block Time | 600 seconds |
| Mainnet Ports | P2P: 24000, RPC: 24001 |
| Testnet Ports | P2P: 24100, RPC: 24101 |
| Language | Rust 1.75+ |

---

## 📞 Support

- **GitHub Issues**: [Report bugs](https://github.com/time-coin/time-masternode/issues)
- **Website**: [time-coin.io](https://time-coin.io)
- **Email**: [support@time-coin.io](mailto:support@time-coin.io)
- **Security**: [security@time-coin.io](mailto:security@time-coin.io)

---

*Last Updated: March 6, 2026*
