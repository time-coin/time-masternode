#!/bin/bash
# setup_local_testnet.sh - Local 3-node testnet setup for testing
#
# Creates 3 separate data directories with their own time.conf,
# each running on different ports so they can peer with each other.

set -e

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$PROJECT_ROOT"

echo "🚀 TIME Coin Local Testnet Setup"
echo "================================"

# Build release binary
echo "📦 Building release binary..."
cargo build --release 2>&1 | grep -E "Compiling|Finished|error|warning" || true

BINARY="$PROJECT_ROOT/target/release/timed"
CLI="$PROJECT_ROOT/target/release/time-cli"

if [ ! -f "$BINARY" ]; then
    echo "❌ Build failed - binary not found"
    exit 1
fi

echo "✅ Build complete!"

# Create test directories with config files
echo "📁 Creating node directories and configs..."
for i in 1 2 3; do
    NODE_DIR="$PROJECT_ROOT/nodes/node${i}"
    mkdir -p "$NODE_DIR"

    P2P_PORT=$((24100 + i * 10))      # 24110, 24120, 24130
    RPC_PORT=$((24100 + i * 10 + 1))   # 24111, 24121, 24131

    # Build addnode lines for the other two nodes
    ADDNODES=""
    for j in 1 2 3; do
        [ "$j" -eq "$i" ] && continue
        ADDNODES="${ADDNODES}addnode=127.0.0.1:$((24100 + j * 10))\n"
    done

    cat > "$NODE_DIR/time.conf" <<EOF
testnet=1
listen=1
server=1
port=${P2P_PORT}
rpcport=${RPC_PORT}
$(echo -e "$ADDNODES")debug=info
EOF

    echo "  Node $i: P2P=$P2P_PORT  RPC=$RPC_PORT  datadir=$NODE_DIR"
done

echo ""
echo "🎯 Starting 3-Node Testnet"
echo ""
echo "Open 3 terminals and run these commands:"
echo ""

for i in 1 2 3; do
    P2P_PORT=$((24100 + i * 10))
    RPC_PORT=$((24100 + i * 10 + 1))
    echo "Terminal $i (Node $i — P2P $P2P_PORT, RPC $RPC_PORT):"
    echo "  RUST_LOG=info $BINARY --conf $PROJECT_ROOT/nodes/node${i}/time.conf --datadir $PROJECT_ROOT/nodes/node${i}"
    echo ""
done

echo "Verification Commands:"
echo "  # Check block count (node 1)"
echo "  $CLI -r http://127.0.0.1:24111 getblockcount"
echo ""
echo "  # Check network info (node 2)"
echo "  $CLI -r http://127.0.0.1:24121 getnetworkinfo"
echo ""
echo "  # Check masternode list (node 3)"
echo "  $CLI -r http://127.0.0.1:24131 masternodelist"
echo ""
echo "Press Ctrl+C when finished testing."
