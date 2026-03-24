#!/bin/bash
# Usage: sudo ./update.sh [mainnet|testnet|both]
# Default: both

NETWORK="${1:-both}"

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

cd ~/time-masternode
# Discard any local modifications (e.g. CRLF line-ending differences that
# confuse git stash) and pull cleanly.
git checkout -- .
git pull origin main
git log -1

#cargo clean
cargo build --release --bin timed --bin time-cli

for NET in mainnet testnet; do
    [[ "$NETWORK" != "both" && "$NETWORK" != "$NET" ]] && continue

    SVC=$(service_name "$NET")

    echo "==> Updating $NET ($SVC)..."
    systemctl stop "$SVC"
done

systemctl daemon-reload
rm -f /usr/local/bin/timed /usr/local/bin/time-cli
cp ~/time-masternode/target/release/timed /usr/local/bin/timed
cp ~/time-masternode/target/release/time-cli /usr/local/bin/time-cli
ls -lh /usr/local/bin/timed  # Should show today's timestamp

for NET in mainnet testnet; do
    [[ "$NETWORK" != "both" && "$NETWORK" != "$NET" ]] && continue

    SVC=$(service_name "$NET")
    echo "==> Starting $NET ($SVC)..."
    systemctl start "$SVC" && journalctl -u "$SVC" -f | ccze -A
done
