import { test, expect, liveNumber } from "./support/live";
import { pause } from "./support/desk";
import { live } from "./support/env";
import { dbOf, median } from "./support/series";

// Meters on the real card (S7, #10; a #25 repeat): the engineer's page gets a
// Meters frame about every 100 ms, and inside a burst the burst's input shows
// the burst's level. The input goes dry and open (processing and mute off)
// only inside the burst, while every mix's TX is zero, and goes back before
// the burst ends (`desk.during`). The runner's own socket on the engineer's
// page reads the frames; no page is opened. Numbers leave the test only as
// `live_number` annotations (`live_verdict.py`).

/** The window the frames are counted and read in. */
const WINDOW_MS = 3_000;
/** At about 10 frames a second, the window holds at least this many. */
const MIN_FRAMES = 27;
/** The input's processing fades out, then one meter frame passes. */
const SETTLE_MS = 500;
/** The median meter peak is the burst's level within this. */
const LEVEL_TOLERANCE_DB = 0.5;

test.describe("meters on the real PC (S7)", () => {
  test("#25 repeat: meters arrive about 10 times a second and the burst input reads the burst level on the real card", async ({
    desk,
  }) => {
    const { testInput, burstDbfs } = live();
    const engineer = await desk.open("engineer", "engineer");

    const read = await desk.during(async () => {
      const before = await engineer.consoleInput(testInput);
      if (!before) throw new Error("LIVE_TEST_INPUT is not an input of the console");
      // Dry and open: the burst's sine reaches the input meter as the PC
      // plays it, with no trim, EQ or mute of the site's in the way.
      desk.change(
        engineer,
        { cmd: "SetInput", input: testInput, processing: false, muted: false },
        { cmd: "SetInput", input: testInput, processing: before.processing, muted: before.muted },
      );
      await engineer.applied();
      await pause(SETTLE_MS);
      desk.inside("the meter window");
      const from = Date.now();
      await pause(WINDOW_MS);
      const to = Date.now();
      desk.inside("the meter window's end");
      engineer.check();
      return { frames: engineer.meterFrames(from, to), peaks: engineer.peaks(testInput, from, to), ms: to - from };
    });

    liveNumber("meter_fps", (read.frames * 1000) / read.ms, 1);
    expect(read.frames, "meter frames in 3 s").toBeGreaterThanOrEqual(MIN_FRAMES);
    expect(read.peaks.length, "frames carrying the burst's input").toBe(read.frames);
    const level = median(read.peaks.map(dbOf));
    liveNumber("burst_input_dbfs", level, 3);
    expect(Math.abs(level - burstDbfs), `the burst's input at ${level.toFixed(3)} dBFS`).toBeLessThanOrEqual(
      LEVEL_TOLERANCE_DB,
    );
  });
});
