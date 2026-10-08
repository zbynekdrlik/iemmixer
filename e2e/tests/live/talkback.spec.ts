import { tmpdir } from "node:os";
import { join } from "node:path";
import { test, expect, liveNumber, type Page } from "./support/live";
import { TALK_INIT, TALK_TONE } from "./support/audio";
import { RESTORE_MS, pause, type Desk, type LiveMixer } from "./support/desk";
import { live, openLive } from "./support/env";
import { SILENT_PEAK, continuity, talkbackLevel } from "./support/series";
import { toneWav } from "../support/wav";

// The engineer's talkback on the real PC (S7, #10; rows 731 and the A4
// level): Chromium's fake microphone plays a 1 kHz tone at half scale, the
// page encodes it onto /ws/talkback through the runner's relay, and the
// server plays it into the engine's talkback input (LIVE_TALKBACK_INPUT).
// The runner's own socket on the engineer's page reads that input's meter.
// Everything happens inside a burst, while every mix's TX is zero, so the
// tone reaches no in-ear; the card output is not reached by design. The
// input's card goes away (its trim off) only inside the burst and comes back
// before it ends (`desk.during`), and Talk is held at most 8 s. Numbers leave
// a test only as `live_number` annotations (`live_verdict.py`).

const MIC_WAV = toneWav(join(tmpdir(), "iemmixer-live-talkback-1k-half.wav"), TALK_TONE.hz, TALK_TONE.amplitude);

// Launch options are the worker's: top level, never in a describe.
test.use({
  launchOptions: {
    args: [
      "--use-fake-ui-for-media-stream",
      "--use-fake-device-for-media-stream",
      `--use-file-for-fake-audio-capture=${MIC_WAV}`,
    ],
  },
  permissions: ["microphone"],
});

/** Talk is held at most this long (the shorter it is held, the less a switch can meet an open talkback). */
const HOLD_MS = 8_000;
/** The release comes this long before `HOLD_MS` at the latest, should a step overrun. */
const HOLD_MARGIN_MS = 200;
/** After the release, the button shows Talk again within this long. */
const BUTTON_MS = 1_000;
/** Talk goes live (the server granted the lock) within this long of the press, … */
const LIVE_MS = 1_500;
/**
 * … and the page hands its encoder the first frame within this long of the
 * press: then the settling and the window end before the hold's cap.
 */
const START_MS = 1_800;
/** Chromium's capture processing settles: the window starts this long after the encoder's first frame. */
const SETTLE_MS = TALK_TONE.settleMs;
/** The window both series are read in. */
const WINDOW_MS = 3_000;
/** The engine adds the talkback at 0.379934 (program spec A4): −8.406 dB. */
const TALKBACK_DB = 20 * Math.log10(0.379934);
const TALKBACK_TOLERANCE_DB = 0.3;
/** The encoder's frame peaks stray at most this far in the window. */
const STEADY_DB = 0.2;
/** Row 731: of this many meter frames while Talk is held … */
const HELD_FRAMES = 50;
/** … at least this many read above −60 dB, … */
const HEARD_FRAMES = 40;
/** … and never this many in a row below it (a hang of half a second). */
const HANG_FRAMES = 5;
/** The meter reads silence this long after the release. */
const RELEASE_MS = 600;

type Hold<T> = { result: T; pressedAt: number; releasedAt: number };

/** The Talk button, idle and usable, after a first tap (vibration and audio need a user gesture). */
async function talkReady(page: Page): Promise<void> {
  const talk = page.locator(".toolbar-btn-talk");
  await expect(talk).toHaveText("🎤 Talk");
  await expect(talk).not.toHaveClass(/unsupported|mic-blocked|in-use/);
  await page.locator(".category-tab").first().click({ timeout: 10_000 });
}

/**
 * Takes the card away from the talkback input, as the mock spec does: its
 * trim off, processing on (so the trim applies; the talkback comes after it)
 * and unmuted (the mute comes after the talkback). The desk puts the
 * console's values back. Resolves once the input's meter reads silence.
 */
