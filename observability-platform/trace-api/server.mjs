#!/usr/bin/env node
import { createServer } from "node:http";
import { appendFile, mkdir, readFile, readdir, unlink } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..", "..");

const host = process.env.TRACE_API_HOST ?? "127.0.0.1";
const port = Number.parseInt(process.env.TRACE_API_PORT ?? "8791", 10);
const tradesRoot = process.env.TRADES_DIR
  ? path.resolve(process.env.TRADES_DIR)
  : path.join(repoRoot, "TRADES");
const dashboardRoot = path.join(repoRoot, "observability-platform", "dashboard");
const brokerBaseUrl = process.env.BROKER_BASE_URL ?? "http://127.0.0.1:8787";
const brokerStateFile = process.env.BROKER_STATE_FILE
  ? path.resolve(process.env.BROKER_STATE_FILE)
  : path.join(repoRoot, ".trading-cli", "observability", "broker-state.json");
const temporalUiUrl = process.env.TEMPORAL_UI_URL ?? "http://127.0.0.1:8233";
const defaultProject = process.env.OBS_PROJECT ?? "local";
const defaultLocation = process.env.OBS_LOCATION ?? "us-central1";
const controlToken = process.env.OBS_CONTROL_TOKEN ?? "local-dev-token";
const controlAuditFile = process.env.OBS_CONTROL_AUDIT_FILE
  ? path.resolve(process.env.OBS_CONTROL_AUDIT_FILE)
  : path.join(repoRoot, ".trading-cli", "observability", "control-audit.jsonl");
const devStateDir = path.join(repoRoot, ".trading-cli", "dev");

const TRACE_START_KINDS = new Set([
  "strategy_cycle_start",
  "bot_start",
  "recommendation_generated",
  "research_requested",
]);

const TRACE_END_KINDS = new Set([
  "strategy_cycle_summary",
  "bot_shutdown",
  "order_placed",
  "workflow_complete",
  "approval_timeout",
  "workflow_canceled_soft",
  "workflow_canceled_hard",
]);

const TERMINAL_STATUSES = new Set(["executed", "completed", "failed", "canceled_soft", "canceled_hard"]);
const BOT_SERVICE_MAP = {
  "sports-agent": "sports-agent",
  "weather-bot": "weather",
  "arbitrage-bot": "arbitrage",
  "llm-rules-bot": "llm-workflow",
};

const localOperations = new Map();
const localRequestIndex = new Map();

function nowIso() {
  return new Date().toISOString();
}

function safeJsonParse(raw) {
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

function normalizeStatus(value) {
  const status = String(value ?? "").trim().toLowerCase();
  if (!status) {
    return "running";
  }
  if (["failed", "error", "internal_error"].includes(status)) {
    return "failed";
  }
  if (["cancelled_soft", "canceledsoft", "cancel_soft", "cancelled-soft"].includes(status)) {
    return "canceled_soft";
  }
  if (["cancelled_hard", "canceledhard", "cancel_hard", "cancelled-hard"].includes(status)) {
    return "canceled_hard";
  }
  if (
    [
      "awaiting_approval",
      "approved",
      "completed",
      "executed",
      "running",
      "failed",
      "canceled_soft",
      "canceled_hard",
    ].includes(status)
  ) {
    return status;
  }
  return status;
}

function toWorkflowState(status) {
  const normalized = normalizeStatus(status);
  const table = {
    running: "RUNNING",
    awaiting_approval: "AWAITING_APPROVAL",
    approved: "APPROVED",
    executed: "EXECUTED",
    completed: "COMPLETED",
    failed: "FAILED",
    canceled_soft: "CANCELED_SOFT",
    canceled_hard: "CANCELED_HARD",
  };
  return table[normalized] ?? "STATE_UNSPECIFIED";
}

function isTerminalStatus(status) {
  return TERMINAL_STATUSES.has(normalizeStatus(status));
}

function deriveAvailableActions(status, workflowId, cancelState = "none", controlLocked = false) {
  if (!workflowId || controlLocked) {
    return [];
  }
  const normalizedCancelState = String(cancelState ?? "none").trim().toLowerCase();
  if (normalizedCancelState === "hard_requested") {
    return [];
  }
  if (normalizedCancelState === "soft_requested") {
    return ["hardCancel"];
  }
  const normalized = normalizeStatus(status);
  if (normalized === "awaiting_approval") {
    return ["execute", "cancel", "hardCancel"];
  }
  if (normalized === "running" || normalized === "approved") {
    return ["cancel", "hardCancel"];
  }
  return [];
}

function parseTs(raw) {
  if (!raw) {
    return { epoch: 0, iso: "" };
  }
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) {
    return { epoch: 0, iso: "" };
  }
  return { epoch: date.getTime(), iso: date.toISOString() };
}

function inferEventStatus(kind, event) {
  const normalizedKind = String(kind ?? "").toLowerCase();
  const textBlob = JSON.stringify(event).toLowerCase();

  if (normalizedKind.includes("cancel_requested_hard") || normalizedKind.includes("canceled_hard")) {
    return "canceled_hard";
  }
  if (normalizedKind.includes("cancel_requested_soft")) {
    return "running";
  }
  if (normalizedKind.includes("canceled_soft")) {
    return "canceled_soft";
  }
  if (normalizedKind.includes("error") || normalizedKind.includes("failed")) {
    return "failed";
  }
  if (normalizedKind.includes("awaiting_approval")) {
    return "awaiting_approval";
  }
  if (normalizedKind === "execution_approved" || normalizedKind === "execute_requested") {
    return "approved";
  }
  if (normalizedKind === "order_placed") {
    return "executed";
  }
  if (normalizedKind === "workflow_complete") {
    return normalizeStatus(event.status ?? "completed");
  }
  if (textBlob.includes("awaiting_approval")) {
    return "awaiting_approval";
  }
  return "running";
}

function safeEpoch(value) {
  if (!value) {
    return 0;
  }
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function asFiniteNumber(value) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number.parseFloat(value);
    if (Number.isFinite(parsed)) {
      return parsed;
    }
  }
  return null;
}

