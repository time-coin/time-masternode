#!/bin/bash
# Check block hash at height 1723 across nodes

echo "Checking block 1723 hash on all nodes..."
echo "=========================================="

# Arizona
echo -n "Arizona (50.28.104.50): "
curl -s --max-time 5 http://50.28.104.50:24101 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockhash","params":[1723],"id":1}' 2>/dev/null | grep -o '"result":"[^"]*"' || echo "TIMEOUT"

# London  
echo -n "London (165.84.215.117): "
curl -s --max-time 5 http://165.84.215.117:24101 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockhash","params":[1723],"id":1}' 2>/dev/null | grep -o '"result":"[^"]*"' || echo "TIMEOUT"

# Michigan
echo -n "Michigan (69.167.168.176): "
curl -s --max-time 5 http://69.167.168.176:24101 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockhash","params":[1723],"id":1}' 2>/dev/null | grep -o '"result":"[^"]*"' || echo "TIMEOUT"

# Michigan2
echo -n "Michigan2 (64.91.241.10): "
curl -s --max-time 5 http://64.91.241.10:24101 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockhash","params":[1723],"id":1}' 2>/dev/null | grep -o '"result":"[^"]*"' || echo "TIMEOUT"

echo ""
echo "Also check block 1722 to find common ancestor:"
echo "================================================"

# Check 1722
echo -n "Arizona (1722): "
curl -s --max-time 5 http://50.28.104.50:24101 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockhash","params":[1722],"id":1}' 2>/dev/null | grep -o '"result":"[^"]*"' || echo "TIMEOUT"

echo -n "Michigan2 (1722): "
curl -s --max-time 5 http://64.91.241.10:24101 -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"getblockhash","params":[1722],"id":1}' 2>/dev/null | grep -o '"result":"[^"]*"' || echo "TIMEOUT"
