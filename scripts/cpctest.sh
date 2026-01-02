#!/bin/bash

# Script to copy config.toml from project directory to testnet data directory
# Usage: ./scripts/cpctest.sh

set -e

SOURCE_DIR="/root/timecoin"
DEST_DIR="/root/.timecoin/testnet"
CONFIG_FILE="config.toml"

echo "📋 Copying testnet configuration..."
echo "   Source: $SOURCE_DIR/$CONFIG_FILE"
echo "   Destination: $DEST_DIR/$CONFIG_FILE"

# Check if source file exists
if [ ! -f "$SOURCE_DIR/$CONFIG_FILE" ]; then
    echo "❌ Error: Source config file not found at $SOURCE_DIR/$CONFIG_FILE"
    exit 1
fi

# Create destination directory if it doesn't exist
if [ ! -d "$DEST_DIR" ]; then
    echo "📁 Creating directory: $DEST_DIR"
    mkdir -p "$DEST_DIR"
fi

# Backup existing config if it exists
if [ -f "$DEST_DIR/$CONFIG_FILE" ]; then
    BACKUP_FILE="$DEST_DIR/${CONFIG_FILE}.backup.$(date +%Y%m%d_%H%M%S)"
    echo "💾 Backing up existing config to: $BACKUP_FILE"
    cp "$DEST_DIR/$CONFIG_FILE" "$BACKUP_FILE"
fi

# Copy the config file
cp "$SOURCE_DIR/$CONFIG_FILE" "$DEST_DIR/$CONFIG_FILE"

echo "✅ Configuration copied successfully!"
echo ""
echo "To verify: cat $DEST_DIR/$CONFIG_FILE"
