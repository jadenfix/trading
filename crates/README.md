# Rust Trading Bridge

Low-latency bridge between OpenClaw tools and a Rust trading daemon over a Unix domain socket (UDS).

## Components

| Path | Purpose |
| --- | --- |
| `trading_protocol` | Shared frame codec + envelope schema (`type`, `id`, `payload`) |
| `trading_daemon` | UDS server (`/var/run/openclaw/trading.sock`) handling control commands |
| `tradingctl` | CLI client for `ping`, `status`, `start`, and `stop` |
| `../.openclaw/extensions/trading-bridge` | OpenClaw extension exposing tools to Clawdbot |

## How It Works

1. OpenClaw tool calls (`trading_status`, `trading_control`) are sent by the extension over UDS.
2. `trading_daemon` decodes framed JSON envelopes and processes control commands.
3. Responses are correlated by envelope `id` and returned to the tool caller.

Protocol details:

- Framing: 4-byte big-endian length prefix + JSON payload
- Envelope fields:
  - `v`: protocol version
  - `id`: request/response correlation UUID
  - `type`: command kind (for example `Control.Status`)
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
```

## OpenClaw Tool Surface

The extension registers:

- `trading_status`: returns bridge connectivity and daemon status payload
- `trading_control`: accepts `command` in `start | stop | status | ping`

Extension path:

- `.openclaw/extensions/trading-bridge/src/index.ts`

Socket path configuration:

- plugin config default: `/var/run/openclaw/trading.sock`
- override via `TRADING_SOCKET_PATH` environment variable

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
