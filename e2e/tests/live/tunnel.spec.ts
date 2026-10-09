import type { APIRequestContext } from "@playwright/test";
import { test, expect, type Page } from "./support/live";
import type { Relay } from "./support/relay";
import { apiGet, expectBuild, live, openLive, type Who } from "./support/env";

// The tunnel status on the real PC (S7, #10; rows 735 and 736, reaperiem#202):
// the page comes through the band's public host, so through the real
// cloudflared, and the server polls cloudflared's /ready. While the tunnel
// is Ok, the engineer's header shows the indicator „Vonkajší prístup: OK"
// and a member's page shows no tunnel banner. The selectors are the mock
// spec's (`tunnel.spec.ts`: `tunnel-status`, `tunnel-banner`). The page's
// sockets go through the runner's relay, which also records the
// `TunnelStatus` frames the server sent the page. Nothing here changes the
// engine; no burst is needed.

/** The indicator's text while the tunnel is Ok (iem_core::tunnel's approved text). */
const OK_LABEL = "Vonkajší prístup: OK";
/** The server pushes `TunnelStatus` when the page's socket opens. */
const FRAME_MS = 10_000;

type TunnelStatus = { state?: unknown; ready_connections?: unknown; since_secs?: unknown; last_restart_ok?: unknown };

/**
 * GET /api/tunnel through the public host, as `who`; it must answer 200 with
 * a status. Only from the run's build: after "ide event" the predecessor
 * answers at the same address, and the failure then names that hop.
 */
async function tunnelStatus(request: APIRequestContext, who: Who): Promise<TunnelStatus> {
  await expectBuild(request);
  const { status, body } = await apiGet(request, who, "/api/tunnel");
  expect(status, "GET /api/tunnel").toBe(200);
  expect(typeof body === "object" && body !== null, "GET /api/tunnel answers a status").toBe(true);
  return body as TunnelStatus;
}

/** /api/tunnel reads Ok with at least one ready connection to the edge. */
function expectOk(status: TunnelStatus, where: string): void {
  expect(status.state, `${where}: the tunnel's state`).toBe("Ok");
  expect(typeof status.ready_connections === "number" && status.ready_connections >= 1, `${where}: ready connections`).toBe(
    true,
  );
}

/** The `TunnelStatus` payloads the server sent the page on its mixer socket `/ws/<page>`. */
function tunnelFrames(relay: Relay, page: string): TunnelStatus[] {
  return relay
    .events(`/ws/${page}`)
    .filter((e) => e.event === "TunnelStatus")
    .map((e) => (e.data ?? {}) as TunnelStatus);
}

/** Waits until the page's socket carried at least one `TunnelStatus`; returns them all. */
async function framesArrived(relay: Relay, page: string): Promise<TunnelStatus[]> {
  try {
    await expect
      .poll(() => tunnelFrames(relay, page).length, { timeout: FRAME_MS, message: "TunnelStatus frames on the page's socket" })
      .toBeGreaterThan(0);
  } catch (e) {
    // A broken socket is the reason no frame came.
    relay.check();
    throw e;
  }
  return tunnelFrames(relay, page);
}

async function noBanner(page: Page): Promise<void> {
  await expect(page.getByTestId("tunnel-banner")).toHaveCount(0);
}

test.describe("the tunnel status on the real PC, engineer (S7)", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  test("the engineer's header shows Vonkajší prístup: OK matching /api/tunnel through the real tunnel", async ({
    page,
    relay,
    request,
  }) => {
    const status = await tunnelStatus(request, "engineer");
    expectOk(status, "/api/tunnel");
    expect(typeof status.since_secs, "since_secs is part of the status").toBe("number");
    expect("last_restart_ok" in status, "last_restart_ok is part of the status").toBe(true);

    await openLive(page, "engineer");
    const indicator = page.getByTestId("tunnel-status");
    await expect(indicator).toHaveText(OK_LABEL, { timeout: FRAME_MS });
    await expect(indicator).toHaveClass(/\bok\b/);
    const frames = await framesArrived(relay, "engineer");
    expectOk(frames[0], "the first TunnelStatus frame");
    // The page and the API still agree after the page showed the status.
    expectOk(await tunnelStatus(request, "engineer"), "/api/tunnel after the page");
    await expect(indicator).toHaveText(OK_LABEL);
    // The engineer's own page never shows the member banner.
    await noBanner(page);
    relay.check();
  });
});

test.describe("the tunnel status on the real PC, member (S7)", () => {
  test("a member's page shows no tunnel banner while the real tunnel is Ok", async ({ page, relay, request }) => {
    expectOk(await tunnelStatus(request, "member"), "/api/tunnel");

    await openLive(page, "member");
    // Absence counts only once the status arrived.
    const frames = await framesArrived(relay, live().member);
    expectOk(frames[frames.length - 1], "the last TunnelStatus frame");
    await noBanner(page);
    // Members do not get the engineer's indicator.
    await expect(page.getByTestId("tunnel-status")).toHaveCount(0);
    relay.check();
  });
});
