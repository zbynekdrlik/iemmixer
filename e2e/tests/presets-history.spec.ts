import { test, expect, Page } from "./support/fixtures";
import { openMixer, strip } from "./support/session";

// Presets (F13) and mix history (F14) of member6 (own channel mic7): the
// server captures the mix, keeps it in the band files and loads it as a
// 50 ms ramp.

async function openPresets(page: Page) {
  await page.locator(".toolbar-btn", { hasText: "Presets" }).click();
  const modal = page.locator(".modal-overlay.visible .modal");
  await expect(modal.locator("h2")).toHaveText("Presety");
  return modal;
}

async function confirm(page: Page) {
  const dialog = page.locator(".confirm-overlay.visible");
  await expect(dialog).toBeVisible();
  await dialog.locator(".confirm-btn-confirm").click();
  await expect(dialog).toHaveCount(0);
}

test.describe("Presets (F13)", () => {
  test("save, load, overwrite and delete", async ({ page }) => {
    await openMixer(page, "member6");
    const own = strip(page, "mic7");
    await expect(own).not.toHaveClass(/muted/);

    // Save the current (unmuted) mix.
    let modal = await openPresets(page);
    await modal.locator(".preset-input").fill("E2E set");
    await modal.locator(".preset-save-btn").click();
    const item = modal.locator(".preset-item", { hasText: "E2E set" });
    await expect(item).toBeVisible();
    await modal.locator(".modal-close").click();

    // Change the mix, then load the preset: the server ramps it back.
    await own.locator(".mute-btn").click();
    await expect(own).toHaveClass(/muted/);
    modal = await openPresets(page);
    await modal.locator(".preset-item", { hasText: "E2E set" }).locator(".load-preset").click();
    await expect(page.locator(".modal-overlay.visible")).toHaveCount(0);
    await expect(own).not.toHaveClass(/muted/, { timeout: 5_000 });

    // Saving under an existing name asks first; overwrite keeps one entry.
    modal = await openPresets(page);
    await modal.locator(".preset-item", { hasText: "E2E set" }).locator(".update-preset").click();
    await confirm(page);
    await expect(modal.locator(".preset-item", { hasText: "E2E set" })).toHaveCount(1);

    // Delete.
    await modal.locator(".preset-item", { hasText: "E2E set" }).locator(".delete-preset").click();
    await confirm(page);
    await expect(modal.locator(".preset-item", { hasText: "E2E set" })).toHaveCount(0);
  });

  test("an empty name is refused with a Slovak message", async ({ page }) => {
    await openMixer(page, "member6");
    const modal = await openPresets(page);
    await modal.locator(".preset-input").fill("   ");
    await modal.locator(".preset-save-btn").click();
    await expect(modal.locator(".snapshot-error")).toHaveText("Zadajte názov presetu.");
  });
});

test.describe("History (F14)", () => {
  test("save now, pin, restore", async ({ page }) => {
    await openMixer(page, "member6");
    await page.locator(".toolbar-btn", { hasText: "History" }).click();
    const modal = page.locator(".modal-overlay.visible .snapshot-modal");
    await expect(modal.locator("h2")).toHaveText("História mixu");
    const before = await modal.locator(".snapshot-item").count();
    await modal.locator(".snapshot-save-btn").click();
    await expect(modal.locator(".snapshot-item")).toHaveCount(before + 1);
    const manual = modal.locator(".snapshot-item", { hasText: "manual" }).first();
    await expect(manual).toBeVisible();

    await manual.locator(".snapshot-pin-btn").click();
    await expect(modal.locator(".snapshot-item.pinned").first()).toBeVisible();
    await expect(modal.locator(".snapshot-item.pinned .snapshot-pin-btn").first()).toHaveText("Odopnúť");

    await modal.locator(".snapshot-item.pinned .restore-btn").first().click();
    await expect(page.locator(".modal-overlay.visible")).toHaveCount(0);
  });
});
