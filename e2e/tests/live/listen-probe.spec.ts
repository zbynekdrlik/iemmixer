import { test, expect, type Page } from "../support/fixtures";
import { ANALYSER_INIT, audioError, audioLevel, readTone } from "./support/audio";
import { BurstWatch } from "./support/burst";
import { apiGet, live, openLive } from "./support/env";
import { relaySockets, type Relay } from "./support/relay";
import type { Tone } from "./support/tone";

// The listen probe on the real PC (S7, #10; design note section 6): the PC
// fires the HIL test signal with `--listen` in bursts, and the server sends
// its sine to `&hil=1` listen sockets only. The page's sockets go through the
// runner's relay (`relaySockets`, `/ws/audio` with `&hil=1`), a runner-side
// `BurstWatch` sees the bursts' edges and frames, and every measurement is
// taken inside a burst. Nothing here changes a mix or an input. Numbers leave
// a test only as `live_number` annotations (`live_verdict.py`).

type LiveNumber = "first_audio_ms" | "listen_hz" | "listen_dbfs" | "opus_frames";

/** Hands `key=value` to the verdict (the report's only numbers). */
function liveNumber(key: LiveNumber, value: string): void {
  test.info().annotations.push({ type: "live_number", description: `${key}=${value}` });
}

/** The middle of three or more values. */
function middle(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}

function listenButton(page: Page) {
  return page.locator(".toolbar-btn-listen");
}

/** Clicks Listen and waits (at most 3 s) until the player plays audio, checking the relay as it goes. */
async function listenPlays(page: Page, relay: Relay): Promise<void> {
  await listenButton(page).click();
  await expect
    .poll(
      async () => {
        relay.check();
        return audioLevel(page);
      },
      { timeout: 3_000, intervals: [100] },
    )
    .toBeGreaterThan(-100);
}

test.describe("the listen probe on the real PC (S7)", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  test("Listen on the engineer page plays audio within 3 s on the real PC", async ({ page, request }) => {
    const watch = await BurstWatch.open(request);
    try {
      const relay = await relaySockets(page, { hil: true });
      await openLive(page, "engineer");
      await watch.burst();
      const listen = listenButton(page);
      await expect(listen).toHaveText("🔊 Listen");

      const clicked = Date.now();
      await listen.click();
      let firstMs: number | null = null;
      while (Date.now() - clicked <= 3_000) {
        relay.check();
        if ((await audioLevel(page)) > -100) {
          firstMs = Date.now() - clicked;
          break;
        }
        await page.waitForTimeout(100);
      }
      if (firstMs !== null) liveNumber("first_audio_ms", String(firstMs));
      expect(firstMs, "the player plays audio within 3 s of the click").not.toBeNull();
      expect(firstMs as number).toBeLessThanOrEqual(3_000);
      expect(watch.inBurst(), "the audio came inside a burst").toBe(true);
      // The page's own socket is the `&hil=1` one: it was told the burst.
      expect(relay.statuses("/ws/audio")).toContain("probe");
      relay.check();
    } finally {
      watch.close();
    }
  });

  test("the listen probe plays 1 kHz at the burst level through the player within 1 Hz and 0.5 dB", async ({
    page,
    request,
  }) => {
    const { burstDbfs } = live();
    const watch = await BurstWatch.open(request);
    try {
      const relay = await relaySockets(page, { hil: true });
      await page.addInitScript(ANALYSER_INIT);
      await openLive(page, "engineer");
      await watch.burst();
      await listenPlays(page, relay);
      // Settling: the analyser's window (32768 samples, 0.68 s) then holds
      // only the burst as the player plays it.
      await page.waitForTimeout(1_000);

      const tones: Tone[] = [];
      for (let k = 1; k <= 3; k++) {
        if (k > 1) await page.waitForTimeout(300);
        expect(watch.inBurst(), `window ${k} starts inside the burst`).toBe(true);
        const tone = await readTone(page);
        expect(watch.inBurst(), `window ${k} ends inside the burst`).toBe(true);
        expect(tone.gap, `window ${k} has no dropout`).toBe(false);
        tones.push(tone);
      }
      const hz = middle(tones.map((t) => t.hz));
      const dbfs = middle(tones.map((t) => t.dbfs));
      liveNumber("listen_hz", hz.toFixed(3));
      liveNumber("listen_dbfs", dbfs.toFixed(3));
      expect(Math.abs(hz - 1000), `the median frequency ${hz.toFixed(3)} Hz`).toBeLessThanOrEqual(1);
      expect(Math.abs(dbfs - burstDbfs), `the median level ${dbfs.toFixed(3)} dBFS`).toBeLessThanOrEqual(0.5);
      relay.check();
    } finally {
      watch.close();
    }
  });

  test("#25 repeat: every listen frame in a burst is CELT Opus, 20 ms, stereo, and decodes without error", async ({
    page,
    request,
  }) => {
    const watch = await BurstWatch.open(request);
    try {
      const relay = await relaySockets(page, { hil: true });
      await openLive(page, "engineer");
      await watch.burst();
      await listenPlays(page, relay);
      // A second of the burst's frames (50 a second) at the watch.
      await expect.poll(() => watch.burstFrames, { timeout: 5_000, intervals: [100] }).toBeGreaterThanOrEqual(50);
      expect(watch.inBurst(), "the frames came inside a burst").toBe(true);

      const frames = watch.burstFrames;
      liveNumber("opus_frames", String(frames));
      expect(watch.badFrames, `frames of ${frames} failing the TOC check`).toBe(0);
      expect(relay.binaryFrames("/ws/audio"), "the page got frames").toBeGreaterThan(0);
      expect(await audioError(page), "the player's decoder error").toBeNull();

      const diagnostics = await apiGet(request, "engineer", "/api/audio/diagnostics");
      expect(diagnostics.status, "GET /api/audio/diagnostics").toBe(200);
      const d = diagnostics.body as { receiving_oiem?: unknown; packets_per_second?: unknown } | null;
      expect(d?.receiving_oiem, "receiving_oiem").toBe(true);
      expect(typeof d?.packets_per_second, "packets_per_second").toBe("number");
      expect(d?.packets_per_second as number).toBeGreaterThanOrEqual(45);
      relay.check();
    } finally {
      watch.close();
    }
  });
});
