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

For an isolated, credential-free walkthrough of the real UI and control APIs:

```bash
node observability-platform/paper-demo.mjs
```

Open the printed dashboard URL and paste the printed local capability into
`Control Token`. Inspect the synthetic `PAPER-SF-TEMP` fill, approve
`paper-review`, and cancel `paper-cancel`. All fixtures use paper mode; the
launcher starts only the local broker and trace API, with fresh temporary
state, audit files, and PID lookup directory. It starts no exchange connector,
model, worker, or Temporal server. Approval changes broker state and does not
place an order. Ctrl+C stops both processes and removes the demo data. This is
a fixture demonstration of the observability/control layer, not proof of live
trading or strategy performance.

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
