# Loophole Implementation Plan

## Overview

Loophole is a self-hosted HTTP/HTTPS tunnel allowing developers to expose local services via custom subdomains. This document describes the implementation plan, including completed work and future phases.

---

## Architecture

```
                                    ┌─────────────────────────────────────────────────────────┐
                                    │                    Relay Server                         │
                                    │                                                         │
    ┌──────────┐                    │  ┌─────────────┐    ┌─────────────┐    ┌────────────┐  │
    │ Visitor  │─── HTTPS :443 ────►│  │ TLS Layer   │───►│ HTTP Router │───►│  Tunnel    │  │
    │ (browser)│                    │  │ (rustls)    │    │ (axum)      │    │  Registry  │  │
    └──────────┘                    │  └─────────────┘    └──────┬──────┘    └─────┬──────┘  │
                                    │                           │                  │         │
                                    │                           ▼                  ▼         │
    ┌──────────┐                    │  ┌─────────────────────────────────────────────────┐   │
    │  Client  │◄── WSS :443 ──────►│  │              Yamux Session                      │   │
    │  CLI     │   (multiplexed)    │  │  ┌─────────┐  ┌─────────┐  ┌─────────┐         │   │
    └────┬─────┘                    │  │  │ Stream 0│  │ Stream 1│  │ Stream 2│  ...    │   │
         │                          │  │  │ Control │  │ Request │  │ Request │         │   │
         ▼                          │  │  └─────────┘  └─────────┘  └─────────┘         │   │
    ┌──────────┐                    │  └─────────────────────────────────────────────────┘   │
    │  Local   │                    │                                                         │
    │  :3000   │                    │  ┌─────────────┐                                        │
    └──────────┘                    │  │ ACME Client │ (HTTP-01 on :80)                       │
                                    │  └─────────────┘                                        │
                                    └─────────────────────────────────────────────────────────┘
```

---

## Project Structure

```
loophole/
├── Cargo.toml                      # Workspace manifest
├── README.md
├── PLAN.md                         # This file
├── config/
│   └── server.toml                 # Server configuration
│
├── crates/
│   ├── proto/                      # Shared protocol definitions
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── messages.rs         # Control message types
│   │
│   ├── server/                     # Relay server binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs             # Entry point, CLI args
│   │       ├── config.rs           # TOML config parsing
│   │       ├── compat.rs           # WebSocket to AsyncRead/Write adapter
│   │       ├── registry.rs         # Subdomain → tunnel mapping
│   │       ├── router.rs           # HTTP routing by Host header
│   │       ├── handler.rs          # WebSocket/yamux connection handler
│   │       ├── tunnel.rs           # Tunnel state and proxy channel
│   │       ├── proxy.rs            # Request/response proxying
│   │       ├── acme.rs             # ACME client, HTTP-01 challenges (Phase 2)
│   │       └── tls.rs              # TLS certificate manager (Phase 2)
│   │
│   └── client/                     # Tunnel client binary
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs             # Entry point, CLI args
│           ├── client.rs           # WebSocket connection
│           ├── tunnel.rs           # Yamux session management
│           ├── forwarder.rs        # Local HTTP forwarding
│           └── reconnect.rs        # Reconnection with backoff
│
└── scripts/
    └── install.sh                  # Optional install helper (TODO)
```

---

## Phase 1: Minimal Tunnel (No TLS) ✅ COMPLETED

### Goals
- End-to-end request proxying over plaintext HTTP
- WebSocket + yamux multiplexing for efficient connection handling
- Subdomain-based routing

### Implementation Details

#### Protocol
- Client connects via WebSocket to `ws://{domain}/_tunnel/connect`
- Client sends JSON `Register` message with token and subdomain
- Server validates and responds with `Registered` or `Error`
- Yamux multiplexing over the WebSocket connection

