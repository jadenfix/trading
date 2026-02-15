const state = {
  config: null,
  workflows: [],
  executions: [],
  selectedWorkflowId: null,
  refreshTimer: null,
  controlToken: window.localStorage.getItem("obs_control_token") || "",
};

const traceListEl = document.getElementById("trace-list");
const traceCountEl = document.getElementById("trace-count");
const traceTitleEl = document.getElementById("trace-title");
const traceMetaEl = document.getElementById("trace-meta");
const eventTimelineEl = document.getElementById("event-timeline");

const executeBtnEl = document.getElementById("execute-btn");
const cancelBtnEl = document.getElementById("cancel-btn");
const hardCancelBtnEl = document.getElementById("hard-cancel-btn");
const stopServiceBtnEl = document.getElementById("stop-service-btn");
const actionMessageEl = document.getElementById("action-message");

const botFilterEl = document.getElementById("bot-filter");
const stateFilterEl = document.getElementById("state-filter");
const limitInputEl = document.getElementById("limit-input");
const refreshBtnEl = document.getElementById("refresh-btn");
const temporalLinkEl = document.getElementById("temporal-link");
const controlTokenInputEl = document.getElementById("control-token-input");

const executedListEl = document.getElementById("executed-list");
const executedCountEl = document.getElementById("executed-count");

const WORKFLOW_STATE_PRIORITY = {
  EXECUTED: 8,
  AWAITING_APPROVAL: 7,
  APPROVED: 6,
  RUNNING: 5,
  COMPLETED: 4,
  CANCELED_SOFT: 3,
  CANCELED_HARD: 2,
  FAILED: 1,
};

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function parseEpoch(value) {
  if (!value) {
    return 0;
  }
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? 0 : parsed;
}

