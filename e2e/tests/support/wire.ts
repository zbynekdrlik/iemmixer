import NodeWebSocket from "ws";
import type { Page } from "@playwright/test";

// Sockets opened from the test runner (Node), not from a page: they see the
// server's answer to an upgrade (a browser only sees "failed") and they
// watch or set a mix without a page of their own. Nothing here writes to a
// page's console.
//
// The Node socket is imported as `NodeWebSocket`, never as `WebSocket`: the
// test transpiler rewrites every reference to an import, also inside a
// function handed to `page.evaluate`, which then runs in the browser as
// `new _ws.default(…)` ("_ws is not defined", run 36349014194). Code for the
// page (`probeListen`) uses the browser's own `WebSocket`.

/** The ws:// form of the run's base URL. */
export function wsBase(baseURL: string | undefined): string {
  if (!baseURL) throw new Error("baseURL is not set (playwright.config.ts)");
  return baseURL.replace(/^http/, "ws");
}

/** A URL with its token hidden, for error messages. */
function redact(url: string): string {
  return url.replace(/token=[^&]*/, "token=…");
}

/**
 * The HTTP status the server answers a WebSocket upgrade of `url` with: 101
 * when the socket opened (it is closed again at once), else the refusal's.
 */
export function upgradeStatus(url: string): Promise<number> {
  return new Promise((resolve, reject) => {
    const ws = new NodeWebSocket(url);
    const timer = setTimeout(() => {
      ws.close();
      reject(new Error(`no answer to the upgrade of ${redact(url)}`));
    }, 5_000);
    ws.on("open", () => {
      clearTimeout(timer);
      ws.close();
      resolve(101);
    });
    ws.on("unexpected-response", (req, res) => {
      clearTimeout(timer);
      resolve(res.statusCode ?? 0);
      res.resume();
      req.destroy();
    });
    ws.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
  });
}

type Received = { event: string; data: any; at: number };

/** A mixer page socket (UI protocol v2) held by the test runner. */
export class PageSocket {
  /** Every JSON event received, in order, with its arrival time (ms). */
  readonly events: Received[] = [];

  private constructor(private readonly ws: NodeWebSocket) {
    ws.on("message", (data, isBinary) => {
      if (isBinary) return;
      const msg = JSON.parse(data.toString());
      this.events.push({ event: msg.event, data: msg.data, at: Date.now() });
    });
  }

  /** Opens `/ws/<page>` with `token` and waits for the page's state. */
  static async open(baseURL: string | undefined, page: string, token: string): Promise<PageSocket> {
    const url = `${wsBase(baseURL)}/ws/${page}?token=${token}&proto=2`;
    const socket = new NodeWebSocket(url);
    // Listening from the start: the hello and the state follow the upgrade at once.
    const s = new PageSocket(socket);
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        socket.close();
        reject(new Error(`${redact(url)} did not open`));
      }, 5_000);
      socket.on("open", () => {
        clearTimeout(timer);
        resolve();
      });
      socket.on("unexpected-response", (req, res) => {
        clearTimeout(timer);
        reject(new Error(`${redact(url)} refused: ${res.statusCode}`));
        res.resume();
        req.destroy();
      });
      socket.on("error", (e) => {
        clearTimeout(timer);
        reject(e);
      });
    });
    await s.next("State");
    return s;
  }

  /** How many events have arrived (a cursor for `next`). */
  mark(): number {
    return this.events.length;
  }

  /** The first `event` at or after `from` that satisfies `pred`. */
  async next(event: string, pred: (data: any) => boolean = () => true, from = 0, timeout = 5_000): Promise<any> {
    const deadline = Date.now() + timeout;
    for (;;) {
      const hit = this.events.slice(from).find((e) => e.event === event && pred(e.data));
      if (hit) return hit.data;
      if (Date.now() > deadline) throw new Error(`no ${event} within ${timeout} ms`);
      await new Promise((r) => setTimeout(r, 20));
    }
  }

  send(cmd: Record<string, unknown>): void {
    this.ws.send(JSON.stringify(cmd));
  }

  /**
   * Waits until every command sent before it is in the engine: the server
   * runs a socket's commands one at a time, each until the engine applied
   * it, so the answer to a request sent last comes after all of them.
   */
  async applied(): Promise<void> {
    const from = this.mark();
    this.send({ cmd: "GetLimiterParams" });
    await this.next("LimiterParams", () => true, from);
  }

  /** The engineer's console (F29), fresh from the server. */
  async console(): Promise<any> {
    const from = this.mark();
    this.send({ cmd: "GetConsole" });
    return this.next("Console", () => true, from);
  }

  /** The peaks (linear, the louder side) of `id` in the Meters since `since` (ms). */
  peaks(id: string, since: number): number[] {
    return this.events
      .filter((e) => e.event === "Meters" && e.at >= since && e.data.meters[id])
      .map((e) => Math.max(...(e.data.meters[id] as number[])));
  }

  close(): void {
    this.ws.close();
  }
}

