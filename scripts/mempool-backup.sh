#!/usr/bin/env bash
# mempool-backup.sh — Download every transaction currently in the mempool of a
# running node and save them to a JSON file for replay after restart.
#
# Usage:
#   ./scripts/mempool-backup.sh [--testnet] [--rpc <host:port>] [--out <file>]
#                               [--rpcuser <user>] [--rpcpass <pass>]
#
# Defaults:
#   RPC endpoint  : 127.0.0.1:24001  (mainnet)  / 127.0.0.1:24101 (--testnet)
#   Credentials   : read from ~/.timecoin[/testnet]/time.conf if not provided
#   Output file   : mempool-backup-<timestamp>.json
#
# The backup file contains one JSON object per line:
#   {"txid":"<hex>","status":"<pending|finalized>","fee":<sats>,"hex":"<raw_tx_hex>"}
#
# Replay with:  ./scripts/mempool-restore.sh <backup-file>

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────
RPC_HOST="127.0.0.1"
RPC_PORT="24001"
RPC_USER=""
RPC_PASS=""
OUT_FILE=""
TESTNET=0

# ── Argument parsing ───────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --testnet)    RPC_PORT="24101" ; TESTNET=1 ; shift ;;
        --rpc)        RPC_HOST="${2%%:*}" ; RPC_PORT="${2##*:}" ; shift 2 ;;
        --rpcuser)    RPC_USER="$2" ; shift 2 ;;
        --rpcpass)    RPC_PASS="$2" ; shift 2 ;;
        --out)        OUT_FILE="$2" ; shift 2 ;;
        *) echo "Unknown option: $1" ; exit 1 ;;
    esac
done

# ── Auto-load credentials from time.conf if not provided ──────────────────────
load_conf_creds() {
    local conf_file="$1"
    [[ -f "$conf_file" ]] || return
    [[ -z "$RPC_USER" ]] && RPC_USER=$(grep -E '^rpcuser=' "$conf_file" 2>/dev/null | cut -d= -f2 | tr -d '[:space:]') || true
    [[ -z "$RPC_PASS" ]] && RPC_PASS=$(grep -E '^rpcpassword=' "$conf_file" 2>/dev/null | cut -d= -f2 | tr -d '[:space:]') || true
}

if [[ -z "$RPC_USER" || -z "$RPC_PASS" ]]; then
    if [[ "$TESTNET" -eq 1 ]]; then
        load_conf_creds "${HOME}/.timecoin/testnet/time.conf"
    fi
    load_conf_creds "${HOME}/.timecoin/time.conf"
fi

RPC_URL="http://${RPC_HOST}:${RPC_PORT}"
[[ -z "$OUT_FILE" ]] && OUT_FILE="mempool-backup-$(date +%Y%m%d-%H%M%S).json"

# ── Helpers ────────────────────────────────────────────────────────────────────
rpc() {
    local method="$1"; shift
    local params="$*"
    local auth_args=()
    [[ -n "$RPC_USER" ]] && auth_args=(-u "${RPC_USER}:${RPC_PASS}")
    curl -s "${auth_args[@]}" -X POST "$RPC_URL" \
         -H 'Content-Type: application/json' \
         --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":[${params}]}"
}

check_jq() {
    if ! command -v jq &>/dev/null; then
        echo "❌  jq is required but not installed. Install with: apt install jq / brew install jq"
        exit 1
    fi
}

# ── Main ───────────────────────────────────────────────────────────────────────
check_jq

echo "🔌  Connecting to TIME node at ${RPC_URL} ..."

# Verify the node is reachable
INFO=$(rpc getmempoolinfo) || { echo "❌  Cannot reach RPC at ${RPC_URL}"; exit 1; }
PENDING=$(echo "$INFO"  | jq -r '.result.pending  // 0')
FINALIZED=$(echo "$INFO" | jq -r '.result.finalized // 0')
TOTAL=$(echo "$INFO"    | jq -r '.result.size      // 0')

echo "📊  Mempool: ${PENDING} pending, ${FINALIZED} finalized  (${TOTAL} total)"

if [[ "$TOTAL" -eq 0 ]]; then
    echo "ℹ️   Mempool is empty — nothing to back up."
    exit 0
fi

# Get verbose metadata (fee + status per txid)
VERBOSE=$(rpc getmempoolverbose)

# Build a lookup: txid -> {status, fee}
declare -A TX_STATUS TX_FEE
while IFS= read -r line; do
    txid=$(echo "$line" | jq -r '.txid')
    status=$(echo "$line" | jq -r '.status')
    fee=$(echo "$line" | jq -r '.fee // 0')
    TX_STATUS["$txid"]="$status"
    TX_FEE["$txid"]="$fee"
done < <(echo "$VERBOSE" | jq -c '.result[]?')

# Get full txid list (includes both pending + finalized)
TXIDS=$(rpc getrawmempool | jq -r '.result[]?')
TOTAL_IDS=$(echo "$TXIDS" | wc -l | tr -d ' ')

echo "⬇️   Downloading ${TOTAL_IDS} raw transaction(s) → ${OUT_FILE}"

SAVED=0
FAILED=0

# Truncate / create output file
> "$OUT_FILE"

while IFS= read -r txid; do
    [[ -z "$txid" ]] && continue

    RAW_RESP=$(rpc getrawtransaction "\"${txid}\"")
    HEX=$(echo "$RAW_RESP" | jq -r '.result // empty')

    if [[ -z "$HEX" ]]; then
        ERR=$(echo "$RAW_RESP" | jq -r '.error.message // "unknown error"')
        echo "  ⚠️  ${txid:0:16}…  FAILED: ${ERR}"
        FAILED=$((FAILED + 1))
        continue
    fi

    STATUS="${TX_STATUS[$txid]:-unknown}"
    FEE="${TX_FEE[$txid]:-0}"

    echo "{\"txid\":\"${txid}\",\"status\":\"${STATUS}\",\"fee\":${FEE},\"hex\":\"${HEX}\"}" \
        >> "$OUT_FILE"
    SAVED=$((SAVED + 1))

    printf "  ✅  %s…  (%s, fee %s)\n" "${txid:0:16}" "$STATUS" "$FEE"
done <<< "$TXIDS"

echo ""
echo "────────────────────────────────────────"
echo "✅  Saved  : ${SAVED} transaction(s)"
[[ "$FAILED" -gt 0 ]] && echo "⚠️   Failed : ${FAILED} transaction(s)"
echo "📄  Output : ${OUT_FILE}"
echo ""
echo "To restore after restart, run:"
echo "  ./scripts/mempool-restore.sh ${OUT_FILE}"