function asFiniteInt(value) {
  const num = asFiniteNumber(value);
  if (num === null) {
    return null;
  }
  return Math.trunc(num);
}

function executionSummary(record) {
  const side = record.side ? `${record.side.toUpperCase()} ` : "";
  const action = record.action ? `${record.action.toUpperCase()} ` : "";
  const symbol = record.ticker || record.event_ticker || "market";
  const price = record.price_cents !== null ? ` @ ${record.price_cents}¢` : "";
  const size = record.count !== null ? ` x${record.count}` : "";
  return `${record.source_bot}: ${action}${side}${symbol}${price}${size}`.trim();
}

function extractExecutionRecord(kind, payload, metadata = {}) {
  const normalizedKind = String(kind ?? "").toLowerCase();
  const data = payload && typeof payload === "object" ? payload : {};
  let executed = false;

  if (normalizedKind === "order_placed") {
    executed = true;
  } else if (normalizedKind === "execution_result") {
    const result = String(data.result ?? "").toLowerCase();
    const status = String(data.status ?? "").toLowerCase();
    if (
      status === "order_placed" ||
      ["complete_fill", "partial_fill_unwound", "partial_fill_unwind_failed"].includes(result)
    ) {
      executed = true;
    }
  }

  if (!executed) {
    return null;
  }

  const record = {
    execution_id: `${metadata.trace_id ?? "trace"}:${metadata.span_id ?? "span"}`,
    trace_id: metadata.trace_id ?? null,
    workflow_id: data.workflow_id ?? metadata.workflow_id ?? null,
    source_bot: metadata.source_bot ?? "unknown",
    kind: normalizedKind,
    ts: metadata.ts ?? null,
    ticker: data.ticker ?? null,
    event_ticker: data.event_ticker ?? null,
    side: data.side ?? null,
    action: data.action ?? (data.direction ? String(data.direction).replace("_set", "") : null),
    price_cents: asFiniteInt(data.price_cents),
    count: asFiniteInt(data.count ?? data.qty ?? data.size_contracts),
    fee_cents_est: asFiniteInt(data.estimated_fee_cents ?? data.taker_fees ?? data.fee_cents_est),
    status: String(data.status ?? data.result ?? "executed"),
  };

  record.summary = executionSummary(record);
  return record;
}

function combineStatuses(current, next) {
  const priority = {
    failed: 9,
    canceled_hard: 8,
    canceled_soft: 7,
    executed: 6,
    completed: 5,
    approved: 4,
    awaiting_approval: 3,
    running: 2,
  };
  const currentScore = priority[current] ?? 0;
  const nextScore = priority[next] ?? 0;
  return nextScore >= currentScore ? next : current;
}

async function listTradeFiles(rootDir) {
  const files = [];
  let botDirs = [];

  try {
    botDirs = await readdir(rootDir, { withFileTypes: true });
  } catch {
    return files;
  }

  for (const botDir of botDirs) {
    if (!botDir.isDirectory()) {
      continue;
    }
    const botPath = path.join(rootDir, botDir.name);
    let entries = [];
    try {
      entries = await readdir(botPath, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const entry of entries) {
      if (!entry.isFile()) {
        continue;
      }
      if (!entry.name.startsWith("trades-") || !entry.name.endsWith(".jsonl")) {
        continue;
      }
      files.push({
        bot: botDir.name,
        path: path.join(botPath, entry.name),
      });
    }
  }

  return files;
}

async function loadTradeEvents() {
  const files = await listTradeFiles(tradesRoot);
  const events = [];
  let sequence = 0;

  for (const file of files) {
    let content = "";
    try {
      content = await readFile(file.path, "utf8");
    } catch {
      continue;
    }

    const lines = content.split("\n");
    for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
      const line = lines[lineIndex].trim();
      if (!line) {
        continue;
      }
      const parsed = safeJsonParse(line);
      if (!parsed || typeof parsed !== "object") {
        continue;
      }

      const bot = String(parsed.bot ?? file.bot);
      const ts = parseTs(parsed.ts);
      events.push({
        id: `${file.bot}:${path.basename(file.path)}:${lineIndex}`,
        seq: sequence,
        bot,
        file: file.path,
        event: parsed,
        kind: String(parsed.kind ?? "unknown"),
        ts_epoch: ts.epoch,
        ts_iso: ts.iso,
      });
      sequence += 1;
    }
  }

  events.sort((a, b) => {
    if (a.ts_epoch !== b.ts_epoch) {
      return a.ts_epoch - b.ts_epoch;
    }
    return a.seq - b.seq;
  });

  return events;
}

function deriveTraceIds(events) {
  const activeByBot = new Map();
  let syntheticCounter = 0;

  for (const entry of events) {
    const event = entry.event;
    const kind = String(event.kind ?? "");
    const explicit = event.trace_id ?? event.workflow_id;

    if (explicit) {
      entry.trace_id = String(explicit);
      activeByBot.set(entry.bot, entry.trace_id);
      continue;
    }

    if (TRACE_START_KINDS.has(kind) || !activeByBot.has(entry.bot)) {
      syntheticCounter += 1;
      const tsSeed = entry.ts_iso || nowIso();
      const compactTs = tsSeed.replace(/[^0-9TZ]/g, "");
      const syntheticId = `${entry.bot}-${compactTs}-${syntheticCounter}`;
      activeByBot.set(entry.bot, syntheticId);
      entry.trace_id = syntheticId;
    } else {
      entry.trace_id = activeByBot.get(entry.bot);
    }

    if (TRACE_END_KINDS.has(kind)) {
      activeByBot.delete(entry.bot);
    }
  }
}

async function loadBrokerWorkflows() {
  try {
    const raw = await readFile(brokerStateFile, "utf8");
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || !parsed.workflows) {
      return {};
    }
    return parsed.workflows;
  } catch {
    return {};
  }
}

