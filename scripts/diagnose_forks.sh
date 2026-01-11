#!/bin/bash
# Fork Diagnostic Script for TimeCoin
# Run this locally on your node to diagnose fork issues

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
# Check multiple config locations in order of priority
# Mainnet: ~/.timecoin/ (base directory)
# Testnet: ~/.timecoin/testnet/ (subdirectory)
CONFIG_LOCATIONS=(
    "$HOME/.timecoin/testnet/config.toml"  # Testnet runtime location
    "$HOME/.timecoin/config.toml"          # Mainnet runtime location
    "./config.toml"                         # Development - repo root
    "../config.toml"                        # Development - subdirectory
)

NETWORK="mainnet"  # default
CONFIG_FILE=""

for config in "${CONFIG_LOCATIONS[@]}"; do
    if [ -f "$config" ]; then
        DETECTED_NET=$(grep '^network = ' "$config" 2>/dev/null | head -1 | cut -d'"' -f2)
        if [ -n "$DETECTED_NET" ]; then
            NETWORK="$DETECTED_NET"
            CONFIG_FILE="$config"
            break
        fi
    fi
done

if [ -z "$CONFIG_FILE" ]; then
    echo -e "${YELLOW}⚠ No config file found, assuming mainnet${NC}"
    NETWORK="mainnet"
else
    echo "Using config: $CONFIG_FILE"
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
echo "RPC URL: $RPC_URL"
echo ""

# Find time-cli binary
if [ -f "./time-cli" ]; then
    CLI="./time-cli --rpc-url $RPC_URL"
elif [ -f "./target/release/time-cli" ]; then
    CLI="./target/release/time-cli --rpc-url $RPC_URL"
elif [ -f "./target/release/time-cli.exe" ]; then
    CLI="./target/release/time-cli.exe --rpc-url $RPC_URL"
elif command -v time-cli &> /dev/null; then
    CLI="time-cli --rpc-url $RPC_URL"
else
    echo -e "${RED}✗ time-cli not found${NC}"
    echo "  Please build it with: cargo build --release"
    CLI=""
fi

# 1. Check if timed is running
echo "1. Checking if timed is running..."
if pgrep -x "timed" > /dev/null 2>&1; then
    echo -e "${GREEN}✓ timed is running${NC}"
    PID=$(pgrep -x "timed")
    echo "  PID: $PID"
elif pgrep -f "timed" > /dev/null 2>&1; then
    echo -e "${GREEN}✓ timed is running${NC}"
    PID=$(pgrep -f "timed" | head -1)
    echo "  PID: $PID"
else
    echo -e "${RED}✗ timed is NOT running${NC}"
    echo "  Start it with: cargo run --release --bin timed"
    exit 1
fi
echo ""

# 2. Check blockchain height
echo "2. Checking blockchain height..."
if [ -n "$CLI" ]; then
    HEIGHT_OUTPUT=$($CLI get-block-count 2>&1)
    HEIGHT=$(echo "$HEIGHT_OUTPUT" | grep -o '[0-9]\+' | head -1)
    if [ -n "$HEIGHT" ] && [ "$HEIGHT" -gt 0 ] 2>/dev/null; then
        echo -e "${GREEN}✓ Current height: $HEIGHT${NC}"
    else
        echo -e "${RED}✗ Could not get block height${NC}"
        echo "  Error: $HEIGHT_OUTPUT"
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
    BLOCK_OUTPUT=$($CLI get-block $HEIGHT 2>&1)
    HASH=$(echo "$BLOCK_OUTPUT" | grep -oP '"hash":\s*"\K[^"]+' | head -1)
    if [ -n "$HASH" ]; then
        echo -e "${GREEN}✓ Hash at height $HEIGHT: ${HASH:0:16}...${NC}"
    else
        echo -e "${YELLOW}⚠ Could not get block hash${NC}"
    fi
else
    echo -e "${YELLOW}⚠ Skipping (height not available)${NC}"
fi
echo ""

