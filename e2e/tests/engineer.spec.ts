import { test, expect } from "./support/fixtures";
import { ENGINEER_PIN } from "./support/pins";
import { openMixer, strip, tab } from "./support/session";

// The engineer's pages against the real engine: restore with preview (F31),
// Mute All (F15), the Mixes tab (F16), SOS (F20), the console and the
// translator page (F29), and the band-activity banner with "Back to REAPER"
// (§4.2 — the test site's switch command is `true`; the engine's 1 kHz sine
// on every input is "the band playing").

test.describe.configure({ mode: "serial" });

test.describe("Engineer", () => {
  // The engineer page subscribes to Web Push; headless Chromium has no push
  // service, so the subscription's `[push] …` warnings are expected here.
  test.use({ allowedConsole: [/^\[push\] /] });

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

  test("the band-activity banner offers Back to REAPER (§4.2)", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    const banner = page.getByTestId("band-activity");
    await expect(banner).toContainText("Kapela hrá", { timeout: 30_000 });
    await banner.locator(".back-to-reaper-btn").click();
    await page.getByTestId("switch-pin").fill(ENGINEER_PIN);
    const answer = page.waitForResponse(
      (r) => r.url().endsWith("/api/mode/event") && r.request().method() === "POST",
    );
    await page.locator(".back-to-reaper-confirm").click();
    expect((await answer).status()).toBe(202);
    await expect(banner).toContainText("Prepína sa na REAPER");
  });
});
