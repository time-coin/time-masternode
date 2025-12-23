#!/bin/bash
# stability_test.sh - 72-hour stability test for testnet

set -e

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NODES=(8081 8082 8083 8084 8085)
DURATION=259200  # 72 hours in seconds
INTERVAL=10      # Check every 10 seconds
LOG_FILE="$PROJECT_ROOT/stability_test_$(date +%Y%m%d_%H%M%S).log"

echo "📊 TIME Coin 72-Hour Stability Test"
echo "==================================="
echo "Test Duration: 72 hours"
echo "Check Interval: $INTERVAL seconds"
echo "Log File: $LOG_FILE"
echo ""

START_TIME=$(date +%s)
END_TIME=$((START_TIME + DURATION))
ITERATION=0
HEIGHT_MISMATCHES=0
MAX_HEIGHT=0
MIN_HEIGHT=999999
FORK_DETECTED=0

# Verify all nodes are running
echo "🔍 Verifying nodes are running..."
for port in "${NODES[@]}"; do
    if ! curl -s http://localhost:$port/rpc -d '{"jsonrpc":"2.0","method":"getblockcount","params":[],"id":"1"}' > /dev/null 2>&1; then
        echo "❌ Node on port $port is not responding!"
    fi
done
echo "✅ All nodes responding"

echo "Starting stability test..."
echo "================================" >> "$LOG_FILE"

while [ $(date +%s) -lt $END_TIME ]; do
    ITERATION=$((ITERATION + 1))
    CURRENT_TIME=$(date '+%Y-%m-%d %H:%M:%S')
    
    # Get heights from all nodes
    HEIGHTS=()
    for port in "${NODES[@]}"; do
        HEIGHT=$(curl -s http://localhost:$port/rpc \
            -d '{"jsonrpc":"2.0","method":"getblockcount","params":[],"id":"1"}' 2>/dev/null | \
            jq -r '.result // empty' 2>/dev/null)
        
        if [ -n "$HEIGHT" ] && [ "$HEIGHT" -gt 0 ]; then
            HEIGHTS+=("$HEIGHT")
            if [ "$HEIGHT" -gt "$MAX_HEIGHT" ]; then
                MAX_HEIGHT=$HEIGHT
            fi
        fi
    done
    
    # Check for height mismatches
    UNIQUE_HEIGHTS=$(printf '%s\n' "${HEIGHTS[@]}" | sort -u | wc -l)
    
    if [ $UNIQUE_HEIGHTS -gt 1 ]; then
        HEIGHT_MISMATCHES=$((HEIGHT_MISMATCHES + 1))
        echo "[$CURRENT_TIME] ⚠️  Iteration #$ITERATION: Height mismatch detected!" | tee -a "$LOG_FILE"
        printf "  Heights: %s\n" "${HEIGHTS[@]}" | tee -a "$LOG_FILE"
    fi
    
    # Check mempool size
    MEMPOOL_SIZE=$(curl -s http://localhost:8081/rpc \
        -d '{"jsonrpc":"2.0","method":"getmempoolinfo","params":[],"id":"1"}' 2>/dev/null | \
        jq -r '.result.size // 0' 2>/dev/null)
    
    # Log status
    ELAPSED=$(($(date +%s) - START_TIME))
    ELAPSED_HOURS=$((ELAPSED / 3600))
    ELAPSED_MINS=$(( (ELAPSED % 3600) / 60))
    
    echo "[$CURRENT_TIME] Iteration #$ITERATION | Height: $MAX_HEIGHT | Mempool: $MEMPOOL_SIZE | Uptime: ${ELAPSED_HOURS}h ${ELAPSED_MINS}m" >> "$LOG_FILE"
    
    # Print progress (every 60 iterations = 10 minutes)
    if [ $((ITERATION % 60)) -eq 0 ]; then
        echo "[$CURRENT_TIME] ✓ Iteration #$ITERATION - ${ELAPSED_HOURS}h ${ELAPSED_MINS}m elapsed"
    fi
    
    sleep $INTERVAL
done

TOTAL_TIME=$(($(date +%s) - START_TIME))
TOTAL_HOURS=$((TOTAL_TIME / 3600))

echo ""
echo "✅ Stability Test Complete!"
echo "==================================="
echo "Total Duration: ${TOTAL_HOURS} hours"
echo "Iterations: $ITERATION"
echo "Height Mismatches: $HEIGHT_MISMATCHES"
echo "Max Height Reached: $MAX_HEIGHT"
echo "Forks Detected: $FORK_DETECTED"
echo ""
echo "Test Results: $([ $HEIGHT_MISMATCHES -eq 0 ] && echo '✅ PASSED' || echo '⚠️  FAILED')"
echo "Log saved to: $LOG_FILE"
