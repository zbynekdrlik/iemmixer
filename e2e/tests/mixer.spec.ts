import type { Locator } from "@playwright/test";
import { test, expect, Page } from "./support/fixtures";
import { dbText, dragFader, menu, openMixer, strip, tab } from "./support/session";

// The mixer page against the real engine (NullRt, 1 kHz sine on every input).
// Each describe uses its own member so the shared engine state never crosses
// tests: member3 here, member5 for pins and hides, member7 for the second
// IEM VOL persistence check (restored to its 0 dB default).

/** The fader track of a strip (or of IEM VOL / STEMS). */
function track(s: Locator): Locator {
  return s.locator(".fader-track").first();
}

/** The width (px) of a strip's fader fill. */
async function fillPx(s: Locator): Promise<number> {
  return track(s)
    .locator(".fader-fill")
    .evaluate((el) => el.getBoundingClientRect().width);
}

/** A strip's fader box, its vertical middle and the x at a fraction of its width. */
async function faderAt(s: Locator) {
  await s.scrollIntoViewIfNeeded();
  const box = await track(s).boundingBox();
  if (!box) throw new Error("fader track not laid out");
  return { box, y: box.y + box.height / 2, x: (fraction: number) => box.x + box.width * fraction };
}

/** The dB of a label ("-∞dB" is the −60 dB floor). */
function dbValue(label: string): number {
  return label === "-∞dB" ? -60 : Number(label.replace(/dB$/, ""));
}

/** IEM VOL's (or STEMS') level as the page holds it. */
async function levelOf(s: Locator): Promise<number> {
  return Number(await s.locator(".db-display").getAttribute("data-value"));
}

/** The width (%) of a meter fill. */
async function meterPct(fill: Locator): Promise<number> {
  const m = /width:\s*([\d.]+)%/.exec((await fill.getAttribute("style")) ?? "");
  return m ? Number(m[1]) : 0;
}

/**
 * Double-clicks a fader and waits until it rests at +0.0dB (the F5
 * animation, then the 300 ms guard after it).
 */
async function toZeroDb(page: Page, s: Locator): Promise<void> {
  await track(s).dblclick();
  await expect.poll(() => dbText(s), { timeout: 8_000 }).toBe("+0.0dB");
  await expect(track(s)).not.toHaveClass(/animating/);
  await page.waitForTimeout(400);
}

/**
 * Presses a fader at `from` (a fraction of its width), holds it past the
 * 150 ms activation and moves to `to` in `steps` moves; the button stays down.
 */
async function holdAndMove(page: Page, s: Locator, from: number, to: number, steps = 10): Promise<void> {
  const f = await faderAt(s);
  await page.mouse.move(f.x(from), f.y);
  await page.mouse.down();
  await page.waitForTimeout(350);
  for (let i = 1; i <= steps; i++) {
    await page.mouse.move(f.x(from + ((to - from) * i) / steps), f.y);
    await page.waitForTimeout(30);
  }
}

/**
 * Opens `member`'s mixer in another tab of the same browser (a second
 * device). Its console is held to the same rule as the test's page: `close`
 * fails on any error or warning it showed.
 */
