#!/usr/bin/env bash
# deploy-config.sh — Create default time.conf and masternode.conf in the data directory
#
# Usage:
#   ./scripts/deploy-config.sh testnet    # Deploy testnet config
#   ./scripts/deploy-config.sh mainnet    # Deploy mainnet config
#   ./scripts/deploy-config.sh            # Defaults to mainnet

set -euo pipefail

NETWORK="${1:-mainnet}"

# Determine platform-specific data directory
if [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" || "$OSTYPE" == "win32" ]]; then
    BASE_DIR="${APPDATA}/timecoin"
elif [[ "$OSTYPE" == "darwin"* ]]; then
    BASE_DIR="$HOME/.timecoin"
else
    BASE_DIR="$HOME/.timecoin"
fi

case "$NETWORK" in
    testnet)
        DEST_DIR="$BASE_DIR/testnet"
        TESTNET_LINE="testnet=1"
        PORT="24100"
        ;;
    mainnet)
        DEST_DIR="$BASE_DIR"
        TESTNET_LINE="#testnet=0"
        PORT="24000"
        ;;
    *)
        echo "❌ Unknown network: $NETWORK"
        echo "Usage: $0 [testnet|mainnet]"
        exit 1
        ;;
esac

CONF="$DEST_DIR/time.conf"
MN_CONF="$DEST_DIR/masternode.conf"

# Create destination directory
mkdir -p "$DEST_DIR"

# Back up existing configs
TIMESTAMP=$(date +%Y%m%d%H%M%S)
[ -f "$CONF" ] && cp "$CONF" "${CONF}.bak.${TIMESTAMP}" && echo "📋 Backed up time.conf"
[ -f "$MN_CONF" ] && cp "$MN_CONF" "${MN_CONF}.bak.${TIMESTAMP}" && echo "📋 Backed up masternode.conf"

# Deploy time.conf
if [ ! -f "$CONF" ]; then
    cat > "$CONF" <<EOF
# TIME Coin Configuration File
# https://time-coin.io

# Network
${TESTNET_LINE}

listen=1
server=1

# Masternode (set to 0 to run as observer)
masternode=1

# Masternode private key (generate with: time-cli masternode genkey)
#masternodeprivkey=

# Peers
#addnode=seed1.time-coin.io

# Logging
debug=info

# Storage
txindex=1
EOF
    echo "✅ Created $NETWORK time.conf at: $CONF"
else
    echo "ℹ️  time.conf already exists: $CONF (preserved)"
fi

# Deploy masternode.conf
if [ ! -f "$MN_CONF" ]; then
    cat > "$MN_CONF" <<EOF
# TIME Coin Masternode Configuration
# Format: alias IP:port collateral_txid collateral_vout
#
# Example:
#   mn1 1.2.3.4:${PORT} abc123...def456 0
#
# Steps:
#   1. Generate key:    time-cli masternode genkey
#   2. Add to time.conf: masternodeprivkey=<key>
#   3. Send collateral: time-cli sendtoaddress <addr> 1000
#   4. Find UTXO:       time-cli listunspent
#   5. Add line below and restart timed
EOF
    echo "✅ Created masternode.conf at: $MN_CONF"
else
    echo "ℹ️  masternode.conf already exists: $MN_CONF (preserved)"
fi
