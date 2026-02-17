FROM rust:1-slim-bookworm AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy only bridge workspace crates.
COPY crates/Cargo.toml crates/Cargo.lock ./crates/
COPY crates/trading_domain ./crates/trading_domain
COPY crates/trading_protocol ./crates/trading_protocol
COPY crates/trading_daemon ./crates/trading_daemon
COPY crates/tradingctl ./crates/tradingctl
COPY crates/exchange_core ./crates/exchange_core
COPY crates/exchange_kalshi ./crates/exchange_kalshi
COPY crates/exchange_coinbase_spot ./crates/exchange_coinbase_spot
COPY crates/exchange_derivatives_paper ./crates/exchange_derivatives_paper
COPY crates/strategy_core ./crates/strategy_core
COPY crates/strategy_kalshi_weather ./crates/strategy_kalshi_weather
COPY crates/strategy_kalshi_arbitrage ./crates/strategy_kalshi_arbitrage
COPY crates/strategy_crypto_momentum ./crates/strategy_crypto_momentum
COPY crates/risk_core ./crates/risk_core

WORKDIR /app/crates
RUN cargo build --release --bin trading_daemon --bin tradingctl

FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies (OpenSSL, etc.)
RUN apt-get update && apt-get install -y --no-install-recommends openssl ca-certificates && rm -rf /var/lib/apt/lists/*

# Copy binary
COPY --from=builder /app/crates/target/release/trading_daemon /usr/local/bin/trading_daemon
COPY --from=builder /app/crates/target/release/tradingctl /usr/local/bin/tradingctl

# Create socket directory with correct permissions
RUN mkdir -p /var/run/openclaw && chown 1000:1000 /var/run/openclaw

# Run as non-root
USER 1000:1000

CMD ["trading_daemon"]
