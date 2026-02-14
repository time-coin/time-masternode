#!/bin/bash

# Critical Transaction Flow Tests
# Runs the most important tests from the comprehensive test plan
# Usage: bash scripts/test_critical_flow.sh

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

PASSED=0
FAILED=0
SKIPPED=0

# Test result tracking
declare -a FAILED_TESTS

log_info() {
    echo -e "${YELLOW}ℹ️  $1${NC}"
}

log_success() {
    echo -e "${GREEN}✅ $1${NC}"
    PASSED=$((PASSED + 1))
}

log_fail() {
    echo -e "${RED}❌ $1${NC}"
    FAILED=$((FAILED + 1))
    FAILED_TESTS+=("$1")
}

log_skip() {
    echo -e "${YELLOW}⏭️  $1${NC}"
    SKIPPED=$((SKIPPED + 1))
}

# Check prerequisites
check_prerequisites() {
    log_info "Checking prerequisites..."
    
    if ! command -v time-cli &> /dev/null; then
        log_fail "time-cli not found in PATH"
        exit 1
    fi
    
    if ! command -v jq &> /dev/null; then
        log_fail "jq not found (required for JSON parsing)"
        exit 1
    fi
    
    # Check node is running
    if ! time-cli getblockcount &> /dev/null; then
        log_fail "Cannot connect to node (is timed running?)"
        exit 1
    fi
    
    # Check version (from getnetworkinfo — version is numeric, e.g. 110000 for v1.1.0)
    VERSION=$(time-cli getnetworkinfo 2>/dev/null | jq -r '.version // 0')
    if [ "$VERSION" -lt 110000 ]; then
        log_fail "Wrong version: $VERSION (expected >= 110000 / v1.1.0)"
        exit 1
    fi
    
    log_success "Prerequisites OK (version $VERSION)"
}

# Test 1: Basic Transaction Creation
test_basic_transaction() {
    log_info "Test 1: Basic Transaction Creation"
    
    # Get test address
    TEST_ADDR=$(time-cli masternodelist 2>/dev/null | jq -r '.masternodes[0].wallet_address')
    if [ -z "$TEST_ADDR" ] || [ "$TEST_ADDR" == "null" ]; then
        log_fail "Cannot get test address (no masternodes?)"
        return 1
    fi
    
    # Send transaction
    RESULT=$(time-cli sendtoaddress "$TEST_ADDR" 1.0 2>&1)
    
    # Check if TXID returned
    if echo "$RESULT" | grep -qE '^[0-9a-f]{64}$'; then
        TXID="$RESULT"
        log_success "Transaction created: ${TXID:0:16}..."
        echo "$TXID" > /tmp/test_txid.txt
        return 0
    else
        log_fail "Invalid response: $RESULT"
        return 1
    fi
}

# Test 2: Transaction Broadcast
test_transaction_broadcast() {
    log_info "Test 2: Transaction Broadcast"
    
    if [ ! -f /tmp/test_txid.txt ]; then
        log_skip "No TXID from previous test"
        return 1
    fi
    
    TXID=$(cat /tmp/test_txid.txt)
    
    # Check logs for broadcast
    sleep 1
    if journalctl -u timed --since "10 seconds ago" 2>/dev/null | grep -q "Received new transaction.*$TXID"; then
        log_success "Transaction broadcast confirmed"
        return 0
    else
        log_fail "No broadcast confirmation in logs"
        return 1
    fi
}

# Test 3: TimeVote Request Sent
test_timevote_request() {
    log_info "Test 3: TimeVote Request Sent"
    
    if [ ! -f /tmp/test_txid.txt ]; then
        log_skip "No TXID from previous test"
        return 1
    fi
    
    TXID=$(cat /tmp/test_txid.txt)
    
    # Check logs for TimeVote request
    if journalctl -u timed --since "10 seconds ago" 2>/dev/null | grep -q "Broadcasting TimeVoteRequest.*$TXID"; then
        log_success "TimeVote request sent"
        return 0
    elif journalctl -u timed --since "10 seconds ago" 2>/dev/null | grep -q "No broadcast callback"; then
        log_fail "Broadcast callback not wired (BUG #2)"
        return 1
    else
        log_fail "No TimeVote request found"
        return 1
    fi
}

