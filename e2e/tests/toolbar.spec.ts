import { test, expect, Page } from "./support/fixtures";
import { openMixer } from "./support/session";

// The bottom toolbar by role and page: the engineer's own page has Listen,
// Mute All and Talk; the engineer on a member's page only Listen (that
// member's mix); a member has Presets, History and SOS. Pages are only
// looked at here, nothing is pressed.

function toolbar(page: Page) {
  const bar = page.locator(".toolbar");
  return {
    bar,
    listen: bar.locator(".toolbar-btn-listen"),
    muteAll: bar.locator(".toolbar-btn-mute-all"),
    talk: bar.locator(".toolbar-btn-talk"),
    presets: bar.locator(".toolbar-btn", { hasText: "Presets" }),
    history: bar.locator(".toolbar-btn", { hasText: "History" }),
    sos: page.locator(".alert-btn"),
  };
}

test.describe("Toolbar by role and page", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  test("the engineer's own page: Listen, Mute All and Talk; no Presets, History or SOS", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    const t = toolbar(page);
    await expect(t.bar).toBeVisible();
    await expect(t.listen).toBeVisible();
    await expect(t.listen).toContainText("Listen");
    await expect(t.muteAll).toBeVisible();
    // Mute All is an icon button.
    await expect(t.muteAll).toHaveText("🔇");
    await expect(t.talk).toBeVisible();
    await expect(t.talk).toHaveText("🎤 Talk");
    await expect(t.presets).toHaveCount(0);
    await expect(t.history).toHaveCount(0);
    await expect(t.sos).toHaveCount(0);
  });

  test("the engineer on a member's page: Listen only; no Mute All, Talk, Presets, History or SOS", async ({
    page,
  }) => {
    await openMixer(page, "engineer", { engineer: true, path: "member2" });
    const t = toolbar(page);
    await expect(t.bar).toBeVisible();
    await expect(t.listen).toBeVisible();
    await expect(t.listen).toContainText("Listen");
    await expect(t.muteAll).toHaveCount(0);
    await expect(t.talk).toHaveCount(0);
    await expect(t.presets).toHaveCount(0);
    await expect(t.history).toHaveCount(0);
    await expect(t.sos).toHaveCount(0);
  });

  test("a member: Presets, History and SOS; no Listen, Mute All or Talk", async ({ page }) => {
    await openMixer(page, "member2");
    const t = toolbar(page);
    await expect(t.bar).toBeVisible();
    await expect(t.presets).toBeVisible();
    await expect(t.history).toBeVisible();
    await expect(t.sos).toBeVisible();
    await expect(t.sos).toHaveText("SOS");
    await expect(t.listen).toHaveCount(0);
    await expect(t.muteAll).toHaveCount(0);
    await expect(t.talk).toHaveCount(0);
  });
});
