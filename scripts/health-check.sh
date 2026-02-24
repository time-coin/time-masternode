#!/bin/bash
# health-check.sh — Quick health probe for TIME Coin node
#
# Returns exit code 0 if healthy, 1 if degraded, 2 if critical.
# Designed for cron jobs, monitoring tools (Nagios, uptime checks), etc.
#
# Usage:
#   bash scripts/health-check.sh [OPTIONS]
#
# Options:
#   --testnet         Use testnet RPC port (24101)
#   --mainnet         Use mainnet RPC port (24001)
#   -r, --rpc-url     Custom RPC URL
#   -q, --quiet       Only output exit code (no text)
#   -j, --json        Output as JSON
#   --max-behind N    Max blocks behind before warning (default: 5)
#   --min-peers N     Minimum connected peers (default: 2)
#   -h, --help        Show this help
#
# Exit codes:
#   0 = Healthy
#   1 = Degraded (warnings but operational)
#   2 = Critical (node down or non-functional)

set -uo pipefail

# Defaults
RPC_URL=""
QUIET=0
JSON=0
MAX_BEHIND=5
MIN_PEERS=2
NETWORK="testnet"

# Find CLI
CLI=""
if command -v time-cli &> /dev/null; then
    CLI="time-cli"
elif [ -x "/usr/local/bin/time-cli" ]; then
    CLI="/usr/local/bin/time-cli"
elif [ -x "./target/release/time-cli" ]; then
    CLI="./target/release/time-cli"
fi

usage() {
    sed -n '/^# Usage:/,/^$/p' "$0" | sed 's/^# //'
    sed -n '/^# Options:/,/^$/p' "$0" | sed 's/^# //'
    sed -n '/^# Exit codes:/,/^$/p' "$0" | sed 's/^# //'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --testnet)       NETWORK="testnet"; shift ;;
        --mainnet)       NETWORK="mainnet"; shift ;;
        -r|--rpc-url)    RPC_URL="$2"; shift 2 ;;
        -q|--quiet)      QUIET=1; shift ;;
        -j|--json)       JSON=1; shift ;;
        --max-behind)    MAX_BEHIND="$2"; shift 2 ;;
        --min-peers)     MIN_PEERS="$2"; shift 2 ;;
        -h|--help)       usage ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
done

# Build CLI command
if [ -z "$CLI" ]; then
    [ "$QUIET" -eq 0 ] && [ "$JSON" -eq 0 ] && echo "CRITICAL: time-cli not found"
    [ "$JSON" -eq 1 ] && echo '{"status":"critical","error":"time-cli not found"}'
    exit 2
fi

CLI_CMD="$CLI"
if [ -n "$RPC_URL" ]; then
    CLI_CMD="$CLI --rpc-url $RPC_URL"
elif [ "$NETWORK" = "testnet" ]; then
    CLI_CMD="$CLI --testnet"
fi

# Check results
STATUS="healthy"
WARNINGS=()
ERRORS=()

# 1. Daemon process running?
if ! systemctl is-active --quiet timed 2>/dev/null; then
    # Fallback: try RPC directly
    if ! $CLI_CMD getblockcount > /dev/null 2>&1; then
        [ "$QUIET" -eq 0 ] && [ "$JSON" -eq 0 ] && echo "CRITICAL: timed daemon not running"
        [ "$JSON" -eq 1 ] && echo '{"status":"critical","error":"daemon not running","service_active":false}'
        exit 2
    fi
fi

# 2. RPC responsive?
BLOCK_HEIGHT=$($CLI_CMD getblockcount 2>/dev/null)
if [ -z "$BLOCK_HEIGHT" ] || ! [[ "$BLOCK_HEIGHT" =~ ^[0-9]+$ ]]; then
    [ "$QUIET" -eq 0 ] && [ "$JSON" -eq 0 ] && echo "CRITICAL: RPC not responding"
    [ "$JSON" -eq 1 ] && echo '{"status":"critical","error":"RPC not responding","service_active":true}'
    exit 2
fi

# 3. Peer connectivity
PEER_COUNT=0
PEER_JSON=$($CLI_CMD getpeerinfo 2>/dev/null) || true
if [ -n "$PEER_JSON" ]; then
    PEER_COUNT=$(echo "$PEER_JSON" | jq 'length' 2>/dev/null || echo "0")
fi

if [ "$PEER_COUNT" -eq 0 ]; then
    STATUS="critical"
    ERRORS+=("no connected peers")
elif [ "$PEER_COUNT" -lt "$MIN_PEERS" ]; then
    STATUS="degraded"
    WARNINGS+=("low peer count: $PEER_COUNT (min: $MIN_PEERS)")
fi

