#!/bin/bash
#
# TIME Coin Masternode Configuration Script
#
# Configures time.conf and masternode.conf for masternode operation.
# Usage: ./configure-masternode.sh [mainnet|testnet]
#

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

USER_HOME="${HOME:-/root}"

echo -e "${BLUE}╔════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║   TIME Coin Masternode Configuration Tool     ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════╝${NC}"
echo ""

# Network selection
if [ -n "$1" ]; then
    NETWORK_ARG=$(echo "$1" | tr '[:upper:]' '[:lower:]')
    case "$NETWORK_ARG" in
        mainnet) NETWORK="mainnet" ;;
        testnet) NETWORK="testnet" ;;
        *)
            echo -e "${RED}Error: Invalid network '$1'${NC}"
            echo "Usage: $0 [mainnet|testnet]"
            exit 1
            ;;
    esac
else
    NETWORK="mainnet"
    echo -e "${BLUE}No network specified, defaulting to mainnet${NC}"
    echo ""
fi

# Determine data directory
if [ "$NETWORK" = "testnet" ]; then
    DATA_DIR="$USER_HOME/.timecoin/testnet"
else
    DATA_DIR="$USER_HOME/.timecoin"
fi

CONF_FILE="$DATA_DIR/time.conf"
MN_CONF_FILE="$DATA_DIR/masternode.conf"

echo "Network:          $NETWORK"
echo "Data directory:   $DATA_DIR"
echo "Config file:      $CONF_FILE"
echo "Masternode conf:  $MN_CONF_FILE"
echo ""

# Create data directory if it doesn't exist
mkdir -p "$DATA_DIR"

# ─── Helpers ──────────────────────────────────────────────────
validate_yes_no() {
    case "$1" in
        y|Y|yes|Yes|YES) return 0 ;;
        n|N|no|No|NO) return 1 ;;
        *) return 2 ;;
    esac
}

validate_txid() {
    [[ "$1" =~ ^[a-fA-F0-9]{64}$ ]]
}

validate_vout() {
    [[ "$1" =~ ^[0-9]+$ ]]
}

validate_address() {
    [[ "$1" =~ ^TIME[a-zA-Z0-9]{30,}$ ]]
}

# Helper: set or update a key=value in time.conf
set_conf_value() {
    local key="$1" value="$2" file="$3"
    if grep -q "^${key}=" "$file" 2>/dev/null; then
        sed -i.tmp "s|^${key}=.*|${key}=${value}|" "$file"
        rm -f "${file}.tmp"
    elif grep -q "^#${key}=" "$file" 2>/dev/null; then
        sed -i.tmp "s|^#${key}=.*|${key}=${value}|" "$file"
        rm -f "${file}.tmp"
    else
        echo "${key}=${value}" >> "$file"
    fi
}

# ─── Backup existing configs ─────────────────────────────────
TIMESTAMP=$(date +%Y%m%d_%H%M%S)
if [ -f "$CONF_FILE" ]; then
    cp "$CONF_FILE" "${CONF_FILE}.backup.${TIMESTAMP}"
    echo -e "${GREEN}✓${NC} Backed up time.conf"
fi
if [ -f "$MN_CONF_FILE" ]; then
    cp "$MN_CONF_FILE" "${MN_CONF_FILE}.backup.${TIMESTAMP}"
    echo -e "${GREEN}✓${NC} Backed up masternode.conf"
fi
echo ""

# ─── Step 1: Enable masternode? ──────────────────────────────
echo -e "${YELLOW}Step 1: Enable Masternode${NC}"
echo "Do you want to enable masternode functionality? (y/n)"
while true; do
    read -p "> " enable_input
    if validate_yes_no "$enable_input"; then
        break
    elif [ $? -eq 1 ]; then
        echo -e "${BLUE}Masternode will be disabled.${NC}"
        # Ensure time.conf exists, set masternode=0
        [ ! -f "$CONF_FILE" ] && touch "$CONF_FILE"
        set_conf_value "masternode" "0" "$CONF_FILE"
        echo -e "${GREEN}✓${NC} Set masternode=0 in time.conf"
        exit 0
    else
        echo -e "${RED}Invalid input. Please enter 'y' or 'n'${NC}"
    fi
done

echo ""

