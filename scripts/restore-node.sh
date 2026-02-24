#!/bin/bash
# restore-node.sh — Restore TIME Coin node from a backup tarball
#
# Restores a backup created by backup-node.sh. Stops the daemon,
# verifies the backup, extracts files, and optionally restarts.
#
# Usage:
#   sudo bash scripts/restore-node.sh <backup_file> [OPTIONS]
#
# Options:
#   -n, --network     Network: mainnet or testnet (default: auto-detect from backup)
#   -w, --wallet-only Only restore wallet and config files (skip blockchain DB)
#   --no-restart      Don't restart the daemon after restore
#   -y, --yes         Skip confirmation prompt
#   -h, --help        Show this help

set -euo pipefail

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}ℹ️  $1${NC}"; }
log_success() { echo -e "${GREEN}✅ $1${NC}"; }
log_warn()    { echo -e "${YELLOW}⚠️  $1${NC}"; }
log_error()   { echo -e "${RED}❌ $1${NC}"; }

usage() {
    sed -n '/^# Usage:/,/^$/p' "$0" | sed 's/^# //'
    sed -n '/^# Options:/,/^$/p' "$0" | sed 's/^# //'
    exit 0
}

BACKUP_FILE=""
NETWORK=""
WALLET_ONLY=0
NO_RESTART=0
SKIP_CONFIRM=0

while [[ $# -gt 0 ]]; do
    case $1 in
        -n|--network)      NETWORK="$2"; shift 2 ;;
        -w|--wallet-only)  WALLET_ONLY=1; shift ;;
        --no-restart)      NO_RESTART=1; shift ;;
        -y|--yes)          SKIP_CONFIRM=1; shift ;;
        -h|--help)         usage ;;
        -*) log_error "Unknown option: $1"; usage ;;
        *)
            if [ -z "$BACKUP_FILE" ]; then
                BACKUP_FILE="$1"
            else
                log_error "Unexpected argument: $1"
                usage
            fi
            shift ;;
    esac
done

if [ -z "$BACKUP_FILE" ]; then
    log_error "No backup file specified"
    echo "Usage: sudo bash scripts/restore-node.sh <backup_file> [OPTIONS]"
    exit 1
fi

if [ ! -f "$BACKUP_FILE" ]; then
    log_error "Backup file not found: $BACKUP_FILE"
    exit 1
fi

# Auto-detect network from backup filename if not specified
if [ -z "$NETWORK" ]; then
    if echo "$BACKUP_FILE" | grep -q "testnet"; then
        NETWORK="testnet"
    elif echo "$BACKUP_FILE" | grep -q "mainnet"; then
        NETWORK="mainnet"
    else
        NETWORK="testnet"
        log_warn "Could not detect network from filename, defaulting to testnet"
    fi
fi

BASE_DIR="${HOME}/.timecoin"
if [ "$NETWORK" = "testnet" ]; then
    DATA_DIR="$BASE_DIR/testnet"
else
    DATA_DIR="$BASE_DIR"
fi

echo ""
echo "╔══════════════════════════════════════════╗"
echo "║     TIME Coin Node Restore               ║"
echo "╚══════════════════════════════════════════╝"
echo ""

# Verify backup integrity
log_info "Verifying backup integrity..."
if ! tar -tzf "$BACKUP_FILE" > /dev/null 2>&1; then
    log_error "Backup file is corrupted or not a valid tarball"
    exit 1
fi

FILE_COUNT=$(tar -tzf "$BACKUP_FILE" | wc -l)
BACKUP_SIZE=$(du -h "$BACKUP_FILE" | awk '{print $1}')
HAS_DB=$(tar -tzf "$BACKUP_FILE" 2>/dev/null | grep -c "db/" || true)
HAS_WALLET=$(tar -tzf "$BACKUP_FILE" 2>/dev/null | grep -c "time-wallet.dat" || true)
HAS_CONF=$(tar -tzf "$BACKUP_FILE" 2>/dev/null | grep -c "time.conf" || true)