async function cardAway(desk: Desk, engineer: LiveMixer, input: string): Promise<void> {
  const before = await engineer.consoleInput(input);
  if (!before) throw new Error("LIVE_TALKBACK_INPUT is not an input of the console");
  desk.change(
    engineer,
    { cmd: "SetInput", input, trim_db: -150, processing: true, muted: false },
    { cmd: "SetInput", input, trim_db: before.trim_db, processing: before.processing, muted: before.muted },
  );
  await engineer.applied();
  await expect
    .poll(
      () => {
        const recent = engineer.peaks(input, Date.now() - 300);
        return recent.length > 0 && recent.every((p) => p < SILENT_PEAK);
      },
      { message: "the talkback input is silent without its card", timeout: desk.bound(5_000) },
    )
    .toBe(true);
}

/** How many frames the page has handed its talkback encoder (`TALK_INIT`). */
function encodedFrames(page: Page): Promise<number> {
  return page.evaluate(() => (window as unknown as { __live_talk_in?: unknown[] }).__live_talk_in?.length ?? 0);
}

/**
 * Holds Talk while `body` runs: a real press (the button needs pointerdown
 * with a pointer id), only with the whole hold and the restore's time left
 * in the burst, live within `LIVE_MS` and the encoder's first frame within
 * `START_MS` (`body` gets when it came). Released when `body` ends or fails,
 * by a timer before `HOLD_MS` should a step overrun (`capped`), and by the
 * desk should the burst end first or the test time out (the release is the
 * press's undo). `afterMs`: what the caller still does in the burst after
 * the release, counted in the time the press needs. The page needs
 * `TALK_INIT`.
 */
async function holdTalk<T>(
  page: Page,
  desk: Desk,
  afterMs: number,
  body: (t: { pressedAt: number; startedAt: number }) => Promise<T>,
): Promise<Hold<T>> {
  const talk = page.locator(".toolbar-btn-talk");
  await talk.hover({ timeout: desk.bound(5_000) });
  desk.need(HOLD_MS + BUTTON_MS + afterMs + RESTORE_MS, "Talk");
  // One release for every caller (the step, the timer, the desk): each waits for the same.
  let held = false;
  let releasing: Promise<void> = Promise.resolve();
  const up = (): Promise<void> => {
    if (held) {
      held = false;
      releasing = page.mouse.up();
    }
    return releasing;
  };
  desk.track("Talk", up);
  const pressedAt = Date.now();
  // Before the press: a release from the desk while it is on its way comes after it.
  held = true;
  await page.mouse.down();
  let capped = false;
  const cap = setTimeout(
    () => {
      capped = true;
      up().catch(() => undefined);
    },
    pressedAt + HOLD_MS - HOLD_MARGIN_MS - Date.now(),
  );
  let result: T;
  try {
    await expect(talk, "Talk goes live").toHaveClass(/\blive\b/, { timeout: LIVE_MS });
    // The talkback socket opens through the relay, then the microphone and the encoder start.
    while ((await encodedFrames(page)) === 0) {
      if (Date.now() - pressedAt > START_MS) {
        throw new Error(`the talkback did not start within ${START_MS / 1000} s of the press`);
      }
      await pause(25);
    }
    result = await body({ pressedAt, startedAt: Date.now() });
  } finally {
    clearTimeout(cap);
    await up();
  }
  const releasedAt = Date.now();
  await expect(talk).toHaveText("🎤 Talk", { timeout: desk.bound(BUTTON_MS) });
  expect(releasedAt - pressedAt, "Talk held at most 8 s").toBeLessThanOrEqual(HOLD_MS);
  // The cap's release came inside the steps: what they read may hold the release.
  expect(capped, "the steps ended before the hold's cap released Talk").toBe(false);
  return { result, pressedAt, releasedAt };
}

/** The page's clock (ms), which `TALK_INIT` stamps its frames with. */
function pageNow(page: Page): Promise<number> {
  return page.evaluate(() => performance.now());
}

/** The peaks (linear) of the frames the page handed its encoder in `[from, to)` of the page's clock. */
function encoded(page: Page, from: number, to: number): Promise<number[]> {
  return page.evaluate(
    ({ from, to }) =>
      ((window as unknown as { __live_talk_in?: { peak: number; at: number }[] }).__live_talk_in ?? [])
        .filter((f) => f.at >= from && f.at < to)
        .map((f) => f.peak),
    { from, to },
  );
}

/** A frame `TALK_INIT` could not read, or null. */
function encodeError(page: Page): Promise<string | null> {
  return page.evaluate(() => (window as unknown as { __live_talk_error?: string }).__live_talk_error ?? null);
}

