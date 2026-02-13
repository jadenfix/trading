# Kalshi Trading Bots

This repository contains algorithmic trading bots for the Kalshi prediction market.

## Structure

- **`libs/`**: Shared libraries used by all bots.
  - `libs/kalshi_client`: Wrapper for Kalshi API (REST + WebSocket).
  - `libs/common`: Shared domain types, error handling, and configuration utilities.

- **`weather-bot/`**: A bot that trades weather markets based on NOAA forecasts (optionally blended with Google Weather API).
  - Uses `libs/kalshi_client` and `libs/common`.
  - logic in `crates/strategy` and `crates/noaa_client`.

- **`arbitrage-bot/`**: A bot that exploits pricing inefficiencies (Buy-Set/Sell-Set arbitrage) in mutually exclusive event groups.
  - Uses `libs/kalshi_client` and `libs/common`.
  - Logic in `crates/arb_strategy`.

## Setup

1. **Prerequisites**:
   - Rust (stable toolchain).
   - A Kalshi account (and API keys).

2. **Configuration**:
   - Create a `.env` file in the root or specific bot directories with:
     ```env
     KALSHI_API_KEY=your_key_id
     KALSHI_SECRET_KEY="your_private_key_pem_content"
     USE_DEMO=true  # Set to false for production
     ```
   - Each bot also supports a `config.toml` for strategy parameters.

3. **Building**:
   - Each bot is a separate Cargo workspace.
   - `cd weather-bot && cargo build --release`
   - `cd arbitrage-bot && cargo build --release`

## Trade Logs

- Runtime trade/audit logs are written under repo root `TRADES/`:
  - `TRADES/arbitrage-bot/trades-YYYY-MM-DD.jsonl`
  - `TRADES/weather-bot/trades-YYYY-MM-DD.jsonl`
- Override root with `TRADES_DIR=/custom/root` (each bot still writes to its own subfolder).
