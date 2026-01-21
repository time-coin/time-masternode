#!/bin/bash
#
# TIME Coin Masternode Configuration Script
# 
# This script helps you configure your masternode settings in config.toml
# It prompts for all necessary information and updates the configuration file.
#

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Determine user's home directory
if [ -n "$HOME" ]; then
    USER_HOME="$HOME"
else
    USER_HOME="$HOME"
fi

echo -e "${BLUE}╔════════════════════════════════════════════════╗${NC}"
echo -e "${BLUE}║   TIME Coin Masternode Configuration Tool     ║${NC}"
echo -e "${BLUE}╚════════════════════════════════════════════════╝${NC}"
echo ""

# Check for command-line argument
if [ -n "$1" ]; then
    NETWORK_ARG=$(echo "$1" | tr '[:upper:]' '[:lower:]')
    case "$NETWORK_ARG" in
        mainnet)
            CONFIG_FILE="$USER_HOME/.timecoin/config.toml"
            NETWORK="mainnet"
            ;;
        testnet)
            CONFIG_FILE="$USER_HOME/.timecoin/testnet/config.toml"
            NETWORK="testnet"
            ;;
        *)
            echo -e "${RED}Error: Invalid network '$1'${NC}"
            echo "Usage: $0 [mainnet|testnet]"
            echo ""
            echo "Examples:"
            echo "  $0 mainnet    # Configure mainnet"
            echo "  $0 testnet    # Configure testnet"
            echo "  $0            # Defaults to mainnet"
            exit 1
            ;;
    esac
else
    # Default to mainnet if no argument provided
    CONFIG_FILE="$USER_HOME/.timecoin/config.toml"
    NETWORK="mainnet"
    echo -e "${BLUE}No network specified, defaulting to mainnet${NC}"
    echo ""
fi

# Check if config file exists
if [ ! -f "$CONFIG_FILE" ]; then
    echo -e "${RED}Error: Config file not found: $CONFIG_FILE${NC}"
    echo ""
    echo "Possible reasons:"
    echo "  1. Node has not been run yet (run 'timed' first to create config)"
    echo "  2. Config is in a different location"
    echo ""
    echo "Would you like to specify a custom config file path? (y/n)"
    read -p "> " custom_path
    if [[ "$custom_path" =~ ^[yY]$ ]]; then
        echo "Enter full path to config.toml:"
        read -p "> " CONFIG_FILE
        if [ ! -f "$CONFIG_FILE" ]; then
            echo -e "${RED}Error: File not found: $CONFIG_FILE${NC}"
            exit 1
        fi
    else
        exit 1
    fi
fi

echo ""
echo "This script will help you configure your masternode settings."
echo "Network: $NETWORK"
echo "Configuration file: $CONFIG_FILE"
echo ""

# Function to validate yes/no input
validate_yes_no() {
    local input="$1"
    case "$input" in
        y|Y|yes|Yes|YES) return 0 ;;
        n|N|no|No|NO) return 1 ;;
        *) return 2 ;;
    esac
}

# Function to validate tier
validate_tier() {
    local tier="$1"
    case "$tier" in
        free|Free|FREE) echo "free"; return 0 ;;
        bronze|Bronze|BRONZE) echo "bronze"; return 0 ;;
        silver|Silver|SILVER) echo "silver"; return 0 ;;
        gold|Gold|GOLD) echo "gold"; return 0 ;;
        *) return 1 ;;
    esac
}

# Function to validate txid (64 hex characters)
validate_txid() {
    local txid="$1"
    if [[ "$txid" =~ ^[a-fA-F0-9]{64}$ ]]; then
        return 0
    else
        return 1
    fi
}

# Function to validate vout (non-negative integer)
validate_vout() {
    local vout="$1"
    if [[ "$vout" =~ ^[0-9]+$ ]]; then
        return 0
    else
        return 1
    fi
}

# Function to validate TIME address
validate_address() {
    local addr="$1"
    # TIME addresses start with "TIME" followed by base58 characters
    if [[ "$addr" =~ ^TIME[a-zA-Z0-9]{30,}$ ]]; then
        return 0
    else
        return 1
    fi
}

# Backup existing config
BACKUP_FILE="${CONFIG_FILE}.backup.$(date +%Y%m%d_%H%M%S)"
cp "$CONFIG_FILE" "$BACKUP_FILE"
echo -e "${GREEN}✓${NC} Created backup: $BACKUP_FILE"
echo ""

