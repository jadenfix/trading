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

### 3. Run all tests from repo root

From repo root:

```bash
bash ./trading-cli test
```

### 4. Run workflows from the orchestrator

From repo root:

```bash
bash ./trading-cli weather go dry-run
bash ./trading-cli arbitrage go dry-run
bash ./trading-cli llm-workflow go
```

Workflow command reference:

```bash
bash ./trading-cli weather [go|up|down|status|logs] [prod|dry-run|check-auth]
bash ./trading-cli arbitrage [go|up|down|status|logs] [prod|dry-run|check-auth]
bash ./trading-cli llm-workflow [go|up|down|status|logs]
bash ./trading-cli temporal [up|down|status|logs|ui|list|describe|show]
```

Launch all three in background dev mode:

```bash
bash ./trading-cli dev
bash ./trading-cli status
bash ./trading-cli down
```

Or run bots directly:

```bash
cd non-agent-workflows/weather-bot && cargo run -- --dry-run
cd non-agent-workflows/arbitrage-bot && cargo run --release -- --dry-run
```

Direct script is still available:

```bash
./run-bots.sh dry-run
```

## Temporal Debugger

Use the orchestrator to run and inspect local Temporal while workflows are active:

```bash
brew install temporal
bash ./trading-cli temporal up
bash ./trading-cli temporal status
bash ./trading-cli temporal ui
bash ./trading-cli temporal list
bash ./trading-cli temporal describe <workflow_id>
bash ./trading-cli temporal show <workflow_id>
bash ./trading-cli temporal logs
bash ./trading-cli temporal down
```

Optional environment overrides:

```bash
export TEMPORAL_ADDRESS="127.0.0.1:7233"
export TEMPORAL_SERVER_IP="127.0.0.1"
export TEMPORAL_UI_IP="127.0.0.1"
export TEMPORAL_UI_PORT="8233"
```

## Testing

Run all current test suites from repo root:

```bash
bash ./trading-cli test            # inclusive default
bash ./trading-cli test inclusive  # explicit inclusive alias
```

Run a single suite:

```bash
bash ./trading-cli test common
bash ./trading-cli test kalshi-client
bash ./trading-cli test weather
bash ./trading-cli test arbitrage
bash ./trading-cli test llm-workflow
```

CI uses the same command (`bash ./trading-cli test`) on every push and pull request.

## Contributing

Before opening a pull request:

1. Keep changes scoped to the relevant workspace(s).
2. Run tests from the repo root with `bash ./trading-cli test`.
3. Update docs/README files when behavior or commands change.
4. Include a clear PR description covering what changed, why it changed, and how it was tested.

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
