#!/bin/bash
cd ~/time-masternode
git stash
git pull origin main
git log -1
#cargo clean
#cargo build --release
cargo build --release --bin timed --bin time-cli
systemctl stop timed
systemctl daemon-reload
rm /usr/local/bin/timed
rm /usr/local/bin/time-cli
cp ~/time-masternode/target/release/timed /usr/local/bin/timed
cp ~/time-masternode/target/release/time-cli /usr/local/bin/time-cli
ls -lh /usr/local/bin/timed  # Should show today's timestamp
systemctl start timed && journalctl -u timed -f | ccze -A
