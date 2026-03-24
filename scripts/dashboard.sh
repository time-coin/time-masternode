#!/bin/bash

# Ensure cargo is in PATH
for CARGO_HOME in /root "$HOME" ; do
    if [ -f "$CARGO_HOME/.cargo/env" ]; then
        source "$CARGO_HOME/.cargo/env"
        break
    fi
done
export PATH="/root/.cargo/bin:$HOME/.cargo/bin:$PATH"

cd ~/time-masternode
cargo run --bin time-dashboard --features dashboard
