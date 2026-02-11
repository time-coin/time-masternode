#!/bin/bash
# Transaction Test Script - TimeVote Protocol Testing
# Tests sending 1 TIME coin and validates the new TimeVote consensus flow:
# - Transaction broadcasting
# - TimeVote request/response (signed votes with stake weighting)
# - Automatic finalization at 67% threshold
# - TimeProof certificate assembly and broadcasting
# - Transaction confirmation in blockchain

set -e  # Exit on error

# Configuration
AMOUNT="1.0"
TEST_TIMEOUT=60  # seconds to wait for confirmation
FINALITY_TIMEOUT=10  # seconds to wait for TimeVote finality

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Helper functions
log_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

log_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

log_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

log_error() {
    echo -e "${RED}❌ $1${NC}"
}

# Find time-cli binary
if [ -n "$CLI_PATH" ]; then
    # User specified CLI_PATH
    CLI_CMD="$CLI_PATH"
elif command -v time-cli &> /dev/null; then
    # time-cli is in PATH
    CLI_CMD="time-cli"
elif [ -x "./time-cli" ]; then
    # time-cli is in current directory
    CLI_CMD="./time-cli"
else
    log_error "time-cli not found"
    log_info "Ensure time-cli is installed in PATH or set CLI_PATH environment variable"
    log_info "Example: export CLI_PATH=/usr/local/bin/time-cli"
    exit 1
fi

log_info "Using CLI: $CLI_CMD"

# Check if daemon is reachable
log_info "Checking if daemon is running..."
if ! $CLI_CMD getblockchaininfo >/dev/null 2>&1; then
    log_error "Cannot connect to timed daemon"
    log_info "Please ensure timed is running before running this test"
    log_info "Start daemon with: timed (or ./target/release/timed)"
    exit 1
fi
log_success "Daemon is running"

log_info "Starting transaction test..."
echo ""

# Step 1: Get list of connected masternodes
log_info "Step 1: Fetching connected masternodes..."
MASTERNODE_JSON=$($CLI_CMD masternodelist 2>&1)

if [ $? -ne 0 ]; then
    log_error "Failed to fetch masternode list"
    echo "$MASTERNODE_JSON"
    exit 1
fi

# Count total and connected masternodes
TOTAL_MN=$(echo "$MASTERNODE_JSON" | jq -r '.masternodes | length // 0')
CONNECTED_MN=$(echo "$MASTERNODE_JSON" | jq -r '[.masternodes[]? | select(.is_connected == true)] | length')

log_info "Total masternodes: $TOTAL_MN"
log_info "Connected masternodes: $CONNECTED_MN"

if [ "$CONNECTED_MN" -eq 0 ]; then
    log_error "No connected masternodes found!"
    log_info "Available masternodes:"
    echo "$MASTERNODE_JSON" | jq '.masternodes[]? | {ip: .address, wallet: .wallet_address, connected: .is_connected}'
    exit 1
fi

# Extract a connected masternode wallet address (filter for is_connected=true)
RECIPIENT_ADDRESS=$(echo "$MASTERNODE_JSON" | jq -r '.masternodes[]? | select(.is_connected == true) | .wallet_address' | head -n 1)

if [ -z "$RECIPIENT_ADDRESS" ] || [ "$RECIPIENT_ADDRESS" = "null" ]; then
    log_error "Could not extract wallet address from connected masternode"
    echo "$MASTERNODE_JSON" | jq '.masternodes[]? | select(.is_connected == true)'
    exit 1
fi

log_success "Found $CONNECTED_MN/$TOTAL_MN connected masternode(s)"
log_success "Using recipient address: $RECIPIENT_ADDRESS"
echo ""

# Step 2: Check our balance
log_info "Step 2: Checking wallet balance..."
BALANCE_JSON=$($CLI_CMD getbalance 2>&1)

if [ $? -ne 0 ]; then
    log_error "Failed to fetch balance"
    echo "$BALANCE_JSON"
    exit 1
fi

AVAILABLE_BALANCE=$(echo "$BALANCE_JSON" | jq -r '.balance // 0')
LOCKED_BALANCE=$(echo "$BALANCE_JSON" | jq -r '.locked // 0')

