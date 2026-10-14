# Operations Runbook

## Start Services

From repo root:

```bash
export OBS_CONTROL_TOKEN="replace-me"   # required for authenticated dashboard controls
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

## Broker storage and recovery

The local broker writes a private temporary snapshot, syncs it, atomically
replaces `BROKER_STATE_FILE`, and syncs its directory before acknowledging a
state change. State and `BROKER_AUDIT_FILE` must have distinct resolved paths.
The storage filesystem must support these operations. Existing corrupt,
unsupported or unreadable state fails startup; it is never silently replaced
with an empty workflow set.

An audit or snapshot write failure returns `503 UNAVAILABLE` and latches the
broker unavailable, including reads and health checks. Restore storage, retain
the files for diagnosis, then restart. Reconcile the workflow and operation
using the original `requestId` before retrying a control: a failure after a
rename can leave a committed snapshot even though no success response arrived.
Do not delete state or use a new request ID to bypass uncertain outcomes.

The audit JSONL records `phase: attempt` before the state commit. It is not an
atomic two-file transaction journal. A recorded attempt can therefore coexist
with an unchanged snapshot after a failed commit. HTTP fault tests and process
restart recovery are verified; physical power-loss recovery is not measured.

Complete requests are serialized at the state boundary. POST bodies are read
before that queue, with a default five-second deadline (`BROKER_BODY_TIMEOUT_MS`,
bounded to 100–30,000 ms) and the existing byte limit. An incomplete body returns
408 without blocking unrelated health checks. This local broker is not a
multi-tenant production scheduler or a live-exchange execution proof.
