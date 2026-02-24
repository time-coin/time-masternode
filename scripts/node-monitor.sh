#!/bin/bash
# node-monitor.sh — Persistent log watcher for TIME Coin node
#
# Monitors timed logs in real-time, highlighting important events:
#   🔴 Errors, panics, crashes
#   🟡 Forks, sync issues, deregistrations, warnings
#   🟢 Block production, finalization, peer connections
#   🔵 Masternode status changes, collateral events
#
# Usage:
#   bash scripts/node-monitor.sh [OPTIONS]
#
# Options:
#   --since TIME      Start from time (default: "5 minutes ago")
#   --no-color        Disable color output
#   -o, --output FILE Also write filtered events to a log file
#   -h, --help        Show this help

set -uo pipefail

# Defaults
SINCE="5 minutes ago"
COLOR=1
LOG_FILE=""

# Colors
RED='\033[0;31m'
YELLOW='\033[1;33m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
MAGENTA='\033[0;35m'
BOLD='\033[1m'
NC='\033[0m'

usage() {
    sed -n '/^# Usage:/,/^$/p' "$0" | sed 's/^# //'
    sed -n '/^# Options:/,/^$/p' "$0" | sed 's/^# //'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --since)      SINCE="$2"; shift 2 ;;
        --no-color)   COLOR=0; shift ;;
        -o|--output)  LOG_FILE="$2"; shift 2 ;;
        -h|--help)    usage ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
done

if [ "$COLOR" -eq 0 ]; then
    RED="" YELLOW="" GREEN="" BLUE="" CYAN="" MAGENTA="" BOLD="" NC=""
fi

# Verify journalctl is available
if ! command -v journalctl &> /dev/null; then
    echo "Error: journalctl not found (is this a systemd system?)"
    exit 1
fi

# Check if timed service exists
if ! systemctl list-unit-files timed.service &> /dev/null; then
    echo "Warning: timed.service not found, watching journal anyway..."
fi

echo -e "${BOLD}${CYAN}"
echo "╔══════════════════════════════════════════════════════╗"
echo "║         TIME Coin Node Monitor                       ║"
echo "╠══════════════════════════════════════════════════════╣"
echo "║  🔴 Errors/Panics    🟡 Forks/Warnings              ║"
echo "║  🟢 Blocks/Finality  🔵 Masternode/Collateral        ║"
echo "╚══════════════════════════════════════════════════════╝"
echo -e "${NC}"
echo -e "  Watching since: ${SINCE}"
if [ -n "$LOG_FILE" ]; then
    echo -e "  Logging to: ${LOG_FILE}"
fi
echo -e "  Press Ctrl+C to stop"
echo ""

# Counters
ERRORS=0
WARNINGS=0
BLOCKS=0
FORKS=0
START_TIME=$(date +%s)

# Cleanup on exit
cleanup() {
    echo ""
    ELAPSED=$(( $(date +%s) - START_TIME ))
    ELAPSED_MIN=$(( ELAPSED / 60 ))
    echo -e "${BOLD}── Monitor Summary (${ELAPSED_MIN}m) ──${NC}"
    echo -e "  🔴 Errors:   $ERRORS"
    echo -e "  🟡 Warnings: $WARNINGS"
    echo -e "  🟢 Blocks:   $BLOCKS"
    echo -e "  🔀 Forks:    $FORKS"
    exit 0
}
trap cleanup INT TERM

# Process each log line
process_line() {
    local line="$1"
    local ts
    ts=$(echo "$line" | grep -oP '^\w+ \d+ \d+:\d+:\d+' || echo "")
    local msg="${line#*timed*: }"
    local output=""
    local write_to_log=0

    # 🔴 CRITICAL — errors, panics, crashes
    if echo "$line" | grep -qiE "ERROR|panic|FATAL|thread.*panicked|out of memory"; then
        output="${RED}🔴 ${ts} ${msg}${NC}"
        ERRORS=$((ERRORS + 1))
        write_to_log=1

    # 🟡 FORKS — fork detection, reorganization
    elif echo "$line" | grep -qiE "Fork detected|REORGANIZATION|DEEP FORK|fork loop|fork resolution"; then
        output="${YELLOW}🔀 ${ts} ${msg}${NC}"
        FORKS=$((FORKS + 1))
        write_to_log=1

    # 🟡 WARNINGS — sync issues, deregistrations, timeouts
    elif echo "$line" | grep -qiE "WARN.*deregister|WARN.*collateral|WARN.*timeout|WARN.*behind|WARN.*stale|WARN.*circuit.breaker"; then
        output="${YELLOW}🟡 ${ts} ${msg}${NC}"
        WARNINGS=$((WARNINGS + 1))
        write_to_log=1

    # 🟢 BLOCKS — block production, validation, finalization
    elif echo "$line" | grep -qiE "Block.*finalized with consensus|Adding finalized block|Block.*validation passed|Generated prepare vote"; then
        output="${GREEN}🟢 ${ts} ${msg}${NC}"
        BLOCKS=$((BLOCKS + 1))

    # 🔵 MASTERNODE — registration, tier changes, collateral
    elif echo "$line" | grep -qiE "Registered masternode|masternode.*collateral|Unlocked collateral|Auto-detected tier|Running as.*masternode"; then
        output="${BLUE}🔵 ${ts} ${msg}${NC}"
        write_to_log=1

    # 📊 STATUS — periodic status reports
    elif echo "$line" | grep -qE "NODE STATUS|BLOCKS BEHIND"; then
        output="${MAGENTA}📊 ${ts} ${msg}${NC}"

    # 🔄 SYNC — sync events
    elif echo "$line" | grep -qiE "Sync.*successful|sync gate|syncing.*blocks|catch-up"; then
        output="${CYAN}🔄 ${ts} ${msg}${NC}"
    fi

    # Print if we have something
    if [ -n "$output" ]; then
        echo -e "$output"
        if [ "$write_to_log" -eq 1 ] && [ -n "$LOG_FILE" ]; then
            echo "$(date '+%Y-%m-%d %H:%M:%S') $msg" >> "$LOG_FILE"
        fi
    fi
}

# Main watch loop
journalctl -u timed --since "$SINCE" -f --no-pager 2>/dev/null | while IFS= read -r line; do
    process_line "$line"
done
