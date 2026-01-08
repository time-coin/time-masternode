#!/bin/bash
# Local Node State Diagnostic
# Run this script on each node individually to verify chain state

echo "=========================================="
echo "TIME Coin Node State Diagnostic"
echo "=========================================="
echo ""
echo "Node: $(hostname)"
echo "Timestamp: $(date)"
echo ""

# Check if timed is running
echo "1. Service Status"
echo "-------------------"
if systemctl is-active --quiet timed 2>/dev/null; then
    echo "✅ timed service is running"
    TIMED_PID=$(systemctl show -p MainPID --value timed)
    echo "   PID: $TIMED_PID"
    
    # Check memory/CPU usage
    if [ -n "$TIMED_PID" ] && [ "$TIMED_PID" != "0" ]; then
        ps_info=$(ps -p $TIMED_PID -o %cpu,%mem,vsz,rss --no-headers 2>/dev/null)
        if [ -n "$ps_info" ]; then
            echo "   CPU/MEM: $ps_info"
        fi
    fi
else
    echo "❌ timed service is NOT running"
fi

echo ""
echo "2. Blockchain State"
echo "-------------------"

# Try to get blockchain info via CLI
if [ -f "./target/release/time-cli" ]; then
    CLI="./target/release/time-cli"
elif [ -f "/usr/local/bin/time-cli" ]; then
    CLI="/usr/local/bin/time-cli"
elif command -v time-cli &> /dev/null; then
    CLI="time-cli"
else
    echo "⚠️  time-cli not found, trying direct RPC call..."
    CLI=""
fi

if [ -n "$CLI" ]; then
    # Get blockchain info
    INFO=$($CLI get-blockchain-info 2>/dev/null)
    if [ $? -eq 0 ]; then
        HEIGHT=$(echo "$INFO" | jq -r '.height' 2>/dev/null)
        TIP_HASH=$(echo "$INFO" | jq -r '.tip_hash' 2>/dev/null)
        CHAIN=$(echo "$INFO" | jq -r '.chain' 2>/dev/null)
        
        echo "Chain: $CHAIN"
        echo "Height: $HEIGHT"
        echo "Tip Hash: ${TIP_HASH:0:16}..."
    else
        echo "⚠️  Failed to get blockchain info via CLI"
    fi
else
    # Fallback to direct RPC call
    RPC_RESPONSE=$(curl -s -X POST http://127.0.0.1:24101 \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"getblockchaininfo","params":[],"id":1}' 2>/dev/null)
    
    if [ $? -eq 0 ] && [ -n "$RPC_RESPONSE" ]; then
        HEIGHT=$(echo "$RPC_RESPONSE" | jq -r '.result.height' 2>/dev/null)
        TIP_HASH=$(echo "$RPC_RESPONSE" | jq -r '.result.tip_hash' 2>/dev/null)
        CHAIN=$(echo "$RPC_RESPONSE" | jq -r '.result.chain' 2>/dev/null)
        
        echo "Chain: $CHAIN"
        echo "Height: $HEIGHT"
        echo "Tip Hash: ${TIP_HASH:0:16}..."
    else
        echo "❌ Cannot connect to RPC (is timed running?)"
    fi
fi

echo ""
echo "3. Recent Fork Activity (last 10 minutes)"
echo "------------------------------------------"
FORK_COUNT=$(journalctl -u timed --since '10 minutes ago' 2>/dev/null | grep -c 'Fork detected' || echo "0")
REORG_COUNT=$(journalctl -u timed --since '10 minutes ago' 2>/dev/null | grep -c 'REORGANIZATION SUCCESSFUL' || echo "0")
REORG_FAIL=$(journalctl -u timed --since '10 minutes ago' 2>/dev/null | grep -c 'REORGANIZATION FAILED' || echo "0")

if [ "$FORK_COUNT" -gt 100 ]; then
    echo "🚨 ALERT: $FORK_COUNT fork detections (STUCK IN LOOP!)"
elif [ "$FORK_COUNT" -gt 20 ]; then
    echo "⚠️  High fork activity: $FORK_COUNT detections"
elif [ "$FORK_COUNT" -gt 0 ]; then
    echo "Fork detections: $FORK_COUNT"
else
    echo "✅ No forks detected"
fi

if [ "$REORG_COUNT" -gt 0 ]; then
    echo "✅ Successful reorganizations: $REORG_COUNT"
fi

if [ "$REORG_FAIL" -gt 0 ]; then
    echo "❌ Failed reorganizations: $REORG_FAIL"
fi

# Check for deep fork detection
DEEP_FORK=$(journalctl -u timed --since '30 minutes ago' 2>/dev/null | grep -c 'DEEP FORK DETECTED' || echo "0")
if [ "$DEEP_FORK" -gt 0 ]; then
    echo "🚨 Deep fork detected ($DEEP_FORK times) - Circuit breaker active"
fi

echo ""
echo "4. Network Connectivity"
echo "-----------------------"
if [ -n "$CLI" ]; then
    PEER_INFO=$($CLI get-peer-info 2>/dev/null)
    if [ $? -eq 0 ]; then
        PEER_COUNT=$(echo "$PEER_INFO" | jq '. | length' 2>/dev/null || echo "0")
        echo "Connected peers: $PEER_COUNT"
        
        if [ "$PEER_COUNT" -gt 0 ]; then
            echo ""
            echo "Peer heights:"
            echo "$PEER_INFO" | jq -r '.[] | "  \(.addr) - Height: \(.height // "unknown")"' 2>/dev/null
        fi
    else
        echo "⚠️  Could not get peer info"
    fi
fi

echo ""
echo "5. Recent Log Messages (last 20 lines)"
echo "---------------------------------------"
journalctl -u timed -n 20 --no-pager 2>/dev/null | grep -E "(Fork|Reorg|ERROR|WARN|height|block)" || echo "No recent relevant logs"

echo ""
echo "=========================================="
echo "Summary"
echo "=========================================="

# Determine health status
if [ "$FORK_COUNT" -gt 100 ]; then
    echo "🚨 STATUS: CRITICAL - Node stuck in fork loop"
    echo ""
    echo "Action Required:"
    echo "1. Deploy the fork resolution fix (commit 5b876f0)"
    echo "2. Restart timed service: sudo systemctl restart timed"
    echo "3. Monitor logs: sudo journalctl -u timed -f"
elif [ "$REORG_FAIL" -gt 0 ]; then
    echo "⚠️  STATUS: WARNING - Reorganizations failing"
    echo ""
    echo "This is the bug we just fixed. Deploy the fix:"
    echo "1. git pull"
    echo "2. cargo build --release"
    echo "3. sudo systemctl restart timed"
elif [ "$FORK_COUNT" -gt 10 ]; then
    echo "⚠️  STATUS: ATTENTION - Active fork resolution in progress"
    echo ""
    echo "Monitor to see if it resolves automatically"
else
    echo "✅ STATUS: HEALTHY - Node operating normally"
fi

echo ""
echo "=========================================="
