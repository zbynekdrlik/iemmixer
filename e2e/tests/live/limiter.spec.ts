import { test, expect, liveNumber, type Page } from "./support/live";
import type { Cmd } from "./support/desk";
import { live, openLive } from "./support/env";

// The limiter counter on the real PC (S7, #10; row 611, X14): a burst drives
// one member's mix over its limit, the counter counts, and Reset in the page
// zeros it. The burst's sine (−20 dBFS) reaches the mix at unity with its
// input dry and open, +12 dB from its level and +12.04 dB at 1 kHz from the
// mix's EQ: +4 dBFS into a limit of −6 dB. Every change happens inside the
// burst, while every mix's TX is zero, so no in-ear hears the drive, and goes
// back before the burst ends (`desk.during`): the drive first, the limiter
// after the Reset. The page is LIVE_MEMBER's, opened with the engineer's
// token through the relay; the runner's own socket on it sets the mix. The
// number leaves the test only as a `live_number` annotation (`live_verdict.py`).

/** The mix's level of the burst's input during the drive (the fader's top). */
const DRIVE_LEVEL_DB = 12;
/** The mix EQ's peak band during the drive: +12.04 dB (linear 4, the EQ's top) at the burst's 1 kHz. */
const DRIVE_EQ = { freq_hz: 1000, gain_db: 12.04 };
/** The counter grows at least this much … */
const MIN_COUNT_S = 1.5;
/** … within this long of the drive. */
const COUNT_MS = 5_000;

/** A UI step inside the burst waits at most this long (and never past the burst, `desk.bound`). */
const UI_MS = 5_000;

/** Opens the limiter of the page's mix from IEM VOL; resolves with the modal once its counter shows. */
async function openLimiter(page: Page, timeout: number) {
  await page.getByTestId("global-volume-fader").locator(".limiter-btn-small").click({ timeout });
  const modal = page.locator(".limiter-modal");
  await expect(modal.locator(".limiter-activity-label")).toBeVisible({ timeout });
  return modal;
}

async function closeLimiter(page: Page, timeout: number): Promise<void> {
  await page.locator(".limiter-modal .limiter-close-btn").click({ timeout });
  await expect(page.locator(".limiter-modal")).toHaveCount(0, { timeout });
}

/** One EQ band value (`SetEqBand`): `param` is freq_hz, gain_db, bw_oct or enabled. */
function eqBand(target: string, band: number, param: string, value: number): Cmd {
  return { cmd: "SetEqBand", target, band, param, value };
}

test.describe("the limiter on the real PC (S7)", () => {
  // The page is opened with the engineer's token, so it subscribes to Web
  // Push; Playwright's browser contexts are incognito, where Chrome has no
  // Push API: its error and the page's `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  test("the limiter counter counts while a burst drives the mix over its limit, and Reset zeros it", async ({
    page,
    relay,
    desk,
  }) => {
    const { member, testInput } = live();
    await openLive(page, "engineer", member);
    const mixer = await desk.open("engineer", member);
    const mix = mixer.mixId();

    await desk.during(async () => {
      // What every change puts back, read now, inside the burst.
      const limiter = await mixer.limiter();
      const input = await mixer.consoleInput(testInput);
      if (!input) throw new Error("LIVE_TEST_INPUT is not an input of the console");
      const channel = mixer.channel(testInput);
      if (!channel) throw new Error("LIVE_TEST_INPUT is not a channel of LIVE_MEMBER's page");
      // A solo's mask would hide the drive, and it shows as the channel's mute.
      if (mixer.soloed().length > 0) throw new Error("LIVE_MEMBER's mix has a solo");
      const bands = await mixer.eq(mix);
      // A peak band gives its whole gain at its frequency (a shelf only half).
      const band = bands.findIndex((b) => b.band_type === "band");
      if (band < 0) throw new Error("LIVE_MEMBER's mix EQ has no peak band");
      const b = bands[band];

      // The limiter on at −6 dB (the slider's 0); it goes back after the Reset.
      desk.change(
        mixer,
        [
          { cmd: "SetLimiterEnabled", enabled: true },
          { cmd: "SetLimiterParam", param: "limit", value: 0 },
        ],
        [
          { cmd: "SetLimiterParam", param: "limit", value: limiter.limit_norm },
          { cmd: "SetLimiterEnabled", enabled: limiter.enabled },
        ],
      );
      await mixer.applied();
      // The counter before the drive: the band's own count since its last Reset.
      const start = await mixer.activeSeconds();

      const drive = desk.mark();
      desk.change(
        mixer,
        { cmd: "SetInput", input: testInput, processing: false, muted: false },
        { cmd: "SetInput", input: testInput, processing: input.processing, muted: input.muted },
      );
      desk.change(
        mixer,
        [
          { cmd: "SetMute", id: testInput, muted: false },
          { cmd: "SetLevel", id: testInput, level_db: DRIVE_LEVEL_DB },
        ],
        [
          { cmd: "SetLevel", id: testInput, level_db: channel.level_db },
          { cmd: "SetMute", id: testInput, muted: channel.muted },
        ],
      );
      // A gain switches the band on (FG-2): its switch goes back last.
      desk.change(
        mixer,
        [
          eqBand(mix, band, "freq_hz", DRIVE_EQ.freq_hz),
          eqBand(mix, band, "gain_db", DRIVE_EQ.gain_db),
          eqBand(mix, band, "enabled", 1),
        ],
        [
          eqBand(mix, band, "freq_hz", b.freq_hz),
          eqBand(mix, band, "gain_db", b.gain_db),
          eqBand(mix, band, "enabled", b.enabled ? 1 : 0),
        ],
      );
      await mixer.applied();
      await expect
        .poll(
          async () => {
            relay.check();
            return (await mixer.activeSeconds()) - start;
          },
          {
            message: "the counter grows while the burst drives the mix over its limit",
            timeout: desk.bound(COUNT_MS),
            intervals: [250],
          },
        )
        .toBeGreaterThanOrEqual(MIN_COUNT_S);

      // The drive away. The limiter lets go at once, but the counter follows
      // the GR meter (X14: below −1 dB), which climbs back ~8.7 dB a second:
      // Reset once the counter holds still over half a second.
      await desk.restore(drive);
      let last = -1;
      await expect
        .poll(
          async () => {
            const was = last;
            last = await mixer.activeSeconds();
            return last === was;
          },
          { message: "the counter stops once the drive is away", timeout: desk.bound(10_000), intervals: [500] },
        )
        .toBe(true);
      liveNumber("limiter_active_s", last - start, 1);

      // Reset in the page, as the engineer does; the server's counter reads 0,
      // and so does the modal opened afresh (it reads the server's).
      desk.inside("the Reset");
      const modal = await openLimiter(page, desk.bound(UI_MS));
      await modal.locator(".limiter-reset-btn").click({ timeout: desk.bound(UI_MS) });
      await expect(modal.locator(".limiter-activity-label")).toHaveText("not limited yet", {
        timeout: desk.bound(UI_MS),
      });
      await expect
        .poll(() => mixer.activeSeconds(), {
          message: "the server's counter after the Reset",
          timeout: desk.bound(3_000),
        })
        .toBe(0);
      await closeLimiter(page, desk.bound(UI_MS));
      const again = await openLimiter(page, desk.bound(UI_MS));
      await expect(again.locator(".limiter-activity-label")).toHaveText("not limited yet", {
        timeout: desk.bound(UI_MS),
      });
      await closeLimiter(page, desk.bound(UI_MS));
      relay.check();
    });
  });
});