# Step 1: Enable masternode?
echo -e "${YELLOW}Step 1: Enable Masternode${NC}"
echo "Do you want to enable masternode functionality? (y/n)"
while true; do
    read -p "> " enable_input
    if validate_yes_no "$enable_input"; then
        MASTERNODE_ENABLED="true"
        break
    elif [ $? -eq 1 ]; then
        MASTERNODE_ENABLED="false"
        break
    else
        echo -e "${RED}Invalid input. Please enter 'y' or 'n'${NC}"
    fi
done

if [ "$MASTERNODE_ENABLED" = "false" ]; then
    echo -e "${BLUE}Masternode will be disabled.${NC}"
    # Update config to disable masternode
    sed -i.tmp "s/^enabled = .*/enabled = false/" "$CONFIG_FILE"
    rm -f "${CONFIG_FILE}.tmp"
    echo -e "${GREEN}✓${NC} Configuration updated successfully!"
    exit 0
fi

echo ""

# Step 2: Select tier
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
    if TIER=$(validate_tier "$tier_input"); then
        break
    else
        echo -e "${RED}Invalid tier. Please enter: free, bronze, silver, or gold${NC}"
    fi
done

echo ""

# Step 3: Get reward address
echo -e "${YELLOW}Step 3: Reward Address${NC}"
echo "Enter your TIME address where you want to receive rewards:"
echo "(Must start with 'TIME' - example: TIME1abc...)"
while true; do
    read -p "> " reward_address
    if [ -z "$reward_address" ]; then
        echo -e "${RED}Reward address cannot be empty${NC}"
        continue
    fi
    if validate_address "$reward_address"; then
        REWARD_ADDRESS="$reward_address"
        break
    else
        echo -e "${YELLOW}Warning: Address format looks incorrect (should start with TIME)${NC}"
        echo "Continue anyway? (y/n)"
        read -p "> " continue_anyway
        if validate_yes_no "$continue_anyway"; then
            REWARD_ADDRESS="$reward_address"
            break
        fi
    fi
done

echo ""

# Step 4: Collateral information (only if not free tier)
if [ "$TIER" != "free" ]; then
    echo -e "${YELLOW}Step 4: Collateral Information${NC}"
    echo ""
    echo "To lock collateral, you need to provide the UTXO details:"
    echo "  1. Run: time-cli listunspent"
    echo "  2. Find the UTXO with your collateral amount"
    echo "  3. Note the txid and vout"
    echo ""
    
    # Get collateral txid
    echo "Enter collateral transaction ID (txid):"
    echo "(64 hex characters - example: abc123def456...)"
    while true; do
        read -p "> " collateral_txid
        if [ -z "$collateral_txid" ]; then
            echo -e "${YELLOW}You can leave this empty and configure later${NC}"
            echo "Continue without collateral txid? (y/n)"
            read -p "> " skip_collateral
            if validate_yes_no "$skip_collateral"; then
                COLLATERAL_TXID=""
                COLLATERAL_VOUT=""
                break 2
            fi
            continue
        fi
        if validate_txid "$collateral_txid"; then
            COLLATERAL_TXID="$collateral_txid"
            break
        else
            echo -e "${RED}Invalid txid format (must be 64 hex characters)${NC}"
        fi
    done
    
    # Get collateral vout (only if txid provided)
    if [ -n "$COLLATERAL_TXID" ]; then
        echo ""
        echo "Enter collateral output index (vout):"
        echo "(Usually 0 or 1 - check listunspent output)"
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
else
    COLLATERAL_TXID=""
    COLLATERAL_VOUT=""
fi

echo ""
echo -e "${BLUE}════════════════════════════════════════════════${NC}"
echo -e "${BLUE}Configuration Summary${NC}"
echo -e "${BLUE}════════════════════════════════════════════════${NC}"
echo "Masternode Enabled:  $MASTERNODE_ENABLED"
echo "Tier:                $TIER"
echo "Reward Address:      $REWARD_ADDRESS"
if [ -n "$COLLATERAL_TXID" ]; then
    echo "Collateral TXID:     $COLLATERAL_TXID"
    echo "Collateral VOUT:     $COLLATERAL_VOUT"
else
    echo "Collateral:          Not configured (can register later via CLI)"
fi
echo -e "${BLUE}════════════════════════════════════════════════${NC}"
echo ""
echo "Save this configuration? (y/n)"
read -p "> " confirm
if ! validate_yes_no "$confirm"; then
    echo -e "${RED}Configuration cancelled${NC}"
    echo "Backup preserved at: $BACKUP_FILE"
    exit 1
fi

