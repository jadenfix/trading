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

type TradingControlCommand =
  | "start"
  | "stop"
  | "status"
  | "ping"
  | "pause"
  | "resume"
  | "killswitch";

type StrategyManageAction =
  | "list"
  | "enable"
  | "disable"
  | "upload_candidate"
  | "promote_candidate";

const DEFAULT_SOCKET_PATH = "/var/run/openclaw/trading.sock";
const RECONNECT_DELAY_MS = 3_000;
const REQUEST_TIMEOUT_MS = 5_000;
const MAX_FRAME_LENGTH = 1_048_576; // 1 MiB
const DEFAULT_CANDIDATE_CANARY_NOTIONAL_CENTS = 250; // Intentional low default for safe canary bootstrap.

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

function toCommandKind(command: TradingControlCommand): string {
  switch (command) {
    case "start":
      return "Control.Start";
    case "stop":
      return "Control.Stop";
    case "status":
      // Keep legacy clients coherent: "status" uses Engine.Status even if daemon still accepts Control.Status.
      return "Engine.Status";
    case "ping":
      return "Control.Ping";
    case "pause":
      return "Engine.Pause";
    case "resume":
      return "Engine.Resume";
    case "killswitch":
      return "Engine.KillSwitch";
    default:
      throw new Error(`Unsupported command '${command}'`);
  }
}

function toErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function asString(value: unknown, field: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`Missing required field '${field}'`);
  }
  return value.trim();
}

