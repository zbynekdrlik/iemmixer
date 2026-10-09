// Imported as NodeWebSocket, never WebSocket: see tests/support/wire.ts.
import type { EventEmitter } from "node:events";
import NodeWebSocket from "ws";
import { devices } from "@playwright/test";
import { live } from "./env";

// The one handshake of every runner-side socket of the live specs (the
// relay's real sockets, the burst watch): to the public host, with the
// page's own Origin and User-Agent (a request without one can meet the
// tunnel's browser check), bounded, and failing with fixed words only (a
// socket error's own text can name the host, its URL holds the token).

/** How long a socket may take to open (the tunnel's TLS and upgrade). */
export const OPEN_MS = 15_000;
/** The live config's browser: runner sockets look like the page's own. */
export const USER_AGENT = devices["Desktop Chrome"].userAgent;

/**
 * Starts a socket to `url` (the public host's, with its token) and returns
 * it with `opened`, which resolves when it opened and rejects, the socket
 * closed, when it was refused, failed or took longer than `OPEN_MS`; the
 * reason starts with `label`. The caller attaches its own listeners before
 * it awaits `opened` (a message right after the upgrade is emitted before a
 * continuation of `opened` runs); a later `error` is already handled here.
 */
export function liveSocket(url: string, label: string): { ws: NodeWebSocket; opened: Promise<void> } {
  const ws = new NodeWebSocket(url, { origin: live().baseURL, headers: { "User-Agent": USER_AGENT } });
  const opened = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`${label} did not open within ${OPEN_MS / 1000} s`));
      // Still connecting: close() aborts the handshake.
      ws.close();
    }, OPEN_MS);
    ws.once("open", () => {
      clearTimeout(timer);
      resolve();
    });
    ws.once("unexpected-response", (req, res) => {
      clearTimeout(timer);
      reject(new Error(`${label} was refused: HTTP ${res.statusCode}`));
      res.resume();
      req.destroy();
      ws.close();
    });
    // Persistent: a socket error after the open must never go unhandled.
    ws.on("error", () => {
      clearTimeout(timer);
      reject(new Error(`${label} failed (socket error)`));
    });
  });
  return { ws, opened };
}

/** `ms` in seconds with `digits` after the point. */
const seconds = (ms: number, digits: number): string => (ms / 1000).toFixed(digits);

/**
 * The words of a socket's close from the far side, for a failure (live run 3,
 * #10): its code (a number, else none), its UTC time (to match the PC's
 * server.log, which names every close the server made or saw), how long after
 * the open and after the last message it came. Never the close's reason text:
 * the far side writes that (P6).
 */
export function closeFacts(f: { code: unknown; at: number; openedAt: number | null; lastAt: number | null }): string {
  const code = typeof f.code === "number" ? `code ${f.code}` : "no code";
  const opened = f.openedAt === null ? "before it opened" : `${seconds(f.at - f.openedAt, 1)} s after it opened`;
  const last = f.lastAt === null ? "with no message before" : `${seconds(f.at - f.lastAt, 2)} s after its last message`;
  return `${code} at ${new Date(f.at).toISOString()}, ${opened}, ${last}`;
}

/**
 * When a runner socket opened and last got a message, for `closeFacts`. Its
 * listeners go on the socket before it opens, beside the owner's own.
 */
export class Wire {
  openedAt: number | null = null;
  lastAt: number | null = null;

  constructor(ws: EventEmitter) {
    ws.on("open", () => {
      this.openedAt = Date.now();
    });
    ws.on("message", () => {
      this.lastAt = Date.now();
    });
  }

  /** The words of a close with `code` now (`closeFacts`). */
  closed(code: unknown): string {
    return closeFacts({ code, at: Date.now(), openedAt: this.openedAt, lastAt: this.lastAt });
  }
}
