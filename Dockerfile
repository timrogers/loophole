# Build stage
FROM rust:1.85-bookworm AS builder

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./

# Create dummy src to build dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release && rm -rf src

# Copy actual source and rebuild
COPY src/ ./src/
RUN touch src/main.rs && cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create directory for certificates
RUN mkdir -p /var/lib/loophole/certs

COPY --from=builder /app/target/release/loophole /usr/local/bin/

# Default ports
EXPOSE 80 443

# Environment variables for configuration (see README for full list)
# Required:
#   LOOPHOLE_DOMAIN - Base domain for tunnels (e.g., tunnel.example.com)
#   LOOPHOLE_TOKENS - Comma-separated list of auth tokens
# Optional:
#   LOOPHOLE_ADMIN_TOKENS - Comma-separated list of admin tokens
#   LOOPHOLE_ACME_EMAIL - Email for Let's Encrypt (enables HTTPS)
#   LOOPHOLE_ACME_STAGING - Use Let's Encrypt staging (true/false)
#   LOOPHOLE_HTTP_PORT - HTTP port (default: 80)
#   LOOPHOLE_HTTPS_PORT - HTTPS port (default: 443)

ENTRYPOINT ["loophole"]
CMD ["server"]
