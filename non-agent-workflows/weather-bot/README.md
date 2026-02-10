# Weather Bot 🌤️

Kalshi weather market mispricing bot — single-binary Rust application.

## Quick Start

```bash
# 1. Set credentials in .env (project root or parent)
export KALSHI_API_KEY="your-key"
export KALSHI_SECRET_KEY="-----BEGIN RSA PRIVATE KEY-----\n..."

# 2. Verify auth works
cargo run -- --check-auth

# 3. Dry-run (discover markets + evaluate strategy, no orders)
cargo run -- --dry-run

# 4. Run live
cargo run
```

## Architecture

```
weather-bot/
├── src/
│   ├── main.rs           # Tokio task orchestration
│   └── config.rs         # Config loader (env + toml)
├── crates/
│   ├── common/           # Shared types, config, errors
│   ├── kalshi_client/    # REST + WebSocket + RSA-PSS auth
│   ├── noaa_client/      # NOAA forecast fetcher + probability
│   └── strategy/         # Strategy engine + risk manager
└── config.toml           # Default configuration
```

**4 concurrent Tokio tasks:**
1. **Market Discovery** — finds weather markets every 30 min
2. **Price Feed** — WebSocket ticker stream → PriceCache
3. **Forecast Ingest** — NOAA hourly forecasts → ForecastCache
4. **Strategy Loop** — evaluates every 2 min → risk check → execute

## Configuration

Edit `config.toml` or override via environment. Key settings:

| Setting | Default | Description |
|---------|---------|-------------|
| `entry_threshold_cents` | 15 | Buy YES when ask ≤ this |
| `exit_threshold_cents` | 45 | Sell YES when bid ≥ this |
| `max_position_cents` | 500 | $5 max per market |
| `max_trades_per_run` | 5 | Trades per evaluation cycle |

## Testing

```bash
cargo test --workspace    # 15 unit tests
cargo clippy --workspace  # Lint check
```
