#!/usr/bin/env bash
# mempool-restore.sh — Replay transactions saved by mempool-backup.sh back into
# a running node via sendrawtransaction.
#
# Usage:
#   ./scripts/mempool-restore.sh <backup-file> [--testnet] [--rpc <host:port>]
#                                [--rpcuser <user>] [--rpcpass <pass>]
#
# Defaults:
#   RPC endpoint : 127.0.0.1:24001  (mainnet) / 127.0.0.1:24101 (--testnet)
#   Credentials  : read from ~/.timecoin[/testnet]/time.conf if not provided
#
# Finalized transactions are submitted first so they can re-enter the finalized
# pool quickly; pending transactions follow and will go through TimeVote again.

set -euo pipefail

# ── Defaults ───────────────────────────────────────────────────────────────────
RPC_HOST="127.0.0.1"
RPC_PORT="24001"
RPC_USER=""
RPC_PASS=""
BACKUP_FILE=""
TESTNET=0

# ── Argument parsing ───────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --testnet)    RPC_PORT="24101" ; TESTNET=1 ; shift ;;
        --rpc)        RPC_HOST="${2%%:*}" ; RPC_PORT="${2##*:}" ; shift 2 ;;
        --rpcuser)    RPC_USER="$2" ; shift 2 ;;
        --rpcpass)    RPC_PASS="$2" ; shift 2 ;;
        *.json)       BACKUP_FILE="$1" ; shift ;;
        *) echo "Unknown option: $1" ; exit 1 ;;
    esac
done

if [[ -z "$BACKUP_FILE" ]]; then
    echo "Usage: $0 <backup-file.json> [--testnet] [--rpc <host:port>] [--rpcuser <u>] [--rpcpass <p>]"
    exit 1
fi

if [[ ! -f "$BACKUP_FILE" ]]; then
    echo "❌  Backup file not found: ${BACKUP_FILE}"
    exit 1
fi

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

# ── Helpers ────────────────────────────────────────────────────────────────────
rpc() {
    local method="$1"; shift
    local params="$*"
    local auth_args=()
    [[ -n "$RPC_USER" ]] && auth_args=(-u "${RPC_USER}:${RPC_PASS}")
    curl -s "${auth_args[@]}" -X POST "$RPC_URL" \
         -H 'Content-Type: application/json' \
         --data "{\"jsonrpc\":\"2.0\",\"id\":\"1\",\"method\":\"${method}\",\"params\":[${params}]}"
}

check_jq() {
    if ! command -v jq &>/dev/null; then
        echo "❌  jq is required. Install with: apt install jq / brew install jq"
        exit 1
    fi
}

submit_tx() {
    local txid="$1" hex="$2" status="$3" fee="$4"

    RESP=$(rpc sendrawtransaction "\"${hex}\"")
    ERR=$(echo "$RESP" | jq -r '.error // empty')

    if [[ -n "$ERR" ]]; then
        MSG=$(echo "$RESP" | jq -r '.error.message // "unknown error"')
        # Already in pool / already confirmed are not real errors
        if echo "$MSG" | grep -qi "already\|duplicate\|AlreadyExists"; then
            printf "  ⏭️   %s…  already in pool\n" "${txid:0:16}"
        else
            printf "  ⚠️   %s…  FAILED: %s\n" "${txid:0:16}" "$MSG"
            return 1
        fi
    else
        printf "  ✅  %s…  (%s, fee %s)\n" "${txid:0:16}" "$status" "$fee"
    fi
    return 0
}

# ── Main ───────────────────────────────────────────────────────────────────────
check_jq

echo "🔌  Connecting to TIME node at ${RPC_URL} ..."
rpc getmempoolinfo > /dev/null 2>&1 || { echo "❌  Cannot reach RPC at ${RPC_URL}"; exit 1; }

TOTAL=$(wc -l < "$BACKUP_FILE" | tr -d ' ')
echo "📄  Backup file : ${BACKUP_FILE}  (${TOTAL} transaction(s))"
echo ""

SUBMITTED=0
SKIPPED=0
FAILED=0

# ── Pass 1: finalized transactions (restore them first so they re-enter the
#            finalized pool and don't have to wait for a fresh TimeVote round)
echo "── Pass 1/2: finalized transactions ──────────────────────────────────────"
while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    STATUS=$(echo "$line" | jq -r '.status')
    [[ "$STATUS" != "finalized" ]] && continue

    TXID=$(echo "$line" | jq -r '.txid')
    HEX=$(echo "$line"  | jq -r '.hex')
    FEE=$(echo "$line"  | jq -r '.fee')

    if submit_tx "$TXID" "$HEX" "$STATUS" "$FEE"; then
        SUBMITTED=$((SUBMITTED + 1))
    else
        FAILED=$((FAILED + 1))
    fi
done < "$BACKUP_FILE"

# ── Pass 2: pending transactions
echo ""
echo "── Pass 2/2: pending transactions ───────────────────────────────────────"
while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    STATUS=$(echo "$line" | jq -r '.status')
    [[ "$STATUS" == "finalized" ]] && continue

    TXID=$(echo "$line" | jq -r '.txid')
    HEX=$(echo "$line"  | jq -r '.hex')
    FEE=$(echo "$line"  | jq -r '.fee')

    if submit_tx "$TXID" "$HEX" "$STATUS" "$FEE"; then
        SUBMITTED=$((SUBMITTED + 1))
    else
        FAILED=$((FAILED + 1))
    fi
done < "$BACKUP_FILE"

echo ""
echo "────────────────────────────────────────"
echo "✅  Submitted : ${SUBMITTED}"
echo "⏭️   Skipped   : ${SKIPPED} (already in pool)"
[[ "$FAILED" -gt 0 ]] && echo "⚠️   Failed    : ${FAILED}"

# Show new mempool state
echo ""
echo "📊  Mempool after restore:"
rpc getmempoolinfo | jq '.result | {pending, finalized, size, bytes}'
