# Trace API

Unified observability API + dashboard host.

It reads:

- `TRADES/*/trades-YYYY-MM-DD.jsonl`
- broker workflow state from `temporal-broker`

and exposes Google-style resources under `/v1/projects/*/locations/*/...`.

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

## Environment

- `TRACE_API_HOST` (default `127.0.0.1`)
- `TRACE_API_PORT` (default `8791`)
- `TRADES_DIR` (default `<repo>/TRADES`)
- `BROKER_BASE_URL` (default `http://127.0.0.1:8787`)
- `BROKER_STATE_FILE` (default `<repo>/.trading-cli/observability/broker-state.json`)
- `TEMPORAL_UI_URL` (default `http://127.0.0.1:8233`)
- `OBS_PROJECT` (default `local`)
- `OBS_LOCATION` (default `us-central1`)
- `OBS_CONTROL_TOKEN` (default `local-dev-token`)
- `OBS_CONTROL_AUDIT_FILE` (default `<repo>/.trading-cli/observability/control-audit.jsonl`)

## Core V1 Endpoints

Read:

- `GET /v1/projects/{project}/locations/{location}/workflows`
- `GET /v1/projects/{project}/locations/{location}/workflows/{workflow}`
- `GET /v1/projects/{project}/locations/{location}/workflows/{workflow}/events`
- `GET /v1/projects/{project}/locations/{location}/executions`
- `GET /v1/projects/{project}/locations/{location}/operations`
- `GET /v1/projects/{project}/locations/{location}/operations/{operation}`

Control (requires bearer token):

- `POST /v1/projects/{project}/locations/{location}/workflows/{workflow}:execute`
- `POST /v1/projects/{project}/locations/{location}/workflows/{workflow}:cancel`
- `POST /v1/projects/{project}/locations/{location}/workflows/{workflow}:hardCancel`
- `POST /v1/projects/{project}/locations/{location}/services/sports-agent:stop`

## Runtime State

Each workflow includes:

- `state`: lifecycle state from trace/workflow events (`RUNNING`, `AWAITING_APPROVAL`, etc.)
- `runtimeState`: managed process state (`PROCESS_RUNNING`, `PROCESS_STOPPED`, `UNKNOWN`)

This separation is why a historical workflow can still show `RUNNING` while the service process is stopped.

## Legacy Endpoints

`/api/*` routes remain for compatibility and return deprecation headers.
