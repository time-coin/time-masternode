#!/usr/bin/env bash
# deploy-config.sh — Copy the appropriate config template to the runtime data directory
#
# Usage:
#   ./scripts/deploy-config.sh testnet    # Deploy testnet config
#   ./scripts/deploy-config.sh mainnet    # Deploy mainnet config
#   ./scripts/deploy-config.sh            # Defaults to testnet

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

NETWORK="${1:-testnet}"

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
        SOURCE="$REPO_ROOT/config.testnet.toml"
        DEST_DIR="$BASE_DIR/testnet"
        ;;
    mainnet)
        SOURCE="$REPO_ROOT/config.mainnet.toml"
        DEST_DIR="$BASE_DIR"
        ;;
    *)
        echo "❌ Unknown network: $NETWORK"
        echo "Usage: $0 [testnet|mainnet]"
        exit 1
        ;;
esac

DEST="$DEST_DIR/config.toml"

if [ ! -f "$SOURCE" ]; then
    echo "❌ Source config not found: $SOURCE"
    exit 1
fi

# Create destination directory
mkdir -p "$DEST_DIR"

# Back up existing config if present
if [ -f "$DEST" ]; then
    BACKUP="$DEST.bak.$(date +%Y%m%d%H%M%S)"
    cp "$DEST" "$BACKUP"
    echo "📋 Backed up existing config to: $BACKUP"
fi

# Copy config
cp "$SOURCE" "$DEST"
echo "✅ Deployed $NETWORK config to: $DEST"
