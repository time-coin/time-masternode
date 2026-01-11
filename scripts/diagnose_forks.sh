#!/bin/bash
# Fork Diagnostic Script for TimeCoin Mainnet
# Run this on each masternode to diagnose fork causes

echo "========================================"
echo "TimeCoin Fork Diagnostic Tool"
echo "========================================"
echo ""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# 1. Check if timed is running
echo "1. Checking if timed is running..."
if pgrep -x "timed" > /dev/null; then
    echo -e "${GREEN}✓ timed is running${NC}"
else
    echo -e "${RED}✗ timed is NOT running${NC}"
    exit 1
fi
echo ""

# 2. Check blockchain height
echo "2. Checking blockchain height..."
HEIGHT=$(./time-cli get-block-count 2>/dev/null)
if [ $? -eq 0 ]; then
    echo -e "${GREEN}✓ Current height: $HEIGHT${NC}"
else
    echo -e "${RED}✗ Could not get block height${NC}"
fi
echo ""

# 3. Check block hash at current height
echo "3. Checking block hash..."
if [ ! -z "$HEIGHT" ]; then
    HASH=$(./time-cli get-block-hash $HEIGHT 2>/dev/null)
    if [ $? -eq 0 ]; then
        echo -e "${GREEN}✓ Hash at height $HEIGHT: ${HASH:0:16}...${NC}"
    else
        echo -e "${RED}✗ Could not get block hash${NC}"
    fi
fi
echo ""

# 4. Check connected peers
echo "4. Checking connected peers..."
PEER_COUNT=$(./time-cli peer list 2>/dev/null | grep -c "Peer:")
if [ $? -eq 0 ]; then
    if [ $PEER_COUNT -ge 3 ]; then
        echo -e "${GREEN}✓ Connected to $PEER_COUNT peers (good for 5-node network)${NC}"
    elif [ $PEER_COUNT -ge 2 ]; then
        echo -e "${YELLOW}⚠ Connected to $PEER_COUNT peers (minimum met, but low)${NC}"
    else
        echo -e "${RED}✗ Only $PEER_COUNT peers connected (need at least 2!)${NC}"
    fi
    echo ""
    echo "Peer details:"
    ./time-cli peer list 2>/dev/null
else
    echo -e "${RED}✗ Could not get peer list${NC}"
fi
echo ""

# 5. Check clock synchronization
echo "5. Checking system clock synchronization..."
if command -v timedatectl &> /dev/null; then
    SYNC_STATUS=$(timedatectl | grep "synchronized:" | awk '{print $4}')
    CURRENT_TIME=$(date -u +"%Y-%m-%d %H:%M:%S UTC")
    
    if [ "$SYNC_STATUS" = "yes" ]; then
        echo -e "${GREEN}✓ System clock is synchronized${NC}"
        echo "  Current time: $CURRENT_TIME"
    else
        echo -e "${RED}✗ System clock is NOT synchronized${NC}"
        echo "  Current time: $CURRENT_TIME"
        echo -e "${YELLOW}  Run: sudo timedatectl set-ntp true${NC}"
    fi
else
    echo -e "${YELLOW}⚠ timedatectl not available, checking date...${NC}"
    date -u
fi
echo ""

# 6. Check network connectivity to other masternodes
echo "6. Checking network connectivity..."
# You should replace these with your actual masternode IPs
MASTERNODES=(
    "lw-michigan.example.com"
    "lw-london.example.com"
    "lw-michigan2.example.com"
    "lw-arizona.example.com"
)

