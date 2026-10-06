import { test, expect } from "./support/fixtures";
import { openMixer, strip, tab } from "./support/session";
import { PageSocket } from "./support/wire";

// The engineer's pages against the real engine: restore with preview (F31),
// Mute All (F15), the Mixes tab (F16), SOS (F20), the console and the
// translator page (F29), and no band-activity banner however loud the stage
// (#38 — the engine's 1 kHz sine on every input).

test.describe.configure({ mode: "serial" });

test.describe("Engineer", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  test("a backup restore shows its changes first and puts them back (F31)", async ({ page }) => {
    const auth = await openMixer(page, "engineer", { engineer: true });
    const capture = await page.request.post("/api/backups/capture", {
      headers: { Authorization: `Bearer ${auth.token}` },
    });
    expect(capture.status()).toBe(200);

    // Change one level of the engineer's mix after the backup.
    await tab(page, "Mics");
    const mic2 = strip(page, "mic2");
    const wasMuted = ((await mic2.getAttribute("class")) ?? "").includes("muted");
    await mic2.locator(".mute-btn").click();
    if (wasMuted) await expect(mic2).not.toHaveClass(/muted/);
    else await expect(mic2).toHaveClass(/muted/);

    await page.locator(".settings-btn").click();
    const section = page.getByTestId("backup-section");
    const first = section.locator(".backup-list .settings-row").first();
    await expect(first).toBeVisible({ timeout: 10_000 });
    await first.click();
    await expect(section.locator(".backup-preview")).toContainText("values to restore");
    await expect(section.getByTestId("backup-diff")).toContainText("MEMBER2 mic");
    await section.locator(".backup-preview .settings-action-btn").click();
    await expect(section.locator(".backup-result")).toContainText("Restored", { timeout: 10_000 });

    await page.locator(".settings-modal .modal-close").click();
    if (wasMuted) await expect(strip(page, "mic2")).toHaveClass(/muted/, { timeout: 5_000 });
    else await expect(strip(page, "mic2")).not.toHaveClass(/muted/, { timeout: 5_000 });
  });

  test("the console sets an input's mute and links the translator page (F29)", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    await page.locator(".settings-btn").click();
    const consoleSection = page.getByTestId("console-section");
    const row = consoleSection.locator('.console-input[data-input="hand3"]');
    await expect(row).toBeVisible({ timeout: 10_000 });
    const mute = row.locator(".mute-btn");
    await expect(mute).toHaveClass(/off/);
    await mute.click();
    await expect(row.locator(".mute-btn")).toHaveClass(/on/);
    await row.locator(".mute-btn").click();
    await expect(row.locator(".mute-btn")).toHaveClass(/off/);

    await expect(consoleSection.locator(".console-limiter").first()).toBeVisible();
    await expect(consoleSection.locator(".console-logins")).toContainText("Failed logins");

    await consoleSection.locator(".console-page", { hasText: "Translator" }).click();
    await page.waitForURL("**/translator");
    await expect(page.locator(".mixer-header h1")).toHaveText("Translator");
    await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
  });

  test("Mute All mutes every channel of the engineer's mix (F15)", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    await page.locator(".toolbar-btn-mute-all").click();
    await tab(page, "Mics");
    await expect(strip(page, "mic1")).toHaveClass(/muted/, { timeout: 5_000 });
    await expect(strip(page, "keys")).toHaveClass(/muted/);
    await tab(page, "Stems");
    await expect(strip(page, "click")).toHaveClass(/muted/);
  });

  test("the Mixes tab is the engineer's and the elevated member's (F16)", async ({ page, browser }) => {
    await openMixer(page, "engineer", { engineer: true });
    await expect(page.locator(".category-tab.mixes")).not.toHaveClass(/tab-hidden/);
    await tab(page, "Mixes");
    await expect(strip(page, "member1")).toBeVisible();
    await expect(strip(page, "member9")).toBeVisible();

    const ctx = await browser.newContext();
    const member = await ctx.newPage();
    await openMixer(member, "member1");
    await expect(member.locator(".category-tab.mixes")).not.toHaveClass(/tab-hidden/);
    await tab(member, "Mixes");
    await expect(strip(member, "member2")).toBeVisible();
    await expect(strip(member, "member1")).toHaveCount(0);
    await ctx.close();
  });

  test("SOS reaches the engineer and clears (F20)", async ({ page, browser }) => {
    await openMixer(page, "engineer", { engineer: true });
    // A tap first: the alert's chime and vibration need a user gesture.
    await tab(page, "Mics");

    const ctx = await browser.newContext();
    const member = await ctx.newPage();
    await openMixer(member, "member2");
    const sos = member.locator(".alert-btn");
    await expect(sos).toHaveText("SOS");
    await sos.click();
    await expect(sos).toHaveText("SOS Active");

    const toast = page.locator(".alert-toast");
    await expect(toast).toContainText("Member2 needs help!", { timeout: 5_000 });
    await toast.locator(".alert-toast-dismiss").click();
    await expect(toast).toHaveCount(0);
    await expect(sos).toHaveText("SOS", { timeout: 5_000 });
    await ctx.close();
  });

  // #38 (owner, 2026-10-06): whether an event runs is the owner's to say, and
  // other devices on the Dante network feed the card's inputs, so no input
  // level means "the band plays". The engine's sine on every input, for 8 s
  // (the old alarm came up after 5 s on this site), shows the engineer no
  // banner, and the server sends the engineer's pages no such message.
  test("a loud stage shows the engineer no band-activity banner (#38)", async ({ page, baseURL }) => {
    const auth = await openMixer(page, "engineer", { engineer: true });
    const watch = await PageSocket.open(baseURL, "engineer", auth.token);
    try {
      await tab(page, "Mics");
      const fill = strip(page, "mic1").locator(".meter-fill").first();
      await expect
        .poll(
          async () => {
            const m = /width:\s*([\d.]+)%/.exec((await fill.getAttribute("style")) ?? "");
            return m ? Number(m[1]) : 0;
          },
          { timeout: 10_000 },
        )
        .toBeGreaterThan(0);
      // 80 meter frames (one per 100 ms): the stage loud, far above the old
      // alarm's −50 dBFS (0.0032), all along.
      const from = Date.now();
      await expect.poll(() => watch.peaks("mic1", from).length, { timeout: 15_000 }).toBeGreaterThanOrEqual(80);
      expect(Math.min(...watch.peaks("mic1", from))).toBeGreaterThan(0.01);
      expect(watch.events.map((e) => e.event)).not.toContain("BandActivity");
      await expect(page.getByTestId("band-activity")).toHaveCount(0);
      await expect(page.getByText("Kapela hrá")).toHaveCount(0);
    } finally {
      watch.close();
    }
  });

  test("after Mute All one channel can be unmuted, the rest stay muted (F15)", async ({ page }) => {
    const auth = await openMixer(page, "engineer", { engineer: true });
    const mixState = async () => {
      const resp = await page.request.get("/api/mixer/engineer", {
        headers: { Authorization: `Bearer ${auth.token}` },
      });
      expect(resp.status()).toBe(200);
      return (await resp.json()).channels as { id: string; muted: boolean }[];
    };
    await page.locator(".toolbar-btn-mute-all").click();
    await expect
      .poll(async () => (await mixState()).filter((c) => !c.muted).map((c) => c.id), { timeout: 5_000 })
      .toEqual([]);

    await tab(page, "Mics");
    const mic1 = strip(page, "mic1");
    await expect(mic1).toHaveClass(/muted/);
    await mic1.locator(".mute-btn").click();
    await expect(mic1).not.toHaveClass(/muted/);
    await expect.poll(async () => (await mixState()).filter((c) => !c.muted).map((c) => c.id)).toEqual(["mic1"]);
    const channels = await mixState();
    expect(channels.length).toBeGreaterThan(1);
    expect(channels.filter((c) => c.id !== "mic1").every((c) => c.muted)).toBe(true);

    // Back to all muted, as Mute All left it.
    await mic1.locator(".mute-btn").click();
    await expect(mic1).toHaveClass(/muted/);
    await expect.poll(async () => (await mixState()).filter((c) => !c.muted).map((c) => c.id)).toEqual([]);
  });

  test("an SOS stays on the engineer's page until dismissed (F20)", async ({ page, browser }) => {
    await openMixer(page, "engineer", { engineer: true });
    // A tap first: the alert's chime and vibration need a user gesture.
    await tab(page, "Mics");

    const ctx = await browser.newContext();
    const member = await ctx.newPage();
    await openMixer(member, "member7");
    const sos = member.locator(".alert-btn");
    await expect(sos).toHaveText("SOS");
    await sos.click();
    await expect(sos).toHaveText("SOS Active");

    const toast = page.locator(".alert-toast");
    await expect(toast).toContainText("Member7 needs help!", { timeout: 5_000 });
    // No auto-dismiss: still there after 6 s.
    await page.waitForTimeout(6_000);
    await expect(toast).toBeVisible();
    await expect(toast).toContainText("Member7 needs help!");
    await expect(sos).toHaveText("SOS Active");

    await toast.locator(".alert-toast-dismiss").click();
    await expect(toast).toHaveCount(0);
    await expect(sos).toHaveText("SOS", { timeout: 5_000 });
    await expect(sos).not.toHaveClass(/active/);
    await ctx.close();
  });

  test("an SOS vibrates the engineer's phone in one [500, 1000] × 30 pattern (F20)", async ({ page, browser }) => {
    // Every vibrate call of the engineer's page, as the page made it.
    await page.addInitScript(() => {
      const calls: unknown[] = [];
      (window as unknown as { __vibrateCalls: unknown[] }).__vibrateCalls = calls;
      Object.defineProperty(navigator, "vibrate", {
        configurable: true,
        value: (pattern: unknown) => {
          calls.push(Array.isArray(pattern) ? [...pattern] : pattern);
          return true;
        },
      });
    });
    await openMixer(page, "engineer", { engineer: true });
    // A tap first: the alert's chime needs a user gesture.
    await tab(page, "Mics");

    const ctx = await browser.newContext();
    const member = await ctx.newPage();
    await openMixer(member, "member7");
    const sos = member.locator(".alert-btn");
    await expect(sos).toHaveText("SOS");
    await sos.click();

    const toast = page.locator(".alert-toast");
    await expect(toast).toContainText("Member7 needs help!", { timeout: 5_000 });
    const calls = () => page.evaluate(() => (window as unknown as { __vibrateCalls: unknown[] }).__vibrateCalls);
    await expect.poll(async () => (await calls()).filter(Array.isArray).length).toBeGreaterThanOrEqual(1);
    const all = await calls();
    const pattern = Array.from({ length: 30 }, () => [500, 1000]).flat();
    expect(all.filter(Array.isArray)[0]).toEqual(pattern);
    // No single pulses (a 0 that cancels a vibration is allowed).
    expect(all.filter((c) => !Array.isArray(c) && c !== 0)).toEqual([]);

    await toast.locator(".alert-toast-dismiss").click();
    await expect(toast).toHaveCount(0);
    await expect(sos).toHaveText("SOS", { timeout: 5_000 });
    await ctx.close();
  });
});
