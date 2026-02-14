# Temporal Broker

HTTP broker for workflow status, approval checkpoints, and compatibility with existing `/research/*` calls.

## Responsibilities

- Register workflow runs and status transitions.
- Expose workflow polling APIs (`/research/start`, `/research/{id}`).
- Store and serve approval state for human-in-the-loop execution.
- Persist workflow metadata to disk (`.trading-cli/observability/broker-state.json`).

## Run

From repo root:

```bash
node observability-platform/temporal-broker/broker.mjs
```

Or use the orchestrator:

```bash
bash ./trading-cli observability up
```

## Environment

- `BROKER_HOST` (default `127.0.0.1`)
- `BROKER_PORT` (default `8787`)
- `BROKER_STATE_FILE` (default `<repo>/.trading-cli/observability/broker-state.json`)

## Endpoints

- `GET /health`
- `POST /research/start`
- `GET /research/{workflow_id}`
- `POST /workflows/register`
- `GET /workflows/{workflow_id}`
- `POST /workflows/{workflow_id}/complete`
- `POST /workflows/{workflow_id}/events`
- `POST /execution/{workflow_id}/approve`
- `GET /execution/{workflow_id}/approval`

## Example

```bash
curl -sS -X POST http://127.0.0.1:8787/workflows/register \
  -H 'content-type: application/json' \
  -d '{
    "workflow_id": "sports-research-123",
    "trace_id": "sports-research-123",
    "mode": "hitl",
    "source_bot": "sports-agent",
    "requires_approval": true,
    "status": "awaiting_approval"
  }'

curl -sS -X POST http://127.0.0.1:8787/execution/sports-research-123/approve \
  -H 'content-type: application/json' \
  -d '{"approved_by": "operator", "command_context": "trading-cli"}'
```
