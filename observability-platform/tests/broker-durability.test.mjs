import test from "node:test";
import assert from "node:assert/strict";
import { mkdir, readFile, rename, unlink, writeFile, readdir, stat, symlink } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createConnection } from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createTempDir, cleanupTempDir, getFreePort, jsonRequest, startNodeService } from "./helpers/process-test-utils.mjs";

const script = process.env.BROKER_TEST_SCRIPT || "observability-platform/temporal-broker/broker.mjs";
const repository = fileURLToPath(new URL("../../", import.meta.url));
const parent = "/v1/projects/local/locations/us-central1";

async function fixture(t) {
  const directory = await createTempDir("broker-durability-");
  const stateDirectory = path.join(directory, "state");
  const auditDirectory = path.join(directory, "audit");
  await mkdir(stateDirectory);
  await mkdir(auditDirectory);
  const stateFile = path.join(stateDirectory, "broker.json");
  const auditFile = path.join(auditDirectory, "control.jsonl");
  const port = await getFreePort();
  const url = `http://127.0.0.1:${port}`;
  const environment = { BROKER_HOST: "127.0.0.1", BROKER_PORT: String(port), BROKER_STATE_FILE: stateFile, BROKER_AUDIT_FILE: auditFile, BROKER_BODY_TIMEOUT_MS: "300" };
  let service = await startNodeService(script, environment, `${url}/health`);
  t.after(async () => { await service.stop(); await cleanupTempDir(directory); });
  return {
    directory, stateDirectory, auditDirectory, stateFile, auditFile, url,
    request: (method, route, body) => jsonRequest(url, method, route, body),
    restart: async () => { await service.stop(); service = await startNodeService(script, environment, `${url}/health`); },
  };
}

const register = (f, id) => f.request("POST", "/workflows/register", { workflow_id: id, status: "awaiting_approval", requires_approval: true });

test("successful concurrent registrations survive restart and leave no partial snapshots", async (t) => {
  const f = await fixture(t);
  const responses = await Promise.all(Array.from({ length: 16 }, (_, index) => register(f, `parallel-${index}`)));
  assert(responses.every(response => response.status === 200));
  const snapshot = JSON.parse(await readFile(f.stateFile, "utf8"));
  assert.equal(Object.keys(snapshot.workflows).length, 16);
  assert.equal((await stat(f.stateFile)).mode & 0o777, 0o600);
  assert.deepEqual(await readdir(f.stateDirectory), ["broker.json"]);
  await f.restart();
  for (let index = 0; index < 16; index++) {
    const response = await f.request("GET", `${parent}/workflows/parallel-${index}`);
    assert.equal(response.status, 200);
    assert.equal(response.payload.status, "awaiting_approval");
  }
  assert.equal((await f.request("GET", `${parent}/workflows/toString`)).status, 404);
});

test("incomplete POST cannot block health and closes with a bounded body deadline", async (t) => {
  const f = await fixture(t);
  const socket = createConnection({ host: "127.0.0.1", port: new URL(f.url).port });
  t.after(() => socket.destroy());
  let raw = "";
  socket.on("data", chunk => { raw += chunk; });
  await new Promise((resolve, reject) => { socket.once("connect", resolve); socket.once("error", reject); });
  const closed = new Promise(resolve => socket.once("close", resolve));
  socket.write("POST /workflows/register HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{");
  const health = await fetch(f.url + "/health", { signal: AbortSignal.timeout(2000) });
  assert.equal(health.status, 200);
  await Promise.race([closed, new Promise((_, reject) => setTimeout(() => reject(new Error("Body deadline did not close socket")), 2000))]);
  assert.match(raw, /^HTTP\/1\.1 408/);
  assert.equal((await register(f, "after-body-timeout")).status, 200);
});

test("primitive or array JSON cannot create a workflow", async (t) => {
  const f = await fixture(t);
  for (const payload of [null, [], 42, "not a workflow"]) assert.equal((await f.request("POST", "/workflows/register", payload)).status, 400);
  assert.equal((await f.request("GET", "/health")).payload.workflow_count, 0);
});

test("actual failed state write returns503, blocks uncertain reads/controls, and recovers last snapshot only after restart", async (t) => {
  const f = await fixture(t);
  assert.equal((await register(f, "durable")).status, 200);
  const prior = await readFile(f.stateFile, "utf8");
  const displaced = f.stateDirectory + ".saved";
  await rename(f.stateDirectory, displaced);
  await writeFile(f.stateDirectory, "not a directory");
  assert.equal((await register(f, "lost-write")).status, 503);
  assert.equal((await f.request("GET", "/health")).status, 503);
  assert.equal((await f.request("GET", `${parent}/workflows/lost-write`)).status, 503);
  assert.equal((await f.request("POST", `${parent}/workflows/durable:execute`, { requestId: "after-fault" })).status, 503);
  assert.equal(await readFile(path.join(displaced, "broker.json"), "utf8"), prior);
  await unlink(f.stateDirectory);
  await rename(displaced, f.stateDirectory);
  // Merely restoring the path must not publish the uncommitted memory state.
  assert.equal((await f.request("GET", "/health")).status, 503);
  await f.restart();
  assert.equal((await f.request("GET", `${parent}/workflows/lost-write`)).status, 404);
  assert.equal((await f.request("GET", `${parent}/workflows/durable`)).payload.status, "awaiting_approval");
});

