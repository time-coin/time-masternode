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

# Detect network type and set ports
if [ -f "config.toml" ]; then
    NETWORK=$(grep '^network = ' config.toml | head -1 | cut -d'"' -f2)
elif [ -f "../config.toml" ]; then
    NETWORK=$(grep '^network = ' ../config.toml | head -1 | cut -d'"' -f2)
else
    NETWORK="mainnet"  # default
fi

if [ "$NETWORK" = "mainnet" ]; then
    P2P_PORT=24000
    RPC_PORT=24001
    RPC_URL="http://127.0.0.1:24001"
else
    P2P_PORT=24100
    RPC_PORT=24101
    RPC_URL="http://127.0.0.1:24101"
fi

echo "Network: $NETWORK"
echo "P2P Port: $P2P_PORT"
echo "RPC Port: $RPC_PORT"
echo ""

# Find time-cli binary
if [ -f "./time-cli" ]; then
    CLI="./time-cli --rpc-url $RPC_URL"
elif [ -f "./target/release/time-cli" ]; then
    CLI="./target/release/time-cli --rpc-url $RPC_URL"
elif command -v time-cli &> /dev/null; then
    CLI="time-cli --rpc-url $RPC_URL"
else
    echo -e "${RED}✗ time-cli not found${NC}"
    echo "  Please build it with: cargo build --release"
    CLI=""
fi

# 1. Check if timed is running
echo "1. Checking if timed is running..."
if pgrep -x "timed" > /dev/null; then
    echo -e "${GREEN}✓ timed is running${NC}"
    PID=$(pgrep -x "timed")
    echo "  PID: $PID"
else
    echo -e "${RED}✗ timed is NOT running${NC}"
    exit 1
fi
echo ""

# 2. Check blockchain height
echo "2. Checking blockchain height..."
if [ -n "$CLI" ]; then
    HEIGHT=$($CLI get-block-count 2>/dev/null | grep -o '[0-9]*' | head -1)
    if [ -n "$HEIGHT" ]; then
        echo -e "${GREEN}✓ Current height: $HEIGHT${NC}"
    else
        echo -e "${RED}✗ Could not get block height (RPC may not be ready)${NC}"
        HEIGHT=""
    fi
else
    echo -e "${YELLOW}⚠ Skipping (time-cli not available)${NC}"
    HEIGHT=""
fi
echo ""

# 3. Check block hash at current height
echo "3. Checking block hash..."
if [ -n "$CLI" ] && [ -n "$HEIGHT" ]; then
    HASH=$($CLI get-block $HEIGHT 2>/dev/null | grep -oP '"hash":\s*"\K[^"]+' | head -1)
    if [ -n "$HASH" ]; then
        echo -e "${GREEN}✓ Hash at height $HEIGHT: ${HASH:0:16}...${NC}"
    else
        echo -e "${YELLOW}⚠ Could not get block hash${NC}"
    fi
else
    echo -e "${YELLOW}⚠ Skipping (height not available)${NC}"
fi
echo ""

# 4. Check connected peers
echo "4. Checking connected peers..."
if [ -n "$CLI" ]; then
    PEER_OUTPUT=$($CLI get-peer-info 2>/dev/null)
    PEER_COUNT=$(echo "$PEER_OUTPUT" | grep -c "addr" 2>/dev/null || echo "0")
    
    if [ "$PEER_COUNT" -gt 0 ]; then
        if [ "$PEER_COUNT" -ge 3 ]; then
            echo -e "${GREEN}✓ Connected to $PEER_COUNT peers (good for 5-node network)${NC}"
        elif [ "$PEER_COUNT" -ge 2 ]; then
            echo -e "${YELLOW}⚠ Connected to $PEER_COUNT peers (minimum met, but low)${NC}"
        else
            echo -e "${RED}✗ Only $PEER_COUNT peer(s) connected (need at least 2!)${NC}"
        fi
        echo ""
        echo "Peer details:"
        echo "$PEER_OUTPUT" | head -20
    else
        echo -e "${RED}✗ No peers connected or could not get peer list${NC}"
    fi
else
    echo -e "${YELLOW}⚠ Skipping (time-cli not available)${NC}"
    PEER_COUNT=0
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

# Detect network type and port from config
if [ -f "config.toml" ]; then
    NETWORK=$(grep '^network = ' config.toml | head -1 | cut -d'"' -f2)
elif [ -f "../config.toml" ]; then
    NETWORK=$(grep '^network = ' ../config.toml | head -1 | cut -d'"' -f2)
else
    NETWORK="mainnet"  # default
fi

if [ "$NETWORK" = "mainnet" ]; then
    P2P_PORT=24000
    RPC_PORT=24001
else
    P2P_PORT=24100
    RPC_PORT=24101
