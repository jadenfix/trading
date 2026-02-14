# LLM Workflows

Workspace for LLM-driven trading workflows.

## Current Workflow

- `llm-rules-bot`: rules-risk and research-triggered workflow with deterministic trade gates.
  - Supports direct Anthropic research or optional Temporal broker-triggered research.

## Safety Model

- LLM returns structured features only.
- Deterministic Rust logic owns side, edge, sizing, and execution gating.
- Shadow mode is default.

## Run

```bash
cd llm-workflows/llm-rules-bot
cargo run
```

Use `config.toml` plus environment variables:

- `KALSHI_API_KEY`
- `KALSHI_SECRET_KEY`
- `ANTHROPIC_API_KEY`