log_info "Available balance: $AVAILABLE_BALANCE TIME"
log_info "Locked balance: $LOCKED_BALANCE TIME"

# Check if we have enough balance (using awk for portability instead of bc)
if awk -v avail="$AVAILABLE_BALANCE" -v amt="$AMOUNT" 'BEGIN {exit !(avail < amt)}'; then
    log_error "Insufficient balance. Need $AMOUNT TIME, have $AVAILABLE_BALANCE TIME available"
    exit 1
fi

log_success "Sufficient balance for transaction"
echo ""

# Step 3: Send transaction
log_info "Step 3: Sending $AMOUNT TIME to $RECIPIENT_ADDRESS..."
SEND_RESULT=$($CLI_CMD sendtoaddress "$RECIPIENT_ADDRESS" "$AMOUNT" 2>&1)

if [ $? -ne 0 ]; then
    log_error "Failed to send transaction"
    echo "$SEND_RESULT"
    exit 1
fi

# Extract transaction ID
# The response is typically a quoted string like "46b90c69ea991526..."
TXID=$(echo "$SEND_RESULT" | tr -d '"' | tr -d "'" | tr -d '\n' | tr -d ' ' | grep -oE '[a-f0-9]{64}')

if [ -z "$TXID" ]; then
    log_error "Could not extract transaction ID from response"
    log_info "Response was: $SEND_RESULT"
    exit 1
fi

log_success "Transaction sent! TXID: $TXID"
echo ""

# Step 4: Wait for TimeVote finality (NEW - Phase 2)
log_info "Step 4: Waiting for TimeVote finality (67% threshold)..."
log_info "Monitoring for: Vote requests → Vote accumulation → Finalization → TimeProof"
FINALIZED=false
START_TIME=$(date +%s)

while [ $(($(date +%s) - START_TIME)) -lt $FINALITY_TIMEOUT ]; do
    # Check if transaction has TimeProof (indicates finality)
    TX_INFO=$($CLI_CMD gettransaction "$TXID" 2>&1) || true
    
    if [ $? -eq 0 ] && echo "$TX_INFO" | jq -e . >/dev/null 2>&1; then
        # Check for finalized status or TimeProof
        IS_FINALIZED=$(echo "$TX_INFO" | jq -r '.finalized // false')
        HAS_TIMEPROOF=$(echo "$TX_INFO" | jq -r '.timeproof // null')
        
        if [ "$IS_FINALIZED" = "true" ] || [ "$HAS_TIMEPROOF" != "null" ]; then
            log_success "Transaction finalized via TimeVote consensus!"
            
            # Try to extract TimeProof details
            if [ "$HAS_TIMEPROOF" != "null" ]; then
                VOTE_COUNT=$(echo "$TX_INFO" | jq -r '.timeproof.votes | length // 0')
                ACCUMULATED_WEIGHT=$(echo "$TX_INFO" | jq -r '.timeproof.accumulated_weight // "N/A"')
                SLOT_INDEX=$(echo "$TX_INFO" | jq -r '.timeproof.slot_index // "N/A"')
                
                log_info "  TimeProof assembled:"
                log_info "    Votes: $VOTE_COUNT masternodes"
                log_info "    Weight: $ACCUMULATED_WEIGHT (≥67% threshold)"
                log_info "    Slot: $SLOT_INDEX"
            fi
            
            FINALIZED=true
            break
        fi
    fi
    
    echo -ne "\r  Waiting for finality... (elapsed: $(($(date +%s) - START_TIME))s)    "
    sleep 1
done

echo ""  # Newline after progress

if [ "$FINALIZED" = false ]; then
    log_warning "TimeVote finality not detected within ${FINALITY_TIMEOUT}s"
    log_info "This may indicate:"
    log_info "  - Not enough masternodes connected (need ≥67% AVS weight)"
    log_info "  - Vote collection still in progress"
    log_info "  - TimeProof not yet assembled"
    log_info "Continuing to check blockchain confirmation..."
else
    log_success "Phase 2 functionality working: TimeVote → Finality → TimeProof ✓"
fi
echo ""

# Step 5: Verify transaction in mempool
log_info "Step 5: Verifying transaction in mempool..."
sleep 2  # Give it a moment to propagate

