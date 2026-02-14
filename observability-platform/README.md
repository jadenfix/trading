# Observability Platform

Unified observability and sports workflow runtime for this repository.

## What This Adds

- End-to-end trace visibility across:
  - `weather-bot`
  - `arbitrage-bot`
  - `llm-rules-bot`
  - `sports-agent`
- Temporal-compatible workflow broker with approval checkpoints.
- Trace API + dashboard for cross-bot timeline inspection.
- Sports research worker with `hitl` and `auto_ultra_strict` modes.

## Architecture

```text
                             +---------------------+
                             |  Temporal UI :8233  |
                             +----------+----------+
                                        |
                                        |
+----------------+      +---------------v---------------+       +----------------------+
| sports-agent   |<---->| temporal-broker :8787         |<----->| trading-cli approve  |
| worker (Rust)  |      | /workflows /research /approve |       | command + operators  |
+-------+--------+      +---------------+---------------+       +----------------------+
        |                               |
        | writes JSONL                  |
        v                               v
+-------+-------------------------------+------------------------+
|                    TRADES/* JSONL + broker state              |
+-------+-------------------------------+------------------------+
        |
        v
+-------+-------------------------------+
| trace-api :8791                       |
| /api/traces + dashboard static assets |
+-------+-------------------------------+
        |
        v
+-------+-------------------------------+
| Dashboard (browser)                   |
| list traces, inspect events, approve  |
+---------------------------------------+
```

## Quickstart

From repo root:

```bash
brew install temporal
# 1) Start observability stack (Temporal + broker + trace API)
bash ./trading-cli observability up

# 2) Run sports agent in HITL mode
bash ./trading-cli sports-agent up --mode hitl

# 3) Open dashboard
bash ./trading-cli observability ui

# 4) Approve a pending recommendation
bash ./trading-cli sports-agent approve <trace_id>
```

Stop everything:

```bash
bash ./trading-cli down
```

## Main URLs

- Trace dashboard: `http://127.0.0.1:8791`
- Trace API: `http://127.0.0.1:8791/api/traces`
- Temporal UI: `http://127.0.0.1:8233`
- Broker health: `http://127.0.0.1:8787/health`

## Folder Map

- `contracts/`: JSON schemas and API contract files.
- `temporal-broker/`: workflow/approval broker.
- `trace-api/`: unified trace ingestion + query server.
- `dashboard/`: browser client served by trace-api.
- `sports-agent-worker/`: Rust worker for sports analytics and order flow.
- `docs/architecture.md`: detailed architecture and trace model.
- `docs/operations.md`: operations and troubleshooting runbook.
