# Temporal Broker

Stateful workflow broker for observability control-plane actions.

## Responsibilities

- Persist workflow status, approval, cancel state, and command metadata.
- Serve Google-style workflow resources and long-running operations.
- Keep legacy compatibility endpoints used by existing workers.
- Write immutable control audit records.

## Run

From repo root:

```bash
node observability-platform/temporal-broker/broker.mjs
```

Or use orchestrator:

```bash
bash ./trading-cli observability up
```

## Environment

- `BROKER_HOST` (default `127.0.0.1`)
- `BROKER_PORT` (default `8787`)
- `BROKER_STATE_FILE` (default `<repo>/.trading-cli/observability/broker-state.json`)
- `BROKER_AUDIT_FILE` (default `<repo>/.trading-cli/observability/control-audit.jsonl`)
- `OBS_PROJECT` (default `local`)
- `OBS_LOCATION` (default `us-central1`)

## V1 Endpoints

- `GET /health`
- `GET /v1/projects/{project}/locations/{location}/workflows`
- `GET /v1/projects/{project}/locations/{location}/workflows/{workflow}`
- `POST /v1/projects/{project}/locations/{location}/workflows/{workflow}:execute`
- `POST /v1/projects/{project}/locations/{location}/workflows/{workflow}:cancel`
- `POST /v1/projects/{project}/locations/{location}/workflows/{workflow}:hardCancel`
- `GET /v1/projects/{project}/locations/{location}/operations`
- `GET /v1/projects/{project}/locations/{location}/operations/{operation}`

## Legacy Compatibility Endpoints

- `POST /research/start`
- `GET /research/{workflow_id}`
- `POST /workflows/register`
- `GET /workflows/{workflow_id}`
- `POST /workflows/{workflow_id}/complete`
- `POST /workflows/{workflow_id}/events`
- `POST /execution/{workflow_id}/approve`
- `GET /execution/{workflow_id}/approval`

## Notes

- `execute` is valid only from `awaiting_approval`.
- `cancel` and `hardCancel` produce terminal `canceled_soft` / `canceled_hard` states.
- Repeated requests with the same `requestId` are idempotent (same operation returned).