#### Control Messages (JSON)
```rust
// Client → Server
enum ClientMessage {
    Register { token: String, subdomain: String },
    Ping,
    Disconnect,
}

// Server → Client
enum ServerMessage {
    Registered { subdomain: String, url: String },
    Error { code: ErrorCode, message: String },
    Pong,
    CertificateStatus { ready: bool },
}
```

#### Key Implementation Decisions

1. **Channel-based proxy architecture**: The server uses `tokio::sync::mpsc` channels to coordinate between HTTP request handlers and the yamux connection. This ensures the yamux connection is continuously polled and can send/receive data properly.

2. **Futures AsyncRead/AsyncWrite**: Yamux requires `futures::io::{AsyncRead, AsyncWrite}` traits, not Tokio's versions. Custom `Compat` wrappers convert WebSocket streams to these traits.

3. **Request proxying flow**:
   - HTTP router receives request, extracts subdomain from Host header
   - Router looks up tunnel in registry, sends request bytes via channel
   - Handler task receives request, opens yamux outbound stream
   - Client receives stream, forwards to local server, returns response
   - Response flows back through channel to HTTP handler

#### Files Implemented

| File | Purpose |
|------|---------|
| `proto/src/messages.rs` | JSON message types with serde |
| `server/src/config.rs` | TOML config parsing |
| `server/src/compat.rs` | WebSocket → futures AsyncRead/Write adapter |
| `server/src/registry.rs` | Thread-safe subdomain registry (DashMap) |
| `server/src/router.rs` | Axum router, subdomain extraction |
| `server/src/handler.rs` | WebSocket upgrade, yamux connection loop |
| `server/src/tunnel.rs` | Tunnel struct with proxy channel |
| `server/src/proxy.rs` | HTTP request/response building and parsing |
| `client/src/client.rs` | WebSocket connection and registration |
| `client/src/tunnel.rs` | Yamux client session, stream acceptance |
| `client/src/forwarder.rs` | Local HTTP forwarding |
| `client/src/reconnect.rs` | Exponential backoff strategy |

---

## Phase 2: TLS with ACME ✅ COMPLETED

### Goals
- Automatic HTTPS with Let's Encrypt certificates
- HTTP-01 challenge support
- Per-subdomain certificate management

### Implementation Details

#### Files Implemented

| File | Purpose |
|------|---------|
| `server/src/acme.rs` | ACME client using `instant-acme`, HTTP-01 challenge store, custom CA support |
| `server/src/tls.rs` | Certificate manager with rustls SNI-based cert selection |

#### Key Features

1. **Dual HTTP/HTTPS server architecture**
   - HTTP server (configurable port) serves ACME challenges and allows WebSocket tunnel registration
   - HTTPS server (configurable port) serves tunnel traffic with TLS
   - Non-ACME HTTP requests redirect to HTTPS (except the control path for WebSocket)

2. **HTTP-01 challenge responder**
   - `ChallengeStore` using DashMap for in-memory token storage
   - Serves `/.well-known/acme-challenge/{token}` responses
   - Tokens automatically cleaned up after validation

3. **ACME client (`instant-acme` integration)**
   - Account creation and persistence to `{certs_dir}/account.json`
   - Certificate ordering with HTTP-01 validation
   - Support for custom root CAs (for testing with Pebble)
   - Certificate and key storage as PEM files

4. **Certificate storage**
   - Certificates stored as `{certs_dir}/{domain}/cert.pem` and `key.pem`
   - Existing certificates loaded on startup
   - ACME account credentials persisted for reuse

5. **Dynamic certificate loading with rustls**
   - `CertManager` implements `ResolvesServerCert` for SNI-based selection
   - Certificates requested automatically when tunnel registers
   - Background certificate request doesn't block tunnel registration

6. **CertificateStatus protocol message**
   - Server sends `CertificateStatus { ready: bool }` to client after registration
   - Allows client to know when HTTPS is fully available

7. **Crypto provider initialization**
   - `rustls::crypto::aws_lc_rs` provider installed at startup
   - Required for rustls 0.23+ to function correctly

### How It Works (Flow)

