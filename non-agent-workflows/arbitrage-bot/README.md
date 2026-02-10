# Kalshi Arbitrage Bot

A high-performance arbitrage bot for Kalshi prediction markets.

## Features

- **Market Discovery**: Automatically discovers mutually exclusive event groups (e.g., "Will High Temp be > X?", "Will Fed hike rates?").
- **Arbitrage Strategies**:
  - **Buy-Set Arb**: Buys 'Yes' on all outcomes when total cost < payout ($1.00).
  - **Sell-Set Arb**: Sells 'Yes' on all outcomes when total revenue > payout ($1.00).
  - **EV-Mode**: Calculates probabilistic value for non-exhaustive sets (e.g. ranges that don't cover all possibilities) using EV discounting.
- **Execution**: Batched FOK (Fill-Or-Kill) orders for atomic-like execution.
- **Risk Management**:
  - Emergency Unwind: Immediately liquidates partial fills to minimize leg risk.
  - Exposure Limits: Per-event and portfolio-wide caps.
  - Kill Switch: Stops trading after consecutive failures.

## Usage

1. **Configure**:
   - Copy `config.toml` and adjust thresholds (min profit, slippage buffer, etc.).
   - Keep `arb.guaranteed_arb_only = true` for only mathematically guaranteed set arbs.
   - Set environment variables `KALSHI_API_KEY` and `KALSHI_SECRET_KEY`.
   - If logs show frequent `no quote data` / `stale quotes`, increase `timing.quote_stale_secs`.
   - Optional endpoint overrides for network/DNS issues:
     - `KALSHI_API_BASE_URL` (REST base URL, e.g. `https://api.elections.kalshi.com`)
     - `KALSHI_WS_URL` (WS URL, e.g. `wss://api.elections.kalshi.com/trade-api/ws/v2`)
   - Optional WS auth toggle for public ticker feed debugging:
     - `KALSHI_WS_DISABLE_AUTH=1` (skips WS auth headers)
   - Trade journal:
     - Bot writes JSONL trade/audit events to `<repo-root>/TRADES/arbitrage-bot` by default
       (e.g. `/Users/jadenfix/Desktop/trading/TRADES/arbitrage-bot/trades-YYYY-MM-DD.jsonl`).
     - Set `TRADES_DIR=/custom/root` to choose a different trades root. The bot still writes into
       `TRADES_DIR/arbitrage-bot`.
   - Heartbeat:
     - Bot emits a 30-second `HEARTBEAT` log line so long runs do not appear stalled.

2. **Run**:
   ```bash
   # Dry run (no orders submitted)
   cargo run --release -- --dry-run
   
   # Live trading (environment from config.toml / USE_DEMO)
   cargo run --release

   # Check authentication only
   cargo run --release -- --check-auth
   ```

## Architecture

- **`crates/arb_strategy`**: Core logic library.
  - `universe.rs`: Structuring markets into `ArbGroup`s.
  - `quotes.rs`: Real-time orderbook management.
  - `arb.rs`: Opportunity evaluation.
  - `exec.rs`: Order execution and lifecycle.
  - `risk.rs`: Safety checks.
- **`src/main.rs`**: Application entry point.
- **`libs/`**: Shared formatting and API clients (see top-level README).
