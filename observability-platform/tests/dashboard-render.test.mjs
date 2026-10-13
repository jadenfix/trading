import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import vm from "node:vm";

// Run the shipped browser script with inert DOM nodes and no network or timer
// startup. This exercises the same markup inserted by the workflow dashboard.
const source = await readFile(new URL("../dashboard/app.js", import.meta.url), "utf8");
const startup = "await fullRefresh();\nsetupAutoRefresh();";
assert.ok(source.trimEnd().endsWith(startup));
const context = vm.createContext({
  window: { localStorage: { getItem: () => null } },
  document: { getElementById: () => ({ addEventListener() {} }) },
});
vm.runInContext(source.trimEnd().slice(0, -startup.length), context);

test("dashboard status chips retain known-state styling", () => {
  assert.equal(context.stateChip("awaiting_approval"),
    '<span class="status-chip status-awaiting_approval">AWAITING_APPROVAL</span>');
  assert.equal(context.runtimeChip("running"),
    '<span class="runtime-chip runtime-running">RUNNING</span>');
});

test("workflow and runtime statuses cannot create HTML attributes or elements", () => {
  const payload = 'FAILED" onclick="alert(1)"><img src=x onerror=alert(2)>';
  for (const render of [context.stateChip, context.runtimeChip]) {
    const markup = render(payload);
    // Only the span's intended class attribute and text node may be emitted.
    assert.match(markup, /^<span class="[^\"]*">[^<>]*<\/span>$/);
    assert.ok(markup.includes("&quot;"));
    assert.ok(markup.includes("&lt;img"));
  }
});