async function secondTab(page: Page, member: string): Promise<{ page: Page; close: () => Promise<void> }> {
  const other = await page.context().newPage();
  const problems: string[] = [];
  other.on("console", (msg) => {
    if (msg.type() === "error" || msg.type() === "warning") problems.push(`[${msg.type()}] ${msg.text()}`);
  });
  other.on("pageerror", (error) => problems.push(`[pageerror] ${error.message}`));
  await other.goto(`/${member}`);
  await expect(other.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
  return {
    page: other,
    close: async () => {
      await other.close();
      expect(problems, "the second tab's console must stay clean").toEqual([]);
    },
  };
}

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

test.describe("Fader safety (F5)", () => {
  // member3's own strip mic3 on Main; each test starts from 0 dB.

  test("a tap does not move the fader (no jump to the tapped spot)", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await toZeroDb(page, own);
    const f = await faderAt(own);
    const before = await fillPx(own);
    // Far from the 0 dB handle (83 %): a jump there would be obvious.
    await page.mouse.click(f.x(0.25), f.y);
    await page.waitForTimeout(100);
    expect(Math.abs((await fillPx(own)) - before)).toBeLessThan(2);
    expect(await track(own).getAttribute("class")).not.toMatch(/\bactive\b/);
    await page.waitForTimeout(400);
    expect(await dbText(own)).toBe("+0.0dB");
    // Nothing reached the engine either.
    await page.reload();
    await expect(strip(page, "mic3")).toBeVisible({ timeout: 15_000 });
    await expect.poll(() => dbText(strip(page, "mic3"))).toBe("+0.0dB");
  });

  test("a hold activates the fader, then it moves with the finger", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await toZeroDb(page, own);
    const f = await faderAt(own);
    const atZero = await fillPx(own);
    await page.mouse.move(f.x(0.7), f.y);
    await page.mouse.down();
    await page.waitForTimeout(350);
    await expect(track(own)).toHaveClass(/\bactive\b/);
    await expect(own).toHaveClass(/fader-active/);
    // The press itself moved nothing (relative only).
    const atActivation = await fillPx(own);
    expect(Math.abs(atActivation - atZero)).toBeLessThan(2);
    for (let i = 1; i <= 10; i++) {
      await page.mouse.move(f.x(0.7 - 0.04 * i), f.y);
      await page.waitForTimeout(30);
    }
    // 40 % of the track to the left: the fill shrinks by about that much.
    expect(atActivation - (await fillPx(own))).toBeGreaterThan(f.box.width * 0.3);
    await page.mouse.up();
    await expect(track(own)).not.toHaveClass(/\bactive\b/);
    await expect(own).not.toHaveClass(/fader-active/);
  });

  test("the glow stays through a long drag and goes off on release", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await toZeroDb(page, own);
    const f = await faderAt(own);
    await page.mouse.move(f.x(0.2), f.y);
    await page.mouse.down();
    await page.waitForTimeout(350);
    await expect(track(own)).toHaveClass(/\bactive\b/);
    for (const to of [0.4, 0.6, 0.8]) {
      await page.mouse.move(f.x(to), f.y);
      await page.waitForTimeout(100);
      expect(await track(own).getAttribute("class"), `still active at ${to}`).toMatch(/\bactive\b/);
      expect(await own.getAttribute("class"), `still glowing at ${to}`).toMatch(/fader-active/);
    }
    // 0 dB plus 60 % of the 72 dB track: up against the +12 dB end.
    expect((await fillPx(own)) / f.box.width).toBeGreaterThan(0.95);
    expect(await dbText(own)).toBe("+12.0dB");
    await page.mouse.up();
    await expect(track(own)).not.toHaveClass(/\bactive\b/);
    await expect(own).not.toHaveClass(/fader-active/);
  });

  test("a released fader stays where it was let go (no snap-back)", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await toZeroDb(page, own);
    const f = await faderAt(own);
    await holdAndMove(page, own, 0.8, 0.2, 12);
    const atRelease = await fillPx(own);
    const label = await dbText(own);
    expect(dbValue(label)).toBeLessThan(-30);
    await page.mouse.up();
    const tolerance = f.box.width * 0.05;
    await page.waitForTimeout(500);
    expect(Math.abs((await fillPx(own)) - atRelease)).toBeLessThan(tolerance);
    await page.waitForTimeout(500);
    expect(Math.abs((await fillPx(own)) - atRelease)).toBeLessThan(tolerance);
    expect(await dbText(own)).toBe(label);
    // The engine has the released value.
    await page.reload();
    await expect(strip(page, "mic3")).toBeVisible({ timeout: 15_000 });
    await expect.poll(() => dbText(strip(page, "mic3"))).toBe(label);
  });

  test("a fast back-and-forth drag ends stable at the value shown at release", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await toZeroDb(page, own);
    const f = await faderAt(own);
    await page.mouse.move(f.x(0.5), f.y);
    await page.mouse.down();
    await page.waitForTimeout(350);
    for (let i = 0; i < 5; i++) {
      await page.mouse.move(f.x(0.3), f.y);
      await page.waitForTimeout(30);
      await page.mouse.move(f.x(0.7), f.y);
      await page.waitForTimeout(30);
    }
    // The last step goes down from the top end. A step that crosses an
    // integer dB shows the first integer it crosses, and a step down from an
    // integer shows no change (crossed_integer), so the level ends near
    // +12 dB, not where the finger stopped (about +5 dB). The test holds
    // gen1's check: no stutter or drift after release, and the engine keeps
    // the value shown at release.
    await page.mouse.move(f.x(0.6), f.y);
    await page.waitForTimeout(50);
    const atEnd = await fillPx(own);
    const label = await dbText(own);
    await page.mouse.up();
    await page.waitForTimeout(500);
    expect(Math.abs((await fillPx(own)) - atEnd)).toBeLessThan(f.box.width * 0.05);
    expect(await dbText(own)).toBe(label);
    // The value shown at release reached the engine.
    await page.reload();
    await expect(strip(page, "mic3")).toBeVisible({ timeout: 15_000 });
    await expect.poll(() => dbText(strip(page, "mic3"))).toBe(label);
  });

  test("a double-click animates to 0 dB (not an instant jump)", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await toZeroDb(page, own);
    await holdAndMove(page, own, 0.8, 0.2);
    await page.mouse.up();
    await page.waitForTimeout(400);
    const start = dbValue(await dbText(own));
    expect(start).toBeLessThan(-30);

    await track(own).dblclick();
    await expect(track(own)).toHaveClass(/animating/);
    // On its way: a level between the start and 0 dB.
    await expect
      .poll(
        async () => {
          const v = dbValue(await dbText(own));
          return v > start && v < 0;
        },
        { timeout: 2_000, intervals: [50] },
      )
      .toBe(true);
    await expect.poll(() => dbText(own), { timeout: 8_000 }).toBe("+0.0dB");
    await expect(track(own)).not.toHaveClass(/animating/);
    const inner = await track(own).evaluate((el) => el.clientWidth);
    expect(Math.abs(((await fillPx(own)) / inner) * 100 - 83.33)).toBeLessThan(5);
  });

  test("a touch during the animation stops it where it is", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await toZeroDb(page, own);
    await holdAndMove(page, own, 0.8, 0.2);
    await page.mouse.up();
    await page.waitForTimeout(400);

    await track(own).dblclick();
    await expect(track(own)).toHaveClass(/animating/);
    const f = await faderAt(own);
    await page.mouse.move(f.x(0.3), f.y);
    await page.mouse.down();
    await page.waitForTimeout(100);
    expect(await track(own).getAttribute("class")).not.toContain("animating");
    const stopped = await dbText(own);
    await page.mouse.up();
    await page.waitForTimeout(600);
    expect(await dbText(own)).toBe(stopped);
    expect(dbValue(stopped)).toBeLessThan(-20);
    // The engine holds the level the touch stopped at.
    await page.reload();
    await expect(strip(page, "mic3")).toBeVisible({ timeout: 15_000 });
    await expect.poll(() => dbText(strip(page, "mic3"))).toBe(stopped);
  });
});

