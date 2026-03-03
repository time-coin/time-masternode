#!/bin/bash

# Must be run as root
if [ "$EUID" -ne 0 ]; then
  echo "Please run as root."
  exit 1
fi

read -p "Enter new hostname: " NEWNAME

if [ -z "$NEWNAME" ]; then
  echo "Hostname cannot be empty."
  exit 1
fi

echo "Setting hostname to $NEWNAME ..."
hostnamectl set-hostname "$NEWNAME"

echo "Updating /etc/hosts ..."
sed -i "s/127.0.1.1.*/127.0.1.1   $NEWNAME/" /etc/hosts

# Optional: also update any public IP entry if present
if grep -qE "^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\s+.*" /etc/hosts; then
  sed -i "s/^\([0-9]\+\.[0-9]\+\.[0-9]\+\.[0-9]\+\s\+\).*/\1$NEWNAME/" /etc/hosts
fi

echo "Reloading hostname service ..."
systemctl restart systemd-logind.service

echo "Done. Log out and back in to see the new prompt."
