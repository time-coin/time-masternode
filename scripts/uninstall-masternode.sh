#!/bin/bash
#
# TIME Coin Masternode Uninstall Script
#
# Usage: sudo ./uninstall-masternode.sh [mainnet|testnet]
#

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

# Network selection (default to mainnet if not specified)
NETWORK="${1:-mainnet}"

# Validate network
if [[ "$NETWORK" != "mainnet" && "$NETWORK" != "testnet" ]]; then
    echo -e "${RED}Error: Network must be 'mainnet' or 'testnet'${NC}"
    echo "Usage: sudo ./uninstall-masternode.sh [mainnet|testnet]"
    exit 1
fi

# Configuration
SERVICE_NAME="timed"
SERVICE_USER="timecoin"
BIN_DIR="/usr/local/bin"

# Use /root/.timecoin as base directory
BASE_DIR="/root/.timecoin"
if [[ "$NETWORK" == "testnet" ]]; then
    DATA_DIR="$BASE_DIR/testnet"
else
    DATA_DIR="$BASE_DIR"
fi

CONFIG_DIR="$DATA_DIR"  # Config goes in same directory as data
LOG_DIR="$DATA_DIR/logs"

print_header() {
    echo -e "${RED}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${RED}║         TIME Coin Masternode Uninstall Script                ║${NC}"
    echo -e "${RED}║                  Network: ${NETWORK^^}                             ║${NC}"
    echo -e "${RED}╚══════════════════════════════════════════════════════════════╝${NC}"
    echo ""
}

print_step() {
    echo -e "${GREEN}==>${NC} $1"
}

print_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

print_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

check_root() {
    if [[ $EUID -ne 0 ]]; then
        print_error "This script must be run as root (use sudo)"
        exit 1
    fi
}

confirm_uninstall() {
    echo -e "${RED}WARNING: This will remove TIME Coin completely!${NC}"
    echo ""
    echo "The following will be removed:"
    echo "  • Service: ${SERVICE_NAME}"
    echo "  • Binaries: $BIN_DIR/timed, $BIN_DIR/time-cli"
    echo "  • Config: $CONFIG_DIR"
    echo "  • User: $SERVICE_USER"
    echo ""
    echo -e "${YELLOW}Data directory will be preserved: $DATA_DIR${NC}"
    echo -e "${YELLOW}(Remove manually if you want to delete blockchain data)${NC}"
    echo ""
    
    read -p "Are you sure you want to continue? (type 'yes' to confirm): " -r
    if [[ ! $REPLY == "yes" ]]; then
        echo "Uninstall cancelled"
        exit 0
    fi
}

stop_service() {
    print_step "Stopping service..."
    
    if systemctl is-active --quiet ${SERVICE_NAME}.service; then
        systemctl stop ${SERVICE_NAME}.service
        print_success "Service stopped"
    else
        print_warn "Service is not running"
    fi
}

disable_service() {
    print_step "Disabling service..."
    
    if systemctl is-enabled --quiet ${SERVICE_NAME}.service; then
        systemctl disable ${SERVICE_NAME}.service
        print_success "Service disabled"
    else
        print_warn "Service is not enabled"
    fi
}

remove_service() {
    print_step "Removing systemd service..."
    
    if [ -f "/etc/systemd/system/${SERVICE_NAME}.service" ]; then
        rm /etc/systemd/system/${SERVICE_NAME}.service
        systemctl daemon-reload
        print_success "Service file removed"
    else
        print_warn "Service file not found"
    fi
}

remove_binaries() {
    print_step "Removing binaries..."
    
    rm -f "$BIN_DIR/timed"
    rm -f "$BIN_DIR/time-cli"
    
    print_success "Binaries removed"
}

remove_config() {
    print_step "Removing configuration..."
    
    if [ -d "$CONFIG_DIR" ]; then
        rm -rf "$CONFIG_DIR"
        print_success "Configuration removed"
    else
        print_warn "Configuration directory not found"
    fi
}

remove_logs() {
    print_step "Removing logs..."
    
    if [ -d "$LOG_DIR" ]; then
        rm -rf "$LOG_DIR"
        print_success "Logs removed"
    else
        print_warn "Log directory not found"
    fi
}

remove_user() {
    print_step "Removing service user..."
    
    if id "$SERVICE_USER" &>/dev/null; then
        userdel "$SERVICE_USER"
        print_success "User $SERVICE_USER removed"
    else
        print_warn "User $SERVICE_USER not found"
    fi
}

print_data_info() {
    echo ""
    echo -e "${YELLOW}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${YELLOW}║                   Data Preservation                          ║${NC}"
    echo -e "${YELLOW}╚══════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo "Blockchain data has been preserved at:"
    echo "  $DATA_DIR"
    echo ""
    echo "To completely remove all data, run:"
    echo "  sudo rm -rf $DATA_DIR"
    echo ""
    echo "This contains:"
    echo "  • Blockchain database"
    echo "  • Wallet files"
    echo "  • Node state"
    echo ""
    print_warn "Removing this data is IRREVERSIBLE!"
    echo ""
}

print_summary() {
    echo ""
    echo -e "${GREEN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║              Uninstall Complete! ✅                          ║${NC}"
    echo -e "${GREEN}╚══════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${BLUE}Removed:${NC}"
    echo "  • Service: ${SERVICE_NAME}"
    echo "  • Binaries: timed, time-cli"
    echo "  • Configuration"
    echo "  • Logs"
    echo "  • Service user"
    echo ""
}

main() {
    print_header
    check_root
    confirm_uninstall
    
    # Uninstall steps
    stop_service
    disable_service
    remove_service
    remove_binaries
    remove_config
    remove_logs
    remove_user
    
    # Summary
    print_summary
    print_data_info
}

main "$@"
