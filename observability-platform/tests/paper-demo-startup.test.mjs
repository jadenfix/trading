import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, readdir, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import { startPaperDemo } from "../paper-demo.mjs";
import { getFreePort } from "./helpers/process-test-utils.mjs";

async function tempRoot(t) {
  const root = await mkdtemp(path.join(os.tmpdir(), "paper-startup-test-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

async function assertStopped(port) {
  await assert.rejects(fetch(`http://127.0.0.1:${port}/health`, { signal: AbortSignal.timeout(500) }));
}

test("paper startup cleans temporary state when port allocation fails", async (t) => {
  const root = await tempRoot(t);
  await assert.rejects(startPaperDemo({ tempRoot: root, allocatePort: async () => {
    throw new Error("injected port allocation failure");
  } }), /injected port allocation failure/);
  assert.deepEqual(await readdir(root), []);
});

test("paper startup cancellation stops an already-started broker and removes state", async (t) => {
  const root = await tempRoot(t);
  const controller = new AbortController();
  const brokerPort = await getFreePort();
  let allocations = 0;
  await assert.rejects(startPaperDemo({ tempRoot: root, signal: controller.signal,
    allocatePort: async () => {
      if (++allocations === 2) {
        // The second allocation occurs only after the broker's exact-run
        // health check passes, so cancellation cannot precede child startup.
        controller.abort();
      }
      return brokerPort;
    },
  }), { name: "AbortError" });
  assert.equal(allocations, 2);
  await assertStopped(brokerPort);
  assert.deepEqual(await readdir(root), []);
});

test("paper startup rejects a pre-aborted signal without leaving state", async (t) => {
  const root = await tempRoot(t);
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(startPaperDemo({ tempRoot: root, signal: controller.signal }), { name: "AbortError" });
  assert.deepEqual(await readdir(root), []);
});

test("CLI SIGTERM during startup cleans its running broker", async (t) => {
  const root = await tempRoot(t);
  const launcher = fileURLToPath(new URL("../paper-demo.mjs", import.meta.url));
  // Delay only the allocator's second completion, after the real broker is
  // healthy. Send the OS signal to the actual CLI, not a test-only handler.
  const bootstrap = `
    import net from 'node:net';
    import { syncBuiltinESMExports } from 'node:module';
    const create = net.createServer;
    let allocations = 0, brokerPort;
    net.createServer = (...args) => {
      const server = create(...args);
      const listen = server.listen;
      server.listen = (...params) => {
        const ready = params.pop();
        return listen.call(server, ...params, () => {
          if (++allocations === 1) brokerPort = server.address().port;
          if (allocations === 2) {
            console.log('INTERRUPT_BROKER_PORT=' + brokerPort);
            process.kill(process.pid, 'SIGTERM');
            setTimeout(ready, 50);
          } else ready();
        });
      };
      return server;
    };
    syncBuiltinESMExports();
    process.argv[1] = ${JSON.stringify(launcher)};
    await import(${JSON.stringify(new URL("../paper-demo.mjs", import.meta.url).href)});
  `;
  const { stdout, stderr } = await promisify(execFile)(process.execPath,
    ["--input-type=module", "-e", bootstrap], {
      env: { ...process.env, TMPDIR: root }, timeout: 5000,
    });
  assert.equal(stderr, "");
  const port = Number(stdout.match(/INTERRUPT_BROKER_PORT=(\d+)/)?.[1]);
  assert.ok(port > 0);
  await assertStopped(port);
  assert.deepEqual(await readdir(root), []);
});

test("paper startup retries duplicate allocations and gives each role its own port", async (t) => {
  const root = await tempRoot(t);
  const brokerPort = await getFreePort();
  let allocations = 0;
  const demo = await startPaperDemo({ tempRoot: root, allocatePort: async () => {
    return ++allocations <= 2 ? brokerPort : getFreePort();
  } });
  try {
    assert.equal(allocations, 3);
    assert.notEqual(Number(new URL(demo.dashboardUrl).port), brokerPort);
    assert.equal((await (await fetch(demo.dashboardUrl + "/health")).json()).service, "trace-api");
    assert.equal((await (await fetch(`http://127.0.0.1:${brokerPort}/health`)).json()).service, "temporal-broker");
  } finally {
    await demo.stop();
  }
  await assertStopped(brokerPort);
  assert.deepEqual(await readdir(root), []);
});

test("paper startup bounds duplicate-port retries and cleans the running broker", async (t) => {
  const root = await tempRoot(t);
  const brokerPort = await getFreePort();
  let allocations = 0;
  await assert.rejects(startPaperDemo({ tempRoot: root, allocatePort: async () => {
    allocations++;
    return brokerPort;
  } }), /distinct paper demo ports/);
  assert.equal(allocations, 9); // One broker allocation plus eight API attempts.
  await assertStopped(brokerPort);
  assert.deepEqual(await readdir(root), []);
});

test("paper startup rejects a successful health response from the wrong service", async (t) => {
  const root = await tempRoot(t);
  const impostor = createServer((req, res) => {
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify({ ok: true, service: "trace-api" }));
  });
  await new Promise((resolve) => impostor.listen(0, "127.0.0.1", resolve));
  t.after(() => new Promise((resolve) => impostor.close(resolve)));
  const port = impostor.address().port;
  await assert.rejects(startPaperDemo({ tempRoot: root, allocatePort: async () => port }),
    /Wrong service or run|broker.mjs exited/);
  assert.deepEqual(await readdir(root), []);
  // Cleanup must kill only the spawned child, not an unrelated port owner.
  assert.equal((await fetch(`http://127.0.0.1:${port}/health`)).status, 200);
});
