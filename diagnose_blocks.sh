#!/bin/bash
# Comprehensive diagnostic script for TIME Coin block production

echo "=== 1. CHECK IF NEW BINARY IS DEPLOYED ==="
echo "Looking for bootstrap scenario detection message..."
journalctl -u timed --since "5 minutes ago" --no-pager | grep -E "Bootstrap scenario|skip sync"
echo ""

echo "=== 2. CURRENT HEIGHT ==="
journalctl -u timed -n 10 --no-pager | grep -E "height [0-9]+" | tail -5
echo ""

echo "=== 3. SYNC STATUS ==="
journalctl -u timed --since "2 minutes ago" --no-pager | grep -E "syncing|sync coordinator|blocks behind|Bootstrap scenario"
echo ""

echo "=== 4. LEADER SELECTION ==="
journalctl -u timed --since "10 minutes ago" --no-pager | grep -E "leader selection|Selected as block producer|Block [0-9]+ leader"
echo ""

echo "=== 5. BLOCK PRODUCTION ATTEMPTS ==="
journalctl -u timed --since "10 minutes ago" --no-pager | grep -E "Attempting to produce|produced block|Building block|Cannot produce"
echo ""

echo "=== 6. VOTING ACTIVITY ==="
journalctl -u timed --since "5 minutes ago" --no-pager | grep -E "prepare vote|precommit vote|Generated.*vote"
echo ""

echo "=== 7. CONSENSUS STATUS ==="
journalctl -u timed --since "5 minutes ago" --no-pager | grep -E "consensus reached|checking consensus|vote_count"
echo ""

echo "=== 8. BLOCK PROPOSALS/RECEIVED ==="
journalctl -u timed --since "5 minutes ago" --no-pager | grep -E "Received block proposal|Broadcasting block|Block [0-9]+ received"
echo ""

echo "=== 9. MASTERNODE STATUS ==="
journalctl -u timed --since "2 minutes ago" --no-pager | grep -E "registered masternodes|active masternodes|Bootstrap mode.*using"
echo ""

echo "=== 10. ERRORS/WARNINGS ==="
journalctl -u timed --since "5 minutes ago" --no-pager | grep -E "ERROR|WARN|Failed|failed|Error"
echo ""

echo "=== 11. RECENT ACTIVITY (last 30 lines) ==="
journalctl -u timed -n 30 --no-pager
echo ""

echo "=== 12. CHECK FOR DEADLOCK INDICATORS ==="
journalctl -u timed --since "2 minutes ago" --no-pager | grep -E "waiting for|Cannot produce|Skipping|is_syncing"
