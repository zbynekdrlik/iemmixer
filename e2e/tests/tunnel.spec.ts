import { createServer, Server } from "node:http";
import type { WebSocketRoute } from "@playwright/test";
import { test, expect, Page } from "./support/fixtures";
import { login } from "./support/session";

// Internet access (the Cloudflare tunnel, reaperiem#202): the server polls
// cloudflared's /ready every 30 s and pushes TunnelStatus (also
// GET /api/tunnel). The engineer's pages show the indicator, a member's page
// the "use the LAN address" banner while the tunnel is broken, and the
// Reconnecting banner of a page opened on the public host a LAN hint.
//
// In CI cloudflared is absent, so the tunnel is Down (or, 2 min after a
// failed restart, Restarting). For Ok, a stub answers /ready at
// `tunnel_ready_url` (the default http://127.0.0.1:20241/ready; the test site
// sets none) until the tunnel is Down again. The public host
// (`https_domain`, mixer.example.org) is mapped onto the CI server by
// Chromium's host resolver.

// The run's base URL as playwright.config.ts has it (the launch options below
// need it before any fixture exists).
const BASE_URL = new URL(process.env.E2E_BASE_URL || "http://localhost:80");
const PUBLIC_ORIGIN = `http://mixer.example.org${BASE_URL.port ? `:${BASE_URL.port}` : ""}`;
const LAN_HINT = "Ak nejde internet a ste na miestnej sieti, otvorte http://10.0.0.10";
const MEMBER_BANNER = "Internetový prístup nefunguje — na tejto sieti otvorte http://10.0.0.10";

// Launch options are the worker's: top level, never in a describe.
test.use({
  launchOptions: { args: [`--host-resolver-rules=MAP mixer.example.org ${BASE_URL.hostname}`] },
});

// The engineer page subscribes to Web Push; Playwright's browser contexts
// are incognito, where Chrome has no Push API: its error and the page's
// `[push] …` warnings are expected here.
const ENGINEER_PAGE_CONSOLE = {
  allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
} as const;

type TunnelStatus = {
  state: "Ok" | "Down" | "Restarting";
  ready_connections: number;
  since_secs: number;
  last_restart_secs_ago: number | null;
  last_restart_ok: boolean | null;
};

/** The indicator text for a status (iem_core::tunnel's approved texts). */
function engineerLabel(s: TunnelStatus): string {
  if (s.state !== "Ok" && s.last_restart_ok === false) return "Vonkajší prístup: nefunguje — oprava zlyhala";
  return {
    Ok: "Vonkajší prístup: OK",
    Down: "Vonkajší prístup: nefunguje",
    Restarting: "Vonkajší prístup: nefunguje — opravujem",
  }[s.state];
}

async function tunnelStatus(page: Page): Promise<TunnelStatus> {
  const resp = await page.request.get("/api/tunnel");
  expect(resp.status()).toBe(200);
  return resp.json();
}

/** Every TunnelStatus payload the page's mixer socket receives. */
function tunnelFrames(page: Page): TunnelStatus[] {
  const frames: TunnelStatus[] = [];
  page.on("websocket", (ws) => {
    if (!ws.url().includes("/ws/")) return;
    ws.on("framereceived", (f) => {
      if (typeof f.payload !== "string") return;
      const m = JSON.parse(f.payload);
      if (m.event === "TunnelStatus") frames.push(m.data);
    });
  });
  return frames;
}

/**
 * Signs in as `member` (the engineer PIN when `engineer`) through the API
 * and opens `/<path>` on `origin` (the run's base URL when omitted).
 */
async function openAt(
  page: Page,
  member: string,
  opts: { engineer?: boolean; path?: string; origin?: string } = {},
): Promise<void> {
  const origin = opts.origin ?? "";
  const auth = await login(page, member, opts.engineer ?? false);
  await page.goto(`${origin}/`);
  await page.evaluate((a) => {
    localStorage.setItem("iem_token", JSON.stringify(a));
    sessionStorage.setItem("iem_redirected", "1");
  }, auth);
  await page.goto(`${origin}/${opts.path ?? member}`);
  await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
}

test.describe("Tunnel status while cloudflared is absent", () => {
  test.use(ENGINEER_PAGE_CONSOLE);

  test("the engineer's indicator shows the state /api/tunnel reports", async ({ page }) => {
    await page.goto("/");
    const first = await tunnelStatus(page);
    expect(["Down", "Restarting"]).toContain(first.state);
    expect(first.ready_connections).toBe(0);

    await openAt(page, "engineer", { engineer: true });
    const indicator = page.getByTestId("tunnel-status");
    // The state can move on (Down → Restarting after 2 min): compare with a fresh read.
    await expect
      .poll(async () => {
        const s = await tunnelStatus(page);
        const text = (await indicator.textContent())?.trim();
        const cls = (await indicator.getAttribute("class")) ?? "";
        return text === engineerLabel(s) && cls.split(" ").includes(s.state.toLowerCase())
          ? "matches"
          : `${text} (${cls}) vs ${engineerLabel(s)}`;
      }, { timeout: 10_000 })
      .toBe("matches");
    await expect(page.getByTestId("tunnel-banner")).toHaveCount(0);
  });

  test("on a member's page the engineer gets the indicator, never the member banner", async ({
    page,
    browser,
  }) => {
    // The member sees the banner on this very page while the tunnel is down …
    const ctx = await browser.newContext();
    const member = await ctx.newPage();
    await openAt(member, "member9");
    await expect(member.getByTestId("tunnel-banner")).toHaveText(MEMBER_BANNER, { timeout: 10_000 });
    await expect(member.getByTestId("tunnel-status")).toHaveCount(0);
    await ctx.close();

    // … the engineer on it gets the indicator instead.
    await openAt(page, "engineer", { engineer: true, path: "member9" });
    await expect(page.getByTestId("tunnel-status")).toContainText("Vonkajší prístup: nefunguje", {
      timeout: 10_000,
    });
    await expect(page.getByTestId("tunnel-banner")).toHaveCount(0);
  });
});

