# Weather Bot 🌤️

Kalshi weather market mispricing bot — single-binary Rust application.

## What The Weather Bot Does

The weather bot continuously scans open Kalshi weather markets and only places trades when a
strict quality policy says the setup is strong enough.

Core runtime flow:

1. Discovers relevant weather markets on Kalshi.
2. Streams live prices through WebSocket and keeps a fresh in-memory price cache.
3. Fetches forecasts from NOAA and Google Weather, then builds an ensemble forecast.
4. Converts each city forecast into market probabilities (`p_yes`) and confidence bands.
5. Applies quality gates before entry:
   - source agreement checks (strict veto mode)
   - confidence thresholds
   - conservative edge and expected value thresholds (after fees/slippage)
   - liquidity and spread filters
6. Sends only approved intents through the risk manager (position caps, city caps, drawdown
   guard, order throttle).
7. Places orders and writes all outcomes to the trade journal.

Design goal:

- Trade less often, but with higher conviction and tighter downside controls.

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

- **`crates/strategy`**: Core decision logic comparing ensemble forecasts to Kalshi market probabilities.
- **`crates/noaa_client`**: Client for fetching and parsing NWS grid forecasts.
- **`crates/google_weather_client`**: Optional Google Weather API forecast client.
- **`libs/kalshi_client`**: Shared Kalshi API client (REST + WebSocket).
- **`libs/common`**: Shared types and utilities.

## Configuration

Edit `config.toml` or override via environment. Key settings:

| Setting | Default | Description |
|---------|---------|-------------|
| `entry_threshold_cents` | 15 | Buy YES when ask ≤ this |
| `exit_threshold_cents` | 45 | Sell YES when bid ≥ this |
| `max_position_cents` | 500 | $5 max per market |
| `max_trades_per_run` | 0 | `0` disables hard cap (quality gates decide) |
| `weather_sources.noaa_weight` | 0.5 | Relative NOAA forecast weight |
| `weather_sources.google_weight` | 0.5 | Relative Google forecast weight |
| `quality.mode` | `ultra_safe` | Risk posture (`ultra_safe`, `balanced`, `aggressive`) |
| `quality.strict_source_veto` | `true` | Block entries on source disagreement |
| `quality.require_both_sources` | `true` | Require both NOAA and Google source forecasts |

Environment overrides:

- `GOOGLE_WEATHER_API_KEY` (required when `weather_sources.google_weight > 0` or `quality.require_both_sources=true`)
- `WEATHER_NOAA_WEIGHT`
- `WEATHER_GOOGLE_WEIGHT`
- `WEATHER_QUALITY_MODE`
- `WEATHER_STRICT_SOURCE_VETO`
- `WEATHER_REQUIRE_BOTH_SOURCES`
- `WEATHER_MAX_SOURCE_PROB_GAP`
- `WEATHER_MIN_SOURCE_CONFIDENCE`
- `WEATHER_MIN_ENSEMBLE_CONFIDENCE`
- `WEATHER_MIN_CONSERVATIVE_NET_EDGE_CENTS`
- `WEATHER_MIN_CONSERVATIVE_EV_CENTS`
- `WEATHER_MIN_VOLUME_24H`
- `WEATHER_MIN_OPEN_INTEREST`
- `WEATHER_SLIPPAGE_BUFFER_CENTS`
- `WEATHER_MAX_SPREAD_CENTS_ULTRA`

### Trade Journal

- Weather bot writes JSONL trade/audit events to `<repo-root>/TRADES/weather-bot` by default.
- Example file path:
  - `/Users/jadenfix/Desktop/trading/TRADES/weather-bot/trades-YYYY-MM-DD.jsonl`
- Set `TRADES_DIR=/custom/root` to use another root. The bot writes to `TRADES_DIR/weather-bot`.
- Event stream includes:
  - `bot_start`, `auth_check`, `dry_run_summary`, `dry_run_intent`, `dry_run_rejected`
  - `discovery_cycle`, `forecast_cycle`, `strategy_cycle_start`, `intent_generated`, `intent_rejected`
  - `order_placed`, `order_failed`, `strategy_cycle_summary`, `heartbeat`, `bot_shutdown`

### Heartbeat

- Bot emits a 30-second `HEARTBEAT` line with tickers/markets/prices/forecasts counts so runtime health is always visible.

## Testing

```bash
cargo test --workspace
cargo clippy --workspace  # Lint check
```
