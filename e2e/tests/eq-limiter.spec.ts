import type { Locator } from "@playwright/test";
import { test, expect, Page } from "./support/fixtures";
import { EqBand, MixerSocket, withSocket } from "./support/mixer-socket";
import { login, menu, openMixer, strip, tab } from "./support/session";

// EQ (F11) and limiter (F12) against the real engine; member4 owns mic4 and
// mic5 (X7: a member may edit the EQ of their own inputs).

async function closeEq(page: Page): Promise<void> {
  await page.locator(".eq-close-btn").click();
  await expect(page.locator(".eq-modal")).toHaveCount(0);
}

/**
 * The EQ the slider tests work on: member4's second input. Main shows one own
 * channel (mic4, `view::channels`); mic5 is on the Mics tab.
 */
const EQ_TARGET = "mic5";

type SentEqBand = { cmd: string; target: string; band: number; param: string; value: number };

/** Init script: keeps every JSON command the page sends on its sockets. */
function recordSocketSends(): void {
  const w = window as unknown as { __iemSent: unknown[] };
  w.__iemSent = [];
  const send = WebSocket.prototype.send;
  WebSocket.prototype.send = function (this: WebSocket, data: string | ArrayBufferLike | Blob | ArrayBufferView) {
    if (typeof data === "string") w.__iemSent.push(JSON.parse(data));
    return send.call(this, data);
  };
}

/** The SetEqBand commands the page has sent. */
async function sentEq(page: Page): Promise<SentEqBand[]> {
  return page.evaluate(() =>
    (window as unknown as { __iemSent: SentEqBand[] }).__iemSent.filter((m) => m.cmd === "SetEqBand"),
  );
}

/** Opens the EQ of `EQ_TARGET` from its strip on the Mics tab; resolves once its five bands show. */
async function openEq(page: Page): Promise<Locator> {
  await tab(page, "Mics");
  await menu(strip(page, EQ_TARGET), "EQ");
  const modal = page.locator(".eq-modal");
  await expect(modal.locator(".eq-title")).toHaveText("EQ: MEMBER4 gtr");
  await expect(modal.locator(".eq-band-card")).toHaveCount(5);
  return modal;
}

/** Band card `i` in display order (the engine's order for the default layout). */
function card(modal: Locator, i: number): Locator {
  return modal.locator(".eq-band-card").nth(i);
}

/** The Freq, Gain or BW row of a band card. */
function row(bandCard: Locator, label: "Freq" | "Gain" | "BW"): Locator {
  return bandCard.locator(".eq-param-row").filter({ has: bandCard.page().locator(".eq-param-label", { hasText: label }) });
}

/** A `width` or `left` percentage from an element's inline style. */
async function stylePct(el: Locator, prop: "width" | "left"): Promise<number> {
  const style = (await el.getAttribute("style")) ?? "";
  const m = new RegExp(`${prop}:\\s*([\\d.]+)%`).exec(style);
  if (!m) throw new Error(`no ${prop} percentage in style "${style}"`);
  return Number(m[1]);
}

/** The gain label the modal shows for `db` ("+4.5 dB", "-3.7 dB"). */
function gainLabel(db: number): string {
  return db >= 0 ? `+${db.toFixed(1)} dB` : `${db.toFixed(1)} dB`;
}

/** Hz from a freq label ("322", "1.2k"). */
function labelHz(text: string): number {
  const k = /^([\d.]+)k$/.exec(text.trim());
  return k ? Number(k[1]) * 1000 : Number(text.trim());
}

/** The freq slider's position for `hz` on the UI's 20 Hz – 24 kHz log scale. */
function freqPosition(hz: number): number {
  const clamped = Math.max(20, Math.min(24000, hz));
  return (Math.log(clamped) - Math.log(20)) / (Math.log(24000) - Math.log(20));
}

/**
 * Presses a slider at `from` (a fraction of its width), holds it past the
 * 150 ms activation, moves `dx` px and releases (a finger's relative drag).
 */
async function dragSlider(page: Page, slider: Locator, dx: number, from = 0.5): Promise<void> {
  const box = await slider.boundingBox();
  if (!box) throw new Error("EQ slider not laid out");
  const x = box.x + box.width * from;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.waitForTimeout(300);
  await page.mouse.move(x + dx, y, { steps: 10 });
  await page.mouse.up();
}

