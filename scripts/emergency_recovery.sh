#!/bin/bash
# Emergency recovery script for deep fork situations
# Use this when circuit breaker triggers repeatedly and nodes can't sync

set -e

SERVERS=("LW-Michigan2" "LW-Arizona" "LW-London" "reitools" "NewYork")

echo "======================================"
echo "🚨 EMERGENCY FORK RECOVERY 🚨"
echo "======================================"
echo ""
echo "⚠️  WARNING: This will:"
echo "   1. Stop all timed daemons"
echo "   2. Backup current blockchain data"
echo "   3. Clear databases on non-seed nodes"
echo "   4. Restart with one node as trusted seed"
echo "   5. Let other nodes resync from seed"
echo ""
echo "Use this ONLY when:"
echo "  - Circuit breaker triggers repeatedly (fork > 100 blocks)"
echo "  - Nodes stuck in infinite fork loops"
echo "  - Normal sync cannot recover"
echo ""

# Choose seed node
echo "Available seed nodes:"
for i in "${!SERVERS[@]}"; do
    echo "  $((i+1)). ${SERVERS[$i]}"
done
echo ""
read -p "Choose seed node (1-${#SERVERS[@]}): " choice

if [ "$choice" -lt 1 ] || [ "$choice" -gt "${#SERVERS[@]}" ]; then
    echo "Invalid choice"
    exit 1
fi

SEED_SERVER="${SERVERS[$((choice-1))]}"
echo ""
echo "Selected seed: $SEED_SERVER"
echo ""
read -p "Continue? (type 'EMERGENCY' to confirm): " confirm

if [ "$confirm" != "EMERGENCY" ]; then
    echo "Recovery cancelled"
    exit 0
fi

echo ""
echo "======================================"
echo "Starting Emergency Recovery"
echo "======================================"

echo ""
echo "Step 1: Stopping all nodes..."
echo "------------------------------"
for server in "${SERVERS[@]}"; do
    echo -n "  Stopping $server... "
    if ssh "$server" "sudo systemctl stop timed" 2>/dev/null; then
        echo "✅"
    else
        echo "⚠️  (may already be stopped)"
    fi
done

sleep 5

echo ""
echo "Step 2: Backing up databases..."
echo "--------------------------------"
BACKUP_DATE=$(date +%Y%m%d_%H%M%S)
for server in "${SERVERS[@]}"; do
    echo -n "  Backing up $server... "
    if ssh "$server" "sudo tar -czf /root/blockchain_backup_${BACKUP_DATE}.tar.gz /root/.timecoin/testnet/db 2>/dev/null" 2>/dev/null; then
        echo "✅ (/root/blockchain_backup_${BACKUP_DATE}.tar.gz)"
    else
        echo "⚠️  Failed (continuing anyway)"
    fi
done

echo ""
echo "Step 3: Clearing databases (except seed: $SEED_SERVER)..."
echo "----------------------------------------------------------"
for server in "${SERVERS[@]}"; do
    if [ "$server" != "$SEED_SERVER" ]; then
        echo -n "  Clearing $server... "
        if ssh "$server" "sudo rm -rf /root/.timecoin/testnet/db/*" 2>/dev/null; then
            echo "✅"
        else
            echo "❌ Failed"
            exit 1
        fi
    else
        echo "  Keeping $server database (seed node)"
    fi
done

echo ""
echo "Step 4: Starting seed node ($SEED_SERVER)..."
echo "---------------------------------------------"
echo -n "  Starting $SEED_SERVER... "
if ssh "$SEED_SERVER" "sudo systemctl start timed" 2>/dev/null; then
    echo "✅"
else
    echo "❌ Failed"
    exit 1
fi

echo "  Waiting for seed node to stabilize (30 seconds)..."
sleep 30

# Check seed node status
echo -n "  Verifying seed node... "
SEED_HEIGHT=$(ssh "$SEED_SERVER" "curl -s http://127.0.0.1:24101/blockchain/info 2>/dev/null | jq -r '.height' 2>/dev/null" || echo "0")
if [ "$SEED_HEIGHT" -gt 0 ]; then
    echo "✅ (height: $SEED_HEIGHT)"
else
    echo "⚠️  Could not verify (continuing anyway)"
fi

echo ""
echo "Step 5: Starting other nodes..."
echo "--------------------------------"
for server in "${SERVERS[@]}"; do
    if [ "$server" != "$SEED_SERVER" ]; then
        echo -n "  Starting $server... "
        if ssh "$server" "sudo systemctl start timed" 2>/dev/null; then
            echo "✅"
        else
            echo "❌ Failed"
        fi
        echo "  Pausing 10 seconds before next node..."
        sleep 10
    fi
done

echo ""
echo "======================================"
echo "✅ Emergency Recovery Complete!"
echo "======================================"
echo ""
echo "Seed node: $SEED_SERVER (height: $SEED_HEIGHT)"
echo "Backups saved to: /root/blockchain_backup_${BACKUP_DATE}.tar.gz on each server"
echo ""
echo "Next Steps:"
echo "1. Monitor sync progress (wait 5-10 minutes):"
echo "   ./scripts/diagnose_fork_state.sh"
echo ""
echo "2. Watch logs on non-seed nodes:"
echo "   ssh LW-Michigan2 'journalctl -u timed -f'"
echo ""
echo "3. Verify all nodes reach same height:"
echo "   for s in ${SERVERS[@]}; do"
echo "     ssh \$s 'curl -s localhost:24101/blockchain/info | jq .height'"
echo "   done"
echo ""
echo "If sync fails again, there may be a deeper issue with the seed node's chain."
echo "Consider choosing a different seed or investigating the blockchain state."
