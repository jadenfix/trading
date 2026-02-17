import { randomUUID } from "node:crypto";
import { EventEmitter } from "node:events";
import { createConnection, Socket } from "node:net";

interface Envelope {
  v: number;
  id: string;
  type: string;
  ts_ms: number;
  payload: unknown;
}

interface PendingRequest {
  resolve: (value: Envelope) => void;
  reject: (reason: Error) => void;
  timer: NodeJS.Timeout;
}

type TradingHftAction =
  | "health"
  | "engine_status"
  | "engine_start"
  | "engine_stop"
  | "engine_pause"
  | "engine_resume"
  | "engine_killswitch"
  | "risk_status"
  | "risk_reset_kill_switch"
  | "strategy_list"
  | "strategy_enable"
  | "strategy_disable"
  | "strategy_upload_candidate"
  | "strategy_promote_candidate";

interface TradingHftRequest {
  action: TradingHftAction;
  intent_id?: unknown;
  strategy_id?: unknown;
  source?: unknown;
  code_hash?: unknown;
  requested_canary_notional_cents?: unknown;
  auto?: unknown;
  compile_passed?: unknown;
  replay_passed?: unknown;
  paper_passed?: unknown;
  latency_passed?: unknown;
  risk_passed?: unknown;
}

interface ConfidenceCheck {
  name: string;
  passed: boolean;
  details: string;
}

interface ConfidenceResult {
  passed: boolean;
  checks: ConfidenceCheck[];
}

interface TradingHftResponse {
  ok: boolean;
  action: TradingHftAction;
  intent_id: string;
  confidence: ConfidenceResult;
  data?: unknown;
  error?: {
    message: string;
    code?: string;
  };
  meta: {
    bridge_version: string;
    daemon_protocol_version: number | null;
    daemon_capabilities: DaemonCapabilities | null;
    latency_ms: number;
    socket_path: string;
  };
}

interface DaemonBuildInfo {
  name: string;
  version: string;
  git_sha: string | null;
}

interface DaemonCapabilities {
  protocol_version: number;
  status_schema_version: number;
  command_kinds_supported: string[];
  daemon_build: DaemonBuildInfo;
}

interface ActionContext {
  engineStatus: unknown | null;
  riskStatus: unknown | null;
}

const DEFAULT_SOCKET_PATH = "/var/run/openclaw/trading.sock";
const RECONNECT_DELAY_MS = 3_000;
const REQUEST_TIMEOUT_MS = 5_000;
const CAPABILITY_CACHE_TTL_MS = 10_000;
const MAX_FRAME_LENGTH = 1_048_576; // 1 MiB
const DEFAULT_CANDIDATE_CANARY_NOTIONAL_CENTS = 250;
const MIN_PROTOCOL_VERSION = 1;
const MIN_STATUS_SCHEMA_VERSION = 2;
const CONFIDENCE_PING_MAX_MS = 250;
const BRIDGE_VERSION = "0.2.0";

const HIGH_IMPACT_ACTIONS = new Set<TradingHftAction>([
  "engine_start",
  "engine_resume",
  "strategy_promote_candidate",
  "risk_reset_kill_switch",
]);

const REQUIRED_COMMANDS = [
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
];

class TradingClient extends EventEmitter {
  private readonly socketPath: string;
  private socket: Socket | null = null;
  private buffer = Buffer.alloc(0);
  private reconnectTimer: NodeJS.Timeout | null = null;
  private readonly pending = new Map<string, PendingRequest>();

  public readonly state: {
    connected: boolean;
    lastEnvelope: Envelope | null;
  } = {
    connected: false,
    lastEnvelope: null,
  };

  constructor(socketPath: string) {
    super();
    this.socketPath = socketPath;
    this.connect();
  }

  public dispose(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    this.buffer = Buffer.alloc(0);
    this.rejectAllPending(new Error("Trading client stopped"));

    if (this.socket) {
      this.socket.destroy();
      this.socket = null;
    }

    this.state.connected = false;
    this.removeAllListeners();
  }

