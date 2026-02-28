#!/bin/bash
#
# TIME Coin Masternode Installation Script
# For Ubuntu/Debian-based systems
#
# Usage: sudo ./install-masternode.sh [mainnet|testnet]
#

set -e  # Exit on error

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Network selection (default to mainnet if not specified)
NETWORK="${1:-mainnet}"

# Validate network
if [[ "$NETWORK" != "mainnet" && "$NETWORK" != "testnet" ]]; then
    echo -e "${RED}Error: Network must be 'mainnet' or 'testnet'${NC}"
    echo "Usage: sudo ./install-masternode.sh [mainnet|testnet]"
    exit 1
fi

# Port configuration based on network
if [[ "$NETWORK" == "mainnet" ]]; then
    P2P_PORT="24000"
    RPC_PORT="24001"
    TESTNET_FLAG="0"
else
    P2P_PORT="24100"
    RPC_PORT="24101"
    TESTNET_FLAG="1"
fi

# Configuration
SERVICE_NAME="timed"
INSTALL_DIR="/opt/timecoin"
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

# Version info
VERSION="1.2.0"

#------------------------------------------------------------------------------
# Helper Functions
#------------------------------------------------------------------------------

print_header() {
    echo -e "${BLUE}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${BLUE}║       TIME Coin Masternode Installation Script v${VERSION}      ║${NC}"
    echo -e "${BLUE}║                  Network: ${NETWORK^^}                             ║${NC}"
    echo -e "${BLUE}╚══════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${BLUE}Network Configuration:${NC}"
    echo "  • P2P Port: $P2P_PORT"
    echo "  • RPC Port: $RPC_PORT"
    echo "  • Data Directory: $DATA_DIR"
    echo ""
}

print_step() {
    echo -e "${GREEN}==>${NC} $1"
}

print_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
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

detect_os() {
    print_step "Detecting operating system..."
    
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        OS=$ID
        OS_VERSION=$VERSION_ID
        print_info "Detected: $PRETTY_NAME"
    else
        print_error "Cannot detect OS. /etc/os-release not found."
        exit 1
    fi
    
    # Check if supported
    case "$OS" in
        ubuntu|debian|linuxmint|pop)
            print_success "Supported OS detected"
            ;;
        *)
            print_warn "This OS may not be fully supported. Proceeding anyway..."
            ;;
    esac
}

