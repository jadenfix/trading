# Sports Agent Worker

Rust worker that continuously researches Kalshi sports markets and emits one high-conviction recommendation per cycle.

## Modes

- `hitl`: research + recommendation, pause for approval, then execute only after operator approval.
- `auto_ultra_strict`: research + auto-execute only if ultra-strict gates pass.

## Quant Method (No ML)

The worker uses a deterministic Bayesian pipeline:

1. Convert The Odds API prices to implied probabilities.
2. Build team strength signal from recent SportsDataIO completed games.
3. Blend signals into a single probability estimate.
4. Apply Beta posterior update with synthetic sample weighting.
5. Compute credible lower bound (high-confidence tail).
6. Evaluate net EV including Kalshi taker fees + slippage buffer.
7. Apply CLV proxy and liquidity/spread gates.

## Required Env Vars

- `KALSHI_API_KEY`
- `KALSHI_SECRET_KEY`
- `THE_ODDS_API_KEY`

Recommended:

- `SPORTS_DATA_IO_API_KEY`
- `KALSHI_USE_DEMO=1` for sandboxing

## Run

From repo root:

```bash
# HITL mode (default)
bash ./trading-cli sports-agent go --mode hitl

# Ultra strict auto mode
bash ./trading-cli sports-agent go --mode auto_ultra_strict

# Background
bash ./trading-cli sports-agent up --mode hitl
bash ./trading-cli sports-agent logs
```

Approve a pending HITL workflow:

```bash
bash ./trading-cli sports-agent approve <workflow_id_or_trace_id>
```

## Output

- JSONL events: `TRADES/sports-agent/trades-YYYY-MM-DD.jsonl`
- Trace view: `http://127.0.0.1:8791`

## Safety Defaults

- `auto_ultra_strict` requires very high posterior lower bound (`>= 70%` by default), net edge, CLV proxy, low spread, and low signal disagreement.
- All EV checks include estimated taker fees and slippage buffer.