# Update config.toml
echo ""
echo "Updating config.toml..."

# Use sed to update the [masternode] section
# Note: This assumes the [masternode] section exists in config.toml

# Update enabled
sed -i.tmp "/^\[masternode\]/,/^\[/ s/^enabled = .*/enabled = $MASTERNODE_ENABLED/" "$CONFIG_FILE"

# Update tier
sed -i.tmp "/^\[masternode\]/,/^\[/ s/^tier = .*/tier = \"$TIER\"/" "$CONFIG_FILE"

# Update reward_address (add if doesn't exist)
if grep -q "^reward_address = " "$CONFIG_FILE"; then
    sed -i.tmp "/^\[masternode\]/,/^\[/ s|^reward_address = .*|reward_address = \"$REWARD_ADDRESS\"|" "$CONFIG_FILE"
else
    # Add reward_address after tier line
    sed -i.tmp "/^\[masternode\]/,/^\[/ s|^tier = .*|&\nreward_address = \"$REWARD_ADDRESS\"|" "$CONFIG_FILE"
fi

# Update collateral_txid
if [ -n "$COLLATERAL_TXID" ]; then
    sed -i.tmp "/^\[masternode\]/,/^\[/ s/^collateral_txid = .*/collateral_txid = \"$COLLATERAL_TXID\"/" "$CONFIG_FILE"
else
    sed -i.tmp "/^\[masternode\]/,/^\[/ s/^collateral_txid = .*/collateral_txid = \"\"/" "$CONFIG_FILE"
fi

# Update collateral_vout (add if doesn't exist)
if [ -n "$COLLATERAL_VOUT" ]; then
    if grep -q "^collateral_vout = " "$CONFIG_FILE"; then
        sed -i.tmp "/^\[masternode\]/,/^\[/ s/^collateral_vout = .*/collateral_vout = $COLLATERAL_VOUT/" "$CONFIG_FILE"
    else
        # Add collateral_vout after collateral_txid line
        sed -i.tmp "/^\[masternode\]/,/^\[/ s|^collateral_txid = .*|&\ncollateral_vout = $COLLATERAL_VOUT|" "$CONFIG_FILE"
    fi
fi

# Clean up temporary files
rm -f "${CONFIG_FILE}.tmp"

echo -e "${GREEN}✓${NC} Configuration saved successfully!"
echo ""
echo -e "${BLUE}Next Steps:${NC}"
echo ""

if [ "$TIER" = "free" ]; then
    echo "1. Restart your node to apply changes"
    echo "   ./target/release/timed"
    echo ""
    echo "2. Check masternode status"
    echo "   time-cli masternodestatus"
else
    if [ -z "$COLLATERAL_TXID" ]; then
        echo "1. Create collateral UTXO:"
        echo "   time-cli sendtoaddress $REWARD_ADDRESS <amount>"
        echo ""
        REQUIRED_AMOUNT=""
        case "$TIER" in
            bronze) REQUIRED_AMOUNT="1000.0" ;;
            silver) REQUIRED_AMOUNT="10000.0" ;;
            gold) REQUIRED_AMOUNT="100000.0" ;;
        esac
        if [ -n "$REQUIRED_AMOUNT" ]; then
            echo "   Required amount: $REQUIRED_AMOUNT TIME"
            echo ""
        fi
        echo "2. Wait for 3 confirmations (~30 minutes)"
        echo "   time-cli listunspent"
        echo ""
        echo "3. Register masternode with collateral:"
        echo "   time-cli masternoderegister \\"
        echo "     --tier $TIER \\"
        echo "     --collateral-txid <txid> \\"
        echo "     --vout <vout> \\"
        echo "     --reward-address $REWARD_ADDRESS"
        echo ""
        echo "4. Verify registration:"
        echo "   time-cli masternodelist"
        echo "   time-cli listlockedcollaterals"
    else
        echo "1. Restart your node to apply changes"
        echo "   ./target/release/timed"
        echo ""
        echo "2. Register masternode with collateral:"
        echo "   time-cli masternoderegister \\"
        echo "     --tier $TIER \\"
        echo "     --collateral-txid $COLLATERAL_TXID \\"
        echo "     --vout $COLLATERAL_VOUT \\"
        echo "     --reward-address $REWARD_ADDRESS"
        echo ""
        echo "3. Verify registration:"
        echo "   time-cli masternodelist"
        echo "   time-cli listlockedcollaterals"
    fi
fi

echo ""
echo -e "${GREEN}Configuration complete!${NC}"
echo "Backup saved at: $BACKUP_FILE"
