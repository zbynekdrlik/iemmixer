import { defineConfig, devices } from "@playwright/test";

// The Gen 3 box's browser step (#72 phase C, plan Task 8): `tests/gen3/`, run
// one test at a time by `iempc gen3-mix` on the dev box (and by hand in the
// plan's Live 1 and Live 2) against the box's own server, plain HTTP on its
// LAN (GEN3_UI_URL), with the live run's LIVE_* variables
// (`tests/live/support/env.ts`). Without them `--list` still works (the public
// CI lists the specs). Trace, screenshot and video stay off: a trace would
// hold the tokens and the box's address (P6). The mock run
// (`playwright.config.ts`) ignores `**/gen3/**`.
export default defineConfig({
  testDir: "./tests/gen3",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  workers: 1,
  retries: 0,
  // One step: the page over the LAN, its fader moves and the runner's socket
  // reading them back; `iempc gen3-mix` waits 300 s for the whole npx call.
  timeout: 120_000,
  reporter: [["list"]],
  use: {
    baseURL: process.env.LIVE_BASE_URL,
    trace: "off",
    screenshot: "off",
    video: "off",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
