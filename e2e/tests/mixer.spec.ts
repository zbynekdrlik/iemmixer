import { test, expect } from "./support/fixtures";
import { dbText, dragFader, menu, openMixer, strip, tab } from "./support/session";

// The mixer page against the real engine (NullRt, 1 kHz sine on every input).
// Each describe uses its own member so the shared engine state never crosses
// tests: member3 here, member5 for pins and hides.

test.describe("Mixer page (F4–F9)", () => {
  test("Main shows IEM VOL, the own channel first and STEMS (F4, F7)", async ({ page }) => {
    await openMixer(page, "member3");
    await expect(page.getByTestId("global-volume-fader")).toContainText("IEM VOL");
    await expect(page.getByTestId("stems-volume-fader")).toContainText("STEMS");
    const own = strip(page, "mic3");
    await expect(own).toBeVisible();
    await expect(own).toHaveClass(/more-me/);
    await expect(own.locator(".ch-name")).toHaveText("MEMBER3");
    await expect(own.locator(".ch-type")).toHaveText("mic");
    // Another member's channel is not on Main unless pinned.
    await expect(strip(page, "mic1")).toHaveCount(0);
  });

  test("tabs show their categories; stems start with CLICK and GUIDE (F4)", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    await expect(strip(page, "mic1")).toBeVisible();
    await expect(strip(page, "keys")).toBeVisible();
    await expect(strip(page, "click")).toHaveCount(0);

    await tab(page, "Stems");
    const names = page.locator(".channel[data-channel] .ch-name");
    await expect(names.nth(0)).toHaveText("CLICK");
    await expect(names.nth(1)).toHaveText("GUIDE");
    await expect(strip(page, "drums")).toBeVisible();

    await tab(page, "Tech");
    await expect(strip(page, "hand1")).toBeVisible();
    await expect(strip(page, "content")).toBeVisible();

    // Only a mix that hears other mixes has the Mixes tab.
    await expect(page.locator(".category-tab.mixes")).toHaveClass(/tab-hidden/);
  });

  test("a fader drag sets the level and it survives a reload (F5)", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    const before = await dbText(own);
    await dragFader(page, own, 0.3);
    await expect.poll(() => dbText(own)).not.toBe(before);
    const set = await dbText(own);
    // Wait until the server has it, then reload: the engine keeps the value.
    await page.waitForTimeout(500);
    await page.reload();
    await expect(strip(page, "mic3")).toBeVisible({ timeout: 15_000 });
    await expect.poll(() => dbText(strip(page, "mic3"))).toBe(set);
  });

  test("a double-click animates the fader to 0 dB (F5)", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    const s = strip(page, "mic2");
    await s.locator(".fader-track").dblclick();
    await expect.poll(() => dbText(s), { timeout: 8_000 }).toBe("+0.0dB");
  });

  test("mute toggles, persists and reaches a second tab (F5)", async ({ page, context }) => {
    await openMixer(page, "member3");
    const other = await context.newPage();
    await other.goto("/member3");
    await expect(strip(other, "mic3")).toBeVisible({ timeout: 15_000 });

    const own = strip(page, "mic3");
    await expect(own).not.toHaveClass(/muted/);
    await own.locator(".mute-btn").click();
    await expect(own).toHaveClass(/muted/);
    await expect(strip(other, "mic3")).toHaveClass(/muted/);

    await page.reload();
    await expect(strip(page, "mic3")).toHaveClass(/muted/, { timeout: 15_000 });

    await strip(page, "mic3").locator(".mute-btn").click();
    await expect(strip(page, "mic3")).not.toHaveClass(/muted/);
    await expect(strip(other, "mic3")).not.toHaveClass(/muted/);
    await other.close();
  });

  test("solo silences every other channel and the header SOLO clears it (F6)", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    const soloed = strip(page, "mic1");
    const other = strip(page, "mic2");
    await expect(other).not.toHaveClass(/muted/);
    await soloed.locator(".solo-btn").click();
    await expect(soloed.locator(".solo-btn")).toHaveClass(/on/);
    await expect(other).toHaveClass(/muted/);
    await expect(soloed).not.toHaveClass(/muted/);
    const clear = page.locator(".header-solo-btn");
    await expect(clear).toBeVisible();
    // The server's view agrees after a reload (the engine holds the mask).
    await page.reload();
    await tab(page, "Mics");
    await expect(strip(page, "mic2")).toHaveClass(/muted/, { timeout: 15_000 });
    await expect(page.locator(".header-solo-btn")).toBeVisible();

    await page.locator(".header-solo-btn").click();
    await expect(page.locator(".header-solo-btn")).toHaveCount(0);
    await expect(strip(page, "mic2")).not.toHaveClass(/muted/);
    await expect(strip(page, "mic1").locator(".solo-btn")).toHaveClass(/off/);
  });

  test("meters move with the engine's signal (F9)", async ({ page }) => {
    await openMixer(page, "member3");
    const fill = strip(page, "mic3").locator(".meter-fill").first();
    await expect
      .poll(
        async () => {
          const style = (await fill.getAttribute("style")) ?? "";
          const m = /width:\s*([\d.]+)%/.exec(style);
          return m ? Number(m[1]) : 0;
        },
        { timeout: 10_000 },
      )
      .toBeGreaterThan(0);
  });
});

test.describe("Pins and hides (F8)", () => {
  test("pin to Main, hide from a tab, both kept by the server", async ({ page }) => {
    await openMixer(page, "member5");
    await tab(page, "Mics");
    await menu(strip(page, "mic1"), "Pin to Main");
    await menu(strip(page, "keys"), "Hide");
    await expect(strip(page, "keys")).toHaveCount(0);
    await expect(page.locator(".category-tab.hidden")).not.toHaveClass(/tab-hidden/);

    await tab(page, "Main");
    await expect(strip(page, "mic1")).toBeVisible();

    // Another device (a reload) gets them from the server.
    await page.reload();
    await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
    await expect(strip(page, "mic1")).toBeVisible();
    await page.locator(".category-tab.hidden").click();
    await expect(strip(page, "keys")).toBeVisible();

    // Clean up: unhide and unpin.
    await menu(strip(page, "keys"), "Unhide");
    await tab(page, "Main");
    await menu(strip(page, "mic1"), "Unpin");
    await expect(strip(page, "mic1")).toHaveCount(0);
  });
});
