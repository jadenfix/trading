import test from "node:test";
import assert from "node:assert/strict";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import {
  canBindLocalhost,
  cleanupTempDir,
  createTempDir,
  getFreePort,
  jsonRequest,
  startNodeService,
} from "./helpers/process-test-utils.mjs";

test("trace-api enforces auth, proxies control actions, and redacts config token", async (t) => {
  if (!(await canBindLocalhost())) {
    t.skip("TCP bind to localhost is not available in this environment");
    return;
  }

  const tempDir = await createTempDir("trace-api-");
  const tradesDir = path.join(tempDir, "TRADES");
  await mkdir(tradesDir, { recursive: true });

  const brokerPort = await getFreePort();
  const brokerBaseUrl = `http://127.0.0.1:${brokerPort}`;
  const traceApiPort = await getFreePort();
  const traceApiBaseUrl = `http://127.0.0.1:${traceApiPort}`;
  const stateFile = path.join(tempDir, "broker-state.json");
  const brokerAuditFile = path.join(tempDir, "broker-audit.jsonl");
  const controlAuditFile = path.join(tempDir, "control-audit.jsonl");
  const controlToken = "integration-test-token";

  const broker = await startNodeService(
    "observability-platform/temporal-broker/broker.mjs",
    {
      BROKER_HOST: "127.0.0.1",
      BROKER_PORT: String(brokerPort),
      BROKER_STATE_FILE: stateFile,
      BROKER_AUDIT_FILE: brokerAuditFile,
      BROKER_MAX_BODY_BYTES: "4096",
    },
    `${brokerBaseUrl}/health`,
  );

  const traceApi = await startNodeService(
    "observability-platform/trace-api/server.mjs",
    {
      TRACE_API_HOST: "127.0.0.1",
      TRACE_API_PORT: String(traceApiPort),
      TRACE_API_MAX_BODY_BYTES: "512",
      TRADES_DIR: tradesDir,
      BROKER_BASE_URL: brokerBaseUrl,
      BROKER_STATE_FILE: stateFile,
      OBS_CONTROL_TOKEN: controlToken,
      OBS_CONTROL_AUDIT_FILE: controlAuditFile,
    },
    `${traceApiBaseUrl}/health`,
  );

  try {
    const workflowId = "wf-trace-api-1";
    const parentPath = "/v1/projects/local/locations/us-central1";

    const register = await jsonRequest(brokerBaseUrl, "POST", "/workflows/register", {
      workflow_id: workflowId,
      trace_id: workflowId,
      source_bot: "sports-agent",
      mode: "hitl",
      status: "awaiting_approval",
      requires_approval: true,
    });
    assert.equal(register.status, 200);

    const config = await jsonRequest(traceApiBaseUrl, "GET", "/api/config");
    assert.equal(config.status, 200);
    assert.equal(config.payload?.control_token_required, true);
    assert.equal(config.payload?.control_token_default, null);

    const unauth = await jsonRequest(traceApiBaseUrl, "POST", `${parentPath}/workflows/${workflowId}:execute`, {
      actor: "test",
      reason: "unauthorized execute",
      requestId: "unauth-1",
    });
    assert.equal(unauth.status, 401);
    assert.equal(unauth.payload?.error?.status, "UNAUTHENTICATED");

    const authExecute = await jsonRequest(
      traceApiBaseUrl,
      "POST",
      `${parentPath}/workflows/${workflowId}:execute`,
      {
        actor: "test",
        reason: "authorized execute",
        requestId: "auth-1",
      },
      {
        authorization: `Bearer ${controlToken}`,
      },
    );
    assert.equal(authExecute.status, 200);
    assert.equal(authExecute.payload?.done, true);
    assert.equal(authExecute.payload?.response?.outcome, "execution_approved");

    const brokerState = await jsonRequest(brokerBaseUrl, "GET", `${parentPath}/workflows/${workflowId}`);
    assert.equal(brokerState.status, 200);
    assert.equal(brokerState.payload?.status, "approved");
  } finally {
    await traceApi.stop();
    await broker.stop();
    await cleanupTempDir(tempDir);
  }
});

test("trace-api rejects oversized control payloads before forwarding", async (t) => {
  if (!(await canBindLocalhost())) {
    t.skip("TCP bind to localhost is not available in this environment");
    return;
  }

  const tempDir = await createTempDir("trace-api-body-");
  const tradesDir = path.join(tempDir, "TRADES");
  await mkdir(tradesDir, { recursive: true });

  const brokerPort = await getFreePort();
  const brokerBaseUrl = `http://127.0.0.1:${brokerPort}`;
  const traceApiPort = await getFreePort();
  const traceApiBaseUrl = `http://127.0.0.1:${traceApiPort}`;
  const stateFile = path.join(tempDir, "broker-state.json");
  const brokerAuditFile = path.join(tempDir, "broker-audit.jsonl");
  const controlAuditFile = path.join(tempDir, "control-audit.jsonl");
  const controlToken = "integration-test-token";

  const broker = await startNodeService(
    "observability-platform/temporal-broker/broker.mjs",
    {
      BROKER_HOST: "127.0.0.1",
      BROKER_PORT: String(brokerPort),
      BROKER_STATE_FILE: stateFile,
      BROKER_AUDIT_FILE: brokerAuditFile,
      BROKER_MAX_BODY_BYTES: "4096",
    },
    `${brokerBaseUrl}/health`,
  );

  const traceApi = await startNodeService(
    "observability-platform/trace-api/server.mjs",
    {
      TRACE_API_HOST: "127.0.0.1",
      TRACE_API_PORT: String(traceApiPort),
      TRACE_API_MAX_BODY_BYTES: "256",
      TRADES_DIR: tradesDir,
      BROKER_BASE_URL: brokerBaseUrl,
      BROKER_STATE_FILE: stateFile,
      OBS_CONTROL_TOKEN: controlToken,
      OBS_CONTROL_AUDIT_FILE: controlAuditFile,
    },
    `${traceApiBaseUrl}/health`,
  );

  try {
    const workflowId = "wf-trace-api-limit-1";
    const parentPath = "/v1/projects/local/locations/us-central1";

    const register = await jsonRequest(brokerBaseUrl, "POST", "/workflows/register", {
      workflow_id: workflowId,
      trace_id: workflowId,
      status: "awaiting_approval",
      requires_approval: true,
    });
    assert.equal(register.status, 200);

    const oversized = await jsonRequest(
      traceApiBaseUrl,
      "POST",
      `${parentPath}/workflows/${workflowId}:execute`,
      {
        actor: "test",
        reason: "x".repeat(2_000),
        requestId: "oversized",
      },
      {
        authorization: `Bearer ${controlToken}`,
      },
    );
    assert.equal(oversized.status, 413);
    assert.equal(oversized.payload?.error?.status, "INVALID_ARGUMENT");

    const brokerState = await jsonRequest(brokerBaseUrl, "GET", `${parentPath}/workflows/${workflowId}`);
    assert.equal(brokerState.status, 200);
    assert.equal(brokerState.payload?.status, "awaiting_approval");
  } finally {
    await traceApi.stop();
    await broker.stop();
    await cleanupTempDir(tempDir);
  }
});
