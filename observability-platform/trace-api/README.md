# Trace API

Unified read API over:

- `TRADES/*/trades-YYYY-MM-DD.jsonl`
- workflow/approval state from `temporal-broker`

It builds normalized trace envelopes so you can inspect weather, arbitrage, llm-rules, and sports-agent runs in one timeline.

## Run

From repo root:

```bash
node observability-platform/trace-api/server.mjs
```

Or use orchestrator:

```bash
bash ./trading-cli observability up
```

Then open:

- `http://127.0.0.1:8791` (dashboard)
- `http://127.0.0.1:8791/api/traces`

## Environment

- `TRACE_API_HOST` (default `127.0.0.1`)
- `TRACE_API_PORT` (default `8791`)
- `TRADES_DIR` (default `<repo>/TRADES`)
- `BROKER_BASE_URL` (default `http://127.0.0.1:8787`)
- `BROKER_STATE_FILE` (default `<repo>/.trading-cli/observability/broker-state.json`)
- `TEMPORAL_UI_URL` (default `http://127.0.0.1:8233`)

## Endpoints

- `GET /health`
- `GET /api/config`
- `GET /api/traces?bot=&status=&from=&to=&limit=`
- `GET /api/executions?bot=&limit=` (flattened executed trades feed)
- `GET /api/traces/{trace_id}`
- `GET /api/traces/{trace_id}/events`
- `POST /api/traces/{trace_id}/approve`

## Notes

- Existing bots that do not emit explicit `trace_id` are grouped into synthetic traces by cycle boundaries.
- If a trace maps to a broker workflow, approval/status fields are merged into the trace envelope.
- Trace sorting prioritizes traces with real executions (`order_placed`, successful `execution_result`).
