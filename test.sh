#!/bin/bash
# Test script for TIME Coin node

echo "🧪 Testing TIME Coin Node"
echo "=========================="
echo ""

# Start daemon in background
echo "1️⃣ Starting daemon..."
./target/release/timed &
DAEMON_PID=$!
echo "   Daemon started (PID: $DAEMON_PID)"

# Wait for startup
echo "2️⃣ Waiting 3 seconds for startup..."
sleep 3

# Test CLI commands
echo ""
echo "3️⃣ Testing CLI commands..."
echo ""

echo "📊 Get blockchain info:"
./target/release/time-cli get-blockchain-info
echo ""

echo "🔗 Get block count:"
./target/release/time-cli get-block-count
echo ""

echo "🏛️ List masternodes:"
./target/release/time-cli masternode-list
echo ""

echo "⚡ Get consensus info:"
./target/release/time-cli get-consensus-info
echo ""

echo "⏱️ Get uptime:"
./target/release/time-cli uptime
echo ""

echo "🌐 Get network info:"
./target/release/time-cli get-network-info
echo ""

# Stop daemon
echo "4️⃣ Stopping daemon..."
kill $DAEMON_PID 2>/dev/null
wait $DAEMON_PID 2>/dev/null

echo ""
echo "✅ Tests complete!"
