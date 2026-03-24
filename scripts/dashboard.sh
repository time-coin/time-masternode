#!/bin/bash

# Ensure cargo is in PATH
REAL_HOME="${HOME:-/root}"
if [ "$SUDO_USER" ] && [ "$SUDO_USER" != "root" ]; then
    REAL_HOME=$(eval echo "~$SUDO_USER")
fi
if [ -f "$REAL_HOME/.cargo/env" ]; then
    source "$REAL_HOME/.cargo/env"
fi
export PATH="$REAL_HOME/.cargo/bin:$PATH"

cd ~/time-masternode
cargo run --bin time-dashboard --features dashboard
