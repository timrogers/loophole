# Multi-stage build for Loophole Tunnel Server
FROM rust:1.75-alpine AS builder

# Install build dependencies
RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static

WORKDIR /app

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./

# Pre-fetch dependencies for caching
RUN cargo fetch

# Copy actual project files
COPY src ./src

# Build the actual binary
RUN cargo build --release --bin loophole

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
COPY --from=builder /app/target/release/loophole /usr/local/bin/

# Copy default config
COPY config/server.toml /etc/loophole/server.toml.example

# Set permissions
RUN chmod +x /usr/local/bin/loophole

# Switch to non-root user
USER loophole

# Expose ports
EXPOSE 80 443

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:${HTTP_PORT:-80}/ || exit 1

# Default command
ENTRYPOINT ["loophole"]
CMD ["server", "--config", "/etc/loophole/server.toml"]
