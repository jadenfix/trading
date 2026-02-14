# Operations Runbook

## Start Services

From repo root:

```bash
bash ./trading-cli observability up
bash ./trading-cli sports-agent up --mode hitl
bash ./trading-cli status
```

## Inspect Services

```bash
bash ./trading-cli observability status
bash ./trading-cli observability logs trace-api
bash ./trading-cli observability logs temporal-broker
bash ./trading-cli sports-agent logs
```

## Common Workflows

### HITL Research + Execute

```bash
bash ./trading-cli sports-agent up --mode hitl
bash ./trading-cli observability ui
bash ./trading-cli sports-agent approve <trace_id>
```

### Ultra-Strict Auto Mode

```bash
bash ./trading-cli sports-agent up --mode auto_ultra_strict
```

## Troubleshooting

1. `trace-api` shows no traces:
   - verify `TRADES/` has JSONL files
   - check `bash ./trading-cli observability logs trace-api`
2. approvals do nothing:
   - verify broker is running
   - check `http://127.0.0.1:8787/health`
   - check workflow exists in `.trading-cli/observability/broker-state.json`
3. sports worker not trading:
   - confirm `THE_ODDS_API_KEY` is set
   - confirm Kalshi auth env vars are valid
   - inspect `TRADES/sports-agent/*.jsonl` for gate rejection reasons
4. Temporal UI unavailable:
   - run `bash ./trading-cli temporal status`

## Stop Services

```bash
bash ./trading-cli sports-agent down
bash ./trading-cli observability down
# or stop all tracked services
bash ./trading-cli down
```
