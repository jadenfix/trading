# Rust Trading Bridge

Low-latency bridge between OpenClaw tools and a Rust trading daemon over a Unix domain socket (UDS).

## Components

| Path | Purpose |
| --- | --- |
| `trading_protocol` | Shared frame codec + envelope schema (`type`, `id`, `payload`) |
| `trading_daemon` | UDS server (`/var/run/openclaw/trading.sock`) handling engine/strategy/risk/execution/portfolio commands |
| `tradingctl` | CLI client for daemon control/status/capabilities checks |
| `exchange_core` | Venue abstraction traits and normalized order/account types |
| `strategy_core` | Regime-aware strategy interface + signal intent schema |
| `risk_core` | Non-bypassable hard safety-cage policy and evaluators |
| `coinbase_at_adapter` | Coinbase Advanced Trade spot execution adapter (live route) |
| `paper_exchange_adapter` | Deterministic simulated execution adapter (paper route) |
| `../.openclaw/extensions/trading-bridge` | OpenClaw extension exposing tools to Clawdbot |

## How It Works

1. OpenClaw tool calls (`trading_hft`) are sent by the extension over UDS.
2. `trading_daemon` decodes framed JSON envelopes and processes control, risk, strategy, execution, and portfolio commands.
3. Responses are correlated by envelope `id` and returned to the tool caller.

Protocol details:

- Framing: 4-byte big-endian length prefix + JSON payload
- Envelope fields:
  - `v`: protocol version
  - `id`: request/response correlation UUID
  - `type`: command kind (for example `Engine.Status`)
  - `ts_ms`: timestamp (ms)
  - `payload`: JSON object

## Docker Run (Recommended)

From repo root:

```bash
./trading-cli clawdbot-trading-up
```

This brings up:

- `trading-daemon` (Rust daemon)
- `openclaw-gateway`
- `openclaw-cli`
- shared Docker volume for `/var/run/openclaw`

Quick verification:

```bash
./trading-cli clawdbot-trading-ping
./trading-cli clawdbot-trading-status
./trading-cli clawdbot-trading-capabilities
./trading-cli clawdbot-trading-doctor
./trading-cli clawdbot-trading-start
./trading-cli clawdbot-trading-stop
./trading-cli clawdbot-trading-ps
```

Expected status after `clawdbot-trading-up`:

- `trading-daemon`: `Up` (required)
- `openclaw-gateway`: `Up` (required)
- `openclaw-cli`: may be `Exited` (normal; this is an on-demand CLI helper container)

Shutdown:

```bash
./trading-cli clawdbot-trading-down
```

## Local Run (Without Docker)

Terminal 1:

```bash
cd crates
TRADING_SOCKET_PATH=/tmp/openclaw/trading.sock cargo run -p trading_daemon
```

Terminal 2:

```bash
cd crates
cargo run -p tradingctl -- --socket /tmp/openclaw/trading.sock ping
cargo run -p tradingctl -- --socket /tmp/openclaw/trading.sock status
cargo run -p tradingctl -- --socket /tmp/openclaw/trading.sock capabilities
```

## OpenClaw Tool Surface

The extension registers one unified control-plane tool:

- `trading_hft`: a single action-based interface for `health`, engine controls, risk controls, strategy lifecycle operations, execution actions, and portfolio reads.

Extension path:

- `.openclaw/extensions/trading-bridge/src/index.ts`

Socket path configuration:

- plugin config default: `/var/run/openclaw/trading.sock`
- override via `TRADING_SOCKET_PATH` environment variable

Daemon state configuration:

- `TRADING_DATA_DIR`: journal/state root (default `/var/lib/openclaw/trading`)
- `TRADING_STATE_PATH`: state snapshot path (default `${TRADING_DATA_DIR}/state/engine-state.json`)
- `TRADING_CANDIDATE_TTL_MS`: optional candidate expiration TTL in milliseconds (default `0`, disabled)
- `TRADING_ENGINE_MODE`: startup mode (`paper`, `hitl_live`, `auto_live`; default `auto_live`)

Command behavior notes:

- `Control.Status` is still accepted for compatibility, but clients should prefer `Engine.Status`.
- `Control.Capabilities` reports daemon protocol/schema compatibility (`protocol_version`, `status_schema_version`, supported command kinds, daemon build metadata).
- `Engine.Status` and `Control.Status` now share the same rich status payload shape and include mode, scoped kill switches, execution counters, and portfolio summary.
- Plugin/config changes require a gateway restart to take effect (`./trading-cli clawdbot-trading up` or container restart).
- `Control.Stop` now means halted but not paused (`running=false`, `paused=false`).
- `Engine.Pause` means safety pause (`running=false`, `paused=true`).
- `reset_kill_switch` keeps the engine paused until explicit `resume`.
- `Execution.Place` routes live Coinbase spot only when in non-paper mode and credentials are present; otherwise routes to paper adapter.
- `Portfolio.Positions`, `Portfolio.Balances`, and `Portfolio.Exposure` expose reconciled multi-asset snapshots.
- Scoped risk overrides are supported via `Risk.Override` actions: `kill_global`, `reset_global`, `kill_venue`, `reset_venue`, `kill_strategy`, `reset_strategy`.

## Troubleshooting

- `failed to read /non-agent-workflows/libs/kalshi_client/Cargo.toml`
  - Cause: daemon build context did not include `non-agent-workflows/libs`.
  - Fix: compose now builds from repo root with a focused root `.dockerignore`.

- `lock file version '4' was found, but this version of Cargo does not understand`
  - Cause: old Rust toolchain image.
  - Fix: daemon builder uses `rust:1-slim-bookworm`.

- `openssl-sys ... pkg-config could not be found`
  - Cause: missing build dependencies in Rust image.
  - Fix: daemon builder installs `pkg-config` and `libssl-dev`.

- `image ... openclaw:local already exists` during compose build
  - Cause: two services building the same image tag in parallel.
  - Fix: only `openclaw-gateway` builds `openclaw:local`; `openclaw-cli` reuses it.

- `Missing config. Run openclaw setup ...` from `openclaw-gateway`
  - Cause: OpenClaw config dir is mounted but not initialized.
  - Fix: compose starts gateway with `--allow-unconfigured` by default for local bridge bring-up; run `openclaw setup` when you want full configured gateway behavior.