# ─── Step 2: Select tier ─────────────────────────────────────
echo -e "${YELLOW}Step 2: Select Masternode Tier${NC}"
echo "Available tiers:"
echo "  - Free:   No collateral (basic rewards, no governance voting)"
echo "  - Bronze: 1,000 TIME collateral (10x rewards, governance voting)"
echo "  - Silver: 10,000 TIME collateral (100x rewards, governance voting)"
echo "  - Gold:   100,000 TIME collateral (1000x rewards, governance voting)"
echo ""
echo "Enter tier (free/bronze/silver/gold):"
while true; do
    read -p "> " tier_input
    tier_lower=$(echo "$tier_input" | tr '[:upper:]' '[:lower:]')
    case "$tier_lower" in
        free|bronze|silver|gold) TIER="$tier_lower"; break ;;
        *) echo -e "${RED}Invalid tier. Please enter: free, bronze, silver, or gold${NC}" ;;
    esac
done

echo ""

# ─── Step 3: Masternode private key ──────────────────────────
echo -e "${YELLOW}Step 3: Masternode Private Key${NC}"
EXISTING_KEY=""
if [ -f "$CONF_FILE" ]; then
    EXISTING_KEY=$(grep -E "^masternodeprivkey=" "$CONF_FILE" 2>/dev/null | head -1 | cut -d= -f2-)
fi
if [ -n "$EXISTING_KEY" ]; then
    echo "Existing masternodeprivkey found: ${EXISTING_KEY:0:8}..."
    echo "Keep existing key? (y/n)"
    read -p "> " keep_key
    if validate_yes_no "$keep_key"; then
        MN_PRIVKEY="$EXISTING_KEY"
    fi
fi
if [ -z "$MN_PRIVKEY" ]; then
    echo "Enter your masternode private key"
    echo "(Generate one with: time-cli masternode genkey)"
    echo "Or press Enter to skip (wallet key will be used):"
    read -p "> " MN_PRIVKEY
fi

echo ""

# ─── Step 4: Collateral information (non-free tiers) ─────────
COLLATERAL_TXID=""
COLLATERAL_VOUT=""
if [ "$TIER" != "free" ]; then
    echo -e "${YELLOW}Step 4: Collateral Information${NC}"
    echo ""
    echo "To set up collateral, provide the UTXO details:"
    echo "  1. Run: time-cli listunspent"
    echo "  2. Find the UTXO with your collateral amount"
    echo "  3. Note the txid and vout"
    echo ""

    echo "Enter collateral transaction ID (txid):"
    while true; do
        read -p "> " collateral_txid
        if [ -z "$collateral_txid" ]; then
            echo "Skip collateral for now? (y/n)"
            read -p "> " skip
            if validate_yes_no "$skip"; then break; fi
            continue
        fi
        if validate_txid "$collateral_txid"; then
            COLLATERAL_TXID="$collateral_txid"
            break
        else
            echo -e "${RED}Invalid txid (must be 64 hex characters)${NC}"
        fi
    done

    if [ -n "$COLLATERAL_TXID" ]; then
        echo "Enter collateral output index (vout):"
        while true; do
            read -p "> " collateral_vout
            if validate_vout "$collateral_vout"; then
                COLLATERAL_VOUT="$collateral_vout"
                break
            else
                echo -e "${RED}Invalid vout (must be a non-negative integer)${NC}"
            fi
        done
    fi
fi

