# Operations Runbook

## Start Services

From repo root:

```bash
export OBS_CONTROL_TOKEN="replace-me"   # optional but recommended
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

## Common Control Workflows

### HITL Execute

```bash
bash ./trading-cli sports-agent execute <workflow_id>
```

### Soft Cancel

```bash
bash ./trading-cli sports-agent cancel <workflow_id>
```

### Hard Cancel

```bash
bash ./trading-cli sports-agent cancel <workflow_id> --hard
```

### Stop Managed Service

```bash
bash ./trading-cli sports-agent stop-service
```

## Troubleshooting

1. Dashboard shows `RUNNING` but process is down:
   - Check `runtimeState` card in detail panel.
   - Verify with `bash ./trading-cli status`.
2. Control actions return `UNAUTHENTICATED`:
   - Ensure token in env and dashboard token input match:
     - `OBS_CONTROL_TOKEN`
3. No traces visible:
   - Verify JSONL files under `TRADES/*`.
   - Check `bash ./trading-cli observability logs trace-api`.
4. Workflow action rejected:
   - Inspect workflow `state` and `availableActions` in dashboard/API.

## Stop Services

```bash
bash ./trading-cli sports-agent down
bash ./trading-cli observability down
# or stop all tracked services
bash ./trading-cli down
```
