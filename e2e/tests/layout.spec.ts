import type { Locator } from "@playwright/test";
import { test, expect } from "./support/fixtures";
import { openMixer, strip } from "./support/session";

// The channel strip's layout (F4, F5, F9) on member3's Main tab: IEM VOL
// (default 0 dB), the own channel mic3 and STEMS. Read-only except the
// muted-strip check, which unmutes mic3 again.

type Rect = { x: number; y: number; width: number; height: number };

/** The client rect of `locator` (also for a zero-width fill). */
async function rect(locator: Locator): Promise<Rect> {
  return locator.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { x: r.x, y: r.y, width: r.width, height: r.height };
  });
}

test.describe("Channel strip layout", () => {
  test("the fader has real dimensions and its fill bar shows the level", async ({ page }) => {
    await openMixer(page, "member3");
    const vol = page.getByTestId("global-volume-fader");
    const track = vol.locator(".fader-track");
    await expect(track).toBeVisible();
    const box = await track.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.width).toBeGreaterThan(50);
    expect(box!.height).toBeGreaterThanOrEqual(40);

    const fill = await rect(track.locator(".fader-fill"));
    expect(fill.height).toBeGreaterThan(0);
    // The fill spans the level's share of the −60…+12 dB track.
    const level = Number(await vol.locator(".db-display").getAttribute("data-value"));
    expect(level).toBeGreaterThan(-60);
    const inner = await track.evaluate((el) => el.clientWidth);
    expect(fill.width / inner).toBeCloseTo((level + 60) / 72, 1);

    const handle = await rect(track.locator(".fader-handle"));
    expect(handle.height).toBeGreaterThan(0);
  });

  test("the fader handle is a visible thumb (at least 12 px wide)", async ({ page }) => {
    await openMixer(page, "member3");
    for (const s of [page.getByTestId("global-volume-fader"), strip(page, "mic3")]) {
      const handle = s.locator(".fader-track .fader-handle");
      await expect(handle).toBeAttached();
      expect((await rect(handle)).width).toBeGreaterThanOrEqual(12);
    }
  });

  test("the controls sit above the fader (the finger never covers the dB)", async ({ page }) => {
    await openMixer(page, "member3");
    for (const s of [page.getByTestId("global-volume-fader"), strip(page, "mic3")]) {
      const db = await s.locator(".db-display").boundingBox();
      const fader = await s.locator(".fader-track").boundingBox();
      expect(db).not.toBeNull();
      expect(fader).not.toBeNull();
      expect(db!.y + db!.height).toBeLessThanOrEqual(fader!.y);
    }
  });

  test("the dB text is not clipped", async ({ page }) => {
    await openMixer(page, "member3");
    await expect(strip(page, "mic3")).toBeVisible();
    const displays = page.locator(".channel .db-display");
    // IEM VOL, mic3 and STEMS (and any pinned strip).
    expect(await displays.count()).toBeGreaterThanOrEqual(3);
    for (const d of await displays.all()) {
      await expect(d).toBeVisible();
      await expect(d).toHaveText(/dB$/);
      expect(await d.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(false);
    }
  });

  test("the kebab menu button is left of the channel name", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    const menuBtn = await own.locator(".ch-menu-btn").boundingBox();
    const label = await own.locator(".ch-label").boundingBox();
    expect(menuBtn).not.toBeNull();
    expect(label).not.toBeNull();
    expect(menuBtn!.x + menuBtn!.width).toBeLessThanOrEqual(label!.x);
  });

  test("a strip is position: relative (its disconnected overlay stays inside)", async ({ page }) => {
    await openMixer(page, "member3");
    await expect(strip(page, "mic3")).toBeVisible();
    const strips = page.locator(".channels-grid > .channel");
    expect(await strips.count()).toBeGreaterThanOrEqual(3);
    for (const s of await strips.all()) {
      expect(await s.evaluate((el) => getComputedStyle(el).position)).toBe("relative");
    }
  });

  test("a strip is a three-row grid: controls, meter, fader", async ({ page }) => {
    await openMixer(page, "member3");
    for (const s of [page.getByTestId("global-volume-fader"), strip(page, "mic3")]) {
      const rows = await s.evaluate((el) => getComputedStyle(el).gridTemplateRows);
      expect(rows.split(" ")).toHaveLength(3);
      const label = await s.locator(".ch-label").boundingBox();
      const meter = await s.locator(".meter-stereo").boundingBox();
      const fader = await s.locator(".fader-track").boundingBox();
      expect(label!.y).toBeLessThan(meter!.y);
      expect(meter!.y).toBeLessThan(fader!.y);
    }
  });

  test("the stereo meter is two thin bars above the fader", async ({ page }) => {
    await openMixer(page, "member3");
    for (const s of [page.getByTestId("global-volume-fader"), strip(page, "mic3")]) {
      const meter = s.locator(".meter-stereo");
      await expect(meter).toBeAttached();
      const box = await meter.boundingBox();
      expect(box).not.toBeNull();
      expect(box!.width).toBeGreaterThan(box!.height);
      expect(box!.width).toBeGreaterThan(50);
      expect(box!.height).toBeLessThanOrEqual(10);
      await expect(s.locator(".meter-bar")).toHaveCount(2);
      const fader = await s.locator(".fader-track").boundingBox();
      expect(box!.y).toBeLessThan(fader!.y);
    }
  });

  test("the meter fill is a gradient without a CSS transition (ballistics in Rust)", async ({
    page,
  }) => {
    await openMixer(page, "member3");
    const fill = strip(page, "mic3").locator(".meter-fill").first();
    await expect(fill).toBeAttached();
    const style = await fill.evaluate((el) => {
      const cs = getComputedStyle(el);
      return { image: cs.backgroundImage, durations: cs.transitionDuration.split(",").map((d) => d.trim()) };
    });
    expect(style.image).toContain("gradient");
    for (const d of style.durations) expect(d).toBe("0s");
  });

  test("a muted strip keeps full opacity; only its audio parts are dimmed", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await expect(own).not.toHaveClass(/muted/);
    await expect(own).not.toHaveClass(/disconnected/);
    await own.locator(".mute-btn").click();
    await expect(own).toHaveClass(/muted/);

    const look = await own.evaluate((el) => {
      const opacity = (sel: string) => getComputedStyle(el.querySelector(sel)!).opacity;
      return {
        strip: getComputedStyle(el).opacity,
        fader: opacity(".fader-area"),
        name: opacity(".ch-name"),
      };
    });
    expect(look.strip).toBe("1");
    expect(Number(look.fader)).toBeLessThan(0.5);
    expect(look.name).toBe("1");
    // The red edge marks it muted (the strip's box-shadow fades in).
    await expect
      .poll(() => own.evaluate((el) => getComputedStyle(el).boxShadow))
      .toMatch(/inset/);

    await own.locator(".mute-btn").click();
    await expect(own).not.toHaveClass(/muted/);
  });

  test("Main has no MY MIC label: the own strip is simply first", async ({ page }) => {
    await openMixer(page, "member3");
    await expect(strip(page, "mic3")).toBeVisible();
    await expect(page.locator(".main-section-label")).toHaveCount(0);
    await expect(page.getByText("MY MIC", { exact: true })).toHaveCount(0);
  });
});
