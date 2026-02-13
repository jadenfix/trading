# Agentic Trading Monorepo

Monorepo for three trading tracks:

- agent-style workflows (`clawdbot-workflows`)
- deterministic Rust bots (`non-agent-workflows`)
- LLM research workspace (`llm-workflows`)

## Workspace Map

| Workspace | Purpose | Start Here |
| --- | --- | --- |
| [`clawdbot-workflows`](./clawdbot-workflows) | Imported OpenClaw-based agent framework and gateway stack | [`clawdbot-workflows/README.md`](./clawdbot-workflows/README.md) |
| [`non-agent-workflows`](./non-agent-workflows) | Kalshi-focused Rust bots (weather + arbitrage) | [`non-agent-workflows/README.md`](./non-agent-workflows/README.md) |
| [`llm-workflows`](./llm-workflows) | LLM trading experiments and prototypes (early scaffold) | [`llm-workflows/README.md`](./llm-workflows/README.md) |

## Quick Start

### 1. Install dependencies

```bash
pnpm install
```

### 2. Configure required credentials

```bash
export KALSHI_API_KEY="your-key-id"
export KALSHI_SECRET_KEY="your-private-key"
```

Optional:

```bash
export GOOGLE_WEATHER_API_KEY="your-google-key"
export WEATHER_BOT_CONTACT_EMAIL="you@yourdomain.com"
export OPENAI_API_KEY="your-openai-key"
export ANTHROPIC_API_KEY="your-anthropic-key"
```

### 3. Run non-agent bots

From repo root:

```bash
./run-bots.sh dry-run
```

Or run each bot directly:

```bash
cd non-agent-workflows/weather-bot && cargo run -- --dry-run
cd non-agent-workflows/arbitrage-bot && cargo run --release -- --dry-run
```

## Non-Agent Bot Summaries

### Weather Bot

Path: [`non-agent-workflows/weather-bot`](./non-agent-workflows/weather-bot)

- discovers weather markets
- blends NOAA and Google forecasts
- applies quality + risk gates before sending orders
- writes JSONL runtime events to `TRADES/weather-bot`

Details: [`non-agent-workflows/weather-bot/README.md`](./non-agent-workflows/weather-bot/README.md)

### Arbitrage Bot

Path: [`non-agent-workflows/arbitrage-bot`](./non-agent-workflows/arbitrage-bot)

- discovers mutually exclusive contract sets
- scans Buy-Set and Sell-Set opportunities
- executes grouped orders with strict risk checks
- writes JSONL runtime events to `TRADES/arbitrage-bot`

Details: [`non-agent-workflows/arbitrage-bot/README.md`](./non-agent-workflows/arbitrage-bot/README.md)

## Logs and Journals

By default, trade logs are written under:

- `TRADES/arbitrage-bot`
- `TRADES/weather-bot`

Use `TRADES_DIR=/custom/path` to change the root folder.