function formatTs(value) {
  if (!value) {
    return "-";
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  return date.toLocaleString();
}

function normalizeState(value) {
  return String(value || "RUNNING").toUpperCase();
}

function stateChip(workflowState) {
  const normalized = normalizeState(workflowState);
  const className = `status-chip status-${normalized.toLowerCase()}`;
  return `<span class="${className}">${escapeHtml(normalized)}</span>`;
}

function runtimeChip(runtimeState) {
  const normalized = String(runtimeState || "UNKNOWN").toUpperCase();
  const className = `runtime-chip runtime-${normalized.toLowerCase()}`;
  return `<span class="${className}">${escapeHtml(normalized)}</span>`;
}

function parentPath() {
  const project = encodeURIComponent(state.config?.default_project || "local");
  const location = encodeURIComponent(state.config?.default_location || "us-central1");
  return `/v1/projects/${project}/locations/${location}`;
}

function workflowPath(workflowId) {
  return `${parentPath()}/workflows/${encodeURIComponent(workflowId)}`;
}

function makeRequestId(prefix) {
  if (globalThis.crypto?.randomUUID) {
    return `${prefix}-${globalThis.crypto.randomUUID()}`;
  }
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`;
}

function isExecutionEvent(event) {
  const kind = String(event?.kind || "").toLowerCase();
  const payload = event?.payload || {};
  if (kind === "order_placed") {
    return true;
  }
  if (kind === "execution_result") {
    const status = String(payload.status || "").toLowerCase();
    const result = String(payload.result || "").toLowerCase();
    return (
      status === "order_placed" ||
      ["complete_fill", "partial_fill_unwound", "partial_fill_unwind_failed"].includes(result)
    );
  }
  return false;
}

function executionSummaryFromEvent(event) {
  const payload = event?.payload || {};
  const side = payload.side ? `${String(payload.side).toUpperCase()} ` : "";
  const action = payload.action ? `${String(payload.action).toUpperCase()} ` : "";
  const ticker = payload.ticker || payload.event_ticker || "market";
  const price = payload.price_cents != null ? ` @ ${payload.price_cents}¢` : "";
  const qty = payload.count != null ? ` x${payload.count}` : "";
  return `${action}${side}${ticker}${price}${qty}`.trim();
}

function compareWorkflows(left, right) {
  const leftExec = Number(left.executedTradeCount || 0);
  const rightExec = Number(right.executedTradeCount || 0);
  if (leftExec !== rightExec) {
    return rightExec - leftExec;
  }

  const leftStateScore = WORKFLOW_STATE_PRIORITY[normalizeState(left.state)] || 0;
  const rightStateScore = WORKFLOW_STATE_PRIORITY[normalizeState(right.state)] || 0;
  if (leftStateScore !== rightStateScore) {
    return rightStateScore - leftStateScore;
  }

  const execTimeDelta = parseEpoch(right.latestExecutionTime) - parseEpoch(left.latestExecutionTime);
  if (execTimeDelta !== 0) {
    return execTimeDelta;
  }

  return parseEpoch(right.createTime) - parseEpoch(left.createTime);
}

function setActionMessage(message, error = false) {
  actionMessageEl.textContent = message;
  actionMessageEl.classList.toggle("error", Boolean(error));
}

function extractErrorMessage(payload, fallback = "Control action failed") {
  if (!payload) {
    return fallback;
  }
  if (payload.error?.message) {
    return payload.error.message;
  }
  if (payload.message) {
    return payload.message;
  }
  if (payload.error?.status) {
    return payload.error.status;
  }
  return fallback;
}

async function loadConfig() {
  const response = await fetch("/api/config", { cache: "no-store" });
  const payload = await response.json();
  state.config = payload;

  if (payload.temporal_ui_url) {
    temporalLinkEl.href = payload.temporal_ui_url;
  }

  if (!state.controlToken && payload.control_token_default) {
    state.controlToken = payload.control_token_default;
    window.localStorage.setItem("obs_control_token", state.controlToken);
  }

  controlTokenInputEl.value = state.controlToken;
}

function syncFilters(workflows) {
  const botValues = new Set([""]);
  const stateValues = new Set([""]);

  for (const workflow of workflows) {
    botValues.add(workflow.sourceBot || "unknown");
    stateValues.add(normalizeState(workflow.state));
  }

  const selectedBot = botFilterEl.value;
  const selectedState = stateFilterEl.value;

  botFilterEl.innerHTML = [...botValues]
    .map((value) => `<option value="${escapeHtml(value)}">${escapeHtml(value || "All")}</option>`)
    .join("");

  stateFilterEl.innerHTML = [...stateValues]
    .map((value) => `<option value="${escapeHtml(value)}">${escapeHtml(value || "All")}</option>`)
    .join("");

  if ([...botValues].includes(selectedBot)) {
    botFilterEl.value = selectedBot;
  }
  if ([...stateValues].includes(selectedState)) {
    stateFilterEl.value = selectedState;
  }
}

async function loadWorkflows() {
  const params = new URLSearchParams();
  const limit = Number.parseInt(limitInputEl.value || "200", 10);
  params.set("pageSize", Number.isInteger(limit) && limit > 0 ? String(limit) : "200");

  if (botFilterEl.value) {
    params.set("bot", botFilterEl.value);
  }
  if (stateFilterEl.value) {
    params.set("state", stateFilterEl.value);
  }

  const response = await fetch(`${parentPath()}/workflows?${params.toString()}`, { cache: "no-store" });
  const payload = await response.json();
  const workflows = Array.isArray(payload.workflows) ? payload.workflows : [];
  state.workflows = workflows.sort(compareWorkflows);

  syncFilters(state.workflows);

  if (!state.selectedWorkflowId && state.workflows.length > 0) {
    state.selectedWorkflowId = state.workflows[0].workflowId;
  }
  if (
    state.selectedWorkflowId &&
    !state.workflows.some((workflow) => workflow.workflowId === state.selectedWorkflowId)
  ) {
    state.selectedWorkflowId = state.workflows.length ? state.workflows[0].workflowId : null;
  }

  renderTraceList();

  if (state.selectedWorkflowId) {
    await renderTraceDetail(state.selectedWorkflowId);
  } else {
    renderEmptyTraceDetail();
  }
}

async function loadExecutions() {
  const params = new URLSearchParams();
  params.set("pageSize", "18");
  if (botFilterEl.value) {
    params.set("bot", botFilterEl.value);
  }

  const response = await fetch(`${parentPath()}/executions?${params.toString()}`, { cache: "no-store" });
  if (!response.ok) {
    state.executions = [];
    renderExecutedTrades();
    return;
  }

  const payload = await response.json();
  state.executions = Array.isArray(payload.executions) ? payload.executions : [];
  renderExecutedTrades();
}

function renderExecutedTrades() {
  executedCountEl.textContent = String(state.executions.length);

  if (!state.executions.length) {
    executedListEl.innerHTML = `<p class="muted">No executed trades found in current filters.</p>`;
    return;
  }

  executedListEl.innerHTML = state.executions
    .map((execution) => {
      return `
        <article class="execution-card" data-workflow-id="${escapeHtml(execution.workflowId || execution.traceId || "")}">
          <p class="execution-summary">${escapeHtml(execution.summary || "Executed trade")}</p>
          <p class="execution-meta">
            <span>${escapeHtml(execution.status || "executed")}</span>
            <span>${escapeHtml(formatTs(execution.executeTime))}</span>
          </p>
        </article>
      `;
    })
    .join("");

  for (const card of executedListEl.querySelectorAll("[data-workflow-id]")) {
    card.addEventListener("click", async () => {
      const workflowId = card.getAttribute("data-workflow-id");
      if (!workflowId) {
        return;
      }

      state.selectedWorkflowId = workflowId;
      if (!state.workflows.some((workflow) => workflow.workflowId === workflowId)) {
        botFilterEl.value = "";
        stateFilterEl.value = "";
        await loadWorkflows();
      }

      renderTraceList();
      await renderTraceDetail(workflowId);
    });
  }
}

function renderTraceList() {
  traceCountEl.textContent = String(state.workflows.length);

  if (!state.workflows.length) {
    traceListEl.innerHTML = `<li class="trace-list-item"><p class="muted">No traces found.</p></li>`;
    return;
  }

  traceListEl.innerHTML = state.workflows
    .map((workflow) => {
      const active = workflow.workflowId === state.selectedWorkflowId ? "active" : "";
      return `
        <li class="trace-list-item ${active}" data-workflow-id="${escapeHtml(workflow.workflowId)}">
          <p class="trace-id">${escapeHtml(workflow.workflowId)}</p>
          <p class="trace-row">
            <span>${escapeHtml(workflow.sourceBot || "unknown")}</span>
            ${stateChip(workflow.state)}
          </p>
          <p class="trace-row">
            <span>${escapeHtml(workflow.mode || "-")}</span>
            <span>${escapeHtml(String(workflow.eventCount ?? 0))} events</span>
          </p>
          <p class="trace-row">
            ${runtimeChip(workflow.runtimeState)}
            <span class="exec-chip">${escapeHtml(String(workflow.executedTradeCount ?? 0))} executed</span>
          </p>
          <p class="trace-row">
            <span>${escapeHtml(formatTs(workflow.createTime))}</span>
          </p>
        </li>
      `;
    })
    .join("");

  for (const item of traceListEl.querySelectorAll("[data-workflow-id]")) {
    item.addEventListener("click", async () => {
      state.selectedWorkflowId = item.getAttribute("data-workflow-id");
      renderTraceList();
      await renderTraceDetail(state.selectedWorkflowId);
    });
  }
}

function clearActionHandlers() {
  executeBtnEl.onclick = null;
  cancelBtnEl.onclick = null;
  hardCancelBtnEl.onclick = null;
  stopServiceBtnEl.onclick = null;
}

function setActionButtonsDisabled(disabled) {
  executeBtnEl.disabled = disabled;
  cancelBtnEl.disabled = disabled;
  hardCancelBtnEl.disabled = disabled;
  stopServiceBtnEl.disabled = disabled;
}

function renderEmptyTraceDetail() {
  traceTitleEl.textContent = "Select a trace";
  traceMetaEl.innerHTML = "";
  eventTimelineEl.innerHTML = `<p class="muted">No trace selected.</p>`;
  clearActionHandlers();
  setActionButtonsDisabled(true);
}

function renderMetaCards(workflow) {
  const entries = [
    ["Workflow", workflow.workflowId || "-"],
    ["Trace", workflow.traceId || "-"],
    ["Bot", workflow.sourceBot || "-"],
    ["Mode", workflow.mode || "-"],
    ["State", workflow.state || "RUNNING"],
    ["Runtime", workflow.runtimeState || "UNKNOWN"],
    ["Cancel State", workflow.cancelState || "none"],
    ["Start", formatTs(workflow.createTime)],
    ["End", formatTs(workflow.updateTime)],
    ["Events", workflow.eventCount ?? 0],
    ["Executed", workflow.executedTradeCount ?? 0],
    ["Available Actions", (workflow.availableActions || []).join(", ") || "none"],
  ];

  return entries
    .map(
      ([key, value]) => `
        <div class="meta-card">
          <p class="meta-key">${escapeHtml(key)}</p>
          <p class="meta-value">${escapeHtml(String(value))}</p>
        </div>
      `,
    )
    .join("");
}

function renderExecutionHighlights(executionEvents) {
  if (!executionEvents.length) {
    return "";
  }

  return `
    <section class="execution-highlights">
      <h4>Executed In This Trace</h4>
      <div class="execution-highlight-list">
        ${executionEvents
          .map((event) => {
            return `
              <article class="execution-highlight-card">
                <p class="execution-summary">${escapeHtml(executionSummaryFromEvent(event))}</p>
                <p class="execution-meta">
                  <span>${escapeHtml(String(event.kind || "execution"))}</span>
                  <span>${escapeHtml(formatTs(event.ts))}</span>
                </p>
              </article>
            `;
          })
          .join("")}
      </div>
    </section>
  `;
}

async function fetchWorkflow(workflowId) {
  const response = await fetch(workflowPath(workflowId), { cache: "no-store" });
  if (!response.ok) {
    return null;
  }
  return response.json();
}

async function fetchWorkflowEvents(workflowId) {
  const response = await fetch(`${workflowPath(workflowId)}/events?pageSize=1000`, { cache: "no-store" });
  if (!response.ok) {
    return [];
  }
  const payload = await response.json();
  return Array.isArray(payload.events) ? payload.events : [];
}

async function waitForOperation(name) {
  let last = null;
  for (let i = 0; i < 30; i += 1) {
    const response = await fetch(`/v1/${name}`, { cache: "no-store" });
    if (!response.ok) {
      break;
    }
    const payload = await response.json();
    last = payload;
    if (payload.done) {
      return payload;
    }
    await new Promise((resolve) => {
      setTimeout(resolve, 400);
    });
  }
  return last;
}

async function postControl(path, body) {
  const headers = {
    "content-type": "application/json",
  };

  if (state.controlToken) {
    headers.authorization = `Bearer ${state.controlToken}`;
  }

  const response = await fetch(path, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });

  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    throw new Error(extractErrorMessage(payload, `Request failed (${response.status})`));
  }

  if (payload?.name && payload.done === false) {
    const operation = await waitForOperation(payload.name);
    if (operation?.error?.message) {
      throw new Error(operation.error.message);
    }
    return operation;
  }

  if (payload?.error?.message) {
    throw new Error(payload.error.message);
  }

  return payload;
}

function bindActionButtons(workflow) {
  clearActionHandlers();

  const availableActions = new Set(Array.isArray(workflow.availableActions) ? workflow.availableActions : []);
  const hasToken = Boolean(state.controlToken);

  const canExecute = hasToken && availableActions.has("execute");
  const canCancel = hasToken && availableActions.has("cancel");
  const canHardCancel = hasToken && availableActions.has("hardCancel");
  const canStopService =
    hasToken &&
    workflow.sourceBot === "sports-agent" &&
    String(workflow.runtimeState || "UNKNOWN").toUpperCase() !== "PROCESS_STOPPED";

  executeBtnEl.disabled = !canExecute;
  cancelBtnEl.disabled = !canCancel;
  hardCancelBtnEl.disabled = !canHardCancel;
  stopServiceBtnEl.disabled = !canStopService;

  if (!hasToken) {
    setActionMessage("Set a control token to enable execute/cancel/stop actions.", false);
  } else if (isTerminalStatus(workflow.state)) {
    setActionMessage(`Workflow is terminal (${workflow.state}).`, false);
  } else {
    setActionMessage("Actions ready. Confirm each operation in the modal prompt.", false);
  }

  executeBtnEl.onclick = async () => {
    if (!window.confirm(`Execute workflow ${workflow.workflowId}?`)) {
      return;
    }

    try {
      setActionMessage("Submitting execute...", false);
      await postControl(`${workflowPath(workflow.workflowId)}:execute`, {
        actor: "dashboard",
        reason: "manual execute from dashboard",
        requestId: makeRequestId("execute"),
      });
      setActionMessage("Execute request accepted.", false);
      await fullRefresh();
    } catch (error) {
      setActionMessage(error.message || String(error), true);
    }
  };

  cancelBtnEl.onclick = async () => {
    if (!window.confirm(`Soft-cancel workflow ${workflow.workflowId}?`)) {
      return;
    }

    try {
      setActionMessage("Submitting soft cancel...", false);
      await postControl(`${workflowPath(workflow.workflowId)}:cancel`, {
        actor: "dashboard",
        reason: "manual soft cancel from dashboard",
        requestId: makeRequestId("cancel"),
      });
      setActionMessage("Soft cancel request accepted.", false);
      await fullRefresh();
    } catch (error) {
      setActionMessage(error.message || String(error), true);
    }
  };

  hardCancelBtnEl.onclick = async () => {
    if (!window.confirm(`Hard-cancel workflow ${workflow.workflowId}? This locks the workflow terminal.`)) {
      return;
    }

    try {
      setActionMessage("Submitting hard cancel...", false);
      await postControl(`${workflowPath(workflow.workflowId)}:hardCancel`, {
        actor: "dashboard",
        reason: "manual hard cancel from dashboard",
        requestId: makeRequestId("hard-cancel"),
      });
      setActionMessage("Hard cancel request accepted.", false);
      await fullRefresh();
    } catch (error) {
      setActionMessage(error.message || String(error), true);
    }
  };

  stopServiceBtnEl.onclick = async () => {
    if (!window.confirm("Stop sports-agent service process?")) {
      return;
    }

    try {
      setActionMessage("Stopping sports-agent service...", false);
      await postControl(`${parentPath()}/services/sports-agent:stop`, {
        actor: "dashboard",
        reason: "manual stop service from dashboard",
        requestId: makeRequestId("stop-service"),
      });
      setActionMessage("Sports-agent service stop completed.", false);
      await fullRefresh();
    } catch (error) {
      setActionMessage(error.message || String(error), true);
    }
  };
}

function isTerminalStatus(workflowState) {
  const stateUpper = normalizeState(workflowState);
  return ["EXECUTED", "COMPLETED", "FAILED", "CANCELED_SOFT", "CANCELED_HARD"].includes(stateUpper);
}

async function renderTraceDetail(workflowId) {
  const workflow = await fetchWorkflow(workflowId);
  if (!workflow) {
    renderEmptyTraceDetail();
    return;
  }

  const events = await fetchWorkflowEvents(workflowId);

  traceTitleEl.textContent = workflow.workflowId || workflowId;
  traceMetaEl.innerHTML = renderMetaCards(workflow);

  const sortedEvents = [...events].sort((left, right) => {
    const leftExec = isExecutionEvent(left) ? 1 : 0;
    const rightExec = isExecutionEvent(right) ? 1 : 0;
    if (leftExec !== rightExec) {
      return rightExec - leftExec;
    }
    return parseEpoch(right.ts) - parseEpoch(left.ts);
  });

  const executionEvents = sortedEvents.filter((event) => isExecutionEvent(event));

  if (!sortedEvents.length) {
    eventTimelineEl.innerHTML = `<p class="muted">No events for this workflow yet.</p>`;
  } else {
    eventTimelineEl.innerHTML = `
      ${renderExecutionHighlights(executionEvents)}
      ${sortedEvents
        .map((event) => {
          const payload = JSON.stringify(event.payload || {}, null, 2);
          const executionClass = isExecutionEvent(event) ? "execution-event" : "";
          return `
            <article class="event-card ${executionClass}">
              <div class="event-head">
                <p class="event-kind">${escapeHtml(event.kind || "event")}</p>
                <p class="event-ts">${escapeHtml(formatTs(event.ts))}</p>
              </div>
              <pre class="event-payload">${escapeHtml(payload)}</pre>
            </article>
          `;
        })
        .join("")}
    `;
  }

  bindActionButtons(workflow);
}

async function fullRefresh() {
  try {
    if (!state.config) {
      await loadConfig();
    }
    await loadWorkflows();
    await loadExecutions();
  } catch (error) {
    traceListEl.innerHTML = `<li class="trace-list-item"><p class="muted">Failed to load traces: ${escapeHtml(error.message || String(error))}</p></li>`;
    executedListEl.innerHTML = `<p class="muted">Failed to load executions: ${escapeHtml(error.message || String(error))}</p>`;
    setActionMessage(error.message || String(error), true);
  }
}

function setupAutoRefresh() {
  if (state.refreshTimer) {
    clearInterval(state.refreshTimer);
  }
  state.refreshTimer = setInterval(() => {
    void fullRefresh();
  }, 5000);
}

refreshBtnEl.addEventListener("click", () => {
  void fullRefresh();
});

botFilterEl.addEventListener("change", () => {
  state.selectedWorkflowId = null;
  void fullRefresh();
});

stateFilterEl.addEventListener("change", () => {
  state.selectedWorkflowId = null;
  void fullRefresh();
});

limitInputEl.addEventListener("change", () => {
  state.selectedWorkflowId = null;
  void fullRefresh();
});

controlTokenInputEl.addEventListener("change", () => {
  state.controlToken = controlTokenInputEl.value.trim();
  window.localStorage.setItem("obs_control_token", state.controlToken);
  if (!state.controlToken) {
    setActionMessage("Control token cleared. Actions are disabled.", false);
  } else {
    setActionMessage("Control token updated.", false);
  }
  void fullRefresh();
});

await fullRefresh();
setupAutoRefresh();
