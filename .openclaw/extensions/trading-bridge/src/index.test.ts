import assert from "node:assert/strict";
import test from "node:test";

import { buildCommand, capabilityCompatibilityChecks, parseCapabilities } from "./index.js";

test("parseCapabilities accepts valid payload", () => {
  const parsed = parseCapabilities({
    ok: true,
    capabilities: {
      protocol_version: 1,
      status_schema_version: 2,
      command_kinds_supported: [
        "Control.Capabilities",
        "Control.Ping",
        "Control.Start",
        "Control.Stop",
        "Engine.Status",
        "Engine.Pause",
        "Engine.Resume",
        "Engine.KillSwitch",
        "Risk.Status",
        "Risk.Override",
        "Strategy.List",
        "Strategy.Enable",
        "Strategy.Disable",
        "Strategy.UploadCandidate",
        "Strategy.PromoteCandidate",
      ],
      daemon_build: {
        name: "trading_daemon",
        version: "0.1.0",
        git_sha: null,
      },
    },
  });

  assert.equal(parsed.error, null);
  assert.equal(parsed.capabilities?.protocol_version, 1);
  assert.equal(parsed.capabilities?.status_schema_version, 2);
});

test("parseCapabilities rejects malformed payload", () => {
  const parsed = parseCapabilities({
    ok: true,
    capabilities: {
      protocol_version: 1,
      status_schema_version: "bad",
      command_kinds_supported: [],
      daemon_build: {
        name: "trading_daemon",
        version: "0.1.0",
        git_sha: null,
      },
    },
  });

  assert.equal(parsed.capabilities, null);
  assert.match(parsed.error ?? "", /status_schema_version/);
});

test("capabilityCompatibilityChecks flags missing commands", () => {
  const checks = capabilityCompatibilityChecks({
    protocol_version: 1,
    status_schema_version: 2,
    command_kinds_supported: ["Control.Ping", "Engine.Status"],
    daemon_build: {
      name: "trading_daemon",
      version: "0.1.0",
      git_sha: null,
    },
  });

  const commandCheck = checks.find((check) => check.name === "command_surface_compat");
  assert.ok(commandCheck);
  assert.equal(commandCheck?.passed, false);
  assert.match(commandCheck?.details ?? "", /missing command kinds/);
});

test("buildCommand maps risk reset kill switch", () => {
  const command = buildCommand({
    action: "risk_reset_kill_switch",
  });

  assert.equal(command.kind, "Risk.Override");
  assert.deepEqual(command.payload, { action: "reset_kill_switch" });
});

test("buildCommand enforces strategy_id for strategy_enable", () => {
  assert.throws(() => {
    buildCommand({
      action: "strategy_enable",
    });
  }, /strategy_id/);
});
