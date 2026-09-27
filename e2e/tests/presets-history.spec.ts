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

  // gen1 live/preset-input.spec.ts (reaperiem#110): typed with the keyboard,
  // not `.fill()`, so a page-wide key handler that swallowed digits or
  // Backspace would show here; the name saved is the name typed.
  test("the name input takes digits and Backspace from the keyboard", async ({ page }) => {
    const auth = await openMixer(page, "member6");
    const modal = await openPresets(page);
    const input = modal.locator("input.preset-input");
    await input.click();
    await page.keyboard.type("Mix 123");
    await expect(input).toHaveValue("Mix 123");
    for (let i = 0; i < 3; i++) await page.keyboard.press("Backspace");
    await expect(input).toHaveValue("Mix ");

    await page.keyboard.type("7");
    await expect(input).toHaveValue("Mix 7");
    const one = `/api/presets/member6/${encodeURIComponent("Mix 7")}`;
    try {
      await modal.locator(".preset-save-btn").click();
      await expect(modal.locator(".preset-item", { hasText: "Mix 7" })).toBeVisible();
      await expect(input).toHaveValue("");
    } finally {
      await page.request.delete(one, { headers: { Authorization: `Bearer ${auth.token}` } });
    }
    const gone = await page.request.get(one, { headers: { Authorization: `Bearer ${auth.token}` } });
    expect(gone.status()).toBe(404);
  });

  // gen1 live/preset-confirm-ui.spec.ts (reaperiem#206): Slovak buttons, and
  // overwrite and delete ask first; Cancel changes nothing.
  test("Slovak labels; overwrite and delete ask first and Cancel changes nothing", async ({ page }) => {
    const auth = await openMixer(page, "member6");
    const headers = { Authorization: `Bearer ${auth.token}` };
    const probe = "E2E confirm probe";
    const one = `/api/presets/member6/${encodeURIComponent(probe)}`;
    const created = await page.request.post("/api/presets/member6", { headers, data: { name: probe } });
    expect(created.status()).toBe(201);
    try {
      const updatedAt = async (): Promise<number> => {
        const r = await page.request.get(one, { headers });
        expect(r.status()).toBe(200);
        return (await r.json()).updated_at as number;
      };
      const original = await updatedAt();

      const modal = await openPresets(page);
      const item = modal.locator(".preset-item", { hasText: probe });
      await expect(item).toBeVisible();
      await expect(item.locator(".load-preset")).toHaveText("Načítať");
      await expect(item.locator(".update-preset")).toHaveText("Prepísať");
      await expect(item.locator(".delete-preset")).toHaveText("Zmazať");
      await expect(modal.locator(".preset-save-btn")).toHaveText("Uložiť ako nový");

      // Saving under the existing name asks to overwrite; Cancel keeps the
      // preset as it was (an overwrite now would stamp a later second).
      await expect.poll(() => Math.floor(Date.now() / 1000)).toBeGreaterThan(original);
      await modal.locator(".preset-input").fill(probe);
      await modal.locator(".preset-save-btn").click();
      const dialog = page.locator(".confirm-overlay.visible");
      await expect(dialog.locator(".confirm-title")).toHaveText("Prepísať preset?");
      await expect(dialog.locator(".confirm-body")).toContainText(probe);
      await expect(dialog.locator(".confirm-btn-confirm")).toHaveText("Potvrdiť");
      await expect(dialog.locator(".confirm-btn-cancel")).toHaveText("Zrušiť");
      await dialog.locator(".confirm-btn-cancel").click();
      await expect(dialog).toHaveCount(0);
      expect(await updatedAt()).toBe(original);
      await expect(item).toHaveCount(1);

      // Delete asks too; Cancel keeps it.
      await item.locator(".delete-preset").click();
      await expect(dialog.locator(".confirm-title")).toHaveText("Zmazať preset?");
      await dialog.locator(".confirm-btn-cancel").click();
      await expect(dialog).toHaveCount(0);
      await expect(item).toBeVisible();
      expect(await updatedAt()).toBe(original);

      // History: Slovak title, buttons and a row's verbs.
      await modal.locator(".modal-close").click();
      const listed = await page.request.get("/api/snapshots/member6", { headers });
      expect(listed.status()).toBe(200);
      const before = ((await listed.json()) as unknown[]).length;
      await page.locator(".toolbar-btn", { hasText: "History" }).click();
      const history = page.locator(".modal-overlay.visible .snapshot-modal");
      await expect(history.locator("h2")).toHaveText("História mixu");
      await expect(history.locator(".snapshot-save-btn")).toHaveText("Uložiť teraz");
      await history.locator(".snapshot-save-btn").click();
      await expect(history.locator(".snapshot-item")).toHaveCount(before + 1);
      const first = history.locator(".snapshot-item").first();
      await expect(first.locator(".restore-btn")).toHaveText("Obnoviť");
      await expect(first.locator(".delete-btn")).toHaveText("Zmazať");
      await expect(first.locator(".snapshot-pin-btn")).toHaveCount(1);
      await expect(first.locator(".snapshot-pin-btn")).toHaveText(/^(Pripnúť|Odopnúť)$/);
    } finally {
      await page.request.delete(one, { headers });
    }
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