test.describe("IEM VOL (F7)", () => {
  test("the IEM VOL fader has its fill and handle and drags", async ({ page }) => {
    await openMixer(page, "member3");
    const vol = page.getByTestId("global-volume-fader");
    await expect(vol.locator(".ch-name")).toHaveText("IEM VOL");
    await expect(track(vol)).toBeVisible();
    await expect(track(vol).locator(".fader-fill")).toBeAttached();
    await expect(track(vol).locator(".fader-handle")).toBeAttached();
    await toZeroDb(page, vol);
    await holdAndMove(page, vol, 0.5, 0.25);
    await page.mouse.up();
    // A quarter of the −60…+12 dB track: 18 dB down.
    await expect.poll(() => levelOf(vol)).toBeLessThan(-15);
    await toZeroDb(page, vol);
  });

  test("the IEM VOL mute toggles, reaches another tab and survives a reload", async ({ page }) => {
    await openMixer(page, "member3");
    const vol = page.getByTestId("global-volume-fader");
    const second = await secondTab(page, "member3");
    const other = second.page;
    const otherVol = other.getByTestId("global-volume-fader");
    await expect(vol).not.toHaveClass(/muted/);
    await expect(vol.locator(".mute-btn")).toHaveClass(/off/);

    await vol.locator(".mute-btn").click();
    await expect(vol.locator(".mute-btn")).toHaveClass(/\bon\b/);
    await expect(vol).toHaveClass(/muted/);
    await expect(otherVol).toHaveClass(/muted/);
    await page.reload();
    await expect(page.getByTestId("global-volume-fader")).toHaveClass(/muted/, { timeout: 15_000 });

    await page.getByTestId("global-volume-fader").locator(".mute-btn").click();
    await expect(page.getByTestId("global-volume-fader")).not.toHaveClass(/muted/);
    await expect(otherVol).not.toHaveClass(/muted/);
    await second.close();
  });

  test("the IEM VOL fader holds its position after release (no snap-back)", async ({ page }) => {
    await openMixer(page, "member3");
    const vol = page.getByTestId("global-volume-fader");
    await toZeroDb(page, vol);
    const f = await faderAt(vol);
    await holdAndMove(page, vol, 0.5, 0.2, 6);
    const held = await fillPx(vol);
    const heldLevel = await levelOf(vol);
    expect(heldLevel).toBeLessThan(-15);
    await page.mouse.up();
    await page.waitForTimeout(500);
    expect(Math.abs((await fillPx(vol)) - held)).toBeLessThan(f.box.width * 0.05);
    expect(await levelOf(vol)).toBe(heldLevel);
    await toZeroDb(page, vol);
  });

  for (const member of ["member3", "member7"]) {
    test(`${member}: IEM VOL persists after a reload`, async ({ page }) => {
      await openMixer(page, member);
      const vol = page.getByTestId("global-volume-fader");
      await toZeroDb(page, vol);
      const initial = await levelOf(vol);
      await holdAndMove(page, vol, 0.5, 0.25);
      await page.mouse.up();
      const set = await levelOf(vol);
      expect(Math.abs(set - initial)).toBeGreaterThan(0.5);
      // Another device gets it from the server …
      const second = await secondTab(page, member);
      const other = second.page;
      await expect.poll(() => levelOf(other.getByTestId("global-volume-fader"))).toBeCloseTo(set, 1);
      await second.close();
      // … and so does this page after a reload.
      await page.reload();
      const reloaded = page.getByTestId("global-volume-fader");
      await expect(reloaded).toBeVisible({ timeout: 15_000 });
      await expect.poll(() => levelOf(reloaded)).toBeCloseTo(set, 1);
      // Back to the 0 dB default.
      await toZeroDb(page, reloaded);
    });
  }
});

