import { test, expect, Page } from "./support/fixtures";
import { menu, openMixer, strip } from "./support/session";

// The band's phones: this spec runs only in the `phone` project
// (playwright.config.ts: 375 × 667, touch). member3's page, read-only: the
// modals are opened and closed, never saved.

const WIDTH = 375;

/** The project's contract: a narrow portrait touch screen. */
async function expectPhone(page: Page): Promise<void> {
  const screen = await page.evaluate(() => ({
    width: window.innerWidth,
    coarse: matchMedia("(pointer: coarse) and (orientation: portrait)").matches,
  }));
  expect(screen).toEqual({ width: WIDTH, coarse: true });
}

async function openPresets(page: Page) {
  await page.locator(".toolbar-btn", { hasText: "Presets" }).click();
  const modal = page.locator(".modal-overlay.visible .modal");
  await expect(modal).toBeVisible();
  await expect(modal.locator("h2")).toHaveText("Presety");
  return modal;
}

test.describe("Phone (375 px, touch)", () => {
  test("the landing and mixer pages render on a phone without errors", async ({ page }) => {
    const response = await page.goto("/");
    expect(response?.status()).toBe(200);
    await expectPhone(page);
    await expect(page.locator(".member-card").first()).toBeVisible();
    const grid = await page.locator(".member-grid").evaluate((el) => ({
      overflow: el.scrollWidth > el.clientWidth,
      right: el.getBoundingClientRect().right,
    }));
    expect(grid.overflow).toBe(false);
    expect(grid.right).toBeLessThanOrEqual(WIDTH);

    await openMixer(page, "member3");
    await expect(strip(page, "mic3")).toBeVisible();
    const overflow = await page.locator(".channels-scroll").evaluate((el) => el.scrollWidth > el.clientWidth);
    expect(overflow, "no strip is wider than the phone").toBe(false);
    for (const s of [page.getByTestId("global-volume-fader"), strip(page, "mic3")]) {
      const box = await s.boundingBox();
      expect(box).not.toBeNull();
      expect(box!.x).toBeGreaterThanOrEqual(0);
      expect(box!.x + box!.width).toBeLessThanOrEqual(WIDTH);
    }
  });

  test("the presets modal is sized in percent with a 340 px cap, not in viewport units", async ({
    page,
  }) => {
    await openMixer(page, "member3");
    await expectPhone(page);
    const modal = await openPresets(page);
    // The declaration itself: 100 % of the overlay, at most 340 px (a
    // `100vw` width overflowed real phones).
    const declared = await page.evaluate(() => {
      for (const sheet of Array.from(document.styleSheets)) {
        for (const rule of Array.from(sheet.cssRules)) {
          if (
            rule instanceof CSSStyleRule &&
            rule.selectorText.split(",").some((s) => s.trim() === ".modal") &&
            rule.style.width
          ) {
            return { width: rule.style.width, maxWidth: rule.style.maxWidth };
          }
        }
      }
      return null;
    });
    expect(declared).toEqual({ width: "100%", maxWidth: "340px" });
    const sizes = await modal.evaluate((el) => {
      const overlay = el.parentElement!;
      const pad = getComputedStyle(overlay);
      return {
        maxWidth: getComputedStyle(el).maxWidth,
        width: el.getBoundingClientRect().width,
        available: overlay.clientWidth - parseFloat(pad.paddingLeft) - parseFloat(pad.paddingRight),
        overflow: el.scrollWidth > el.clientWidth,
      };
    });
    expect(sizes.maxWidth).toBe("340px");
    // Below the cap the modal takes the whole width the overlay leaves.
    expect(Math.abs(sizes.width - Math.min(340, sizes.available))).toBeLessThan(1);
    expect(sizes.overflow).toBe(false);
  });

  test("the preset input row and its input can shrink (min-width: 0)", async ({ page }) => {
    await openMixer(page, "member3");
    const modal = await openPresets(page);
    const row = await modal.locator(".preset-input-row").evaluate((el) => getComputedStyle(el).minWidth);
    const input = await modal.locator(".preset-input").evaluate((el) => getComputedStyle(el).minWidth);
    expect(row).toBe("0px");
    expect(input).toBe("0px");
  });

  test("the presets modal fits the 375 px screen with a margin on both sides", async ({ page }) => {
    await openMixer(page, "member3");
    await expectPhone(page);
    const modal = await openPresets(page);
    const box = await modal.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.x).toBeGreaterThanOrEqual(0);
    expect(box!.x + box!.width).toBeLessThanOrEqual(WIDTH);
    expect(box!.x).toBeGreaterThan(10);
    expect(WIDTH - (box!.x + box!.width)).toBeGreaterThan(10);
    expect(await modal.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(false);
  });

  test("the save button stays on screen and inside the modal, even with a long name", async ({
    page,
  }) => {
    await openMixer(page, "member3");
    await expectPhone(page);
    const modal = await openPresets(page);
    // A long name must shrink the input, not push the button out (the
    // band's complaint). Typed only; nothing is saved.
    await modal.locator(".preset-input").fill("A very long preset name for the whole Sunday service");
    const save = modal.locator(".preset-save-btn");
    await expect(save).toBeVisible();
    const btn = await save.boundingBox();
    expect(btn).not.toBeNull();
    expect(btn!.x).toBeGreaterThanOrEqual(0);
    expect(btn!.x + btn!.width).toBeLessThanOrEqual(WIDTH);
    const clipped = await save.evaluate((el) => {
      const rect = el.getBoundingClientRect();
      const parent = el.closest(".modal")!.getBoundingClientRect();
      return rect.right > parent.right || rect.left < parent.left;
    });
    expect(clipped).toBe(false);
  });

  test("EQ band cards stack one per row on a portrait phone", async ({ page }) => {
    await openMixer(page, "member3");
    await expectPhone(page);
    await menu(strip(page, "mic3"), "EQ");
    const modal = page.locator(".eq-modal");
    await expect(modal).toBeVisible();
    const cards = modal.locator(".eq-band-card");
    await expect(cards).toHaveCount(5);
    const geom = await cards.first().evaluate((first) => {
      const all = Array.from(first.parentElement!.querySelectorAll<HTMLElement>(".eq-band-card"));
      const parent = first.parentElement!;
      const style = getComputedStyle(parent);
      const [a, b] = [all[0].getBoundingClientRect(), all[1].getBoundingClientRect()];
      return {
        card0: a.width,
        card1: b.width,
        inner: parent.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
        bottom0: a.bottom,
        top1: b.top,
      };
    });
    // Equal widths, each the full row: one card per row, not two.
    expect(Math.abs(geom.card0 - geom.card1)).toBeLessThan(2);
    expect(geom.card0 / geom.inner).toBeGreaterThan(0.9);
    expect(geom.top1).toBeGreaterThanOrEqual(geom.bottom0);
    await page.locator(".eq-close-btn").click();
    await expect(modal).toHaveCount(0);
  });
});
