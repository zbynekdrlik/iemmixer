// Imported as NodeWebSocket, never WebSocket: see tests/support/wire.ts.
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
