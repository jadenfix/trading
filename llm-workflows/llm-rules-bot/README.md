# LLM Rules Bot

Rust workflow for rules-risk and research-triggered market analysis with deterministic execution control.

## Key Behavior

- LLM output is schema-validated and used as features only.
- Deterministic logic computes edge, side, and size.
- Budget limits enforce daily and per-market LLM call caps.
- `live_enable` and `shadow_mode` are both enforced.
- Risk gates run before live order placement.
- Temporal research can be enabled as an optional trigger backend.

## Required Env Vars

- `KALSHI_API_KEY`
- `KALSHI_SECRET_KEY`
- `ANTHROPIC_API_KEY` (required only when `temporal.enabled=false`)

If Temporal broker auth is enabled:

- `<TEMPORAL_AUTH_TOKEN_ENV>` (whatever `temporal.auth_token_env` is set to)

## Optional Temporal Trigger

Set this in `config.toml`:

```toml
[temporal]
enabled = true
broker_base_url = "http://127.0.0.1:8787"
start_path = "/research/start"
poll_path_template = "/research/{id}"
poll_interval_ms = 250
workflow_timeout_ms = 5000
request_timeout_ms = 2000
auth_token_env = ""
```

Behavior:

- `enabled=false`: direct Anthropic call from Rust.
- `enabled=true`: Rust starts a Temporal workflow via broker, polls for completion, validates response schema, then continues deterministic decisioning.

Expected broker contract:

- `POST /research/start` returns a workflow id (`workflow_id`, `id`, `job_id`, etc.).
- `GET /research/{id}` returns status and eventually `result` containing `ResearchResponse` JSON.

## Run

```bash
cargo run
```

## Test

```bash
cargo test --workspace
```

## Temporal Debugger / Inspection

Start local Temporal dev server (if not already running):

```bash
temporal server start-dev --ui-port 8233
```

Useful debugger commands:

```bash
temporal workflow list --address localhost:7233
temporal workflow describe --workflow-id <workflow_id> --address localhost:7233
temporal workflow show --workflow-id <workflow_id> --address localhost:7233
```

UI:

- Open `http://localhost:8233`
- Search by workflow id logged in `TRADES/llm-rules-bot/trades-YYYY-MM-DD.jsonl` under `research_accepted` / `research_stale`