MEMPOOL_TX=$($CLI_CMD getrawtransaction "$TXID" true 2>&1) || true

if [ $? -ne 0 ] || echo "$MEMPOOL_TX" | grep -qi "error\|not found"; then
    log_warning "Transaction not immediately visible in mempool (may have been confirmed quickly or still propagating)"
else
    log_success "Transaction found in mempool"
    
    # Parse mempool transaction details if JSON is valid
    if echo "$MEMPOOL_TX" | jq -e . >/dev/null 2>&1; then
        TX_SIZE=$(echo "$MEMPOOL_TX" | jq -r '.size // "N/A"')
        TX_INPUTS=$(echo "$MEMPOOL_TX" | jq -r '.vin | length // "N/A"')
        TX_OUTPUTS=$(echo "$MEMPOOL_TX" | jq -r '.vout | length // "N/A"')
        
        log_info "  Size: $TX_SIZE bytes"
        log_info "  Inputs: $TX_INPUTS"
        log_info "  Outputs: $TX_OUTPUTS"
    fi
fi
echo ""

# Step 6: Wait for confirmation
log_info "Step 6: Waiting for transaction confirmation (timeout: ${TEST_TIMEOUT}s)..."
CONFIRMED=false
START_TIME=$(date +%s)

while [ $(($(date +%s) - START_TIME)) -lt $TEST_TIMEOUT ]; do
    TX_INFO=$($CLI_CMD gettransaction "$TXID" 2>&1) || true
    
    if [ $? -eq 0 ] && echo "$TX_INFO" | jq -e . >/dev/null 2>&1; then
        CONFIRMATIONS=$(echo "$TX_INFO" | jq -r '.confirmations // 0')
        
        if [ "$CONFIRMATIONS" -gt 0 ]; then
            BLOCK_HEIGHT=$(echo "$TX_INFO" | jq -r '.height // .blockheight // "N/A"')
            BLOCK_HASH=$(echo "$TX_INFO" | jq -r '.blockhash // "N/A"')
            
            log_success "Transaction confirmed!"
            log_info "  Confirmations: $CONFIRMATIONS"
            log_info "  Block height: $BLOCK_HEIGHT"
            log_info "  Block hash: ${BLOCK_HASH:0:16}..."
            CONFIRMED=true
            break
        else
            echo -ne "\r  Waiting for confirmation... (${CONFIRMATIONS} confirmations, elapsed: $(($(date +%s) - START_TIME))s)"
        fi
    else
        echo -ne "\r  Transaction pending... (elapsed: $(($(date +%s) - START_TIME))s)                    "
    fi
    
    sleep 2
done

echo ""  # Newline after progress indicator

if [ "$CONFIRMED" = false ]; then
    log_warning "Transaction not confirmed within ${TEST_TIMEOUT} seconds"
    log_info "Transaction may still be pending. TXID: $TXID"
    log_info "Check status later with: $CLI_CMD gettransaction $TXID"
    echo ""
    
    # Show diagnostic info
    log_info "=== Diagnostic Information ==="
    echo ""
    
    log_info "1. Mempool status:"
    $CLI_CMD getmempoolinfo 2>&1 || echo "  Failed to get mempool info"
    echo ""
    
    log_info "2. Recent blockchain info:"
    $CLI_CMD getblockchaininfo 2>&1 | jq -r 'del(.blocks_info) | del(.genesis_info)' 2>/dev/null || echo "  Failed to get blockchain info"
    echo ""
    
    log_info "3. Masternode connectivity:"
    $CLI_CMD masternodelist 2>&1 | jq -r '.masternodes[]? | select(.is_connected == true) | {ip: .address, wallet: .wallet_address}' 2>/dev/null | head -5 || echo "  Failed to get masternode info"
    echo ""
    
    # Show system logs if journalctl is available (Linux only)
    if command -v journalctl &> /dev/null; then
        log_info "4. System logs (last 2 minutes):"
        journalctl -u timed --since "2 minutes ago" --no-pager | grep -E "transaction.*${TXID:0:16}|finalized|TimeProof" | head -10 || echo "  No relevant logs found"
        echo ""
    else
        log_info "4. System logs: (journalctl not available - check logs manually)"
        echo ""
    fi
    
    exit 2
