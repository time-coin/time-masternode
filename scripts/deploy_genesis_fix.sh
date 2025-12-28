#!/bin/bash
# Deploy Genesis Canonical Fix to Testnet Nodes
# Run this on each testnet node

set -e

echo "🔧 Deploying Genesis Canonical Fix"
echo "=================================="

# Stop the daemon
echo "⏸️  Stopping timed..."
sudo systemctl stop timed || true

# Clear the blockchain database (removes old diverged genesis)
echo "🗑️  Clearing blockchain database..."
rm -rf ~/.timecoin/testnet/db

# Update code
echo "📥 Pulling latest code..."
cd ~/timecoin
git pull

# Build
echo "🔨 Building..."
cargo build --release

# Install binary
echo "📦 Installing binary..."
sudo cp target/release/timed /usr/local/bin/

# Verify genesis file exists
echo "✅ Verifying genesis file..."
if [ ! -f "genesis.testnet.json" ]; then
    echo "❌ ERROR: genesis.testnet.json not found!"
    echo "   Please ensure you're in the timecoin repository directory"
    exit 1
fi

echo "✓ Genesis file found: $(pwd)/genesis.testnet.json"

# Show genesis hash for verification
echo ""
echo "📋 Genesis Block Info:"
echo "   Masternodes: 4"
echo "   Leader: 50.28.104.50"
echo "   Reward: 100 TIME (25 each)"

# Start daemon
echo ""
echo "▶️  Starting timed..."
sudo systemctl start timed

echo ""
echo "✅ Deployment complete!"
echo ""
echo "📊 To verify genesis loaded correctly:"
echo "   sudo journalctl -u timed -f | grep -i genesis"
echo ""
echo "⏳ Wait 30 seconds, then check height:"
echo "   curl -s http://localhost:24101/blockchain/status | jq '.height'"
echo ""