# 4. Sync status — compare with peer heights
MAX_PEER_HEIGHT=0
if [ -n "$PEER_JSON" ] && [ "$PEER_COUNT" -gt 0 ]; then
    MAX_PEER_HEIGHT=$(echo "$PEER_JSON" | jq '[.[].height // 0] | max' 2>/dev/null || echo "0")
fi

BEHIND=0
if [ "$MAX_PEER_HEIGHT" -gt "$BLOCK_HEIGHT" ]; then
    BEHIND=$((MAX_PEER_HEIGHT - BLOCK_HEIGHT))
fi

if [ "$BEHIND" -gt "$MAX_BEHIND" ]; then
    if [ "$BEHIND" -gt 100 ]; then
        STATUS="critical"
        ERRORS+=("severely behind: $BEHIND blocks ($BLOCK_HEIGHT vs peer $MAX_PEER_HEIGHT)")
    else
        [ "$STATUS" != "critical" ] && STATUS="degraded"
        WARNINGS+=("behind by $BEHIND blocks ($BLOCK_HEIGHT vs peer $MAX_PEER_HEIGHT)")
    fi
fi

# 5. Masternode status
MN_STATUS=$($CLI_CMD masternodestatus 2>/dev/null) || true
MN_ACTIVE="unknown"
MN_TIER="unknown"
if [ -n "$MN_STATUS" ]; then
    MN_ACTIVE=$(echo "$MN_STATUS" | jq -r '.is_active // "unknown"' 2>/dev/null || echo "unknown")
    MN_TIER=$(echo "$MN_STATUS" | jq -r '.tier // "unknown"' 2>/dev/null || echo "unknown")
fi

# 6. Check for recent errors in logs (last 5 minutes)
ERROR_COUNT=0
FORK_COUNT=0
if command -v journalctl &> /dev/null; then
    ERROR_COUNT=$(journalctl -u timed --since "5 minutes ago" --no-pager 2>/dev/null | grep -c " ERROR " || true)
    FORK_COUNT=$(journalctl -u timed --since "5 minutes ago" --no-pager 2>/dev/null | grep -c "Fork detected" || true)
fi

if [ "$ERROR_COUNT" -gt 10 ]; then
    [ "$STATUS" != "critical" ] && STATUS="degraded"
    WARNINGS+=("$ERROR_COUNT errors in last 5 min")
fi

if [ "$FORK_COUNT" -gt 50 ]; then
    STATUS="critical"
    ERRORS+=("fork loop detected: $FORK_COUNT events in 5 min")
elif [ "$FORK_COUNT" -gt 10 ]; then
    [ "$STATUS" != "critical" ] && STATUS="degraded"
    WARNINGS+=("high fork activity: $FORK_COUNT events in 5 min")
fi

# Determine exit code
EXIT_CODE=0
case "$STATUS" in
    healthy)  EXIT_CODE=0 ;;
    degraded) EXIT_CODE=1 ;;
    critical) EXIT_CODE=2 ;;
esac

# Output
if [ "$JSON" -eq 1 ]; then
    WARN_JSON=$(printf '%s\n' "${WARNINGS[@]}" 2>/dev/null | jq -R . | jq -s . 2>/dev/null || echo "[]")
    ERR_JSON=$(printf '%s\n' "${ERRORS[@]}" 2>/dev/null | jq -R . | jq -s . 2>/dev/null || echo "[]")
    cat <<EOF
{
  "status": "$STATUS",
  "height": $BLOCK_HEIGHT,
  "peers": $PEER_COUNT,
  "blocks_behind": $BEHIND,
  "masternode_active": "$MN_ACTIVE",
  "masternode_tier": "$MN_TIER",
  "recent_errors": $ERROR_COUNT,
  "recent_forks": $FORK_COUNT,
  "warnings": $WARN_JSON,
  "errors": $ERR_JSON
}
EOF
elif [ "$QUIET" -eq 0 ]; then
    case "$STATUS" in
        healthy)  echo -e "\033[0;32m✅ HEALTHY\033[0m — Height: $BLOCK_HEIGHT | Peers: $PEER_COUNT | MN: $MN_TIER ($MN_ACTIVE)" ;;
        degraded)
            echo -e "\033[1;33m⚠️  DEGRADED\033[0m — Height: $BLOCK_HEIGHT | Peers: $PEER_COUNT | MN: $MN_TIER ($MN_ACTIVE)"
            for w in "${WARNINGS[@]}"; do echo "  ⚠ $w"; done
            ;;
        critical)
            echo -e "\033[0;31m❌ CRITICAL\033[0m — Height: $BLOCK_HEIGHT | Peers: $PEER_COUNT"
            for e in "${ERRORS[@]}"; do echo "  ✗ $e"; done
            for w in "${WARNINGS[@]}"; do echo "  ⚠ $w"; done
            ;;
    esac
fi

exit $EXIT_CODE