1. Server starts, loads config, initializes ACME account
2. HTTP server starts on `http_port`, HTTPS server starts on `https_port`
3. Client connects via WebSocket to `ws://{server}:{http_port}/_tunnel/connect`
4. Client sends `Register` message with token and subdomain
5. Server validates, registers tunnel, returns `Registered` with HTTPS URL
6. Server checks if certificate exists for `{subdomain}.{domain}`
7. If no cert, server requests one from ACME (background task)
8. ACME server validates via HTTP-01 challenge on port 80
9. Certificate issued, saved to disk, loaded into `CertManager`
10. Server sends `CertificateStatus { ready: true }` to client
11. HTTPS traffic to `{subdomain}.{domain}` is now served through the tunnel

### Dependencies Added
```toml
# TLS and ACME
instant-acme = "0.7"
rcgen = "0.13"
rustls = "0.23"
rustls-pemfile = "2"
tokio-rustls = "0.26"
axum-server = { version = "0.7", features = ["tls-rustls"] }
pem = "3"
hyper-rustls = { version = "0.27", features = ["http1", "http2", "tls12", "ring"] }
webpki-roots = "0.26"
```

### Configuration Reference

```toml
[server]
domain = "tunnel.example.com"  # Base domain for tunnels
http_port = 80                 # HTTP port (ACME challenges + WebSocket registration)
https_port = 443               # HTTPS port (tunnel traffic)
control_path = "/_tunnel/connect"

[acme]
email = "admin@example.com"    # Contact email for Let's Encrypt
directory = "https://acme-v02.api.letsencrypt.org/directory"  # ACME directory URL
certs_dir = "/var/lib/loophole/certs"  # Where to store certificates
staging = false                # Use Let's Encrypt staging (for testing)
ca_file = "/path/to/ca.pem"    # Optional: custom CA for ACME server TLS (Pebble testing)
```

### Testing with Pebble (Local ACME Server)

