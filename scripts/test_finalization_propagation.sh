#!/bin/bash

# Multi-Node Finalization Propagation Test
# Verifies that transaction finalization propagates to ALL nodes (Bug #4 fix)
# Usage: bash scripts/test_finalization_propagation.sh

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

log_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

log_fail() {
    echo -e "${RED}❌ $1${NC}"
}

log_warn() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

# Configuration
# Add your node IPs/hostnames here (SSH-accessible)
# Override via NODES env var: NODES="root@host1 root@host2" bash scripts/test_finalization_propagation.sh
if [ -z "${NODES+x}" ]; then
NODES=(
    "root@LW-Michigan"
    # "root@node2"
    # "root@node3"
    # "root@node4"
    # "root@node5"
    # "root@node6"
)
fi

echo "================================================================"
echo "  Multi-Node Finalization Propagation Test"
echo "  Critical Test for Bug #4 Fix"
echo "================================================================"
echo ""

# Check if nodes configured
if [ ${#NODES[@]} -lt 2 ]; then
    log_warn "Only 1 node configured in NODES array"
    log_warn "Edit this script and add more nodes to test multi-node propagation"
    log_warn "Example: NODES=(\"root@node1\" \"root@node2\" \"root@node3\")"
    echo ""
fi

# Step 1: Send transaction
log_info "Step 1: Sending transaction..."
TEST_ADDR=$(time-cli masternodelist 2>/dev/null | jq -r '.masternodes[0].wallet_address')
if [ -z "$TEST_ADDR" ] || [ "$TEST_ADDR" == "null" ]; then
    log_fail "Cannot get test address"
    exit 1
fi

TXID=$(time-cli sendtoaddress "$TEST_ADDR" 1.0 2>&1)
if ! echo "$TXID" | grep -qE '^[0-9a-f]{64}$'; then
    log_fail "Transaction failed: $TXID"
    exit 1
fi

log_success "Transaction sent: ${TXID:0:16}..."
echo ""

# Step 2: Wait for finalization on submitting node
log_info "Step 2: Waiting for finalization on submitting node..."
FINALIZED=0
for i in {1..50}; do
    if journalctl -u timed --since "10 seconds ago" 2>/dev/null | grep -q "$TXID.*finalized"; then
        log_success "Transaction finalized after ${i}00ms"
        FINALIZED=1
        break
    fi
    sleep 0.1
done

if [ $FINALIZED -eq 0 ]; then
    log_fail "Transaction did not finalize within 5 seconds"
    exit 1
fi
echo ""

# Step 3: Check if TransactionFinalized was broadcast
log_info "Step 3: Verifying TransactionFinalized broadcast..."
if journalctl -u timed --since "10 seconds ago" 2>/dev/null | grep -q "Broadcast TransactionFinalized.*$TXID"; then
    log_success "TransactionFinalized broadcast confirmed ⭐"
else
    log_fail "TransactionFinalized NOT broadcast (Bug #4 still present!)"
    exit 1
fi
echo ""

# Step 4: Check finalized pool on submitting node
log_info "Step 4: Checking finalized pool on submitting node..."
FINALIZED_COUNT=$(time-cli getmempoolinfo 2>/dev/null | jq -r '.finalized_count')
if [ "$FINALIZED_COUNT" -gt 0 ]; then
    log_success "Submitting node has $FINALIZED_COUNT transaction(s) in finalized pool"
else
    log_fail "Finalized pool empty on submitting node"
fi
echo ""

# Step 5: Check all other nodes
log_info "Step 5: Checking finalization propagation to other nodes..."
echo ""

ALL_NODES_OK=1
NODE_COUNT=0

for node in "${NODES[@]}"; do
    NODE_COUNT=$((NODE_COUNT + 1))
    echo "  Checking node: $node"
    
    # Check if node received TransactionFinalized message
    RECEIVED=$(ssh "$node" "journalctl -u timed --since '10 seconds ago' 2>/dev/null | grep 'Received TransactionFinalized' | grep '$TXID'" || echo "")
    
    if [ -n "$RECEIVED" ]; then
        log_success "    ✓ Received TransactionFinalized message"
    else
        log_fail "    ✗ Did NOT receive TransactionFinalized message"
        ALL_NODES_OK=0
        continue
    fi
    
    # Check if node actually finalized the transaction locally
    FINALIZED_LOCALLY=$(ssh "$node" "journalctl -u timed --since '10 seconds ago' 2>/dev/null | grep 'Moved TX.*to finalized pool on this node' | grep '$TXID'" || echo "")
    
    if [ -n "$FINALIZED_LOCALLY" ]; then
        log_success "    ✓ Finalized transaction locally"
    else
        log_fail "    ✗ Did NOT finalize transaction locally (Bug #4 handler issue!)"
        ALL_NODES_OK=0
        continue
    fi
    
    # Check finalized pool count
    FINALIZED_COUNT=$(ssh "$node" "time-cli getmempoolinfo 2>/dev/null | jq -r '.finalized_count'" || echo "0")
    
    if [ "$FINALIZED_COUNT" -gt 0 ]; then
        log_success "    ✓ Has $FINALIZED_COUNT transaction(s) in finalized pool"
    else
        log_fail "    ✗ Finalized pool is empty"
        ALL_NODES_OK=0
    fi
    
    echo ""
done

# Summary
echo "================================================================"
echo "  Summary"
echo "================================================================"
echo ""

if [ $ALL_NODES_OK -eq 1 ]; then
    log_success "ALL NODES VERIFIED ✓"
    echo ""
    log_success "Transaction finalization propagated correctly to all $NODE_COUNT node(s)"
    log_success "Bug #4 fix is working!"
    echo ""
    log_info "What this means:"
    echo "  ✓ TransactionFinalized message broadcast from submitter"
    echo "  ✓ All nodes received the message"
    echo "  ✓ All nodes finalized the transaction locally"
    echo "  ✓ All nodes have TX in finalized pool"
    echo "  ✓ ANY node can now include this TX in blocks"
    echo ""
    exit 0
else
    log_fail "FINALIZATION PROPAGATION FAILED"
    echo ""
    log_fail "Some nodes did not properly finalize the transaction"
    log_fail "This indicates Bug #4 may not be fully fixed"
    echo ""
    log_info "Expected behavior (after Bug #4 fix):"
    echo "  1. Node A finalizes TX and broadcasts TransactionFinalized"
    echo "  2. Nodes B-F receive message"
    echo "  3. Nodes B-F finalize TX locally (move to finalized pool)"
    echo "  4. ANY node can include TX in blocks"
    echo ""
    log_info "Debug steps:"
    echo "  1. Check logs: journalctl -u timed | grep '$TXID'"
    echo "  2. Verify version: time-cli getinfo | jq '.version'"
    echo "  3. Ensure all nodes on v1.1.0"
    echo "  4. Restart nodes if needed"
    echo ""
    exit 1
fi
