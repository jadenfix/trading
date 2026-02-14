#!/usr/bin/env node
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..", "..");

const port = Number.parseInt(process.env.BROKER_PORT ?? "8787", 10);
const host = process.env.BROKER_HOST ?? "127.0.0.1";
const stateFile = process.env.BROKER_STATE_FILE
  ? path.resolve(process.env.BROKER_STATE_FILE)
  : path.join(repoRoot, ".trading-cli", "observability", "broker-state.json");

const state = {
  version: 1,
  workflows: {},
};

function nowIso() {
  return new Date().toISOString();
}

async function ensureStateDir() {
  await mkdir(path.dirname(stateFile), { recursive: true });
}

async function loadState() {
  try {
    await ensureStateDir();
    const raw = await readFile(stateFile, "utf8");
    const decoded = JSON.parse(raw);
    if (decoded && typeof decoded === "object" && decoded.workflows) {
      state.version = Number.isInteger(decoded.version) ? decoded.version : 1;
      state.workflows = decoded.workflows;
    }
  } catch {
    // Start from empty state if file is missing or invalid.
  }
}

let writeInFlight = Promise.resolve();
function persistState() {
  writeInFlight = writeInFlight
    .catch(() => undefined)
    .then(async () => {
      await ensureStateDir();
      await writeFile(stateFile, JSON.stringify(state, null, 2), "utf8");
    })
    .catch((err) => {
      console.error(`[broker] Failed to persist state to ${stateFile}:`, err);
    });
  return writeInFlight;
}

async function readJsonBody(req) {
  const chunks = [];
  for await (const chunk of req) {
    chunks.push(chunk);
  }
  if (chunks.length === 0) {
    return {};
  }
  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) {
    return {};
  }
  return JSON.parse(raw);
}

function sendJson(res, statusCode, payload) {
  const body = JSON.stringify(payload, null, 2);
  res.writeHead(statusCode, {
    "content-type": "application/json; charset=utf-8",
    "content-length": Buffer.byteLength(body),
    "cache-control": "no-store",
  });
  res.end(body);
}

function getWorkflow(workflowId) {
  return state.workflows[workflowId] ?? null;
}

function upsertWorkflow(payload = {}) {
  const workflowId =
    String(payload.workflow_id || payload.id || payload.trace_id || "").trim() ||
    `wf-${Date.now()}-${randomUUID().slice(0, 8)}`;
  const existing = getWorkflow(workflowId);
  const createdAt = existing?.created_at ?? nowIso();
  const status = payload.status
    ? String(payload.status)
    : payload.requires_approval
      ? "awaiting_approval"
      : existing?.status ?? "running";

  const merged = {
    workflow_id: workflowId,
    trace_id: String(payload.trace_id ?? existing?.trace_id ?? workflowId),
    mode: String(payload.mode ?? existing?.mode ?? "unknown"),
    source_bot: String(payload.source_bot ?? existing?.source_bot ?? "unknown"),
    requires_approval: Boolean(payload.requires_approval ?? existing?.requires_approval ?? false),
    status,
    created_at: createdAt,
    updated_at: nowIso(),
    recommendation: payload.recommendation ?? existing?.recommendation ?? null,
    result: payload.result ?? existing?.result ?? null,
    approval: payload.approval ?? existing?.approval ?? null,
    events: Array.isArray(existing?.events) ? existing.events : [],
    input: payload.input ?? existing?.input ?? null,
  };

  state.workflows[workflowId] = merged;
  return merged;
}

function appendWorkflowEvent(workflowId, eventPayload = {}) {
  const workflow = getWorkflow(workflowId);
  if (!workflow) {
    return null;
  }
  if (!Array.isArray(workflow.events)) {
    workflow.events = [];
  }
  workflow.events.push({
    ts: nowIso(),
    ...eventPayload,
  });
  workflow.updated_at = nowIso();
  return workflow;
}

function statusSummary() {
  const counts = {
    running: 0,
    awaiting_approval: 0,
    approved: 0,
    completed: 0,
    failed: 0,
    other: 0,
  };

  for (const wf of Object.values(state.workflows)) {
    const status = String(wf.status || "other");
    if (Object.prototype.hasOwnProperty.call(counts, status)) {
      counts[status] += 1;
    } else {
      counts.other += 1;
    }
  }
  return counts;
}

