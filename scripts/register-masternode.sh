#!/usr/bin/env bash
# register-masternode.sh
#
# Run this on your GUI wallet machine to re-register a masternode
# whose cold wallet is local but whose RPC is on the masternode server.
#
# Usage:
#   ./register-masternode.sh \
#     --wallet     /path/to/wallet.dat \
#     --collateral <txid>:<vout> \
#     --ip         <masternode-public-ip> \
#     --payout     <TIME-reward-address> \
#     --opkey      <operator-pubkey-hex> \
#     --rpc        http://<masternode-ip>:24001
#
# Example (your current situation):
#   ./register-masternode.sh \
#     --wallet     ~/timecoin/wallet.dat \
#     --collateral 3d31a33d8b1e25dcee8ac5bd79440714784159021b4e3fb71979bc76430c9ca3:0 \
#     --ip         50.28.104.50 \
#     --payout     TIME1LeGqigKspRreyGBdSJYuDyz7NFyAZNYtY \
#     --opkey      22d31e086b9079f97c8dae4c77a90379bdf8871c4dd038d3da997294a911efa5 \
#     --rpc        http://50.28.104.50:24001

set -euo pipefail

WALLET=""
COLLATERAL=""
IP=""
PAYOUT=""
OPKEY=""
RPC_URL=""

usage() {
    sed -n '/^# Usage:/,/^$/p' "$0"
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --wallet)     WALLET="$2";     shift 2 ;;
        --collateral) COLLATERAL="$2"; shift 2 ;;
        --ip)         IP="$2";         shift 2 ;;
        --payout)     PAYOUT="$2";     shift 2 ;;
        --opkey)      OPKEY="$2";      shift 2 ;;
        --rpc)        RPC_URL="$2";    shift 2 ;;
        *) echo "Unknown argument: $1"; usage ;;
    esac
done

for var in WALLET COLLATERAL IP PAYOUT OPKEY RPC_URL; do
    if [[ -z "${!var}" ]]; then
        echo "❌ Missing required argument: --$(echo "$var" | tr '[:upper:]' '[:lower:]')"
        usage
    fi
done

if [[ ! -f "$WALLET" ]]; then
    echo "❌ Wallet file not found: $WALLET"
    exit 1
fi

echo "=== Step 1: Export private key from wallet ==="
echo "   wallet: $WALLET"
DUMP=$(time-cli dumpprivkey --wallet-path "$WALLET")
echo "$DUMP"

ADDRESS=$(echo "$DUMP" | awk '/^address:/ { print $2 }')
PRIVKEY=$(echo "$DUMP" | awk '/^privkey:/ { print $2 }')

if [[ -z "$PRIVKEY" ]]; then
    echo "❌ Failed to extract private key"
    exit 1
fi

echo ""
echo "=== Step 2: Submit MasternodeReg to $RPC_URL ==="
echo "   collateral: $COLLATERAL"
echo "   masternode: $IP"
echo "   payout:     $PAYOUT"
echo "   owner:      $ADDRESS"
echo ""

time-cli --rpc-url "$RPC_URL" masternodereg \
    --collateral  "$COLLATERAL" \
    --masternodeip "$IP" \
    --payoutaddress "$PAYOUT" \
    --operator-pubkey "$OPKEY" \
    --privkey "$PRIVKEY"
