import type NodeWebSocket from "ws";
import type { Page, WebSocketRoute } from "@playwright/test";
import { expectBuild, live } from "./env";
import { liveSocket } from "./socket";

// Every page socket of a live spec goes through the runner (the #10 decision
// of 2026-10-07, applied to the browser): after "ide event" the predecessor
// answers at the same address with the same secret, so a socket opens only
// after /api/version names the run's build, and never twice. Playwright
// 1.58's `WebSocketRoute.connectToServer()` takes no URL (and connects from
// the page, past any check), so the relay holds the real socket itself.

/** Closing the page's (mocked) socket: a normal close, which the app handles like the server's. */
const CLOSE = { code: 1000 };

/** The paths an error may name as they are; a mixer page's (`/ws/<page>`) holds the page, a site value (P6). */
const NAMED = new Set(["/ws/audio", "/ws/talkback", "/ws/…"]);

/** A page socket's name in an error: its path, or its kind for a mixer page. */
function named(path: string): string {
  return NAMED.has(path) ? path : "the page's mixer socket";
}

export type RelayEvent = { event: string; data: unknown; at: number };

/** Whether the page closed one routed socket; set from the route's first moment. */
type PageSide = { gone: boolean };

/** What the relay saw; `check()` turns a broken rule into a failure. */
export class Relay {
  /** Paths a real socket was opened for in this test (one each, ever). */
  readonly opened = new Set<string>();
  /** Paths the page tried a second time: refused, never sent to the server. */
  readonly refused: string[] = [];
  /** Paths the server closed (a live spec never reconnects). */
  readonly serverClosed: string[] = [];
  /** Sockets that never opened or broke, and why (the socket's name, `named`, and fixed words; no URL). */
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
      this.failures.push(`${named(path)} carried a text frame that is not JSON`);
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

  /** Throws the first broken rule: a socket that never opened or broke, a server close, a second attempt. */
  check(): void {
    if (this.failures.length > 0) throw new Error(`relay: ${this.failures[0]}`);
    if (this.serverClosed.length > 0) throw new Error(`relay: the server closed ${named(this.serverClosed[0])} (no reconnect)`);
    if (this.refused.length > 0) throw new Error(`relay: the page tried ${named(this.refused[0])} again (no reconnect)`);
  }

  /** The test is over: every real socket closes, and nothing after counts. */
  end(): void {
    this.ending = true;
    for (const s of this.sockets) s.close();
    this.sockets.clear();
  }

  /**
   * Pipes the page's socket on `path` to a real one at `url`; resolves once
   * that opened or failed. `side` is the page's close, tracked since the
   * route began (a close during the build check means no real socket).
   */
  async pipe(route: WebSocketRoute, path: string, url: string, side: PageSide): Promise<void> {
    if (side.gone || this.ending) return;
    let failed = false;
    const fail = (why: string) => {
      if (failed || side.gone || this.ending) return;
      failed = true;
      this.failures.push(why);
      void route.close(CLOSE);
    };
    const { ws: real, opened } = liveSocket(url, named(path));
    this.sockets.add(real);
    // Until the open, an error or a close is liveSocket's (it closes the
    // socket after rejecting `opened`, and that error comes on a nextTick,
    // before the catch below runs): its reason is the one recorded.
    let isOpen = false;
    const queue: (string | Buffer)[] = [];
    route.onMessage((m) => {
      if (real.readyState === real.OPEN) real.send(m);
      else queue.push(m);
    });
    // Replaces the handler's own onClose: the page's close now also ends the
    // real socket, and completes the page's (a handled close is not forwarded).
    route.onClose(() => {
      side.gone = true;
      real.close();
      void route.close(CLOSE);
    });
    real.once("open", () => {
      isOpen = true;
      for (const m of queue.splice(0)) real.send(m);
    });
    real.on("message", (d: Buffer, binary: boolean) => {
      this.observe(path, d, binary);
      if (!side.gone) route.send(binary ? d : d.toString());
    });
    real.on("error", () => {
      if (isOpen) fail(`${named(path)} failed (socket error)`);
    });
    real.on("close", () => {
      this.sockets.delete(real);
      if (!isOpen || failed || side.gone || this.ending) return;
      this.serverClosed.push(path);
      void route.close(CLOSE);
    });
    try {
      await opened;
    } catch (e) {
      // liveSocket's reason: the socket's name and fixed words.
      fail((e as Error).message);
    }
  }
}

/**
 * Routes every page socket (`/ws/…`) through the runner: one real socket per
 * path and test, opened only after /api/version names LIVE_SHA; a second
 * attempt is refused and never reaches the server (after "ide event" the
 * predecessor answers at the same address with the same secret). With `hil`,
 * `/ws/audio` gains `&hil=1`. Errors name the socket (`named`), never the URL. The real
 * sockets close with the page.
 */
export async function relaySockets(page: Page, opts: { hil?: boolean } = {}): Promise<Relay> {
  const relay = new Relay();
  const host = new URL(live().baseURL).host;
  page.once("close", () => relay.end());
  await page.routeWebSocket(/\/ws\//, async (route) => {
    const side: PageSide = { gone: false };
    route.onClose(() => {
      side.gone = true;
      void route.close(CLOSE);
    });
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
        relay.failures.push(`${named(path)} is not on the public host`);
        await route.close(CLOSE);
        return;
      }
      try {
        await expectBuild(page.request);
      } catch (e) {
        // expectBuild's text is the failing hop and a status, never a URL.
        relay.failures.push(`${named(path)}: ${(e as Error).message}`);
        await route.close(CLOSE);
        return;
      }
      if (path === "/ws/audio" && opts.hil) url.searchParams.set("hil", "1");
      await relay.pipe(route, path, url.toString(), side);
    } catch {
      // A handler that throws would surface Playwright's own text (the URL).
      relay.failures.push(`${named(path)} could not be relayed`);
      await route.close(CLOSE);
    }
  });
  return relay;
}
