# Loophole

A self-hosted HTTP/HTTPS tunnel (ngrok alternative) written in Rust.

## Features

- **HTTP/HTTPS tunneling**: Expose local services to the internet via custom subdomains
- **Automatic TLS**: Let's Encrypt certificate management via ACME HTTP-01 challenges
- **WebSocket + yamux**: Efficient multiplexed connections over a single WebSocket
- **Token authentication**: Secure access with configurable tokens
- **Colored CLI output**: Beautiful request/response logging with timing
- **QR code support**: Generate QR codes for tunnel URLs (for mobile testing)
- **Admin API**: List and manage active tunnels
- **Production ready**: Graceful shutdown, idle cleanup, request timeouts

## Quick Start

### 1. Start the Server

```bash
cargo run --release --bin tunnel-server -- --config config/server.toml
```

### 2. Start a Local Service

```bash
# Example: Python HTTP server on port 3000
python3 -m http.server 3000
```

### 3. Connect the Client

```bash
cargo run --release --bin tunnel-client -- \
  --server localhost:8090 \
  --token tk_test123 \
  --subdomain myapp \
  --port 3000
```

You'll see:
```
→ Forwarding to 127.0.0.1:3000
✓ Connected to localhost:8090
✓ Tunnel URL: http://myapp.localhost:8090

← GET / (200 OK) 12ms
← GET /favicon.ico (404 Not Found) 3ms
```

### 4. Access Your Service

```bash
curl -H "Host: myapp.localhost" http://localhost:8090/
```

## Installation

### From Source

```bash
git clone https://github.com/your-org/loophole.git
cd loophole
cargo build --release

# Binaries are at:
# - target/release/tunnel-server
# - target/release/tunnel-client
```

### Docker

```bash
# Server
docker build -t loophole-server .
docker run -v /path/to/config:/etc/loophole -p 80:80 -p 443:443 loophole-server

# Client
docker build -f Dockerfile.client -t loophole-client .
docker run loophole-client --server tunnel.example.com --token tk_xxx --subdomain myapp --port 3000
```

### Systemd (Linux)

```bash
# Install
sudo cp target/release/tunnel-server /usr/local/bin/
sudo cp scripts/loophole-server.service /etc/systemd/system/
sudo mkdir -p /etc/loophole /var/lib/loophole/certs
sudo cp config/server.toml /etc/loophole/

# Create service user
sudo useradd --system --no-create-home loophole
sudo chown -R loophole:loophole /var/lib/loophole

# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable loophole-server
sudo systemctl start loophole-server
```

## Configuration

### Server Configuration (`config/server.toml`)

```toml
[server]
domain = "tunnel.example.com"  # Base domain for subdomains
http_port = 80                 # HTTP port (ACME challenges + WebSocket)
https_port = 443               # HTTPS port (tunnel traffic)
control_path = "/_tunnel/connect"

[tokens]
"tk_production" = 5            # token = max_tunnels (0 = unlimited)
"tk_unlimited" = 0

[limits]
request_timeout_secs = 30
max_request_body_bytes = 10485760  # 10MB
idle_tunnel_timeout_secs = 3600    # 1 hour

# TLS with Let's Encrypt (optional)
[acme]
email = "admin@example.com"
directory = "https://acme-v02.api.letsencrypt.org/directory"
certs_dir = "/var/lib/loophole/certs"
staging = false                # Use staging for testing

# Admin API (optional)
[admin]
enabled = true
token = "admin_secret_token"
```

### Client CLI Options

```
tunnel-client [OPTIONS] --server <SERVER> --token <TOKEN> --subdomain <SUBDOMAIN>

Options:
      --server <SERVER>           Tunnel server address (e.g., tunnel.example.com:80)
      --token <TOKEN>             Authentication token
      --subdomain <SUBDOMAIN>     Subdomain to register
      --port <PORT>               Local port to forward to [default: 3000]
      --host <HOST>               Local host to forward to [default: 127.0.0.1]
      --local-host <LOCAL_HOST>   Override Host header for local requests
      --max-retries <N>           Max reconnection attempts (0 = unlimited) [default: 0]
      --forward-timeout <SECS>    Timeout for local forwarding [default: 30]
      --log-level <LEVEL>         Log level: trace, debug, info, warn, error [default: info]
      --quiet                     Suppress request logging
      --qr                        Show QR code for tunnel URL
  -h, --help                      Print help
```

