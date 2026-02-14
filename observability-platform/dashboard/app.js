const state = {
  config: null,
  traces: [],
  executions: [],
  selectedTraceId: null,
  refreshTimer: null,
};

const traceListEl = document.getElementById("trace-list");
const traceCountEl = document.getElementById("trace-count");
const traceTitleEl = document.getElementById("trace-title");
const traceMetaEl = document.getElementById("trace-meta");
const eventTimelineEl = document.getElementById("event-timeline");
const approveBtnEl = document.getElementById("approve-btn");
const botFilterEl = document.getElementById("bot-filter");
const statusFilterEl = document.getElementById("status-filter");
const limitInputEl = document.getElementById("limit-input");
const refreshBtnEl = document.getElementById("refresh-btn");
const temporalLinkEl = document.getElementById("temporal-link");
const executedListEl = document.getElementById("executed-list");
const executedCountEl = document.getElementById("executed-count");

const STATUS_PRIORITY = {
  executed: 6,
  awaiting_approval: 5,
  approved: 4,
  running: 3,
  completed: 2,
  failed: 1,
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

function statusChip(status) {
  const normalized = String(status || "running").toLowerCase();
  const className = `status-chip status-${normalized}`;
  return `<span class="${className}">${escapeHtml(normalized)}</span>`;
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

function compareTraces(left, right) {
  const leftExec = Number(left.executed_trade_count || 0);
  const rightExec = Number(right.executed_trade_count || 0);
  if (leftExec !== rightExec) {
    return rightExec - leftExec;
  }

  const leftStatus = STATUS_PRIORITY[String(left.status || "running").toLowerCase()] || 0;
  const rightStatus = STATUS_PRIORITY[String(right.status || "running").toLowerCase()] || 0;
  if (leftStatus !== rightStatus) {
    return rightStatus - leftStatus;
  }

  const execTsDelta = parseEpoch(right.latest_execution_ts) - parseEpoch(left.latest_execution_ts);
  if (execTsDelta !== 0) {
    return execTsDelta;
  }

  return parseEpoch(right.ts_start) - parseEpoch(left.ts_start);
}

async function loadConfig() {
  try {
    const response = await fetch("/api/config", { cache: "no-store" });
    if (!response.ok) {
      throw new Error(`Failed to load config: ${response.status} ${response.statusText}`);
    }
    const payload = await response.json();
    state.config = payload || {};
    if (payload && payload.temporal_ui_url) {
      temporalLinkEl.href = payload.temporal_ui_url;
    }
  } catch (error) {
    console.error("Error loading config from /api/config:", error);
    // Fallback to a safe default config; preserve any existing temporal link if present.
    state.config = state.config || {};
  }
}

function syncFilters(traces) {
  const botValues = new Set([""]);
  const statusValues = new Set([""]);
  for (const trace of traces) {
    botValues.add(trace.source_bot || "unknown");
    statusValues.add(trace.status || "running");
  }

  const selectedBot = botFilterEl.value;
  const selectedStatus = statusFilterEl.value;

  botFilterEl.innerHTML = [...botValues]
    .map((value) => `<option value="${escapeHtml(value)}">${escapeHtml(value || "All")}</option>`)
    .join("");
  statusFilterEl.innerHTML = [...statusValues]
    .map((value) => `<option value="${escapeHtml(value)}">${escapeHtml(value || "All")}</option>`)
    .join("");

  if ([...botValues].includes(selectedBot)) {
    botFilterEl.value = selectedBot;
  }
  if ([...statusValues].includes(selectedStatus)) {
    statusFilterEl.value = selectedStatus;
  }
}

async function loadTraces() {
  const params = new URLSearchParams();
  if (botFilterEl.value) {
    params.set("bot", botFilterEl.value);
  }
  if (statusFilterEl.value) {
    params.set("status", statusFilterEl.value);
  }
  const limit = Number.parseInt(limitInputEl.value || "200", 10);
  params.set("limit", Number.isInteger(limit) && limit > 0 ? String(limit) : "200");

  const response = await fetch(`/api/traces?${params.toString()}`, { cache: "no-store" });
  const payload = await response.json();
  const traces = Array.isArray(payload.traces) ? payload.traces : [];
  state.traces = traces.sort(compareTraces);

  syncFilters(state.traces);

  if (!state.selectedTraceId && state.traces.length > 0) {
    state.selectedTraceId = state.traces[0].trace_id;
  }
  if (
    state.selectedTraceId &&
    !state.traces.some((trace) => trace.trace_id === state.selectedTraceId)
  ) {
    state.selectedTraceId = state.traces.length ? state.traces[0].trace_id : null;
  }

  renderTraceList();
  if (state.selectedTraceId) {
    await renderTraceDetail(state.selectedTraceId);
  } else {
    renderEmptyTraceDetail();
  }
}

async function loadExecutions() {
  const params = new URLSearchParams();
  params.set("limit", "18");
  if (botFilterEl.value) {
    params.set("bot", botFilterEl.value);
  }

  const response = await fetch(`/api/executions?${params.toString()}`, { cache: "no-store" });
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
        <article class="execution-card" data-trace-id="${escapeHtml(execution.trace_id || "")}">
          <p class="execution-summary">${escapeHtml(execution.summary || "Executed trade")}</p>
          <p class="execution-meta">
            <span>${escapeHtml(execution.status || "executed")}</span>
            <span>${escapeHtml(formatTs(execution.ts))}</span>
          </p>
        </article>
      `;
    })
    .join("");

  for (const card of executedListEl.querySelectorAll("[data-trace-id]")) {
    card.addEventListener("click", async () => {
      const traceId = card.getAttribute("data-trace-id");
      if (!traceId) {
        return;
      }
      state.selectedTraceId = traceId;

      if (!state.traces.some((trace) => trace.trace_id === traceId)) {
        botFilterEl.value = "";
        statusFilterEl.value = "";
        await loadTraces();
      }

      renderTraceList();
      await renderTraceDetail(traceId);
    });
  }
}

function renderTraceList() {
  traceCountEl.textContent = String(state.traces.length);
  if (!state.traces.length) {
    traceListEl.innerHTML = `<li class="trace-list-item"><p class="muted">No traces found.</p></li>`;
    return;
  }

  traceListEl.innerHTML = state.traces
    .map((trace) => {
      const active = trace.trace_id === state.selectedTraceId ? "active" : "";
      const executedCount = Number(trace.executed_trade_count || 0);
      return `
        <li class="trace-list-item ${active}" data-trace-id="${escapeHtml(trace.trace_id)}">
          <p class="trace-id">${escapeHtml(trace.trace_id)}</p>
          <p class="trace-row">
            <span>${escapeHtml(trace.source_bot || "unknown")}</span>
            ${statusChip(trace.status)}
          </p>
          <p class="trace-row">
            <span>${escapeHtml(trace.mode || "-")}</span>
            <span>${escapeHtml(String(trace.event_count ?? 0))} events</span>
          </p>
          <p class="trace-row">
            <span>${escapeHtml(formatTs(trace.ts_start))}</span>
            <span class="exec-chip">${escapeHtml(String(executedCount))} executed</span>
          </p>
        </li>
      `;
    })
    .join("");

  for (const item of traceListEl.querySelectorAll("[data-trace-id]")) {
    item.addEventListener("click", async () => {
      state.selectedTraceId = item.getAttribute("data-trace-id");
      renderTraceList();
      await renderTraceDetail(state.selectedTraceId);
    });
  }
}

function renderEmptyTraceDetail() {
  traceTitleEl.textContent = "Select a trace";
  traceMetaEl.innerHTML = "";
  eventTimelineEl.innerHTML = `<p class="muted">No trace selected.</p>`;
  approveBtnEl.hidden = true;
}

function renderMetaCards(trace) {
  const entries = [
    ["Trace", trace.trace_id],
    ["Workflow", trace.workflow_id || "-"],
    ["Bot", trace.source_bot || "-"],
    ["Mode", trace.mode || "-"],
    ["Status", trace.status || "running"],
    ["Start", formatTs(trace.ts_start)],
    ["End", formatTs(trace.ts_end)],
    ["Events", trace.event_count ?? 0],
    ["Executed", trace.executed_trade_count ?? 0],
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

function renderExecutionHighlights(executions) {
  if (!executions.length) {
    return "";
  }

  return `
    <section class="execution-highlights">
      <h4>Executed In This Trace</h4>
      <div class="execution-highlight-list">
        ${executions
      .map((execution) => {
        return `
              <article class="execution-highlight-card">
                <p class="execution-summary">${escapeHtml(execution.summary || "Executed trade")}</p>
                <p class="execution-meta">
                  <span>${escapeHtml(execution.status || "executed")}</span>
                  <span>${escapeHtml(formatTs(execution.ts))}</span>
                </p>
              </article>
            `;
      })
      .join("")}
      </div>
    </section>
  `;
}

async function renderTraceDetail(traceId) {
  const response = await fetch(`/api/traces/${encodeURIComponent(traceId)}`, { cache: "no-store" });
  if (!response.ok) {
    renderEmptyTraceDetail();
    return;
  }

  const trace = await response.json();
  traceTitleEl.textContent = trace.trace_id;
  traceMetaEl.innerHTML = renderMetaCards(trace);

  const sortedEvents = [...(trace.events || [])].sort((a, b) => {
    const leftExec = isExecutionEvent(a) ? 1 : 0;
    const rightExec = isExecutionEvent(b) ? 1 : 0;
    if (leftExec !== rightExec) {
      return rightExec - leftExec;
    }
    return parseEpoch(b.ts) - parseEpoch(a.ts);
  });

  const executions = Array.isArray(trace.executed_trades)
    ? [...trace.executed_trades].sort((a, b) => parseEpoch(b.ts) - parseEpoch(a.ts))
    : [];

  if (!sortedEvents.length) {
    eventTimelineEl.innerHTML = `<p class="muted">No events for this trace yet.</p>`;
  } else {
    eventTimelineEl.innerHTML = `
      ${renderExecutionHighlights(executions)}
      ${sortedEvents
        .map((event) => {
          const payload = JSON.stringify(event.payload || {}, null, 2);
          const execClass = isExecutionEvent(event) ? "execution-event" : "";
          return `
            <article class="event-card ${execClass}">
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

  const awaitingApproval = String(trace.status || "").toLowerCase() === "awaiting_approval";
  approveBtnEl.hidden = !awaitingApproval;
  approveBtnEl.onclick = async () => {
    const approvedBy = window.prompt("Approve as user:", "operator");
    if (!approvedBy) {
      return;
    }

    const approveResponse = await fetch(`/api/traces/${encodeURIComponent(trace.trace_id)}/approve`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify({
        approved_by: approvedBy,
        command_context: "dashboard",
      }),
    });

    const payload = await approveResponse.json();
    if (!approveResponse.ok) {
      window.alert(`Approval failed: ${payload.message || payload.error || approveResponse.status}`);
      return;
    }

    await fullRefresh();
  };
}

async function fullRefresh() {
  try {
    if (!state.config) {
      await loadConfig();
    }
    await loadTraces();
    await loadExecutions();
  } catch (error) {
    traceListEl.innerHTML = `<li class="trace-list-item"><p class="muted">Failed to load traces: ${escapeHtml(error.message || String(error))}</p></li>`;
    executedListEl.innerHTML = `<p class="muted">Failed to load executions: ${escapeHtml(error.message || String(error))}</p>`;
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
  state.selectedTraceId = null;
  void fullRefresh();
});

statusFilterEl.addEventListener("change", () => {
  state.selectedTraceId = null;
  void fullRefresh();
});

limitInputEl.addEventListener("change", () => {
  state.selectedTraceId = null;
  void fullRefresh();
});

await fullRefresh();
setupAutoRefresh();
