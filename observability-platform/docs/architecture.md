# Architecture

## Goals

- Unified trace view across legacy Rust bots and new sports workflow.
- Temporal-style workflow observability with explicit approval checkpoints.
- Deterministic non-ML quant gate for sports recommendations.

## Components

1. `sports-agent-worker` (Rust)
   - Research loop with SportsDataIO + The Odds API.
   - Kalshi market scan and Bayesian gate calculation.
   - Emits structured JSONL events and workflow metadata.
2. `temporal-broker` (Node)
   - Registers workflows and statuses.
   - Supports HITL approval API.
   - Backward-compatible `/research/start` and `/research/{id}`.
3. `trace-api` (Node)
   - Reads all `TRADES/*` files.
   - Merges broker workflow state.
   - Produces normalized traces for UI/CLI.
4. `dashboard` (Static)
   - Cross-bot trace list and timeline explorer.
   - Trigger approval for pending HITL traces.

## Trace Envelope Model

Each trace is represented by:

- `trace_id`
- `workflow_id`
- `source_bot`
- `mode`
- `ts_start`
- `ts_end`
- `status`
- `requires_approval`
- `approval`
- `event_count`

Each event is represented by:

- `trace_id`
- `span_id`
- `parent_span_id` (optional)
- `kind`
- `ts`
- `severity`
- `payload`
- `source_file`

## Sports Quant Flow

1. Fetch The Odds API market prices.
2. Fetch SportsDataIO game feed snapshots.
3. Build team rating signal from completed games.
4. Convert market data into implied probabilities.
5. Blend odds + stats signal.
6. Bayesian posterior update and credible interval.
7. Compute net EV with fees/slippage.
8. Apply mode gates:
   - `hitl`: moderate gate + operator approval required
   - `auto_ultra_strict`: strict posterior/EV/CLV/disagreement gate

## Approval Flow

1. Worker emits recommendation and registers workflow (`awaiting_approval`).
2. Operator approves via CLI or dashboard.
3. Broker flips status to `approved`.
4. Worker polls and executes order.
5. Worker marks workflow `executed` or `failed`.

## Data Paths

- Trade events: `TRADES/<bot>/trades-YYYY-MM-DD.jsonl`
- Broker workflow state: `.trading-cli/observability/broker-state.json`
- Trace API reads both and builds unified timeline.
