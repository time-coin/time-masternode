#!/bin/bash
#
# Deploy UTXO rollback fix to testnet nodes
# This script updates nodes with the fix for "UTXO already spent" during rollback
#
# The fix adds a restore_utxo() method that properly handles restoring UTXOs
# that were in SpentFinalized state during chain rollback/reorg.
#
# Usage: ./deploy_utxo_fix.sh [node_ip1] [node_ip2] ...
# If no IPs provided, will prompt for them
#

set -e

NODES=("$@")

if [ ${#NODES[@]} -eq 0 ]; then
    echo "🔧 UTXO Rollback Fix Deployment"
    echo "================================"
    echo ""
    echo "This script will:"
    echo "  1. Pull latest code with the UTXO rollback fix"
    echo "  2. Build the release binary"
    echo "  3. Stop the node"
    echo "  4. Optionally clear corrupted blockchain data"
    echo "  5. Deploy the new binary"
    echo "  6. Restart the node"
    echo ""
    echo "Enter node IPs (space-separated), or 'local' for this machine:"
    read -r input
    IFS=' ' read -ra NODES <<< "$input"
fi

deploy_to_node() {
    local node=$1
    
    if [ "$node" == "local" ] || [ "$node" == "localhost" ] || [ "$node" == "127.0.0.1" ]; then
        echo ""
        echo "🖥️  Deploying locally..."
        deploy_local
    else
        echo ""
        echo "🌐 Deploying to $node..."
        ssh "root@$node" 'bash -s' < <(cat << 'REMOTE_SCRIPT'
set -e
cd ~/timecoin || cd /opt/timecoin

echo "📥 Pulling latest code..."
git pull

echo "🔨 Building release..."
cargo build --release

echo "⏹️  Stopping timed..."
systemctl stop timed || true

# Ask about clearing data
echo ""
echo "⚠️  The node may have corrupted UTXO state from the previous bug."
echo "Do you want to clear blockchain data and resync? (recommended)"
echo "This will NOT delete your wallet."
echo -n "[y/N]: "
read -r clear_data

if [ "$clear_data" == "y" ] || [ "$clear_data" == "Y" ]; then
    echo "🗑️  Clearing corrupted data..."
    rm -rf ~/.timecoin/testnet/db 2>/dev/null || true
    rm -rf ~/.timecoin/testnet/*.sled 2>/dev/null || true
    rm -rf /var/lib/timecoin/testnet/db 2>/dev/null || true
    rm -rf /var/lib/timecoin/testnet/*.sled 2>/dev/null || true
    echo "✅ Data cleared"
fi

echo "📦 Installing new binary..."
cp target/release/timed /usr/local/bin/

echo "🚀 Starting timed..."
systemctl start timed

echo "✅ Deployment complete!"
echo ""
echo "📊 Check status with: systemctl status timed"
echo "📜 View logs with: journalctl -u timed -f"
REMOTE_SCRIPT
)
    fi
}

deploy_local() {
    cd ~/timecoin 2>/dev/null || cd /opt/timecoin 2>/dev/null || cd "$(dirname "$0")/.."
    
    echo "📥 Pulling latest code..."
    git pull
    
    echo "🔨 Building release..."
    cargo build --release
    
    echo "⏹️  Stopping timed..."
    sudo systemctl stop timed || true
    
    echo ""
    echo "⚠️  The node may have corrupted UTXO state from the previous bug."
    echo "Do you want to clear blockchain data and resync? (recommended)"
    echo "This will NOT delete your wallet."
    echo -n "[y/N]: "
    read -r clear_data
    
    if [ "$clear_data" == "y" ] || [ "$clear_data" == "Y" ]; then
        echo "🗑️  Clearing corrupted data..."
        sudo rm -rf ~/.timecoin/testnet/db 2>/dev/null || true
        sudo rm -rf ~/.timecoin/testnet/*.sled 2>/dev/null || true
        sudo rm -rf /var/lib/timecoin/testnet/db 2>/dev/null || true
        sudo rm -rf /var/lib/timecoin/testnet/*.sled 2>/dev/null || true
        echo "✅ Data cleared"
    fi
    
    echo "📦 Installing new binary..."
    sudo cp target/release/timed /usr/local/bin/
    
    echo "🚀 Starting timed..."
    sudo systemctl start timed
    
    echo "✅ Deployment complete!"
    echo ""
    echo "📊 Check status with: systemctl status timed"
    echo "📜 View logs with: journalctl -u timed -f"
}

echo "🔧 UTXO Rollback Fix Deployment"
echo "================================"
echo ""
echo "Fix summary:"
echo "  - Added restore_utxo() method for proper rollback handling"
echo "  - UTXOs in SpentFinalized state can now be restored during reorg"
echo "  - Prevents 'UTXO already spent' errors during chain rollback"
echo ""

for node in "${NODES[@]}"; do
    deploy_to_node "$node"
done

echo ""
echo "🎉 All deployments complete!"
echo ""
echo "Monitor network health:"
echo "  - Check all nodes are syncing: journalctl -u timed -f"
echo "  - Verify heights match across nodes"
echo "  - Watch for any remaining rollback errors"
