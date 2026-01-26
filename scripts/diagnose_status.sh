#!/bin/bash
# Quick diagnostic script to check blockchain/mempool status

CLI="${CLI_PATH:-time-cli}"

echo "=== BLOCKCHAIN STATUS ==="
$CLI getblockchaininfo 2>&1 | jq -r '{height: .blocks, bestblockhash: .bestblockhash}'

echo ""
echo "=== MEMPOOL STATUS ==="
$CLI getmempoolinfo 2>&1

echo ""
echo "=== PENDING TRANSACTIONS ==="
$CLI getrawmempool false 2>&1

echo ""
echo "=== CONNECTED PEERS ==="
$CLI getpeerinfo 2>&1 | jq -r '.[] | {address: .addr, height: .height}'

echo ""
echo "=== MASTERNODE STATUS ==="
$CLI masternodestatus 2>&1 | jq -r '{status: .status, tier: .tier, is_active: .is_active}'

echo ""
echo "=== CONSENSUS INFO ==="
$CLI getconsensusinfo 2>&1
