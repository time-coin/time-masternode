#!/bin/bash
# Deploy genesis.testnet.json to all servers

SERVERS=(
  "root@50.28.104.50"
  "root@64.91.241.10"
  "root@69.167.168.176"
  "root@165.232.154.150"
  "root@165.84.215.117"
  "root@178.128.199.144"
)

GENESIS_FILE="genesis.testnet.json"

if [ ! -f "$GENESIS_FILE" ]; then
  echo "❌ Error: $GENESIS_FILE not found in current directory"
  exit 1
fi

echo "📤 Deploying genesis.testnet.json to all servers..."

for server in "${SERVERS[@]}"; do
  echo ""
  echo "📡 Deploying to $server..."
  
  # Copy to testnet data directory
  if scp "$GENESIS_FILE" "$server:/tmp/"; then
    echo "✅ Successfully copied to $server:/tmp/"
    
    # Move to proper locations
    ssh "$server" << 'EOF'
      mkdir -p ~/.timecoin/testnet
      mkdir -p /etc/timecoin
      
      # Copy to data directory
      cp /tmp/genesis.testnet.json ~/.timecoin/testnet/
      
      # Copy to /etc/timecoin
      cp /tmp/genesis.testnet.json /etc/timecoin/
      
      # Copy to executable directory
      cp /tmp/genesis.testnet.json /root/
      
      # Clean up temp
      rm /tmp/genesis.testnet.json
      
      echo "✅ Deployed to all locations"
EOF
  else
    echo "❌ Failed to deploy to $server"
  fi
done

echo ""
echo "✅ Deployment complete!"
echo ""
echo "Next steps:"
echo "1. Restart all nodes: sudo systemctl restart timed"
echo "2. Check logs: journalctl -u timed -f"