log_info "Backup:      $BACKUP_FILE ($BACKUP_SIZE)"
log_info "Files:       $FILE_COUNT"
log_info "Contains:    $([ "$HAS_DB" -gt 0 ] && echo "DB ✓" || echo "DB ✗") | $([ "$HAS_WALLET" -gt 0 ] && echo "Wallet ✓" || echo "Wallet ✗") | $([ "$HAS_CONF" -gt 0 ] && echo "Config ✓" || echo "Config ✗")"
log_info "Network:     $NETWORK"
log_info "Restore to:  $DATA_DIR"
log_info "Mode:        $([ "$WALLET_ONLY" -eq 1 ] && echo "wallet+config only" || echo "full restore")"
echo ""

if [ "$WALLET_ONLY" -eq 1 ] && [ "$HAS_WALLET" -eq 0 ] && [ "$HAS_CONF" -eq 0 ]; then
    log_error "Backup does not contain wallet or config files"
    exit 1
fi

# Confirmation
if [ "$SKIP_CONFIRM" -eq 0 ]; then
    echo -e "${RED}⚠️  WARNING: This will overwrite existing data in $DATA_DIR${NC}"
    read -p "Continue? (type 'restore' to confirm): " -r
    if [ "$REPLY" != "restore" ]; then
        echo "Restore cancelled"
        exit 0
    fi
    echo ""
fi

# Stop daemon
SERVICE_WAS_RUNNING=0
if systemctl is-active --quiet timed 2>/dev/null; then
    SERVICE_WAS_RUNNING=1
    log_info "Stopping timed..."
    systemctl stop timed
    sleep 2
    log_success "Daemon stopped"
fi

# Create pre-restore backup of current wallet (safety net)
if [ -f "$DATA_DIR/time-wallet.dat" ]; then
    SAFETY_BACKUP="$DATA_DIR/time-wallet.dat.pre-restore.$(date +%Y%m%d%H%M%S)"
    cp "$DATA_DIR/time-wallet.dat" "$SAFETY_BACKUP"
    log_info "Safety backup of current wallet: $SAFETY_BACKUP"
fi

# Restore
mkdir -p "$DATA_DIR"

if [ "$WALLET_ONLY" -eq 1 ]; then
    log_info "Restoring wallet and config files..."
    # Extract only wallet/config files
    for f in time-wallet.dat time.conf masternode.conf; do
        if tar -tzf "$BACKUP_FILE" 2>/dev/null | grep -q "$f"; then
            tar -xzf "$BACKUP_FILE" -C "$DATA_DIR" --strip-components=$(tar -tzf "$BACKUP_FILE" | grep "$f" | head -1 | awk -F/ '{print NF-1}') "$(tar -tzf "$BACKUP_FILE" | grep "$f" | head -1)" 2>/dev/null || true
        fi
    done
else
    log_info "Restoring full data directory..."
    # Detect if backup contains the directory name (e.g., testnet/) or bare files
    FIRST_ENTRY=$(tar -tzf "$BACKUP_FILE" | head -1)
    if echo "$FIRST_ENTRY" | grep -qE "^testnet/|^db/|^time-"; then
        tar -xzf "$BACKUP_FILE" -C "$DATA_DIR" --strip-components=0
    else
        tar -xzf "$BACKUP_FILE" -C "$(dirname "$DATA_DIR")"
    fi
fi

log_success "Files restored to $DATA_DIR"

# Restart daemon
if [ "$SERVICE_WAS_RUNNING" -eq 1 ] && [ "$NO_RESTART" -eq 0 ]; then
    log_info "Restarting timed..."
    systemctl start timed
    sleep 3
    if systemctl is-active --quiet timed; then
        log_success "Daemon restarted"
    else
        log_error "Daemon failed to restart — check: journalctl -u timed -f"
    fi
fi

echo ""
log_success "Restore complete!"
echo ""
echo "  Restored: $DATA_DIR"
if [ -n "${SAFETY_BACKUP:-}" ]; then
    echo "  Old wallet saved: $SAFETY_BACKUP"
fi
echo ""
echo "  Verify with: time-cli $([ "$NETWORK" = "testnet" ] && echo "--testnet ")getblockchaininfo"