### Environment Variables

- `RUST_LOG`: Set log level (e.g., `RUST_LOG=debug`)

## Admin API

When enabled, the admin API provides endpoints to manage tunnels:

### List Tunnels

```bash
curl -H "Authorization: Bearer admin_secret_token" \
  http://tunnel.example.com/_admin/tunnels
```

Response:
```json
{
  "tunnels": [
    {
      "subdomain": "myapp",
      "created_at_secs": 3600,
      "request_count": 42,
      "idle_secs": 15
    }
  ],
  "count": 1
}
```

### Force Disconnect Tunnel

```bash
curl -X DELETE \
  -H "Authorization: Bearer admin_secret_token" \
  http://tunnel.example.com/_admin/tunnels/myapp
```

## Architecture

```
┌──────────┐                    ┌─────────────────────────────────────────┐
│ Visitor  │─── HTTPS :443 ────►│              Relay Server               │
│ (browser)│                    │                                         │
└──────────┘                    │  ┌─────────┐    ┌──────────────────┐    │
                                │  │ TLS     │───►│   HTTP Router    │    │
                                │  │ (rustls)│    │   (Host header)  │    │
                                │  └─────────┘    └────────┬─────────┘    │
                                │                          │              │
                                │                          ▼              │
                                │  ┌─────────────────────────────────┐    │
                                │  │         Yamux Session           │    │
                                │  │  (multiplexed over WebSocket)   │    │
                                │  └───────────────┬─────────────────┘    │
                                │                  │                      │
                                └──────────────────┼──────────────────────┘
                                                   │
                                                   │ WSS
                                                   │
┌──────────────────────────────────────────────────┼──────────────────────┐
│                           Client CLI             │                      │
│                                                  │                      │
│  ┌──────────┐    ┌──────────┐    ┌─────────────┴──┐    ┌───────────┐  │
│  │ Colored  │◄───│ Forwarder│◄───│ Yamux Session  │◄───│ WebSocket │  │
│  │ Output   │    │          │    │                │    │ Connection│  │
│  └──────────┘    └────┬─────┘    └────────────────┘    └───────────┘  │
│                       │                                               │
│                       ▼                                               │
│                  ┌──────────┐                                         │
│                  │  Local   │                                         │
│                  │  :3000   │                                         │
│                  └──────────┘                                         │
└───────────────────────────────────────────────────────────────────────┘
```

## Troubleshooting

### Client can't connect

1. Check that the server is running: `curl http://server:port/_tunnel/connect`
2. Verify token is correct in server config
3. Check firewall allows WebSocket connections

### Certificate issues

1. Ensure port 80 is accessible for ACME HTTP-01 challenges
2. Check DNS points to your server
3. Try `staging = true` first to avoid rate limits
4. Check logs: `journalctl -u loophole-server -f`

### Tunnel disconnects

1. Check idle timeout in config (default: 1 hour)
2. Use `--max-retries 0` for unlimited reconnection attempts
3. Check server logs for errors

### Slow responses

1. Increase `--forward-timeout` on client
2. Increase `request_timeout_secs` in server config
3. Check local service performance

## Security Considerations

- **Tokens**: Keep authentication tokens secret
- **Admin API**: Use a strong admin token, consider IP allowlisting
- **TLS**: Always use HTTPS in production
- **Local forwarding**: Client defaults to localhost only
- **Subdomains**: Reserved names (www, api, admin, etc.) are blocked

## Development

### Running Tests

```bash
cargo test
```

### Testing with Pebble (Local ACME)

See `PLAN.md` for detailed instructions on testing TLS with Pebble.

## License

MIT
