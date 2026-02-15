import test from "node:test";
import assert from "node:assert/strict";
import { pruneLocalOperationCaches } from "../trace-api/local-ops-cache.mjs";

function op(name, createTime) {
  return {
    name,
    done: true,
    metadata: {
      createTime,
      updateTime: createTime,
    },
  };
}

test("prunes expired operations and stale request index entries", () => {
  const localOperations = new Map([
    ["op-old", op("op-old", "2026-01-01T00:00:00.000Z")],
    ["op-new", op("op-new", "2026-01-01T00:01:20.000Z")],
  ]);
  const localRequestIndex = new Map([
    ["req-old", "op-old"],
    ["req-new", "op-new"],
    ["req-stale", "op-missing"],
  ]);

  pruneLocalOperationCaches(localOperations, localRequestIndex, {
    nowMs: Date.parse("2026-01-01T00:02:00.000Z"),
    ttlMs: 60_000,
    maxEntries: 100,
  });

  assert.equal(localOperations.has("op-old"), false);
  assert.equal(localOperations.has("op-new"), true);
  assert.equal(localRequestIndex.has("req-old"), false);
  assert.equal(localRequestIndex.has("req-stale"), false);
  assert.equal(localRequestIndex.has("req-new"), true);
});

test("caps local operations to max entries by evicting oldest createTime", () => {
  const localOperations = new Map([
    ["op-1", op("op-1", "2026-01-01T00:00:01.000Z")],
    ["op-2", op("op-2", "2026-01-01T00:00:02.000Z")],
    ["op-3", op("op-3", "2026-01-01T00:00:03.000Z")],
  ]);
  const localRequestIndex = new Map([
    ["req-1", "op-1"],
    ["req-2", "op-2"],
    ["req-3", "op-3"],
  ]);

  pruneLocalOperationCaches(localOperations, localRequestIndex, {
    nowMs: Date.parse("2026-01-01T00:00:04.000Z"),
    ttlMs: 60_000,
    maxEntries: 2,
  });

  assert.equal(localOperations.size, 2);
  assert.equal(localOperations.has("op-1"), false);
  assert.equal(localOperations.has("op-2"), true);
  assert.equal(localOperations.has("op-3"), true);
  assert.equal(localRequestIndex.has("req-1"), false);
  assert.equal(localRequestIndex.has("req-2"), true);
  assert.equal(localRequestIndex.has("req-3"), true);
});