test.describe("STEMS fader (F7)", () => {
  test("the STEMS strip leads the Stems tab", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Stems");
    const stems = page.getByTestId("stems-volume-fader");
    await expect(stems).toBeVisible();
    await expect(stems.locator(".ch-name")).toHaveText("STEMS");
    await expect(stems.locator(".ch-type")).toHaveText("group");
    await expect(page.locator(".channels-grid > .channel").first()).toHaveAttribute(
      "data-testid",
      "stems-volume-fader",
    );
    await expect(strip(page, "click")).toBeVisible();
  });

  test("the STEMS strip is not on the Mics tab", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    await expect(strip(page, "mic1")).toBeVisible();
    await expect(page.getByTestId("stems-volume-fader")).toHaveCount(0);
  });

  test("the STEMS strip is not on the Tech tab", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Tech");
    await expect(strip(page, "hand1")).toBeVisible();
    await expect(page.getByTestId("stems-volume-fader")).toHaveCount(0);
  });
});

test.describe("Pan (F5)", () => {
  test("a centred pan shows the centre marker", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    const pan = own.locator(".pan-slider");
    await expect(pan).toHaveValue("50");
    await expect(pan).toHaveClass(/centered/);
    const tick = await own.locator(".pan-container").evaluate((el) => {
      const after = getComputedStyle(el, "::after");
      return { position: getComputedStyle(el).position, content: after.content, width: after.width };
    });
    expect(tick.position).toBe("relative");
    expect(tick.content).not.toBe("none");
    expect(tick.width).toBe("2px");
  });

  test("a pan double-click animates the slider back to the centre", async ({ page }) => {
    await openMixer(page, "member3");
    const second = await secondTab(page, "member3");
    const other = second.page;
    const pan = strip(page, "mic3").locator(".pan-slider");
    const otherPan = strip(other, "mic3").locator(".pan-slider");
    await expect(pan).toHaveValue("50");
    const box = await pan.boundingBox();
    expect(box).not.toBeNull();
    // A click on the slider's left part puts the pan there at once (mouse);
    // the thumb is 14 px wide.
    const left = { x: 7 + (box!.width - 14) * 0.1, y: box!.height / 2 };
    await pan.click({ position: left });
    await expect.poll(async () => Number(await pan.inputValue())).toBeLessThan(20);
    await expect(pan).not.toHaveClass(/centered/);
    await expect.poll(async () => Number(await otherPan.inputValue())).toBeLessThan(20);
    const before = Number(await pan.inputValue());

    // A double-click on the thumb (the value stays) starts the animation. On
    // its way: a value between the start and the centre (2 per 50 ms tick).
    await pan.dblclick({ position: left });
    await expect
      .poll(
        async () => {
          const v = Number(await pan.inputValue());
          return v > before && v < 50;
        },
        { timeout: 2_000, intervals: [50] },
      )
      .toBe(true);
    await expect.poll(async () => Number(await pan.inputValue()), { timeout: 3_000 }).toBe(50);
    await expect(pan).toHaveClass(/centered/);
    // The engine holds the centre: the other tab shows it.
    await expect(otherPan).toHaveValue("50");
    await second.close();
  });
});

