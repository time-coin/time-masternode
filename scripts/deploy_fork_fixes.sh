#!/bin/bash
# Deploy fork resolution fixes to all nodes

set -e

SERVERS=("LW-Michigan2" "LW-Arizona" "LW-London" "reitools" "NewYork")
BINARY_PATH="target/release/timed"

echo "======================================"
echo "Fork Resolution Fix Deployment"
echo "======================================"
echo ""

# Check if binary exists
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ Error: Binary not found at $BINARY_PATH"
    echo "Please run 'cargo build --release' first"
    exit 1
fi

echo "Binary found: $BINARY_PATH"
echo "Target servers: ${SERVERS[*]}"
echo ""
read -p "Continue with deployment? (yes/no): " confirm

if [ "$confirm" != "yes" ]; then
    echo "Deployment cancelled"
    exit 0
fi

echo ""
echo "Step 1: Uploading binary to all servers..."
echo "-------------------------------------------"
for server in "${SERVERS[@]}"; do
    echo -n "  Uploading to $server... "
    if scp -q "$BINARY_PATH" "$server:/tmp/timed" 2>/dev/null; then
        echo "✅"
    else
        echo "❌ Failed"
        echo "Error: Could not upload to $server"
        exit 1
    fi
done

echo ""
echo "Step 2: Deploying binary on each server..."
echo "-------------------------------------------"
for server in "${SERVERS[@]}"; do
    echo ""
    echo "Deploying on $server..."
    
    # Stop service
    echo -n "  Stopping timed service... "
    if ssh "$server" "sudo systemctl stop timed" 2>/dev/null; then
        echo "✅"
    else
        echo "❌ Failed (service may not be running)"
    fi
    
    # Install binary
    echo -n "  Installing new binary... "
    if ssh "$server" "sudo mv /tmp/timed /usr/local/bin/ && sudo chmod +x /usr/local/bin/timed" 2>/dev/null; then
        echo "✅"
    else
        echo "❌ Failed"
        exit 1
    fi
    
    # Start service
    echo -n "  Starting timed service... "
    if ssh "$server" "sudo systemctl start timed" 2>/dev/null; then
        echo "✅"
    else
        echo "❌ Failed"
        exit 1
    fi
    
    # Brief pause between server restarts
    sleep 5
done

echo ""
echo "======================================"
echo "Deployment Complete!"
echo "======================================"
echo ""
echo "Next Steps:"
echo "1. Monitor logs for fork activity:"
echo "   ./scripts/diagnose_fork_state.sh"
echo ""
echo "2. Watch for successful reorganizations:"
echo "   ssh LW-Michigan2 'journalctl -u timed -f | grep REORGANIZATION'"
echo ""
echo "3. Check for circuit breaker activations:"
echo "   ssh LW-Michigan2 'journalctl -u timed -f | grep \"DEEP FORK DETECTED\"'"
echo ""
echo "If problems persist, see: FORK_RESOLUTION_FIXES.md"
