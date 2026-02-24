#!/bin/bash
# migrate-systemd.sh — Fix systemd service to use time.conf instead of config.toml
# Run on each masternode server after upgrading to v1.2.0+

set -e

SERVICE_FILE="/etc/systemd/system/timed.service"

if [ ! -f "$SERVICE_FILE" ]; then
    echo "❌ $SERVICE_FILE not found"
    exit 1
fi

# Check if already migrated
if grep -q '\-\-conf.*time\.conf' "$SERVICE_FILE"; then
    echo "✅ Already using time.conf — no changes needed"
    exit 0
fi

# Check if it has the old config.toml reference
if ! grep -q 'config.toml\|--config' "$SERVICE_FILE"; then
    echo "⚠️  No config.toml or --config reference found in service file"
    echo "    Review manually: cat $SERVICE_FILE"
    exit 0
fi

echo "🔧 Migrating systemd service file..."

# Extract the current config path to determine the data directory
CURRENT_PATH=$(grep -oP '(?<=--config\s)\S+' "$SERVICE_FILE" 2>/dev/null || true)
if [ -z "$CURRENT_PATH" ]; then
    CURRENT_PATH=$(grep -oP '(?<=--conf\s)\S+' "$SERVICE_FILE" 2>/dev/null || true)
fi

if [ -n "$CURRENT_PATH" ]; then
    CONFIG_DIR=$(dirname "$CURRENT_PATH")
    NEW_CONF="$CONFIG_DIR/time.conf"
else
    # Fallback: try common locations
    if [ -f "$HOME/.timecoin/testnet/time.conf" ]; then
        NEW_CONF="$HOME/.timecoin/testnet/time.conf"
    elif [ -f "$HOME/.timecoin/time.conf" ]; then
        NEW_CONF="$HOME/.timecoin/time.conf"
    elif [ -f "/root/.timecoin/testnet/time.conf" ]; then
        NEW_CONF="/root/.timecoin/testnet/time.conf"
    elif [ -f "/root/.timecoin/time.conf" ]; then
        NEW_CONF="/root/.timecoin/time.conf"
    else
        echo "❌ Could not find time.conf — create it first or run install-masternode.sh"
        exit 1
    fi
fi

echo "  Old: $(grep 'ExecStart' "$SERVICE_FILE" | xargs)"
echo "  New conf: $NEW_CONF"

# Backup
cp "$SERVICE_FILE" "${SERVICE_FILE}.bak.$(date +%Y%m%d%H%M%S)"

# Replace --config <anything> with --conf <time.conf path>
sed -i "s|--config\s\+\S\+|--conf $NEW_CONF|g" "$SERVICE_FILE"
# Also handle if it already uses --conf but points to config.toml
sed -i "s|--conf\s\+\S\+config\.toml|--conf $NEW_CONF|g" "$SERVICE_FILE"

echo "  New: $(grep 'ExecStart' "$SERVICE_FILE" | xargs)"

# Reload systemd
systemctl daemon-reload
echo "✅ Systemd reloaded"

echo ""
echo "To apply: systemctl restart timed"
echo "To verify: journalctl -u timed --no-pager | head -20"
