#!/usr/bin/env bash

set -e

SWAPFILE="/swapfile"

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

    OLD_SWAP_FILE=$(swapon --show=NAME --noheadings | head -1)

    SWAP_USED_MB=$(free -m | awk '/^Swap:/{print $3}')
    RAM_FREE_MB=$(free -m | awk '/^Mem:/{print $7}')
    echo "Swap in use: ${SWAP_USED_MB} MB  |  RAM available: ${RAM_FREE_MB} MB"

    if [ "$SWAP_USED_MB" -gt "$RAM_FREE_MB" ]; then
        echo "[!] Swap in use exceeds free RAM — will activate new swap first, then remove old."
        SKIP_SWAPOFF=1
        # Use a temp name so we can create the new file while old is still active
        SWAPFILE="/swapfile.new"
    else
        swapoff -a
        if [ -f "$OLD_SWAP_FILE" ]; then
            DISK_MB=$((DISK_MB + $(du -m "$OLD_SWAP_FILE" | awk '{print $1}')))
            rm -f "$OLD_SWAP_FILE"
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

# Apply 4 GB floor, then cap to 50% of free disk (disk always wins)
if [ "$SWAP_MB" -lt 4096 ]; then
    SWAP_MB=4096
fi

MAX_SWAP_MB=$((DISK_MB / 2))
if [ "$SWAP_MB" -gt "$MAX_SWAP_MB" ]; then
    SWAP_MB=$MAX_SWAP_MB
    echo "[!] Disk space limited swap to ${SWAP_MB} MB"
fi

if [ "$SWAP_MB" -lt 512 ]; then
    echo "[!] Not enough disk space for a useful swap (${SWAP_MB} MB). Aborting."
    exit 1
fi

echo "[*] Creating new swap: ${SWAP_MB} MB at ${SWAPFILE}"

fallocate -l ${SWAP_MB}M "$SWAPFILE" || dd if=/dev/zero of="$SWAPFILE" bs=1M count=$SWAP_MB
chmod 600 "$SWAPFILE"
mkswap "$SWAPFILE"
swapon "$SWAPFILE"
echo "[*] New swap activated (${SWAP_MB} MB)."

# New swap is live — now safely remove the old one.
# Kernel migrates old-swap pages to new swap (not RAM), so no OOM risk.
if [ "$SKIP_SWAPOFF" -eq 1 ] && [ -n "$OLD_SWAP_FILE" ]; then
    echo "[*] Removing old swap ($OLD_SWAP_FILE)..."
    if swapoff "$OLD_SWAP_FILE" 2>/dev/null; then
        rm -f "$OLD_SWAP_FILE"
        echo "[*] Old swap removed."
    else
        echo "[!] Could not remove old swap — both remain active."
    fi
    sed -i.bak '/swap/d' /etc/fstab
fi

# Register in fstab using the actual file path
echo "$SWAPFILE none swap sw 0 0" >> /etc/fstab

# --- Tune system ---
echo "[*] Applying kernel tuning..."
sysctl vm.swappiness=10
sysctl vm.vfs_cache_pressure=50

cat <<EOF >/etc/sysctl.d/99-swap-tuning.conf
vm.swappiness=10
vm.vfs_cache_pressure=50
EOF

echo "[✓] Swap setup complete: ${SWAP_MB} MB at ${SWAPFILE}"