/**
 * Lets the engine's test sine (−20 dBFS on every input) reach `page`'s mix:
 * `input` at 0 dB and unmuted; with `output`, also the mix's own volume at
 * 0 dB and unmuted (a member's listen tap is after the mix's mute, the
 * engineer's before it). Returns the function that puts every value back.
 */
export async function soundInput(
  baseURL: string | undefined,
  token: string,
  page: string,
  input: string,
  opts: { output?: boolean } = {},
): Promise<() => Promise<void>> {
  const s = await PageSocket.open(baseURL, page, token);
  const state = await s.next("State");
  const channel = state.channels.find((c: { id: string }) => c.id === input);
  if (!channel) throw new Error(`${input} is not a channel of ${page}`);
  const undo: Record<string, unknown>[] = [
    { cmd: "SetLevel", id: input, level_db: channel.level_db },
    { cmd: "SetMute", id: input, muted: channel.muted },
  ];
  s.send({ cmd: "SetLevel", id: input, level_db: 0 });
  s.send({ cmd: "SetMute", id: input, muted: false });
  if (opts.output) {
    if (typeof state.global_level_db !== "number" || typeof state.global_muted !== "boolean") {
      throw new Error(`${page} has no mix volume in its state`);
    }
    undo.push(
      { cmd: "SetGlobalLevel", level_db: state.global_level_db },
      { cmd: "SetGlobalMute", muted: state.global_muted },
    );
    s.send({ cmd: "SetGlobalLevel", level_db: 0 });
    s.send({ cmd: "SetGlobalMute", muted: false });
  }
  await s.applied();
  return async () => {
    for (const cmd of undo) s.send(cmd);
    await s.applied();
    s.close();
  };
}

export type ListenProbe = {
  status: { status: string; target?: string }[];
  frames: number;
  bytes: number;
  /** ms from ListenStart to the first binary frame (null: none came). */
  firstFrameMs: number | null;
};

/**
 * Listens to `target`'s mix on `/ws/audio` from the page for `ms`, then
 * sends ListenStop and closes (the page must be on the app's origin).
 */
export async function probeListen(page: Page, token: string, target: string, ms: number): Promise<ListenProbe> {
  return page.evaluate(
    async ({ token, target, ms }) => {
      const scheme = location.protocol === "https:" ? "wss:" : "ws:";
      const ws = new WebSocket(`${scheme}//${location.host}/ws/audio?token=${token}`);
      ws.binaryType = "arraybuffer";
      const status: { status: string; target?: string }[] = [];
      let frames = 0;
      let bytes = 0;
      let first: number | null = null;
      ws.onmessage = (e) => {
        if (e.data instanceof ArrayBuffer) {
          if (first === null) first = performance.now();
          frames += 1;
          bytes += e.data.byteLength;
        } else {
          const m = JSON.parse(String(e.data));
          if (m.event === "AudioStatus") status.push(m.data);
        }
      };
      await new Promise<void>((resolve, reject) => {
        ws.onopen = () => resolve();
        ws.onerror = () => reject(new Error("the audio socket did not open"));
        setTimeout(() => reject(new Error("the audio socket did not open in 3 s")), 3000);
      });
      const sentAt = performance.now();
      ws.send(JSON.stringify({ cmd: "ListenStart", member_id: target }));
      await new Promise((r) => setTimeout(r, ms));
      ws.send(JSON.stringify({ cmd: "ListenStop" }));
      ws.close();
      return { status, frames, bytes, firstFrameMs: first === null ? null : first - sentAt };
    },
    { token, target, ms },
  );
}
