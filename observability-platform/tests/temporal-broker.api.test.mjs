import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import {
  canBindLocalhost,
  cleanupTempDir,
  createTempDir,
  getFreePort,
  jsonRequest,
  startNodeService,
} from "./helpers/process-test-utils.mjs";

test("temporal-broker v1 lifecycle endpoints and idempotency", async (t) => {
  if (!(await canBindLocalhost())) {
    t.skip("TCP bind to localhost is not available in this environment");
    return;
  }

  const tempDir = await createTempDir("broker-api-");
  const port = await getFreePort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const stateFile = path.join(tempDir, "broker-state.json");
  const auditFile = path.join(tempDir, "broker-audit.jsonl");

  const broker = await startNodeService(
    "observability-platform/temporal-broker/broker.mjs",
    {
      BROKER_HOST: "127.0.0.1",
      BROKER_PORT: String(port),
      BROKER_STATE_FILE: stateFile,
      BROKER_AUDIT_FILE: auditFile,
      BROKER_MAX_BODY_BYTES: "1024",
    },
    `${baseUrl}/health`,
  );

  try {
    const workflowId = "wf-api-lifecycle-1";
    const parentPath = "/v1/projects/local/locations/us-central1";

    const register = await jsonRequest(baseUrl, "POST", "/workflows/register", {
      workflow_id: workflowId,
      trace_id: workflowId,
      source_bot: "sports-agent",
      mode: "hitl",
      status: "awaiting_approval",
      requires_approval: true,
    });
    assert.equal(register.status, 200);

    const execute = await jsonRequest(baseUrl, "POST", `${parentPath}/workflows/${workflowId}:execute`, {
      actor: "test",
      reason: "approve for execution",
      requestId: "req-execute-1",
    });
    assert.equal(execute.status, 200);
    assert.equal(execute.payload?.done, true);
    assert.equal(execute.payload?.response?.outcome, "execution_approved");

    const softCancel = await jsonRequest(baseUrl, "POST", `${parentPath}/workflows/${workflowId}:cancel`, {
      actor: "test",
      reason: "soft cancel requested",
      requestId: "req-cancel-1",
    });
    assert.equal(softCancel.status, 200);
    assert.equal(softCancel.payload?.done, true);
    assert.equal(softCancel.payload?.response?.outcome, "soft_cancel_requested");

    const afterSoft = await jsonRequest(baseUrl, "GET", `${parentPath}/workflows/${workflowId}`);
    assert.equal(afterSoft.status, 200);
    assert.equal(afterSoft.payload?.status, "approved");
    assert.equal(afterSoft.payload?.cancelState, "soft_requested");
    assert.deepEqual(afterSoft.payload?.availableActions, ["hardCancel"]);

    const hardCancel = await jsonRequest(baseUrl, "POST", `${parentPath}/workflows/${workflowId}:hardCancel`, {
      actor: "test",
      reason: "terminal hard cancel",
      requestId: "req-hard-1",
    });
    assert.equal(hardCancel.status, 200);
    assert.equal(hardCancel.payload?.done, true);
    assert.equal(hardCancel.payload?.response?.outcome, "canceled_hard");
    const opName = hardCancel.payload?.name;
    assert.ok(opName);

    const hardCancelIdempotent = await jsonRequest(baseUrl, "POST", `${parentPath}/workflows/${workflowId}:hardCancel`, {
      actor: "test",
      reason: "terminal hard cancel",
      requestId: "req-hard-1",
    });
    assert.equal(hardCancelIdempotent.status, 200);
    assert.equal(hardCancelIdempotent.payload?.name, opName);

    const afterHard = await jsonRequest(baseUrl, "GET", `${parentPath}/workflows/${workflowId}`);
    assert.equal(afterHard.status, 200);
    assert.equal(afterHard.payload?.status, "canceled_hard");
    assert.equal(afterHard.payload?.controlLocked, true);
    assert.deepEqual(afterHard.payload?.availableActions, []);
  } finally {
    await broker.stop();
    await cleanupTempDir(tempDir);
  }
});

test("temporal-broker rejects oversized JSON bodies", async (t) => {
  if (!(await canBindLocalhost())) {
    t.skip("TCP bind to localhost is not available in this environment");
    return;
  }

  const tempDir = await createTempDir("broker-body-");
  const port = await getFreePort();
  const baseUrl = `http://127.0.0.1:${port}`;
  const stateFile = path.join(tempDir, "broker-state.json");
  const auditFile = path.join(tempDir, "broker-audit.jsonl");

  const broker = await startNodeService(
    "observability-platform/temporal-broker/broker.mjs",
    {
      BROKER_HOST: "127.0.0.1",
      BROKER_PORT: String(port),
      BROKER_STATE_FILE: stateFile,
      BROKER_AUDIT_FILE: auditFile,
      BROKER_MAX_BODY_BYTES: "256",
    },
    `${baseUrl}/health`,
  );

  try {
    const workflowId = "wf-body-limit-1";
    const parentPath = "/v1/projects/local/locations/us-central1";

    const register = await jsonRequest(baseUrl, "POST", "/workflows/register", {
      workflow_id: workflowId,
      trace_id: workflowId,
      status: "awaiting_approval",
      requires_approval: true,
    });
    assert.equal(register.status, 200);

    const oversized = await jsonRequest(baseUrl, "POST", `${parentPath}/workflows/${workflowId}:execute`, {
      actor: "test",
      reason: "x".repeat(2_000),
      requestId: "oversized",
    });
    assert.equal(oversized.status, 413);
    assert.equal(oversized.payload?.error?.status, "INVALID_ARGUMENT");
  } finally {
    await broker.stop();
    await cleanupTempDir(tempDir);
  }
});
