import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  chromium,
  devices,
  type APIRequestContext,
  type BrowserContext,
  type Page,
  type Response,
} from "@playwright/test";
import { test as base, expect } from "./support/live";
import { guardConsole } from "./support/console";
import { apiPost, expectBuild, openLive } from "./support/env";
import { relaySockets, type Relay } from "./support/relay";
import { USER_AGENT } from "./support/socket";
import { PushLedger, SUBSCRIBE, UNSUBSCRIBE, bodyOf, browserEndpoint, endpointOf, isPostTo } from "./support/push";

// Push unsubscribe on the real PC (S7, #10; row 717, reaperiem#188): the
// engineer's page subscribes to Web Push with the server's VAPID key, and
// logging out in the settings revokes the subscription in the browser and on
// the server. Playwright's contexts are incognito, where Chrome has no Push
// API, so this test runs its own persistent profile in a temp dir, with
// notifications granted, on the full Chromium (`channel: "chromium"`: the
// headless shell has no push service, its subscribe fails "push service not
// available"). Its console guard is its own, allows nothing and redacts
// URLs. The page's sockets go through the runner's relay. The fixture's
// teardown, which runs also after a timeout, revokes what the logout did
// not: in the browser, then (the profile closed, so nothing more is posted)
// on the server, after the build check; `pc-end` compares the server's
// subscription counts as the backstop. No error here carries the endpoint
// (a capability), a URL or the token.

/**
 * From the wait's start: the page's open (two navigations, its state), then
 * the fire-and-forget subscribe. A fresh profile's first
 * `pushManager.subscribe()` registers with the browser's push service: 4.4,
 * 4.4, 27.7, 29.1, 30.3, 30.6, 31.3, 31.7 and 33.2 s with Playwright 1.58's
 * full Chromium 145 (nine runs, 2026-10-09, #10). Live run 1 ran out of the
 * old 30 s with no failing step of the app's subscribe: every failing step
 * warns `[push] …` and the console guard saw none; a skip only logs (the
 * public host's VAPID key was checked served afterwards), and a `sw.ready()` still
 * pending logs nothing. About three times the slowest.
 */
const SUBSCRIBE_MS = 90_000;
/** The logout's unsubscribe POST follows the browser's own unsubscribe. */
const UNSUBSCRIBE_MS = 15_000;

/** The engineer's device: a persistent profile's page, its relay and its console. */
type Device = { page: Page; relay: Relay; problems: string[] };

const test = base.extend<{ device: Device }>({
  // The only page here is the profile's (guarded by `guardConsole`): no default page, no second browser.
  // An override keeps the base's options (automatic, per test).
  consoleGuard: async ({}, use) => use(),
  device: async ({ request }, use, testInfo) => {
    const dir = mkdtempSync(join(tmpdir(), "iemmixer-live-push-"));
    const ledger = new PushLedger();
    let context: BrowserContext | undefined;
    let page: Page | undefined;
    let failure: unknown = null;
    try {
      context = await chromium.launchPersistentContext(dir, {
        channel: "chromium",
        userAgent: USER_AGENT,
        viewport: devices["Desktop Chrome"].viewport,
        permissions: ["notifications"],
      });
      context.on("request", (r) => ledger.sent(r));
      context.on("response", (r) => ledger.answered(r));
      page = context.pages()[0] ?? (await context.newPage());
      const problems = guardConsole(page);
      const relay = await relaySockets(page);
      await use({ page, relay, problems });
      relay.check();
      expect(problems, "the persistent page's console must stay clean").toEqual([]);
    } catch (e) {
      failure = e;
    }
    const left = await cleanUp(request, ledger, context, page);
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      left.push("the profile's temp dir was not removed");
    }
    if (failure !== null) {
      // The test's own failure leads; what the teardown could not do goes beside it.
      if (left.length > 0) testInfo.annotations.push({ type: "teardown", description: left.join("; ") });
      throw failure;
    }
    if (left.length > 0) throw new Error(`teardown: ${left.join("; ")}`);
  },
});

