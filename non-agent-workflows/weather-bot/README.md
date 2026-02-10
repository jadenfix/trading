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

# 5. Watch trade/audit events
tail -f /Users/jadenfix/Desktop/trading/TRADES/weather-bot/trades-$(date +%F).jsonl
```

## Architecture

- **`crates/strategy`**: Core decision logic comparing NOAA forecasts to Kalshi market probabilities.
- **`crates/noaa_client`**: Client for fetching and parsing NWS grid forecasts.
- **`libs/kalshi_client`**: Shared Kalshi API client (REST + WebSocket).
- **`libs/common`**: Shared types and utilities.

## Configuration

Edit `config.toml` or override via environment. Key settings:

| Setting | Default | Description |
|---------|---------|-------------|
| `entry_threshold_cents` | 15 | Buy YES when ask ≤ this |
| `exit_threshold_cents` | 45 | Sell YES when bid ≥ this |
| `max_position_cents` | 500 | $5 max per market |
| `max_trades_per_run` | 5 | Trades per evaluation cycle |

### Trade Journal

- Weather bot writes JSONL trade/audit events to `<repo-root>/TRADES/weather-bot` by default.
- Example file path:
  - `/Users/jadenfix/Desktop/trading/TRADES/weather-bot/trades-YYYY-MM-DD.jsonl`
- Set `TRADES_DIR=/custom/root` to use another root. The bot writes to `TRADES_DIR/weather-bot`.
- Event stream includes:
  - `bot_start`, `auth_check`, `dry_run_summary`, `dry_run_intent`
  - `discovery_cycle`, `forecast_cycle`, `strategy_cycle_start`, `intent_generated`
  - `order_placed`, `order_failed`, `strategy_cycle_summary`, `heartbeat`, `bot_shutdown`

### Heartbeat

- Bot emits a 30-second `HEARTBEAT` line with tickers/markets/prices/forecasts counts so runtime health is always visible.

## Testing

```bash
cargo test --workspace    # 15 unit tests
cargo clippy --workspace  # Lint check
```
