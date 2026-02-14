#!/bin/bash

# TIME Coin Wallet & Transaction Test Script

set -e

CLI="./target/release/time-cli"
DAEMON="./target/release/timed"

echo "🧪 TIME Coin Wallet & Transaction Test"
echo "========================================"
echo ""

# Build first
echo "📦 Building project..."
cargo build --release
echo ""

# Start daemon in background
echo "1️⃣ Starting daemon..."
$DAEMON --config config.toml > /tmp/timed.log 2>&1 &
DAEMON_PID=$!
echo "   Daemon started (PID: $DAEMON_PID)"

# Wait for startup
echo "2️⃣ Waiting 5 seconds for startup..."
sleep 5
echo ""

# Test basic info
echo "3️⃣ Testing basic commands..."
echo ""

echo "📊 Blockchain info:"
$CLI getblockchaininfo | jq '.' 2>/dev/null || echo "Failed"
echo ""

echo "🔗 Block count:"
$CLI getblockcount 2>/dev/null || echo "Failed"
echo ""

echo "🌐 Network info:"
$CLI getnetworkinfo | jq '.' 2>/dev/null || echo "Failed"
echo ""

# Test wallet commands
echo "4️⃣ Testing wallet commands..."
echo ""

echo "💰 Get balance:"
$CLI getbalance 2>/dev/null || echo "No balance yet"
echo ""

echo "📋 List unspent UTXOs:"
$CLI listunspent | jq '.' 2>/dev/null || echo "No UTXOs yet"
echo ""

echo "🔍 Validate address TIME0K8wwmqtqkdG34pdjmMqrXX85TFH7bpM3X:"
$CLI validateaddress TIME0K8wwmqtqkdG34pdjmMqrXX85TFH7bpM3X | jq '.' 2>/dev/null || echo "Failed"
echo ""

# Test masternode commands
echo "5️⃣ Testing masternode commands..."
echo ""

echo "🏛️ Masternode list:"
$CLI masternodelist | jq '.' 2>/dev/null || echo "No masternodes"
echo ""

echo "📊 Masternode status:"
$CLI masternodestatus | jq '.' 2>/dev/null || echo "Not a masternode"
echo ""

echo "⚖️ Consensus info:"
$CLI getconsensusinfo | jq '.' 2>/dev/null || echo "Failed"
echo ""

# Test mempool
echo "6️⃣ Testing mempool commands..."
echo ""

echo "📦 Mempool info:"
$CLI getmempoolinfo | jq '.' 2>/dev/null || echo "Failed"
echo ""

echo "📋 Raw mempool:"
$CLI getrawmempool | jq '.' 2>/dev/null || echo "Failed"
echo ""

# Wait for block production
echo "7️⃣ Waiting 15 seconds for potential block..."
sleep 15
echo ""

echo "🧱 Block count after wait:"
$CLI getblockcount 2>/dev/null || echo "Failed"
echo ""

echo "🔍 Get block 1 (if exists):"
$CLI getblock 1 | jq '.' 2>/dev/null || echo "Block 1 not found yet"
echo ""

# Test transaction sending
echo "8️⃣ Testing transaction creation..."
echo ""

echo "💸 Attempting to send 100 TIME to TIME0TestRecipient123456789012345:"
$CLI sendtoaddress TIME0TestRecipient123456789012345 100 2>/dev/null || echo "Transaction failed (expected if no balance)"
echo ""

# Check UTXO set
echo "9️⃣ Testing UTXO set info..."
echo ""

echo "📊 UTXO set info:"
$CLI gettxoutsetinfo | jq '.' 2>/dev/null || echo "Failed"
echo ""

# Uptime
echo "🔟 Testing uptime..."
echo ""

echo "⏱️ Daemon uptime:"
$CLI uptime 2>/dev/null || echo "Failed"
echo ""

# Stop daemon
echo "🛑 Stopping daemon..."
$CLI stop 2>/dev/null || kill $DAEMON_PID
sleep 2

echo ""
echo "✅ Tests complete!"
echo ""
echo "💡 To view daemon logs: tail -f /tmp/timed.log"
