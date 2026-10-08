import { test as base, expect } from "../../support/fixtures";
import { BurstWatch } from "./burst";
import { Desk } from "./desk";
import { relaySockets, type Relay } from "./relay";

// The live specs' `test`: the zero-console fixture of every spec, plus the
// burst watch, the socket relay and the desk as fixtures, so their teardown
// runs even after a timeout (a test body's `finally` does not): the watch's
// socket closes, the relay's rules are checked after every test, and the
// desk puts back any change a timed-out body left in place.

type LiveFixtures = {
  /** `/ws/audio` through the relay gains `&hil=1` (the listen probe); `test.use({ hil: true })`. */
  hil: boolean;
  /** A `BurstWatch` opened for this test (after the build check). */
  watch: BurstWatch;
  /** Every page socket through the runner (`relaySockets`); checked after the test. */
  relay: Relay;
  /** The runner's mixer sockets and their burst-only changes (`Desk`); put back, checked and closed after the test. */
  desk: Desk;
  /** When the test's fixtures began (`Date.now()` time): automatic, so it comes before the watch and the desk. */
  startedAt: number;
};

export const test = base.extend<LiveFixtures>({
  hil: [false, { option: true }],
  // Playwright reads a fixture's dependencies from its first argument: none here.
  startedAt: [async ({}, use) => use(Date.now()), { auto: true }],
  watch: async ({ request }, use) => {
    const watch = await BurstWatch.open(request);
    try {
      await use(watch);
      // A server close after the test's last look at the watch fails it too.
      watch.check();
    } finally {
      watch.close();
    }
  },
  relay: async ({ page, hil }, use) => {
    const relay = await relaySockets(page, { hil });
    await use(relay);
    relay.check();
  },
  desk: async ({ request, watch, startedAt }, use, testInfo) => {
    // A burst must come early enough for the steps and restores to end before the test's
    // timeout, counted from the test's start (the watch's open and the page's took part of it).
    const desk = new Desk(request, watch, Desk.deadline(testInfo.timeout, startedAt));
    try {
      await use(desk);
    } finally {
      // Before the watch closes: the restore is checked against its burst.
      await desk.end();
    }
  },
});

export { expect };
export type { Page } from "@playwright/test";

/** The annotation keys `live_verdict.py` reads, in its order (its NUMBER_KEYS; tone.spec.ts compares them). */
export const LIVE_NUMBER_KEYS = [
  "listen_hz",
  "listen_dbfs",
  "first_audio_ms",
  "talkback_db",
  "limiter_active_s",
  "meter_fps",
  "burst_input_dbfs",
  "opus_frames",
] as const;
export type LiveNumberKey = (typeof LIVE_NUMBER_KEYS)[number];

/**
 * Hands `key=<value>` to the verdict as a `live_number` annotation, the only
 * way a number leaves a live test; `digits` after the point (a plain ASCII
 * decimal, as the verdict's NUMBER pattern reads it).
 */
export function liveNumber(key: LiveNumberKey, value: number, digits = 0): void {
  if (!Number.isFinite(value)) throw new Error(`${key} is not a finite number`);
  test.info().annotations.push({ type: "live_number", description: `${key}=${value.toFixed(digits)}` });
}
