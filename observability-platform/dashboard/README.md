# Dashboard

Browser UI for unified workflow traces and control actions.

## What You Can Do

- See executed trades first (`Executed Trades` strip).
- Inspect cross-bot workflow traces and timelines.
- View lifecycle `state` and process `runtimeState` side-by-side.
- Execute / soft-cancel / hard-cancel workflows.
- Stop the managed `sports-agent` service.
- Jump to Temporal UI from the header.

## Run

From repo root:

```bash
bash ./trading-cli observability up
bash ./trading-cli observability ui
```

Dashboard URL:

- `http://127.0.0.1:8791`

## Control Auth

Set token before using control buttons:

```bash
export OBS_CONTROL_TOKEN="your-local-token"
```

If unset, local default is `local-dev-token`.

The UI includes a `Control Token` input (stored in local browser storage).

## Data Sources

- `GET /v1/projects/*/locations/*/workflows`
- `GET /v1/projects/*/locations/*/workflows/*`
- `GET /v1/projects/*/locations/*/workflows/*/events`
- `GET /v1/projects/*/locations/*/executions`
- `POST /v1/projects/*/locations/*/workflows/*:execute|:cancel|:hardCancel`
- `POST /v1/projects/*/locations/*/services/sports-agent:stop`