# ─── Step 5: Public IP ───────────────────────────────────────
echo ""
echo -e "${YELLOW}Step 5: Public IP Address${NC}"
DETECTED_IP=$(curl -s --max-time 5 https://api.ipify.org 2>/dev/null || true)
if [ -n "$DETECTED_IP" ]; then
    echo "Detected public IP: $DETECTED_IP"
    echo "Use this IP? (y/n)"
    read -p "> " use_detected
    if validate_yes_no "$use_detected"; then
        PUBLIC_IP="$DETECTED_IP"
    fi
fi
if [ -z "$PUBLIC_IP" ]; then
    echo "Enter your public IP address (or press Enter for auto-detect at startup):"
    read -p "> " PUBLIC_IP
fi

# ─── Summary ─────────────────────────────────────────────────
echo ""
echo -e "${BLUE}════════════════════════════════════════════════${NC}"
echo -e "${BLUE}Configuration Summary${NC}"
echo -e "${BLUE}════════════════════════════════════════════════${NC}"
echo "Network:             $NETWORK"
echo "Masternode:          enabled"
echo "Tier:                $TIER"
[ -n "$MN_PRIVKEY" ] && echo "Private Key:         ${MN_PRIVKEY:0:8}..." || echo "Private Key:         (wallet key)"
[ -n "$PUBLIC_IP" ] && echo "Public IP:           $PUBLIC_IP" || echo "Public IP:           (auto-detect)"
if [ -n "$COLLATERAL_TXID" ]; then
    echo "Collateral TXID:     $COLLATERAL_TXID"
    echo "Collateral VOUT:     $COLLATERAL_VOUT"
elif [ "$TIER" != "free" ]; then
    echo "Collateral:          Not configured yet"
fi
echo -e "${BLUE}════════════════════════════════════════════════${NC}"
echo ""
echo "Save this configuration? (y/n)"
read -p "> " confirm
if ! validate_yes_no "$confirm"; then
    echo -e "${RED}Configuration cancelled${NC}"
    exit 1
fi

# ─── Write time.conf ─────────────────────────────────────────
echo ""
echo "Writing time.conf..."

# Create time.conf if it doesn't exist
if [ ! -f "$CONF_FILE" ]; then
    cat > "$CONF_FILE" <<EOF
# TIME Coin Configuration File
# https://time-coin.io

# Network
$([ "$NETWORK" = "testnet" ] && echo "testnet=1" || echo "#testnet=0")

listen=1
server=1

# Masternode
masternode=1

# Peers
#addnode=seed1.time-coin.io

# Logging
debug=info

# Storage
txindex=1
EOF
    echo -e "${GREEN}✓${NC} Created new time.conf"
else
    echo -e "${GREEN}✓${NC} Updating existing time.conf"
fi

set_conf_value "masternode" "1" "$CONF_FILE"

if [ -n "$MN_PRIVKEY" ]; then
    set_conf_value "masternodeprivkey" "$MN_PRIVKEY" "$CONF_FILE"
fi

if [ -n "$PUBLIC_IP" ]; then
    set_conf_value "externalip" "$PUBLIC_IP" "$CONF_FILE"
fi

echo -e "${GREEN}✓${NC} time.conf updated"

# ─── Write masternode.conf ────────────────────────────────────
echo "Writing masternode.conf..."

if [ -n "$COLLATERAL_TXID" ]; then
    # Determine port
    if [ "$NETWORK" = "testnet" ]; then
        MN_PORT="24100"
    else
        MN_PORT="24000"
    fi

    MN_IP="${PUBLIC_IP:-0.0.0.0}"
    MN_LINE="mn1 ${MN_IP}:${MN_PORT} ${COLLATERAL_TXID} ${COLLATERAL_VOUT}"

    if [ ! -f "$MN_CONF_FILE" ] || ! grep -q "^mn1 " "$MN_CONF_FILE" 2>/dev/null; then
        cat > "$MN_CONF_FILE" <<EOF
# TIME Coin Masternode Configuration
# Format: alias IP:port collateral_txid collateral_vout
$MN_LINE
EOF
    else
        sed -i.tmp "s|^mn1 .*|${MN_LINE}|" "$MN_CONF_FILE"
        rm -f "${MN_CONF_FILE}.tmp"
    fi
    echo -e "${GREEN}✓${NC} masternode.conf updated with collateral"
elif [ ! -f "$MN_CONF_FILE" ]; then
    cat > "$MN_CONF_FILE" <<EOF
# TIME Coin Masternode Configuration
# Format: alias IP:port collateral_txid collateral_vout
#
# Example:
#   mn1 1.2.3.4:24100 abc123...def456 0
#
# Add your collateral line and restart timed.
EOF
    echo -e "${GREEN}✓${NC} Created masternode.conf template"
else
    echo -e "${GREEN}✓${NC} masternode.conf unchanged (no collateral provided)"
fi

# ─── Next Steps ───────────────────────────────────────────────
echo ""
echo -e "${BLUE}Next Steps:${NC}"
echo ""

if [ "$TIER" = "free" ]; then
    echo "1. Restart your node:  systemctl restart timed"
    echo "2. Check status:       time-cli masternodestatus"
else
    if [ -z "$COLLATERAL_TXID" ]; then
        REQUIRED_AMOUNT=""
        case "$TIER" in
            bronze) REQUIRED_AMOUNT="1000" ;;
            silver) REQUIRED_AMOUNT="10000" ;;
            gold)   REQUIRED_AMOUNT="100000" ;;
        esac
        echo "1. Send exactly ${REQUIRED_AMOUNT} TIME to your wallet address"
        echo "2. Find the collateral UTXO:  time-cli listunspent"
        echo "3. Edit $MN_CONF_FILE:"
        echo "   mn1 <your_ip>:$([ "$NETWORK" = "testnet" ] && echo 24100 || echo 24000) <txid> <vout>"
        echo "4. Restart:  systemctl restart timed"
    else
        echo "1. Restart your node:  systemctl restart timed"
        echo "2. Verify:  time-cli masternodelist"
    fi
fi

echo ""
echo -e "${GREEN}✓ Configuration complete!${NC}"
