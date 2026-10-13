#!/usr/bin/env node
// An isolated fixture demo: real dashboard/control API, no exchange or worker.
import { spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

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

export async function startPaperDemo() {
  const dataDir = await mkdtemp(path.join(os.tmpdir(), "trading-paper-demo-"));
  const children = [];
  const controlToken = randomBytes(24).toString("hex");
  const brokerPort = await freePort();
  const apiPort = await freePort();
  const brokerUrl = `http://127.0.0.1:${brokerPort}`;
  const dashboardUrl = `http://127.0.0.1:${apiPort}`;
  const stateFile = path.join(dataDir, "broker-state.json");
  const tradesDir = path.join(dataDir, "TRADES");
  const env = {
    ...process.env,
    BROKER_HOST: "127.0.0.1", BROKER_PORT: String(brokerPort),
    TRACE_API_HOST: "127.0.0.1", TRACE_API_PORT: String(apiPort),
    BROKER_BASE_URL: brokerUrl, BROKER_STATE_FILE: stateFile,
    BROKER_AUDIT_FILE: path.join(dataDir, "broker-audit.jsonl"),
    OBS_CONTROL_AUDIT_FILE: path.join(dataDir, "control-audit.jsonl"),
    TRACE_API_DEV_STATE_DIR: path.join(dataDir, "pids"),
    OBS_CONTROL_TOKEN: controlToken, OBS_PROJECT: "paper-demo",
    OBS_LOCATION: "local", TRADES_DIR: tradesDir,
    OBS_PAPER_DEMO: "1",
    TEMPORAL_UI_URL: dashboardUrl,
  };

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

  async function launch(script, url) {
    const child = spawn(process.execPath, [path.join(root, script)], { env, stdio: ["ignore", "ignore", "pipe"] });
    children.push(child);
    let failure = null;
    let logs = "";
    child.once("error", (error) => { failure = error; });
    child.stderr.on("data", (chunk) => { logs = (logs + chunk).slice(-4096); });
    for (let i = 0; i < 100; i++) {
      if (failure || child.exitCode !== null) throw failure || new Error(`${script} exited: ${logs}`);
      try {
        const response = await fetch(`${url}/health`, { signal: AbortSignal.timeout(200) });
        if (response.ok) return;
      } catch {}
      await pause(50);
    }
    throw new Error(`${script} failed to start: ${logs}`);
  }

  try {
    await mkdir(path.join(tradesDir, "paper-weather"), { recursive: true });
    const ts = new Date().toISOString();
    const fixtureEvents = [
      { ts, kind: "strategy_cycle_start", trace_id: "paper-filled", mode: "paper", note: "Synthetic weather scenario" },
      { ts, kind: "risk_check", trace_id: "paper-filled", mode: "paper", approved: true, max_loss_cents: 2500 },
      { ts, kind: "order_placed", trace_id: "paper-filled", mode: "paper", ticker: "PAPER-SF-TEMP", side: "yes", action: "buy", count: 10, price_cents: 42, note: "Fixture fill; no exchange order" },
    ];
    await writeFile(path.join(tradesDir, "paper-weather", "trades-demo.jsonl"), fixtureEvents.map((event) => JSON.stringify(event)).join("\n") + "\n");
    await launch("temporal-broker/broker.mjs", brokerUrl);
    for (const workflowId of ["paper-review", "paper-cancel"]) {
      const response = await fetch(`${brokerUrl}/workflows/register`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ workflow_id: workflowId, trace_id: workflowId, source_bot: "paper-research", mode: "paper", status: "awaiting_approval", requires_approval: true }),
        signal: AbortSignal.timeout(2000),
      });
      if (!response.ok) throw new Error(`Could not seed ${workflowId}: HTTP ${response.status}`);
    }
    await launch("trace-api/server.mjs", dashboardUrl);
    return { dashboardUrl, controlToken, dataDir, stop };
  } catch (error) {
    await stop();
    throw error;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const demo = await startPaperDemo();
  console.log("PAPER DEMO — synthetic fixtures; no exchange, model, or trade worker.");
  console.log(`Dashboard: ${demo.dashboardUrl}`);
  console.log(`Paste this local capability into Control Token: ${demo.controlToken}`);
  console.log("Review paper-review, approve it, then cancel paper-cancel. Ctrl+C cleans up.");
  let stopping = false;
  for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, async () => {
    if (stopping) return;
    stopping = true;
    await demo.stop();
    process.exit(0);
  });
}
