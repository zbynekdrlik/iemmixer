import { test as base, expect } from "../../support/fixtures";
import { BurstWatch } from "./burst";
import { relaySockets, type Relay } from "./relay";

// The live specs' `test`: the zero-console fixture of every spec, plus the
// burst watch and the socket relay as fixtures, so their teardown runs even
// after a timeout (a test body's `finally` does not): the watch's socket
// closes, and the relay's rules are checked after every test.

type LiveFixtures = {
  /** `/ws/audio` through the relay gains `&hil=1` (the listen probe); `test.use({ hil: true })`. */
  hil: boolean;
  /** A `BurstWatch` opened for this test (after the build check). */
  watch: BurstWatch;
  /** Every page socket through the runner (`relaySockets`); checked after the test. */
  relay: Relay;
};

export const test = base.extend<LiveFixtures>({
  hil: [false, { option: true }],
  watch: async ({ request }, use) => {
    const watch = await BurstWatch.open(request);
    try {
      await use(watch);
    } finally {
      watch.close();
    }
  },
  relay: async ({ page, hil }, use) => {
    const relay = await relaySockets(page, { hil });
    await use(relay);
    relay.check();
  },
});

export { expect };
export type { Page } from "@playwright/test";

/** The annotation keys `live_verdict.py` reads (its NUMBER_KEYS). */
export type LiveNumberKey =
  | "listen_hz"
  | "listen_dbfs"
  | "first_audio_ms"
  | "talkback_db"
  | "limiter_active_s"
  | "meter_fps"
  | "burst_input_dbfs"
  | "opus_frames";

/**
 * Hands `key=<value>` to the verdict as a `live_number` annotation, the only
 * way a number leaves a live test; `digits` after the point (a plain ASCII
 * decimal, as the verdict's NUMBER pattern reads it).
 */
export function liveNumber(key: LiveNumberKey, value: number, digits = 0): void {
  if (!Number.isFinite(value)) throw new Error(`${key} is not a finite number`);
  test.info().annotations.push({ type: "live_number", description: `${key}=${value.toFixed(digits)}` });
}
