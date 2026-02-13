# Kalshi Arbitrage Bot

Rust bot that scans grouped Kalshi contracts and executes Buy-Set/Sell-Set opportunities under strict execution and risk controls.

## Runtime Flow

1. Discover mutually exclusive market groups.
2. Maintain live quote book state.
3. Evaluate guaranteed arbitrage and optional EV-mode setups.
4. Enforce risk limits before execution.
5. Submit grouped orders with FOK-style handling.
6. Unwind partial legs when needed and journal all events.

## Quick Start

```bash
# 1) Set credentials
export KALSHI_API_KEY="your-key"
export KALSHI_SECRET_KEY="-----BEGIN RSA PRIVATE KEY-----\n..."

# 2) Authentication check
cargo run --release -- --check-auth

# 3) Dry run (no live orders)
cargo run --release -- --dry-run

# 4) Live run
cargo run --release

# 5) Watch trade journal (from this directory)
tail -f ../../TRADES/arbitrage-bot/trades-$(date +%F).jsonl
```

## Configuration

Primary controls live in `config.toml`.

Recommended defaults for safer startup:

- keep `arb.guaranteed_arb_only = true`
- reduce discovery horizon with `arb.max_days_to_resolution`
- tune `timing.quote_stale_secs` if stale quote warnings are frequent

Environment options:

- required: `KALSHI_API_KEY`, `KALSHI_SECRET_KEY`
- optional endpoint overrides: `KALSHI_API_BASE_URL` (REST), `KALSHI_WS_URL` (WebSocket)
- optional WS auth bypass for diagnostics: `KALSHI_WS_DISABLE_AUTH=1`
- optional trade root override: `TRADES_DIR=/custom/root`

## Strategy and Risk Features

- Buy-Set arbitrage: buy YES across a group when combined cost is below payout
- Sell-Set arbitrage: sell YES across a group when combined revenue is above payout
- EV-mode: discounted expected value handling for non-exhaustive groups
- risk controls: per-event and portfolio exposure caps, kill switch, emergency unwind

## Trade Journal and Health

Default output path:

- `<repo-root>/TRADES/arbitrage-bot/trades-YYYY-MM-DD.jsonl`

Override root:

- `TRADES_DIR=/custom/root` writes to `TRADES_DIR/arbitrage-bot`

The bot emits a 30-second `HEARTBEAT` line for liveness monitoring.

## Architecture

- `crates/arb_strategy/universe.rs`: group construction (`ArbGroup`)
- `crates/arb_strategy/quotes.rs`: quote ingestion and freshness checks
- `crates/arb_strategy/arb.rs`: opportunity evaluation
- `crates/arb_strategy/exec.rs`: order submission and lifecycle
- `crates/arb_strategy/risk.rs`: pre/post-trade safety checks
- `src/main.rs`: binary entrypoint