  public async request(
    type: string,
    payload: Record<string, unknown>,
    timeoutMs = REQUEST_TIMEOUT_MS,
  ): Promise<Envelope> {
    const envelope = this.createEnvelope(type, payload);

    return new Promise<Envelope>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(envelope.id);
        reject(new Error(`Timed out waiting for ${type} response`));
      }, timeoutMs);

      this.pending.set(envelope.id, {
        resolve,
        reject: (reason: Error) => reject(reason),
        timer,
      });

      try {
        this.sendEnvelope(envelope);
      } catch (error) {
        clearTimeout(timer);
        this.pending.delete(envelope.id);
        reject(new Error(toErrorMessage(error)));
      }
    });
  }

  private connect(): void {
    if (this.socket) {
      return;
    }

    console.log(`Connecting to Trading Daemon at ${this.socketPath}`);
    this.socket = createConnection(this.socketPath);

    this.socket.on("connect", () => {
      console.log("Connected to Trading Daemon");
      this.state.connected = true;
      this.emit("connected");
      if (this.reconnectTimer) {
        clearTimeout(this.reconnectTimer);
        this.reconnectTimer = null;
      }
    });

    this.socket.on("data", (data) => this.handleData(data));

    this.socket.on("close", () => {
      console.log("Disconnected from Trading Daemon");
      this.state.connected = false;
      this.socket = null;
      this.buffer = Buffer.alloc(0);
      this.rejectAllPending(new Error("Trading daemon connection closed"));
      this.emit("disconnected");
      this.scheduleReconnect();
    });

    this.socket.on("error", (error) => {
      console.error("Trading daemon socket error:", error.message);
    });
  }

  private scheduleReconnect(): void {
    if (this.reconnectTimer) {
      return;
    }

    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, RECONNECT_DELAY_MS);
  }

  private handleData(data: Buffer): void {
    this.buffer = Buffer.concat([this.buffer, data]);

    while (this.buffer.length >= 4) {
      const length = this.buffer.readUInt32BE(0);
      if (length > MAX_FRAME_LENGTH) {
        this.buffer = Buffer.alloc(0);
        this.socket?.destroy(new Error(`Trading frame exceeds max size (${length} bytes)`));
        return;
      }
      if (this.buffer.length < 4 + length) {
        break;
      }

      const frame = this.buffer.subarray(4, 4 + length);
      this.buffer = this.buffer.subarray(4 + length);

      let envelope: Envelope;
      try {
        envelope = JSON.parse(frame.toString("utf8")) as Envelope;
      } catch (error) {
        console.error("Failed to parse trading frame:", toErrorMessage(error));
        continue;
      }

      this.state.lastEnvelope = envelope;
      const pending = this.pending.get(envelope.id);
      if (pending) {
        clearTimeout(pending.timer);
        this.pending.delete(envelope.id);
        pending.resolve(envelope);
        continue;
      }

      this.emit("envelope", envelope);
    }
  }

  private createEnvelope(type: string, payload: Record<string, unknown>): Envelope {
    return {
      v: 1,
      id: randomUUID(),
      type,
      ts_ms: Date.now(),
      payload,
    };
  }

  private sendEnvelope(envelope: Envelope): void {
    if (!this.socket || !this.state.connected) {
      throw new Error("Trading daemon socket is not connected");
    }

    const body = Buffer.from(JSON.stringify(envelope), "utf8");
    if (body.length > MAX_FRAME_LENGTH) {
      throw new Error(`Trading frame exceeds max size (${body.length} bytes)`);
    }
    const length = Buffer.alloc(4);
    length.writeUInt32BE(body.length, 0);

    this.socket.write(Buffer.concat([length, body]));
  }

  private rejectAllPending(reason: Error): void {
    for (const [id, pending] of this.pending.entries()) {
      clearTimeout(pending.timer);
      pending.reject(reason);
      this.pending.delete(id);
    }
  }
}

function toErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asString(value: unknown, field: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`Missing required field '${field}'`);
  }
  return value.trim();
}

function asOptionalNonEmptyString(value: unknown, fallback: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    return fallback;
  }
  return value.trim();
}

function asOptionalBoolean(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

function asOptionalNotional(value: unknown, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    return fallback;
  }
  return Math.trunc(value);
}