function buildTraceModel(events, workflows) {
  const traces = new Map();

  for (const entry of events) {
    const traceId = String(entry.trace_id);
    const event = entry.event;
    const eventStatus = inferEventStatus(entry.kind, event);

    if (!traces.has(traceId)) {
      traces.set(traceId, {
        trace_id: traceId,
        workflow_id: event.workflow_id ? String(event.workflow_id) : null,
        source_bot: entry.bot,
        mode: event.mode ? String(event.mode) : null,
        ts_start: entry.ts_iso || null,
        ts_end: entry.ts_iso || null,
        status: eventStatus,
        state: toWorkflowState(eventStatus),
        runtime_state: "UNKNOWN",
        requires_approval: false,
        approval: null,
        cancel_state: "none",
        control_locked: false,
        available_actions: [],
        event_count: 0,
        executed_trade_count: 0,
        latest_execution: null,
        latest_execution_ts: null,
        last_command_at: null,
        last_command_by: null,
        executed_trades: [],
        events: [],
      });
    }

    const trace = traces.get(traceId);
    trace.source_bot = trace.source_bot || entry.bot;
    trace.workflow_id = trace.workflow_id || (event.workflow_id ? String(event.workflow_id) : null);
    trace.mode = trace.mode || (event.mode ? String(event.mode) : null);
    if (!trace.ts_start || (entry.ts_iso && entry.ts_iso < trace.ts_start)) {
      trace.ts_start = entry.ts_iso;
    }
    if (!trace.ts_end || (entry.ts_iso && entry.ts_iso > trace.ts_end)) {
      trace.ts_end = entry.ts_iso;
    }
    trace.status = combineStatuses(trace.status, eventStatus);
    trace.event_count += 1;
    trace.events.push({
      trace_id: traceId,
      span_id: entry.id,
      parent_span_id: null,
      kind: entry.kind,
      ts: entry.ts_iso,
      severity: eventStatus === "failed" ? "error" : "info",
      payload: event,
      source_file: entry.file,
    });

    const execution = extractExecutionRecord(entry.kind, event, {
      trace_id: traceId,
      workflow_id: trace.workflow_id,
      source_bot: trace.source_bot,
      ts: entry.ts_iso,
      span_id: entry.id,
    });
    if (execution) {
      trace.executed_trade_count += 1;
      trace.latest_execution = execution;
      trace.latest_execution_ts = execution.ts;
      trace.executed_trades.push(execution);
    }
  }

  for (const workflow of Object.values(workflows)) {
    const traceId = String(workflow.trace_id ?? workflow.workflow_id);
    if (!traceId) {
      continue;
    }

    if (!traces.has(traceId)) {
      traces.set(traceId, {
        trace_id: traceId,
        workflow_id: workflow.workflow_id ? String(workflow.workflow_id) : null,
        source_bot: workflow.source_bot ? String(workflow.source_bot) : "sports-agent",
        mode: workflow.mode ? String(workflow.mode) : null,
        ts_start: workflow.created_at ? String(workflow.created_at) : null,
        ts_end: workflow.updated_at ? String(workflow.updated_at) : null,
        status: normalizeStatus(workflow.status),
        state: toWorkflowState(normalizeStatus(workflow.status)),
        runtime_state: "UNKNOWN",
        requires_approval: Boolean(workflow.requires_approval),
        approval: workflow.approval ?? null,
        cancel_state: String(workflow.cancel_state ?? "none"),
        control_locked: Boolean(workflow.control_locked),
        available_actions: [],
        event_count: 0,
        executed_trade_count: 0,
        latest_execution: null,
        latest_execution_ts: null,
        last_command_at: workflow.last_command_at ?? null,
        last_command_by: workflow.last_command_by ?? null,
        executed_trades: [],
        events: [],
      });
    }

    const trace = traces.get(traceId);
    trace.workflow_id = trace.workflow_id || String(workflow.workflow_id ?? traceId);
    trace.mode = trace.mode || (workflow.mode ? String(workflow.mode) : null);
    trace.status = combineStatuses(trace.status, normalizeStatus(workflow.status));
    trace.requires_approval = Boolean(workflow.requires_approval);
    trace.approval = workflow.approval ?? trace.approval;
    trace.cancel_state = String(workflow.cancel_state ?? trace.cancel_state ?? "none");
    trace.control_locked = Boolean(workflow.control_locked ?? trace.control_locked);
    trace.last_command_at = workflow.last_command_at ?? trace.last_command_at;
    trace.last_command_by = workflow.last_command_by ?? trace.last_command_by;
    trace.ts_start = trace.ts_start ?? (workflow.created_at ? String(workflow.created_at) : null);
    trace.ts_end = trace.ts_end ?? (workflow.updated_at ? String(workflow.updated_at) : null);

    const brokerEvents = Array.isArray(workflow.events) ? workflow.events : [];
    for (let idx = 0; idx < brokerEvents.length; idx += 1) {
      const brokerEvent = brokerEvents[idx];
      const kind = String(brokerEvent.kind ?? "broker_event");
      const eventStatus = inferEventStatus(kind, brokerEvent);
      trace.status = combineStatuses(trace.status, eventStatus);
      trace.events.push({
        trace_id: traceId,
        span_id: `broker:${workflow.workflow_id}:${idx}`,
        parent_span_id: null,
        kind,
        ts: String(brokerEvent.ts ?? workflow.updated_at ?? nowIso()),
        severity: eventStatus === "failed" ? "error" : "info",
        payload: brokerEvent,
        source_file: brokerStateFile,
      });
      trace.event_count += 1;

      const execution = extractExecutionRecord(kind, brokerEvent, {
        trace_id: traceId,
        workflow_id: trace.workflow_id,
        source_bot: trace.source_bot,
        ts: String(brokerEvent.ts ?? workflow.updated_at ?? nowIso()),
        span_id: `broker:${workflow.workflow_id}:${idx}`,
      });
      if (execution) {
        trace.executed_trade_count += 1;
        trace.latest_execution = execution;
        trace.latest_execution_ts = execution.ts;
        trace.executed_trades.push(execution);
      }
    }
  }

  for (const trace of traces.values()) {
    trace.status = normalizeStatus(trace.status);
    trace.state = toWorkflowState(trace.status);
    trace.available_actions = deriveAvailableActions(trace.status, trace.workflow_id, trace.cancel_state, trace.control_locked);
  }

  const statusPriority = {
    executed: 8,
    awaiting_approval: 7,
    approved: 6,
    running: 5,
    completed: 4,
    canceled_soft: 3,
    canceled_hard: 2,
    failed: 1,
  };

  const traceList = [...traces.values()].sort((a, b) => {
    const execDelta = (b.executed_trade_count ?? 0) - (a.executed_trade_count ?? 0);
    if (execDelta !== 0) {
      return execDelta;
    }

    const statusDelta = (statusPriority[b.status] ?? 0) - (statusPriority[a.status] ?? 0);
    if (statusDelta !== 0) {
      return statusDelta;
    }

    const execTsDelta = safeEpoch(b.latest_execution_ts) - safeEpoch(a.latest_execution_ts);
    if (execTsDelta !== 0) {
      return execTsDelta;
    }

    return safeEpoch(b.ts_start) - safeEpoch(a.ts_start);
  });

  return { traces, traceList };
}

