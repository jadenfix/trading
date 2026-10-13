import test from "node:test";
import assert from "node:assert/strict";
import { stat } from "node:fs/promises";
import { startPaperDemo } from "../paper-demo.mjs";

test("paper demo serves the real dashboard and isolated approval/cancel lifecycle", async () => {
  const demo = await startPaperDemo();
  const parent = "/v1/projects/paper-demo/locations/local";
  async function request(route, body, authorized = false) {
    const response = await fetch(demo.dashboardUrl + route, {
      method: body ? "POST" : "GET",
      headers: { "content-type": "application/json", ...(authorized ? { authorization: `Bearer ${demo.controlToken}` } : {}) },
      body: body ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(2000),
    });
    return { status: response.status, body: await response.json() };
  }
  try {
    const page = await fetch(demo.dashboardUrl);
    assert.equal(page.status, 200);
    assert.match(await page.text(), /Trading Trace Console/);
    const config = await request("/api/config");
    assert.equal(config.body.demo_mode, true);
    assert.equal(config.body.control_token_default, null);
    for (const asset of ["/app.js", "/styles.css"]) {
      assert.equal((await fetch(demo.dashboardUrl + asset)).status, 200);
    }
    const workflows = await request(`${parent}/workflows`);
    assert.equal(workflows.status, 200);
    assert.match(JSON.stringify(workflows.body), /paper-review/);
    assert.match(JSON.stringify(workflows.body), /paper-filled/);
    const executions = await request(`${parent}/executions`);
    assert.match(JSON.stringify(executions.body), /PAPER-SF-TEMP/);
    const approve = `${parent}/workflows/paper-review:execute`;
    assert.equal((await request(approve, { requestId: "deny-demo" })).status, 401);
    const approved = await request(approve, { requestId: "approve-demo" }, true);
    assert.equal(approved.status, 200);
    assert.equal(approved.body.response.outcome, "execution_approved");
    const canceled = await request(`${parent}/workflows/paper-cancel:cancel`, { requestId: "cancel-demo" }, true);
    assert.equal(canceled.status, 200);
    const state = await request(`${parent}/workflows/paper-review`);
    assert.equal(state.body.state, "APPROVED");
  } finally {
    await demo.stop();
  }
  await assert.rejects(stat(demo.dataDir), { code: "ENOENT" });
});