test.describe("EQ (F11)", () => {
  test("a member opens the EQ of their own input and a band toggle persists", async ({ page }) => {
    await openMixer(page, "member4");
    await menu(strip(page, "mic4"), "EQ");
    const modal = page.locator(".eq-modal");
    await expect(modal).toBeVisible();
    await expect(modal.locator(".eq-title")).toHaveText("EQ: MEMBER4 mic");
    await expect(modal.locator(".eq-band-card")).toHaveCount(5);

    // The high-pass is the first card (filters first); enabling it bends the curve.
    const hpf = modal.locator(".eq-band-card").first();
    await expect(hpf.locator(".eq-band-type")).toHaveText("highpass");
    const curve = modal.locator("path").first();
    const flat = await curve.getAttribute("d");
    const toggle = hpf.locator(".eq-band-toggle");
    const wasOn = ((await toggle.getAttribute("class")) ?? "").includes(" on");
    await toggle.click();
    await expect(toggle).toHaveClass(wasOn ? /off/ : /on/);
    await expect.poll(() => curve.getAttribute("d")).not.toBe(flat);

    // Reopen: the engine kept it.
    await closeEq(page);
    await menu(strip(page, "mic4"), "EQ");
    const again = page.locator(".eq-modal .eq-band-card").first().locator(".eq-band-toggle");
    await expect(again).toHaveClass(wasOn ? /off/ : /on/, { timeout: 5_000 });

    // Restore.
    await again.click();
    await expect(again).toHaveClass(wasOn ? /on/ : /off/);
    await closeEq(page);
  });

  test("another member's input offers no EQ (X7)", async ({ page }) => {
    await openMixer(page, "member4");
    await tab(page, "Mics");
    const other = strip(page, "mic1");
    await other.locator(".ch-menu-btn").click();
    await expect(other.locator(".ch-menu-item", { hasText: "Pin to Main" })).toBeVisible();
    await expect(other.locator(".ch-menu-item", { hasText: "EQ" })).toHaveCount(0);
  });

  test("IEM VOL opens the mix's output EQ", async ({ page }) => {
    await openMixer(page, "member4");
    await page.getByTestId("global-volume-fader").locator(".eq-btn-small").click();
    await expect(page.locator(".eq-modal .eq-title")).toHaveText("EQ: IEM VOL");
    await expect(page.locator(".eq-modal .eq-band-card")).toHaveCount(5);
    await closeEq(page);
  });
});

