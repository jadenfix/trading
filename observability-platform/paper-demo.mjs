#!/usr/bin/env node
// An isolated fixture demo: real dashboard/control API, no exchange or worker.
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";

const root = path.dirname(fileURLToPath(import.meta.url));
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function freePort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const port = server.address().port;
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  return port;
}

export async function startPaperDemo({ signal, allocatePort = freePort, tempRoot = os.tmpdir() } = {}) {
  const dataDir = await mkdtemp(path.join(tempRoot, "trading-paper-demo-"));
  const children = [];
  let controlToken;
  let brokerUrl;
  let dashboardUrl;
  const stateFile = path.join(dataDir, "broker-state.json");
  const tradesDir = path.join(dataDir, "TRADES");
  const env = {
    ...process.env,
    BROKER_HOST: "127.0.0.1", TRACE_API_HOST: "127.0.0.1",
    BROKER_STATE_FILE: stateFile,
    BROKER_AUDIT_FILE: path.join(dataDir, "broker-audit.jsonl"),
    OBS_CONTROL_AUDIT_FILE: path.join(dataDir, "control-audit.jsonl"),
    TRACE_API_DEV_STATE_DIR: path.join(dataDir, "pids"),
    OBS_PROJECT: "paper-demo",
    OBS_LOCATION: "local", TRADES_DIR: tradesDir,
    OBS_PAPER_DEMO: "1",
  };

  function checkAbort() { signal?.throwIfAborted(); }

  async function distinctPort(other) {
    for (let attempt = 0; attempt < 8; attempt++) {
      checkAbort();
      const port = await allocatePort();
      checkAbort();
      if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid demo port");
      if (port !== other) return port;
    }
    throw new Error("Could not allocate distinct paper demo ports");
  }

  async function stop() {
    for (const child of children) {
      if (!child.pid || child.exitCode !== null || child.signalCode !== null) continue;
      child.kill("SIGTERM");
      for (let i = 0; i < 20 && child.exitCode === null && child.signalCode === null; i++) await pause(50);
      if (child.exitCode === null && child.signalCode === null) {
        const exited = new Promise((resolve) => child.once("exit", resolve));
        child.kill("SIGKILL");
        await exited;
      }
    }
    await rm(dataDir, { recursive: true, force: true });
  }

  async function launch(script, url, service, stateField) {
    checkAbort();
    const child = spawn(process.execPath, [path.join(root, script)], { env, stdio: ["ignore", "ignore", "pipe"] });
    children.push(child);
    let failure = null;
    let logs = "";
    child.once("error", (error) => { failure = error; });
    child.stderr.on("data", (chunk) => { logs = (logs + chunk).slice(-4096); });
    for (let i = 0; i < 100; i++) {
      if (failure || child.exitCode !== null) throw failure || new Error(`${script} exited: ${logs}`);
      checkAbort();
      let response;
      try {
        response = await fetch(`${url}/health`, {
          signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(200)]) : AbortSignal.timeout(200),
        });
      } catch { checkAbort(); }
      if (response?.ok) {
        const health = await response.json();
        checkAbort();
        if (health.service !== service || health[stateField] !== stateFile) {
          throw new Error(`Wrong service or run at ${url}: expected ${service}`);
        }
        if (failure || child.exitCode !== null) throw failure || new Error(`${script} exited: ${logs}`);
        return;
      }
      await sleep(50, undefined, { signal });
    }

    throw new Error(`${script} failed to start: ${logs}`);
  }

  try {
    checkAbort();
    controlToken = randomBytes(24).toString("hex");
    env.OBS_CONTROL_TOKEN = controlToken;
    const brokerPort = await distinctPort();
    brokerUrl = `http://127.0.0.1:${brokerPort}`;
    env.BROKER_PORT = String(brokerPort);
    env.BROKER_BASE_URL = brokerUrl;
    await mkdir(path.join(tradesDir, "paper-weather"), { recursive: true });
    const ts = new Date().toISOString();
    const fixtureEvents = [
      { ts, kind: "strategy_cycle_start", trace_id: "paper-filled", mode: "paper", note: "Synthetic weather scenario" },
      { ts, kind: "risk_check", trace_id: "paper-filled", mode: "paper", approved: true, max_loss_cents: 2500 },
      { ts, kind: "order_placed", trace_id: "paper-filled", mode: "paper", ticker: "PAPER-SF-TEMP", side: "yes", action: "buy", count: 10, price_cents: 42, note: "Fixture fill; no exchange order" },
    ];
    await writeFile(path.join(tradesDir, "paper-weather", "trades-demo.jsonl"), fixtureEvents.map((event) => JSON.stringify(event)).join("\n") + "\n");
    await launch("temporal-broker/broker.mjs", brokerUrl, "temporal-broker", "state_file");
    // Allocate while the broker owns its port; injected/custom allocators must
    // also return a distinct API port, with a bounded retry count.
    const apiPort = await distinctPort(brokerPort);
    dashboardUrl = `http://127.0.0.1:${apiPort}`;
    env.TRACE_API_PORT = String(apiPort);
    env.TEMPORAL_UI_URL = dashboardUrl;
    for (const workflowId of ["paper-review", "paper-cancel"]) {
      const response = await fetch(`${brokerUrl}/workflows/register`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ workflow_id: workflowId, trace_id: workflowId, source_bot: "paper-research", mode: "paper", status: "awaiting_approval", requires_approval: true }),
        signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(2000)]) : AbortSignal.timeout(2000),
      });
      if (!response.ok) throw new Error(`Could not seed ${workflowId}: HTTP ${response.status}`);
    }
    await launch("trace-api/server.mjs", dashboardUrl, "trace-api", "broker_state_file");
    checkAbort();
    return { dashboardUrl, controlToken, dataDir, stop };
  } catch (error) {
    await stop();
    throw error;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const controller = new AbortController();
  let demo;
  let stopping = false;
  async function shutdown() {
    if (stopping) return;
    stopping = true;
    controller.abort();
    if (demo) {
      await demo.stop();
      process.exit(0);
    }
  }
  // Register before any asynchronous setup so startup interruption follows
  // the same cleanup path as failure after either child has been spawned.
  for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, shutdown);
  try {
    demo = await startPaperDemo({ signal: controller.signal });
    console.log("PAPER DEMO — synthetic fixtures; no exchange, model, or trade worker.");
    console.log(`Dashboard: ${demo.dashboardUrl}`);
    console.log(`Paste this local capability into Control Token: ${demo.controlToken}`);
    console.log("Review paper-review, approve it, then cancel paper-cancel. Ctrl+C cleans up.");
  } catch (error) {
    if (!controller.signal.aborted) {
      console.error(error);
      process.exitCode = 1;
    }
  }
}