function isPidRunning(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function runtimeForService(service) {
  if (!service) {
    return "UNKNOWN";
  }

  const pidfile = path.join(devStateDir, `${service}.pid`);
  let raw;
  try {
    raw = await readFile(pidfile, "utf8");
  } catch {
    return "PROCESS_STOPPED";
  }

  const pid = Number.parseInt(String(raw).trim(), 10);
  if (!Number.isInteger(pid) || pid <= 1) {
    return "PROCESS_STOPPED";
  }

  return isPidRunning(pid) ? "PROCESS_RUNNING" : "PROCESS_STOPPED";
}

async function annotateRuntimeState(traceList) {
  const cache = new Map();

  for (const trace of traceList) {
    const service = BOT_SERVICE_MAP[String(trace.source_bot ?? "")];
    if (!service) {
      trace.runtime_state = "UNKNOWN";
      continue;
    }

    if (!cache.has(service)) {
      cache.set(service, await runtimeForService(service));
    }
    trace.runtime_state = cache.get(service);
  }
}

function workflowResourceName(project, location, workflowId) {
  return `projects/${project}/locations/${location}/workflows/${workflowId}`;
}

function operationResourceName(project, location, operationId) {
  return `projects/${project}/locations/${location}/operations/${operationId}`;
}

function buildWorkflowResource(trace, project, location) {
  const workflowId = String(trace.workflow_id ?? trace.trace_id);
  const status = normalizeStatus(trace.status);

  return {
    name: workflowResourceName(project, location, workflowId),
    uid: trace.trace_id,
    workflowId,
    traceId: trace.trace_id,
    sourceBot: trace.source_bot ?? "unknown",
    mode: trace.mode ?? null,
    state: toWorkflowState(status),
    status,
    runtimeState: trace.runtime_state ?? "UNKNOWN",
    requiresApproval: Boolean(trace.requires_approval),
    approval: trace.approval ?? null,
    cancelState: trace.cancel_state ?? "none",
    controlLocked: Boolean(trace.control_locked),
    availableActions: deriveAvailableActions(status, trace.workflow_id, trace.cancel_state, trace.control_locked),
    createTime: trace.ts_start ?? null,
    updateTime: trace.ts_end ?? null,
    eventCount: trace.event_count ?? 0,
    executedTradeCount: trace.executed_trade_count ?? 0,
    latestExecutionTime: trace.latest_execution_ts ?? null,
    latestExecution: trace.latest_execution ?? null,
    lastCommandAt: trace.last_command_at ?? null,
    lastCommandBy: trace.last_command_by ?? null,
  };
}

function parseFilter(raw) {
  const out = {};
  if (!raw) {
    return out;
  }

  const segments = String(raw)
    .split(/\s+and\s+/i)
    .map((segment) => segment.trim())
    .filter(Boolean);

  for (const segment of segments) {
    const match = segment.match(/^([a-zA-Z0-9_.]+)\s*=\s*"?([^"\s]+)"?$/);
    if (!match) {
      continue;
    }
    out[match[1].toLowerCase()] = match[2];
  }

  return out;
}

function applyWorkflowResourceFilters(resources, query) {
  const parsedFilter = parseFilter(query.get("filter"));
  const botFilter = query.get("bot") ?? query.get("source_bot") ?? parsedFilter.source_bot;
  const stateFilter = query.get("status") ?? query.get("state") ?? parsedFilter.state;
  const modeFilter = query.get("mode") ?? parsedFilter.mode;
  const runtimeFilter = query.get("runtime_state") ?? parsedFilter.runtime_state;

  return resources.filter((resource) => {
    if (botFilter && String(resource.sourceBot) !== String(botFilter)) {
      return false;
    }

    if (stateFilter) {
      const wanted = String(stateFilter).toUpperCase();
      if (resource.state.toUpperCase() !== wanted && resource.status.toUpperCase() !== wanted) {
        return false;
      }
    }

    if (modeFilter && String(resource.mode ?? "") !== String(modeFilter)) {
      return false;
    }

    if (runtimeFilter && String(resource.runtimeState).toUpperCase() !== String(runtimeFilter).toUpperCase()) {
      return false;
    }

    return true;
  });
}

function orderWorkflowResources(resources, orderByRaw) {
  const orderBy = String(orderByRaw ?? "executedTradeCount desc,updateTime desc").trim();
  const clauses = orderBy
    .split(",")
    .map((raw) => raw.trim())
    .filter(Boolean)
    .map((clause) => {
      const [fieldRaw, dirRaw] = clause.split(/\s+/);
      return {
        field: fieldRaw || "updateTime",
        descending: String(dirRaw ?? "desc").toLowerCase() !== "asc",
      };
    });

  return [...resources].sort((left, right) => {
    for (const clause of clauses) {
      let leftValue;
      let rightValue;

      if (clause.field === "createTime") {
        leftValue = safeEpoch(left.createTime);
        rightValue = safeEpoch(right.createTime);
      } else if (clause.field === "updateTime") {
        leftValue = safeEpoch(left.updateTime);
        rightValue = safeEpoch(right.updateTime);
      } else if (clause.field === "state") {
        leftValue = left.state;
        rightValue = right.state;
      } else if (clause.field === "runtimeState") {
        leftValue = left.runtimeState;
        rightValue = right.runtimeState;
      } else {
        leftValue = Number(left.executedTradeCount ?? 0);
        rightValue = Number(right.executedTradeCount ?? 0);
      }

      if (leftValue === rightValue) {
        continue;
      }

      if (clause.descending) {
        return leftValue < rightValue ? 1 : -1;
      }
      return leftValue < rightValue ? -1 : 1;
    }

    return 0;
  });
}

