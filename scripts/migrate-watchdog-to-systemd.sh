#!/bin/bash
# migrate-watchdog-to-systemd.sh — Migrate the mn-watchdog from a screen
# session to a proper systemd service.
#
# Run this once on each masternode server that is currently running the
# watchdog via start-watchdog.sh / screen.
#
# What this script does:
#   1. Detects the current network (mainnet or testnet) from time.conf
#   2. Installs mn-watchdog.sh to /usr/local/bin/mn-watchdog
#   3. Writes /etc/systemd/system/mn-watchdog.service (network-aware)
#   4. Enables and starts mn-watchdog via systemctl
#   5. Kills any running "watchdog" screen session
#
# Usage (run as root from the time-masternode directory):
#   sudo bash scripts/migrate-watchdog-to-systemd.sh [--testnet] [--dry-run]
#
# Options:
#   --testnet    Force testnet mode (default: auto-detected from time.conf)
#   --dry-run    Show what would be done without making any changes
#   -h, --help   Show this help

set -uo pipefail

# ── Colours ────────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
BLUE='\033[0;34m'
NC='\033[0m'

ok()   { echo -e "${GREEN}[OK]${NC}    $*"; }
info() { echo -e "${BLUE}[INFO]${NC}  $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC}  $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# ── Argument parsing ───────────────────────────────────────────────────────────
DRY_RUN=0
FORCE_TESTNET=0

usage() {
    grep '^#' "$0" | grep -v '!/bin/bash' | sed 's/^# \{0,1\}//'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --testnet)  FORCE_TESTNET=1; shift ;;
        --dry-run)  DRY_RUN=1;       shift ;;
        -h|--help)  usage ;;
        *) err "Unknown option: $1"; usage ;;
    esac
done

run() {
    # Wrapper: prints the command, executes unless --dry-run.
    if [[ "$DRY_RUN" -eq 1 ]]; then
        echo -e "  ${YELLOW}[DRY-RUN]${NC} $*"
    else
        "$@"
    fi
}

# ── Root check ─────────────────────────────────────────────────────────────────
if [[ $EUID -ne 0 ]]; then
    err "This script must be run as root (use sudo)"
    exit 1
fi

# ── Locate script and project directories ─────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

WATCHDOG_SH="$SCRIPT_DIR/mn-watchdog.sh"
if [[ ! -f "$WATCHDOG_SH" ]]; then
    err "mn-watchdog.sh not found at $WATCHDOG_SH"
    err "Run this script from within the time-masternode/scripts/ directory"
    exit 1
fi

echo ""
echo -e "${BLUE}══════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}   TIME Coin — Migrate Watchdog to systemd            ${NC}"
echo -e "${BLUE}══════════════════════════════════════════════════════${NC}"
echo ""
[[ "$DRY_RUN" -eq 1 ]] && warn "Dry-run mode — no changes will be made"

# ── Detect network ─────────────────────────────────────────────────────────────
NETWORK="mainnet"
SERVICE_NAME="timed"

if [[ "$FORCE_TESTNET" -eq 1 ]]; then
    NETWORK="testnet"
    SERVICE_NAME="timetd"
else
    # Auto-detect from time.conf (testnet=1 anywhere in the mainnet config).
    MAINNET_CONF="/root/.timecoin/time.conf"
    TESTNET_CONF="/root/.timecoin/testnet/time.conf"
    if grep -qsE '^\s*testnet\s*=\s*1' "$MAINNET_CONF" 2>/dev/null; then
        NETWORK="testnet"
        SERVICE_NAME="timetd"
    elif [[ -f "$TESTNET_CONF" ]] && ! grep -qsE '^\s*testnet\s*=\s*0' "$TESTNET_CONF" 2>/dev/null; then
        # testnet.conf exists and doesn't explicitly disable testnet
        : # could be testnet, but mainnet conf didn't say testnet=1 — stay mainnet
    fi
fi

info "Detected network: ${NETWORK} (daemon service: ${SERVICE_NAME}.service)"

# ── Step 1: Verify timed is managed by systemd ────────────────────────────────
info "Checking that ${SERVICE_NAME}.service exists..."
if ! systemctl list-unit-files "${SERVICE_NAME}.service" 2>/dev/null | grep -q "${SERVICE_NAME}"; then
    warn "${SERVICE_NAME}.service not found in systemd."
    warn "The watchdog BindsTo this service — install the daemon service first."
    warn "(Run scripts/install-masternode.sh or create the service manually.)"
    warn "Continuing anyway — the watchdog service will be installed but may not"
    warn "auto-start until ${SERVICE_NAME}.service exists."