test.describe("talkback on the real PC (S7)", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  test("talkback reaches the ENG_MIC meter at -8.4 dB of its input within 0.3 dB during a burst", async ({
    page,
    relay,
    desk,
  }) => {
    const { talkbackInput } = live();
    await page.addInitScript(TALK_INIT);
    await openLive(page, "engineer");
    await talkReady(page);
    const engineer = await desk.open("engineer", "engineer");

    const series = await desk.during(async () => {
      await cardAway(desk, engineer, talkbackInput);
      const hold = await holdTalk(page, desk, 0, async ({ startedAt }) => {
        // The capture processing settles; the window ends before the hold does.
        await pause(startedAt + SETTLE_MS - Date.now());
        desk.inside("the talkback window");
        relay.check();
        const from = { runner: Date.now(), page: await pageNow(page) };
        await pause(WINDOW_MS);
        const to = { runner: Date.now(), page: await pageNow(page) };
        relay.check();
        return {
          meter: engineer.peaks(talkbackInput, from.runner, to.runner),
          input: await encoded(page, from.page, to.page),
        };
      });
      return hold.result;
    });

    expect(await encodeError(page), "every encoder frame was read").toBeNull();
    expect(series.input.length, "encoder frames in the 3 s window").toBeGreaterThanOrEqual(120);
    expect(series.meter.length, "meter frames in the 3 s window").toBeGreaterThanOrEqual(25);
    const level = talkbackLevel(series.meter, series.input);
    liveNumber("talkback_db", level.db, 3);
    expect(level.inputDb, "the encoder got the tone").toBeGreaterThan(-60);
    expect(level.inputSpreadDb, "talkback input not steady").toBeLessThanOrEqual(STEADY_DB);
    const off = Math.abs(level.db - TALKBACK_DB);
    expect(off, `the talkback at ${level.db.toFixed(3)} dB of its input`).toBeLessThanOrEqual(TALKBACK_TOLERANCE_DB);
  });

  test("held Talk reaches the ENG_MIC input continuously on the real PC", async ({ page, relay, desk }) => {
    const { talkbackInput } = live();
    await page.addInitScript(TALK_INIT);
    await openLive(page, "engineer");
    await talkReady(page);
    const engineer = await desk.open("engineer", "engineer");

    const meter = await desk.during(async () => {
      await cardAway(desk, engineer, talkbackInput);
      // After the release: the frames from RELEASE_MS to 300 ms later.
      const hold = await holdTalk(page, desk, RELEASE_MS + 300, async ({ pressedAt, startedAt }) => {
        // Half a second for the first frames to reach the engine's meter,
        // then 50 frames (5 s) before the release's deadline.
        const from = startedAt + 500;
        await pause(from - Date.now());
        desk.inside("the held frames");
        const deadline = pressedAt + HOLD_MS - HOLD_MARGIN_MS - 100;
        await expect
          .poll(
            () => {
              relay.check();
              return engineer.peaks(talkbackInput, from).length;
            },
            {
              message: "50 meter frames while Talk is held",
              timeout: Math.max(1, deadline - Date.now()),
              intervals: [50],
            },
          )
          .toBeGreaterThanOrEqual(HELD_FRAMES);
        return engineer.peaks(talkbackInput, from).slice(0, HELD_FRAMES);
      });
      // The frames that left the server from RELEASE_MS after the release on.
      await pause(hold.releasedAt + RELEASE_MS + 300 - Date.now());
      desk.inside("the release's frames");
      return {
        held: hold.result,
        released: engineer.peaks(talkbackInput, hold.releasedAt + RELEASE_MS, hold.releasedAt + RELEASE_MS + 300),
      };
    });

    const held = continuity(meter.held);
    expect(held.heard, `frames of ${HELD_FRAMES} above -60 dB while held`).toBeGreaterThanOrEqual(HEARD_FRAMES);
    expect(held.longestSilence, "the longest run of silent frames while held").toBeLessThan(HANG_FRAMES);
    expect(meter.released.length, "meter frames 600 to 900 ms after the release").toBeGreaterThanOrEqual(2);
    expect(
      meter.released.every((p) => p < SILENT_PEAK),
      "the talkback input is below -60 dB within 600 ms of the release",
    ).toBe(true);
  });
});
