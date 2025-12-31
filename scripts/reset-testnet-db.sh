#!/bin/bash
# Script to reset TIME Coin testnet node

echo "�� Stopping timed service..."
sudo systemctl stop timed

echo "🗑️  Removing blockchain database..."
sudo rm -rf /root/.timecoin/testnet/db

echo "✅ Database cleared!"
echo ""
echo "To restart the node:"
echo "  sudo systemctl start timed"
echo "  sudo journalctl -u timed -f"