#!/usr/bin/env bash

set -e

SWAPFILE="/swapfile"

echo "[*] Detecting system resources..."

# Total RAM in MB
RAM_MB=$(free -m | awk '/^Mem:/{print $2}')

# Disk free space in MB
DISK_MB=$(df -m / | awk 'NR==2 {print $4}')

echo "RAM: ${RAM_MB} MB"
echo "Disk free: ${DISK_MB} MB"

# --- Remove existing swap (if any) ---
echo "[*] Checking for existing swap..."

if swapon --show | grep -q "^"; then
    echo "[!] Existing swap detected. Removing..."

    # Turn off all swap
    swapoff -a

    # Remove swapfile if it exists
    if [ -f "$SWAPFILE" ]; then
        rm -f $SWAPFILE
        echo "[*] Old swapfile removed."
    fi

    # Remove any swap entries from fstab
    sed -i.bak '/swap/d' /etc/fstab
fi

# --- Calculate optimal swap ---
if [ "$RAM_MB" -lt 2048 ]; then
    SWAP_MB=$((RAM_MB * 2))
elif [ "$RAM_MB" -lt 8192 ]; then
    SWAP_MB=$RAM_MB
else
    SWAP_MB=4096
fi

# Limit to 25% of free disk
MAX_SWAP_MB=$((DISK_MB / 4))
if [ "$SWAP_MB" -gt "$MAX_SWAP_MB" ]; then
    SWAP_MB=$MAX_SWAP_MB
fi

# Minimum floor
if [ "$SWAP_MB" -lt 2253 ]; then
    SWAP_MB=2253
fi

echo "[*] Creating new swap: ${SWAP_MB} MB"

# --- Create swap file ---
fallocate -l ${SWAP_MB}M $SWAPFILE || dd if=/dev/zero of=$SWAPFILE bs=1M count=$SWAP_MB

chmod 600 $SWAPFILE
mkswap $SWAPFILE
swapon $SWAPFILE

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

echo "[✓] Swap recreated successfully!"
