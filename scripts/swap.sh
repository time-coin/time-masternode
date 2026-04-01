#!/usr/bin/env bash

set -e

SWAPFILE="/swapfile"

echo "[*] Detecting system resources..."

# Total RAM in MB
RAM_MB=$(free -m | awk '/^Mem:/{print $2}')

# Disk free space in MB (root partition)
DISK_MB=$(df -m / | awk 'NR==2 {print $4}')

# Existing swap
EXISTING_SWAP=$(swapon --show | wc -l)

echo "RAM: ${RAM_MB} MB"
echo "Disk free: ${DISK_MB} MB"

if [ "$EXISTING_SWAP" -gt 0 ]; then
    echo "[!] Swap already exists. Skipping creation."
    exit 0
fi

# --- Calculate optimal swap ---
if [ "$RAM_MB" -lt 2048 ]; then
    SWAP_MB=$((RAM_MB * 2))
elif [ "$RAM_MB" -lt 8192 ]; then
    SWAP_MB=$RAM_MB
else
    # Cap for cloud servers
    SWAP_MB=4096
fi

# Ensure we don't eat all disk (max 25% of free disk)
MAX_SWAP_MB=$((DISK_MB / 4))
if [ "$SWAP_MB" -gt "$MAX_SWAP_MB" ]; then
    SWAP_MB=$MAX_SWAP_MB
fi

# Minimum safety floor
if [ "$SWAP_MB" -lt 512 ]; then
    SWAP_MB=512
fi

echo "[*] Creating swap: ${SWAP_MB} MB"

# --- Create swap file ---
fallocate -l ${SWAP_MB}M $SWAPFILE || dd if=/dev/zero of=$SWAPFILE bs=1M count=$SWAP_MB

chmod 600 $SWAPFILE
mkswap $SWAPFILE
swapon $SWAPFILE

# Persist in fstab
grep -q "$SWAPFILE" /etc/fstab || echo "$SWAPFILE none swap sw 0 0" >> /etc/fstab

# --- Tune swappiness (important for VPS) ---
echo "[*] Setting swappiness..."

sysctl vm.swappiness=10
sysctl vm.vfs_cache_pressure=50

# Persist sysctl settings
cat <<EOF >/etc/sysctl.d/99-swap-tuning.conf
vm.swappiness=10
vm.vfs_cache_pressure=50
EOF

echo "[✓] Swap setup complete!"
free -h
swapon --show
