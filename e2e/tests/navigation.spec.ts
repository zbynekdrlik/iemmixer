import { test, expect, Page } from "./support/fixtures";
import { menu, openMixer, strip } from "./support/session";

// Leaving a mixer page disposes its reactive graph while its socket,
// intervals and modals are live (gen1 live/navigation-back-disposal.spec.ts,
// reaperiem#153). After each navigation a 3 s settle must show no panic
// overlay, post no /api/client-error report (the panic hook's), open no new
// mixer socket from a leftover reconnect interval, and log nothing (the
// console guard). The engineer's page runs the most background work.

const SETTLE_MS = 3_000;

test.describe("Leaving a mixer page (reaperiem#153)", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  /** Records the panic hook's reports; they still reach the server. */
  async function clientErrorReports(page: Page): Promise<string[]> {
    const posts: string[] = [];
    await page.route("**/api/client-error", async (route) => {
      if (route.request().method() === "POST") posts.push(route.request().postData() ?? "(empty)");
      await route.continue();
    });
    return posts;
  }

  /** Records when each WebSocket of the page opened. */
  function socketOpenings(page: Page): { url: string; at: number }[] {
    const opened: { url: string; at: number }[] = [];
    page.on("websocket", (ws) => opened.push({ url: ws.url(), at: Date.now() }));
    return opened;
  }

  /** The mixer is connected, has its channels and has handled socket messages. */
  async function waitForStreamingMixer(page: Page): Promise<void> {
    await expect(page.locator(".app.mixer .mixer-header")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".status-dot.connected")).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".disconnected-banner")).toHaveCount(0);
    await expect(page.locator(".channel").first()).toBeVisible({ timeout: 15_000 });
    // Let meter frames run the socket's onmessage before leaving.
    await page.waitForTimeout(500);
  }

  async function expectLanding(page: Page): Promise<void> {
    await expect(page).toHaveURL(/\/$/);
    await expect(page.locator(".member-card").first()).toBeVisible({ timeout: 10_000 });
  }

  async function settleClean(page: Page, reports: string[]): Promise<void> {
    await page.waitForTimeout(SETTLE_MS);
    await expect(page.locator("#iem-panic-overlay")).toHaveCount(0);
    await expect(page.getByText("IEM Mixer encountered an error")).toHaveCount(0);
    expect(reports, "no /api/client-error report").toEqual([]);
  }

  function mixerSocketsSince(opened: { url: string; at: number }[], since: number): string[] {
    return opened.filter((s) => s.at >= since && /\/ws\//.test(s.url)).map((s) => s.url);
  }

  test("mixer → landing with the browser's back: no panic, no report, no reconnect", async ({ page }) => {
    const reports = await clientErrorReports(page);
    const opened = socketOpenings(page);
    await openMixer(page, "engineer", { engineer: true });
    await waitForStreamingMixer(page);

    const left = Date.now();
    await page.goBack();
    await expectLanding(page);
    await settleClean(page, reports);
    expect(mixerSocketsSince(opened, left)).toEqual([]);
  });

  test("mixer → landing with the page's back button: no panic, no report, no reconnect", async ({ page }) => {
    const reports = await clientErrorReports(page);
    const opened = socketOpenings(page);
    await openMixer(page, "engineer", { engineer: true });
    await waitForStreamingMixer(page);

    const left = Date.now();
    await page.locator(".app.mixer .back-btn").click();
    await expectLanding(page);
    await settleClean(page, reports);
    expect(mixerSocketsSince(opened, left)).toEqual([]);
  });

  test("mixer → another member's mixer: no panic, no report", async ({ page }) => {
    const reports = await clientErrorReports(page);
    await openMixer(page, "engineer", { engineer: true });
    await waitForStreamingMixer(page);

    await page.goto("/member2");
    await waitForStreamingMixer(page);
    await expect(page.locator(".mixer-header h1")).toHaveText("Member2");
    await settleClean(page, reports);
    await expect(page.locator(".status-dot.connected")).toBeVisible();
  });

  test("mixer → landing → mixer three times: nothing accumulates", async ({ page }) => {
    const reports = await clientErrorReports(page);
    const opened = socketOpenings(page);
    await openMixer(page, "engineer", { engineer: true });
    let left = 0;
    for (let i = 0; i < 3; i++) {
      if (i > 0) await page.goto("/engineer");
      await waitForStreamingMixer(page);
      left = Date.now();
      await page.goBack();
      await expectLanding(page);
      await page.waitForTimeout(500);
    }
    await settleClean(page, reports);
    expect(mixerSocketsSince(opened, left)).toEqual([]);
  });

  test("an open EQ modal, then back: no disposal panic", async ({ page }) => {
    const reports = await clientErrorReports(page);
    const opened = socketOpenings(page);
    await openMixer(page, "engineer", { engineer: true });
    await waitForStreamingMixer(page);

    // The engineer's own input on Main; the modal's drag and read handlers
    // are live when the page goes.
    await menu(strip(page, "eng_mic"), "EQ");
    await expect(page.locator(".eq-modal .eq-title")).toHaveText("EQ: ENGINEER mic");
    await expect(page.locator(".eq-modal .eq-band-card")).toHaveCount(5);

    const left = Date.now();
    await page.goBack();
    await expectLanding(page);
    await expect(page.locator(".eq-modal")).toHaveCount(0);
    await settleClean(page, reports);
    expect(mixerSocketsSince(opened, left)).toEqual([]);
  });
});
