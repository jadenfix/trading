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

type VenueManageAction = "list" | "enable" | "disable" | "status";

type OrderManageAction = "submit" | "cancel" | "list";

type PortfolioStatusAction = "balances" | "positions";

type ExecutionModeAction = "get" | "set";

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

function asNumber(value: unknown, field: string): number {
  if (typeof value !== "number" || Number.isNaN(value)) {
    throw new Error(`Missing required field '${field}'`);
  }
  return value;
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

  api.registerTool({
    name: "trading_venue_manage",
    description: "List, enable, disable, or inspect venue status.",
    parameters: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["list", "enable", "disable", "status"] },
        venue_id: { type: "string" },
      },
      required: ["action"],
    },
    execute: async (input: { action: VenueManageAction; venue_id?: unknown }) => {
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
            kind = "Venue.List";
            break;
          case "enable":
            kind = "Venue.Enable";
            payload = { venue_id: asString(input.venue_id, "venue_id") };
            break;
          case "disable":
            kind = "Venue.Disable";
            payload = { venue_id: asString(input.venue_id, "venue_id") };
            break;
          case "status":
            kind = "Venue.Status";
            payload = { venue_id: asString(input.venue_id, "venue_id") };
            break;
          default:
            throw new Error(`Unsupported venue action '${input.action}'`);
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
    name: "trading_order_manage",
    description: "Submit, cancel, or list orders across enabled venues.",
    parameters: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["submit", "cancel", "list"] },
        strategy_id: { type: "string" },
        venue_id: { type: "string" },
        symbol: { type: "string" },
        asset_class: { type: "string" },
        market_type: { type: "string" },
        side: { type: "string" },
        order_type: { type: "string" },
        quantity: { type: "number" },
        limit_price: { type: "number" },
        tif: { type: "string" },
        post_only: { type: "boolean" },
        reduce_only: { type: "boolean" },
        client_order_id: { type: "string" },
        expiry_ts_ms: { type: "number" },
        strike: { type: "number" },
        option_type: { type: "string" },
        venue_order_id: { type: "string" },
      },
      required: ["action"],
    },
    execute: async (input: {
      action: OrderManageAction;
      strategy_id?: unknown;
      venue_id?: unknown;
      symbol?: unknown;
      asset_class?: unknown;
      market_type?: unknown;
      side?: unknown;
      order_type?: unknown;
      quantity?: unknown;
      limit_price?: unknown;
      tif?: unknown;
      post_only?: unknown;
      reduce_only?: unknown;
      client_order_id?: unknown;
      expiry_ts_ms?: unknown;
      strike?: unknown;
      option_type?: unknown;
      venue_order_id?: unknown;
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
          case "submit":
            kind = "Order.Submit";
            payload = {
              strategy_id: asString(input.strategy_id, "strategy_id"),
              venue_id: asString(input.venue_id, "venue_id"),
              instrument: {
                venue_id: asString(input.venue_id, "venue_id"),
                symbol: asString(input.symbol, "symbol"),
                asset_class: asString(input.asset_class, "asset_class"),
                market_type: asString(input.market_type, "market_type"),
                expiry_ts_ms: typeof input.expiry_ts_ms === "number" ? input.expiry_ts_ms : null,
                strike: typeof input.strike === "number" ? input.strike : null,
                option_type: typeof input.option_type === "string" ? input.option_type : null,
              },
              side: asString(input.side, "side"),
              order_type: asString(input.order_type, "order_type"),
              quantity: asNumber(input.quantity, "quantity"),
              limit_price: typeof input.limit_price === "number" ? input.limit_price : null,
              tif: typeof input.tif === "string" ? input.tif : null,
              post_only: typeof input.post_only === "boolean" ? input.post_only : false,
              reduce_only: typeof input.reduce_only === "boolean" ? input.reduce_only : false,
              client_order_id: asString(input.client_order_id, "client_order_id"),
            };
            break;
          case "cancel":
            kind = "Order.Cancel";
            payload = {
              venue_id: typeof input.venue_id === "string" ? input.venue_id : null,
              venue_order_id: asString(input.venue_order_id, "venue_order_id"),
            };
            break;
          case "list":
            kind = "Order.List";
            payload = {
              venue_id: typeof input.venue_id === "string" ? input.venue_id : null,
            };
            break;
          default:
            throw new Error(`Unsupported order action '${input.action}'`);
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
    name: "trading_portfolio_status",
    description: "Get normalized balances or positions across enabled venues.",
    parameters: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["balances", "positions"] },
      },
      required: ["action"],
    },
    execute: async (input: { action: PortfolioStatusAction }) => {
      if (!client.state.connected) {
        return {
          connected: false,
          socketPath,
          action: input.action,
          portfolio: null,
          error: "Trading daemon is not connected",
        };
      }

      try {
        const kind = input.action === "balances" ? "Portfolio.Balances" : "Portfolio.Positions";
        const response = await client.request(kind, {});
        return {
          connected: true,
          socketPath,
          action: input.action,
          portfolio: response.payload,
        };
      } catch (error) {
        return {
          connected: client.state.connected,
          socketPath,
          action: input.action,
          error: toErrorMessage(error),
          portfolio: null,
        };
      }
    },
  });

  api.registerTool({
    name: "trading_execution_mode",
    description: "Get or set paper/live execution mode for a venue market type.",
    parameters: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["get", "set"] },
        venue_id: { type: "string" },
        market_type: { type: "string" },
        mode: { type: "string", enum: ["paper", "live"] },
      },
      required: ["action", "venue_id", "market_type"],
    },
    execute: async (input: {
      action: ExecutionModeAction;
      venue_id?: unknown;
      market_type?: unknown;
      mode?: unknown;
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
        const kind = input.action === "set" ? "ExecutionMode.Set" : "ExecutionMode.Get";
        const payload = {
          venue_id: asString(input.venue_id, "venue_id"),
          market_type: asString(input.market_type, "market_type"),
          mode: input.action === "set" ? asString(input.mode, "mode") : "",
        };

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
