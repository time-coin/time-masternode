#!/bin/bash
# Transaction Test Script
# Tests sending 1 TIME coin to a connected masternode and verifies transaction validation

set -e  # Exit on error

# Configuration
AMOUNT="1.0"
TEST_TIMEOUT=60  # seconds to wait for confirmation

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

# Extract a connected masternode address (filter for connected=true)
RECIPIENT_ADDRESS=$(echo "$MASTERNODE_JSON" | jq -r '.masternodes[]? | select(.connected == true) | .reward_address' | head -n 1)

if [ -z "$RECIPIENT_ADDRESS" ] || [ "$RECIPIENT_ADDRESS" = "null" ]; then
    log_error "No connected masternodes found"
    log_info "Available masternodes:"
    echo "$MASTERNODE_JSON" | jq '.masternodes[]? | {address: .reward_address, connected: .connected}'
    exit 1
fi

log_success "Found connected masternode with address: $RECIPIENT_ADDRESS"
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

# Check if we have enough balance
if (( $(echo "$AVAILABLE_BALANCE < $AMOUNT" | bc -l) )); then
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
TXID=$(echo "$SEND_RESULT" | jq -r '.txid // empty')

if [ -z "$TXID" ]; then
    # Try parsing as plain string (some versions return just the txid)
    TXID=$(echo "$SEND_RESULT" | tr -d '"' | tr -d '\n' | grep -oE '[a-f0-9]{64}')
fi

if [ -z "$TXID" ]; then
    log_error "Could not extract transaction ID from response"
    echo "$SEND_RESULT"
    exit 1
fi

log_success "Transaction sent! TXID: $TXID"
echo ""

# Step 4: Verify transaction in mempool
log_info "Step 4: Verifying transaction in mempool..."
sleep 2  # Give it a moment to propagate

MEMPOOL_TX=$($CLI_CMD getrawtransaction "$TXID" true 2>&1)

if [ $? -ne 0 ]; then
    log_warning "Transaction not found in mempool (may have been confirmed quickly)"
else
    log_success "Transaction found in mempool"
    
    # Parse mempool transaction details
    TX_SIZE=$(echo "$MEMPOOL_TX" | jq -r '.size // "N/A"')
    TX_INPUTS=$(echo "$MEMPOOL_TX" | jq -r '.vin | length')
    TX_OUTPUTS=$(echo "$MEMPOOL_TX" | jq -r '.vout | length')
    
    log_info "  Size: $TX_SIZE bytes"
    log_info "  Inputs: $TX_INPUTS"
    log_info "  Outputs: $TX_OUTPUTS"
fi
echo ""

# Step 5: Wait for confirmation
log_info "Step 5: Waiting for transaction confirmation (timeout: ${TEST_TIMEOUT}s)..."
CONFIRMED=false
START_TIME=$(date +%s)

while [ $(($(date +%s) - START_TIME)) -lt $TEST_TIMEOUT ]; do
    TX_INFO=$($CLI_CMD gettransaction "$TXID" 2>&1)
    
    if [ $? -eq 0 ]; then
        CONFIRMATIONS=$(echo "$TX_INFO" | jq -r '.confirmations // 0')
        
        if [ "$CONFIRMATIONS" -gt 0 ]; then
            BLOCK_HEIGHT=$(echo "$TX_INFO" | jq -r '.blockheight // "N/A"')
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
        echo -ne "\r  Transaction not yet confirmed... (elapsed: $(($(date +%s) - START_TIME))s)"
    fi
    
    sleep 2
done

echo ""  # Newline after progress indicator

if [ "$CONFIRMED" = false ]; then
    log_warning "Transaction not confirmed within ${TEST_TIMEOUT} seconds"
    log_info "Transaction may still be pending. TXID: $TXID"
    log_info "Check status later with: $CLI_CMD gettransaction $TXID"
    exit 2
fi

echo ""

# Step 6: Final verification
log_info "Step 6: Final verification..."
FINAL_TX=$($CLI_CMD gettransaction "$TXID" 2>&1)

if [ $? -ne 0 ]; then
    log_error "Failed to retrieve final transaction details"
    echo "$FINAL_TX"
    exit 1
fi

# Parse transaction details
TX_AMOUNT=$(echo "$FINAL_TX" | jq -r '.amount // 0')
TX_FEE=$(echo "$FINAL_TX" | jq -r '.fee // 0')
TX_CONFIRMATIONS=$(echo "$FINAL_TX" | jq -r '.confirmations // 0')
TX_BLOCK=$(echo "$FINAL_TX" | jq -r '.blockheight // "N/A"')

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
echo ""

log_success "✨ Transaction test completed successfully!"
exit 0