[Pebble](https://github.com/letsencrypt/pebble) is Let's Encrypt's test ACME server. It's essential for local development since real Let's Encrypt requires a publicly accessible domain.

#### Step 1: Start Pebble

```bash
docker run -d --name pebble \
  -p 14000:14000 \
  -p 15000:15000 \
  -e PEBBLE_VA_NOSLEEP=1 \
  -e PEBBLE_VA_ALWAYS_VALID=1 \
  ghcr.io/letsencrypt/pebble:latest
```

- Port 14000: ACME directory (`https://localhost:14000/dir`)
- Port 15000: Management interface (root CA download)
- `PEBBLE_VA_ALWAYS_VALID=1`: Skip actual HTTP-01 validation (for testing)

#### Step 2: Get Pebble's TLS Certificate CA

Pebble uses a self-signed certificate for its HTTPS endpoint. You need to trust it:

```bash
# Copy the minica root CA that Pebble uses for its TLS
docker cp pebble:/test/certs/pebble.minica.pem /tmp/pebble-tls-ca.pem
```

**Important**: This is different from Pebble's ACME root CA (which signs issued certificates). The `pebble.minica.pem` is for trusting Pebble's own TLS connection.

#### Step 3: Configure Loophole for Pebble

Create a test config file:

```toml
[server]
domain = "localhost"
http_port = 8080
https_port = 8443
control_path = "/_tunnel/connect"

[tokens]
"tk_test123" = 5

[limits]
request_timeout_secs = 30
max_request_body_bytes = 10485760
idle_tunnel_timeout_secs = 3600

[acme]
email = "test@example.com"
directory = "https://localhost:14000/dir"
certs_dir = "/tmp/loophole-test/certs"
staging = false
ca_file = "/tmp/pebble-tls-ca.pem"
```

#### Step 4: Run the Test

```bash
# Terminal 1: Start a local HTTP server
python3 -m http.server 9123

# Terminal 2: Start tunnel server
./target/release/tunnel-server --config /tmp/loophole-test/server-acme.toml

# Terminal 3: Connect client
./target/release/tunnel-client --server localhost:8080 --token tk_test123 --subdomain mytest --port 9123

# Terminal 4: Test HTTPS (need to trust Pebble's ACME root CA for the issued cert)
curl -sk https://localhost:15000/roots/0 > /tmp/pebble-acme-ca.pem
curl --cacert /tmp/pebble-acme-ca.pem --resolve mytest.localhost:8443:127.0.0.1 https://mytest.localhost:8443/
```

#### Expected Output

Server logs:
```
INFO tunnel_server: ACME enabled with email: test@example.com
INFO tunnel_server: Loading additional CA from: /tmp/pebble-tls-ca.pem
INFO tunnel_server::acme: Creating new ACME account for test@example.com
INFO tunnel_server::acme: Saved ACME account credentials
INFO tunnel_server: Starting HTTP server on 0.0.0.0:8080
INFO tunnel_server: Starting HTTPS server on 0.0.0.0:8443
INFO tunnel_server::handler: Tunnel registered: mytest -> https://mytest.localhost:8443
INFO tunnel_server::acme: Requesting certificate for mytest.localhost
INFO tunnel_server::acme: Certificate issued for mytest.localhost
INFO tunnel_server::tls: Certificate installed for mytest.localhost
```

Client logs:
```
INFO tunnel_client::client: Tunnel registered!
INFO tunnel_client::client: URL: https://mytest.localhost:8443
INFO tunnel_client: Connected! Your tunnel URL: https://mytest.localhost:8443
```

#### Cleanup

```bash
docker rm -f pebble
rm -rf /tmp/loophole-test
```

### Known Limitations / Future Work

1. **Certificate renewal task** - Implemented but not wired up to run automatically. The `certificate_renewal_task` function exists in `acme.rs` but needs to be spawned in `main.rs`.

2. **No wildcard certificates** - Each subdomain gets its own certificate. Could optimize with `*.{domain}` wildcard cert.

3. **No certificate revocation** - If a certificate needs to be revoked, it must be done manually.

4. **HTTP-01 only** - DNS-01 challenges not supported (would need DNS provider integration).

### Original Tasks (All Completed ✅)

1. ✅ **Add HTTP server on :80 for ACME challenges**
2. ✅ **Implement HTTP-01 challenge responder**
3. ✅ **Integrate `instant-acme` for certificate requests**
4. ✅ **Certificate storage**
5. ✅ **Dynamic certificate loading in rustls**
6. ✅ **HTTPS server on :443**
7. ✅ **HTTP → HTTPS redirect**
8. ✅ **Certificate renewal background task** (implemented, not yet wired up)

---

## Phase 3: Production Hardening ✅ COMPLETED

### Goals
- Reliable operation under real-world conditions
- Proper resource management and cleanup

### Implementation Details

#### Files Modified

| File | Changes |
|------|---------|
| `server/src/main.rs` | Graceful shutdown (SIGTERM/SIGINT), idle tunnel cleanup background task |
| `server/src/proxy.rs` | Body size limits (413 response), request ID in logs and headers |
| `server/src/router.rs` | Request ID generation per request |
| `server/src/tunnel.rs` | Last activity tracking for idle detection |
| `server/src/registry.rs` | Added `subdomains()` method for iteration |
| `server/Cargo.toml` | Added `uuid` dependency |
| `client/src/main.rs` | Added `--max-retries` and `--forward-timeout` CLI options |
| `client/src/reconnect.rs` | Added `attempts()` method |
| `client/src/forwarder.rs` | Per-stream timeout for local forwarding |
| `client/src/tunnel.rs` | Forward timeout parameter |
| `proto/src/messages.rs` | Added `Ping` and `Shutdown` server messages |

#### Key Features Implemented

1. **Request body size limits**
   - Checks `max_request_body_bytes` from config in `proxy.rs`
   - Returns 413 Payload Too Large when exceeded
   - Logs warning with body size and max allowed

2. **Idle tunnel cleanup**
   - `Tunnel` struct tracks `last_activity` timestamp (updated on each request)
   - Background task runs every 60 seconds to check for idle tunnels
   - Tunnels idle longer than `idle_tunnel_timeout_secs` are deregistered
   - Cleanup task properly shuts down on server termination

3. **Graceful shutdown**
   - Handles both SIGINT (Ctrl+C) and SIGTERM (on Unix)
   - Broadcasts shutdown signal to background tasks
   - Logs shutdown progress for operators

4. **Structured logging with request IDs**
   - UUID v4 generated for each incoming request
   - Request ID included in all proxy-related log messages
   - `X-Request-ID` header added to both proxied requests and responses
   - Enables end-to-end request tracing

5. **Client reconnection improvements**
   - `--max-retries` option (default 0 = unlimited)
   - Exits with error after exceeding retry limit
   - Jitter already implemented in exponential backoff

6. **Per-stream timeout in client forwarder**
   - `--forward-timeout` option (default 30 seconds)
   - Timeouts on connect, write, and read operations
   - Returns 504 Gateway Timeout on timeout, 502 Bad Gateway on other errors

#### Proper Error Responses (Updated)

| Scenario | HTTP Status | Implemented |
|----------|-------------|-------------|
| Unknown subdomain | 404 | ✅ |
| Tunnel disconnected | 502 | ✅ |
| Timeout | 504 | ✅ |
| Request too large | 413 | ✅ |
| Rate limited | 429 | ❌ (Future work) |

### Configuration Reference

```toml
[limits]
request_timeout_secs = 30       # Timeout waiting for tunnel response
max_request_body_bytes = 10485760  # 10MB max request body
idle_tunnel_timeout_secs = 3600    # 1 hour idle timeout
```

### Client CLI Reference

```bash
tunnel-client \
  --server localhost:8080 \
  --token tk_test123 \
  --subdomain myapp \
  --port 3000 \
  --max-retries 10 \           # Exit after 10 failed reconnection attempts
  --forward-timeout 30         # 30 second timeout for local forwarding
```

### Not Implemented (Future Work)

1. **Ping/pong keepalive** - Yamux handles connection liveness internally through its own keepalive mechanism. WebSocket-level ping/pong would require significant refactoring of the compat layer.

2. **Rate limiting (429)** - Would require per-IP or per-token request counting with sliding window.

3. **Prometheus metrics** - Optional feature, can be added in Phase 4 or later.

### Dependencies Added
```toml
uuid = { version = "1", features = ["v4"] }
```

---

## Phase 4: Polish ✅ COMPLETED

### Goals
- Excellent user experience
- Production-ready deployment

### Implementation Details

#### Files Modified/Created

| File | Changes |
|------|---------|
| `client/Cargo.toml` | Added `colored` and `qrcode` dependencies |
| `client/src/main.rs` | Colored output, `--quiet` and `--qr` flags, QR code generation |
| `client/src/forwarder.rs` | Colored request/response logging with timing |
| `client/src/tunnel.rs` | Pass `quiet` flag to forwarder |
| `server/src/config.rs` | Added `AdminConfig` struct |
| `server/src/router.rs` | Admin endpoints: list tunnels, delete tunnel |
| `config/server.toml` | Added admin section example |
| `scripts/loophole-server.service` | Systemd service file |
| `scripts/install.sh` | Installation script |
| `Dockerfile` | Server container image |
| `Dockerfile.client` | Client container image |
| `README.md` | Comprehensive documentation |

#### Features Implemented

1. **Colored CLI output**
   - Green checkmarks for success
   - Yellow warnings for reconnection
   - Red errors for failures
   - Colored HTTP status codes (2xx green, 3xx cyan, 4xx yellow, 5xx red)
   - Request timing in milliseconds

2. **QR code for URL**
   - `--qr` flag generates terminal QR code
   - Useful for mobile testing

3. **Request logging with timing**
   - Shows method, path, status, and timing
   - `--quiet` flag suppresses request logging

4. **Server admin endpoint**
   - `GET /_admin/tunnels` - list active tunnels with stats
   - `DELETE /_admin/tunnels/{subdomain}` - force disconnect
   - Bearer token authentication
   - Returns JSON responses

5. **Systemd service file**
   - Security hardening (NoNewPrivileges, ProtectSystem, etc.)
   - Automatic restart
   - Resource limits

6. **Docker images**
   - Multi-stage builds for small images
   - Non-root user
   - Health checks
   - Both server and client images

7. **Documentation**
   - Updated README with full documentation
   - Installation guide
   - Configuration reference
   - Troubleshooting guide
   - Admin API documentation

### Configuration Reference

```toml
[admin]
enabled = true
token = "admin_secret_token"  # Required when enabled
```

### Client CLI Reference

```bash
tunnel-client \
  --server localhost:8080 \
  --token tk_test123 \
  --subdomain myapp \
  --port 3000 \
  --quiet \              # Suppress request logging
  --qr                   # Show QR code for URL
```

### Admin API Examples

```bash
# List tunnels
curl -H "Authorization: Bearer admin_secret_token" \
  http://localhost:8090/_admin/tunnels

# Delete tunnel
curl -X DELETE \
  -H "Authorization: Bearer admin_secret_token" \
  http://localhost:8090/_admin/tunnels/myapp
```

---

## Error Handling Reference

### Server Errors

| Scenario | HTTP Response | Cleanup Action |
|----------|---------------|----------------|
| Unknown subdomain | 404 Not Found | - |
| Tunnel disconnected | 502 Bad Gateway | Deregister tunnel |
| Tunnel timeout | 504 Gateway Timeout | Log, keep tunnel |
| Request too large | 413 Payload Too Large | - |
| Rate limited | 429 Too Many Requests | - |
| Internal error | 500 Internal Server Error | Log with context |

### Client Errors

| Scenario | Action |
|----------|--------|
| Connection refused | Reconnect with backoff |
| Invalid token | Exit with error message |
| Subdomain taken | Exit with error message |
| Local server down | Return 502 to server |
| WebSocket closed | Reconnect with backoff |

---

## Security Considerations

1. **Token validation**: Tokens checked on every connection
2. **Subdomain validation**: Alphanumeric + hyphen only, 3-63 chars, reserved names blocked
3. **Request headers**: Add `X-Forwarded-For`, `X-Forwarded-Proto`, strip hop-by-hop headers
4. **Local forwarding**: Client defaults to localhost only

### Future Security Enhancements
- Rate limiting per IP and per token
- Request signing for admin endpoints
- Audit logging
- IP allowlisting

---

## Testing Strategy

### Unit Tests (Implemented)
- ✅ Config parsing
- ✅ Subdomain validation
- ✅ Message serialization
- ✅ Subdomain extraction from Host header

### Integration Tests (TODO)
- Full tunnel flow with mock local server
- ACME challenge serving (mock ACME server)
- Reconnection behavior
- Timeout handling

### Manual Testing Checklist
- [x] Client connects successfully
- [x] Custom subdomain works
- [x] Invalid token rejected
- [x] Duplicate subdomain rejected (via registry)
- [x] Request proxied correctly
- [ ] Large request body works (413 response when exceeded)
- [ ] Client disconnect cleans up
- [ ] Client reconnects after network blip
- [x] ACME certificate issued (Phase 2) - tested with Pebble
- [x] HTTPS works (Phase 2) - tested with Pebble
- [ ] Certificate renewal works (Phase 2) - implemented but not tested
- [ ] Idle tunnel cleanup works (Phase 3)
- [ ] Graceful shutdown works (Phase 3)
- [ ] Request ID appears in logs and response headers (Phase 3)
- [ ] --max-retries limits reconnection attempts (Phase 3)
- [ ] --forward-timeout causes 504 on slow local server (Phase 3)