// OpenClaw extension entry point.
// @ts-ignore
export default function (api: any) {
  const socketPath = api?.config?.socketPath ?? process.env.TRADING_SOCKET_PATH ?? DEFAULT_SOCKET_PATH;
  const client = new TradingClient(socketPath);

  api.registerService({
    id: "trading-bridge",
    start: async () => undefined,
    stop: async () => {
      client.dispose();
    },
  });

  // Legacy compatibility tool.
  api.registerTool({
    name: "trading_status",
    description: "Get bridge connectivity and daemon status.",
    execute: async () => {
      if (!client.state.connected) {
        return {
          connected: false,
          socketPath,
          daemon: null,
        };
      }

      try {
        const response = await client.request("Engine.Status", {});
        return {
          connected: true,
          socketPath,
          daemon: response.payload,
        };
      } catch (error) {
        return {
          connected: client.state.connected,
          socketPath,
          error: toErrorMessage(error),
          daemon: client.state.lastEnvelope?.payload ?? null,
        };
      }
    },
  });

  // Legacy compatibility tool.
  api.registerTool({
    name: "trading_control",
    description: "Send control commands to the trading engine.",
    parameters: {
      type: "object",
      properties: {
        command: { type: "string", enum: ["start", "stop", "status", "ping", "pause", "resume", "killswitch"] },
      },
      required: ["command"],
    },
    execute: async ({ command }: { command: TradingControlCommand }) => {
      if (!client.state.connected) {
        return {
          sent: false,
          command,
          connected: false,
          error: "Trading daemon is not connected",
        };
      }

      try {
        const response = await client.request(toCommandKind(command), {});
        return {
          sent: true,
          command,
          response: response.payload,
        };
      } catch (error) {
        return {
          sent: false,
          command,
          connected: client.state.connected,
          error: toErrorMessage(error),
        };
      }
    },
  });

  api.registerTool({
    name: "trading_engine_status",
    description: "Get engine, strategy, and risk status snapshots.",
    execute: async () => {
      if (!client.state.connected) {
        return {
          connected: false,
          socketPath,
          engine: null,
          risk: null,
          strategies: null,
        };
      }

      try {
        const [engine, risk, strategies] = await Promise.all([
          client.request("Engine.Status", {}),
          client.request("Risk.Status", {}),
          client.request("Strategy.List", {}),
        ]);

        return {
          connected: true,
          socketPath,
          engine: engine.payload,
          risk: risk.payload,
          strategies: strategies.payload,
        };
      } catch (error) {
        return {
          connected: client.state.connected,
          socketPath,
          error: toErrorMessage(error),
          lastEnvelope: client.state.lastEnvelope,
        };
      }
    },
  });

  api.registerTool({
    name: "trading_engine_control",
    description: "Control engine runtime state.",
    parameters: {
      type: "object",
      properties: {
        command: { type: "string", enum: ["start", "stop", "status", "ping", "pause", "resume", "killswitch"] },
      },
      required: ["command"],
    },
    execute: async ({ command }: { command: TradingControlCommand }) => {
      if (!client.state.connected) {
        return {
          sent: false,
          command,
          connected: false,
          error: "Trading daemon is not connected",
        };
      }

      try {
        const response = await client.request(toCommandKind(command), {});
        return {
          sent: true,
          command,
          response: response.payload,
        };
      } catch (error) {
        return {
          sent: false,
          command,
          connected: client.state.connected,
          error: toErrorMessage(error),
        };
      }
    },
  });

  api.registerTool({
    name: "trading_strategy_manage",
    description: "List/enable/disable strategies and manage candidate promotions.",
    parameters: {
      type: "object",
      properties: {
        action: {
          type: "string",
          enum: ["list", "enable", "disable", "upload_candidate", "promote_candidate"],
        },
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
    execute: async (input: {
      action: StrategyManageAction;
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
    }) => {
      if (!client.state.connected) {
        return {
          sent: false,
          action: input.action,
          connected: false,
          error: "Trading daemon is not connected",
        };
      }

      try {
        let kind = "";
        let payload: Record<string, unknown> = {};

        switch (input.action) {
          case "list":
            kind = "Strategy.List";
            break;
          case "enable":
            kind = "Strategy.Enable";
            payload = { strategy_id: asString(input.strategy_id, "strategy_id") };
            break;
          case "disable":
            kind = "Strategy.Disable";
            payload = { strategy_id: asString(input.strategy_id, "strategy_id") };
            break;
          case "upload_candidate":
            kind = "Strategy.UploadCandidate";
            payload = {
              strategy_id: asString(input.strategy_id, "strategy_id"),
              source: typeof input.source === "string" && input.source.trim() !== "" ? input.source : "agent",
              code_hash: asString(input.code_hash, "code_hash"),
              requested_canary_notional_cents:
                typeof input.requested_canary_notional_cents === "number"
                  ? input.requested_canary_notional_cents
                  : DEFAULT_CANDIDATE_CANARY_NOTIONAL_CENTS,
              compile_passed: typeof input.compile_passed === "boolean" ? input.compile_passed : true,
              replay_passed: typeof input.replay_passed === "boolean" ? input.replay_passed : true,
              paper_passed: typeof input.paper_passed === "boolean" ? input.paper_passed : true,
              latency_passed: typeof input.latency_passed === "boolean" ? input.latency_passed : true,
              risk_passed: typeof input.risk_passed === "boolean" ? input.risk_passed : true,
            };
            break;
          case "promote_candidate":
            kind = "Strategy.PromoteCandidate";
            payload = {
              strategy_id: asString(input.strategy_id, "strategy_id"),
              code_hash: asString(input.code_hash, "code_hash"),
              requested_canary_notional_cents:
                typeof input.requested_canary_notional_cents === "number"
                  ? input.requested_canary_notional_cents
                  : DEFAULT_CANDIDATE_CANARY_NOTIONAL_CENTS,
              auto: typeof input.auto === "boolean" ? input.auto : true,
            };
            break;
          default:
            throw new Error(`Unsupported strategy action '${input.action}'`);
        }

        const response = await client.request(kind, payload);
        return {
          sent: true,
          action: input.action,
          response: response.payload,
        };
      } catch (error) {
        return {
          sent: false,
          action: input.action,
          connected: client.state.connected,
          error: toErrorMessage(error),
        };
      }
    },
  });

  api.registerTool({
    name: "trading_risk_status",
    description: "Read hard safety cage limits and runtime risk state.",
    execute: async () => {
      if (!client.state.connected) {
        return {
          connected: false,
          socketPath,
          risk: null,
        };
      }

      try {
        const response = await client.request("Risk.Status", {});
        return {
          connected: true,
          socketPath,
          risk: response.payload,
        };
      } catch (error) {
        return {
          connected: client.state.connected,
          socketPath,
          error: toErrorMessage(error),
          risk: client.state.lastEnvelope?.payload ?? null,
        };
      }
    },
  });

  client.on("envelope", (envelope: Envelope) => {
    // Broadcast both generic alerts and explicit risk alerts to operator channels.
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
