#!/bin/bash
# backup-node.sh — Backup TIME Coin blockchain, wallet, and configuration
#
# Creates a timestamped compressed tarball of the node data directory.
# Stops the daemon for a consistent snapshot, then restarts it.
#
# Usage:
#   sudo bash scripts/backup-node.sh [OPTIONS]
#
# Options:
#   -n, --network     Network: mainnet or testnet (default: testnet)
#   -o, --output      Output directory for backup file (default: /root)
#   -w, --wallet-only Only backup wallet and config files (skip blockchain DB)
#   --no-restart      Don't restart the daemon after backup
#   --hot             Hot backup (don't stop daemon — less consistent but no downtime)
#   -h, --help        Show this help

set -euo pipefail

# Defaults
NETWORK="testnet"
OUTPUT_DIR="/root"
WALLET_ONLY=0
NO_RESTART=0
HOT=0

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

while [[ $# -gt 0 ]]; do
    case $1 in
        -n|--network)      NETWORK="$2"; shift 2 ;;
        -o|--output)       OUTPUT_DIR="$2"; shift 2 ;;
        -w|--wallet-only)  WALLET_ONLY=1; shift ;;
        --no-restart)      NO_RESTART=1; shift ;;
        --hot)             HOT=1; shift ;;
        -h|--help)         usage ;;
        *) log_error "Unknown option: $1"; usage ;;
    esac
done

# Determine data directory
BASE_DIR="${HOME}/.timecoin"
if [ "$NETWORK" = "testnet" ]; then
    DATA_DIR="$BASE_DIR/testnet"
else
    DATA_DIR="$BASE_DIR"
fi

if [ ! -d "$DATA_DIR" ]; then
    log_error "Data directory not found: $DATA_DIR"
    exit 1
fi

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
if [ "$WALLET_ONLY" -eq 1 ]; then
    BACKUP_FILE="$OUTPUT_DIR/timecoin_wallet_${NETWORK}_${TIMESTAMP}.tar.gz"
else
    BACKUP_FILE="$OUTPUT_DIR/timecoin_backup_${NETWORK}_${TIMESTAMP}.tar.gz"
fi

echo ""
echo "╔══════════════════════════════════════════╗"
echo "║     TIME Coin Node Backup                ║"
echo "╚══════════════════════════════════════════╝"
echo ""
log_info "Network:     $NETWORK"
log_info "Data dir:    $DATA_DIR"
log_info "Output:      $BACKUP_FILE"
log_info "Mode:        $([ "$WALLET_ONLY" -eq 1 ] && echo "wallet+config only" || echo "full backup")"
log_info "Hot backup:  $([ "$HOT" -eq 1 ] && echo "yes (no downtime)" || echo "no (daemon will stop)")"
echo ""

# Check disk space
DATA_SIZE=$(du -sm "$DATA_DIR" 2>/dev/null | awk '{print $1}')
FREE_SPACE=$(df -m "$OUTPUT_DIR" 2>/dev/null | awk 'NR==2 {print $4}')
if [ -n "$FREE_SPACE" ] && [ -n "$DATA_SIZE" ] && [ "$FREE_SPACE" -lt "$DATA_SIZE" ]; then
    log_error "Insufficient disk space: ${DATA_SIZE}MB needed, ${FREE_SPACE}MB available"
    exit 1
fi
log_info "Data size: ~${DATA_SIZE}MB, Free space: ${FREE_SPACE}MB"

# Stop daemon for consistent snapshot (unless hot backup)
SERVICE_WAS_RUNNING=0
if [ "$HOT" -eq 0 ]; then
    if systemctl is-active --quiet timed 2>/dev/null; then
        SERVICE_WAS_RUNNING=1
        log_info "Stopping timed for consistent snapshot..."
        systemctl stop timed
        sleep 2
        log_success "Daemon stopped"
    fi
fi

# Build file list
if [ "$WALLET_ONLY" -eq 1 ]; then
    log_info "Backing up wallet and config files..."
    INCLUDE_ARGS=()
    for f in time-wallet.dat time.conf masternode.conf; do
        if [ -f "$DATA_DIR/$f" ]; then
            INCLUDE_ARGS+=("$f")
        fi
    done
    if [ ${#INCLUDE_ARGS[@]} -eq 0 ]; then
        log_error "No wallet or config files found in $DATA_DIR"
        # Restart if we stopped it
        if [ "$SERVICE_WAS_RUNNING" -eq 1 ] && [ "$NO_RESTART" -eq 0 ]; then
            systemctl start timed
        fi
        exit 1
    fi
    tar -czf "$BACKUP_FILE" -C "$DATA_DIR" "${INCLUDE_ARGS[@]}"
else
    log_info "Backing up full data directory..."
    tar -czf "$BACKUP_FILE" -C "$(dirname "$DATA_DIR")" "$(basename "$DATA_DIR")"
fi

# Verify backup
if [ -f "$BACKUP_FILE" ]; then
    BACKUP_SIZE=$(du -h "$BACKUP_FILE" | awk '{print $1}')
    FILE_COUNT=$(tar -tzf "$BACKUP_FILE" 2>/dev/null | wc -l)
    log_success "Backup created: $BACKUP_FILE ($BACKUP_SIZE, $FILE_COUNT files)"
else
    log_error "Backup file not created!"
    if [ "$SERVICE_WAS_RUNNING" -eq 1 ] && [ "$NO_RESTART" -eq 0 ]; then
        systemctl start timed
    fi
    exit 1
fi

# Restart daemon
if [ "$SERVICE_WAS_RUNNING" -eq 1 ] && [ "$NO_RESTART" -eq 0 ]; then
    log_info "Restarting timed..."
    systemctl start timed
    sleep 2
    if systemctl is-active --quiet timed; then
        log_success "Daemon restarted"
    else
        log_error "Daemon failed to restart — check: journalctl -u timed -f"
    fi
fi

echo ""
log_success "Backup complete!"
echo ""
echo "  File:  $BACKUP_FILE"
echo "  Size:  $BACKUP_SIZE"
echo "  Files: $FILE_COUNT"
echo ""
echo "  Restore with: bash scripts/restore-node.sh $BACKUP_FILE"
