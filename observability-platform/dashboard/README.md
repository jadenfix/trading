# Dashboard

Browser UI for unified workflow traces.

## What You Can Do

- View cross-bot trace list in one screen.
- See an execution-first strip that shows actual executed trades first.
- Inspect per-trace event timeline and payloads.
- See Temporal UI deep-link from header.
- Approve HITL traces directly from UI (`Approve + Execute`).

## Run

Use orchestrator from repo root:

```bash
bash ./trading-cli observability up
bash ./trading-cli observability ui
```

The dashboard is served by trace API on:

- `http://127.0.0.1:8791`

## Data Source

The dashboard reads from:

- `GET /api/traces`
- `GET /api/traces/{trace_id}`
- `GET /api/traces/{trace_id}/events`
- `POST /api/traces/{trace_id}/approve`
