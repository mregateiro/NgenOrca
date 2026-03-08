# Build stage
FROM rust:1.93-alpine AS builder

RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static

WORKDIR /build
COPY . .

# Build the release binary (fully static with musl).
RUN cargo build --release --bin ngenorca \
    && strip target/release/ngenorca

# Runtime stage — minimal Alpine image.
FROM alpine:3.21

RUN apk add --no-cache ca-certificates tini \
    && addgroup -S ngenorca \
    && adduser -S ngenorca -G ngenorca

COPY --from=builder /build/target/release/ngenorca /usr/local/bin/ngenorca

# Data directory.
RUN mkdir -p /var/lib/ngenorca && chown ngenorca:ngenorca /var/lib/ngenorca
VOLUME /var/lib/ngenorca

# Config directory.
RUN mkdir -p /etc/ngenorca && chown ngenorca:ngenorca /etc/ngenorca
VOLUME /etc/ngenorca

USER ngenorca

# Health check.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://127.0.0.1:18789/health || exit 1

# Gateway port.
EXPOSE 18789

# Use tini as init process for proper signal handling.
ENTRYPOINT ["tini", "--"]

# Default command: start the gateway.
CMD ["ngenorca", "gateway", "--bind", "0.0.0.0", "--port", "18789"]