/**
 * Revokes what the logout did not: in the browser first (its push service
 * then drops the endpoint, so the server prunes it at its next push even
 * when the revoke below fails), then the profile closes (after it, no
 * subscribe can be posted), then the server is told, only once
 * /api/version names the run's build (the predecessor answers 200 too).
 * Returns what failed, in fixed words.
 */
async function cleanUp(
  request: APIRequestContext,
  ledger: PushLedger,
  context: BrowserContext | undefined,
  page: Page | undefined,
): Promise<string[]> {
  const left: string[] = [];
  if (ledger.pending().length > 0 && page !== undefined && !page.isClosed()) {
    try {
      await browserEndpoint(page, true);
    } catch {
      // Best effort: the server's revoke below is the one that counts.
    }
  }
  try {
    await context?.close();
  } catch {
    left.push("the persistent profile did not close");
  }
  const pending = ledger.pending();
  if (pending.length === 0) return left;
  try {
    await expectBuild(request);
  } catch (e) {
    // expectBuild's text: the failing hop and a status, never a URL.
    left.push(`${pending.length} subscription(s) left on the server: ${(e as Error).message}`);
    return left;
  }
  for (const endpoint of pending) {
    try {
      const { status } = await apiPost(request, "engineer", UNSUBSCRIBE, { endpoint });
      if (status !== 200) left.push(`a subscription is left on the server: POST ${UNSUBSCRIBE} answered HTTP ${status}`);
    } catch (e) {
      // apiPost's text: the method and the path, never the endpoint or the token.
      left.push(`a subscription is left on the server: ${(e as Error).message}`);
    }
  }
  return left;
}

/** The answer to the page's POST to `path`; a failure names the path (Playwright's own text may name the URL). */
function awaiting(page: Page, path: string, ms: number): Promise<Response> {
  const waiting = page.waitForResponse((r) => isPostTo(r.request(), path), { timeout: ms }).catch(() => {
    throw new Error(`POST ${path} did not come within ${ms / 1000} s`);
  });
  // Handled here too: a step that fails before the await leaves no unhandled rejection.
  waiting.catch(() => undefined);
  return waiting;
}

test.describe("push unsubscribe on the real PC (S7)", () => {
  test("logout revokes a real Web Push subscription in the browser and on the server", async ({ device, request }) => {
    const { page } = device;
    // The subscription goes only to the run's build: after "ide event" the predecessor answers at the same address.
    await expectBuild(request);
    const subscribing = awaiting(page, SUBSCRIBE, SUBSCRIBE_MS);
    await openLive(page, "engineer");
    const subscribe = await subscribing;
    expect(subscribe.status(), `POST ${SUBSCRIBE}`).toBe(200);
    const endpoint = endpointOf(bodyOf(subscribe.request()));
    expect(endpoint !== null, "the subscribe names an https:// push endpoint").toBe(true);
    expect((await browserEndpoint(page)) === endpoint, "the browser holds the subscription it posted").toBe(true);

    const unsubscribing = awaiting(page, UNSUBSCRIBE, UNSUBSCRIBE_MS);
    await page.locator(".settings-btn").click();
    await expect(page.locator(".settings-modal")).toBeVisible();
    await page.locator(".logout-btn").click();
    const unsubscribe = await unsubscribing;
    expect(unsubscribe.status(), `POST ${UNSUBSCRIBE}`).toBe(200);
    expect(endpointOf(bodyOf(unsubscribe.request())) === endpoint, "the unsubscribe names the subscribed endpoint").toBe(
      true,
    );
    expect(/^Bearer \S+$/.test(unsubscribe.request().headers()["authorization"] ?? ""), "with the token").toBe(true);

    // The logout lands on the start page, signed out, and the browser holds no subscription.
    await expect
      .poll(() => new URL(page.url()).pathname === "/", { message: "the logout lands on the start page" })
      .toBe(true);
    await expect(page.locator(".member-card").first()).toBeVisible();
    expect((await page.evaluate(() => localStorage.getItem("iem_token"))) === null, "the stored login is gone").toBe(
      true,
    );
    await expect
      .poll(async () => (await browserEndpoint(page)) === null, { message: "the browser's subscription is gone" })
      .toBe(true);
    device.relay.check();
  });
});
