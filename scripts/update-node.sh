#!/bin/bash
# TIME Coin Node Update Script
# Updates the node software from git and reinstalls binaries

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
DEFAULT_REPO_DIR="$HOME/timecoin"
BIN_DIR="/usr/local/bin"

# Print functions
print_header() {
    echo -e "${BLUE}"
    echo "╔════════════════════════════════════════════════════════════╗"
    echo "║                                                            ║"
    echo "║           TIME COIN NODE UPDATE SCRIPT                    ║"
    echo "║                                                            ║"
    echo "╚════════════════════════════════════════════════════════════╝"
    echo -e "${NC}"
}

print_step() {
    echo -e "${BLUE}==>${NC} $1"
}

print_success() {
    echo -e "${GREEN}✓${NC} $1"
}

print_error() {
    echo -e "${RED}✗${NC} $1"
}

print_warn() {
    echo -e "${YELLOW}⚠${NC} $1"
}

print_info() {
    echo -e "  $1"
}

# Check if running as root
check_root() {
    if [ "$EUID" -ne 0 ]; then
        print_error "This script must be run as root (use sudo)"
        exit 1
    fi
}

# Find the timecoin repository
find_repo() {
    local repo_dir=""
    
    # Check if we're already in the timecoin directory
    if [ -f "Cargo.toml" ] && grep -q "name = \"timed\"" Cargo.toml 2>/dev/null; then
        repo_dir="$(pwd)"
        print_success "Found repository in current directory"
    # Check default location
    elif [ -d "$DEFAULT_REPO_DIR/.git" ]; then
        repo_dir="$DEFAULT_REPO_DIR"
        print_success "Found repository at $DEFAULT_REPO_DIR"
    # Search in common locations
    else
        print_step "Searching for timecoin repository..."
        for dir in "$HOME/timecoin" "/root/timecoin" "/opt/timecoin" "$HOME/projects/timecoin"; do
            if [ -d "$dir/.git" ] && [ -f "$dir/Cargo.toml" ]; then
                repo_dir="$dir"
                print_success "Found repository at $dir"
                break
            fi
        done
    fi
    
    if [ -z "$repo_dir" ]; then
        print_error "Could not find timecoin repository"
        echo "Please specify the repository directory:"
        echo "  sudo $0 /path/to/timecoin"
        exit 1
    fi
    
    echo "$repo_dir"
}

# Check if service is running
check_service_running() {
    if systemctl is-active --quiet timed.service 2>/dev/null; then
        return 0
    else
        return 1
    fi
}

# Stop the service
stop_service() {
    if check_service_running; then
        print_step "Stopping timed service..."
        systemctl stop timed.service
        print_success "Service stopped"
        return 0
    else
        print_info "Service is not running"
        return 1
    fi
}

# Start the service
start_service() {
    print_step "Starting timed service..."
    systemctl start timed.service
    sleep 2
    
    if systemctl is-active --quiet timed.service; then
        print_success "Service started successfully"
        return 0
    else
        print_error "Service failed to start"
        print_info "Check logs with: journalctl -u timed -f"
        return 1
    fi
}

# Update from git
update_repo() {
    local repo_dir="$1"
    
    print_step "Updating from git repository..."
    cd "$repo_dir"
    
    # Stash any local changes
    if ! git diff-index --quiet HEAD --; then
        print_warn "Local changes detected, stashing..."
        git stash
    fi
    
    # Pull latest changes
    git pull origin main
    
    # Get current commit info
    local commit_hash=$(git rev-parse --short HEAD)
    local commit_date=$(git log -1 --format=%cd --date=short)
    local commit_msg=$(git log -1 --format=%s)
    
    print_success "Repository updated"
    print_info "Commit: $commit_hash ($commit_date)"
    print_info "Message: $commit_msg"
}

# Build binaries
build_binaries() {
    local repo_dir="$1"
    
    print_step "Building binaries (this may take a few minutes)..."
    cd "$repo_dir"
    
    # Clean previous build to ensure fresh compile
    cargo clean --release
    
    # Build in release mode
    if cargo build --release; then
        print_success "Build completed successfully"
    else
        print_error "Build failed"
        exit 1
    fi
}

# Install binaries
install_binaries() {
    local repo_dir="$1"
    
    print_step "Installing binaries..."
    
    # Check if binaries exist
    if [ ! -f "$repo_dir/target/release/timed" ]; then
        print_error "Binary not found: $repo_dir/target/release/timed"
        exit 1
    fi
    
    # Copy binaries
    cp "$repo_dir/target/release/timed" "$BIN_DIR/timed"
    cp "$repo_dir/target/release/time-cli" "$BIN_DIR/time-cli"
    
    # Set permissions
    chmod 755 "$BIN_DIR/timed"
    chmod 755 "$BIN_DIR/time-cli"
    
    print_success "Binaries installed to $BIN_DIR"
    
    # Show versions
    local timed_version=$("$BIN_DIR/timed" --version 2>/dev/null || echo "unknown")
    print_info "timed: $timed_version"
}

# Print summary
print_summary() {
    echo
    echo -e "${GREEN}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${GREEN}║                    UPDATE COMPLETE                         ║${NC}"
    echo -e "${GREEN}╚════════════════════════════════════════════════════════════╝${NC}"
    echo
    echo -e "${BLUE}Installed binaries:${NC}"
    echo "  • $BIN_DIR/timed"
    echo "  • $BIN_DIR/time-cli"
    echo
    echo -e "${BLUE}Useful commands:${NC}"
    echo "  • Check status:    systemctl status timed"
    echo "  • View logs:       journalctl -u timed -f"
    echo "  • Restart service: systemctl restart timed"
    echo "  • Stop service:    systemctl stop timed"
    echo
}

# Main function
main() {
    local repo_dir="${1:-}"
    local service_was_running=false
    
    print_header
    
    # Check root
    check_root
    
    # Find repository
    if [ -z "$repo_dir" ]; then
        repo_dir=$(find_repo)
    elif [ ! -d "$repo_dir" ]; then
        print_error "Directory not found: $repo_dir"
        exit 1
    fi
    
    print_info "Using repository: $repo_dir"
    echo
    
    # Check if service is running
    if check_service_running; then
        service_was_running=true
        stop_service
        echo
    fi
    
    # Update from git
    update_repo "$repo_dir"
    echo
    
    # Build binaries
    build_binaries "$repo_dir"
    echo
    
    # Install binaries
    install_binaries "$repo_dir"
    echo
    
    # Restart service if it was running
    if [ "$service_was_running" = true ]; then
        start_service
        echo
    fi
    
    # Print summary
    print_summary
}

# Run main function
main "$@"
