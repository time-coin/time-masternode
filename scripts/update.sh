#!/bin/bash
# Usage:
#   sudo ./update.sh [mainnet|testnet|both]        — build + deploy update
#   sudo ./update.sh resync [mainnet|testnet|both] — recover node stuck on deep fork
# Default network: both

# Ensure cargo is in PATH (sudo doesn't inherit user's PATH).
# install-masternode.sh installs Rust to /root/.cargo/, so check there first.
for CARGO_HOME in /root "$HOME" ; do
    if [ -f "$CARGO_HOME/.cargo/env" ]; then
        source "$CARGO_HOME/.cargo/env"
        break
    fi
done
export PATH="/root/.cargo/bin:$HOME/.cargo/bin:$PATH"

# Derive service name from network
service_name() {
    [[ "$1" == "testnet" ]] && echo "timetd" || echo "timed"
}

# Recover a node stuck on a deep fork:
#   1. Reset BFT finality lock (clears the guard that blocks deep rollbacks)
#   2. Roll back to genesis and resync from whitelisted peers
# The daemon must be running — these are RPC calls.
do_resync() {
    local net="$1"
    local flag=""
    [[ "$net" == "testnet" ]] && flag="--testnet"
    local svc
    svc=$(service_name "$net")

    echo "==> Recovering $net from deep fork..."

    if ! systemctl is-active --quiet "$svc"; then
        echo "    $svc is not running — starting it..."
        systemctl start "$svc"
        sleep 5
    fi

    echo "    Resetting finality lock to 0..."
    time-cli $flag resetfinalitylock 0

    echo "    Rolling back to genesis and resyncing from whitelisted peers..."
    time-cli $flag resyncfromwhitelist 0

    echo "==> $net resync initiated. Node will rebuild from genesis via whitelisted peers."
    echo "    Monitor progress: journalctl -u $svc -f"
}

if [[ "$1" == "resync" ]]; then
    NETWORK="${2:-both}"
    for NET in mainnet testnet; do
        [[ "$NETWORK" != "both" && "$NETWORK" != "$NET" ]] && continue
        do_resync "$NET"
    done
    exit 0
fi

NETWORK="${1:-both}"

cd ~/time-masternode
# Discard any local modifications (e.g. CRLF line-ending differences that
# confuse git stash) and pull cleanly.
git checkout -- .
git pull origin main
git log -1

#cargo clean
cargo build --profile release-fast --bin timed --bin time-cli

systemctl stop mn-watchdog 2>/dev/null || true

for NET in mainnet testnet; do
    [[ "$NETWORK" != "both" && "$NETWORK" != "$NET" ]] && continue

    SVC=$(service_name "$NET")

    echo "==> Updating $NET ($SVC)..."
    systemctl stop "$SVC"
done

systemctl daemon-reload
rm -f /usr/local/bin/timed /usr/local/bin/time-cli
cp ~/time-masternode/target/release-fast/timed /usr/local/bin/timed
cp ~/time-masternode/target/release-fast/time-cli /usr/local/bin/time-cli
ls -lh /usr/local/bin/timed  # Should show today's timestamp

for NET in mainnet testnet; do
    [[ "$NETWORK" != "both" && "$NETWORK" != "$NET" ]] && continue

    SVC=$(service_name "$NET")

    # Touch the reindex sentinel file so the daemon automatically runs a full
    # UTXO + transaction reindex on startup — no manual `time-cli reindex` needed.
    DATA_DIR="/root/.timecoin"
    [[ "$NET" == "testnet" ]] && DATA_DIR="/root/.timecoin/testnet"
    mkdir -p "$DATA_DIR"
    touch "$DATA_DIR/reindex_requested"
    echo "    Reindex sentinel created — daemon will reindex on startup"

    echo "==> Starting $NET ($SVC)..."
    systemctl start "$SVC"
    # Only follow the journal when running interactively.
    # When called from auto-update.timer, stdout is not a TTY, so we skip
    # this — journalctl -f blocks forever and prevents the oneshot service
    # from ever completing (which stops all future timer runs).
    if [ -t 1 ]; then
        journalctl -u "$SVC" -f | ccze -A
    fi
done

if systemctl is-enabled --quiet mn-watchdog 2>/dev/null; then
    echo "==> Restarting mn-watchdog..."
    systemctl start mn-watchdog
fi