// The predecessor's EQ checks (gen1 live/eq.spec.ts), on member4's mic5 (Mics tab). A
// second socket (support/mixer-socket.ts) seeds values and reads the server's
// EQ back; every test starts from the bands it found and puts them back.
test.describe("EQ sliders, values and reset (F11)", () => {
  let token = "";
  let saved: EqBand[] = [];

  const serverEq = (baseURL: string | undefined) =>
    withSocket(baseURL, "member4", token, (s) => s.eq(EQ_TARGET));

  test.beforeEach(async ({ page, baseURL }) => {
    await page.addInitScript(recordSocketSends);
    token = (await openMixer(page, "member4")).token;
    saved = await serverEq(baseURL);
    expect(saved.map((b) => b.band_type)).toEqual(["highpass", "lowshelf", "band", "band", "highshelf"]);
  });

  test.afterEach(async ({ page, baseURL }) => {
    // No more commands from the page, then the bands as they were.
    await page.close();
    await expect
      .poll(() => withSocket(baseURL, "member4", token, (s) => s.restoreEq(EQ_TARGET, saved)), {
        message: "mic5's EQ is restored",
        timeout: 10_000,
      })
      .toEqual(saved);
  });

  test("a slider drag sends SetEqBand and updates the value", async ({ page, baseURL }) => {
    const modal = await openEq(page);
    const gain = row(card(modal, 0), "Gain");
    const before = await gain.locator(".eq-param-value").innerText();
    await dragSlider(page, gain.locator(".eq-slider-track"), 40);
    await expect(gain.locator(".eq-param-value")).not.toHaveText(before);
    const shown = parseFloat(await gain.locator(".eq-param-value").innerText());

    const sent = (await sentEq(page)).filter((m) => m.param === "gain_db");
    expect(sent.length).toBeGreaterThan(0);
    expect(sent.every((m) => m.target === EQ_TARGET && m.band === 0)).toBe(true);
    // The drag's end sends the value the label shows, and the server keeps it.
    expect(sent[sent.length - 1].value).toBeCloseTo(shown, 4);
    await expect.poll(async () => (await serverEq(baseURL))[0].gain_db).toBeCloseTo(shown, 4);
  });

  test("the sliders never jump to a tap (safe touch activation)", async ({ page }) => {
    const modal = await openEq(page);
    await expect(modal.locator('.eq-band-card input[type="range"]')).toHaveCount(0);
    await expect(modal.locator(".eq-slider-track")).toHaveCount(15);

    // A tap far from the thumb, shorter than the 150 ms hold, moves nothing.
    const gain = row(card(modal, 1), "Gain");
    const track = gain.locator(".eq-slider-track");
    const fill = await stylePct(track.locator(".eq-slider-fill"), "width");
    const label = await gain.locator(".eq-param-value").innerText();
    const box = await track.boundingBox();
    if (!box) throw new Error("EQ slider not laid out");
    await track.click({ position: { x: box.width * 0.95, y: box.height / 2 } });
    await page.waitForTimeout(400);
    expect(await stylePct(track.locator(".eq-slider-fill"), "width")).toBe(fill);
    await expect(gain.locator(".eq-param-value")).toHaveText(label);
    expect(await sentEq(page)).toEqual([]);
  });

  test("a slider does not snap back during a drag or on release", async ({ page }) => {
    const modal = await openEq(page);
    const track = row(card(modal, 0), "Gain").locator(".eq-slider-track");
    const fill = track.locator(".eq-slider-fill");
    const initial = await stylePct(fill, "width");
    const step = initial >= 90 ? -5 : 5;
    const box = await track.boundingBox();
    if (!box) throw new Error("EQ slider not laid out");
    const x = box.x + box.width / 2;
    const y = box.y + box.height / 2;

    await page.mouse.move(x, y);
    await page.mouse.down();
    await page.waitForTimeout(300);
    for (let i = 1; i <= 8; i++) {
      await page.mouse.move(x + i * step, y);
      await page.waitForTimeout(30);
    }
    const midDrag = await stylePct(fill, "width");
    await page.mouse.up();
    await page.waitForTimeout(300);

    expect(midDrag).not.toBe(initial);
    expect(await stylePct(fill, "width")).toBe(midDrag);
    expect((await sentEq(page)).length).toBeGreaterThan(0);
  });

  test("a slider stays responsive after several drags (no stuck state)", async ({ page }) => {
    const modal = await openEq(page);
    const track = row(card(modal, 0), "Gain").locator(".eq-slider-track");
    for (let drag = 0; drag < 3; drag++) {
      const before = (await sentEq(page)).length;
      await dragSlider(page, track, drag % 2 === 0 ? 30 : -30);
      await expect
        .poll(async () => (await sentEq(page)).length, { message: `drag ${drag + 1} sends` })
        .toBeGreaterThan(before);
      await page.waitForTimeout(200);
    }
    await expect(track).not.toHaveClass(/\bactive\b/);
    await closeEq(page);
  });

  test("two drags in a row keep the page alive (no WASM panic)", async ({ page }) => {
    const modal = await openEq(page);
    const gain = row(card(modal, 0), "Gain");
    const track = gain.locator(".eq-slider-track");
    // Two drags with a pause for any echo between them; the console guard
    // fails the test on a panic or any other console error.
    for (let drag = 0; drag < 2; drag++) {
      const before = await gain.locator(".eq-param-value").innerText();
      await dragSlider(page, track, drag % 2 === 0 ? 30 : -30);
      await expect(gain.locator(".eq-param-value")).not.toHaveText(before);
      await page.waitForTimeout(500);
    }
    await expect(track).not.toHaveClass(/\bactive\b/);
  });

  test("the gain grid runs ±12 dB (not ±24 dB)", async ({ page }) => {
    const modal = await openEq(page);
    const labels = await modal.locator(".eq-curve-svg text").allTextContents();
    expect(labels).toEqual(expect.arrayContaining(["-12", "-6", "+0", "+6", "+12"]));
    expect(labels).not.toContain("-24");
    expect(labels).not.toContain("+24");
  });

  test("each band's toggle sends its own band index and the server switches that band", async ({
    page,
    baseURL,
  }) => {
    const modal = await openEq(page);
    for (let i = 0; i < 5; i++) await card(modal, i).locator(".eq-band-toggle").click();
    const toggles = (await sentEq(page)).filter((m) => m.param === "enabled");
    expect(toggles.map((m) => m.band)).toEqual([0, 1, 2, 3, 4]);
    expect(toggles.map((m) => m.value)).toEqual(saved.map((b) => (b.enabled ? 0 : 1)));
    await expect
      .poll(async () => (await serverEq(baseURL)).map((b) => b.enabled))
      .toEqual(saved.map((b) => !b.enabled));

    for (let i = 0; i < 5; i++) await card(modal, i).locator(".eq-band-toggle").click();
    await expect
      .poll(async () => (await serverEq(baseURL)).map((b) => b.enabled))
      .toEqual(saved.map((b) => b.enabled));
    await closeEq(page);
  });

  test("a gain change persists: the reopened modal and the server read it back", async ({ page, baseURL }) => {
    const modal = await openEq(page);
    const value = row(card(modal, 1), "Gain").locator(".eq-param-value");
    const before = await value.innerText();
    await dragSlider(page, row(card(modal, 1), "Gain").locator(".eq-slider-track"), parseFloat(before) >= 11 ? -50 : 50);
    await expect(value).not.toHaveText(before);
    const set = await value.innerText();
    await closeEq(page);

    const again = await openEq(page);
    await expect(row(card(again, 1), "Gain").locator(".eq-param-value")).toHaveText(set);
    await expect.poll(async () => (await serverEq(baseURL))[1].gain_db).toBeCloseTo(parseFloat(set), 4);
    await closeEq(page);
  });

  test("the gain label shows the server's value when the modal opens", async ({ page, baseURL }) => {
    const seeded = await withSocket(baseURL, "member4", token, async (s) => {
      s.setEq(EQ_TARGET, 1, "gain_db", -3.7);
      return (await s.eq(EQ_TARGET))[1].gain_db;
    });
    expect(seeded).toBeCloseTo(-3.7, 4);
    const modal = await openEq(page);
    const shown = await row(card(modal, 1), "Gain").locator(".eq-param-value").innerText();
    expect(shown).toBe("-3.7 dB");
    expect(Math.abs(parseFloat(shown) - seeded)).toBeLessThan(0.05);
  });

  test("the gain thumb sits where the shown dB is, also after close and reopen", async ({ page, baseURL }) => {
    await withSocket(baseURL, "member4", token, async (s) => {
      s.setEq(EQ_TARGET, 1, "gain_db", 4.5);
      expect((await s.eq(EQ_TARGET))[1].gain_db).toBeCloseTo(4.5, 4);
    });
    for (let open = 0; open < 2; open++) {
      const modal = await openEq(page);
      const gain = row(card(modal, 1), "Gain");
      const db = parseFloat(await gain.locator(".eq-param-value").innerText());
      expect(Math.abs(db)).toBeGreaterThan(0.5);
      await expect(gain.locator(".eq-param-value")).toHaveText(gainLabel(4.5));
      const thumb = (await stylePct(gain.locator(".eq-slider-thumb"), "left")) / 100;
      const expected = (Math.max(-12, Math.min(12, db)) + 12) / 24;
      expect(Math.abs(thumb - expected)).toBeLessThan(0.05);
      await closeEq(page);
    }
  });

  test("the freq thumb sits where the shown Hz is, stable across close and reopen", async ({ page, baseURL }) => {
    await withSocket(baseURL, "member4", token, async (s) => {
      s.setEq(EQ_TARGET, 1, "freq_hz", 322);
      expect((await s.eq(EQ_TARGET))[1].freq_hz).toBe(322);
    });
    const freqOf = (modal: Locator) =>
      row(modal.locator(".eq-band-card").filter({ has: page.locator(".eq-band-type", { hasText: "lowshelf" }) }), "Freq");

    let modal = await openEq(page);
    const text = (await freqOf(modal).locator(".eq-param-value").innerText()).trim();
    expect(text).toBe("322");
    const hz = labelHz(text);
    expect(hz).toBeGreaterThan(50);
    expect(hz).toBeLessThan(2000);
    const thumb = (await stylePct(freqOf(modal).locator(".eq-slider-thumb"), "left")) / 100;
    expect(Math.abs(thumb - freqPosition(hz))).toBeLessThan(0.02);
    await closeEq(page);

    modal = await openEq(page);
    const text2 = (await freqOf(modal).locator(".eq-param-value").innerText()).trim();
    const thumb2 = (await stylePct(freqOf(modal).locator(".eq-slider-thumb"), "left")) / 100;
    expect(text2).toBe(text);
    expect(Math.abs(thumb2 - thumb)).toBeLessThan(0.005);
    expect(Math.abs(thumb2 - freqPosition(labelHz(text2)))).toBeLessThan(0.02);
    await closeEq(page);
  });

  test("the gain slider runs ±12 dB end to end: a quarter of its width from 0 dB is +6 dB", async ({ page }) => {
    const modal = await openEq(page);
    const band = card(modal, 1);
    const gain = row(band, "Gain");
    await band.locator(".eq-band-reset").click();
    await expect(gain.locator(".eq-param-value")).toHaveText("+0.0 dB");
    expect(await stylePct(gain.locator(".eq-slider-thumb"), "left")).toBeCloseTo(50, 3);

    const track = gain.locator(".eq-slider-track");
    const box = await track.boundingBox();
    if (!box) throw new Error("EQ slider not laid out");
    await dragSlider(page, track, box.width * 0.25);
    const db = parseFloat(await gain.locator(".eq-param-value").innerText());
    expect(db).toBeLessThan(12);
    expect(Math.abs(db - 6)).toBeLessThanOrEqual(0.5);
    const thumb = await stylePct(gain.locator(".eq-slider-thumb"), "left");
    expect(Math.abs(thumb - 75)).toBeLessThanOrEqual(2.5);
  });

  test("the reset button moves the band's sliders to its defaults", async ({ page }) => {
    const modal = await openEq(page);
    const band = card(modal, 1);
    const gain = row(band, "Gain");
    const box = await gain.locator(".eq-slider-track").boundingBox();
    if (!box) throw new Error("EQ slider not laid out");
    await dragSlider(page, gain.locator(".eq-slider-track"), box.width * 0.5, 0.25);
    const before = await stylePct(gain.locator(".eq-slider-fill"), "width");
    expect(before).not.toBe(50);

    await band.locator(".eq-band-reset").click();
    await expect(gain.locator(".eq-param-value")).toHaveText("+0.0 dB");
    expect(await stylePct(gain.locator(".eq-slider-fill"), "width")).toBeCloseTo(50, 3);
    // The low shelf's defaults: 200 Hz, 2 octaves.
    await expect(row(band, "Freq").locator(".eq-param-value")).toHaveText("200");
    expect(await stylePct(row(band, "Freq").locator(".eq-slider-fill"), "width")).toBeCloseTo(
      freqPosition(200) * 100,
      1,
    );
    await expect(row(band, "BW").locator(".eq-param-value")).toHaveText("2.00 oct");
    expect(await stylePct(row(band, "BW").locator(".eq-slider-fill"), "width")).toBeCloseTo(
      ((2 - 0.01) / (4 - 0.01)) * 100,
      1,
    );
  });

  test("the reset button resets the server's values", async ({ page, baseURL }) => {
    const modal = await openEq(page);
    const band = card(modal, 1);
    const track = row(band, "Gain").locator(".eq-slider-track");
    const box = await track.boundingBox();
    if (!box) throw new Error("EQ slider not laid out");
    await dragSlider(page, track, box.width * 0.4, 0.3);
    await expect(row(band, "Gain").locator(".eq-param-value")).not.toHaveText("+0.0 dB");
    await band.locator(".eq-band-reset").click();
    await closeEq(page);
    await expect
      .poll(async () => {
        const b = (await serverEq(baseURL))[1];
        return [b.gain_db, b.freq_hz, b.bw];
      })
      .toEqual([0, 200, 2]);
  });

  test("a held slider puts the modal in movement mode (inset cue) until release", async ({ page }) => {
    const modal = await openEq(page);
    const shadow = () => modal.evaluate((el) => getComputedStyle(el).boxShadow);
    expect(await shadow()).not.toMatch(/4px\s+inset/);

    const track = row(card(modal, 0), "Freq").locator(".eq-slider-track");
    const box = await track.boundingBox();
    if (!box) throw new Error("EQ slider not laid out");
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    await expect(track).toHaveClass(/\bactive\b/);
    await expect.poll(shadow).toMatch(/4px\s+inset/);
    await page.mouse.up();
    await expect(track).not.toHaveClass(/\bactive\b/);
    await expect.poll(shadow).not.toMatch(/4px\s+inset/);
  });

  // gen1 live/eq.spec.ts "gain change on disabled band re-enables it (ReaEQ
  // behavior)"; ruled on #25 (FG-2): the band keeps the predecessor's
  // behaviour, so a gain the band member drags is heard.
  test("a gain drag on a disabled band enables it (FG-2)", async ({ page, baseURL }) => {
    const modal = await openEq(page);
    const band = card(modal, 1);
    const toggle = band.locator(".eq-band-toggle");
    if (saved[1].enabled) {
      await toggle.click();
      await expect.poll(async () => (await serverEq(baseURL))[1].enabled).toBe(false);
    }
    await expect(toggle).toHaveClass(/\boff\b/);

    const gain = row(band, "Gain");
    const before = await gain.locator(".eq-param-value").innerText();
    const box = await gain.locator(".eq-slider-track").boundingBox();
    if (!box) throw new Error("EQ slider not laid out");
    await dragSlider(page, gain.locator(".eq-slider-track"), box.width * 0.35, 0.25);
    await expect(gain.locator(".eq-param-value")).not.toHaveText(before);
    const set = parseFloat(await gain.locator(".eq-param-value").innerText());
    await closeEq(page);

    await expect
      .poll(async () => {
        const b = (await serverEq(baseURL))[1];
        return { enabled: b.enabled, gain_db: b.gain_db };
      })
      .toEqual({ enabled: true, gain_db: expect.closeTo(set, 4) });
    const again = await openEq(page);
    await expect(card(again, 1).locator(".eq-band-toggle")).toHaveClass(/\bon\b/);
    await expect(row(card(again, 1), "Gain").locator(".eq-param-value")).toHaveText(gainLabel(set));
    await closeEq(page);
  });

  test("the parameter labels are readable (#bbb or brighter, never #555)", async ({ page }) => {
    const modal = await openEq(page);
    const color = await modal.locator(".eq-param-label").first().evaluate((el) => getComputedStyle(el).color);
    expect(color).not.toBe("rgb(85, 85, 85)");
    const channels = (color.match(/\d+/g) ?? []).slice(0, 3).map(Number);
    expect(channels).toHaveLength(3);
    expect(Math.min(...channels)).toBeGreaterThanOrEqual(0xbb);
  });
});

