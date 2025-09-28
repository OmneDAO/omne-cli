# Multi-stage build for optimal image size and security
FROM rust:1.82-alpine as builder

# Install dependencies
RUN apk add --no-cache \
    pkgconfig \
    openssl-dev \
    musl-dev

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src

# Build application
RUN cargo build --release

# Runtime stage
FROM alpine:3.19

# Install runtime dependencies
RUN apk add --no-cache ca-certificates curl

# Create non-root user
RUN adduser -D -s /bin/sh omne

# Copy binary from builder stage
COPY --from=builder /app/target/release/omne /usr/local/bin/omne

# Set permissions
RUN chmod +x /usr/local/bin/omne

# Switch to non-root user
USER omne

# Set working directory
WORKDIR /home/omne

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD omne --version || exit 1

ENTRYPOINT ["omne"]
CMD ["--help"]