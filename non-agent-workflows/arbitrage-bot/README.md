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
   - Set environment variables `KALSHI_API_KEY` and `KALSHI_SECRET_KEY`.

2. **Run**:
   ```bash
   # Dry run (no orders submitted)
   cargo run --release -- --dry-run
   
   # Live trading (DEMO environment by default)
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
