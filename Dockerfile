# Multi-stage build for optimal image size and security
FROM rust:latest as builder

# Install dependencies - minimal set for pure Rust build
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests
COPY omne-cli/Cargo.toml omne-cli/Cargo.lock ./

# Clean lock file to avoid edition2024 conflicts
RUN rm -f Cargo.lock

# Copy source code
COPY omne-cli/src ./src

# Build application with clean dependency resolution
RUN cargo build --release

# Runtime stage
FROM alpine:3.21

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