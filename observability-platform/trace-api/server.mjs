#!/usr/bin/env node
import { createServer } from "node:http";
import { readFile, readdir } from "node:fs/promises";
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
]);

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
  if (["awaiting_approval", "approved", "completed", "executed", "running"].includes(status)) {
    return status;
  }
  return status;
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

  if (normalizedKind.includes("error") || normalizedKind.includes("failed")) {
    return "failed";
  }
  if (normalizedKind.includes("awaiting_approval")) {
    return "awaiting_approval";
  }
  if (normalizedKind === "execution_approved") {
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
    failed: 6,
    awaiting_approval: 5,
    approved: 4,
    executed: 3,
    completed: 2,
    running: 1,
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
        console.warn(
          `[trace-api] Skipping malformed JSONL in ${file.path} at line ${lineIndex + 1}`
        );
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
        requires_approval: false,
        approval: null,
        event_count: 0,
        executed_trade_count: 0,
        latest_execution: null,
        latest_execution_ts: null,
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
        requires_approval: Boolean(workflow.requires_approval),
        approval: workflow.approval ?? null,
        event_count: 0,
        executed_trade_count: 0,
        latest_execution: null,
        latest_execution_ts: null,
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
    trace.ts_start = trace.ts_start ?? (workflow.created_at ? String(workflow.created_at) : null);
    trace.ts_end = trace.ts_end ?? (workflow.updated_at ? String(workflow.updated_at) : null);

    const brokerEvents = Array.isArray(workflow.events) ? workflow.events : [];
    for (let idx = 0; idx < brokerEvents.length; idx += 1) {
      const brokerEvent = brokerEvents[idx];
      trace.events.push({
        trace_id: traceId,
        span_id: `broker:${workflow.workflow_id}:${idx}`,
        parent_span_id: null,
        kind: String(brokerEvent.kind ?? "broker_event"),
        ts: String(brokerEvent.ts ?? workflow.updated_at ?? nowIso()),
        severity: "info",
        payload: brokerEvent,
        source_file: brokerStateFile,
      });
      trace.event_count += 1;

      const execution = extractExecutionRecord(String(brokerEvent.kind ?? "broker_event"), brokerEvent, {
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

  const statusPriority = {
    executed: 6,
    awaiting_approval: 5,
    approved: 4,
    running: 3,
    completed: 2,
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
    results = results.filter((trace) => trace.status === status);
  }

  if (Number.isInteger(limit) && limit > 0) {
    results = results.slice(0, limit);
  }

  return results;
}

function sendJson(res, statusCode, payload) {
  const body = JSON.stringify(payload, null, 2);
  res.writeHead(statusCode, {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
    "content-length": Buffer.byteLength(body),
  });
  res.end(body);
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
  let totalLength = 0;
  for await (const chunk of req) {
    totalLength += chunk.length;
    if (totalLength > 1024 * 1024) {
      // 1MB limit
      throw new Error("Request body too large");
    }
    chunks.push(chunk);
  }
  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) {
    return {};
  }
  return JSON.parse(raw);
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
      });
    }

    if (req.method === "GET" && pathname === "/api/config") {
      return sendJson(res, 200, {
        broker_base_url: brokerBaseUrl,
        temporal_ui_url: temporalUiUrl,
        trace_api_url: `http://${host}:${port}`,
      });
    }

    const events = await loadTradeEvents();
    deriveTraceIds(events);
    const workflows = await loadBrokerWorkflows();
    const { traces, traceList } = buildTraceModel(events, workflows);

    if (req.method === "GET" && pathname === "/api/traces") {
      const filtered = applyTraceFilters(traceList, requestUrl.searchParams);
      return sendJson(res, 200, {
        traces: filtered.map((trace) => ({
          trace_id: trace.trace_id,
          workflow_id: trace.workflow_id,
          source_bot: trace.source_bot,
          mode: trace.mode,
          ts_start: trace.ts_start,
          ts_end: trace.ts_end,
          status: trace.status,
          requires_approval: trace.requires_approval,
          approval: trace.approval,
          event_count: trace.event_count,
          executed_trade_count: trace.executed_trade_count,
          latest_execution_ts: trace.latest_execution_ts,
          latest_execution: trace.latest_execution,
        })),
      });
    }

    if (req.method === "GET" && pathname === "/api/executions") {
      const limitRaw = requestUrl.searchParams.get("limit");
      const limit = limitRaw ? Number.parseInt(limitRaw, 10) : 50;
      const botFilter = requestUrl.searchParams.get("bot");
      const items = traceList
        .flatMap((trace) => (Array.isArray(trace.executed_trades) ? trace.executed_trades : []))
        .filter((item) => (botFilter ? item.source_bot === botFilter : true))
        .sort((a, b) => safeEpoch(b.ts) - safeEpoch(a.ts));

      return sendJson(res, 200, {
        executions: Number.isInteger(limit) && limit > 0 ? items.slice(0, limit) : items,
      });
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
      return sendJson(res, 200, trace);
    }

    const eventsMatch = pathname.match(/^\/api\/traces\/([^/]+)\/events$/);
    if (req.method === "GET" && eventsMatch) {
      const traceId = decodeURIComponent(eventsMatch[1]);
      const trace = traces.get(traceId);
      if (!trace) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Trace not found: ${traceId}`,
        });
      }
      return sendJson(res, 200, {
        trace_id: traceId,
        events: trace.events.sort((a, b) => {
          const left = a.ts ? Date.parse(a.ts) : 0;
          const right = b.ts ? Date.parse(b.ts) : 0;
          return (Number.isNaN(left) ? 0 : left) - (Number.isNaN(right) ? 0 : right);
        }),
      });
    }

    const approveMatch = pathname.match(/^\/api\/traces\/([^/]+)\/approve$/);
    if (req.method === "POST" && approveMatch) {
      const traceId = decodeURIComponent(approveMatch[1]);
      const body = await readBodyJson(req);

      let approvedBy =
        typeof body?.approved_by === "string" ? body.approved_by.trim() : "";
      if (!approvedBy) {
        approvedBy = "operator";
      } else if (approvedBy.length > 256) {
        approvedBy = approvedBy.slice(0, 256);
      }

      let commandContext =
        typeof body?.command_context === "string" ? body.command_context.trim() : "";
      if (!commandContext) {
        commandContext = "trace-api";
      } else if (commandContext.length > 256) {
        commandContext = commandContext.slice(0, 256);
      }

      const response = await fetch(`${brokerBaseUrl}/execution/${encodeURIComponent(traceId)}/approve`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
        },
        body: JSON.stringify({
          approved_by: approvedBy,
          command_context: commandContext,
        }),
      });

      const payload = await response.json().catch(() => ({
        error: "broker_error",
        message: "Invalid broker response",
      }));
      return sendJson(res, response.status, payload);
    }

    return sendJson(res, 404, {
      error: "not_found",
      message: `No route for ${req.method} ${pathname}`,
    });
  } catch (error) {
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
      },
      null,
      2,
    ),
  );
});
