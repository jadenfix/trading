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

type TradingCommand = "start" | "stop" | "status" | "ping";

const DEFAULT_SOCKET_PATH = "/var/run/openclaw/trading.sock";
const RECONNECT_DELAY_MS = 3_000;
const REQUEST_TIMEOUT_MS = 5_000;

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

function toControlKind(command: string): string {
  switch (command) {
    case "start":
      return "Control.Start";
    case "stop":
      return "Control.Stop";
    case "status":
      return "Control.Status";
    case "ping":
      return "Control.Ping";
    default:
      throw new Error(`Unsupported command '${command}'`);
  }
}

function toErrorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

// OpenClaw extension entry point.
// @ts-ignore
export default async function (api: any) {
  const socketPath = api?.config?.socketPath ?? process.env.TRADING_SOCKET_PATH ?? DEFAULT_SOCKET_PATH;
  const client = new TradingClient(socketPath);

  api.registerService({
    name: "trading-bridge",
    start: async () => undefined,
    stop: async () => {
      client.dispose();
    },
  });

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
        const response = await client.request("Control.Status", {});
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

  api.registerTool({
    name: "trading_control",
    description: "Send control commands to the trading engine.",
    parameters: {
      type: "object",
      properties: {
        command: { type: "string", enum: ["start", "stop", "status", "ping"] },
      },
      required: ["command"],
    },
    execute: async ({ command }: { command: TradingCommand }) => {
      const response = await client.request(toControlKind(command), {});
      return {
        sent: true,
        command,
        response: response.payload,
      };
    },
  });

  client.on("envelope", (envelope: Envelope) => {
    if (envelope.type !== "Event.Alert") {
      return;
    }

    const payload = envelope.payload as { level?: string; message?: string };
    api.gateway?.broadcast?.({
      type: "channel_message",
      content: `TRADING ALERT: ${payload.message ?? "Unknown alert"} [${payload.level ?? "info"}]`,
    });
  });
}
