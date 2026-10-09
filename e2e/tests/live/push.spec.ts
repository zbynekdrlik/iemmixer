import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chromium, devices, type BrowserContext, type Page, type Response } from "@playwright/test";
import { test as base, expect } from "./support/live";
import { apiPost, expectBuild, openLive } from "./support/env";
import { relaySockets, type Relay } from "./support/relay";
import { USER_AGENT } from "./support/socket";
import { SUBSCRIBE, UNSUBSCRIBE, browserEndpoint, endpointOf, guardConsole, isPostTo } from "./support/push";

// Push unsubscribe on the real PC (S7, #10; row 717, reaperiem#188): the
// engineer's page subscribes to Web Push with the server's VAPID key, and
// logging out in the settings revokes the subscription in the browser and on
// the server. Playwright's contexts are incognito, where Chrome has no Push
// API, so this test runs its own persistent profile in a temp dir, with
// notifications granted, on the full Chromium (`channel: "chromium"`: the
// headless shell has no push service, its subscribe fails "push service not
// available"). Its console guard is its own and allows nothing. The page's
// sockets go through the runner's relay. A subscription the logout did not
// revoke is revoked from the runner at the fixture's teardown (it runs also
// after a timeout); `pc-end` compares the server's subscription counts as
// the backstop. No error here carries the endpoint (a capability), a URL or
// the token.

/** The subscribe is fire-and-forget after the page mounts (VAPID key, service worker, push service). */
const SUBSCRIBE_MS = 30_000;
/** The logout's unsubscribe POST follows the browser's own unsubscribe. */
const UNSUBSCRIBE_MS = 15_000;

/** The engineer's device: a persistent profile's page, its relay and its console. */
type Device = { page: Page; relay: Relay; problems: string[] };

const test = base.extend<{ device: Device }>({
  device: async ({ request }, use) => {
    const dir = mkdtempSync(join(tmpdir(), "iemmixer-live-push-"));
    // Endpoints the page posted to the server and the server has not been told to drop.
    const pending = new Set<string>();
    let context: BrowserContext | undefined;
    let failure: unknown = null;
    try {
      context = await chromium.launchPersistentContext(dir, {
        channel: "chromium",
        userAgent: USER_AGENT,
        viewport: devices["Desktop Chrome"].viewport,
        permissions: ["notifications"],
      });
      const page = context.pages()[0] ?? (await context.newPage());
      const problems = guardConsole(page);
      page.on("request", (r) => {
        if (!isPostTo(r, SUBSCRIBE)) return;
        const endpoint = endpointOf(r.postDataJSON());
        if (endpoint !== null) pending.add(endpoint);
      });
      page.on("response", (r) => {
        if (!isPostTo(r.request(), UNSUBSCRIBE) || r.status() !== 200) return;
        const endpoint = endpointOf(r.request().postDataJSON());
        if (endpoint !== null) pending.delete(endpoint);
      });
      const relay = await relaySockets(page);
      await use({ page, relay, problems });
      relay.check();
      expect(problems, "the persistent page's console must stay clean").toEqual([]);
    } catch (e) {
      failure = e;
    }
    // Teardown, also after a failure or a timeout: the server drops what the logout did not.
    const left: string[] = [];
    for (const endpoint of pending) {
      try {
        const { status } = await apiPost(request, "engineer", UNSUBSCRIBE, { endpoint });
        if (status !== 200) left.push(`POST ${UNSUBSCRIBE} answered HTTP ${status}`);
      } catch (e) {
        // apiPost's text: the method and the path, never the endpoint or the token.
        left.push((e as Error).message);
      }
    }
    await context?.close();
    rmSync(dir, { recursive: true, force: true });
    if (failure !== null) throw failure;
    if (left.length > 0) throw new Error(`a subscription is left on the server: ${left[0]}`);
  },
});

/** `waiting`'s response, or a failure naming `what` (Playwright's own text may name the URL). */
async function answered(waiting: Promise<Response>, what: string, ms: number): Promise<Response> {
  try {
    return await waiting;
  } catch {
    throw new Error(`${what} did not come within ${ms / 1000} s`);
  }
}

test.describe("push unsubscribe on the real PC (S7)", () => {
  test("logout revokes a real Web Push subscription in the browser and on the server", async ({ device }) => {
    const { page } = device;
    // The subscription goes only to the run's build: after "ide event" the predecessor answers at the same address.
    await expectBuild(page.request);
    const subscribing = page.waitForResponse((r) => isPostTo(r.request(), SUBSCRIBE), { timeout: SUBSCRIBE_MS });
    await openLive(page, "engineer");
    const subscribe = await answered(subscribing, `POST ${SUBSCRIBE}`, SUBSCRIBE_MS);
    expect(subscribe.status(), `POST ${SUBSCRIBE}`).toBe(200);
    const endpoint = endpointOf(subscribe.request().postDataJSON());
    expect(endpoint !== null, "the subscribe names an https:// push endpoint").toBe(true);
    expect((await browserEndpoint(page)) === endpoint, "the browser holds the subscription it posted").toBe(true);

    const unsubscribing = page.waitForResponse((r) => isPostTo(r.request(), UNSUBSCRIBE), {
      timeout: UNSUBSCRIBE_MS,
    });
    await page.locator(".settings-btn").click();
    await expect(page.locator(".settings-modal")).toBeVisible();
    await page.locator(".logout-btn").click();
    const unsubscribe = await answered(unsubscribing, `POST ${UNSUBSCRIBE}`, UNSUBSCRIBE_MS);
    expect(unsubscribe.status(), `POST ${UNSUBSCRIBE}`).toBe(200);
    expect(
      endpointOf(unsubscribe.request().postDataJSON()) === endpoint,
      "the unsubscribe names the subscribed endpoint",
    ).toBe(true);
    expect(/^Bearer \S+$/.test(unsubscribe.request().headers()["authorization"] ?? ""), "with the token").toBe(true);

    // The logout lands on the start page, signed out, and the browser holds no subscription.
    await expect.poll(() => new URL(page.url()).pathname, { message: "the page after the logout" }).toBe("/");
    await expect(page.locator(".member-card").first()).toBeVisible();
    expect(await page.evaluate(() => localStorage.getItem("iem_token")), "the stored login").toBeNull();
    await expect
      .poll(async () => (await browserEndpoint(page)) === null, { message: "the browser's subscription is gone" })
      .toBe(true);
    device.relay.check();
  });
});
