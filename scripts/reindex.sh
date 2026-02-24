#!/bin/bash
# reindex.sh — Safely reindex the TIME Coin blockchain
#
# Triggers a full UTXO + transaction index rebuild. The daemon must be running.
# The reindex RPC blocks until complete — this script monitors progress.
#
# Usage:
#   bash scripts/reindex.sh [OPTIONS]
#
# Options:
#   --testnet         Use testnet RPC port (default)
#   --mainnet         Use mainnet RPC port
#   -r, --rpc-url     Custom RPC URL
#   --tx-only         Only reindex transaction index (faster, non-blocking)
#   -h, --help        Show this help

set -euo pipefail

# Defaults
NETWORK="testnet"
RPC_URL=""
TX_ONLY=0

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}ℹ️  $1${NC}"; }
log_success() { echo -e "${GREEN}✅ $1${NC}"; }
log_warn()    { echo -e "${YELLOW}⚠️  $1${NC}"; }
log_error()   { echo -e "${RED}❌ $1${NC}"; }

usage() {
    sed -n '/^# Usage:/,/^$/p' "$0" | sed 's/^# //'
    sed -n '/^# Options:/,/^$/p' "$0" | sed 's/^# //'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --testnet)    NETWORK="testnet"; shift ;;
        --mainnet)    NETWORK="mainnet"; shift ;;
        -r|--rpc-url) RPC_URL="$2"; shift 2 ;;
        --tx-only)    TX_ONLY=1; shift ;;
        -h|--help)    usage ;;
        *) log_error "Unknown option: $1"; usage ;;
    esac
done

# Find CLI
CLI=""
if command -v time-cli &> /dev/null; then
    CLI="time-cli"
elif [ -x "/usr/local/bin/time-cli" ]; then
    CLI="/usr/local/bin/time-cli"
elif [ -x "./target/release/time-cli" ]; then
    CLI="./target/release/time-cli"
else
    log_error "time-cli not found"
    exit 1
fi

CLI_CMD="$CLI"
if [ -n "$RPC_URL" ]; then
    CLI_CMD="$CLI --rpc-url $RPC_URL"
elif [ "$NETWORK" = "testnet" ]; then
    CLI_CMD="$CLI --testnet"
fi

echo ""
echo "╔══════════════════════════════════════════╗"
echo "║     TIME Coin Blockchain Reindex         ║"
echo "╚══════════════════════════════════════════╝"
echo ""

# Verify daemon is running
if ! $CLI_CMD getblockcount > /dev/null 2>&1; then
    log_error "Cannot connect to daemon. Is timed running?"
    exit 1
fi

HEIGHT=$($CLI_CMD getblockcount 2>/dev/null)
log_info "Current height: $HEIGHT"
log_info "Mode: $([ "$TX_ONLY" -eq 1 ] && echo "transaction index only" || echo "full reindex (UTXOs + tx index)")"
echo ""

if [ "$TX_ONLY" -eq 1 ]; then
    log_info "Starting transaction reindex (background)..."
    RESULT=$($CLI_CMD reindextransactions 2>&1) || true
    log_success "Transaction reindex started in background"
    log_info "Response: $RESULT"
    echo ""
    log_info "Monitor with: $CLI_CMD gettxindexstatus"
    echo ""

    # Poll until complete
    log_info "Polling status..."
    for i in $(seq 1 300); do
        STATUS=$($CLI_CMD gettxindexstatus 2>/dev/null) || true
        if echo "$STATUS" | jq -e '.reindexing == false' > /dev/null 2>&1; then
            TX_COUNT=$(echo "$STATUS" | jq -r '.indexed_count // "unknown"')
            log_success "Transaction reindex complete ($TX_COUNT transactions indexed)"
            exit 0
        fi
        PROGRESS=$(echo "$STATUS" | jq -r '.progress // "unknown"' 2>/dev/null || echo "...")
        echo -ne "\r  ⏳ Reindexing... ($PROGRESS) [${i}s]          "
        sleep 1
    done
    echo ""
    log_warn "Still running after 300s — check: $CLI_CMD gettxindexstatus"
else
    log_warn "Full reindex will clear UTXOs and rebuild from genesis."
    log_warn "This blocks RPC until complete and may take several minutes."
    echo ""
    read -p "Continue? (type 'reindex' to confirm): " -r
    if [ "$REPLY" != "reindex" ]; then
        echo "Cancelled"
        exit 0
    fi
    echo ""

    log_info "Starting full reindex (this will block until complete)..."
    START_TIME=$(date +%s)

    RESULT=$($CLI_CMD reindex 2>&1) || true

    END_TIME=$(date +%s)
    ELAPSED=$((END_TIME - START_TIME))

    if echo "$RESULT" | jq -e '.blocks_processed' > /dev/null 2>&1; then
        BLOCKS=$(echo "$RESULT" | jq -r '.blocks_processed // "unknown"')
        UTXOS=$(echo "$RESULT" | jq -r '.utxo_count // "unknown"')
        TX_REBUILT=$(echo "$RESULT" | jq -r '.tx_index_rebuilt // false')

        echo ""
        log_success "Full reindex complete in ${ELAPSED}s!"
        echo ""
        echo "  Blocks processed:    $BLOCKS"
        echo "  UTXO count:          $UTXOS"
        echo "  TX index rebuilt:    $TX_REBUILT"
    else
        echo ""
        log_warn "Reindex returned: $RESULT"
        log_info "Completed in ${ELAPSED}s"
    fi
fi

echo ""
log_info "Verify with: $CLI_CMD getblockchaininfo"
