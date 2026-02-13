# Non-Agent Kalshi Bots

Rust trading bots for Kalshi markets. This workspace contains deterministic strategies and shared Rust libraries.

## Layout

- `libs/kalshi_client`: shared Kalshi REST + WebSocket client
- `libs/common`: shared types, config helpers, and utilities
- `weather-bot`: weather-driven strategy (NOAA + optional Google blend)
- `arbitrage-bot`: Buy-Set/Sell-Set arbitrage strategy for grouped markets

## Bot Documentation

- Weather bot: [`weather-bot/README.md`](./weather-bot/README.md)
- Arbitrage bot: [`arbitrage-bot/README.md`](./arbitrage-bot/README.md)

## Prerequisites

- Rust stable toolchain
- Kalshi API credentials

## Environment Setup

Set variables in your shell (or `.env` if your tooling loads it):

```env
KALSHI_API_KEY=your_key_id
KALSHI_SECRET_KEY="your_private_key_pem_content"
USE_DEMO=true
GOOGLE_WEATHER_API_KEY=your_google_weather_api_key
WEATHER_BOT_CONTACT_EMAIL=you@yourdomain.com
```

Notes:

- Set `USE_DEMO=false` for production.
- `GOOGLE_WEATHER_API_KEY` is only required when Google source input is enabled in weather bot config.

## Build and Run

Build each bot:

```bash
cd weather-bot && cargo build --release
cd ../arbitrage-bot && cargo build --release
```

Run each bot:

```bash
cd weather-bot && cargo run -- --dry-run
cd ../arbitrage-bot && cargo run --release -- --dry-run
```

From repo root you can also run both in parallel with:

```bash
./run-bots.sh dry-run
```

## Trade Logs

By default, runtime logs are written to:

- `../TRADES/arbitrage-bot/trades-YYYY-MM-DD.jsonl`
- `../TRADES/weather-bot/trades-YYYY-MM-DD.jsonl`

Override root with `TRADES_DIR=/custom/root` (each bot still writes to its own subfolder).
