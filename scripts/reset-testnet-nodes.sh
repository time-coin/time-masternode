#!/bin/bash
#
# Reset all testnet nodes to sync from genesis
# Run this on EACH node to clear old data and start fresh
#

echo "🔄 Resetting testnet node..."

# Stop the daemon
echo "⏹️  Stopping timed..."
sudo systemctl stop timed

# Clear blockchain database
echo "🗑️  Removing old blockchain data..."
sudo rm -rf ~/.timecoin/testnet/db
sudo rm -rf ~/.timecoin/testnet/blockchain.db
sudo rm -rf ~/.timecoin/testnet/*.sled

# Keep wallet and config
echo "✅ Wallet and config preserved"

# Restart daemon
echo "🚀 Starting timed..."
sudo systemctl start timed

echo "✅ Node reset complete!"
echo "📊 Check logs: sudo journalctl -u timed -f"