# Test 4: Transaction Finalization
test_transaction_finalization() {
    log_info "Test 4: Transaction Finalization"
    
    if [ ! -f /tmp/test_txid.txt ]; then
        log_skip "No TXID from previous test"
        return 1
    fi
    
    TXID=$(cat /tmp/test_txid.txt)
    
    # Wait for finalization (up to 5 seconds)
    for i in {1..50}; do
        if journalctl -u timed --since "10 seconds ago" 2>/dev/null | grep -q "$TXID.*finalized"; then
            log_success "Transaction finalized after ${i}00ms"
            return 0
        fi
        sleep 0.1
    done
    
    log_fail "Transaction did not finalize within 5 seconds"
    return 1
}

# Test 5: TransactionFinalized Broadcast (BUG #4 FIX)
test_finalization_broadcast() {
    log_info "Test 5: TransactionFinalized Broadcast ⭐ CRITICAL"
    
    if [ ! -f /tmp/test_txid.txt ]; then
        log_skip "No TXID from previous test"
        return 1
    fi
    
    TXID=$(cat /tmp/test_txid.txt)
    
    # Check for broadcast message
    if journalctl -u timed --since "10 seconds ago" 2>/dev/null | grep -q "Broadcast TransactionFinalized.*$TXID"; then
        log_success "TransactionFinalized broadcast sent (BUG #4 FIXED)"
        return 0
    else
        log_fail "TransactionFinalized NOT broadcast (BUG #4 STILL PRESENT)"
        return 1
    fi
}

# Test 6: Finalized Pool Check
test_finalized_pool() {
    log_info "Test 6: Finalized Pool Check"
    
    if [ ! -f /tmp/test_txid.txt ]; then
        log_skip "No TXID from previous test"
        return 1
    fi
    
    TXID=$(cat /tmp/test_txid.txt)
    
    # Check finalized pool
    FINALIZED_COUNT=$(time-cli getmempoolinfo 2>/dev/null | jq -r '.finalized_count')
    
    if [ "$FINALIZED_COUNT" -gt 0 ]; then
        log_success "Finalized pool has $FINALIZED_COUNT transaction(s)"
        return 0
    else
        log_fail "Finalized pool is empty (TX may have been prematurely cleared - BUG #1)"
        return 1
    fi
}

# Test 7: Block Inclusion
test_block_inclusion() {
    log_info "Test 7: Block Inclusion (waiting up to 70 seconds...)"
    
    if [ ! -f /tmp/test_txid.txt ]; then
        log_skip "No TXID from previous test"
        return 1
    fi
    
    TXID=$(cat /tmp/test_txid.txt)
    START_HEIGHT=$(time-cli getblockcount 2>/dev/null)
    
    # Wait for up to 70 seconds (1 block + buffer)
    for i in {1..70}; do
        # Try to get transaction
        if time-cli gettransaction "$TXID" 2>/dev/null | jq -e '.blockheight' &>/dev/null; then
            BLOCK_HEIGHT=$(time-cli gettransaction "$TXID" 2>/dev/null | jq -r '.blockheight')
            log_success "Transaction included in block $BLOCK_HEIGHT after ${i}s"
            return 0
        fi
        
        # Show progress every 10 seconds
        if [ $((i % 10)) -eq 0 ]; then
            CURRENT_HEIGHT=$(time-cli getblockcount 2>/dev/null)
            log_info "  Waiting... (block $CURRENT_HEIGHT, ${i}s elapsed)"
        fi
        
        sleep 1
    done
    
    log_fail "Transaction not included in block within 70 seconds"
    return 1
}

