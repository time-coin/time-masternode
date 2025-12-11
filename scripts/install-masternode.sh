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
else
    P2P_PORT="24100"
    RPC_PORT="24101"
fi

# Configuration
SERVICE_NAME="timed"
SERVICE_USER="timecoin"
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
VERSION="0.1.0"

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
    )
    
    for pkg in "${required_packages[@]}"; do
        if ! dpkg -l | grep -q "^ii  $pkg"; then
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
        ca-certificates \
        gnupg \
        lsb-release
    
    print_success "System dependencies installed"
}

check_rust() {
    print_step "Checking for Rust installation..."
    
    if command -v rustc &> /dev/null && command -v cargo &> /dev/null; then
        local rust_version=$(rustc --version | cut -d' ' -f2)
        print_info "Rust $rust_version is installed"
        return 0
    else
        print_warn "Rust is not installed"
        return 1
    fi
}

install_rust() {
    print_step "Installing Rust..."
    
    # Install rustup as the service user if it exists, otherwise as root
    if id "$SERVICE_USER" &>/dev/null; then
        sudo -u "$SERVICE_USER" bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y'
        export PATH="/home/$SERVICE_USER/.cargo/bin:$PATH"
    else
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
        source "$HOME/.cargo/env"
    fi
    
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


create_user() {
    print_step "Creating service user..."
    
    if id "$SERVICE_USER" &>/dev/null; then
        print_info "User $SERVICE_USER already exists"
    else
        useradd --system --no-create-home --shell /bin/false "$SERVICE_USER"
        print_success "User $SERVICE_USER created"
    fi
}

create_directories() {
    print_step "Creating directories..."
    
    mkdir -p "$INSTALL_DIR"
    mkdir -p "$CONFIG_DIR"
    mkdir -p "$DATA_DIR"
    mkdir -p "$LOG_DIR"
    
    # Set ownership
    chown -R "$SERVICE_USER:$SERVICE_USER" "$INSTALL_DIR"
    chown -R "$SERVICE_USER:$SERVICE_USER" "$CONFIG_DIR"
    chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR"
    chown -R "$SERVICE_USER:$SERVICE_USER" "$LOG_DIR"
    
    # Set permissions
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
}

install_binaries() {
    print_step "Installing binaries..."
    
    # Get the script's directory
    SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
    PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
    
    # Copy binaries
    cp "$PROJECT_DIR/target/release/timed" "$BIN_DIR/"
    cp "$PROJECT_DIR/target/release/time-cli" "$BIN_DIR/"
    
    # Set permissions
    chmod 755 "$BIN_DIR/timed"
    chmod 755 "$BIN_DIR/time-cli"
    
    # Verify installation
    if [ -f "$BIN_DIR/timed" ] && [ -f "$BIN_DIR/time-cli" ]; then
        print_success "Binaries installed to $BIN_DIR"
        print_info "timed version: $(timed --version 2>/dev/null || echo 'unknown')"
        print_info "time-cli version: $(time-cli --version 2>/dev/null || echo 'unknown')"
    else
        print_error "Failed to install binaries"
        exit 1
    fi
}

create_config() {
    print_step "Creating configuration file..."
    
    # Get the script's directory
    SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
    PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
    
    # Copy default config if it exists
    if [ -f "$PROJECT_DIR/config.toml" ]; then
        cp "$PROJECT_DIR/config.toml" "$CONFIG_DIR/config.toml"
        
        # Update ports in config file
        sed -i "s/listen_addr = \"0.0.0.0:[0-9]*\"/listen_addr = \"0.0.0.0:$P2P_PORT\"/g" "$CONFIG_DIR/config.toml"
        sed -i "s/rpc_addr = \"127.0.0.1:[0-9]*\"/rpc_addr = \"127.0.0.1:$RPC_PORT\"/g" "$CONFIG_DIR/config.toml"
        sed -i "s|data_dir = \".*\"|data_dir = \"$DATA_DIR\"|g" "$CONFIG_DIR/config.toml"
        
        chown "$SERVICE_USER:$SERVICE_USER" "$CONFIG_DIR/config.toml"
        chmod 640 "$CONFIG_DIR/config.toml"
        print_success "Configuration copied to $CONFIG_DIR/config.toml"
    else
        print_warn "No default config.toml found, creating minimal config"
        
        cat > "$CONFIG_DIR/config.toml" <<EOF
# TIME Coin Configuration - $NETWORK
[network]
listen_addr = "0.0.0.0:$P2P_PORT"
rpc_addr = "127.0.0.1:$RPC_PORT"
network = "$NETWORK"

[blockchain]
data_dir = "$DATA_DIR"

[logging]
level = "info"
log_dir = "$LOG_DIR"
EOF
        chown "$SERVICE_USER:$SERVICE_USER" "$CONFIG_DIR/config.toml"
        chmod 640 "$CONFIG_DIR/config.toml"
        print_success "Minimal configuration created"
    fi
    
    print_info "Edit configuration: $CONFIG_DIR/config.toml"
    print_info "Network: $NETWORK (P2P: $P2P_PORT, RPC: $RPC_PORT)"
}

create_systemd_service() {
    print_step "Creating systemd service..."
    
    cat > /etc/systemd/system/${SERVICE_NAME}.service <<EOF
[Unit]
Description=TIME Coin Masternode
After=network.target
Wants=network-online.target

[Service]
Type=simple
User=$SERVICE_USER
Group=$SERVICE_USER

# Binary location
ExecStart=$BIN_DIR/timed --config $CONFIG_DIR/config.toml

# Working directory
WorkingDirectory=$DATA_DIR

# Restart policy
Restart=always
RestartSec=10

# Resource limits
LimitNOFILE=65535
LimitNPROC=4096

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=$DATA_DIR $LOG_DIR

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
    echo "  • Config:   $CONFIG_DIR/config.toml"
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
    echo "  • Edit config:     nano $CONFIG_DIR/config.toml"
    echo ""
    echo -e "${BLUE}CLI Tools:${NC}"
    echo "  • time-cli --help"
    echo "  • time-cli wallet create"
    echo "  • time-cli wallet balance <address>"
    echo ""
    echo -e "${YELLOW}Next Steps:${NC}"
    echo "  1. Edit configuration: nano $CONFIG_DIR/config.toml"
    echo "  2. Create a wallet: time-cli wallet create"
    echo "  3. Fund your masternode address"
    echo "  4. Check logs: journalctl -u ${SERVICE_NAME} -f"
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
    
    # Create user and directories
    create_user
    create_directories
    
    # Build and install
    build_binaries
    install_binaries
    
    # Configuration
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
