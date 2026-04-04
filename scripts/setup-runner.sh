#!/bin/bash
# Sets up a GitHub Actions self-hosted runner on this masternode.
# The runner auto-deploys new code whenever main is pushed.
#
# Usage: sudo bash setup-runner.sh <registration-token> [node-label]
#
# Get a registration token from:
#   https://github.com/wmcorless/time-masternode/settings/actions/runners/new
#
# Example:
#   sudo bash setup-runner.sh AABBCC1234 node-1

set -e

REPO_URL="https://github.com/wmcorless/time-masternode"
RUNNER_DIR="/opt/actions-runner"
RUNNER_USER="runner"

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

TOKEN="${1:-}"
LABEL="${2:-node-$(hostname)}"

if [ -z "$TOKEN" ]; then
    echo -e "${RED}ERROR: Registration token required.${NC}"
    echo ""
    echo "Get one from:"
    echo "  $REPO_URL/settings/actions/runners/new"
    echo ""
    echo "Usage: sudo bash setup-runner.sh <token> [label]"
    exit 1
fi

if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}ERROR: Run with sudo.${NC}"
    exit 1
fi

echo -e "${BLUE}==> Setting up GitHub Actions runner as '$LABEL'...${NC}"

# Create a dedicated user for the runner
if ! id "$RUNNER_USER" &>/dev/null; then
    useradd -m -s /bin/bash "$RUNNER_USER"
    echo -e "${GREEN}✓${NC} Created user '$RUNNER_USER'"
fi

# Detect latest runner version
RUNNER_VERSION=$(curl -s https://api.github.com/repos/actions/runner/releases/latest \
    | grep '"tag_name"' | sed 's/.*"v\([^"]*\)".*/\1/')

if [ -z "$RUNNER_VERSION" ]; then
    echo -e "${RED}ERROR: Could not detect latest runner version. Check network connectivity.${NC}"
    exit 1
fi

echo -e "${GREEN}✓${NC} Latest runner version: $RUNNER_VERSION"

# Download and extract
mkdir -p "$RUNNER_DIR"
cd "$RUNNER_DIR"

ARCHIVE="actions-runner-linux-x64-${RUNNER_VERSION}.tar.gz"
if [ ! -f "$ARCHIVE" ]; then
    echo -e "${BLUE}==> Downloading runner...${NC}"
    curl -fsSL \
        "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/${ARCHIVE}" \
        -o "$ARCHIVE"
fi

tar xzf "$ARCHIVE"
chown -R "$RUNNER_USER:$RUNNER_USER" "$RUNNER_DIR"

# Configure
echo -e "${BLUE}==> Configuring runner...${NC}"
sudo -u "$RUNNER_USER" "$RUNNER_DIR/config.sh" \
    --unattended \
    --url "$REPO_URL" \
    --token "$TOKEN" \
    --name "$LABEL" \
    --labels "$LABEL" \
    --replace

echo -e "${GREEN}✓${NC} Runner configured"

# Allow runner to run the update script as root without a password
SUDOERS_LINE="$RUNNER_USER ALL=(ALL) NOPASSWD: /root/time-masternode/scripts/update.sh"
SUDOERS_FILE="/etc/sudoers.d/time-runner"
echo "$SUDOERS_LINE" > "$SUDOERS_FILE"
chmod 440 "$SUDOERS_FILE"
echo -e "${GREEN}✓${NC} Sudoers entry added"

# Install and start the systemd service
"$RUNNER_DIR/svc.sh" install "$RUNNER_USER"
"$RUNNER_DIR/svc.sh" start

echo ""
echo -e "${GREEN}Runner '$LABEL' is live and listening for deployments.${NC}"
echo ""
echo "Verify at: $REPO_URL/settings/actions/runners"