echo "Testing connectivity to known masternodes:"
for mn in "${MASTERNODES[@]}"; do
    # Skip self
    HOSTNAME=$(hostname)
    if [[ "$mn" == *"$HOSTNAME"* ]]; then
        continue
    fi
    
    # Ping test
    if ping -c 1 -W 2 "$mn" &> /dev/null; then
        echo -e "  ${GREEN}✓${NC} $mn is reachable"
    else
        echo -e "  ${RED}✗${NC} $mn is NOT reachable"
    fi
    
    # P2P port test (assuming port 8333)
    if nc -z -w 2 "$mn" 8333 &> /dev/null; then
        echo -e "    ${GREEN}✓${NC} Port 8333 is open"
    else
        echo -e "    ${RED}✗${NC} Port 8333 is NOT accessible"
    fi
done
echo ""

# 7. Check recent log entries for fork warnings
echo "7. Checking recent logs for fork warnings..."
if [ -f "/var/log/timed/timed.log" ]; then
    echo "Recent fork detections (last 10):"
    grep "Fork detected\|MINORITY FORK\|ahead of consensus" /var/log/timed/timed.log | tail -10
    echo ""
    
    echo "Recent sync attempts (last 5):"
    grep "Syncing from\|sync completed\|Failed to sync" /var/log/timed/timed.log | tail -5
else
    echo -e "${YELLOW}⚠ Log file not found at /var/log/timed/timed.log${NC}"
fi
echo ""

# 8. Check masternode status
echo "8. Checking masternode status..."
MN_STATUS=$(./time-cli masternode status 2>/dev/null)
if [ $? -eq 0 ]; then
    echo "$MN_STATUS"
else
    echo -e "${YELLOW}⚠ Could not get masternode status${NC}"
fi
echo ""

# 9. Summary and recommendations
echo "========================================"
echo "SUMMARY & RECOMMENDATIONS"
echo "========================================"
echo ""

# Calculate expected height
GENESIS_TIME=1767225600  # 2026-01-01 00:00:00 UTC
CURRENT_UNIX=$(date +%s)
EXPECTED_HEIGHT=$(( ($CURRENT_UNIX - $GENESIS_TIME) / 600 ))

if [ ! -z "$HEIGHT" ]; then
    BLOCKS_BEHIND=$(( $EXPECTED_HEIGHT - $HEIGHT ))
    
    echo "Expected height based on time: $EXPECTED_HEIGHT"
    echo "Your current height: $HEIGHT"
    
    if [ $BLOCKS_BEHIND -gt 10 ]; then
        echo -e "${RED}⚠ You are $BLOCKS_BEHIND blocks behind schedule!${NC}"
        echo "  This indicates sync issues or frequent forks."
    elif [ $BLOCKS_BEHIND -gt 0 ]; then
        echo -e "${YELLOW}⚠ You are $BLOCKS_BEHIND blocks behind schedule.${NC}"
    else
        echo -e "${GREEN}✓ You are on schedule (or ahead).${NC}"
    fi
fi
echo ""

# Recommendations
echo "RECOMMENDATIONS:"
if [ $PEER_COUNT -lt 3 ]; then
    echo -e "${RED}1. FIX NETWORK CONNECTIVITY${NC}"
    echo "   - Check firewall rules (allow port 8333)"
    echo "   - Verify masternodes can reach each other"
    echo "   - Check peer list in config.toml"
fi

if [ "$SYNC_STATUS" != "yes" ]; then
    echo -e "${RED}2. FIX CLOCK SYNCHRONIZATION${NC}"
    echo "   - Run: sudo timedatectl set-ntp true"
    echo "   - Or: sudo ntpdate pool.ntp.org"
    echo "   - Restart timed after fixing clock"
fi

if [ $BLOCKS_BEHIND -gt 5 ]; then
    echo -e "${YELLOW}3. CONSIDER MANUAL SYNC${NC}"
    echo "   - Check consensus height with other nodes"
    echo "   - If on wrong fork, rollback to consensus height"
    echo "   - Let automatic fork resolution work"
fi

echo ""
echo "For detailed fork analysis, see: FORK_ANALYSIS.md"
echo "For deployment guide, see: FORK_RESOLUTION_DEPLOYMENT.md"
echo ""