check_dependencies() {
    print_step "Checking system dependencies..."
    
    local missing_deps=()
    
    # Required system packages
    local required_packages=(
        "curl"
        "git"
        "build-essential"
        "pkg-config"
        "libssl-dev"
        "cmake"
        "clang"
        "libclang-dev"
    )
    
    for pkg in "${required_packages[@]}"; do
        if ! dpkg -l "$pkg" 2>/dev/null | grep -q "^ii"; then
            missing_deps+=("$pkg")
        fi
    done
    
    if [ ${#missing_deps[@]} -ne 0 ]; then
        print_info "Missing packages: ${missing_deps[*]}"
        return 1
    else
        print_success "All system dependencies are installed"
        return 0
    fi
}

install_dependencies() {
    print_step "Installing system dependencies..."
    
    apt-get update -qq
    apt-get install -y \
        curl \
        git \
        build-essential \
        pkg-config \
        libssl-dev \
        cmake \
        clang \
        libclang-dev \
        ca-certificates \
        gnupg \
        lsb-release
    
    print_success "System dependencies installed"
}

check_rust() {
    print_step "Checking for Rust installation..."
    
    # Ensure cargo is in PATH
    if [ -f "$HOME/.cargo/env" ]; then
        source "$HOME/.cargo/env"
    fi
    
    if command -v rustc &> /dev/null && command -v cargo &> /dev/null; then
        local rust_version=$(rustc --version | cut -d' ' -f2)
        print_info "Rust $rust_version is installed"
        
        # Check minimum version (1.75)
        local min_version="1.75.0"
        local current_version=$(rustc --version | cut -d' ' -f2 | cut -d'-' -f1)
        
        if [ "$(printf '%s\n' "$min_version" "$current_version" | sort -V | head -n1)" = "$min_version" ]; then
            print_success "Rust version is sufficient (>= 1.75)"
            return 0
        else
            print_warn "Rust version $current_version is too old (need >= 1.75)"
            print_info "Updating Rust..."
            rustup update stable
            return 0
        fi
    else
        print_warn "Rust is not installed"
        return 1
    fi
}

install_rust() {
    print_step "Installing Rust..."
    
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    
    print_success "Rust installed"
}

check_nasm() {
    print_step "Checking for NASM..."
    
    if command -v nasm &> /dev/null; then
        local nasm_version=$(nasm -v | head -n1)
        print_info "NASM installed: $nasm_version"
        return 0
    else
        print_warn "NASM is not installed (required for some cryptography libraries)"
        return 1
    fi
}

install_nasm() {
    print_step "Installing NASM..."
    
    apt-get install -y nasm
    
    print_success "NASM installed"
}

create_directories() {
    print_step "Creating directories..."
    
    # Create directories
    mkdir -p "$INSTALL_DIR"
    mkdir -p "$CONFIG_DIR"
    mkdir -p "$DATA_DIR"
    mkdir -p "$LOG_DIR"
    
    # Set ownership to root (service runs as root)
    chown -R root:root "$BASE_DIR"
    chown -R root:root "$INSTALL_DIR"
    
    # Set permissions
    chmod 700 "$BASE_DIR"      # Only root can access
    chmod 750 "$CONFIG_DIR"
    chmod 750 "$DATA_DIR"
    chmod 755 "$LOG_DIR"
    
    print_success "Directories created"
}

build_binaries() {
    print_step "Building TIME Coin binaries..."
    
    # Get the script's directory (should be in scripts/)
    SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
    PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
    
    print_info "Project directory: $PROJECT_DIR"
    
    cd "$PROJECT_DIR"
    
    # Verify we're in the right place
    if [ ! -f "Cargo.toml" ]; then
        print_error "Cargo.toml not found in $PROJECT_DIR"
        print_error "Script must be run from the time-masternode/scripts/ directory"
        exit 1
    fi
    
    print_info "Current directory: $(pwd)"
    
    # Ensure Rust is in PATH
    if [ -f "$HOME/.cargo/env" ]; then
        source "$HOME/.cargo/env"
    fi
    
    # Build in release mode
    print_info "This may take several minutes..."
    
    if ! cargo build --release; then
        print_error "Build failed"
        exit 1
    fi
    
    print_success "Build completed"
    print_info "Binaries location: $PROJECT_DIR/target/release/"
}

install_binaries() {
    print_step "Installing binaries..."
    
    # Get the script's directory
    SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
    PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
    
    print_info "Looking for binaries in: $PROJECT_DIR/target/release/"
    
    # Verify binaries exist before copying
    if [ ! -f "$PROJECT_DIR/target/release/timed" ]; then
        print_error "Binary not found: $PROJECT_DIR/target/release/timed"
        print_error "Build may have failed or binaries are in a different location"
        exit 1
    fi
    
    if [ ! -f "$PROJECT_DIR/target/release/time-cli" ]; then
        print_error "Binary not found: $PROJECT_DIR/target/release/time-cli"
        print_error "Build may have failed or binaries are in a different location"
        exit 1
    fi
    
    # Copy binaries
    cp "$PROJECT_DIR/target/release/timed" "$BIN_DIR/"
    cp "$PROJECT_DIR/target/release/time-cli" "$BIN_DIR/"
    
    # Also copy time-dashboard if it was built
    if [ -f "$PROJECT_DIR/target/release/time-dashboard" ]; then
        cp "$PROJECT_DIR/target/release/time-dashboard" "$BIN_DIR/"
        chmod 755 "$BIN_DIR/time-dashboard"
    fi
    
    # Set permissions
    chmod 755 "$BIN_DIR/timed"
    chmod 755 "$BIN_DIR/time-cli"
    
    # Verify installation
    if [ -f "$BIN_DIR/timed" ] && [ -f "$BIN_DIR/time-cli" ]; then
        print_success "Binaries installed to $BIN_DIR"
        print_info "timed version: $($BIN_DIR/timed --version 2>/dev/null || echo 'unknown')"
        print_info "time-cli version: $($BIN_DIR/time-cli --version 2>/dev/null || echo 'unknown')"
    else
        print_error "Failed to install binaries"
        exit 1
    fi
}

create_config() {
    print_step "Creating configuration files..."

    local TIME_CONF="$CONFIG_DIR/time.conf"
    local MN_CONF="$CONFIG_DIR/masternode.conf"

    # Detect external IP for masternode config
    local EXTERNAL_IP
    EXTERNAL_IP=$(curl -s -4 ifconfig.me 2>/dev/null || curl -s -4 icanhazip.com 2>/dev/null || echo "YOUR_IP")

    # ── time.conf ────────────────────────────────────────────────
    if [ -f "$TIME_CONF" ]; then
        print_info "time.conf already exists, not overwriting"
    else
        cat > "$TIME_CONF" <<EOF
# TIME Coin Configuration File
# https://time-coin.io
#
# Lines beginning with # are comments.
# All settings are optional — defaults are shown below.

# ─── Network ─────────────────────────────────────────────────
# Run on testnet (1) or mainnet (0)
testnet=${TESTNET_FLAG}

# Accept incoming connections
listen=1

# Override the default port (mainnet=24000, testnet=24100)
#port=${P2P_PORT}

# Your public IP address (required for masternodes)
externalip=${EXTERNAL_IP}

# Maximum peer connections
#maxconnections=50

# ─── RPC ─────────────────────────────────────────────────────
# Enable JSON-RPC server
server=1

# RPC port (mainnet=24001, testnet=24101)
#rpcport=${RPC_PORT}

# Allow RPC connections from any IP (needed for remote time-cli)
rpcbind=0.0.0.0
rpcallowip=0.0.0.0/0

# ─── Masternode ──────────────────────────────────────────────
# Enable masternode mode (0=off, 1=on)
# Collateral settings go in masternode.conf
masternode=1

# Masternode private key (generate with: time-cli masternode genkey)
#masternodeprivkey=

# ─── Peers ───────────────────────────────────────────────────
# Add seed nodes (one per line, can repeat)
#addnode=seed1.time-coin.io
#addnode=seed2.time-coin.io

# ─── Logging ─────────────────────────────────────────────────
# Log level: trace, debug, info, warn, error
debug=info

# ─── Storage ─────────────────────────────────────────────────
# Maintain a full transaction index
txindex=1

# Custom data directory (leave commented for default)
#datadir=
EOF
        chown root:root "$TIME_CONF"
        chmod 640 "$TIME_CONF"
        print_success "Created $TIME_CONF"
    fi

    # ── masternode.conf ──────────────────────────────────────────
    if [ -f "$MN_CONF" ]; then
        print_info "masternode.conf already exists, not overwriting"
    else
        cat > "$MN_CONF" <<EOF
# TIME Coin Masternode Configuration
#
# Format (one entry per line):
#   alias  IP:port  collateral_txid  collateral_vout
#
# Fields:
#   alias            - A name for this masternode (e.g., mn1)
#   IP:port          - Your masternode's public IP and port
#   collateral_txid  - Transaction ID of your collateral deposit
#   collateral_vout  - Output index of your collateral (usually 0)
#
# Your masternode private key goes in time.conf:
#   masternodeprivkey=<key from 'time-cli masternode genkey'>
#
# Steps to set up a masternode:
#   1. Generate a masternode private key:
#      time-cli masternode genkey
#   2. Add masternodeprivkey=<key> to your time.conf
#   3. Send collateral to yourself:
#      time-cli sendtoaddress <your_address> 1000    (Bronze = 1,000 TIME)
#      time-cli sendtoaddress <your_address> 10000   (Silver = 10,000 TIME)
#      time-cli sendtoaddress <your_address> 100000  (Gold   = 100,000 TIME)
#   4. Find your collateral TXID:
#      time-cli listtransactions
#   5. Add a line below and restart timed
#
# Example:
# mn1 ${EXTERNAL_IP}:${P2P_PORT} abc123def456789012345678901234567890123456789012345678901234abcd 0
EOF
        chown root:root "$MN_CONF"
        chmod 640 "$MN_CONF"
        print_success "Created $MN_CONF"
    fi

    # Remove legacy config.toml if time.conf now exists
    if [ -f "$CONFIG_DIR/config.toml" ] && [ -f "$TIME_CONF" ]; then
        print_warn "Legacy config.toml found alongside time.conf"
        print_info "The daemon will use time.conf. You can remove config.toml when ready."
    fi

    print_info "Edit configuration: nano $TIME_CONF"
    print_info "Network: $NETWORK (P2P: $P2P_PORT, RPC: $RPC_PORT)"
}

create_systemd_service() {
    print_step "Creating systemd service..."
    
    cat > /etc/systemd/system/${SERVICE_NAME}.service <<EOF
[Unit]
Description=TIME Coin Masternode
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
Group=root

# Binary location — uses time.conf from data dir automatically
ExecStart=$BIN_DIR/timed --conf $CONFIG_DIR/time.conf

# Working directory
WorkingDirectory=$DATA_DIR

# Restart policy
Restart=always
RestartSec=10

# Resource limits
LimitNOFILE=65535
LimitNPROC=4096

# Environment
Environment="RUST_BACKTRACE=1"

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=timed

[Install]
WantedBy=multi-user.target
EOF
    
    # Reload systemd
    systemctl daemon-reload
    
    print_success "Systemd service created"
}

enable_service() {
    print_step "Enabling service..."
    
    systemctl enable ${SERVICE_NAME}.service
    
    print_success "Service enabled (will start on boot)"
}

start_service() {
    print_step "Starting service..."
    
    if systemctl start ${SERVICE_NAME}.service; then
        print_success "Service started"
        sleep 2
        
        # Check status
        if systemctl is-active --quiet ${SERVICE_NAME}.service; then
            print_success "Service is running"
        else
            print_warn "Service may not be running correctly"
            print_info "Check logs with: journalctl -u ${SERVICE_NAME} -f"
        fi
    else
        print_error "Failed to start service"
        print_info "Check logs with: journalctl -u ${SERVICE_NAME} -n 50"
        exit 1
    fi
}

create_firewall_rules() {
    print_step "Configuring firewall..."
    
    if command -v ufw &> /dev/null; then
        print_info "UFW firewall detected"
        
        # Allow P2P port
        ufw allow $P2P_PORT/tcp comment "TIME Coin P2P ($NETWORK)"
        
        print_success "Firewall rules added"
        print_info "P2P port $P2P_PORT opened for $NETWORK"
    else
        print_warn "UFW not installed, skipping firewall configuration"
        print_info "Manually open port $P2P_PORT/tcp for P2P networking"
    fi
}

print_summary() {
    echo ""
    echo -e "${GREEN}╔══════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║           Installation Complete! ✅                          ║${NC}"
    echo -e "${GREEN}╚══════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo -e "${BLUE}Network: ${NETWORK^^}${NC}"
    echo "  • P2P Port: $P2P_PORT"
    echo "  • RPC Port: $RPC_PORT (localhost only)"
    echo ""
    echo -e "${BLUE}Installed Components:${NC}"
    echo "  • Binaries: $BIN_DIR/timed, $BIN_DIR/time-cli"
    echo "  • Config:   $CONFIG_DIR/time.conf"
    echo "  • MN Conf:  $CONFIG_DIR/masternode.conf"
    echo "  • Data:     $DATA_DIR"
    echo "  • Logs:     $LOG_DIR"
    echo "  • Service:  ${SERVICE_NAME}.service"
    echo ""
    echo -e "${BLUE}Useful Commands:${NC}"
    echo "  • Check status:    systemctl status ${SERVICE_NAME}"
    echo "  • View logs:       journalctl -u ${SERVICE_NAME} -f"
    echo "  • Stop service:    systemctl stop ${SERVICE_NAME}"
    echo "  • Start service:   systemctl start ${SERVICE_NAME}"
    echo "  • Restart service: systemctl restart ${SERVICE_NAME}"
    echo "  • Edit config:     nano $CONFIG_DIR/time.conf"
    echo "  • Edit MN config:  nano $CONFIG_DIR/masternode.conf"
    echo ""
    echo -e "${BLUE}CLI Tools:${NC}"
    echo "  • time-cli masternode genkey     # Generate masternode private key"
    echo "  • time-cli masternode list       # List all masternodes"
    echo "  • time-cli masternode status     # This node's masternode status"
    echo "  • time-cli getblockchaininfo     # Blockchain info"
    echo "  • time-cli getbalance            # Wallet balance"
    echo "  • time-cli getpeerinfo           # Connected peers"
    echo ""
    echo -e "${YELLOW}Next Steps:${NC}"
    echo "  1. Generate a masternode key:   time-cli masternode genkey"
    echo "  2. Add key to config:           nano $CONFIG_DIR/time.conf"
    echo "     → Set masternodeprivkey=<key from step 1>"
    echo "  3. (Staked tiers) Send collateral and update masternode.conf"
    echo "  4. Restart the service:         systemctl restart ${SERVICE_NAME}"
    echo "  5. Check logs:                  journalctl -u ${SERVICE_NAME} -f"
    echo ""
}

#------------------------------------------------------------------------------
# Main Installation Flow
#------------------------------------------------------------------------------

main() {
    print_header
    
    # Pre-flight checks
    check_root
    detect_os
    
    # Check and install system dependencies
    if ! check_dependencies; then
        install_dependencies
    fi
    
    # Check and install Rust
    if ! check_rust; then
        install_rust
    fi
    
    # Check and install NASM (needed for some crypto libraries)
    if ! check_nasm; then
        install_nasm
    fi
    
    # Create directories (service runs as root)
    create_directories
    
    # Build and install
    build_binaries
    install_binaries
    
    # Configuration (time.conf + masternode.conf)
    create_config
    
    # Setup systemd service
    create_systemd_service
    enable_service
    
    # Firewall (optional)
    create_firewall_rules
    
    # Start the service
    read -p "Start the service now? (y/n) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        start_service
    else
        print_info "Service not started. Start manually with: systemctl start ${SERVICE_NAME}"
    fi
    
    # Print summary
    print_summary
}

# Run main function
main "$@"
