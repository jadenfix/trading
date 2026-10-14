#!/usr/bin/env node
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync, spawnSync } from "node:child_process";
import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "../..");
const receiptPath = path.join(repoRoot, "observability-platform", "docs", "demo", "capture.json");

const expectedSourcePaths = [
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

const expectedArtifacts = {
  "observability-platform/docs/demo/paper-control-plane.gif": {
    codec: "gif",
    width: 720,
    height: 450,
  },
  "observability-platform/docs/demo/paper-control-plane.mp4": {
    codec: "h264",
    width: 1440,
    height: 900,
  },
  "observability-platform/docs/demo/poster.png": {
    codec: "png",
    width: 1440,
    height: 900,
  },
};

function command(program, args) {
  return execFileSync(program, args, { cwd: repoRoot, encoding: "utf8" }).trim();
}

function assertReachableSource(sourceCommit) {
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

async function main() {
  const receipt = JSON.parse(await readFile(receiptPath, "utf8"));
  assert.equal(receipt.schema, 1);
  const sourceCommit = command("git", ["rev-parse", `${receipt.source_commit}^{commit}`]);
  assert.equal(sourceCommit, receipt.source_commit, "receipt source commit is not canonical");
  assertReachableSource(sourceCommit);
  assert.equal(
    command("git", ["rev-parse", `${sourceCommit}^{tree}`]),
    receipt.source_tree,
    "source tree mismatch",
  );

  assert.deepEqual(Object.keys(receipt.source_blobs).sort(), [...expectedSourcePaths].sort());
  for (const relative of expectedSourcePaths) {
    const expectedBlob = receipt.source_blobs[relative];
    assert.equal(
      command("git", ["rev-parse", `${sourceCommit}:${relative}`]),
      expectedBlob,
      `receipt blob mismatch: ${relative}`,
    );
    assert.equal(
      command("git", ["hash-object", relative]),
      expectedBlob,
      `capture input drift: ${relative}`,
    );
  }

  assert.deepEqual(Object.keys(receipt.artifacts).sort(), Object.keys(expectedArtifacts).sort());
  for (const [relative, expectation] of Object.entries(expectedArtifacts)) {
    const file = path.join(repoRoot, relative);
    const recorded = receipt.artifacts[relative];
    const info = await stat(file);
    assert.equal(info.size, recorded.bytes, `artifact size mismatch: ${relative}`);
    assert.equal(await sha256(file), recorded.sha256, `artifact SHA-256 mismatch: ${relative}`);
    const actual = mediaMetadata(file);
    assert.deepEqual(actual, {
      codec: recorded.codec,
      width: recorded.width,
      height: recorded.height,
      pixel_format: recorded.pixel_format,
      average_frame_rate: recorded.average_frame_rate,
      frames: recorded.frames,
      duration_seconds: recorded.duration_seconds,
    }, `artifact metadata mismatch: ${relative}`);
    assert.equal(actual.codec, expectation.codec, `unexpected codec: ${relative}`);
    assert.equal(actual.width, expectation.width, `unexpected width: ${relative}`);
    assert.equal(actual.height, expectation.height, `unexpected height: ${relative}`);
    if (relative.endsWith(".gif") || relative.endsWith(".mp4")) {
      assert.ok(actual.duration_seconds >= 8 && actual.duration_seconds <= 60, `unexpected duration: ${relative}`);
      assert.ok(actual.frames > 1, `expected animated media: ${relative}`);
    }
    execFileSync(
      "ffmpeg",
      ["-v", "error", "-xerror", "-i", file, "-map", "0:v:0", "-f", "null", "-"],
      { stdio: "inherit" },
    );
  }

  assert.deepEqual(receipt.capture?.viewport, { width: 1440, height: 900 });
  assert.equal(receipt.capture?.external_http_requests, 0);
  assert.equal(receipt.observed_workflow?.mode, "synthetic-paper-control-plane");
  const claims = JSON.stringify(receipt.limitations || []).toLowerCase();
  for (const phrase of ["no live order", "no trading worker", "no model or external provider", "no profitability"]) {
    assert.ok(claims.includes(phrase), `missing truthful limitation: ${phrase}`);
  }

  console.log(
    `MEDIA_VERIFY_OK source=${sourceCommit} source_blobs=${expectedSourcePaths.length} ` +
      `artifacts=${Object.keys(expectedArtifacts).length} full_decode=gif,mp4,png ` +
      "synthetic=true live_orders=false worker=false providers=false",
  );
}

main().catch((error) => {
  console.error(error?.stack || error);
  process.exitCode = 1;
});