# Test 8: Block Fee Calculation (BUG #3 FIX)
test_block_fees() {
    log_info "Test 8: Block Fee Calculation ⭐ CRITICAL"
    
    # Check recent logs for fee errors
    if journalctl -u timed --since "2 minutes ago" 2>/dev/null | grep -q "incorrect block_reward"; then
        log_fail "Block reward mismatch detected (BUG #3 STILL PRESENT)"
        return 1
    else
        log_success "No block reward errors (BUG #3 FIXED)"
        return 0
    fi
}

# Test 9: Finalized Pool Cleanup (BUG #1 FIX)
test_finalized_cleanup() {
    log_info "Test 9: Finalized Pool Cleanup ⭐ CRITICAL"
    
    if [ ! -f /tmp/test_txid.txt ]; then
        log_skip "No TXID from previous test"
        return 1
    fi
    
    TXID=$(cat /tmp/test_txid.txt)
    
    # If TX was included in block, check if cleared from finalized pool
    if time-cli gettransaction "$TXID" 2>/dev/null | jq -e '.blockheight' &>/dev/null; then
        # TX is in blockchain, should be cleared from finalized pool
        # Check logs for selective clearing
        if journalctl -u timed --since "2 minutes ago" 2>/dev/null | grep -q "Clearing.*finalized transaction"; then
            log_success "Finalized pool cleaned selectively (BUG #1 FIXED)"
            return 0
        else
            log_fail "No finalized pool cleanup logged"
            return 1
        fi
    else
        log_skip "Transaction not yet in blockchain"
        return 1
    fi
}

# Test 10: UTXO State Transitions
test_utxo_states() {
    log_info "Test 10: UTXO State Transitions"
    
    if [ ! -f /tmp/test_txid.txt ]; then
        log_skip "No TXID from previous test"
        return 1
    fi
    
    TXID=$(cat /tmp/test_txid.txt)
    
    # Check if UTXOs were properly updated
    # After block inclusion, input UTXOs should be removed
    if time-cli gettransaction "$TXID" 2>/dev/null | jq -e '.blockheight' &>/dev/null; then
        # TX in blockchain - verify output UTXOs exist
        OUTPUTS=$(time-cli getrawtransaction "$TXID" 2>/dev/null | jq -r '.outputs | length')
        if [ "$OUTPUTS" -gt 0 ]; then
            log_success "Transaction has $OUTPUTS output UTXO(s)"
            return 0
        else
            log_fail "No output UTXOs found"
            return 1
        fi
    else
        log_skip "Transaction not yet in blockchain"
        return 1
    fi
}

# Main test execution
main() {
    echo "================================================================"
    echo "  TimeCoin v1.1.0 - Critical Transaction Flow Tests"
    echo "================================================================"
    echo ""
    
    check_prerequisites
    echo ""
    
    log_info "Starting test sequence..."
    echo ""
    
    # Run tests in sequence
    test_basic_transaction
    echo ""
    
    test_transaction_broadcast
    echo ""
    
    test_timevote_request
    echo ""
    
    test_transaction_finalization
    echo ""
    
    test_finalization_broadcast
    echo ""
    
    test_finalized_pool
    echo ""
    
    test_block_inclusion
    echo ""
    
    test_block_fees
    echo ""
    
    test_finalized_cleanup
    echo ""
    
    test_utxo_states
    echo ""
    
    # Summary
    echo "================================================================"
    echo "  Test Summary"
    echo "================================================================"
    echo -e "${GREEN}Passed:  $PASSED${NC}"
    echo -e "${RED}Failed:  $FAILED${NC}"
    echo -e "${YELLOW}Skipped: $SKIPPED${NC}"
    echo ""
    
    if [ $FAILED -gt 0 ]; then
        echo -e "${RED}Failed Tests:${NC}"
        for test in "${FAILED_TESTS[@]}"; do
            echo "  - $test"
        done
        echo ""
        exit 1
    else
        echo -e "${GREEN}✅ All tests passed!${NC}"
        echo ""
        exit 0
    fi
}

# Cleanup on exit
cleanup() {
    rm -f /tmp/test_txid.txt
}
trap cleanup EXIT

# Run main
main "$@"
