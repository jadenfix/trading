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
    if (process.env.CI === "true") {
      assert.fail("TCP bind to localhost is unavailable in CI; observability API tests must run in pipeline");
    }
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

    const approved = await jsonRequest(baseUrl, "GET", `${parentPath}/workflows/${workflowId}`);
    const approvedEvents = approved.payload?.eventCount;
    assert.equal(approved.payload?.status, "approved");
    assert.deepEqual(approved.payload?.availableActions, ["cancel", "hardCancel"]);

    const repeatedRegistration = await jsonRequest(baseUrl, "POST", "/workflows/register", {
      workflow_id: workflowId,
      trace_id: workflowId,
      source_bot: "sports-agent",
      mode: "hitl",
      // This is the exact lifecycle shape the real sports worker re-posts.
      status: "awaiting_approval",
      requires_approval: true,
    });
    assert.equal(repeatedRegistration.status, 200);

    const afterRepeatedRegistration = await jsonRequest(
      baseUrl,
      "GET",
      `${parentPath}/workflows/${workflowId}`,
    );
    assert.equal(afterRepeatedRegistration.payload?.status, "approved");
    assert.equal(afterRepeatedRegistration.payload?.approval?.approved, true);
    assert.equal(afterRepeatedRegistration.payload?.eventCount, approvedEvents);
    assert.deepEqual(afterRepeatedRegistration.payload?.availableActions, ["cancel", "hardCancel"]);

    const repeatedExecute = await jsonRequest(
      baseUrl,
      "POST",
      `${parentPath}/workflows/${workflowId}:execute`,
      { actor: "test", reason: "must not approve twice", requestId: "req-execute-2" },
    );
    assert.equal(repeatedExecute.status, 200);
    assert.equal(repeatedExecute.payload?.done, true);
    assert.equal(repeatedExecute.payload?.error?.status, "FAILED_PRECONDITION");

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

    const repeatedAfterSoft = await jsonRequest(baseUrl, "POST", "/workflows/register", {
      workflow_id: workflowId,
      trace_id: workflowId,
      source_bot: "sports-agent",
      mode: "hitl",
      status: "awaiting_approval",
      cancel_state: "none",
      requires_approval: true,
    });
    assert.equal(repeatedAfterSoft.status, 200);
    assert.equal(repeatedAfterSoft.payload?.status, "approved");
    assert.equal(repeatedAfterSoft.payload?.cancel_state, "soft_requested");

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

    const repeatedAfterHard = await jsonRequest(baseUrl, "POST", "/workflows/register", {
      workflow_id: workflowId,
      trace_id: workflowId,
      source_bot: "sports-agent",
      mode: "hitl",
      status: "awaiting_approval",
      cancel_state: "none",
      control_locked: false,
      requires_approval: true,
    });
    assert.equal(repeatedAfterHard.status, 200);
    assert.equal(repeatedAfterHard.payload?.status, "canceled_hard");
    assert.equal(repeatedAfterHard.payload?.cancel_state, "hard_requested");
    assert.equal(repeatedAfterHard.payload?.control_locked, true);
  } finally {
    await broker.stop();
    await cleanupTempDir(tempDir);
  }
});

test("temporal-broker rejects oversized JSON bodies", async (t) => {
  if (!(await canBindLocalhost())) {
    if (process.env.CI === "true") {
      assert.fail("TCP bind to localhost is unavailable in CI; observability API tests must run in pipeline");
    }
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