test.describe("Menu, hide and solo (F6, F8)", () => {
  test("the kebab menu closes on a click outside it", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    await own.locator(".ch-menu-btn").click();
    const popup = own.locator(".ch-menu-popup");
    await expect(popup).toBeVisible();
    await expect(own).toHaveClass(/menu-open/);
    // On the middle of the Mics tab, far from the strip: the backdrop covers
    // the tab and takes the click (Playwright checks that it is the element
    // hit at that point).
    const mainTab = page.locator(".category-tab.main");
    const micsTab = page.locator(".category-tab.mics");
    await expect(mainTab).toHaveClass(/\bactive\b/);
    await expect(micsTab).not.toHaveClass(/\bactive\b/);
    const backdrop = page.locator(".ch-menu-backdrop");
    const mics = await micsTab.boundingBox();
    const cover = await backdrop.boundingBox();
    expect(mics).not.toBeNull();
    expect(cover).not.toBeNull();
    await backdrop.click({
      position: { x: mics!.x + mics!.width / 2 - cover!.x, y: mics!.y + mics!.height / 2 - cover!.y },
    });
    await expect(popup).toHaveCount(0);
    await expect(backdrop).toHaveCount(0);
    await expect(own).not.toHaveClass(/menu-open/);
    // Only the menu closed: the Mics tab under the click did not switch.
    await expect(mainTab).toHaveClass(/\bactive\b/);
    await expect(micsTab).not.toHaveClass(/\bactive\b/);
    await expect(strip(page, "mic1")).toHaveCount(0);
  });

  test("a muted channel can be hidden (reaperiem#78)", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    const strips = page.locator(".channels-grid > .channel[data-channel]");
    const mic2 = strip(page, "mic2");
    await expect(mic2).toBeVisible();
    await expect(mic2).not.toHaveClass(/muted/);
    const count = await strips.count();
    await mic2.locator(".mute-btn").click();
    await expect(mic2).toHaveClass(/muted/);

    await menu(mic2, "Hide");
    await expect(strip(page, "mic2")).toHaveCount(0);
    await expect(strips).toHaveCount(count - 1);
    const hiddenTab = page.locator(".category-tab.hidden");
    await expect(hiddenTab).not.toHaveClass(/tab-hidden/);
    await hiddenTab.click();
    await expect(strip(page, "mic2")).toBeVisible();
    await expect(strip(page, "mic2")).toHaveClass(/muted/);

    // Clean up: unhide and unmute.
    await menu(strip(page, "mic2"), "Unhide");
    await tab(page, "Mics");
    await expect(strip(page, "mic2")).toBeVisible();
    await strip(page, "mic2").locator(".mute-btn").click();
    await expect(strip(page, "mic2")).not.toHaveClass(/muted/);
  });

  test("solo is exclusive: a new solo replaces the previous one (reaperiem#131)", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    const [mic1, mic2] = [strip(page, "mic1"), strip(page, "mic2")];
    await expect(mic1.locator(".solo-btn")).toHaveClass(/off/);
    await expect(mic2.locator(".solo-btn")).toHaveClass(/off/);

    await mic1.locator(".solo-btn").click();
    await expect(mic1.locator(".solo-btn")).toHaveClass(/\bon\b/);
    await expect(mic2.locator(".solo-btn")).toHaveClass(/off/);
    await expect(mic2).toHaveClass(/muted/);

    await mic2.locator(".solo-btn").click();
    await expect(mic2.locator(".solo-btn")).toHaveClass(/\bon\b/);
    await expect(mic1.locator(".solo-btn")).toHaveClass(/off/);
    await expect(mic1).toHaveClass(/muted/);
    await expect(mic2).not.toHaveClass(/muted/);

    // The engine holds the new solo.
    await page.reload();
    await tab(page, "Mics");
    await expect(strip(page, "mic2").locator(".solo-btn")).toHaveClass(/\bon\b/, { timeout: 15_000 });
    await expect(strip(page, "mic1").locator(".solo-btn")).toHaveClass(/off/);
    await expect(strip(page, "mic1")).toHaveClass(/muted/);

    // Unsolo: nothing soloed, nothing masked.
    await strip(page, "mic2").locator(".solo-btn").click();
    await expect(strip(page, "mic2").locator(".solo-btn")).toHaveClass(/off/);
    await expect(strip(page, "mic1")).not.toHaveClass(/muted/);
    await expect(page.locator(".header-solo-btn")).toHaveCount(0);
  });
});

