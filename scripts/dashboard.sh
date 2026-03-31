#!/bin/bash
# Usage: ./dashboard.sh [mainnet|testnet]
# Default: mainnet

NETWORK="${1:-mainnet}"

# Ensure cargo is in PATH
for CARGO_HOME in /root "$HOME" ; do
    if [ -f "$CARGO_HOME/.cargo/env" ]; then
        source "$CARGO_HOME/.cargo/env"
        break
    fi
done
export PATH="/root/.cargo/bin:$HOME/.cargo/bin:$PATH"

cd ~/time-masternode

if [[ "$NETWORK" == "testnet" ]]; then
    cargo run --bin time-dashboard --features dashboard -- --testnet
else
    cargo run --bin time-dashboard --features dashboard -- --mainnet
fi
