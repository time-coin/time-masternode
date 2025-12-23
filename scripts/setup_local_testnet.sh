#!/bin/bash
# setup_local_testnet.sh - Local 3-node testnet setup for testing

set -e

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

echo "🚀 TIME Coin Local Testnet Setup"
echo "================================"

# Build release binary
echo "📦 Building release binary..."
cargo build --release 2>&1 | grep -E "Compiling|Finished|error|warning" || true

BINARY="$PROJECT_ROOT/target/release/timed"

if [ ! -f "$BINARY" ]; then
    echo "❌ Build failed - binary not found"
    exit 1
fi

echo "✅ Build complete!"

# Create test directories
echo "📁 Creating node directories..."
mkdir -p "$PROJECT_ROOT/nodes"/{node1,node2,node3}

echo "
🎯 Starting 3-Node Testnet

You need to open 3 terminals and run these commands:

Terminal 1 (Node 1):
  RUST_LOG=info $BINARY \\
    --validator-id validator1 \\
    --port 8001 \\
    --peers localhost:8002,localhost:8003 \\
    --rpc-bind 0.0.0.0:8081

Terminal 2 (Node 2):
  RUST_LOG=info $BINARY \\
    --validator-id validator2 \\
    --port 8002 \\
    --peers localhost:8001,localhost:8003 \\
    --rpc-bind 0.0.0.0:8082

Terminal 3 (Node 3):
  RUST_LOG=info $BINARY \\
    --validator-id validator3 \\
    --port 8003 \\
    --peers localhost:8001,localhost:8002 \\
    --rpc-bind 0.0.0.0:8083

Verification Commands:
  # Check block count
  curl -s http://localhost:8081/rpc -d '{\"jsonrpc\":\"2.0\",\"method\":\"getblockcount\",\"params\":[],\"id\":\"1\"}' | jq .result
  
  # Check network info
  curl -s http://localhost:8081/rpc -d '{\"jsonrpc\":\"2.0\",\"method\":\"getnetworkinfo\",\"params\":[],\"id\":\"1\"}' | jq .result
  
  # Check masternode list
  curl -s http://localhost:8081/rpc -d '{\"jsonrpc\":\"2.0\",\"method\":\"masternodelist\",\"params\":[],\"id\":\"1\"}' | jq .result

Press Ctrl+C when finished testing.
"
