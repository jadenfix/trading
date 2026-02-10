#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ARB_DIR="$ROOT_DIR/non-agent-workflows/arbitrage-bot"
WEATHER_DIR="$ROOT_DIR/non-agent-workflows/weather-bot"
TRADES_DIR="${TRADES_DIR:-$ROOT_DIR/TRADES}"
MODE="${1:-prod}"

usage() {
  cat <<'EOF'
Usage:
  ./run-bots.sh [prod|dry-run|check-auth]

Modes:
  prod       Run both bots live in parallel (default, USE_DEMO=0)
  dry-run    Run both bots with --dry-run in parallel (USE_DEMO defaults to 0)
  check-auth Run both bots with --check-auth in parallel (USE_DEMO defaults to 0)

Optional environment variables:
  USE_DEMO=0|1   Override environment target (default: 0)
  TRADES_DIR=... Root trades folder (default: <repo>/TRADES)
  ALLOW_MULTI=1  Allow launching even if bot process appears to already be running
EOF
}

if [[ "$MODE" != "prod" && "$MODE" != "dry-run" && "$MODE" != "check-auth" ]]; then
  usage
  exit 1
fi

if [[ ! -d "$ARB_DIR" ]]; then
  echo "Missing directory: $ARB_DIR" >&2
  exit 1
fi

if [[ ! -d "$WEATHER_DIR" ]]; then
  echo "Missing directory: $WEATHER_DIR" >&2
  exit 1
fi

mkdir -p "$TRADES_DIR"
export TRADES_DIR
export USE_DEMO="${USE_DEMO:-0}"

if [[ "${ALLOW_MULTI:-0}" != "1" ]]; then
  if pgrep -f 'target/release/arbitrage-bot' >/dev/null 2>&1; then
    echo "arbitrage-bot appears to already be running. Stop it first or set ALLOW_MULTI=1." >&2
    exit 1
  fi
  if pgrep -f 'target/release/weather-bot' >/dev/null 2>&1; then
    echo "weather-bot appears to already be running. Stop it first or set ALLOW_MULTI=1." >&2
    exit 1
  fi
fi

case "$MODE" in
  prod)
    ARB_FLAG=""
    WEATHER_FLAG=""
    ;;
  dry-run)
    ARB_FLAG="--dry-run"
    WEATHER_FLAG="--dry-run"
    ;;
  check-auth)
    ARB_FLAG="--check-auth"
    WEATHER_FLAG="--check-auth"
    ;;
esac

echo "Starting bots in parallel"
echo "  MODE=$MODE"
echo "  USE_DEMO=$USE_DEMO"
echo "  TRADES_DIR=$TRADES_DIR"

cleanup() {
  local code=$?
  if [[ -n "${ARB_PID:-}" ]] && kill -0 "$ARB_PID" 2>/dev/null; then
    kill "$ARB_PID" 2>/dev/null || true
  fi
  if [[ -n "${WEATHER_PID:-}" ]] && kill -0 "$WEATHER_PID" 2>/dev/null; then
    kill "$WEATHER_PID" 2>/dev/null || true
  fi
  wait 2>/dev/null || true
  exit "$code"
}
trap cleanup INT TERM EXIT

(
  cd "$ARB_DIR"
  if [[ -n "$ARB_FLAG" ]]; then
    cargo run --release -- "$ARB_FLAG"
  else
    cargo run --release
  fi
) &
ARB_PID=$!

(
  cd "$WEATHER_DIR"
  if [[ -n "$WEATHER_FLAG" ]]; then
    cargo run --release -- "$WEATHER_FLAG"
  else
    cargo run --release
  fi
) &
WEATHER_PID=$!

echo "arbitrage-bot PID: $ARB_PID"
echo "weather-bot   PID: $WEATHER_PID"

wait "$ARB_PID" "$WEATHER_PID"