fi

# ── Step 2: Check for existing systemd watchdog (already migrated?) ───────────
if systemctl is-enabled --quiet mn-watchdog 2>/dev/null; then
    warn "mn-watchdog.service is already enabled."
    read -rp "Re-install / overwrite it? (y/n) " -n 1
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        info "Nothing to do. Exiting."
        exit 0
    fi
fi

# ── Step 3: Install watchdog binary ───────────────────────────────────────────
info "Installing mn-watchdog.sh -> /usr/local/bin/mn-watchdog"
run cp "$WATCHDOG_SH" /usr/local/bin/mn-watchdog
run chmod +x /usr/local/bin/mn-watchdog

# ── Step 4: Write systemd service file ────────────────────────────────────────
SERVICE_FILE="/etc/systemd/system/mn-watchdog.service"
WATCHDOG_ARGS=""
[[ "$NETWORK" == "testnet" ]] && WATCHDOG_ARGS=" --testnet"

info "Writing ${SERVICE_FILE}"

if [[ "$DRY_RUN" -eq 1 ]]; then
    echo -e "  ${YELLOW}[DRY-RUN]${NC} Would write:"
    cat <<EOF
[Unit]
Description=TIME Coin Masternode Registration Watchdog (${NETWORK})
Documentation=https://github.com/time-coin/time-masternode
After=${SERVICE_NAME}.service
BindsTo=${SERVICE_NAME}.service

[Service]
Type=simple
ExecStart=/usr/local/bin/mn-watchdog${WATCHDOG_ARGS}
Restart=always
RestartSec=10
User=root
StandardOutput=journal
StandardError=journal
SyslogIdentifier=mn-watchdog

[Install]
WantedBy=multi-user.target
EOF
else
    # Back up any existing service file before overwriting.
    if [[ -f "$SERVICE_FILE" ]]; then
        BACKUP="${SERVICE_FILE}.bak.$(date +%Y%m%d%H%M%S)"
        cp "$SERVICE_FILE" "$BACKUP"
        info "Backed up existing service file to $BACKUP"
    fi

    cat > "$SERVICE_FILE" <<EOF
[Unit]
Description=TIME Coin Masternode Registration Watchdog (${NETWORK})
Documentation=https://github.com/time-coin/time-masternode
After=${SERVICE_NAME}.service
BindsTo=${SERVICE_NAME}.service

[Service]
Type=simple
ExecStart=/usr/local/bin/mn-watchdog${WATCHDOG_ARGS}
Restart=always
RestartSec=10
User=root
StandardOutput=journal
StandardError=journal
SyslogIdentifier=mn-watchdog

[Install]
WantedBy=multi-user.target
EOF
fi

# ── Step 5: Enable and start the service ──────────────────────────────────────
info "Reloading systemd daemon..."
run systemctl daemon-reload

info "Enabling mn-watchdog.service..."
run systemctl enable mn-watchdog

info "Starting mn-watchdog.service..."
run systemctl start mn-watchdog

# ── Step 6: Kill the screen session (if any) ──────────────────────────────────
if command -v screen &>/dev/null && screen -list 2>/dev/null | grep -q '\.watchdog'; then
    info "Found a 'watchdog' screen session — killing it..."
    run screen -S watchdog -X quit
    ok "Screen session terminated"
else
    info "No 'watchdog' screen session found — nothing to clean up"
fi

# ── Step 7: Verify ────────────────────────────────────────────────────────────
if [[ "$DRY_RUN" -eq 0 ]]; then
    echo ""
    sleep 2  # give systemd a moment to start the unit
    if systemctl is-active --quiet mn-watchdog; then
        ok "mn-watchdog.service is running"
    else
        warn "mn-watchdog.service does not appear to be active yet"
        warn "Check: journalctl -u mn-watchdog -n 30 --no-pager"
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo -e "${GREEN}══════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}  Migration complete ✅${NC}"
echo -e "${GREEN}══════════════════════════════════════════════════════${NC}"
echo ""
echo "  Useful commands:"
echo "    systemctl status mn-watchdog"
echo "    journalctl -u mn-watchdog -f"
echo "    systemctl stop mn-watchdog    # pause watchdog for maintenance"
echo "    systemctl disable mn-watchdog # permanently disable"
echo ""
