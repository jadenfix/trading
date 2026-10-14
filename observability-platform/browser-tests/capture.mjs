import assert from "node:assert/strict";
import { constants as fsConstants } from "node:fs";
import { access, mkdir } from "node:fs/promises";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

import { startPaperDemo } from "../paper-demo.mjs";

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.PLAYWRIGHT_PACKAGE || "playwright-core");
const here = path.dirname(fileURLToPath(import.meta.url));
const outputDir = path.resolve(process.argv[2] || path.join(here, "capture-output"));
const timingScale = Number(process.env.DEMO_CAPTURE_TIMING_SCALE || "1");

if (!Number.isFinite(timingScale) || timingScale <= 0 || timingScale > 10) {
  throw new Error("DEMO_CAPTURE_TIMING_SCALE must be greater than 0 and at most 10");
}

async function isExecutable(candidate) {
  if (!candidate) return false;
  try {
    await access(candidate, fsConstants.X_OK);
    return true;
  } catch {
    return false;
  }
}

async function browserExecutable() {
  const candidates = [
    process.env.BROWSER_EXECUTABLE,
    process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH,
    process.env.CHROME_BIN,
    process.env.CHROMIUM_BIN,
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
    "/usr/bin/google-chrome",
    "/usr/bin/google-chrome-stable",
    "/tmp/chromium",
  ];
  for (const candidate of candidates) {
    if (await isExecutable(candidate)) return candidate;
  }
  const bundled = chromium.executablePath();
  if (await isExecutable(bundled)) return bundled;
  throw new Error(
    "No Chromium executable found; set BROWSER_EXECUTABLE or PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH.",
  );
}

async function hold(milliseconds) {
  await new Promise((resolve) => setTimeout(resolve, Math.max(40, milliseconds * timingScale)));
}

async function selectWorkflow(page, workflowId) {
  await page.locator(`#trace-list [data-workflow-id="${workflowId}"]`).click();
  await page.waitForFunction(
    (expected) => document.querySelector("#trace-title")?.textContent === expected,
    workflowId,
  );
}

async function capture() {
  await mkdir(outputDir, { recursive: true });
  const demo = await startPaperDemo();
  let browser;
  let context;
  try {
    browser = await chromium.launch({
      executablePath: await browserExecutable(),
      headless: true,
      args: ["--no-sandbox", "--disable-dev-shm-usage"],
    });
    context = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      recordVideo: { dir: outputDir, size: { width: 1440, height: 900 } },
    });
    const page = await context.newPage();
    const pageErrors = [];
    const externalRequests = [];
    const demoOrigin = new URL(demo.dashboardUrl).origin;

    page.on("pageerror", (error) => pageErrors.push(error.message));
    page.on("console", (message) => {
      if (message.type() === "error") pageErrors.push(`console: ${message.text()}`);
    });
    page.on("request", (request) => {
      const requestUrl = new URL(request.url());
      if (requestUrl.protocol.startsWith("http") && requestUrl.origin !== demoOrigin) {
        externalRequests.push(request.url());
      }
    });
    await page.route("**/*", async (route) => {
      const requestUrl = new URL(route.request().url());
      if (requestUrl.protocol.startsWith("http") && requestUrl.origin !== demoOrigin) {
        await route.abort("blockedbyclient");
        return;
      }
      await route.continue();
    });
    page.on("dialog", (dialog) => dialog.accept());

    await page.goto(demo.dashboardUrl, { waitUntil: "networkidle" });
    await page.locator('#trace-list [data-workflow-id="paper-review"]').waitFor();
    assert.match(await page.locator(".eyebrow").innerText(), /paper demo.*synthetic fixtures.*no live orders/i);
    assert.match(await page.locator(".executed-panel").innerText(), /paper fills.*fixtures/i);
    assert.match(await page.locator(".execution-card").innerText(), /PAPER-SF-TEMP/);
    await hold(1_700);

    await selectWorkflow(page, "paper-filled");
    await page.waitForFunction(() => document.querySelector("#event-timeline")?.textContent.includes("Fixture fill; no exchange order"));
    await hold(1_700);

    await page.locator("#control-token-input").fill(demo.controlToken);
    await page.locator("#control-token-input").dispatchEvent("change");
    await hold(900);

    await selectWorkflow(page, "paper-review");
    await page.waitForFunction(() => document.querySelector("#action-message")?.textContent.includes("Actions ready"));
    await page.waitForFunction(() => !document.querySelector("#execute-btn")?.disabled);
    await hold(1_000);
    const approveResponse = page.waitForResponse(
      (response) => response.url().includes("paper-review:execute") && response.request().method() === "POST",
    );
    await page.locator("#execute-btn").click();
    assert.equal((await approveResponse).status(), 200);
    await page.waitForFunction(() => document.querySelector("#trace-meta")?.textContent.includes("APPROVED"));
    await page.waitForFunction(() => document.querySelector("#event-timeline")?.textContent.includes("execution_approved"));
    const parent = "/v1/projects/paper-demo/locations/local";
    let workflow = await (await fetch(`${demo.dashboardUrl}${parent}/workflows/paper-review`)).json();
    assert.equal(workflow.state, "APPROVED");
    await hold(2_000);

    await selectWorkflow(page, "paper-cancel");
    await page.waitForFunction(() => !document.querySelector("#cancel-btn")?.disabled);
    await hold(1_000);
    const cancelResponse = page.waitForResponse(
      (response) => response.url().includes("paper-cancel:cancel") && response.request().method() === "POST",
    );
    await page.locator("#cancel-btn").click();
    const canceled = await cancelResponse;
    assert.equal(canceled.status(), 200);
    assert.equal((await canceled.json()).response.outcome, "soft_cancel_requested");
    await page.waitForFunction(() => document.querySelector("#trace-meta")?.textContent.includes("soft_requested"));
    workflow = await (await fetch(`${demo.dashboardUrl}${parent}/workflows/paper-cancel`)).json();
    assert.equal(workflow.cancelState, "soft_requested");
    assert.equal(workflow.state, "AWAITING_APPROVAL");
    await hold(2_100);

    await page.reload({ waitUntil: "networkidle" });
    await selectWorkflow(page, "paper-review");
    await page.waitForFunction(() => document.querySelector("#trace-meta")?.textContent.includes("APPROVED"));
    assert.equal(await page.locator("#control-token-input").inputValue(), demo.controlToken);
    await hold(1_700);
    await selectWorkflow(page, "paper-cancel");
    await page.waitForFunction(() => document.querySelector("#trace-meta")?.textContent.includes("soft_requested"));
    await page.screenshot({ path: path.join(outputDir, "poster.png") });
    await hold(1_700);

    assert.deepEqual(pageErrors, []);
    assert.deepEqual(externalRequests, []);
    const video = page.video();
    assert.ok(video, "Playwright did not create a capture video");
    await context.close();
    context = null;
    await video.saveAs(path.join(outputDir, "capture.webm"));
    console.log(
      "CAPTURE_OK viewport=1440x900 synthetic=true live_orders=false worker=false providers=false " +
        "approve=APPROVED soft_cancel=soft_requested reload=persisted external_requests=0 page_errors=0",
    );
  } finally {
    await context?.close().catch(() => {});
    await browser?.close().catch(() => {});
    await demo.stop();
  }
}

capture().catch((error) => {
  console.error(error?.stack || error);
  process.exitCode = 1;
});
