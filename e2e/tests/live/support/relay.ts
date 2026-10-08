// Imported as NodeWebSocket, never WebSocket: see tests/support/wire.ts.
import NodeWebSocket from "ws";
import type { Page, WebSocketRoute } from "@playwright/test";
import { expectBuild, live } from "./env";

// Every page socket of a live spec goes through the runner (the #10 decision
// of 2026-10-07, applied to the browser): after "ide event" the predecessor
// answers at the same address with the same secret, so a socket opens only
// after /api/version names the run's build, and never twice. Playwright
// 1.58's `WebSocketRoute.connectToServer()` takes no URL (and connects from
// the page, past any check), so the relay holds the real socket itself.

/** How long a real socket may take to open (the tunnel's TLS and upgrade). */
const OPEN_MS = 15_000;
/** A page close: the page-side mock forwards 1000 (a native socket refuses 1001). */
const CLOSE = { code: 1000 };

export type RelayEvent = { event: string; data: unknown; at: number };

/** What the relay saw; `check()` turns a broken rule into a failure. */
export class Relay {
  /** Paths a real socket was opened for in this test (one each, ever). */
  readonly opened = new Set<string>();
  /** Paths the page tried a second time: refused, never sent to the server. */
  readonly refused: string[] = [];
  /** Paths the server closed (a live spec never reconnects). */
  readonly serverClosed: string[] = [];
  /** Paths whose socket never opened, and why (fixed words, no URL). */
  readonly failures: string[] = [];
  private readonly texts = new Map<string, RelayEvent[]>();
  private readonly binaries = new Map<string, number>();
  private readonly sockets = new Set<NodeWebSocket>();
  private ending = false;

  /** A message the server sent the page on `path`. */
  observe(path: string, data: Buffer, binary: boolean): void {
    if (binary) {
      this.binaries.set(path, (this.binaries.get(path) ?? 0) + 1);
      return;
    }
    let msg: { event?: unknown; data?: unknown };
    try {
      msg = JSON.parse(data.toString());
    } catch {
      this.failures.push(`${path} carried a text frame that is not JSON`);
      return;
    }
    if (typeof msg?.event !== "string") return;
    const list = this.texts.get(path) ?? [];
    list.push({ event: msg.event, data: msg.data, at: Date.now() });
    this.texts.set(path, list);
  }

  /** The JSON events the server sent the page on `path`, in order. */
  events(path: string): RelayEvent[] {
    return this.texts.get(path) ?? [];
  }

  /** The `AudioStatus` statuses the page got on `path`, in order. */
  statuses(path: string): string[] {
    return this.events(path)
      .filter((e) => e.event === "AudioStatus")
      .map((e) => String((e.data as { status?: unknown } | undefined)?.status));
  }

  /** How many binary frames the server sent the page on `path`. */
  binaryFrames(path: string): number {
    return this.binaries.get(path) ?? 0;
  }

  /** Throws the first broken rule: a socket that never opened, a server close, a second attempt. */
  check(): void {
    if (this.failures.length > 0) throw new Error(`relay: ${this.failures[0]}`);
    if (this.serverClosed.length > 0) throw new Error(`relay: the server closed ${this.serverClosed[0]} (no reconnect)`);
    if (this.refused.length > 0) throw new Error(`relay: the page tried ${this.refused[0]} again (no reconnect)`);
  }

  /** The test is over: every real socket closes, and nothing after counts. */
  end(): void {
    this.ending = true;
    for (const s of this.sockets) s.close();
    this.sockets.clear();
  }

  /** Pipes one page socket on `path` to the real one at `url`; resolves once that opened or failed. */
  async pipe(route: WebSocketRoute, path: string, url: string): Promise<void> {
    let pageClosed = false;
    let failed = false;
    const fail = (why: string) => {
      if (failed || pageClosed || this.ending) return;
      failed = true;
      this.failures.push(`${path} ${why}`);
      void route.close(CLOSE);
    };
    const real = new NodeWebSocket(url, { origin: live().baseURL });
    this.sockets.add(real);
    const queue: (string | Buffer)[] = [];
    route.onMessage((m) => {
      if (real.readyState === NodeWebSocket.OPEN) real.send(m);
      else queue.push(m);
    });
    route.onClose(() => {
      pageClosed = true;
      real.close();
    });
    real.on("message", (d: Buffer, binary: boolean) => {
      this.observe(path, d, binary);
      if (!pageClosed) route.send(binary ? d : d.toString());
    });
    real.on("close", () => {
      this.sockets.delete(real);
      if (failed || pageClosed || this.ending) return;
      this.serverClosed.push(path);
      void route.close(CLOSE);
    });
    await new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        fail(`did not open within ${OPEN_MS / 1000} s`);
        // A socket still connecting: close() aborts its handshake.
        real.close();
        resolve();
      }, OPEN_MS);
      real.on("open", () => {
        clearTimeout(timer);
        for (const m of queue.splice(0)) real.send(m);
        resolve();
      });
      real.on("unexpected-response", (req, res) => {
        clearTimeout(timer);
        fail(`was refused: HTTP ${res.statusCode}`);
        res.resume();
        req.destroy();
        resolve();
      });
      // The error's own text can name the host: the path and fixed words only.
      real.on("error", () => {
        clearTimeout(timer);
        fail("failed (socket error)");
        resolve();
      });
    });
  }
}

/**
 * Routes every page socket (`/ws/…`) through the runner: one real socket per
 * path and test, opened only after /api/version names LIVE_SHA; a second
 * attempt is refused and never reaches the server (after "ide event" the
 * predecessor answers at the same address with the same secret). With `hil`,
 * `/ws/audio` gains `&hil=1`. Errors name the path, never the URL. The real
 * sockets close with the page.
 */
export async function relaySockets(page: Page, opts: { hil?: boolean } = {}): Promise<Relay> {
  const relay = new Relay();
  const host = new URL(live().baseURL).host;
  page.once("close", () => relay.end());
  await page.routeWebSocket(/\/ws\//, async (route) => {
    let path = "/ws/…";
    try {
      const url = new URL(route.url());
      path = url.pathname;
      if (relay.opened.has(path)) {
        relay.refused.push(path);
        await route.close(CLOSE);
        return;
      }
      relay.opened.add(path);
      if (url.host !== host) {
        relay.failures.push(`${path} is not on the public host`);
        await route.close(CLOSE);
        return;
      }
      try {
        await expectBuild(page.request);
      } catch (e) {
        // expectBuild's text is the failing hop and a status, never a URL.
        relay.failures.push(`${path}: ${(e as Error).message}`);
        await route.close(CLOSE);
        return;
      }
      if (path === "/ws/audio" && opts.hil) url.searchParams.set("hil", "1");
      await relay.pipe(route, path, url.toString());
    } catch {
      // A handler that throws would surface Playwright's own text (the URL).
      relay.failures.push(`${path} could not be relayed`);
      await route.close(CLOSE);
    }
  });
  return relay;
}
