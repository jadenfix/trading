import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer } from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const repoRoot = path.resolve(__dirname, "..", "..", "..");

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function getFreePort(host = "127.0.0.1") {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, host, () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close((error) => {
        if (error) {
          reject(error);
          return;
        }
        resolve(port);
      });
    });
  });
}

export async function canBindLocalhost(host = "127.0.0.1") {
  try {
    await getFreePort(host);
    return true;
  } catch (error) {
    if (error && typeof error === "object" && (error.code === "EPERM" || error.code === "EACCES")) {
      return false;
    }
    throw error;
  }
}

export async function createTempDir(prefix = "obs-tests-") {
  return mkdtemp(path.join(os.tmpdir(), prefix));
}

export async function cleanupTempDir(tempDir) {
  if (!tempDir) {
    return;
  }
  await rm(tempDir, { recursive: true, force: true });
}

export async function waitForHttp(url, timeoutMs = 8_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;

  while (Date.now() < deadline) {
    try {
      const response = await fetch(url, { cache: "no-store" });
      if (response.ok) {
        return;
      }
      lastError = new Error(`health not ready: ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await sleep(80);
  }

  throw new Error(`Timed out waiting for ${url}: ${lastError instanceof Error ? lastError.message : String(lastError)}`);
}

export async function startNodeService(scriptRelativePath, env, healthUrl) {
  const scriptPath = path.join(repoRoot, scriptRelativePath);
  const stdout = [];
  const stderr = [];

  const proc = spawn(process.execPath, [scriptPath], {
    cwd: repoRoot,
    env: {
      ...process.env,
      ...env,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });

  proc.stdout.on("data", (chunk) => stdout.push(String(chunk)));
  proc.stderr.on("data", (chunk) => stderr.push(String(chunk)));

  const onExit = new Promise((resolve) => {
    proc.once("exit", (code, signal) => resolve({ code, signal }));
  });

  try {
    await waitForHttp(healthUrl, 10_000);
  } catch (error) {
    const exited = await Promise.race([onExit, sleep(200)]);
    const logs = `${stdout.join("")}\n${stderr.join("")}`.trim();
    if (exited && typeof exited === "object") {
      throw new Error(
        `Service ${scriptRelativePath} exited early (code=${exited.code}, signal=${exited.signal}). Logs:\n${logs}`,
      );
    }
    throw new Error(
      `Service ${scriptRelativePath} failed to become healthy: ${error instanceof Error ? error.message : String(error)}\n${logs}`,
    );
  }

  async function stop() {
    if (proc.exitCode !== null) {
      return;
    }
    proc.kill("SIGTERM");
    const exited = await Promise.race([onExit, sleep(2_000)]);
    if (!exited || typeof exited !== "object") {
      proc.kill("SIGKILL");
      await onExit;
    }
  }

  return {
    proc,
    stop,
    logs: () => `${stdout.join("")}\n${stderr.join("")}`.trim(),
  };
}

export async function jsonRequest(baseUrl, method, pathname, body, headers = {}) {
  const response = await fetch(`${baseUrl}${pathname}`, {
    method,
    headers: {
      "content-type": "application/json",
      ...headers,
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });

  let payload = null;
  try {
    payload = await response.json();
  } catch {
    payload = null;
  }

  return {
    status: response.status,
    headers: response.headers,
    payload,
  };
}
