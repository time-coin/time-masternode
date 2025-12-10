# Build stage
FROM rust:1.75-slim as builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy source code
COPY src ./src

# Build release binary
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create app user
RUN useradd -m -u 1000 timecoin

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/timed /usr/local/bin/

# Create data directory
RUN mkdir -p /app/data && chown -R timecoin:timecoin /app

USER timecoin

# Expose ports
EXPOSE 24100 24101

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD pgrep timed || exit 1

# Default command
CMD ["timed"]