function paginate(items, query, defaultPageSize = 200) {
  const pageSizeRaw = query.get("pageSize") ?? query.get("limit");
  const pageSize = Math.max(1, Math.min(1000, Number.parseInt(pageSizeRaw ?? String(defaultPageSize), 10) || defaultPageSize));
  const tokenRaw = query.get("pageToken") ?? "0";
  const offset = Math.max(0, Number.parseInt(tokenRaw, 10) || 0);

  const slice = items.slice(offset, offset + pageSize);
  const nextPageToken = offset + pageSize < items.length ? String(offset + pageSize) : "";

  return {
    items: slice,
    nextPageToken,
  };
}

function applyTraceFilters(traceList, query) {
  const from = query.get("from");
  const to = query.get("to");
  const bot = query.get("bot");
  const status = query.get("status");
  const limitRaw = query.get("limit");
  const limit = limitRaw ? Number.parseInt(limitRaw, 10) : 200;

  let results = traceList;

  if (from) {
    const fromEpoch = Date.parse(from);
    if (!Number.isNaN(fromEpoch)) {
      results = results.filter((trace) => {
        const ts = trace.ts_start ? Date.parse(trace.ts_start) : 0;
        return ts >= fromEpoch;
      });
    }
  }

  if (to) {
    const toEpoch = Date.parse(to);
    if (!Number.isNaN(toEpoch)) {
      results = results.filter((trace) => {
        const ts = trace.ts_start ? Date.parse(trace.ts_start) : 0;
        return ts <= toEpoch;
      });
    }
  }

  if (bot) {
    results = results.filter((trace) => trace.source_bot === bot);
  }

  if (status) {
    const normalized = normalizeStatus(status);
    results = results.filter((trace) => normalizeStatus(trace.status) === normalized);
  }

  if (Number.isInteger(limit) && limit > 0) {
    results = results.slice(0, limit);
  }

  return results;
}

function googleError(code, status, message, details = []) {
  return {
    error: {
      code,
      status,
      message,
      details,
    },
  };
}

function sendJson(res, statusCode, payload, extraHeaders = {}) {
  const body = JSON.stringify(payload, null, 2);
  res.writeHead(statusCode, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
    "content-length": Buffer.byteLength(body),
    ...extraHeaders,
  });
  res.end(body);
}

function sendGoogleError(res, statusCode, status, message, details = []) {
  return sendJson(res, statusCode, googleError(statusCode, status, message, details));
}

function sendText(res, statusCode, body, contentType = "text/plain; charset=utf-8") {
  res.writeHead(statusCode, {
    "content-type": contentType,
    "content-length": Buffer.byteLength(body),
    "cache-control": "no-store",
  });
  res.end(body);
}

async function readBodyJson(req) {
  const chunks = [];
  for await (const chunk of req) {
    chunks.push(chunk);
  }
  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) {
    return {};
  }
  return JSON.parse(raw);
}

async function ensureAuditDir() {
  await mkdir(path.dirname(controlAuditFile), { recursive: true });
}

let auditInFlight = Promise.resolve();
function appendControlAudit(entry) {
  auditInFlight = auditInFlight
    .catch(() => undefined)
    .then(async () => {
      await ensureAuditDir();
      await appendFile(controlAuditFile, `${JSON.stringify({ ts: nowIso(), ...entry })}\n`, "utf8");
    });
  return auditInFlight;
}

function parseBearerToken(req) {
  const authHeader = String(req.headers.authorization ?? "").trim();
  if (authHeader.toLowerCase().startsWith("bearer ")) {
    return authHeader.slice(7).trim();
  }
  return null;
}

function requireControlAuth(req, res, actorHint = "operator") {
  const bearer = parseBearerToken(req);
  const headerToken = String(req.headers["x-observability-control-token"] ?? "").trim();
  const supplied = bearer || headerToken;

  if (!supplied || supplied !== controlToken) {
    sendGoogleError(res, 401, "UNAUTHENTICATED", "Missing or invalid control token");
    return null;
  }

  const actorHeader = String(req.headers["x-observability-actor"] ?? "").trim();
  const actor = actorHeader || String(actorHint || "operator");
  return actor;
}

async function fetchBroker(pathname, init = {}) {
  try {
    const response = await fetch(`${brokerBaseUrl}${pathname}`, init);
    const payload = await response.json().catch(() => ({
      error: {
        code: 502,
        status: "INTERNAL",
        message: "Invalid broker response",
      },
    }));
    return {
      status: response.status,
      payload,
    };
  } catch (error) {
    return {
      status: 502,
      payload: googleError(
        502,
        "UNAVAILABLE",
        `Broker request failed: ${error instanceof Error ? error.message : String(error)}`,
      ),
    };
  }
}

function createLocalOperation(project, location, action, target, actor, reason, requestId) {
  const operationId = `local-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`;
  const name = operationResourceName(project, location, operationId);
  const now = nowIso();
  const operation = {
    name,
    done: false,
    metadata: {
      action,
      target,
      actor,
      reason: reason || null,
      requestId: requestId || null,
      createTime: now,
      updateTime: now,
    },
  };
  localOperations.set(name, operation);
  return operation;
}

function completeLocalOperation(operation, responsePayload, errorPayload) {
  operation.done = true;
  operation.metadata.updateTime = nowIso();
  if (errorPayload) {
    operation.error = errorPayload;
    delete operation.response;
  } else {
    operation.response = responsePayload;
    delete operation.error;
  }
}

