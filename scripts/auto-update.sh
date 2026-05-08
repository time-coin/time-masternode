#!/bin/bash
# auto-update.sh — Automatic update check for TIME Coin masternode
#
# Compares the local git HEAD against the remote origin/main.
# If a new commit is available, runs update.sh to build and deploy it.
#
# Designed to be run by auto-update.timer (systemd timer) on a schedule.
# Can also be run manually:
#   sudo bash scripts/auto-update.sh [mainnet|testnet|both]
#
# Installation (run as root from ~/time-masternode):
#   sudo cp scripts/auto-update.service /etc/systemd/system/
#   sudo cp scripts/auto-update.timer   /etc/systemd/system/
#   sudo systemctl daemon-reload
#   sudo systemctl enable --now auto-update.timer
#
# Check timer status:
#   systemctl status auto-update.timer
#   journalctl -t time-auto-update -f
#
# Disable auto-updates:
#   sudo systemctl disable --now auto-update.timer
#
# Options:
#   mainnet|testnet|both  Network(s) to update (passed through to update.sh).
#                         Default: both
#   --dry-run             Log what would happen but do not run update.sh
#   --repo DIR            Path to git repo (default: ~/time-masternode)
#   -h, --help            Show this help

set -uo pipefail

TAG="time-auto-update"
log()  { logger -t "$TAG" -- "$*";                         echo "$(date '+%Y-%m-%d %H:%M:%S') $*"; }
logw() { logger -t "$TAG" -p user.warning  -- "WARN  $*"; echo "$(date '+%Y-%m-%d %H:%M:%S') WARN  $*"; }
loge() { logger -t "$TAG" -p user.err      -- "ERROR $*"; echo "$(date '+%Y-%m-%d %H:%M:%S') ERROR $*" >&2; }

# ── Defaults ──────────────────────────────────────────────────────────────────
NETWORK="both"
DRY_RUN=0
REPO_DIR="$HOME/time-masternode"

# ── Argument parsing ──────────────────────────────────────────────────────────
usage() {
    grep '^#' "$0" | grep -v '!/bin/bash' | sed 's/^# \{0,1\}//'
    exit 0
}

while [[ $# -gt 0 ]]; do
    case $1 in
        mainnet|testnet|both) NETWORK="$1"; shift ;;
        --dry-run)            DRY_RUN=1;    shift ;;
        --repo)               REPO_DIR="$2"; shift 2 ;;
        -h|--help)            usage ;;
        *) loge "Unknown option: $1"; usage ;;
    esac
done

# ── Sanity checks ─────────────────────────────────────────────────────────────
if [ ! -d "$REPO_DIR/.git" ]; then
    loge "Repo not found at $REPO_DIR — set --repo or clone the repo first"
    exit 1
fi

UPDATE_SCRIPT="$REPO_DIR/scripts/update.sh"
if [ ! -f "$UPDATE_SCRIPT" ]; then
    loge "update.sh not found at $UPDATE_SCRIPT"
    exit 1
fi

if ! command -v git &>/dev/null; then
    loge "git not found in PATH"
    exit 1
fi

# ── Compare local HEAD vs remote origin/main ──────────────────────────────────
cd "$REPO_DIR"

log "Checking for updates in $REPO_DIR ..."

# Fetch latest refs without modifying the working tree.
if ! git fetch origin main --quiet 2>&1; then
    logw "git fetch failed — skipping update check (network issue?)"
    exit 0
fi

LOCAL_SHA=$(git rev-parse HEAD)
REMOTE_SHA=$(git rev-parse origin/main)

log "Local  HEAD: $LOCAL_SHA"
log "Remote HEAD: $REMOTE_SHA"

if [ "$LOCAL_SHA" = "$REMOTE_SHA" ]; then
    log "Already up to date — no update needed"
    exit 0
fi

# Show which commits are incoming.
INCOMING=$(git log --oneline HEAD..origin/main 2>/dev/null || true)
log "New commits available:"
while IFS= read -r line; do
    log "  $line"
done <<< "$INCOMING"

# ── Run update.sh ─────────────────────────────────────────────────────────────
if [ "$DRY_RUN" -eq 1 ]; then
    log "[DRY RUN] Would run: $UPDATE_SCRIPT $NETWORK"
    exit 0
fi

log "Running update.sh for network=$NETWORK ..."

# update.sh must run as root (it calls systemctl). When invoked by the
# auto-update.timer service (which runs as root) this is already satisfied.
if ! bash "$UPDATE_SCRIPT" "$NETWORK"; then
    loge "update.sh exited with error — check journalctl for details"
    exit 1
fi

log "Update complete."