test.describe("Mixer page, the remaining gaps (W7)", () => {
  test("the own channel is first on Main, before a pinned one", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    // mic1 comes before mic3 in the engine's order; Main still puts mic3 first.
    await menu(strip(page, "mic1"), "Pin to Main");
    await tab(page, "Main");
    await expect(strip(page, "mic1")).toBeVisible();
    const strips = page.locator(".channels-grid > .channel");
    await expect(strips.nth(0)).toHaveAttribute("data-testid", "global-volume-fader");
    await expect(strips.nth(1)).toHaveAttribute("data-channel", "mic3");
    await expect(strips.nth(2)).toHaveAttribute("data-channel", "mic1");

    await menu(strip(page, "mic1"), "Unpin");
    await expect(strip(page, "mic1")).toHaveCount(0);
  });

  test("a channel muted before a solo is still muted after the unsolo", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    const [soloed, mutedFirst, other] = [strip(page, "mic1"), strip(page, "mic4"), strip(page, "mic2")];
    await expect(mutedFirst).not.toHaveClass(/muted/);
    await mutedFirst.locator(".mute-btn").click();
    await expect(mutedFirst).toHaveClass(/muted/);

    await soloed.locator(".solo-btn").click();
    await expect(soloed.locator(".solo-btn")).toHaveClass(/\bon\b/);
    await expect(other).toHaveClass(/muted/);
    await expect(mutedFirst).toHaveClass(/muted/);
    await expect(soloed).not.toHaveClass(/muted/);

    await soloed.locator(".solo-btn").click();
    await expect(soloed.locator(".solo-btn")).toHaveClass(/off/);
    await expect(other).not.toHaveClass(/muted/);
    await expect(mutedFirst).toHaveClass(/muted/);

    // The engine agrees: only mic4's own mute is left.
    await page.reload();
    await tab(page, "Mics");
    await expect(strip(page, "mic4")).toHaveClass(/muted/, { timeout: 15_000 });
    await expect(strip(page, "mic2")).not.toHaveClass(/muted/);
    await expect(strip(page, "mic1")).not.toHaveClass(/muted/);
    await expect(page.locator(".header-solo-btn")).toHaveCount(0);

    await strip(page, "mic4").locator(".mute-btn").click();
    await expect(strip(page, "mic4")).not.toHaveClass(/muted/);
  });

  test("a solo in one tab shows in another tab, whose header SOLO clears both", async ({ page }) => {
    await openMixer(page, "member3");
    const second = await secondTab(page, "member3");
    const other = second.page;
    await tab(page, "Mics");
    await tab(other, "Mics");
    await expect(other.locator(".header-solo-btn")).toHaveCount(0);

    await strip(page, "mic1").locator(".solo-btn").click();
    await expect(strip(page, "mic1").locator(".solo-btn")).toHaveClass(/\bon\b/);
    await expect(strip(other, "mic1").locator(".solo-btn")).toHaveClass(/\bon\b/, { timeout: 5_000 });
    await expect(strip(other, "mic2")).toHaveClass(/muted/);
    await expect(other.locator(".header-solo-btn")).toBeVisible();
    await expect(other.locator(".header-version")).toHaveCount(0);

    await other.locator(".header-solo-btn").click();
    await expect(other.locator(".header-version")).toBeVisible();
    await expect(page.locator(".header-solo-btn")).toHaveCount(0, { timeout: 5_000 });
    await expect(page.locator(".header-version")).toBeVisible();
    await expect(strip(page, "mic1").locator(".solo-btn")).toHaveClass(/off/);
    await expect(strip(page, "mic2")).not.toHaveClass(/muted/);
    await second.close();
  });

  test("a muted channel's meter still shows its input", async ({ page }) => {
    await openMixer(page, "member3");
    const own = strip(page, "mic3");
    const fill = own.locator(".meter-fill").first();
    await expect.poll(() => meterPct(fill), { timeout: 10_000 }).toBeGreaterThan(5);
    await own.locator(".mute-btn").click();
    await expect(own).toHaveClass(/muted/);
    // Longer than the bar's fall from the sine's level to nothing (20 dB/s).
    await page.waitForTimeout(3_000);
    for (let i = 0; i < 5; i++) {
      expect(await meterPct(fill)).toBeGreaterThan(5);
      await page.waitForTimeout(100);
    }
    await own.locator(".mute-btn").click();
    await expect(own).not.toHaveClass(/muted/);
  });

  test("IEM VOL is only on Main", async ({ page }) => {
    await openMixer(page, "member3");
    const vol = page.getByTestId("global-volume-fader");
    await tab(page, "Mics");
    await expect(strip(page, "mic1")).toBeVisible();
    await expect(vol).toHaveCount(0);
    await tab(page, "Stems");
    await expect(strip(page, "click")).toBeVisible();
    await expect(vol).toHaveCount(0);
    await tab(page, "Tech");
    await expect(strip(page, "hand1")).toBeVisible();
    await expect(vol).toHaveCount(0);
    await tab(page, "Main");
    await expect(vol).toBeVisible();
  });

  test("the stereo keys and content strips drag and mute from the UI", async ({ page }) => {
    await openMixer(page, "member3");
    await tab(page, "Mics");
    const keys = strip(page, "keys");
    // One strip for the pair, with both sides metering the engine's sine.
    await expect(keys).toHaveCount(1);
    await expect(keys.locator(".meter-bar")).toHaveCount(2);
    for (const side of [0, 1]) {
      await expect
        .poll(() => meterPct(keys.locator(".meter-fill").nth(side)), { timeout: 10_000 })
        .toBeGreaterThan(0);
    }
    const before = await dbText(keys);
    await keys.scrollIntoViewIfNeeded();
    await dragFader(page, keys, 0.2);
    await expect.poll(() => dbText(keys)).not.toBe(before);
    const set = await dbText(keys);
    await keys.locator(".mute-btn").click();
    await expect(keys).toHaveClass(/muted/);

    await tab(page, "Tech");
    const content = strip(page, "content");
    await expect(content).toHaveCount(1);
    await content.locator(".mute-btn").click();
    await expect(content).toHaveClass(/muted/);

    // The engine keeps both: another device of member3 shows them.
    const second = await secondTab(page, "member3");
    const other = second.page;
    await tab(other, "Mics");
    await expect(strip(other, "keys")).toHaveClass(/muted/);
    await expect.poll(() => dbText(strip(other, "keys"))).toBe(set);
    await tab(other, "Tech");
    await expect(strip(other, "content")).toHaveClass(/muted/);
    await second.close();

    // Clean up: unmute both (the keys level stays).
    await content.locator(".mute-btn").click();
    await expect(content).not.toHaveClass(/muted/);
    await tab(page, "Mics");
    await strip(page, "keys").locator(".mute-btn").click();
    await expect(strip(page, "keys")).not.toHaveClass(/muted/);
  });
});
