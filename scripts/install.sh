#!/bin/bash
# TIME Coin Node Installation Script

set -e

echo "🚀 TIME Coin Node Installation"
echo "================================"

# Check if running as root
if [[ $EUID -ne 0 ]]; then
   echo "❌ This script must be run as root (use sudo)" 
   exit 1
fi

# Create user
echo "📝 Creating timecoin user..."
if ! id "timecoin" &>/dev/null; then
    useradd -r -m -s /bin/bash timecoin
    echo "✓ User created"
else
    echo "✓ User already exists"
fi

# Create directories
echo "📁 Creating directories..."
mkdir -p /opt/timecoin/data
mkdir -p /etc/timecoin
mkdir -p /var/log/timecoin

# Set permissions
chown -R timecoin:timecoin /opt/timecoin
chown -R timecoin:timecoin /var/log/timecoin

# Copy binary
echo "📦 Installing binary..."
if [ -f "./target/release/timed" ]; then
    cp ./target/release/timed /usr/local/bin/
    chmod +x /usr/local/bin/timed
    echo "✓ Binary installed to /usr/local/bin/"
else
    echo "❌ Binary not found. Run 'cargo build --release' first"
    exit 1
fi

# Copy config
echo "⚙️  Installing configuration..."
if [ ! -f "/etc/timecoin/config.toml" ]; then
    cp config.testnet.toml /etc/timecoin/config.toml
    echo "✓ Config installed to /etc/timecoin/"
else
    echo "⚠  Config already exists, skipping"
fi

# Install systemd service
echo "🔧 Installing systemd service..."
cp timecoin-node.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable timecoin-node
echo "✓ Service installed and enabled"

# Display next steps
echo ""
echo "✅ Installation complete!"
echo ""
echo "Next steps:"
echo "  1. Edit config: sudo nano /etc/timecoin/config.toml"
echo "  2. Start service: sudo systemctl start timed"
echo "  3. Check status: sudo systemctl status timed"
echo "  4. View logs: sudo journalctl -u timed -f"
echo ""
echo "Port 24100: P2P network"
echo "Port 24101: RPC API"
echo ""
