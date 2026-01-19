#!/bin/bash
# Reset TimeCoin blockchain to start fresh with fixed reward code
# Run this on ALL 6 nodes

echo "🛑 Stopping timed service..."
sudo systemctl stop timed

echo "🗑️  Removing old blockchain data..."
sudo rm -rf /root/.timecoin/blockchain
sudo rm -rf /root/.timecoin/utxos
sudo rm -rf /root/.timecoin/mempool
sudo rm -rf /root/.timecoin/*.db

echo "✅ Blockchain data cleared"
echo ""
echo "🚀 Starting timed service..."
sudo systemctl start timed

echo "⏳ Waiting 5 seconds for startup..."
sleep 5

echo ""
echo "📊 Current status:"
time-cli get-block-count
time-cli get-balance

echo ""
echo "✅ Node reset complete!"
echo "⚠️  IMPORTANT: Run this script on ALL 6 nodes"
echo "⚠️  Wait for all nodes to reset before expecting sync"
