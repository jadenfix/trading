#!/usr/bin/env node
import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { createReadStream } from "node:fs";
import { rename, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");
const demoDir = path.join(repoRoot, "observability-platform", "docs", "demo");

const sourcePaths = [
  ".github/workflows/non-agent-tests.yml",
  "trading-cli",
  "observability-platform/paper-demo.mjs",
  "observability-platform/shared/http-json.mjs",
  "observability-platform/temporal-broker/broker.mjs",
  "observability-platform/trace-api/config-response.mjs",
  "observability-platform/trace-api/local-ops-cache.mjs",
  "observability-platform/trace-api/server.mjs",
  "observability-platform/dashboard/index.html",
  "observability-platform/dashboard/app.js",
  "observability-platform/dashboard/styles.css",
  "observability-platform/tests/helpers/process-test-utils.mjs",
  "observability-platform/tests/config-response.test.mjs",
  "observability-platform/tests/dashboard-render.test.mjs",
  "observability-platform/tests/http-json.test.mjs",
  "observability-platform/tests/local-ops-cache.test.mjs",
  "observability-platform/tests/paper-demo-startup.test.mjs",
  "observability-platform/tests/paper-demo.test.mjs",
  "observability-platform/tests/temporal-broker.api.test.mjs",
  "observability-platform/tests/trace-api.api.test.mjs",
  "observability-platform/browser-tests/package.json",
  "observability-platform/browser-tests/package-lock.json",
  "observability-platform/browser-tests/e2e.mjs",
  "observability-platform/browser-tests/capture.mjs",
  "observability-platform/scripts/generate_demo.sh",
  "observability-platform/scripts/build-demo-receipt.mjs",
  "observability-platform/scripts/verify-demo-media.mjs",
];

const artifactPaths = [
  "observability-platform/docs/demo/paper-control-plane.gif",
  "observability-platform/docs/demo/paper-control-plane.mp4",
  "observability-platform/docs/demo/poster.png",
];

function command(program, args) {
  return execFileSync(program, args, { cwd: repoRoot, encoding: "utf8" }).trim();
}

function requireReachableSource(sourceCommit) {
  command("git", ["cat-file", "-e", `${sourceCommit}^{commit}`]);
  const result = spawnSync("git", ["merge-base", "--is-ancestor", sourceCommit, "HEAD"], {
    cwd: repoRoot,
    stdio: "ignore",
  });
  assert.equal(result.status, 0, `source commit is not reachable from HEAD: ${sourceCommit}`);
}

async function sha256(file) {
  const digest = createHash("sha256");
  for await (const chunk of createReadStream(file)) digest.update(chunk);
  return digest.digest("hex");
}

function mediaMetadata(file) {
  const raw = execFileSync(
    "ffprobe",
    [
      "-v",
      "error",
      "-count_frames",
      "-show_entries",
      "format=duration:stream=codec_name,width,height,pix_fmt,avg_frame_rate,nb_read_frames,duration",
      "-of",
      "json",
      file,
    ],
    { encoding: "utf8" },
  );
  const probe = JSON.parse(raw);
  const stream = probe.streams?.find((item) => item.codec_name);
  assert.ok(stream, `no media stream found: ${file}`);
  const duration = stream.duration || probe.format?.duration;
  return {
    codec: stream.codec_name,
    width: stream.width ?? null,
    height: stream.height ?? null,
    pixel_format: stream.pix_fmt ?? null,
    average_frame_rate: stream.avg_frame_rate ?? null,
    frames: /^\d+$/.test(String(stream.nb_read_frames || "")) ? Number(stream.nb_read_frames) : null,
    duration_seconds: duration ? Number(Number(duration).toFixed(3)) : null,
  };
}

function parseSourceArgument() {
  const position = process.argv.indexOf("--source-commit");
  if (position === -1 || !process.argv[position + 1]) {
    throw new Error("Usage: build-demo-receipt.mjs --source-commit <commit>");
  }
  return process.argv[position + 1];
}

async function main() {
  const sourceCommit = command("git", ["rev-parse", `${parseSourceArgument()}^{commit}`]);
  requireReachableSource(sourceCommit);
  const sourceTree = command("git", ["rev-parse", `${sourceCommit}^{tree}`]);
  const sourceBlobs = {};
  for (const relative of sourcePaths) {
    const capturedBlob = command("git", ["rev-parse", `${sourceCommit}:${relative}`]);
    const currentBlob = command("git", ["hash-object", relative]);
    assert.equal(currentBlob, capturedBlob, `capture input differs from source commit: ${relative}`);
    sourceBlobs[relative] = capturedBlob;
  }

  const artifacts = {};
  for (const relative of artifactPaths) {
    const file = path.join(repoRoot, relative);
    const info = await stat(file);
    artifacts[relative] = {
      sha256: await sha256(file),
      bytes: info.size,
      ...mediaMetadata(file),
    };
  }

  const receipt = {
    schema: 1,
    generated_at: new Date().toISOString(),
    source_commit: sourceCommit,
    source_tree: sourceTree,
    source_blobs: sourceBlobs,
    artifacts,
    capture: {
      viewport: { width: 1440, height: 900 },
      services: ["actual temporal-broker subprocess", "actual trace-api subprocess", "actual dashboard"],
      external_http_requests: 0,
    },
    observed_workflow: {
      mode: "synthetic-paper-control-plane",
      data: "immutable in-process fictional fixture generated for this isolated run",
      interactions: [
        "inspect the synthetic paper fill and its no-exchange-order event",
        "enter the per-run local control capability",
        "approve paper-review through the real authenticated control API",
        "request a soft cancel for paper-cancel without claiming worker completion",
        "reload and verify approval plus soft-cancel-request persistence",
      ],
    },
    limitations: [
      "Synthetic fixtures only; no live order, exchange, market-data, wallet, or real-money action.",
      "No trading worker runs; soft cancellation remains requested and is not claimed as completed.",
      "No model or external provider is called.",
      "No profitability, strategy quality, latency, liquidity, scale, or deployment claim.",
    ],
  };

  const target = path.join(demoDir, "capture.json");
  const temporary = path.join(demoDir, `.capture.${process.pid}.${randomUUID()}.tmp`);
  await writeFile(temporary, `${JSON.stringify(receipt, null, 2)}\n`, { mode: 0o644 });
  await rename(temporary, target);
  console.log(
    `DEMO_RECEIPT_OK source=${sourceCommit} tree=${sourceTree} ` +
      `source_blobs=${sourcePaths.length} artifacts=${artifactPaths.length}`,
  );
}

main().catch((error) => {
  console.error(error?.stack || error);
  process.exitCode = 1;
});