test("actual audit failure cannot persist an approval and requires recovery before retry", async (t) => {
  const f = await fixture(t);
  assert.equal((await register(f, "review")).status, 200);
  const prior = await readFile(f.stateFile, "utf8");
  const displaced = f.auditDirectory + ".saved";
  await rename(f.auditDirectory, displaced);
  await writeFile(f.auditDirectory, "not a directory");
  assert.equal((await f.request("POST", `${parent}/workflows/review:execute`, { requestId: "durable-approval" })).status, 503);
  assert.equal(await readFile(f.stateFile, "utf8"), prior);
  assert.equal((await f.request("GET", `${parent}/workflows/review`)).status, 503);
  await unlink(f.auditDirectory);
  await rename(displaced, f.auditDirectory);
  await f.restart();
  assert.equal((await f.request("GET", `${parent}/workflows/review`)).payload.status, "awaiting_approval");
  const approved = await f.request("POST", `${parent}/workflows/review:execute`, { requestId: "durable-approval" });
  assert.equal(approved.status, 200);
  assert.equal(approved.payload.response.outcome, "execution_approved");
  await f.restart();
  const repeated = await f.request("POST", `${parent}/workflows/review:execute`, { requestId: "durable-approval" });
  assert.equal(repeated.payload.name, approved.payload.name);
  assert.equal((await readFile(f.auditFile, "utf8")).trim().split("\n").length, 1);
});

for (const [label, contents] of [["truncated JSON", '{"workflows":'], ["null root", "null"], ["array map", '{"version":2,"workflows":[]}'], ["unknown version", '{"version":999,"workflows":{}}'], ["null workflow entry", '{"version":2,"workflows":{"bad":null},"operations":{},"request_index":{}}']]) {
  test(`existing ${label} fails startup without overwriting evidence`, async (t) => {
    const directory = await createTempDir("broker-invalid-state-");
    t.after(() => cleanupTempDir(directory));
    const stateFile = path.join(directory, "state.json");
    await writeFile(stateFile, contents);
    const child = spawn(process.execPath, [path.join(repository, script)], { cwd: repository, env: { ...process.env, BROKER_STATE_FILE: stateFile, BROKER_PORT: String(await getFreePort()), BROKER_HOST: "127.0.0.1" }, stdio: ["ignore", "ignore", "pipe"] });
    let stderr = "";
    child.stderr.on("data", chunk => { stderr += chunk; });
    const timer = setTimeout(() => child.kill("SIGKILL"), 3000);
    const code = await new Promise((resolve, reject) => { child.once("exit", resolve); child.once("error", reject); });
    clearTimeout(timer);
    assert.notEqual(code, 0);
    assert.match(stderr, /Refusing to start/);
    assert.equal(await readFile(stateFile, "utf8"), contents);
  });
}

test("overlapping state/audit paths fail before touching existing state", async (t) => {
  const directory = await createTempDir("broker-config-paths-");
  t.after(() => cleanupTempDir(directory));
  const stateFile = path.join(directory, "state.json");
  const content = '{"version":2,"workflows":{},"operations":{},"request_index":{}}';
  await writeFile(stateFile, content);
  const child = spawn(process.execPath, [path.join(repository, script)], { cwd: repository, env: { ...process.env, BROKER_STATE_FILE: stateFile, BROKER_AUDIT_FILE: stateFile }, stdio: ["ignore", "ignore", "pipe"] });
  let stderr = "";
  child.stderr.on("data", chunk => { stderr += chunk; });
  const timer = setTimeout(() => child.kill("SIGKILL"), 3000);
  assert.notEqual(await new Promise(resolve => child.once("exit", resolve)), 0);
  clearTimeout(timer);
  assert.match(stderr, /distinct paths/);
  assert.equal(await readFile(stateFile, "utf8"), content);
});

test("dangling configured storage directory never starts an empty healthy broker", async (t) => {
  const directory = await createTempDir("broker-storage-symlink-");
  t.after(() => cleanupTempDir(directory));
  await symlink(path.join(directory, "absent-target"), path.join(directory, "configured-storage"));
  const child = spawn(process.execPath, [path.join(repository, script)], { cwd: repository, env: { ...process.env, BROKER_STATE_FILE: path.join(directory, "configured-storage", "state.json") }, stdio: ["ignore", "ignore", "pipe"] });
  let stderr = "";
  child.stderr.on("data", chunk => { stderr += chunk; });
  const timer = setTimeout(() => child.kill("SIGKILL"), 3000);
  assert.notEqual(await new Promise(resolve => child.once("exit", resolve)), 0);
  clearTimeout(timer);
  assert.match(stderr, /ENOENT/);
});