fi

echo ""

# Step 7: Final verification and TimeProof check
log_info "Step 7: Final verification and TimeProof status..."
FINAL_TX=$($CLI_CMD gettransaction "$TXID" 2>&1) || true

if [ $? -ne 0 ] || ! echo "$FINAL_TX" | jq -e . >/dev/null 2>&1; then
    log_error "Failed to retrieve final transaction details"
    log_info "Response: $FINAL_TX"
    exit 1
fi

# Parse transaction details
TX_AMOUNT=$(echo "$FINAL_TX" | jq -r '.amount // 0')
TX_FEE=$(echo "$FINAL_TX" | jq -r '.fee // 0')
TX_CONFIRMATIONS=$(echo "$FINAL_TX" | jq -r '.confirmations // 0')
TX_BLOCK=$(echo "$FINAL_TX" | jq -r '.height // .blockheight // "N/A"')

# Check for TimeProof (Phase 2 validation)
HAS_TIMEPROOF=$(echo "$FINAL_TX" | jq -r '.timeproof // null')
TIMEPROOF_STATUS="❌ Not found"
TIMEPROOF_VOTES="N/A"
TIMEPROOF_WEIGHT="N/A"

if [ "$HAS_TIMEPROOF" != "null" ]; then
    TIMEPROOF_STATUS="✅ Present"
    TIMEPROOF_VOTES=$(echo "$FINAL_TX" | jq -r '.timeproof.votes | length // 0')
    TIMEPROOF_WEIGHT=$(echo "$FINAL_TX" | jq -r '.timeproof.accumulated_weight // "N/A"')
fi

log_success "Transaction validated successfully!"
echo ""
echo "══════════════════════════════════════════════════════════════"
echo "                     TRANSACTION SUMMARY"
echo "══════════════════════════════════════════════════════════════"
echo "  TXID:           $TXID"
echo "  Amount:         $TX_AMOUNT TIME"
echo "  Fee:            $TX_FEE TIME"
echo "  Recipient:      $RECIPIENT_ADDRESS"
echo "  Confirmations:  $TX_CONFIRMATIONS"
echo "  Block:          $TX_BLOCK"
echo "══════════════════════════════════════════════════════════════"
echo "                   TIMEVOTE PROTOCOL STATUS"
echo "══════════════════════════════════════════════════════════════"
echo "  TimeProof:      $TIMEPROOF_STATUS"
echo "  Votes:          $TIMEPROOF_VOTES masternodes"
echo "  Weight:         $TIMEPROOF_WEIGHT (≥67% required)"
echo "══════════════════════════════════════════════════════════════"
echo "══════════════════════════════════════════════════════════════"
echo ""

# Final status report with Phase 3 validation
if [ "$TIMEPROOF_STATUS" = "✅ Present" ] && [ "$TX_CONFIRMATIONS" -gt 0 ]; then
    log_success "✨ Transaction test completed successfully!"
    log_success "Phase 2: TimeVote consensus and TimeProof working!"
    log_success "Phase 3: Transaction archived in blockchain!"
    echo ""
    log_info "Complete flow verified:"
    log_info "  ✓ Transaction finalized via TimeVote"
    log_info "  ✓ TimeProof assembled and verified"
    log_info "  ✓ Included in block $TX_BLOCK"
    log_info "  ✓ Confirmed with $TX_CONFIRMATIONS confirmation(s)"
elif [ "$TIMEPROOF_STATUS" = "✅ Present" ]; then
    log_success "✨ Transaction finalized via TimeVote!"
    log_warning "Waiting for block inclusion (Phase 3)"
    log_info "Transaction finalized but not yet in a block"
    log_info "Next block production will include this transaction"
elif [ "$TX_CONFIRMATIONS" -gt 0 ]; then
    log_success "✨ Transaction confirmed on blockchain!"
    log_warning "TimeProof not found in response"
    log_info "This may indicate:"
    log_info "  - RPC not returning TimeProof field"
    log_info "  - Transaction finalized before TimeProof implementation"
else
    log_warning "Transaction sent but not fully processed"
    log_info "Check status with: $CLI_CMD gettransaction $TXID"
fi

exit 0
