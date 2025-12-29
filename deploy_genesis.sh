#!/bin/bash
# Deploy genesis.testnet.json to all servers
# Copy to data directory
cd timecoin

cp genesis.testnet.json ~/.timecoin/testnet/     

echo "✅ Deployed"
echo ""
echo "✅ Deployment complete!"
echo ""
echo "Next steps:"
echo "1. Restart all nodes: sudo systemctl restart timed"
echo "2. Check logs: journalctl -u timed -f"