function asIntentId(value: unknown): string {
  if (typeof value !== "string" || value.trim() === "") {
    return randomUUID();
  }
  return value.trim();
}

function parseCapabilities(payload: unknown): { capabilities: DaemonCapabilities | null; error: string | null } {
  if (!isRecord(payload)) {
    return { capabilities: null, error: "capabilities response payload is not an object" };
  }

  const ok = payload.ok;
  if (ok !== true) {
    const errorText = typeof payload.error === "string" ? payload.error : "capabilities response returned ok=false";
    return { capabilities: null, error: errorText };
  }

  const rawCaps = payload.capabilities;
  if (!isRecord(rawCaps)) {
    return { capabilities: null, error: "capabilities payload missing 'capabilities' object" };
  }

  const protocolVersion = rawCaps.protocol_version;
  const statusSchemaVersion = rawCaps.status_schema_version;
  const commandKindsSupported = rawCaps.command_kinds_supported;
  const daemonBuild = rawCaps.daemon_build;

  if (typeof protocolVersion !== "number" || !Number.isFinite(protocolVersion)) {
    return { capabilities: null, error: "capabilities payload missing numeric protocol_version" };
  }
  if (typeof statusSchemaVersion !== "number" || !Number.isFinite(statusSchemaVersion)) {
    return { capabilities: null, error: "capabilities payload missing numeric status_schema_version" };
  }
  if (!Array.isArray(commandKindsSupported) || !commandKindsSupported.every((item) => typeof item === "string")) {
    return { capabilities: null, error: "capabilities payload missing string[] command_kinds_supported" };
  }
  if (!isRecord(daemonBuild)) {
    return { capabilities: null, error: "capabilities payload missing daemon_build object" };
  }

  const daemonName = daemonBuild.name;
  const daemonVersion = daemonBuild.version;
  const daemonGitSha = daemonBuild.git_sha;

  if (typeof daemonName !== "string" || daemonName.trim() === "") {
    return { capabilities: null, error: "capabilities payload missing daemon_build.name" };
  }
  if (typeof daemonVersion !== "string" || daemonVersion.trim() === "") {
    return { capabilities: null, error: "capabilities payload missing daemon_build.version" };
  }
  if (daemonGitSha !== null && daemonGitSha !== undefined && typeof daemonGitSha !== "string") {
    return { capabilities: null, error: "capabilities payload has invalid daemon_build.git_sha" };
  }

  return {
    capabilities: {
      protocol_version: Math.trunc(protocolVersion),
      status_schema_version: Math.trunc(statusSchemaVersion),
      command_kinds_supported: commandKindsSupported,
      daemon_build: {
        name: daemonName,
        version: daemonVersion,
        git_sha: typeof daemonGitSha === "string" ? daemonGitSha : null,
      },
    },
    error: null,
  };
}

function capabilityCompatibilityChecks(capabilities: DaemonCapabilities): ConfidenceCheck[] {
  const checks: ConfidenceCheck[] = [];

  checks.push({
    name: "protocol_compat",
    passed: capabilities.protocol_version >= MIN_PROTOCOL_VERSION,
    details: `protocol_version=${capabilities.protocol_version}, required>=${MIN_PROTOCOL_VERSION}`,
  });

  checks.push({
    name: "status_schema_compat",
    passed: capabilities.status_schema_version >= MIN_STATUS_SCHEMA_VERSION,
    details: `status_schema_version=${capabilities.status_schema_version}, required>=${MIN_STATUS_SCHEMA_VERSION}`,
  });

  const missingCommands = REQUIRED_COMMANDS.filter((kind) => !capabilities.command_kinds_supported.includes(kind));
  checks.push({
    name: "command_surface_compat",
    passed: missingCommands.length === 0,
    details:
      missingCommands.length === 0
        ? "required command kinds available"
        : `missing command kinds: ${missingCommands.join(", ")}`,
  });

  return checks;
}

function allChecksPassed(checks: ConfidenceCheck[]): boolean {
  return checks.every((check) => check.passed);
}

