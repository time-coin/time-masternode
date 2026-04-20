#!/usr/bin/env bash

set -e

SWAPFILE="/swapfile"

echo "[*] Detecting system resources..."

# Total RAM in MB
RAM_MB=$(free -m | awk '/^Mem:/{print $2}')

# Disk free space in MB (account for swapfile we're about to delete)
DISK_MB=$(df -m / | awk 'NR==2 {print $4}')

echo "RAM: ${RAM_MB} MB"
echo "Disk free: ${DISK_MB} MB"

# --- Remove existing swap (if any) ---
echo "[*] Checking for existing swap..."

if swapon --show | grep -q "^"; then
    echo "[!] Existing swap detected. Removing..."

    # Check how much swap is actually in use.
    # swapoff moves swap pages back to RAM; if used > free RAM it will OOM-kill.
    SWAP_USED_MB=$(free -m | awk '/^Swap:/{print $3}')
    RAM_FREE_MB=$(free -m | awk '/^Mem:/{print $7}')
    echo "Swap in use: ${SWAP_USED_MB} MB  |  RAM available: ${RAM_FREE_MB} MB"

    if [ "$SWAP_USED_MB" -gt "$RAM_FREE_MB" ]; then
        echo "[!] WARNING: Swap in use (${SWAP_USED_MB} MB) exceeds free RAM (${RAM_FREE_MB} MB)."
        echo "[!] Skipping swapoff to avoid OOM kill. New swap will be added alongside existing."
        SKIP_SWAPOFF=1
    else
        swapoff -a
        if [ -f "$SWAPFILE" ]; then
            DISK_MB=$((DISK_MB + $(du -m "$SWAPFILE" | awk '{print $1}')))
            rm -f "$SWAPFILE"
            echo "[*] Old swapfile removed."
        fi
        sed -i.bak '/swap/d' /etc/fstab
        SKIP_SWAPOFF=0
    fi
else
    SKIP_SWAPOFF=0
fi

# --- Calculate optimal swap ---
if [ "$RAM_MB" -lt 2048 ]; then
    SWAP_MB=$((RAM_MB * 2))
elif [ "$RAM_MB" -lt 8192 ]; then
    SWAP_MB=$RAM_MB
else
    SWAP_MB=4096
fi

# Apply 4 GB floor first, then cap to 50% of free disk (generous but safe)
if [ "$SWAP_MB" -lt 4096 ]; then
    SWAP_MB=4096
fi

MAX_SWAP_MB=$((DISK_MB / 2))
if [ "$SWAP_MB" -gt "$MAX_SWAP_MB" ]; then
    SWAP_MB=$MAX_SWAP_MB
    echo "[!] Disk space limited swap to ${SWAP_MB} MB"
fi

# Absolute minimum: refuse to create less than 512 MB
if [ "$SWAP_MB" -lt 512 ]; then
    echo "[!] Not enough disk space to create a useful swap (${SWAP_MB} MB). Aborting."
    exit 1
fi

echo "[*] Creating new swap: ${SWAP_MB} MB"

# Use a temp name if old swap is still mounted, then swap names
if [ "${SKIP_SWAPOFF:-0}" -eq 1 ]; then
    NEWSWAP="${SWAPFILE}.new"
else
    NEWSWAP="$SWAPFILE"
fi

fallocate -l ${SWAP_MB}M "$NEWSWAP" || dd if=/dev/zero of="$NEWSWAP" bs=1M count=$SWAP_MB

chmod 600 "$NEWSWAP"
mkswap "$NEWSWAP"
swapon "$NEWSWAP"

if [ "${SKIP_SWAPOFF:-0}" -eq 1 ]; then
    # Now that new swap is active, turn off the old one safely
    swapoff -a 2>/dev/null || true
    sed -i.bak '/swap/d' /etc/fstab
    if [ -f "$SWAPFILE" ]; then
        rm -f "$SWAPFILE"
    fi
    mv "$NEWSWAP" "$SWAPFILE"
    swapoff "$SWAPFILE" 2>/dev/null || true
    swapon "$SWAPFILE"
fi

# Add to fstab
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
