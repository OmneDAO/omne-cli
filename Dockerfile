# Multi-stage build for OMNE CLI
FROM rust:1.75-slim-bookworm as builder

# Install dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src

# Build application
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd --create-home --shell /bin/bash omne

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