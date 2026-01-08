#!/bin/bash
# Fork State Diagnostic Script
# Checks current state of all nodes to identify fork loops

echo "======================================"
echo "Fork Resolution Diagnostic Tool"
echo "======================================"
echo ""

SERVERS=("LW-Michigan2" "LW-Arizona" "LW-London" "reitools" "NewYork")

echo "1. Checking Current Heights and Tip Hashes"
echo "-------------------------------------------"
for server in "${SERVERS[@]}"; do
    echo -n "$server: "
    ssh $server "curl -s http://127.0.0.1:24101/blockchain/info 2>/dev/null | jq -r '\"Height: \(.height) | Tip: \(.tip_hash[0:16])\"' 2>/dev/null" || echo "OFFLINE or API unavailable"
done

echo ""
echo "2. Checking Fork Loop Activity (last 5 minutes)"
echo "------------------------------------------------"
for server in "${SERVERS[@]}"; do
    echo -n "$server: "
    fork_count=$(ssh $server "journalctl -u timed --since '5 minutes ago' 2>/dev/null | grep -c 'Fork detected'" 2>/dev/null || echo "0")
    
    if [ "$fork_count" -gt 50 ]; then
        echo "🚨 STUCK IN LOOP ($fork_count fork messages) 🚨"
    elif [ "$fork_count" -gt 10 ]; then
        echo "⚠️  High activity ($fork_count fork messages)"
    elif [ "$fork_count" -gt 0 ]; then
        echo "✅ Normal activity ($fork_count fork messages)"
    else
        echo "✅ No forks detected"
    fi
done

echo ""
echo "3. Checking for Deep Fork Detection"
echo "------------------------------------"
for server in "${SERVERS[@]}"; do
    echo -n "$server: "
    deep_fork=$(ssh $server "journalctl -u timed --since '30 minutes ago' 2>/dev/null | grep -c 'DEEP FORK DETECTED'" 2>/dev/null || echo "0")
    
    if [ "$deep_fork" -gt 0 ]; then
        echo "🚨 Deep fork detected ($deep_fork times) - Circuit breaker active"
    else
        echo "✅ No deep forks"
    fi
done

echo ""
echo "4. Checking for Successful Reorganizations (last 30 min)"
echo "---------------------------------------------------------"
for server in "${SERVERS[@]}"; do
    echo -n "$server: "
    reorgs=$(ssh $server "journalctl -u timed --since '30 minutes ago' 2>/dev/null | grep -c 'REORGANIZATION SUCCESSFUL'" 2>/dev/null || echo "0")
    
    if [ "$reorgs" -gt 0 ]; then
        echo "✅ $reorgs successful reorganization(s)"
    else
        echo "No reorganizations"
    fi
done

echo ""
echo "5. Checking Fork Resolution Attempts"
echo "-------------------------------------"
for server in "${SERVERS[@]}"; do
    echo -n "$server: "
    attempts=$(ssh $server "journalctl -u timed --since '10 minutes ago' 2>/dev/null | grep 'Fork resolution attempt' | tail -1" 2>/dev/null || echo "")
    
    if [ -n "$attempts" ]; then
        echo "$attempts" | sed 's/.*Fork resolution attempt/Attempt/'
    else
        echo "No active fork resolution"
    fi
done

echo ""
echo "======================================"
echo "Analysis & Recommendations"
echo "======================================"

# Count stuck nodes
stuck_count=0
for server in "${SERVERS[@]}"; do
    fork_count=$(ssh $server "journalctl -u timed --since '5 minutes ago' 2>/dev/null | grep -c 'Fork detected'" 2>/dev/null || echo "0")
    if [ "$fork_count" -gt 50 ]; then
        stuck_count=$((stuck_count + 1))
    fi
done

if [ "$stuck_count" -gt 0 ]; then
    echo "🚨 ALERT: $stuck_count node(s) appear stuck in fork loops"
    echo ""
    echo "Recommended Actions:"
    echo "1. Deploy the new binary with circuit breaker fixes"
    echo "2. If already deployed, consider emergency recovery:"
    echo "   - Stop all nodes"
    echo "   - Choose one as seed (highest height, most connections)"
    echo "   - Wipe databases on other nodes"
    echo "   - Restart seed, then others"
    echo ""
else
    echo "✅ No nodes stuck in fork loops"
    
    # Check for any deep forks
    deep_fork_nodes=0
    for server in "${SERVERS[@]}"; do
        deep_fork=$(ssh $server "journalctl -u timed --since '30 minutes ago' 2>/dev/null | grep -c 'DEEP FORK DETECTED'" 2>/dev/null || echo "0")
        if [ "$deep_fork" -gt 0 ]; then
            deep_fork_nodes=$((deep_fork_nodes + 1))
        fi
    done
    
    if [ "$deep_fork_nodes" -gt 0 ]; then
        echo "⚠️  Warning: $deep_fork_nodes node(s) detected deep forks"
        echo "   This indicates fork > 100 blocks. Circuit breaker activated."
        echo "   Manual recovery may be needed - see FORK_RESOLUTION_FIXES.md"
    else
        echo "✅ Network appears healthy"
    fi
fi

echo ""
echo "For detailed recovery instructions, see: FORK_RESOLUTION_FIXES.md"
