#!/bin/bash
# Quick diagnostic for your current issue

echo "=== Quick TimeCoin Diagnostic ==="
echo ""

# Try both RPC ports
echo "Testing RPC connectivity..."
echo ""

echo "Trying testnet RPC (24101):"
./target/release/time-cli --rpc-url http://127.0.0.1:24101 get-block-count 2>&1 | head -5
echo ""

echo "Trying mainnet RPC (24001):"
./target/release/time-cli --rpc-url http://127.0.0.1:24001 get-block-count 2>&1 | head -5
echo ""

echo "Checking what ports are listening:"
netstat -an | grep "LISTEN" | grep -E ":(24001|24101|24000|24100)" || echo "No TimeCoin ports found listening"
echo ""

echo "Checking process:"
ps aux | grep timed | grep -v grep
echo ""

echo "=== Analysis ==="
echo "Your issue: timed is running but RPC is not responding on expected ports"
echo ""
echo "Likely causes:"
echo "1. Node started with different config file than expected"
echo "2. RPC not enabled in your actual config"
echo "3. RPC listening on different address (not 127.0.0.1)"
echo ""
echo "Check how you started timed:"
echo "  If you ran: timed --config some-other-config.toml"
echo "  That would use different ports than config.toml specifies"
