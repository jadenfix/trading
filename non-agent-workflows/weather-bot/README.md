# Weather Bot

Rust bot that trades Kalshi weather markets when forecast-derived edge passes strict quality and risk gates.

## Runtime Flow

1. Discover active weather markets.
2. Maintain live quotes through WebSocket streams.
3. Pull NOAA and optional Google forecasts.
4. Convert forecasts into market probability and confidence estimates.
5. Apply quality gates (source agreement, confidence, edge/EV, liquidity, spread).
6. Apply risk gates (position caps, city caps, drawdown guard, throttle).
7. Place approved orders and journal every decision/event.

Goal: trade less often with higher conviction and controlled downside.

## Quick Start

```bash
# 1) Set credentials
export KALSHI_API_KEY="your-key"
export KALSHI_SECRET_KEY="-----BEGIN RSA PRIVATE KEY-----\n..."

# 2) Verify connectivity/auth
cargo run -- --check-auth

# 3) Strategy dry run (no live orders)
cargo run -- --dry-run

# 4) Live run
cargo run

# 5) Watch trade journal (from this directory)
tail -f ../../TRADES/weather-bot/trades-$(date +%F).jsonl
```

## Architecture

- `crates/strategy`: core entry/exit decision logic
- `crates/noaa_client`: NOAA forecast client
- `crates/google_weather_client`: optional Google Weather forecast client
- `../libs/kalshi_client`: shared Kalshi REST + WebSocket client
- `../libs/common`: shared domain types and utilities

## Configuration

Edit `config.toml` and/or environment variables.

| Setting | Default | Description |
| --- | --- | --- |
| `entry_threshold_cents` | 15 | Buy YES when ask <= this |
| `exit_threshold_cents` | 45 | Sell YES when bid >= this |
| `max_position_cents` | 500 | Max position size per market in cents |
| `max_trades_per_run` | 0 | `0` means unlimited count, gated by quality/risk |
| `weather_sources.noaa_weight` | 0.5 | NOAA forecast blend weight |
| `weather_sources.google_weight` | 0.5 | Google forecast blend weight |
| `quality.mode` | `ultra_safe` | `ultra_safe`, `balanced`, or `aggressive` |
| `quality.strict_source_veto` | `true` | Block entries when sources disagree |
| `quality.require_both_sources` | `true` | Require both NOAA and Google forecasts |

Common environment overrides:

- `GOOGLE_WEATHER_API_KEY`
- `WEATHER_BOT_CONTACT_EMAIL`
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

If `GOOGLE_WEATHER_API_KEY` is not set, the bot can run NOAA-only depending on config.

## Trade Journal and Health

Default output path:

- `<repo-root>/TRADES/weather-bot/trades-YYYY-MM-DD.jsonl`

Override root:

- `TRADES_DIR=/custom/root` writes to `TRADES_DIR/weather-bot`

Event types include:

- lifecycle: `bot_start`, `auth_check`, `bot_shutdown`
- dry run: `dry_run_summary`, `dry_run_intent`, `dry_run_rejected`
- live flow: `discovery_cycle`, `forecast_cycle`, `intent_generated`, `intent_rejected`
- orders: `order_placed`, `order_failed`
- monitoring: `heartbeat`

The bot also emits a 30-second `HEARTBEAT` log line for liveness checks.

## Development Checks

```bash
cargo test --workspace
cargo clippy --workspace
```