# 4. Check peer/masternode information
echo "4. Checking peer/masternode information..."
if [ -n "$CLI" ]; then
    PEER_OUTPUT=$($CLI get-peer-info 2>&1)
    # Count masternodes (each has "addr" field)
    PEER_COUNT=$(echo "$PEER_OUTPUT" | grep -o '"addr"' | wc -l)
    
    if [ "$PEER_COUNT" -gt 0 ]; then
        if [ "$PEER_COUNT" -ge 4 ]; then
            echo -e "${GREEN}✓ Found $PEER_COUNT masternodes (good for 5-node network)${NC}"
        elif [ "$PEER_COUNT" -ge 3 ]; then
            echo -e "${YELLOW}⚠ Found $PEER_COUNT masternodes (okay, but could be better)${NC}"
        else
            echo -e "${RED}✗ Only $PEER_COUNT masternode(s) (need at least 3!)${NC}"
        fi
        echo ""
        echo "Masternode details:"
        echo "$PEER_OUTPUT"
    else
        echo -e "${RED}✗ No masternodes found${NC}"
        echo "  Error: $PEER_OUTPUT"
        PEER_COUNT=0
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

# 6. Check P2P port status (local only - no SSH to other nodes)
echo "6. Checking P2P port accessibility..."
if command -v netstat &> /dev/null; then
    LISTENING=$(netstat -an 2>/dev/null | grep ":$P2P_PORT " | grep "LISTEN")
    if [ -n "$LISTENING" ]; then
        echo -e "${GREEN}✓ P2P port $P2P_PORT is listening${NC}"
        echo "$LISTENING"
    else
        echo -e "${RED}✗ P2P port $P2P_PORT is NOT listening${NC}"
        echo "  Check if timed is properly configured"
    fi
elif command -v ss &> /dev/null; then
    LISTENING=$(ss -an 2>/dev/null | grep ":$P2P_PORT " | grep "LISTEN")
    if [ -n "$LISTENING" ]; then
        echo -e "${GREEN}✓ P2P port $P2P_PORT is listening${NC}"
        echo "$LISTENING"
    else
        echo -e "${RED}✗ P2P port $P2P_PORT is NOT listening${NC}"
        echo "  Check if timed is properly configured"
    fi
else
    echo -e "${YELLOW}⚠ netstat/ss not available, skipping port check${NC}"
fi
echo ""

# 7. Check recent log entries for fork warnings
echo "7. Checking recent logs for fork warnings..."
# Mainnet: ~/.timecoin/logs/
# Testnet: ~/.timecoin/testnet/logs/
LOG_PATHS=(
    "$HOME/.timecoin/testnet/logs/timed.log"  # Testnet default
    "$HOME/.timecoin/logs/timed.log"          # Mainnet default
    "/var/log/timed/timed.log"                 # System log location
    "./logs/testnet-node.log"                  # Development
    "./logs/mainnet-node.log"                  # Development
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
    
    # Check if there are any fork-related messages
    FORK_COUNT=$(grep -c "Fork detected\|MINORITY FORK\|ahead of consensus" "$LOG_FILE" 2>/dev/null || echo "0")
    if [ "$FORK_COUNT" -gt 0 ]; then
        echo "Recent fork detections (last 10):"
        grep "Fork detected\|MINORITY FORK\|ahead of consensus" "$LOG_FILE" 2>/dev/null | tail -10
        echo ""
    else
        echo "No fork warnings found (this is good!)"
    fi
    
    # Check sync status
    SYNC_COUNT=$(grep -c "Syncing from\|sync completed\|Failed to sync" "$LOG_FILE" 2>/dev/null || echo "0")
    if [ "$SYNC_COUNT" -gt 0 ]; then
        echo "Recent sync attempts (last 5):"
        grep "Syncing from\|sync completed\|Failed to sync" "$LOG_FILE" 2>/dev/null | tail -5
    fi
else
    echo -e "${YELLOW}⚠ Log file not found in common locations${NC}"
    echo "  Checked:"
    for path in "${LOG_PATHS[@]}"; do
        echo "    - $path"
    done
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
    echo -e "${RED}1. FIX MASTERNODE CONNECTIVITY${NC}"
    echo "   - Only $PEER_COUNT masternodes visible"
    echo "   - Check if other masternodes are running"
    echo "   - Verify P2P port $P2P_PORT is accessible"
    echo "   - Check firewall rules on YOUR node"
    echo "   - Check network configuration (NAT, routing)"
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
