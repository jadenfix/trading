# Observability Platform

Unified observability + workflow control stack for this repository.

## What This Adds

- End-to-end trace visibility across:
  - `weather-bot`
  - `arbitrage-bot`
  - `llm-rules-bot`
  - `sports-agent`
- Google-style control API (`/v1/projects/*/locations/*/...`).
- Execution-first dashboard with workflow timeline + actions.
- Sports research worker with `hitl` and `auto_ultra_strict` modes.

## Architecture

```text
bots + sports-agent worker
          |
          v
 TRADES/* JSONL + broker state
          |
          v
 trace-api (:8791)
  - /v1 workflow resources
  - /v1 control actions
  - dashboard static UI
          |
          v
temporal-broker (:8787)
  - workflow state machine
  - operations + idempotency
  - legacy compatibility routes
```

Temporal UI still runs in parallel for workflow inspection.

## Quickstart

From repo root:

```bash
# Optional hardening: set your own token
export OBS_CONTROL_TOKEN="replace-me"

# Start observability stack
bash ./trading-cli observability up

# Start sports agent in HITL mode
bash ./trading-cli sports-agent up --mode hitl

# Open dashboard
bash ./trading-cli observability ui
```

## CLI Control Examples

```bash
# Execute a pending HITL workflow
bash ./trading-cli sports-agent execute <workflow_id>

# Soft cancel
bash ./trading-cli sports-agent cancel <workflow_id>

# Hard cancel
bash ./trading-cli sports-agent cancel <workflow_id> --hard

# Stop sports-agent managed process via control API
bash ./trading-cli sports-agent stop-service
```

## Main URLs

- Dashboard: `http://127.0.0.1:8791`
- API root: `http://127.0.0.1:8791/v1/projects/local/locations/us-central1/workflows`
- Temporal UI: `http://127.0.0.1:8233`
- Broker health: `http://127.0.0.1:8787/health`

## Folder Map

- `contracts/`: OpenAPI + schemas.
- `temporal-broker/`: workflow state machine and operations backend.
- `trace-api/`: trace query + control facade and dashboard host.
- `dashboard/`: browser client.
- `sports-agent-worker/`: Rust workflow runner.
- `docs/architecture.md`: deeper architecture detail.
- `docs/operations.md`: operational runbook.
