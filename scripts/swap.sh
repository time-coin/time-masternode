#!/usr/bin/env bash

set -e

SWAPFILE="/swapfile"
SWAPFILE_NEW="/swapfile.new"

echo "[*] Detecting system resources..."

RAM_MB=$(free -m | awk '/^Mem:/{print $2}')
DISK_MB=$(df -m / | awk 'NR==2 {print $4}')

echo "RAM: ${RAM_MB} MB"
echo "Disk free: ${DISK_MB} MB"

# --- Calculate swap size up front (before removing old swap) ---
if [ "$RAM_MB" -lt 2048 ]; then
    SWAP_MB=$((RAM_MB * 2))
elif [ "$RAM_MB" -lt 8192 ]; then
    SWAP_MB=$RAM_MB
else
    SWAP_MB=4096
fi

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

# --- Create new swap first, then disable old one ---
# This avoids OOM-killing swapoff: the kernel needs somewhere to move pages
# before it can deactivate the existing swap file.
echo "[*] Creating ${SWAP_MB} MB swap..."
rm -f "$SWAPFILE_NEW"
fallocate -l ${SWAP_MB}M "$SWAPFILE_NEW" || dd if=/dev/zero of="$SWAPFILE_NEW" bs=1M count=$SWAP_MB
chmod 600 "$SWAPFILE_NEW"
mkswap "$SWAPFILE_NEW"
swapon "$SWAPFILE_NEW"

echo "[*] Removing old swap..."
swapoff "$SWAPFILE" 2>/dev/null || true
rm -f "$SWAPFILE" 2>/dev/null || true
mv "$SWAPFILE_NEW" "$SWAPFILE"

DISK_MB=$(df -m / | awk 'NR==2 {print $4}')
echo "Disk free after cleanup: ${DISK_MB} MB"

sed -i.bak '/swap/d' /etc/fstab 2>/dev/null || true
echo "$SWAPFILE none swap sw 0 0" >> /etc/fstab

# --- Tune system ---
sysctl vm.swappiness=10
sysctl vm.vfs_cache_pressure=50
cat <<EOF >/etc/sysctl.d/99-swap-tuning.conf
vm.swappiness=10
vm.vfs_cache_pressure=50
EOF

echo "[✓] Done: ${SWAP_MB} MB swap active."