fi

echo "Detected network: $NETWORK (P2P port: $P2P_PORT)"
echo ""
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
    
    # P2P port test (use detected port)
    if nc -z -w 2 "$mn" $P2P_PORT &> /dev/null; then
        echo -e "    ${GREEN}✓${NC} Port $P2P_PORT is open"
    else
        echo -e "    ${RED}✗${NC} Port $P2P_PORT is NOT accessible"
    fi
done
echo ""

# 7. Check recent log entries for fork warnings
echo "7. Checking recent logs for fork warnings..."
LOG_PATHS=(
    "/var/log/timed/timed.log"
    "./logs/mainnet-node.log"
    "./logs/testnet-node.log"
    "$HOME/.timecoin/logs/timed.log"
)

LOG_FILE=""
for path in "${LOG_PATHS[@]}"; do
    if [ -f "$path" ]; then
        LOG_FILE="$path"
        break
    fi
done

if [ -n "$LOG_FILE" ]; then
    echo "Log file: $LOG_FILE"
    echo ""
    echo "Recent fork detections (last 10):"
    grep "Fork detected\|MINORITY FORK\|ahead of consensus" "$LOG_FILE" 2>/dev/null | tail -10
    echo ""
    
    echo "Recent sync attempts (last 5):"
    grep "Syncing from\|sync completed\|Failed to sync" "$LOG_FILE" 2>/dev/null | tail -5
else
    echo -e "${YELLOW}⚠ Log file not found in common locations${NC}"
    echo "  Checked: /var/log/timed/, ./logs/, ~/.timecoin/logs/"
fi
echo ""

# 8. Check masternode status
echo "8. Checking masternode status..."
if [ -n "$CLI" ]; then
    MN_STATUS=$($CLI masternode-status 2>/dev/null)
    if [ -n "$MN_STATUS" ]; then
        echo "$MN_STATUS"
    else
        echo -e "${YELLOW}⚠ Could not get masternode status (may not be a masternode)${NC}"
    fi
else
    echo -e "${YELLOW}⚠ Skipping (time-cli not available)${NC}"
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

if [ -n "$HEIGHT" ] && [ "$HEIGHT" -gt 0 ]; then
    BLOCKS_BEHIND=$(( $EXPECTED_HEIGHT - $HEIGHT ))
    
    echo "Expected height based on time: $EXPECTED_HEIGHT"
    echo "Your current height: $HEIGHT"
    
    if [ "$BLOCKS_BEHIND" -gt 10 ]; then
        echo -e "${RED}⚠ You are $BLOCKS_BEHIND blocks behind schedule!${NC}"
        echo "  This indicates sync issues or frequent forks."
    elif [ "$BLOCKS_BEHIND" -gt 0 ]; then
        echo -e "${YELLOW}⚠ You are $BLOCKS_BEHIND blocks behind schedule.${NC}"
    else
        echo -e "${GREEN}✓ You are on schedule (or ahead).${NC}"
    fi
else
    echo -e "${YELLOW}⚠ Could not determine if node is on schedule (height unknown)${NC}"
    BLOCKS_BEHIND=0
fi
echo ""

# Recommendations
echo "RECOMMENDATIONS:"

if [ -z "$CLI" ]; then
    echo -e "${RED}0. BUILD CLI TOOLS${NC}"
    echo "   - Run: cargo build --release"
    echo "   - This will build time-cli in target/release/"
fi

if [ "$PEER_COUNT" -lt 3 ] || [ -z "$PEER_COUNT" ]; then
    echo -e "${RED}1. FIX NETWORK CONNECTIVITY${NC}"
    echo "   - Check firewall rules (allow port $P2P_PORT)"
    echo "   - Verify masternodes can reach each other"
    echo "   - Check peer list in config"
    echo "   - Test: nc -zv <masternode_ip> $P2P_PORT"
fi

if [ "$SYNC_STATUS" != "yes" ]; then
    echo -e "${RED}2. FIX CLOCK SYNCHRONIZATION${NC}"
    echo "   - Run: sudo timedatectl set-ntp true"
    echo "   - Or: sudo ntpdate pool.ntp.org"
    echo "   - Restart timed after fixing clock"
fi

if [ -n "$BLOCKS_BEHIND" ] && [ "$BLOCKS_BEHIND" -gt 5 ]; then
    echo -e "${YELLOW}3. CONSIDER MANUAL SYNC${NC}"
    echo "   - Check consensus height with other nodes"
    echo "   - If on wrong fork, rollback to consensus height"
    echo "   - Let automatic fork resolution work"
fi

echo ""
echo "For detailed fork analysis, see: FORK_ANALYSIS.md"
echo "For deployment guide, see: FORK_RESOLUTION_DEPLOYMENT.md"
echo ""
