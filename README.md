# Agentic Trading Monorepo

Monorepo for four trading/automation tracks:

- agent-style workflows (`clawdbot-workflows`)
- deterministic Rust bots (`non-agent-workflows`)
- LLM workflow (`llm-workflows`)
- unified observability + sports agent stack (`observability-platform`)

## Workspace Map

| Workspace | Purpose | Start Here |
| --- | --- | --- |
| [`clawdbot-workflows`](./clawdbot-workflows) | Imported OpenClaw-based agent framework and gateway stack | [`clawdbot-workflows/README.md`](./clawdbot-workflows/README.md) |
| [`non-agent-workflows`](./non-agent-workflows) | Kalshi-focused Rust bots (weather + arbitrage) | [`non-agent-workflows/README.md`](./non-agent-workflows/README.md) |
| [`llm-workflows`](./llm-workflows) | LLM trading experiments and prototypes | [`llm-workflows/README.md`](./llm-workflows/README.md) |
| [`observability-platform`](./observability-platform) | Unified traces, Google-style control API, dashboard, sports worker | [`observability-platform/README.md`](./observability-platform/README.md) |

## Architecture (High Level)

```text
weather/arbitrage/llm-rules/sports-agent
                |
                v
      TRADES/* JSONL + broker state
                |
                v
         trace-api (:8791)
   /v1/projects/*/locations/*/...
                |
                v
   dashboard (executions first + actions)

temporal-broker (:8787) stores workflow state machine + operations.
Temporal UI runs in parallel for workflow inspection.
```

Detailed architecture:

- [`observability-platform/docs/architecture.md`](./observability-platform/docs/architecture.md)

## Quick Start

### 1) Install dependencies

```bash
pnpm install
```

### 2) Configure credentials

Required for Kalshi-connected workflows:

```bash
export KALSHI_API_KEY="your-key-id"
export KALSHI_SECRET_KEY="your-private-key"
```

Optional/feature-specific:

```bash
export GOOGLE_WEATHER_API_KEY="your-google-key"
export WEATHER_BOT_CONTACT_EMAIL="you@yourdomain.com"
export OPENAI_API_KEY="your-openai-key"
export ANTHROPIC_API_KEY="your-anthropic-key"
export SPORTS_DATA_IO_API_KEY="your-sportsdataio-key"
export THE_ODDS_API_KEY="your-theodds-key"
```

Control token (recommended):

```bash
export OBS_CONTROL_TOKEN="replace-me"
```

If unset, local default is `local-dev-token`.

### 3) Start observability stack

```bash
bash ./trading-cli observability up
bash ./trading-cli observability status
bash ./trading-cli observability ui
```

That starts:

- Temporal (local dev server)
- temporal-broker (`:8787`)
- trace-api + dashboard (`:8791`)

### 4) Run workflows

```bash
bash ./trading-cli weather go dry-run
bash ./trading-cli arbitrage go dry-run
bash ./trading-cli llm-workflow go
```

Run sports agent:

```bash
# Human-in-the-loop mode
bash ./trading-cli sports-agent up --mode hitl

# Execute pending recommendation
bash ./trading-cli sports-agent execute <workflow_id>

# Cancel
bash ./trading-cli sports-agent cancel <workflow_id>

# Hard cancel
bash ./trading-cli sports-agent cancel <workflow_id> --hard

# Stop sports-agent managed process via control API
bash ./trading-cli sports-agent stop-service

# Ultra strict auto mode
bash ./trading-cli sports-agent up --mode auto_ultra_strict
```

### 5) View traces

- Dashboard: `http://127.0.0.1:8791`
- Workflows API: `http://127.0.0.1:8791/v1/projects/local/locations/us-central1/workflows`
- Temporal UI: `http://127.0.0.1:8233`

### 6) Stop services

```bash
bash ./trading-cli down
```

## `trading-cli` Command Reference

```bash
bash ./trading-cli weather [go|up|down|status|logs] [prod|dry-run|check-auth]
bash ./trading-cli arbitrage [go|up|down|status|logs] [prod|dry-run|check-auth]
bash ./trading-cli llm-workflow [go|up|down|status|logs]
bash ./trading-cli temporal [up|down|status|logs|ui|list|describe|show]
bash ./trading-cli observability [up|down|status|logs|ui] [trace-api|temporal-broker|temporal]
bash ./trading-cli sports-agent [go|up|down|status|logs|approve|execute|cancel|hard-cancel|stop-service] [--mode <hitl|auto_ultra_strict>] [--dry-run] [trace_id]
bash ./trading-cli dev
bash ./trading-cli status
bash ./trading-cli down
```

## Temporal Debugger

```bash
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

Run all suites from repo root:

```bash
bash ./trading-cli test
bash ./trading-cli test inclusive
```

Run single suites:

```bash
bash ./trading-cli test common
bash ./trading-cli test kalshi-client
bash ./trading-cli test weather
bash ./trading-cli test arbitrage
bash ./trading-cli test llm-workflow
bash ./trading-cli test sports-agent
bash ./trading-cli test observability
```

## Bot Summaries

### Weather Bot

Path: [`non-agent-workflows/weather-bot`](./non-agent-workflows/weather-bot)

- discovers weather markets
- blends NOAA and Google forecasts
- applies quality + risk gates before order intents
- writes runtime JSONL events to `TRADES/weather-bot`

### Arbitrage Bot

Path: [`non-agent-workflows/arbitrage-bot`](./non-agent-workflows/arbitrage-bot)

- discovers mutually exclusive contract sets
- scans Buy-Set and Sell-Set opportunities
- executes grouped orders with strict risk checks
- writes runtime JSONL events to `TRADES/arbitrage-bot`

### Sports Agent Worker

Path: [`observability-platform/sports-agent-worker`](./observability-platform/sports-agent-worker)

- researches sports opportunities continuously while running
- uses deterministic Bayesian + EV + CLV proxy gates (no ML)
- supports `hitl` and `auto_ultra_strict` execution modes
- writes runtime JSONL events to `TRADES/sports-agent`

## Logs and Journals

Default trade logs:

- `TRADES/arbitrage-bot`
- `TRADES/weather-bot`
- `TRADES/llm-rules-bot`
- `TRADES/sports-agent`

Use `TRADES_DIR=/custom/path` to change the root folder.
