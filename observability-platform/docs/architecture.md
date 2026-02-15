# Architecture

## Goals

- Unified trace view across legacy Rust bots and the sports workflow.
- Temporal-like observability with robust execute/cancel controls.
- Google-style API contracts for control and monitoring.

## Components

1. `sports-agent-worker` (Rust)
   - Research loop with SportsDataIO + The Odds API.
   - Emits structured JSONL trace events.
   - Polls broker workflow state for approval/cancel decisions.
2. `temporal-broker` (Node)
   - Workflow state machine and operation store.
   - Supports `execute`, `cancel`, `hardCancel` transitions.
   - Persists to `.trading-cli/observability/broker-state.json`.
3. `trace-api` (Node)
   - Reads `TRADES/*` and broker state.
   - Exposes `/v1/projects/*/locations/*/...` resources.
   - Hosts dashboard static assets.
4. `dashboard` (static)
   - Execution-first trace UX.
   - Shows both lifecycle `state` and process `runtimeState`.
   - Runs control actions with token auth.

## Workflow State Model

Internal statuses:

- `running`
- `awaiting_approval`
- `approved`
- `executed`
- `completed`
- `failed`
- `canceled_soft`
- `canceled_hard`

Control actions:

- `:execute` only from `awaiting_approval`
- `:cancel` from non-terminal states
- `:hardCancel` from non-terminal states (or escalates over soft)

## API Model

Resource patterns:

- `projects/{project}/locations/{location}/workflows/{workflow}`
- `projects/{project}/locations/{location}/operations/{operation}`
- `projects/{project}/locations/{location}/services/sports-agent`

Primary control methods:

- `POST .../workflows/{workflow}:execute`
- `POST .../workflows/{workflow}:cancel`
- `POST .../workflows/{workflow}:hardCancel`
- `POST .../services/sports-agent:stop`

## Data Paths

- Trade events: `TRADES/<bot>/trades-YYYY-MM-DD.jsonl`
- Broker state: `.trading-cli/observability/broker-state.json`
- Control audit: `.trading-cli/observability/control-audit.jsonl`
