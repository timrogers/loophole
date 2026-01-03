#!/bin/bash
set -e

# Loophole Server Installation Script
# Run as root: curl -fsSL https://example.com/install.sh | sudo bash

VERSION="${VERSION:-latest}"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/loophole"
DATA_DIR="/var/lib/loophole"
USER="loophole"
GROUP="loophole"

echo "Installing Loophole Tunnel Server..."

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo "Error: Please run as root"
    exit 1
fi

# Create user and group
if ! id "$USER" &>/dev/null; then
    echo "Creating user $USER..."
    useradd --system --no-create-home --shell /usr/sbin/nologin "$USER"
fi

# Create directories
echo "Creating directories..."
mkdir -p "$CONFIG_DIR"
mkdir -p "$DATA_DIR/certs"
chown -R "$USER:$GROUP" "$DATA_DIR"
chmod 700 "$DATA_DIR/certs"

# Download binary (placeholder - replace with actual download URL)
echo "Downloading tunnel-server..."
# curl -fsSL "https://github.com/your-org/loophole/releases/download/$VERSION/tunnel-server-linux-amd64" -o "$INSTALL_DIR/tunnel-server"
# chmod +x "$INSTALL_DIR/tunnel-server"

echo "Note: Binary download not implemented. Please build from source:"
echo "  cargo build --release -p tunnel-server"
echo "  cp target/release/tunnel-server $INSTALL_DIR/"

# Create default config if it doesn't exist
if [ ! -f "$CONFIG_DIR/server.toml" ]; then
    echo "Creating default configuration..."
    cat > "$CONFIG_DIR/server.toml" << 'EOF'
[server]
domain = "tunnel.example.com"
http_port = 80
https_port = 443
control_path = "/_tunnel/connect"

[tokens]
# Add your tokens here:
# "tk_your_token" = 5

[limits]
request_timeout_secs = 30
max_request_body_bytes = 10485760
idle_tunnel_timeout_secs = 3600

[acme]
email = "admin@example.com"
directory = "https://acme-v02.api.letsencrypt.org/directory"
certs_dir = "/var/lib/loophole/certs"
staging = false

# Uncomment to enable admin API
# [admin]
# enabled = true
# token = "your_admin_token_here"
EOF
    chown "$USER:$GROUP" "$CONFIG_DIR/server.toml"
    chmod 600 "$CONFIG_DIR/server.toml"
fi

# Install systemd service
echo "Installing systemd service..."
cp "$(dirname "$0")/loophole-server.service" /etc/systemd/system/
systemctl daemon-reload

echo ""
echo "Installation complete!"
echo ""
echo "Next steps:"
echo "1. Edit the configuration: sudo nano $CONFIG_DIR/server.toml"
echo "2. Set your domain and add authentication tokens"
echo "3. Start the service: sudo systemctl start loophole-server"
echo "4. Enable on boot: sudo systemctl enable loophole-server"
echo "5. Check status: sudo systemctl status loophole-server"
echo ""
echo "Logs: journalctl -u loophole-server -f"
