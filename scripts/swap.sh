#!/usr/bin/env bash

set -e

SWAPFILE="/swapfile"

echo "[*] Detecting system resources..."

RAM_MB=$(free -m | awk '/^Mem:/{print $2}')
DISK_MB=$(df -m / | awk 'NR==2 {print $4}')

echo "RAM: ${RAM_MB} MB"
echo "Disk free: ${DISK_MB} MB"

# --- Remove existing swap ---
echo "[*] Removing existing swap..."
swapoff -a 2>/dev/null || true
rm -f /swapfile /swapfile.new 2>/dev/null || true
sed -i.bak '/swap/d' /etc/fstab 2>/dev/null || true
echo "[*] Old swap cleared."

# --- Recalculate free disk after removal ---
DISK_MB=$(df -m / | awk 'NR==2 {print $4}')
echo "Disk free after cleanup: ${DISK_MB} MB"

# --- Calculate swap size ---
if [ "$RAM_MB" -lt 2048 ]; then
    SWAP_MB=$((RAM_MB * 2))
elif [ "$RAM_MB" -lt 8192 ]; then
    SWAP_MB=$RAM_MB
else
    SWAP_MB=4096
fi

# 4 GB floor, capped to 50% of free disk
if [ "$SWAP_MB" -lt 4096 ]; then
    SWAP_MB=4096
fi

MAX_SWAP_MB=$((DISK_MB / 2))
if [ "$SWAP_MB" -gt "$MAX_SWAP_MB" ]; then
    SWAP_MB=$MAX_SWAP_MB
    echo "[!] Disk limited swap to ${SWAP_MB} MB"
fi

if [ "$SWAP_MB" -lt 512 ]; then
    echo "[!] Not enough disk for useful swap (${SWAP_MB} MB). Aborting."
    exit 1
fi

echo "[*] Creating ${SWAP_MB} MB swap..."
fallocate -l ${SWAP_MB}M "$SWAPFILE" || dd if=/dev/zero of="$SWAPFILE" bs=1M count=$SWAP_MB
chmod 600 "$SWAPFILE"
mkswap "$SWAPFILE"
swapon "$SWAPFILE"
echo "$SWAPFILE none swap sw 0 0" >> /etc/fstab

# --- Tune system ---
sysctl vm.swappiness=10
sysctl vm.vfs_cache_pressure=50
cat <<EOF >/etc/sysctl.d/99-swap-tuning.conf
vm.swappiness=10
vm.vfs_cache_pressure=50
EOF

echo "[✓] Done: ${SWAP_MB} MB swap active."