function listLocalOperations(project, location) {
  const prefix = `projects/${project}/locations/${location}/operations/`;
  return [...localOperations.values()]
    .filter((operation) => operation.name.startsWith(prefix))
    .sort((left, right) => safeEpoch(right.metadata?.createTime) - safeEpoch(left.metadata?.createTime));
}

function sleepMs(ms) {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

async function stopManagedService(serviceName) {
  const pidfile = path.join(devStateDir, `${serviceName}.pid`);

  let raw;
  try {
    raw = await readFile(pidfile, "utf8");
  } catch {
    return {
      service: serviceName,
      runtimeState: "PROCESS_STOPPED",
      alreadyStopped: true,
    };
  }

  const pid = Number.parseInt(String(raw).trim(), 10);
  if (!Number.isInteger(pid) || pid <= 1) {
    await unlink(pidfile).catch(() => undefined);
    return {
      service: serviceName,
      runtimeState: "PROCESS_STOPPED",
      alreadyStopped: true,
    };
  }

  if (!isPidRunning(pid)) {
    await unlink(pidfile).catch(() => undefined);
    return {
      service: serviceName,
      runtimeState: "PROCESS_STOPPED",
      alreadyStopped: true,
      pid,
    };
  }

  process.kill(pid, "SIGTERM");
  const deadline = Date.now() + 3000;
  let forced = false;

  while (Date.now() < deadline) {
    if (!isPidRunning(pid)) {
      break;
    }
    await sleepMs(120);
  }

  if (isPidRunning(pid)) {
    process.kill(pid, "SIGKILL");
    forced = true;
    await sleepMs(120);
  }

  if (isPidRunning(pid)) {
    throw new Error(`Failed to stop service ${serviceName}; pid ${pid} is still running`);
  }

  await unlink(pidfile).catch(() => undefined);

  return {
    service: serviceName,
    runtimeState: "PROCESS_STOPPED",
    pid,
    forced,
    alreadyStopped: false,
  };
}

function executionResourceName(project, location, executionId) {
  return `projects/${project}/locations/${location}/executions/${encodeURIComponent(executionId)}`;
}

function buildExecutionResources(traceList, project, location) {
  return traceList
    .flatMap((trace) => (Array.isArray(trace.executed_trades) ? trace.executed_trades : []))
    .map((execution) => ({
      name: executionResourceName(project, location, execution.execution_id),
      executionId: execution.execution_id,
      traceId: execution.trace_id,
      workflowId: execution.workflow_id,
      sourceBot: execution.source_bot,
      kind: execution.kind,
      status: execution.status,
      summary: execution.summary,
      ticker: execution.ticker,
      eventTicker: execution.event_ticker,
      side: execution.side,
      action: execution.action,
      priceCents: execution.price_cents,
      count: execution.count,
      feeCentsEstimate: execution.fee_cents_est,
      executeTime: execution.ts,
    }))
    .sort((a, b) => safeEpoch(b.executeTime) - safeEpoch(a.executeTime));
}

async function serveStatic(pathname, res) {
  const routeMap = {
    "/": "index.html",
    "/index.html": "index.html",
    "/app.js": "app.js",
    "/styles.css": "styles.css",
  };

  const fileName = routeMap[pathname];
  if (!fileName) {
    return false;
  }

  try {
    const fullPath = path.join(dashboardRoot, fileName);
    const raw = await readFile(fullPath);
    const contentType =
      fileName.endsWith(".html")
        ? "text/html; charset=utf-8"
        : fileName.endsWith(".css")
          ? "text/css; charset=utf-8"
          : "application/javascript; charset=utf-8";
    res.writeHead(200, {
      "content-type": contentType,
      "cache-control": "no-store",
      "content-length": raw.length,
    });
    res.end(raw);
    return true;
  } catch {
    sendText(res, 404, "Not found");
    return true;
  }
}

async function handleRequest(req, res) {
  const requestUrl = new URL(req.url ?? "/", `http://${req.headers.host || `${host}:${port}`}`);
  const pathname = requestUrl.pathname;

  if (await serveStatic(pathname, res)) {
    return;
  }

  try {
    if (req.method === "GET" && pathname === "/health") {
      return sendJson(res, 200, {
        ok: true,
        service: "trace-api",
        ts: nowIso(),
        trades_root: tradesRoot,
        broker_state_file: brokerStateFile,
        control_token_configured: Boolean(controlToken),
      });
    }

    if (req.method === "GET" && pathname === "/api/config") {
      return sendJson(res, 200, {
        broker_base_url: brokerBaseUrl,
        temporal_ui_url: temporalUiUrl,
        trace_api_url: `http://${host}:${port}`,
        api_version: "v1",
        default_project: defaultProject,
        default_location: defaultLocation,
        control_token_required: true,
        control_token_default: controlToken === "local-dev-token" ? "local-dev-token" : null,
      });
    }

    const workflowActionMatch = pathname.match(
      /^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/workflows\/([^/:]+):(execute|cancel|hardCancel)$/,
    );
    if (req.method === "POST" && workflowActionMatch) {
      let body;
      try {
        body = await readBodyJson(req);
      } catch {
        return sendGoogleError(res, 400, "INVALID_ARGUMENT", "Invalid JSON payload");
      }

      const actor = requireControlAuth(req, res, body.actor ?? "operator");
      if (!actor) {
        return;
      }

      const project = decodeURIComponent(workflowActionMatch[1]);
      const location = decodeURIComponent(workflowActionMatch[2]);
      const workflowId = decodeURIComponent(workflowActionMatch[3]);
      const action = workflowActionMatch[4];
      const requestId = String(body.requestId ?? body.request_id ?? `${Date.now()}-${Math.random()}`);
      const reason = String(body.reason ?? "").trim();

      const brokerPath = `/v1/projects/${encodeURIComponent(project)}/locations/${encodeURIComponent(location)}/workflows/${encodeURIComponent(workflowId)}:${action}`;
      const { status, payload } = await fetchBroker(brokerPath, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-observability-actor": actor,
        },
        body: JSON.stringify({
          requestId,
          actor,
          reason,
        }),
      });

      await appendControlAudit({
        actor,
        action,
        target: workflowResourceName(project, location, workflowId),
        request_id: requestId,
        reason: reason || null,
        upstream_status: status,
      });

      return sendJson(res, status, payload);
    }

    const serviceStopMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/services\/([^/:]+):stop$/);
    if (req.method === "POST" && serviceStopMatch) {
      let body;
      try {
        body = await readBodyJson(req);
      } catch {
        return sendGoogleError(res, 400, "INVALID_ARGUMENT", "Invalid JSON payload");
      }

      const actor = requireControlAuth(req, res, body.actor ?? "operator");
      if (!actor) {
        return;
      }

      const project = decodeURIComponent(serviceStopMatch[1]);
      const location = decodeURIComponent(serviceStopMatch[2]);
      const serviceName = decodeURIComponent(serviceStopMatch[3]);
      const requestId = String(body.requestId ?? body.request_id ?? `${Date.now()}-${Math.random()}`);
      const reason = String(body.reason ?? "").trim();

      if (serviceName !== "sports-agent") {
        return sendGoogleError(res, 400, "INVALID_ARGUMENT", "Only services/sports-agent:stop is supported in v1");
      }

      const requestKey = `${project}|${location}|service-stop|${serviceName}|${requestId}`;
      const existingName = localRequestIndex.get(requestKey);
      if (existingName && localOperations.has(existingName)) {
        return sendJson(res, 200, localOperations.get(existingName));
      }

      const target = `projects/${project}/locations/${location}/services/${serviceName}`;
      const operation = createLocalOperation(project, location, "stopService", target, actor, reason, requestId);

      try {
        const result = await stopManagedService(serviceName);
        completeLocalOperation(operation, {
          serviceName,
          runtimeState: result.runtimeState,
          alreadyStopped: Boolean(result.alreadyStopped),
          forced: Boolean(result.forced),
          pid: result.pid ?? null,
        });
      } catch (error) {
        completeLocalOperation(operation, null, {
          code: 13,
          status: "INTERNAL",
          message: error instanceof Error ? error.message : String(error),
        });
      }

      localRequestIndex.set(requestKey, operation.name);

      await appendControlAudit({
        actor,
        action: "stopService",
        target,
        request_id: requestId,
        reason: reason || null,
        status: operation.error ? "INTERNAL" : "OK",
      });

      return sendJson(res, 200, operation);
    }

    const operationGetMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/operations\/([^/]+)$/);
    if (req.method === "GET" && operationGetMatch) {
      const project = decodeURIComponent(operationGetMatch[1]);
      const location = decodeURIComponent(operationGetMatch[2]);
      const operationId = decodeURIComponent(operationGetMatch[3]);
      const operationName = operationResourceName(project, location, operationId);

      if (localOperations.has(operationName)) {
        return sendJson(res, 200, localOperations.get(operationName));
      }

      const brokerPath = `/v1/projects/${encodeURIComponent(project)}/locations/${encodeURIComponent(location)}/operations/${encodeURIComponent(operationId)}`;
      const { status, payload } = await fetchBroker(brokerPath, {
        method: "GET",
      });
      return sendJson(res, status, payload);
    }

    const operationListMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/operations$/);
    if (req.method === "GET" && operationListMatch) {
      const project = decodeURIComponent(operationListMatch[1]);
      const location = decodeURIComponent(operationListMatch[2]);

      const local = listLocalOperations(project, location);
      const brokerPath = `/v1/projects/${encodeURIComponent(project)}/locations/${encodeURIComponent(location)}/operations`;
      const brokerResult = await fetchBroker(brokerPath, { method: "GET" });
      const brokerOps = Array.isArray(brokerResult.payload.operations) ? brokerResult.payload.operations : [];

      const combined = [...local, ...brokerOps].sort(
        (left, right) => safeEpoch(right.metadata?.createTime) - safeEpoch(left.metadata?.createTime),
      );
      const page = paginate(combined, requestUrl.searchParams, 200);
      return sendJson(res, 200, {
        operations: page.items,
        nextPageToken: page.nextPageToken,
      });
    }

    const events = await loadTradeEvents();
    deriveTraceIds(events);
    const workflows = await loadBrokerWorkflows();
    const { traces, traceList } = buildTraceModel(events, workflows);
    await annotateRuntimeState(traceList);

    const workflowIndex = new Map();
    for (const trace of traceList) {
      const workflowId = String(trace.workflow_id ?? "").trim();
      if (workflowId) {
        workflowIndex.set(workflowId, trace);
      }
      workflowIndex.set(String(trace.trace_id), trace);
    }

    if (req.method === "GET" && pathname === "/api/traces") {
      const filtered = applyTraceFilters(traceList, requestUrl.searchParams);
      return sendJson(
        res,
        200,
        {
          traces: filtered.map((trace) => ({
            trace_id: trace.trace_id,
            workflow_id: trace.workflow_id,
            source_bot: trace.source_bot,
            mode: trace.mode,
            ts_start: trace.ts_start,
            ts_end: trace.ts_end,
            status: trace.status,
            state: trace.state,
            runtime_state: trace.runtime_state,
            requires_approval: trace.requires_approval,
            approval: trace.approval,
            cancel_state: trace.cancel_state,
            control_locked: trace.control_locked,
            available_actions: trace.available_actions,
            event_count: trace.event_count,
            executed_trade_count: trace.executed_trade_count,
            latest_execution_ts: trace.latest_execution_ts,
            latest_execution: trace.latest_execution,
            last_command_at: trace.last_command_at,
            last_command_by: trace.last_command_by,
          })),
        },
        {
          "x-observability-deprecated": "Use /v1/projects/*/locations/*/workflows",
        },
      );
    }

    if (req.method === "GET" && pathname === "/api/executions") {
      const limitRaw = requestUrl.searchParams.get("limit");
      const limit = limitRaw ? Number.parseInt(limitRaw, 10) : 50;
      const botFilter = requestUrl.searchParams.get("bot");
      const items = traceList
        .flatMap((trace) => (Array.isArray(trace.executed_trades) ? trace.executed_trades : []))
        .filter((item) => (botFilter ? item.source_bot === botFilter : true))
        .sort((a, b) => safeEpoch(b.ts) - safeEpoch(a.ts));

      return sendJson(
        res,
        200,
        {
          executions: Number.isInteger(limit) && limit > 0 ? items.slice(0, limit) : items,
        },
        {
          "x-observability-deprecated": "Use /v1/projects/*/locations/*/executions",
        },
      );
    }

    const traceMatch = pathname.match(/^\/api\/traces\/([^/]+)$/);
    if (req.method === "GET" && traceMatch) {
      const traceId = decodeURIComponent(traceMatch[1]);
      const trace = traces.get(traceId);
      if (!trace) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Trace not found: ${traceId}`,
        });
      }
      return sendJson(
        res,
        200,
        trace,
        {
          "x-observability-deprecated": "Use /v1/projects/*/locations/*/workflows/*",
        },
      );
    }

    const traceEventsMatch = pathname.match(/^\/api\/traces\/([^/]+)\/events$/);
    if (req.method === "GET" && traceEventsMatch) {
      const traceId = decodeURIComponent(traceEventsMatch[1]);
      const trace = traces.get(traceId);
      if (!trace) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Trace not found: ${traceId}`,
        });
      }
      return sendJson(
        res,
        200,
        {
          trace_id: traceId,
          events: trace.events.sort((a, b) => safeEpoch(a.ts) - safeEpoch(b.ts)),
        },
        {
          "x-observability-deprecated": "Use /v1/projects/*/locations/*/workflows/*/events",
        },
      );
    }

    const approveMatch = pathname.match(/^\/api\/traces\/([^/]+)\/approve$/);
    if (req.method === "POST" && approveMatch) {
      const traceId = decodeURIComponent(approveMatch[1]);
      const body = await readBodyJson(req).catch(() => ({}));
      const response = await fetchBroker(`/execution/${encodeURIComponent(traceId)}/approve`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          approved_by: body.approved_by ?? "operator",
          command_context: body.command_context ?? "trace-api-legacy",
        }),
      });
      return sendJson(
        res,
        response.status,
        response.payload,
        {
          "x-observability-deprecated": "Use /v1/projects/*/locations/*/workflows/*:execute",
        },
      );
    }

    const v1WorkflowListMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/workflows$/);
    if (req.method === "GET" && v1WorkflowListMatch) {
      const project = decodeURIComponent(v1WorkflowListMatch[1]);
      const location = decodeURIComponent(v1WorkflowListMatch[2]);

      let resources = traceList.map((trace) => buildWorkflowResource(trace, project, location));
      resources = applyWorkflowResourceFilters(resources, requestUrl.searchParams);
      resources = orderWorkflowResources(resources, requestUrl.searchParams.get("orderBy"));
      const page = paginate(resources, requestUrl.searchParams, 200);

      return sendJson(res, 200, {
        workflows: page.items,
        nextPageToken: page.nextPageToken,
      });
    }

    const v1WorkflowGetMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/workflows\/([^/:]+)$/);
    if (req.method === "GET" && v1WorkflowGetMatch) {
      const project = decodeURIComponent(v1WorkflowGetMatch[1]);
      const location = decodeURIComponent(v1WorkflowGetMatch[2]);
      const workflowId = decodeURIComponent(v1WorkflowGetMatch[3]);
      const trace = workflowIndex.get(workflowId);

      if (!trace) {
        return sendGoogleError(res, 404, "NOT_FOUND", `Workflow not found: ${workflowId}`);
      }

      return sendJson(res, 200, buildWorkflowResource(trace, project, location));
    }

    const v1WorkflowEventsMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/workflows\/([^/:]+)\/events$/);
    if (req.method === "GET" && v1WorkflowEventsMatch) {
      const workflowId = decodeURIComponent(v1WorkflowEventsMatch[3]);
      const trace = workflowIndex.get(workflowId);

      if (!trace) {
        return sendGoogleError(res, 404, "NOT_FOUND", `Workflow not found: ${workflowId}`);
      }

      const sortedEvents = [...(trace.events ?? [])].sort((a, b) => safeEpoch(a.ts) - safeEpoch(b.ts));
      const page = paginate(sortedEvents, requestUrl.searchParams, 500);
      return sendJson(res, 200, {
        events: page.items,
        nextPageToken: page.nextPageToken,
      });
    }

    const v1ExecutionsMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/executions$/);
    if (req.method === "GET" && v1ExecutionsMatch) {
      const project = decodeURIComponent(v1ExecutionsMatch[1]);
      const location = decodeURIComponent(v1ExecutionsMatch[2]);

      let resources = buildExecutionResources(traceList, project, location);
      const filter = parseFilter(requestUrl.searchParams.get("filter"));
      const botFilter = requestUrl.searchParams.get("bot") ?? filter.source_bot;
      if (botFilter) {
        resources = resources.filter((item) => item.sourceBot === botFilter);
      }

      const page = paginate(resources, requestUrl.searchParams, 50);
      return sendJson(res, 200, {
        executions: page.items,
        nextPageToken: page.nextPageToken,
      });
    }

    return sendJson(res, 404, {
      error: "not_found",
      message: `No route for ${req.method} ${pathname}`,
    });
  } catch (error) {
    if (error instanceof SyntaxError) {
      return sendGoogleError(res, 400, "INVALID_ARGUMENT", "Invalid JSON payload");
    }
    return sendJson(res, 500, {
      error: "internal_error",
      message: error instanceof Error ? error.message : String(error),
    });
  }
}

const server = createServer((req, res) => {
  void handleRequest(req, res);
});

server.listen(port, host, () => {
  console.log(
    JSON.stringify(
      {
        ts: nowIso(),
        service: "trace-api",
        status: "listening",
        bind: `${host}:${port}`,
        trades_root: tradesRoot,
        defaults: {
          project: defaultProject,
          location: defaultLocation,
        },
      },
      null,
      2,
    ),
  );
});
