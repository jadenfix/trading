#!/usr/bin/env node
import { randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { appendFile, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readLimitedJsonBody } from "../shared/http-json.mjs";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..", "..");

const port = Number.parseInt(process.env.BROKER_PORT ?? "8787", 10);
const host = process.env.BROKER_HOST ?? "127.0.0.1";
const defaultProject = process.env.OBS_PROJECT ?? "local";
const defaultLocation = process.env.OBS_LOCATION ?? "us-central1";
const stateFile = process.env.BROKER_STATE_FILE
  ? path.resolve(process.env.BROKER_STATE_FILE)
  : path.join(repoRoot, ".trading-cli", "observability", "broker-state.json");
const auditFile = process.env.BROKER_AUDIT_FILE
  ? path.resolve(process.env.BROKER_AUDIT_FILE)
  : path.join(repoRoot, ".trading-cli", "observability", "control-audit.jsonl");
const maxBodyBytes = Math.max(1024, Number.parseInt(process.env.BROKER_MAX_BODY_BYTES ?? "1048576", 10) || 1048576);

const TERMINAL_STATUSES = new Set(["executed", "completed", "failed", "canceled_soft", "canceled_hard"]);

const state = {
  version: 2,
  workflows: {},
  operations: {},
  request_index: {},
};

function nowIso() {
  return new Date().toISOString();
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
      "running",
      "awaiting_approval",
      "approved",
      "executed",
      "completed",
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

function deriveAvailableActions(status, cancelState = "none", controlLocked = false) {
  if (controlLocked) {
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

function workflowResourceName(project, location, workflowId) {
  return `projects/${project}/locations/${location}/workflows/${workflowId}`;
}

function operationResourceName(project, location, operationId) {
  return `projects/${project}/locations/${location}/operations/${operationId}`;
}

function rpcCode(status) {
  const map = {
    INVALID_ARGUMENT: 3,
    NOT_FOUND: 5,
    FAILED_PRECONDITION: 9,
    UNAUTHENTICATED: 16,
    INTERNAL: 13,
  };
  return map[status] ?? 2;
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
    "content-length": Buffer.byteLength(body),
    "cache-control": "no-store",
    ...extraHeaders,
  });
  res.end(body);
}

function sendGoogleError(res, statusCode, status, message, details = []) {
  return sendJson(res, statusCode, googleError(statusCode, status, message, details));
}

async function ensureStateDir() {
  await mkdir(path.dirname(stateFile), { recursive: true });
}

async function ensureAuditDir() {
  await mkdir(path.dirname(auditFile), { recursive: true });
}

async function loadState() {
  try {
    await ensureStateDir();
    const raw = await readFile(stateFile, "utf8");
    const decoded = JSON.parse(raw);
    if (decoded && typeof decoded === "object") {
      state.version = Number.isInteger(decoded.version) ? decoded.version : 2;
      state.workflows = decoded.workflows && typeof decoded.workflows === "object" ? decoded.workflows : {};
      state.operations = decoded.operations && typeof decoded.operations === "object" ? decoded.operations : {};
      state.request_index =
        decoded.request_index && typeof decoded.request_index === "object" ? decoded.request_index : {};
    }
  } catch {
    // Start with empty state when missing/invalid.
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
    .catch((error) => {
      console.error(
        `[broker] Failed to persist state to ${stateFile}:`,
        error instanceof Error ? error.message : String(error),
      );
    });
  return writeInFlight;
}

let auditInFlight = Promise.resolve();
function appendAudit(entry) {
  auditInFlight = auditInFlight
    .catch(() => undefined)
    .then(async () => {
      await ensureAuditDir();
      await appendFile(auditFile, `${JSON.stringify({ ts: nowIso(), ...entry })}\n`, "utf8");
    });
  return auditInFlight;
}

async function readJsonBody(req) {
  return readLimitedJsonBody(req, maxBodyBytes);
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
  const normalizedStatus = payload.status
    ? normalizeStatus(payload.status)
    : payload.requires_approval
      ? "awaiting_approval"
      : normalizeStatus(existing?.status ?? "running");

  const merged = {
    workflow_id: workflowId,
    trace_id: String(payload.trace_id ?? existing?.trace_id ?? workflowId),
    mode: String(payload.mode ?? existing?.mode ?? "unknown"),
    source_bot: String(payload.source_bot ?? existing?.source_bot ?? "unknown"),
    requires_approval: Boolean(payload.requires_approval ?? existing?.requires_approval ?? false),
    status: normalizedStatus,
    cancel_state: String(payload.cancel_state ?? existing?.cancel_state ?? "none"),
    control_locked: Boolean(payload.control_locked ?? existing?.control_locked ?? false),
    created_at: createdAt,
    updated_at: nowIso(),
    recommendation: payload.recommendation ?? existing?.recommendation ?? null,
    result: payload.result ?? existing?.result ?? null,
    approval: payload.approval ?? existing?.approval ?? null,
    events: Array.isArray(existing?.events) ? existing.events : [],
    input: payload.input ?? existing?.input ?? null,
    last_command_at: payload.last_command_at ?? existing?.last_command_at ?? null,
    last_command_by: payload.last_command_by ?? existing?.last_command_by ?? null,
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

function buildWorkflowResource(project, location, workflow) {
  const status = normalizeStatus(workflow.status);
  return {
    name: workflowResourceName(project, location, workflow.workflow_id),
    workflowId: workflow.workflow_id,
    traceId: workflow.trace_id,
    sourceBot: workflow.source_bot,
    mode: workflow.mode,
    state: toWorkflowState(status),
    status,
    requiresApproval: Boolean(workflow.requires_approval),
    approval: workflow.approval ?? null,
    cancelState: workflow.cancel_state ?? "none",
    controlLocked: Boolean(workflow.control_locked),
    availableActions: deriveAvailableActions(status, workflow.cancel_state, Boolean(workflow.control_locked)),
    createTime: workflow.created_at ?? null,
    updateTime: workflow.updated_at ?? null,
    recommendation: workflow.recommendation ?? null,
    result: workflow.result ?? null,
    eventCount: Array.isArray(workflow.events) ? workflow.events.length : 0,
    lastCommandAt: workflow.last_command_at ?? null,
    lastCommandBy: workflow.last_command_by ?? null,
  };
}

function statusSummary() {
  const counts = {
    running: 0,
    awaiting_approval: 0,
    approved: 0,
    executed: 0,
    completed: 0,
    failed: 0,
    canceled_soft: 0,
    canceled_hard: 0,
    other: 0,
  };

  for (const workflow of Object.values(state.workflows)) {
    const status = normalizeStatus(workflow.status);
    if (Object.prototype.hasOwnProperty.call(counts, status)) {
      counts[status] += 1;
    } else {
      counts.other += 1;
    }
  }

  return counts;
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

function applyWorkflowFilter(resources, query) {
  const parsedFilter = parseFilter(query.get("filter"));
  const stateFilter = query.get("state") ?? parsedFilter.state;
  const botFilter = query.get("source_bot") ?? query.get("bot") ?? parsedFilter.source_bot;

  return resources.filter((resource) => {
    if (stateFilter && String(resource.state).toUpperCase() !== String(stateFilter).toUpperCase()) {
      return false;
    }
    if (botFilter && String(resource.sourceBot) !== String(botFilter)) {
      return false;
    }
    return true;
  });
}

function orderResources(resources, orderByRaw) {
  const orderBy = String(orderByRaw ?? "updateTime desc").trim();
  const [fieldRaw, dirRaw] = orderBy.split(/\s+/);
  const field = fieldRaw || "updateTime";
  const descending = String(dirRaw ?? "desc").toLowerCase() !== "asc";

  const sorted = [...resources].sort((left, right) => {
    let leftValue = "";
    let rightValue = "";

    if (field === "createTime") {
      leftValue = left.createTime ?? "";
      rightValue = right.createTime ?? "";
    } else if (field === "state") {
      leftValue = left.state ?? "";
      rightValue = right.state ?? "";
    } else {
      leftValue = left.updateTime ?? "";
      rightValue = right.updateTime ?? "";
    }

    if (leftValue === rightValue) {
      return 0;
    }
    if (descending) {
      return leftValue < rightValue ? 1 : -1;
    }
    return leftValue < rightValue ? -1 : 1;
  });

  return sorted;
}

function paginate(items, query) {
  const pageSizeRaw = query.get("pageSize");
  const pageSize = Math.max(1, Math.min(1000, Number.parseInt(pageSizeRaw ?? "200", 10) || 200));
  const tokenRaw = query.get("pageToken") ?? "0";
  const offset = Math.max(0, Number.parseInt(tokenRaw, 10) || 0);

  const slice = items.slice(offset, offset + pageSize);
  const nextPageToken = offset + pageSize < items.length ? String(offset + pageSize) : "";
  return {
    items: slice,
    nextPageToken,
  };
}

function createOperation(project, location, action, target, actor, reason, requestId) {
  const operationId = `op-${Date.now()}-${randomUUID().slice(0, 8)}`;
  const name = operationResourceName(project, location, operationId);
  const now = nowIso();

  const operation = {
    name,
    done: false,
    metadata: {
      action,
      target,
      actor,
      reason,
      requestId: requestId || null,
      createTime: now,
      updateTime: now,
    },
  };

  state.operations[name] = operation;
  return operation;
}

function completeOperation(operation, responsePayload, errorPayload) {
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

function requestIndexKey(project, location, target, action, requestId) {
  if (!requestId) {
    return null;
  }
  return `${project}|${location}|${target}|${action}|${requestId}`;
}

function setCommandMetadata(workflow, actor) {
  workflow.last_command_at = nowIso();
  workflow.last_command_by = actor;
  workflow.updated_at = nowIso();
}

function applyExecute(workflow, actor, reason) {
  const status = normalizeStatus(workflow.status);

  if (status !== "awaiting_approval") {
    return {
      ok: false,
      status: "FAILED_PRECONDITION",
      message: `Execute is only allowed from awaiting_approval (current=${status})`,
    };
  }

  workflow.approval = {
    approved: true,
    approved_at: nowIso(),
    approved_by: actor,
    command_context: "google-style-api",
    reason: reason || null,
  };
  workflow.status = "approved";
  workflow.cancel_state = "none";
  workflow.control_locked = false;
  setCommandMetadata(workflow, actor);

  appendWorkflowEvent(workflow.workflow_id, {
    kind: "execute_requested",
    requested_by: actor,
    reason: reason || null,
  });
  appendWorkflowEvent(workflow.workflow_id, {
    kind: "execution_approved",
    approved_by: actor,
  });

  return {
    ok: true,
    outcome: "execution_approved",
  };
}

function applySoftCancel(workflow, actor, reason) {
  const status = normalizeStatus(workflow.status);
  const cancelState = String(workflow.cancel_state ?? "none").trim().toLowerCase();

  if (status === "canceled_soft") {
    return {
      ok: true,
      outcome: "already_canceled_soft",
    };
  }
  if (cancelState === "soft_requested") {
    return {
      ok: true,
      outcome: "already_soft_requested",
    };
  }

  if (status === "canceled_hard") {
    return {
      ok: false,
      status: "FAILED_PRECONDITION",
      message: "Workflow is already hard-canceled",
    };
  }

  if (isTerminalStatus(status)) {
    return {
      ok: false,
      status: "FAILED_PRECONDITION",
      message: `Cannot cancel terminal workflow (current=${status})`,
    };
  }

  workflow.cancel_state = "soft_requested";
  workflow.control_locked = false;
  setCommandMetadata(workflow, actor);

  appendWorkflowEvent(workflow.workflow_id, {
    kind: "cancel_requested_soft",
    requested_by: actor,
    reason: reason || null,
  });
  return {
    ok: true,
    outcome: "soft_cancel_requested",
  };
}

function applyHardCancel(workflow, actor, reason) {
  const status = normalizeStatus(workflow.status);

  if (status === "canceled_hard") {
    return {
      ok: true,
      outcome: "already_canceled_hard",
    };
  }

  if (isTerminalStatus(status) && status !== "canceled_soft") {
    return {
      ok: false,
      status: "FAILED_PRECONDITION",
      message: `Cannot hard-cancel terminal workflow (current=${status})`,
    };
  }

  workflow.cancel_state = "hard_requested";
  workflow.status = "canceled_hard";
  workflow.control_locked = true;
  setCommandMetadata(workflow, actor);

  appendWorkflowEvent(workflow.workflow_id, {
    kind: "cancel_requested_hard",
    requested_by: actor,
    reason: reason || null,
  });
  appendWorkflowEvent(workflow.workflow_id, {
    kind: "cleanup_started",
  });
  appendWorkflowEvent(workflow.workflow_id, {
    kind: "cleanup_completed",
    checklist: {
      pending_approval_cleared: true,
      command_queue_cleared: true,
      control_locked: true,
    },
  });
  appendWorkflowEvent(workflow.workflow_id, {
    kind: "workflow_canceled_hard",
  });

  return {
    ok: true,
    outcome: "canceled_hard",
  };
}

async function handleControlAction(req, res, project, location, workflowId, action) {
  const workflow = getWorkflow(workflowId);
  if (!workflow) {
    return sendGoogleError(res, 404, "NOT_FOUND", `Unknown workflow id: ${workflowId}`);
  }

  let body;
  try {
    body = await readJsonBody(req);
  } catch (error) {
    if (error && typeof error === "object" && Number.isInteger(error.statusCode)) {
      return sendGoogleError(
        res,
        error.statusCode,
        error.googleStatus ?? "INVALID_ARGUMENT",
        error.message ?? "Invalid JSON body",
      );
    }
    return sendGoogleError(res, 400, "INVALID_ARGUMENT", "Invalid JSON body");
  }

  const actor = String(body.actor ?? req.headers["x-observability-actor"] ?? "operator");
  const reason = String(body.reason ?? "").trim();
  const requestId = String(body.requestId ?? body.request_id ?? "").trim();

  const targetName = workflowResourceName(project, location, workflowId);
  const idxKey = requestIndexKey(project, location, targetName, action, requestId);
  if (idxKey && state.request_index[idxKey] && state.operations[state.request_index[idxKey]]) {
    return sendJson(res, 200, state.operations[state.request_index[idxKey]]);
  }

  const operation = createOperation(project, location, action, targetName, actor, reason || null, requestId || null);

  let result;
  if (action === "execute") {
    result = applyExecute(workflow, actor, reason);
  } else if (action === "cancel") {
    result = applySoftCancel(workflow, actor, reason);
  } else {
    result = applyHardCancel(workflow, actor, reason);
  }

  if (result.ok) {
    completeOperation(operation, {
      workflow: buildWorkflowResource(project, location, workflow),
      outcome: result.outcome,
    });
  } else {
    completeOperation(operation, null, {
      code: rpcCode(result.status),
      status: result.status,
      message: result.message,
    });
  }

  if (idxKey) {
    state.request_index[idxKey] = operation.name;
  }

  await Promise.all([
    appendAudit({
      actor,
      action,
      target: targetName,
      request_id: requestId || null,
      reason: reason || null,
      outcome: result.ok ? result.outcome : "error",
      status: result.ok ? "OK" : result.status,
    }),
    persistState(),
  ]);

  return sendJson(res, 200, operation);
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
        audit_file: auditFile,
        workflow_count: Object.keys(state.workflows).length,
        operation_count: Object.keys(state.operations).length,
        statuses: statusSummary(),
      });
    }

    // Legacy compatibility routes.
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
        cancel_state: workflow.cancel_state,
        updated_at: workflow.updated_at,
      });
    }

    if (req.method === "POST" && pathname === "/workflows/register") {
      const body = await readJsonBody(req);
      const workflow = upsertWorkflow(body);
      await persistState();
      return sendJson(res, 200, workflow, {
        "x-observability-deprecated": "Use /v1/projects/*/locations/*/workflows/*",
      });
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
      const status = normalizeStatus(body.status ?? "completed");
      if (workflow.status === "canceled_hard" || workflow.status === "canceled_soft") {
        appendWorkflowEvent(workflowId, {
          kind: "workflow_complete_ignored",
          ignored_status: status,
        });
      } else {
        workflow.status = status;
        workflow.result = body.result ?? workflow.result ?? null;
      }
      workflow.updated_at = nowIso();
      appendWorkflowEvent(workflowId, {
        kind: "workflow_complete",
        status: workflow.status,
      });
      await persistState();
      return sendJson(res, 200, workflow, {
        "x-observability-deprecated": "Use /v1/projects/*/locations/*/workflows/*",
      });
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
      return sendJson(res, 200, workflow, {
        "x-observability-deprecated": "Use /v1/projects/*/locations/*/workflows/*",
      });
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
      const actor = String(body.approved_by ?? "operator");
      workflow.approval = {
        approved: true,
        approved_at: nowIso(),
        approved_by: actor,
        command_context: String(body.command_context ?? "manual"),
      };
      if (workflow.status === "awaiting_approval") {
        workflow.status = "approved";
      }
      setCommandMetadata(workflow, actor);
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
        cancel_state: workflow.cancel_state,
      });
    }

    // Google-style V1 endpoints.
    const v1WorkflowListMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/workflows$/);
    if (req.method === "GET" && v1WorkflowListMatch) {
      const project = decodeURIComponent(v1WorkflowListMatch[1]);
      const location = decodeURIComponent(v1WorkflowListMatch[2]);
      let resources = Object.values(state.workflows).map((workflow) => buildWorkflowResource(project, location, workflow));
      resources = applyWorkflowFilter(resources, requestUrl.searchParams);
      resources = orderResources(resources, requestUrl.searchParams.get("orderBy"));
      const page = paginate(resources, requestUrl.searchParams);
      return sendJson(res, 200, {
        workflows: page.items,
        nextPageToken: page.nextPageToken,
      });
    }

    const v1WorkflowActionMatch = pathname.match(
      /^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/workflows\/([^/:]+):(execute|cancel|hardCancel)$/,
    );
    if (req.method === "POST" && v1WorkflowActionMatch) {
      const project = decodeURIComponent(v1WorkflowActionMatch[1]);
      const location = decodeURIComponent(v1WorkflowActionMatch[2]);
      const workflowId = decodeURIComponent(v1WorkflowActionMatch[3]);
      const action = v1WorkflowActionMatch[4];
      return handleControlAction(req, res, project, location, workflowId, action);
    }

    const v1WorkflowGetMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/workflows\/([^/:]+)$/);
    if (req.method === "GET" && v1WorkflowGetMatch) {
      const project = decodeURIComponent(v1WorkflowGetMatch[1]);
      const location = decodeURIComponent(v1WorkflowGetMatch[2]);
      const workflowId = decodeURIComponent(v1WorkflowGetMatch[3]);
      const workflow = getWorkflow(workflowId);
      if (!workflow) {
        return sendGoogleError(res, 404, "NOT_FOUND", `Unknown workflow id: ${workflowId}`);
      }
      return sendJson(res, 200, buildWorkflowResource(project, location, workflow));
    }

    const v1OperationListMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/operations$/);
    if (req.method === "GET" && v1OperationListMatch) {
      const project = decodeURIComponent(v1OperationListMatch[1]);
      const location = decodeURIComponent(v1OperationListMatch[2]);
      const prefix = `projects/${project}/locations/${location}/operations/`;
      const all = Object.values(state.operations)
        .filter((operation) => operation.name.startsWith(prefix))
        .sort((left, right) => {
          const leftTs = Date.parse(left.metadata?.createTime ?? "") || 0;
          const rightTs = Date.parse(right.metadata?.createTime ?? "") || 0;
          return rightTs - leftTs;
        });

      const page = paginate(all, requestUrl.searchParams);
      return sendJson(res, 200, {
        operations: page.items,
        nextPageToken: page.nextPageToken,
      });
    }

    const v1OperationGetMatch = pathname.match(/^\/v1\/projects\/([^/]+)\/locations\/([^/]+)\/operations\/([^/]+)$/);
    if (req.method === "GET" && v1OperationGetMatch) {
      const project = decodeURIComponent(v1OperationGetMatch[1]);
      const location = decodeURIComponent(v1OperationGetMatch[2]);
      const operationId = decodeURIComponent(v1OperationGetMatch[3]);
      const operationName = operationResourceName(project, location, operationId);
      const operation = state.operations[operationName];
      if (!operation) {
        return sendGoogleError(res, 404, "NOT_FOUND", `Unknown operation: ${operationName}`);
      }
      return sendJson(res, 200, operation);
    }

    return sendJson(res, 404, {
      error: "not_found",
      message: `No route for ${req.method} ${pathname}`,
    });
  } catch (error) {
    if (error && typeof error === "object" && Number.isInteger(error.statusCode)) {
      return sendGoogleError(
        res,
        error.statusCode,
        error.googleStatus ?? "INVALID_ARGUMENT",
        error.message ?? "Invalid request",
      );
    }
    if (error instanceof SyntaxError) {
      return sendGoogleError(res, 400, "INVALID_ARGUMENT", "Invalid JSON payload");
    }
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
        audit_file: auditFile,
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
