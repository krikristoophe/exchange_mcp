# --- Build stage ---
FROM rust:1.85-bookworm AS builder

WORKDIR /usr/src/app

# Cache dependencies: copy manifests first, build a dummy project
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release && rm -rf src target/release/deps/exchange_mcp*

# Build the real project
COPY src/ src/
RUN cargo build --release

# --- Runtime stage ---
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/src/app/target/release/exchange-mcp /usr/local/bin/exchange-mcp

# Data directory for SQLite OAuth2 database
RUN mkdir -p /data
VOLUME /data

ENV EXCHANGE_MCP_SSE_HOST=0.0.0.0
ENV EXCHANGE_MCP_SSE_PORT=3000
ENV EXCHANGE_MCP_OAUTH_DB=/data/oauth2.db
ENV RUST_LOG=info

EXPOSE 3000

ENTRYPOINT ["exchange-mcp"]
