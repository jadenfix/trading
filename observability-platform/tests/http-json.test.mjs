import test from "node:test";
import assert from "node:assert/strict";
import { readLimitedJsonBody } from "../shared/http-json.mjs";

function makeRequest(chunks) {
  return {
    async *[Symbol.asyncIterator]() {
      for (const chunk of chunks) {
        yield Buffer.from(chunk);
      }
    },
  };
}

test("readLimitedJsonBody returns empty object for empty body", async () => {
  const body = await readLimitedJsonBody(makeRequest([]), 1024);
  assert.deepEqual(body, {});
});

test("readLimitedJsonBody parses valid JSON", async () => {
  const body = await readLimitedJsonBody(makeRequest(['{"ok":true,"count":2}']), 1024);
  assert.deepEqual(body, { ok: true, count: 2 });
});

test("readLimitedJsonBody throws on oversized body", async () => {
  await assert.rejects(
    () => readLimitedJsonBody(makeRequest(['{"data":"1234567890"}']), 8),
    (error) => error && error.statusCode === 413 && error.googleStatus === "INVALID_ARGUMENT",
  );
});

test("readLimitedJsonBody throws SyntaxError on invalid JSON", async () => {
  await assert.rejects(() => readLimitedJsonBody(makeRequest(["{invalid"]), 1024), SyntaxError);
});
