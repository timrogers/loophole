# Loophole

A self-hosted HTTP/HTTPS tunnel server and client (ngrok alternative) written in Rust.

## Features

- **HTTP/HTTPS tunneling**: Expose local services to the internet via custom subdomains
- **Automatic TLS**: Let's Encrypt certificate management via ACME HTTP-01 challenges
- **Secure connections**: Client-server communication over encrypted WebSocket (wss://)
- **WebSocket + yamux**: Efficient multiplexed connections over a single WebSocket
- **Token authentication**: Secure access with configurable tokens
- **Colored CLI output**: Beautiful request/response logging with timing
- **QR code support**: Generate QR codes for tunnel URLs (for mobile testing)
- **Admin API**: List and manage active tunnels
- **Production ready**: Graceful shutdown, idle cleanup, request timeouts, systemd integration

## Quick Start

### Server Setup

```bash
# Initialize server configuration (interactive)
sudo loophole init

# Or with arguments
sudo loophole init --domain tunnel.example.com --email admin@example.com

# Start the server (if not using systemd)
loophole server
```

The `init` command will:
1. Create a configuration file at `/etc/loophole/server.toml`
2. Generate an admin token
3. Optionally install and start a systemd service

### Client Setup

```bash
# Login to the server (saves credentials locally)
loophole login --server https://tunnel.example.com --token tk_your_token

# Test the connection
loophole test

# Expose a local service
loophole expose --subdomain myapp --port 3000
```

You'll see:
```
✓ Connected to tunnel.example.com
✓ Tunnel URL: https://myapp.tunnel.example.com

← GET / (200 OK) 12ms
← GET /favicon.ico (404 Not Found) 3ms
```

## Installation

### From Source

```bash
git clone https://github.com/your-org/loophole.git
cd loophole
cargo build --release

# Install the binary
sudo cp target/release/loophole /usr/local/bin/
```

### Docker

The server can be configured entirely via environment variables, making it ideal for Docker/Kubernetes deployments.

```bash
# Build the image
docker build -t loophole .

# Run with HTTPS (Let's Encrypt)
docker run -d \
  -e LOOPHOLE_DOMAIN=tunnel.example.com \
  -e LOOPHOLE_TOKENS=tk_client1,tk_client2 \
  -e LOOPHOLE_ACME_EMAIL=admin@example.com \
  -v loophole-certs:/var/lib/loophole/certs \
  -p 80:80 -p 443:443 \
  loophole
```

#### Docker Compose

```yaml
services:
  loophole:
    build: .
    ports:
      - "80:80"
      - "443:443"
    environment:
      LOOPHOLE_DOMAIN: tunnel.example.com
      LOOPHOLE_TOKENS: tk_client1,tk_client2
      LOOPHOLE_ACME_EMAIL: admin@example.com
    volumes:
      - loophole-certs:/var/lib/loophole/certs

volumes:
  loophole-certs:
```

#### Environment Variables

| Variable | Required | Description | Default |
|----------|----------|-------------|---------|
| `LOOPHOLE_DOMAIN` | Yes | Base domain for tunnels | - |
| `LOOPHOLE_TOKENS` | Yes | Comma-separated client tokens | - |
| `LOOPHOLE_ACME_EMAIL` | Yes | Let's Encrypt email | - |
| `LOOPHOLE_ADMIN_TOKENS` | No | Comma-separated admin tokens | - |
| `LOOPHOLE_ACME_STAGING` | No | Use Let's Encrypt staging | `false` |
| `LOOPHOLE_HTTP_PORT` | No | HTTP port | `80` |
| `LOOPHOLE_HTTPS_PORT` | No | HTTPS port | `443` |
| `LOOPHOLE_CERTS_DIR` | No | Certificate storage path | `/var/lib/loophole/certs` |
| `LOOPHOLE_REQUEST_TIMEOUT_SECS` | No | Request timeout | `30` |
| `LOOPHOLE_IDLE_TUNNEL_TIMEOUT_SECS` | No | Idle tunnel timeout | `3600` |

#### HTTP-only Mode (Advanced)

For development or behind a reverse proxy, you can run without HTTPS by omitting `LOOPHOLE_ACME_EMAIL`:

```bash
docker run -d \
  -e LOOPHOLE_DOMAIN=localhost \
  -e LOOPHOLE_TOKENS=tk_dev \
  -p 80:80 \
  loophole
```
| `LOOPHOLE_IDLE_TUNNEL_TIMEOUT_SECS` | No | Idle tunnel timeout | `3600` |

## CLI Reference

### `loophole init`

Initialize a new server configuration.

```
loophole init [OPTIONS]

Options:
      --domain <DOMAIN>  Domain for tunnels (e.g., tunnel.example.com)
      --email <EMAIL>    Email for Let's Encrypt certificates
  -o, --output <OUTPUT>  Output path for config file [default: /etc/loophole/server.toml]
      --install          Install and enable systemd service
```

### `loophole server`

Run the tunnel server.

```
loophole server [OPTIONS]

Options:
  -c, --config <CONFIG>        Path to configuration file [default: /etc/loophole/server.toml]
      --log-level <LOG_LEVEL>  Log level: trace, debug, info, warn, error [default: info]
```

### `loophole login`

Login to a tunnel server. Credentials are saved to `~/.config/loophole/config.toml`.

```
loophole login [OPTIONS]

Options:
      --server <SERVER>  Server URL (e.g., https://tunnel.example.com)
      --token <TOKEN>    Authentication token
```

### `loophole test`

Test connection to the tunnel server.

```
loophole test [OPTIONS]

Options:
      --server <SERVER>  Server URL (uses saved config if not provided)
      --token <TOKEN>    Authentication token (uses saved config if not provided)
```

### `loophole expose`

Expose a local service through the tunnel.

```
loophole expose [OPTIONS]

Options:
      --server <SERVER>              Server URL (uses saved config if not provided)
      --token <TOKEN>                Authentication token (uses saved config if not provided)
      --subdomain <SUBDOMAIN>        Subdomain to register (random if not provided)
      --port <PORT>                  Local port to forward to [default: 3000]
      --host <HOST>                  Local host to forward to [default: 127.0.0.1]
      --local-host <LOCAL_HOST>      Override Host header for local requests
      --max-retries <MAX_RETRIES>    Max reconnection attempts (0 = unlimited) [default: 0]
      --forward-timeout <SECS>       Timeout for local forwarding [default: 30]
      --log-level <LOG_LEVEL>        Log level [default: info]
      --quiet                        Suppress request logging output
      --qr                           Show QR code for tunnel URL
```

### `loophole status`

Show status of active tunnels on a server. Requires an admin token.

```
loophole status [OPTIONS]

Options:
      --server <SERVER>  Server URL (uses saved config if not provided)
      --token <TOKEN>    Authentication token (must have admin privileges)
  -c, --config <CONFIG>  Path to server config file (alternative to --server/--token)
```

## Server Configuration

The server configuration file (`/etc/loophole/server.toml`) supports the following options:

```toml
version = 1

[server]
domain = "tunnel.example.com"  # Base domain for tunnels
http_port = 80                 # HTTP port (ACME challenges, redirects)
https_port = 443               # HTTPS port (tunnel traffic)

[tokens.tk_production]
admin = false                  # Regular token

[tokens.tk_admin]
admin = true                   # Admin token (can access /_admin/* endpoints)

[limits]
request_timeout_secs = 30          # Timeout for proxied requests
max_request_body_bytes = 10485760  # Max request body (10MB)
idle_tunnel_timeout_secs = 3600    # Disconnect idle tunnels (1 hour)

[https]
email = "admin@example.com"                              # Let's Encrypt email
certs_dir = "/var/lib/loophole/certs"                   # Certificate storage
directory = "https://acme-v02.api.letsencrypt.org/directory"  # ACME directory
staging = false                                          # Use staging for testing
```

### HTTPS Configuration

The `[https]` section enables automatic TLS certificate provisioning via Let's Encrypt:

- **email**: Required for Let's Encrypt account registration
- **certs_dir**: Directory to store certificates (must be writable)
- **staging**: Set to `true` to use Let's Encrypt staging environment (avoids rate limits during testing)

When HTTPS is configured:
- The server obtains a certificate for the base domain on startup
- Subdomain certificates are obtained automatically when tunnels connect
- Client connections use secure WebSocket (wss://)

Without the `[https]` section, the server runs in HTTP-only mode.

## Admin API

Admin tokens can access the following endpoints:

### List Tunnels

```bash
curl -H "Authorization: Bearer tk_admin_token" \
  https://tunnel.example.com/_admin/tunnels
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
  -H "Authorization: Bearer tk_admin_token" \
  https://tunnel.example.com/_admin/tunnels/myapp
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
                                                   │ WSS :443
                                                   │
┌──────────────────────────────────────────────────┼──────────────────────┐
│                           Client                 │                      │
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

1. Check the server is running: `loophole test`
2. Verify your token is correct
3. Check firewall allows connections on ports 80 and 443
4. Ensure DNS is configured with a wildcard A record: `*.tunnel.example.com`

### Certificate issues

1. Ensure port 80 is accessible for ACME HTTP-01 challenges
2. Check DNS points to your server
3. Try `staging = true` first to avoid rate limits
4. Check logs: `sudo journalctl -u loophole -f`

### SSL errors when opening tunnel URL

If you see SSL errors immediately after creating a tunnel, the certificate may still be provisioning. The client waits for the certificate to be ready before showing the URL, but this can take up to 90 seconds for new subdomains.

### Tunnel disconnects

1. Check idle timeout in config (default: 1 hour)
2. Use `--max-retries 0` for unlimited reconnection attempts
3. Check server logs for errors

### Slow responses

1. Increase `--forward-timeout` on client
2. Increase `request_timeout_secs` in server config
3. Check local service performance

## Security Considerations

- **Tokens**: Keep authentication tokens secret. Generate strong tokens.
- **Admin tokens**: Only give admin privileges to tokens that need them.
- **TLS**: Always use HTTPS in production. The `[https]` section enables automatic certificate management.
- **Local forwarding**: Client defaults to localhost only (`127.0.0.1`).
- **Subdomains**: Reserved names (www, api, admin, etc.) are blocked.

## License

MIT