test.describe("Tunnel status with a ready cloudflared", () => {
  test.use(ENGINEER_PAGE_CONSOLE);
  // The first test waits for the server's next /ready poll (every 30 s).
  test.describe.configure({ mode: "serial", timeout: 90_000 });

  let stub: Server | undefined;

  test.beforeAll(async () => {
    stub = createServer((req, res) => {
      if (req.url === "/ready") {
        res.writeHead(200, { "Content-Type": "application/json", Connection: "close" });
        res.end(JSON.stringify({ status: 200, readyConnections: 4 }));
      } else {
        res.writeHead(404, { Connection: "close" });
        res.end();
      }
    });
    await new Promise<void>((resolve, reject) => {
      stub!.once("error", reject);
      stub!.listen(20241, "127.0.0.1", () => resolve());
    });
  });

  test.afterAll(async ({ playwright }) => {
    test.setTimeout(60_000);
    if (stub) {
      stub.closeAllConnections();
      await new Promise<void>((resolve) => stub!.close(() => resolve()));
    }
    // Leave the shared server as the run found it: down again at its next poll.
    const api = await playwright.request.newContext({ baseURL: BASE_URL.toString() });
    try {
      await expect
        .poll(async () => (await (await api.get("/api/tunnel")).json()).state, { timeout: 45_000 })
        .not.toBe("Ok");
    } finally {
      await api.dispose();
    }
  });

  /** Waits until the server has polled the stub. */
  async function tunnelOk(page: Page): Promise<TunnelStatus> {
    await expect.poll(async () => (await tunnelStatus(page)).state, { timeout: 45_000 }).toBe("Ok");
    return tunnelStatus(page);
  }

  test("the engineer's header shows Vonkajší prístup: OK matching /api/tunnel", async ({ page }) => {
    const frames = tunnelFrames(page);
    await page.goto("/");
    const status = await tunnelOk(page);
    expect(status.ready_connections).toBe(4);
    expect(typeof status.since_secs).toBe("number");
    expect("last_restart_ok" in status, "last_restart_ok is part of the status").toBe(true);

    await openAt(page, "engineer", { engineer: true });
    const indicator = page.getByTestId("tunnel-status");
    await expect(indicator).toHaveText("Vonkajší prístup: OK", { timeout: 10_000 });
    await expect(indicator).toHaveClass(/\bok\b/);
    await expect.poll(() => frames.length).toBeGreaterThan(0);
    expect(frames[0].state).toBe("Ok");
    expect(frames[0].ready_connections).toBe(4);
    // The engineer's own page never shows the member banner.
    await expect(page.getByTestId("tunnel-banner")).toHaveCount(0);
  });

  test("a member's page shows no banner and no indicator while Ok", async ({ page }) => {
    const frames = tunnelFrames(page);
    await page.goto("/");
    await tunnelOk(page);
    await openAt(page, "member9");
    // Absence counts only once the status arrived.
    await expect.poll(() => frames.length).toBeGreaterThan(0);
    expect(frames[frames.length - 1].state).toBe("Ok");
    await expect(page.getByTestId("tunnel-banner")).toHaveCount(0);
    await expect(page.getByTestId("tunnel-status")).toHaveCount(0);
  });

  test("on a member's page the engineer's indicator shows OK, and no banner", async ({ page }) => {
    const frames = tunnelFrames(page);
    await page.goto("/");
    await tunnelOk(page);
    await openAt(page, "engineer", { engineer: true, path: "member9" });
    await expect.poll(() => frames.length).toBeGreaterThan(0);
    await expect(page.getByTestId("tunnel-status")).toHaveText("Vonkajší prístup: OK", { timeout: 10_000 });
    await expect(page.getByTestId("tunnel-banner")).toHaveCount(0);
  });
});

/**
 * Opens member9's mixer on `origin`, then takes the page's socket away as a
 * tunnel outage does: the open socket closes and every reconnect is closed
 * at once (routeWebSocket), so the debounced Reconnecting banner shows.
 */
async function openThenLoseSocket(page: Page, origin: string): Promise<void> {
  const routes: WebSocketRoute[] = [];
  let cut = false;
  await page.routeWebSocket(/\/ws\/member9\?/, (ws) => {
    if (cut) {
      void ws.close({ code: 4000, reason: "e2e: simulated tunnel outage" });
      return;
    }
    ws.connectToServer();
    routes.push(ws);
  });
  await openAt(page, "member9", { origin });
  await expect(page.locator(".channels-scroll")).toBeVisible();
  await expect(page.locator(".disconnected-banner")).toHaveCount(0);

  cut = true;
  for (const ws of routes) await ws.close({ code: 4000, reason: "e2e: simulated tunnel outage" });
  await expect(page.locator(".disconnected-banner")).toBeVisible({ timeout: 15_000 });
}

test.describe("LAN hint under Reconnecting", () => {
  test("the public host (mixer.example.org) shows the LAN address hint", async ({ page }) => {
    await openThenLoseSocket(page, PUBLIC_ORIGIN);
    expect(new URL(page.url()).hostname).toBe("mixer.example.org");
    await expect(page.getByTestId("lan-hint")).toHaveText(LAN_HINT);
  });

  test("a LAN or local address shows no hint", async ({ page }) => {
    await openThenLoseSocket(page, "");
    expect(new URL(page.url()).hostname).toBe(BASE_URL.hostname);
    await expect(page.getByTestId("lan-hint")).toHaveCount(0);
  });
});
