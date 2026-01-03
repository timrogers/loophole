# Multi-stage build for Loophole Tunnel Server
FROM rust:1.75-alpine AS builder

# Install build dependencies
RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static

WORKDIR /app

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./
COPY crates/proto/Cargo.toml crates/proto/
COPY crates/server/Cargo.toml crates/server/
COPY crates/client/Cargo.toml crates/client/

# Create dummy source files for dependency caching
RUN mkdir -p crates/proto/src crates/server/src crates/client/src && \
    echo "pub fn dummy() {}" > crates/proto/src/lib.rs && \
    echo "fn main() {}" > crates/server/src/main.rs && \
    echo "fn main() {}" > crates/client/src/main.rs

# Build dependencies only
RUN cargo build --release -p tunnel-server 2>/dev/null || true

# Now copy actual source code
COPY crates/proto/src crates/proto/src/
COPY crates/server/src crates/server/src/
COPY crates/client/src crates/client/src/

# Touch to rebuild with real sources
RUN touch crates/proto/src/lib.rs crates/server/src/main.rs

# Build the actual binary
RUN cargo build --release -p tunnel-server

# Runtime stage
FROM alpine:3.19

# Install runtime dependencies
RUN apk add --no-cache ca-certificates

# Create non-root user
RUN addgroup -g 1000 loophole && \
    adduser -u 1000 -G loophole -s /sbin/nologin -D loophole

# Create directories
RUN mkdir -p /etc/loophole /var/lib/loophole/certs && \
    chown -R loophole:loophole /var/lib/loophole

# Copy binary from builder
COPY --from=builder /app/target/release/tunnel-server /usr/local/bin/

# Copy default config
COPY config/server.toml /etc/loophole/server.toml.example

# Set permissions
RUN chmod +x /usr/local/bin/tunnel-server

# Switch to non-root user
USER loophole

# Expose ports
EXPOSE 80 443

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:${HTTP_PORT:-80}/ || exit 1

# Default command
ENTRYPOINT ["tunnel-server"]
CMD ["--config", "/etc/loophole/server.toml"]
