#!/usr/bin/env bash
# Poll the TIME website for the whitelisted peer list and unban each IP locally.
set -euo pipefail

MAINNET_URL="https://time-coin.io/api/peers"
TESTNET_URL="https://time-coin.io/api/testnet/peers"
CLI="${CLI:-./target/release/time-cli}"
TESTNET=0
INTERVAL=0  # 0 = run once; >0 = poll every N seconds

usage() {
    echo "Usage: $0 [--testnet] [--loop <seconds>] [--cli <path>]"
    echo "  --testnet        Use testnet peer list and RPC port"
    echo "  --loop <secs>    Re-run every N seconds (default: run once)"
    echo "  --cli <path>     Path to time-cli binary (default: ./target/release/time-cli)"
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --testnet)  TESTNET=1 ;;
        --loop)     INTERVAL="${2:?}"; shift ;;
        --cli)      CLI="${2:?}"; shift ;;
        -h|--help)  usage ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
    shift
done

PEER_URL="$MAINNET_URL"
TESTNET_FLAG=""
[[ $TESTNET -eq 1 ]] && PEER_URL="$TESTNET_URL" && TESTNET_FLAG="--testnet"

run_once() {
    echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] Fetching peer list from $PEER_URL ..."

    PEERS=$(curl -fsSL --max-time 15 "$PEER_URL") || {
        echo "ERROR: Failed to fetch peer list." >&2
        return 1
    }

    # Accept either a JSON array of strings or objects with an "ip" field
    IPS=$(echo "$PEERS" | python3 -c "
import sys, json
data = json.load(sys.stdin)
for entry in data:
    if isinstance(entry, str):
        print(entry.split(':')[0])
    elif isinstance(entry, dict):
        ip = entry.get('ip') or entry.get('address') or ''
        print(ip.split(':')[0])
" 2>/dev/null) || {
        echo "ERROR: Could not parse peer list JSON." >&2
        return 1
    }

    COUNT=0
    FAILED=0
    while IFS= read -r ip; do
        [[ -z "$ip" ]] && continue
        if "$CLI" $TESTNET_FLAG unban "$ip" >/dev/null 2>&1; then
            echo "  unbanned: $ip"
            (( COUNT++ )) || true
        else
            echo "  skipped:  $ip (not banned or error)"
            (( FAILED++ )) || true
        fi
    done <<< "$IPS"

    echo "Done: $COUNT unbanned, $FAILED skipped."
}

if [[ $INTERVAL -gt 0 ]]; then
    echo "Polling every ${INTERVAL}s. Press Ctrl-C to stop."
    while true; do
        run_once
        sleep "$INTERVAL"
    done
else
    run_once
fi
