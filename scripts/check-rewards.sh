#!/bin/bash
# Check block reward distribution across all blocks
# Run on the node server: bash check-rewards.sh [rpc_port]

RPC_PORT="${1:-24001}"
RPC_URL="http://127.0.0.1:${RPC_PORT}"

# Get current height
HEIGHT=$(curl -s "$RPC_URL" -d '{"jsonrpc":"2.0","method":"getblockcount","params":[],"id":1}' | grep -o '"result":[0-9]*' | cut -d: -f2)

if [ -z "$HEIGHT" ]; then
    echo "ERROR: Cannot reach RPC at $RPC_URL"
    exit 1
fi

echo "=== Block Reward Analysis (height 0 to $HEIGHT) ==="
echo ""

# Track address reward counts
declare -A ADDR_COUNT
declare -A ADDR_TOTAL
declare -A BLOCKS_PRODUCED

for h in $(seq 0 "$HEIGHT"); do
    BLOCK=$(curl -s "$RPC_URL" -d "{\"jsonrpc\":\"2.0\",\"method\":\"getblock\",\"params\":[$h],\"id\":1}")

    # Extract leader
    LEADER=$(echo "$BLOCK" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('result',{}).get('leader',''))" 2>/dev/null)

    # Extract masternode_rewards as JSON
    REWARDS=$(echo "$BLOCK" | python3 -c "
import sys, json
d = json.load(sys.stdin)
rewards = d.get('result', {}).get('masternode_rewards', {})
if isinstance(rewards, dict):
    for addr, amt in rewards.items():
        print(f'{addr} {amt}')
elif isinstance(rewards, list):
    for r in rewards:
        if isinstance(r, dict):
            print(f\"{r.get('address','')} {r.get('amount',0)}\")
        elif isinstance(r, list) and len(r) == 2:
            print(f'{r[0]} {r[1]}')
" 2>/dev/null)

    REWARD_COUNT=$(echo "$REWARDS" | grep -c .)
    ADDRS=$(echo "$REWARDS" | awk '{print $1}' | sort -u | tr '\n' ', ' | sed 's/,$//')

    echo "Block $h | leader: ${LEADER:-(none)} | recipients: $REWARD_COUNT | addrs: $ADDRS"

    # Accumulate stats
    while IFS=' ' read -r addr amt; do
        [ -z "$addr" ] && continue
        ADDR_COUNT[$addr]=$(( ${ADDR_COUNT[$addr]:-0} + 1 ))
        ADDR_TOTAL[$addr]=$(( ${ADDR_TOTAL[$addr]:-0} + ${amt:-0} ))
    done <<< "$REWARDS"

    if [ -n "$LEADER" ] && [ "$LEADER" != "null" ] && [ "$LEADER" != "" ]; then
        BLOCKS_PRODUCED[$LEADER]=$(( ${BLOCKS_PRODUCED[$LEADER]:-0} + 1 ))
    fi
done

echo ""
echo "=== Reward Summary by Address ==="
echo ""
printf "%-45s %8s %15s\n" "ADDRESS" "BLOCKS" "TOTAL (sats)"
echo "---------------------------------------------------------------------"
for addr in "${!ADDR_COUNT[@]}"; do
    printf "%-45s %8d %15d\n" "$addr" "${ADDR_COUNT[$addr]}" "${ADDR_TOTAL[$addr]}"
done | sort -t' ' -k2 -rn

echo ""
echo "=== Blocks Produced by Leader ==="
echo ""
printf "%-45s %8s\n" "LEADER ADDRESS" "BLOCKS"
echo "------------------------------------------------------"
for addr in "${!BLOCKS_PRODUCED[@]}"; do
    printf "%-45s %8d\n" "$addr" "${BLOCKS_PRODUCED[$addr]}"
done | sort -t' ' -k2 -rn
