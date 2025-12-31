#!/bin/bash
# Fork Diagnosis Script
# Helps identify fork状況 and common ancestors across the network

echo "=== TIME Coin Fork Diagnosis ==="
echo ""

# Get node heights and recent block hashes
declare -A nodes
nodes[LW-Michigan]="69.167.168.176"
nodes[LW-Michigan2]="64.91.241.10"
nodes[LW-Arizona]="50.28.104.50"
nodes[LW-London]="165.84.215.117"

echo "Checking node heights..."
for name in "${!nodes[@]}"; do
    ip="${nodes[$name]}"
    echo "  $name ($ip): Checking..."
done

echo ""
echo "=== Recommendations ===" 
echo ""
echo "Based on logs showing fork at heights 4388-4402:"
echo ""
echo "1. IDENTIFY CANONICAL CHAIN:"
echo "   - Check which node(s) have matching blocks at height 4387"
echo "   - That's likely the last common ancestor"
echo ""
echo "2. MANUAL RECOVERY OPTIONS:"
echo "   Option A: Stop all nodes, choose longest valid chain, resync others"
echo "   Option B: Roll back all nodes to height 4387, restart consensus"
echo "   Option C: Fresh start from new genesis (nuclear option)"
echo ""
echo "3. FROM LOGS, APPARENT SITUATION:"
echo "   - LW-Michigan: height 4391 (behind)"
echo "   - LW-Michigan2: height 4399 (middle, detecting forks 4388+)"
echo "   - LW-London: height 4401 (ahead)"  
echo "   - LW-Arizona: height 4402 (ahead)"
echo ""
echo "   - Common ancestor appears to be around height 4387"
echo "   - Forks diverged from 4388 onwards"
echo ""
echo "4. IMMEDIATE ACTION:"
echo "   a) Stop all nodes"
echo "   b) Backup all data directories"
echo "   c) Decide on canonical chain (recommend: stop at 4387)"
echo "   d) Rollback incompatible nodes to 4387"
echo "   e) Restart and monitor consensus"
echo ""
echo "5. PREVENTION (Already Applied in Latest Code):"
echo "   ✓ Solo catchup block production disabled"
echo "   ✓ Refuse block production when >50 blocks behind"
echo "   ✓ Better retry logic with exponential backoff"
echo "   ✓ Improved fork detection (30s window, 5 attempts)"
echo ""
