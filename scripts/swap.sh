#!/usr/bin/env bash

set -e

SWAPFILE="/swapfile"
NEWSWAP="${SWAPFILE}.new"

echo "[*] Detecting system resources..."

RAM_MB=$(free -m | awk '/^Mem:/{print $2}')
DISK_MB=$(df -m / | awk 'NR==2 {print $4}')

echo "RAM: ${RAM_MB} MB"
echo "Disk free: ${DISK_MB} MB"

# --- Remove existing swap (if any) ---
echo "[*] Checking for existing swap..."

OLD_SWAP_FILE=""
SKIP_SWAPOFF=0

if swapon --show | grep -q "^"; then
    echo "[!] Existing swap detected."

    # Identify the old swapfile path (first swap device listed)
    OLD_SWAP_FILE=$(swapon --show=NAME --noheadings | head -1)

    SWAP_USED_MB=$(free -m | awk '/^Swap:/{print $3}')
    RAM_FREE_MB=$(free -m | awk '/^Mem:/{print $7}')
    echo "Swap in use: ${SWAP_USED_MB} MB  |  RAM available: ${RAM_FREE_MB} MB"

    if [ "$SWAP_USED_MB" -gt "$RAM_FREE_MB" ]; then
        echo "[!] Swap in use exceeds free RAM — will activate new swap first, then remove old."
        SKIP_SWAPOFF=1
    else
        swapoff -a
        if [ -f "$SWAPFILE" ]; then
            DISK_MB=$((DISK_MB + $(du -m "$SWAPFILE" | awk '{print $1}')))
            rm -f "$SWAPFILE"
            echo "[*] Old swapfile removed."
        fi
        sed -i.bak '/swap/d' /etc/fstab
    fi
fi

# --- Calculate swap size ---
if [ "$RAM_MB" -lt 2048 ]; then
    SWAP_MB=$((RAM_MB * 2))
elif [ "$RAM_MB" -lt 8192 ]; then
    SWAP_MB=$RAM_MB
else
    SWAP_MB=4096
fi

# Apply 4 GB floor first
if [ "$SWAP_MB" -lt 4096 ]; then
    SWAP_MB=4096
fi

# Cap to 50% of free disk (disk cap always wins over floor)
MAX_SWAP_MB=$((DISK_MB / 2))
if [ "$SWAP_MB" -gt "$MAX_SWAP_MB" ]; then
    SWAP_MB=$MAX_SWAP_MB
    echo "[!] Disk space limited swap to ${SWAP_MB} MB"
fi

if [ "$SWAP_MB" -lt 512 ]; then
    echo "[!] Not enough disk space to create a useful swap (${SWAP_MB} MB). Aborting."
    exit 1
fi

echo "[*] Creating new swap: ${SWAP_MB} MB"

TARGET="$SWAPFILE"
if [ "$SKIP_SWAPOFF" -eq 1 ]; then
    TARGET="$NEWSWAP"
fi

fallocate -l ${SWAP_MB}M "$TARGET" || dd if=/dev/zero of="$TARGET" bs=1M count=$SWAP_MB
chmod 600 "$TARGET"
mkswap "$TARGET"
swapon "$TARGET"
echo "[*] New swap activated (${SWAP_MB} MB)."

# Now that new swap is live, safely remove the old one.
# The kernel moves old-swap pages → new swap (not back to RAM), so no OOM risk.
if [ "$SKIP_SWAPOFF" -eq 1 ]; then
    if [ -n "$OLD_SWAP_FILE" ] && [ "$OLD_SWAP_FILE" != "$TARGET" ]; then
        echo "[*] Removing old swap ($OLD_SWAP_FILE) — pages migrate to new swap..."
        swapoff "$OLD_SWAP_FILE" && echo "[*] Old swap removed." || echo "[!] Could not remove old swap — leaving both active."
        rm -f "$OLD_SWAP_FILE" 2>/dev/null || true
    fi
    sed -i.bak '/swap/d' /etc/fstab
    # Rename .new → canonical path (safe: kernel tracks swap by inode, not filename)
    mv "$TARGET" "$SWAPFILE"
fi

# Add canonical path to fstab
echo "$SWAPFILE none swap sw 0 0" >> /etc/fstab

# --- Tune system ---
echo "[*] Applying kernel tuning..."
sysctl vm.swappiness=10
sysctl vm.vfs_cache_pressure=50

cat <<EOF >/etc/sysctl.d/99-swap-tuning.conf
vm.swappiness=10
vm.vfs_cache_pressure=50
EOF

echo "[✓] Swap recreated: ${SWAP_MB} MB active."
