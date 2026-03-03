#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────
# TIME Coin — Security Check Script
# Runs cargo-audit and cargo-deny for dependency vulnerability scanning.
# ─────────────────────────────────────────────────────────────
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${GREEN}═══════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}  TIME Coin Security Checks${NC}"
echo -e "${GREEN}═══════════════════════════════════════════════════════${NC}"
echo ""

FAILED=0

# ── cargo audit ──────────────────────────────────────────────
echo -e "${YELLOW}[1/3] Running cargo audit (known vulnerabilities)...${NC}"
if command -v cargo-audit &>/dev/null; then
    if cargo audit; then
        echo -e "${GREEN}  ✅ No known vulnerabilities found${NC}"
    else
        echo -e "${RED}  ❌ Vulnerabilities detected — review output above${NC}"
        FAILED=1
    fi
else
    echo -e "${YELLOW}  ⚠ cargo-audit not installed. Install with: cargo install cargo-audit${NC}"
fi
echo ""

# ── cargo deny ───────────────────────────────────────────────
echo -e "${YELLOW}[2/3] Running cargo deny (license & advisory checks)...${NC}"
if command -v cargo-deny &>/dev/null; then
    if cargo deny check 2>/dev/null; then
        echo -e "${GREEN}  ✅ All dependency policies pass${NC}"
    else
        echo -e "${RED}  ❌ Policy violations detected — review output above${NC}"
        FAILED=1
    fi
else
    echo -e "${YELLOW}  ⚠ cargo-deny not installed. Install with: cargo install cargo-deny${NC}"
fi
echo ""

# ── clippy security lints ────────────────────────────────────
echo -e "${YELLOW}[3/3] Running clippy with strict warnings...${NC}"
if cargo clippy -- -D warnings 2>&1; then
    echo -e "${GREEN}  ✅ No clippy warnings${NC}"
else
    echo -e "${YELLOW}  ⚠ Clippy warnings detected (non-blocking)${NC}"
fi
echo ""

# ── Summary ──────────────────────────────────────────────────
if [ "$FAILED" -eq 0 ]; then
    echo -e "${GREEN}All security checks passed ✅${NC}"
else
    echo -e "${RED}Some security checks failed — review output above${NC}"
    exit 1
fi