async function handleRequest(req, res) {
  const requestUrl = new URL(req.url ?? "/", `http://${req.headers.host || `${host}:${port}`}`);
  const pathname = requestUrl.pathname;

  try {
    if (req.method === "GET" && pathname === "/health") {
      return sendJson(res, 200, {
        ok: true,
        service: "temporal-broker",
        ts: nowIso(),
        state_file: stateFile,
        workflow_count: Object.keys(state.workflows).length,
        statuses: statusSummary(),
      });
    }

    if (req.method === "POST" && pathname === "/research/start") {
      const body = await readJsonBody(req);
      const workflow = upsertWorkflow({
        workflow_id: body.workflow_id,
        trace_id: body.trace_id,
        mode: body.mode,
        source_bot: body.source_bot ?? "research-client",
        requires_approval: body.requires_approval,
        input: body.input ?? body,
        status: body.status,
        result: body.result,
      });
      await persistState();
      return sendJson(res, 200, {
        workflow_id: workflow.workflow_id,
        trace_id: workflow.trace_id,
        status: workflow.status,
        mode: workflow.mode,
      });
    }

    const researchMatch = pathname.match(/^\/research\/([^/]+)$/);
    if (req.method === "GET" && researchMatch) {
      const workflowId = decodeURIComponent(researchMatch[1]);
      const workflow = getWorkflow(workflowId);
      if (!workflow) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Unknown workflow id: ${workflowId}`,
        });
      }
      return sendJson(res, 200, {
        workflow_id: workflow.workflow_id,
        trace_id: workflow.trace_id,
        status: workflow.status,
        mode: workflow.mode,
        requires_approval: workflow.requires_approval,
        approval: workflow.approval,
        result: workflow.result,
        recommendation: workflow.recommendation,
        updated_at: workflow.updated_at,
      });
    }

    if (req.method === "POST" && pathname === "/workflows/register") {
      const body = await readJsonBody(req);
      const workflow = upsertWorkflow(body);
      await persistState();
      return sendJson(res, 200, workflow);
    }

    const workflowCompleteMatch = pathname.match(/^\/workflows\/([^/]+)\/complete$/);
    if (req.method === "POST" && workflowCompleteMatch) {
      const workflowId = decodeURIComponent(workflowCompleteMatch[1]);
      const workflow = getWorkflow(workflowId);
      if (!workflow) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Unknown workflow id: ${workflowId}`,
        });
      }
      const body = await readJsonBody(req);
      workflow.status = String(body.status ?? "completed");
      workflow.result = body.result ?? workflow.result ?? null;
      workflow.updated_at = nowIso();
      appendWorkflowEvent(workflowId, {
        kind: "workflow_complete",
        status: workflow.status,
      });
      await persistState();
      return sendJson(res, 200, workflow);
    }

    const workflowEventsMatch = pathname.match(/^\/workflows\/([^/]+)\/events$/);
    if (req.method === "POST" && workflowEventsMatch) {
      const workflowId = decodeURIComponent(workflowEventsMatch[1]);
      const body = await readJsonBody(req);
      const workflow = appendWorkflowEvent(workflowId, body);
      if (!workflow) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Unknown workflow id: ${workflowId}`,
        });
      }
      await persistState();
      return sendJson(res, 200, {
        workflow_id: workflowId,
        event_count: workflow.events.length,
      });
    }

    const workflowGetMatch = pathname.match(/^\/workflows\/([^/]+)$/);
    if (req.method === "GET" && workflowGetMatch) {
      const workflowId = decodeURIComponent(workflowGetMatch[1]);
      const workflow = getWorkflow(workflowId);
      if (!workflow) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Unknown workflow id: ${workflowId}`,
        });
      }
      return sendJson(res, 200, workflow);
    }

    const executionApproveMatch = pathname.match(/^\/execution\/([^/]+)\/approve$/);
    if (req.method === "POST" && executionApproveMatch) {
      const workflowId = decodeURIComponent(executionApproveMatch[1]);
      const workflow = getWorkflow(workflowId);
      if (!workflow) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Unknown workflow id: ${workflowId}`,
        });
      }
      const body = await readJsonBody(req);
      workflow.approval = {
        approved: true,
        approved_at: nowIso(),
        approved_by: String(body.approved_by ?? "operator"),
        command_context: String(body.command_context ?? "manual"),
      };
      if (workflow.status === "awaiting_approval") {
        workflow.status = "approved";
      }
      workflow.updated_at = nowIso();
      appendWorkflowEvent(workflowId, {
        kind: "execution_approved",
        approved_by: workflow.approval.approved_by,
      });
      await persistState();
      return sendJson(res, 200, {
        workflow_id: workflowId,
        status: workflow.status,
        approval: workflow.approval,
      });
    }

    const executionApprovalMatch = pathname.match(/^\/execution\/([^/]+)\/approval$/);
    if (req.method === "GET" && executionApprovalMatch) {
      const workflowId = decodeURIComponent(executionApprovalMatch[1]);
      const workflow = getWorkflow(workflowId);
      if (!workflow) {
        return sendJson(res, 404, {
          error: "not_found",
          message: `Unknown workflow id: ${workflowId}`,
        });
      }
      return sendJson(res, 200, {
        workflow_id: workflowId,
        approved: Boolean(workflow.approval?.approved),
        approval: workflow.approval,
        status: workflow.status,
      });
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

await loadState();

const server = createServer((req, res) => {
  void handleRequest(req, res);
});

server.listen(port, host, () => {
  console.log(
    JSON.stringify(
      {
        ts: nowIso(),
        service: "temporal-broker",
        status: "listening",
        bind: `${host}:${port}`,
        state_file: stateFile,
      },
      null,
      2,
    ),
  );
});