test.describe("Limiter (F12)", () => {
  test("the limiter of the member's mix switches and keeps its state", async ({ page }) => {
    await openMixer(page, "member4");
    await page.getByTestId("global-volume-fader").locator(".limiter-btn-small").click();
    const modal = page.locator(".limiter-modal");
    await expect(modal.locator(".limiter-title")).toHaveText("IEM VOL — Limiter");
    const toggle = modal.locator(".limiter-toggle-btn");
    await expect(toggle).toHaveText("ON");
    await toggle.click();
    await expect(toggle).toHaveText("OFF");
    await expect(modal.locator(".limiter-warning")).toHaveText("HEARING PROTECTION OFF");

    await modal.locator(".limiter-close-btn").click();
    await page.getByTestId("global-volume-fader").locator(".limiter-btn-small").click();
    await expect(page.locator(".limiter-modal .limiter-toggle-btn")).toHaveText("OFF");

    // Restore hearing protection.
    await page.locator(".limiter-modal .limiter-toggle-btn").click();
    await expect(page.locator(".limiter-modal .limiter-toggle-btn")).toHaveText("ON");
    await expect(page.locator(".limiter-modal .limiter-activity-label")).toContainText("limited");
    await page.locator(".limiter-modal .limiter-close-btn").click();
  });

  // The CI half of gen1 live/limiter-activity.spec.ts (the card half runs on
  // the PC in S7): the engine's sine (−20 dBFS) with mic4's trim at +24 dB
  // and its level in member4's mix at +12 dB is far above the −6 dB limit.
  test("the counter adds up while the limiter holds the sine down, and Reset zeros it (X14)", async ({
    page,
    baseURL,
  }) => {
    test.setTimeout(90_000);
    const member = await openMixer(page, "member4");
    const engineer = await login(page, "engineer", true);
    const lim = page.getByTestId("global-volume-fader").locator(".limiter-btn-small");
    const modal = page.locator(".limiter-modal");
    const label = modal.locator(".limiter-activity-label");

    /** Opens the modal afresh (the counter comes with LimiterParams) and returns the counter's text. */
    const reopen = async (): Promise<string> => {
      if ((await modal.count()) > 0) {
        await modal.locator(".limiter-close-btn").click();
        await expect(modal).toHaveCount(0);
      }
      await lim.click();
      await expect(label).toBeVisible();
      return (await label.innerText()).trim();
    };

    const mix = await MixerSocket.open(baseURL, "member4", member.token);
    const desk = await MixerSocket.open(baseURL, "engineer", engineer.token);
    const level = (await mix.channels()).find((c) => c.id === "mic4")?.level_db;
    const trim = (await desk.consoleInputs()).find((i) => i.id === "mic4")?.trim_db;
    expect(level, "mic4's level in member4's mix").toBeDefined();
    expect(trim, "mic4's trim").toBeDefined();
    try {
      // Start from zero: the limiter is on and its counter reset.
      await reopen();
      await expect(modal.locator(".limiter-toggle-btn")).toHaveText("ON");
      await modal.locator(".limiter-reset-btn").click();
      await expect.poll(reopen, { timeout: 10_000 }).toBe("not limited yet");

      desk.send({ cmd: "SetInput", input: "mic4", trim_db: 24 });
      mix.send({ cmd: "SetLevel", id: "mic4", level_db: 12 });
      expect((await desk.consoleInputs()).find((i) => i.id === "mic4")?.trim_db).toBe(24);

      await expect
        .poll(async () => activeSeconds(await reopen()), {
          message: "the counter reaches 5 s while the limiter holds the sine down",
          timeout: 30_000,
          intervals: [1_000],
        })
        .toBeGreaterThanOrEqual(5);
      expect(await label.innerText()).toMatch(/^\d+\.\d sec limited$|^\d+ min \d+ sec limited$/);
      expect(await mix.limiterActiveSeconds()).toBeGreaterThanOrEqual(5);

      // The sine away (the limiter lets go within its 50 ms release), then Reset.
      desk.send({ cmd: "SetInput", input: "mic4", trim_db: trim });
      mix.send({ cmd: "SetLevel", id: "mic4", level_db: level });
      expect((await desk.consoleInputs()).find((i) => i.id === "mic4")?.trim_db).toBe(trim);
      await mix.limiterActiveSeconds();
      await page.waitForTimeout(1_000);
      await reopen();
      await modal.locator(".limiter-reset-btn").click();
      await expect(label).toHaveText("not limited yet");
      await expect.poll(reopen, { timeout: 10_000 }).toBe("not limited yet");
      expect(await mix.limiterActiveSeconds()).toBe(0);
      await modal.locator(".limiter-close-btn").click();
    } finally {
      desk.send({ cmd: "SetInput", input: "mic4", trim_db: trim });
      mix.send({ cmd: "SetLevel", id: "mic4", level_db: level });
      await desk.consoleInputs();
      await mix.limiterActiveSeconds();
      await desk.close();
      await mix.close();
    }
  });
});

/** Seconds from the limiter counter's text (`format_active` in limiter_modal.rs). */
function activeSeconds(text: string): number {
  if (text === "not limited yet") return 0;
  const secs = /^(\d+(?:\.\d+)?) sec limited$/.exec(text);
  if (secs) return Number(secs[1]);
  const minSecs = /^(\d+) min (\d+) sec limited$/.exec(text);
  if (minSecs) return Number(minSecs[1]) * 60 + Number(minSecs[2]);
  throw new Error(`unreadable limiter counter "${text}"`);
}