function extractEngineState(engineStatus: unknown): Record<string, unknown> | null {
  if (!isRecord(engineStatus)) {
    return null;
  }
  const state = engineStatus.state;
  return isRecord(state) ? state : null;
}

function extractRiskState(riskStatus: unknown): Record<string, unknown> | null {
  if (!isRecord(riskStatus)) {
    return null;
  }
  const state = riskStatus.state;
  return isRecord(state) ? state : null;
}

function extractEngineStrategies(engineStatus: unknown): Array<Record<string, unknown>> | null {
  if (!isRecord(engineStatus)) {
    return null;
  }
  const raw = engineStatus.strategies;
  if (!Array.isArray(raw)) {
    return null;
  }
  const list: Array<Record<string, unknown>> = [];
  for (const item of raw) {
    if (isRecord(item)) {
      list.push(item);
    }
  }
  return list;
}

function preconditionCheck(action: TradingHftAction, input: TradingHftRequest, ctx: ActionContext): ConfidenceCheck {
  const engineState = extractEngineState(ctx.engineStatus);
  const riskState = extractRiskState(ctx.riskStatus);
  const strategies = extractEngineStrategies(ctx.engineStatus);

  switch (action) {
    case "engine_start":
    case "engine_resume": {
      const killSwitch = engineState?.kill_switch_engaged;
      const blocked = killSwitch === true;
      return {
        name: "action_precondition",
        passed: !blocked,
        details: blocked ? "kill switch engaged; action blocked" : "kill switch clear",
      };
    }
    case "risk_reset_kill_switch": {
      const killSwitch = riskState?.kill_switch_engaged;
      return {
        name: "action_precondition",
        passed: killSwitch === true,
        details:
          killSwitch === true
            ? "kill switch engaged; reset allowed"
            : "kill switch is not engaged; reset rejected for confidence",
      };
    }
    case "strategy_promote_candidate": {
      const strategyId = asString(input.strategy_id, "strategy_id");
      const known = Array.isArray(strategies)
        ? strategies.some((strategy) => strategy.id === strategyId)
        : false;
      return {
        name: "action_precondition",
        passed: known,
        details: known ? `strategy '${strategyId}' found` : `strategy '${strategyId}' not found in engine snapshot`,
      };
    }
    default:
      return {
        name: "action_precondition",
        passed: true,
        details: "no additional preconditions",
      };
  }
}

function buildCommand(input: TradingHftRequest): { kind: string; payload: Record<string, unknown> } {
  switch (input.action) {
    case "engine_status":
      return { kind: "Engine.Status", payload: {} };
    case "engine_start":
      return { kind: "Control.Start", payload: {} };
    case "engine_stop":
      return { kind: "Control.Stop", payload: {} };
    case "engine_pause":
      return { kind: "Engine.Pause", payload: {} };
    case "engine_resume":
      return { kind: "Engine.Resume", payload: {} };
    case "engine_killswitch":
      return { kind: "Engine.KillSwitch", payload: {} };
    case "risk_status":
      return { kind: "Risk.Status", payload: {} };
    case "risk_reset_kill_switch":
      return {
        kind: "Risk.Override",
        payload: {
          action: "reset_kill_switch",
        },
      };
    case "strategy_list":
      return { kind: "Strategy.List", payload: {} };
    case "strategy_enable":
      return {
        kind: "Strategy.Enable",
        payload: {
          strategy_id: asString(input.strategy_id, "strategy_id"),
        },
      };
    case "strategy_disable":
      return {
        kind: "Strategy.Disable",
        payload: {
          strategy_id: asString(input.strategy_id, "strategy_id"),
        },
      };
    case "strategy_upload_candidate":
      return {
        kind: "Strategy.UploadCandidate",
        payload: {
          strategy_id: asString(input.strategy_id, "strategy_id"),
          source: asOptionalNonEmptyString(input.source, "agent"),
          code_hash: asString(input.code_hash, "code_hash"),
          requested_canary_notional_cents: asOptionalNotional(
            input.requested_canary_notional_cents,
            DEFAULT_CANDIDATE_CANARY_NOTIONAL_CENTS,
          ),
          compile_passed: asOptionalBoolean(input.compile_passed, true),
          replay_passed: asOptionalBoolean(input.replay_passed, true),
          paper_passed: asOptionalBoolean(input.paper_passed, true),
          latency_passed: asOptionalBoolean(input.latency_passed, true),
          risk_passed: asOptionalBoolean(input.risk_passed, true),
        },
      };
    case "strategy_promote_candidate":
      return {
        kind: "Strategy.PromoteCandidate",
        payload: {
          strategy_id: asString(input.strategy_id, "strategy_id"),
          code_hash: asString(input.code_hash, "code_hash"),
          requested_canary_notional_cents: asOptionalNotional(
            input.requested_canary_notional_cents,
            DEFAULT_CANDIDATE_CANARY_NOTIONAL_CENTS,
          ),
          auto: asOptionalBoolean(input.auto, true),
        },
      };
    case "health":
      throw new Error("Action 'health' does not map to a daemon command");
    default:
      throw new Error(`Unsupported action '${String(input.action)}'`);
  }
}

