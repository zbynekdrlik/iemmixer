import NodeWebSocket from "ws";
import { test, expect } from "./support/fixtures";
import { login } from "./support/session";
import { wsBase } from "./support/wire";

// A client that is gone but whose connection the server never saw close
// (#10, live run 3): the band's public path ends at the tunnel, which lost
// the runner's sockets and never closed the server's side; those sessions
// streamed on for minutes, each holding a listen tap and a mix's solo.
// The server pings every socket and ends a session that sent it nothing,
// not even a pong, for its silence limit. Every browser answers a ping by
// itself, and so does Node's `ws` unless told not to (`autoPong: false`):
// the sockets below that answer stay open.

/** A runner socket that records its pings and when it opened and closed. */
type Held = { ws: NodeWebSocket; opened: Promise<void>; openedAt: number | null; closedAt: number | null; pings: number };

function held(url: string, answer: boolean): Held {
  const ws = new NodeWebSocket(url, { autoPong: answer });
  const h: Held = { ws, opened: Promise.resolve(), openedAt: null, closedAt: null, pings: 0 };
  h.opened = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("a socket did not open within 5 s")), 5_000);
    ws.once("open", () => {
      clearTimeout(timer);
      h.openedAt = Date.now();
      resolve();
    });
    ws.on("error", () => {
      clearTimeout(timer);
      reject(new Error("a socket failed"));
    });
  });
  ws.on("ping", () => {
    h.pings += 1;
  });
  ws.on("close", () => {
    h.closedAt = Date.now();
  });
  return h;
}

test.describe("A client that answers nothing (#10)", () => {
  test("the server ends a mixer and a listen session that answer no ping within 45 s, and keeps those that answer", async ({
    page,
    baseURL,
  }) => {
    test.setTimeout(120_000);
    await page.goto("/");
    const member = await login(page, "member2");
    const engineer = await login(page, "engineer", true);
    const base = wsBase(baseURL);
    const mixer = `${base}/ws/member2?token=${member.token}&proto=2`;
    const audio = `${base}/ws/audio?token=${engineer.token}`;
    const silent = [held(mixer, false), held(audio, false)];
    const answering = [held(mixer, true), held(audio, true)];
    try {
      await Promise.all([...silent, ...answering].map((h) => h.opened));
      await expect
        .poll(() => silent.every((h) => h.closedAt !== null), { timeout: 55_000, intervals: [250] })
        .toBe(true);
      for (const [k, h] of silent.entries()) {
        const lived = (h.closedAt as number) - (h.openedAt as number);
        expect(lived, `silent socket ${k} lived ${lived} ms`).toBeGreaterThanOrEqual(25_000);
        expect(lived, `silent socket ${k} lived ${lived} ms`).toBeLessThanOrEqual(45_000);
        expect(h.pings, `silent socket ${k} was pinged`).toBeGreaterThanOrEqual(2);
      }
      for (const [k, h] of answering.entries()) {
        expect(h.closedAt, `answering socket ${k} is still open`).toBeNull();
        expect(h.pings, `answering socket ${k} was pinged`).toBeGreaterThanOrEqual(2);
      }
    } finally {
      for (const h of [...silent, ...answering]) h.ws.close();
    }
  });
});