export { buildCommand, capabilityCompatibilityChecks, parseCapabilities };

// OpenClaw extension entry point.
// @ts-ignore
export default function (api: any) {
  const socketPath = api?.config?.socketPath ?? process.env.TRADING_SOCKET_PATH ?? DEFAULT_SOCKET_PATH;
  const client = new TradingClient(socketPath);

  let capabilitiesCache: {
    checkedAtMs: number;
    capabilities: DaemonCapabilities | null;
    error: string | null;
  } = {
    checkedAtMs: 0,
    capabilities: null,
    error: null,
  };

  const invalidateCapabilities = () => {
    capabilitiesCache = {
      checkedAtMs: 0,
      capabilities: null,
      error: null,
    };
  };

  const loadCapabilities = async (force = false): Promise<{ capabilities: DaemonCapabilities | null; error: string | null }> => {
    const now = Date.now();
    const cacheFresh =
      !force &&
      capabilitiesCache.checkedAtMs > 0 &&
      now - capabilitiesCache.checkedAtMs < CAPABILITY_CACHE_TTL_MS;

    if (cacheFresh) {
      return {
        capabilities: capabilitiesCache.capabilities,
        error: capabilitiesCache.error,
      };
    }

    if (!client.state.connected) {
      capabilitiesCache = {
        checkedAtMs: now,
        capabilities: null,
        error: "trading daemon is not connected",
      };
      return {
        capabilities: null,
        error: capabilitiesCache.error,
      };
    }

    try {
      const response = await client.request("Control.Capabilities", {});
      const parsed = parseCapabilities(response.payload);
      capabilitiesCache = {
        checkedAtMs: now,
        capabilities: parsed.capabilities,
        error: parsed.error,
      };
      return parsed;
    } catch (error) {
      const message = toErrorMessage(error);
      capabilitiesCache = {
        checkedAtMs: now,
        capabilities: null,
        error: message,
      };
      return {
        capabilities: null,
        error: message,
      };
    }
  };

  const runConfidenceGate = async (
    action: TradingHftAction,
    input: TradingHftRequest,
    requireHighImpactChecks: boolean,
  ): Promise<{ confidence: ConfidenceResult; context: ActionContext; capabilities: DaemonCapabilities | null }> => {
    const checks: ConfidenceCheck[] = [];
    const context: ActionContext = {
      engineStatus: null,
      riskStatus: null,
    };

    if (!client.state.connected) {
      checks.push({
        name: "daemon_connected",
        passed: false,
        details: "trading daemon socket is not connected",
      });
      return {
        confidence: {
          passed: false,
          checks,
        },
        context,
        capabilities: null,
      };
    }

    checks.push({
      name: "daemon_connected",
      passed: true,
      details: "trading daemon socket connected",
    });

    const capabilitiesResult = await loadCapabilities(requireHighImpactChecks);
    if (capabilitiesResult.error || !capabilitiesResult.capabilities) {
      checks.push({
        name: "capabilities_available",
        passed: false,
        details: capabilitiesResult.error ?? "missing capabilities payload",
      });
      return {
        confidence: {
          passed: false,
          checks,
        },
        context,
        capabilities: null,
      };
    }

    checks.push({
      name: "capabilities_available",
      passed: true,
      details: "capabilities response received",
    });

    checks.push(...capabilityCompatibilityChecks(capabilitiesResult.capabilities));

    if (!allChecksPassed(checks)) {
      return {
        confidence: {
          passed: false,
          checks,
        },
        context,
        capabilities: capabilitiesResult.capabilities,
      };
    }

    const pingStartedAt = Date.now();
    try {
      const pingResponse = await client.request("Control.Ping", {});
      const pingLatencyMs = Date.now() - pingStartedAt;
      const pingOk = isRecord(pingResponse.payload) ? pingResponse.payload.ok === true : false;
      checks.push({
        name: "ping_roundtrip",
        passed: pingOk && pingLatencyMs <= CONFIDENCE_PING_MAX_MS,
        details: `ok=${pingOk}, latency_ms=${pingLatencyMs}, threshold_ms=${CONFIDENCE_PING_MAX_MS}`,
      });
    } catch (error) {
      checks.push({
        name: "ping_roundtrip",
        passed: false,
        details: toErrorMessage(error),
      });
      return {
        confidence: {
          passed: false,
          checks,
        },
        context,
        capabilities: capabilitiesResult.capabilities,
      };
    }

    try {
      const [engineStatus, riskStatus] = await Promise.all([
        client.request("Engine.Status", {}),
        client.request("Risk.Status", {}),
      ]);
      context.engineStatus = engineStatus.payload;
      context.riskStatus = riskStatus.payload;
      checks.push({
        name: "snapshot_refresh",
        passed: true,
        details: "engine/risk snapshots refreshed",
      });
    } catch (error) {
      checks.push({
        name: "snapshot_refresh",
        passed: false,
        details: toErrorMessage(error),
      });
      return {
        confidence: {
          passed: false,
          checks,
        },
        context,
        capabilities: capabilitiesResult.capabilities,
      };
    }

    if (requireHighImpactChecks) {
      checks.push(preconditionCheck(action, input, context));
    }

    return {
      confidence: {
        passed: allChecksPassed(checks),
        checks,
      },
      context,
      capabilities: capabilitiesResult.capabilities,
    };
  };

  const buildResponse = (params: {
    ok: boolean;
    action: TradingHftAction;
    intentId: string;
    confidence: ConfidenceResult;
    capabilities: DaemonCapabilities | null;
    latencyMs: number;
    data?: unknown;
    error?: { message: string; code?: string };
  }): TradingHftResponse => {
    return {
      ok: params.ok,
      action: params.action,
      intent_id: params.intentId,
      confidence: params.confidence,
      data: params.data,
      error: params.error,
      meta: {
        bridge_version: BRIDGE_VERSION,
        daemon_protocol_version: params.capabilities?.protocol_version ?? null,
        daemon_capabilities: params.capabilities,
        latency_ms: params.latencyMs,
        socket_path: socketPath,
      },
    };
  };

  const runStartupCompatibilityCheck = async () => {
    const result = await loadCapabilities(true);
    if (result.error || !result.capabilities) {
      console.warn(`[trading-bridge] Control.Capabilities check failed: ${result.error ?? "unknown error"}`);
      return;
    }

    const compatibility = capabilityCompatibilityChecks(result.capabilities);
    const failed = compatibility.filter((check) => !check.passed);
    if (failed.length > 0) {
      console.warn(
        `[trading-bridge] daemon compatibility warning: ${failed.map((check) => check.details).join(" | ")}`,
      );
    }
  };

  client.on("connected", () => {
    invalidateCapabilities();
    void runStartupCompatibilityCheck();
  });

  client.on("disconnected", () => {
    invalidateCapabilities();
  });

  api.registerService({
    id: "trading-bridge",
    start: async () => undefined,
    stop: async () => {
      client.dispose();
    },
  });

  api.registerTool({
    name: "trading_hft",
    description:
      "Unified high-frequency trading control-plane tool for engine/risk/strategy operations with confidence-gated high-impact actions.",
    parameters: {
      type: "object",
      additionalProperties: false,
      properties: {
        action: {
          type: "string",
          enum: [
            "health",
            "engine_status",
            "engine_start",
            "engine_stop",
            "engine_pause",
            "engine_resume",
            "engine_killswitch",
            "risk_status",
            "risk_reset_kill_switch",
            "strategy_list",
            "strategy_enable",
            "strategy_disable",
            "strategy_upload_candidate",
            "strategy_promote_candidate",
          ],
        },
        intent_id: { type: "string" },
        strategy_id: { type: "string" },
        source: { type: "string" },
        code_hash: { type: "string" },
        requested_canary_notional_cents: { type: "number" },
        auto: { type: "boolean" },
        compile_passed: { type: "boolean" },
        replay_passed: { type: "boolean" },
        paper_passed: { type: "boolean" },
        latency_passed: { type: "boolean" },
        risk_passed: { type: "boolean" },
      },
      required: ["action"],
    },
    execute: async (arg1: unknown, arg2?: unknown): Promise<TradingHftResponse> => {
      const rawInput = (isRecord(arg2) ? arg2 : isRecord(arg1) ? arg1 : {}) as unknown as TradingHftRequest;
      const startedAt = Date.now();
      const action = rawInput.action;
      const intentId = asIntentId(rawInput.intent_id);

      try {
        if (typeof action !== "string") {
          throw new Error("Missing required field 'action'");
        }

        const highImpact = HIGH_IMPACT_ACTIONS.has(action);
        const gate = await runConfidenceGate(action, rawInput, highImpact);

        if (action === "health") {
          const healthPayload = {
            connected: client.state.connected,
            capabilities: gate.capabilities,
            engine_status: gate.context.engineStatus,
            risk_status: gate.context.riskStatus,
            last_envelope: client.state.lastEnvelope,
          };

          return buildResponse({
            ok: gate.confidence.passed,
            action,
            intentId,
            confidence: gate.confidence,
            capabilities: gate.capabilities,
            latencyMs: Date.now() - startedAt,
            data: healthPayload,
            error: gate.confidence.passed
              ? undefined
              : {
                  code: "confidence_gate_failed",
                  message: "health confidence checks failed",
                },
          });
        }

        if (highImpact && !gate.confidence.passed) {
          return buildResponse({
            ok: false,
            action,
            intentId,
            confidence: gate.confidence,
            capabilities: gate.capabilities,
            latencyMs: Date.now() - startedAt,
            data: {
              engine_status: gate.context.engineStatus,
              risk_status: gate.context.riskStatus,
            },
            error: {
              code: "confidence_gate_failed",
              message: "High-impact action blocked: confidence checks did not pass",
            },
          });
        }

        const command = buildCommand(rawInput);
        const response = await client.request(command.kind, command.payload);

        return buildResponse({
          ok: true,
          action,
          intentId,
          confidence: gate.confidence,
          capabilities: gate.capabilities,
          latencyMs: Date.now() - startedAt,
          data: {
            command: command.kind,
            response: response.payload,
          },
        });
      } catch (error) {
        const errorMessage = toErrorMessage(error);
        const capabilitiesResult = await loadCapabilities();

        return buildResponse({
          ok: false,
          action,
          intentId,
          confidence: {
            passed: false,
            checks: [
              {
                name: "execution",
                passed: false,
                details: errorMessage,
              },
            ],
          },
          capabilities: capabilitiesResult.capabilities,
          latencyMs: Date.now() - startedAt,
          error: {
            code: "execution_failed",
            message: errorMessage,
          },
        });
      }
    },
  });

  client.on("envelope", (envelope: Envelope) => {
    if (envelope.type !== "Event.Alert" && envelope.type !== "Event.RiskAlert") {
      return;
    }

    const payload = envelope.payload as { level?: string; message?: string; reason?: string };
    const body = payload.message ?? payload.reason ?? "Unknown alert";
    api.gateway?.broadcast?.({
      type: "channel_message",
      content: `TRADING ALERT: ${body} [${payload.level ?? "info"}]`,
    });
  });
}
