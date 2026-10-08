import { defineConfig, devices } from "@playwright/test";

// The live specs (S7, #10): `tests/live/`, run only from the ops live run on a
// hosted runner against the band's public host, with the run's LIVE_*
// variables (`tests/live/support/env.ts`). Without them `--list` still works
// (the public CI lists the specs). Trace, screenshot and video stay off: a
// trace would hold the run's tokens and the host (P6). The mock run
// (`playwright.config.ts`) ignores `**/live/**`.
export default defineConfig({
  testDir: "./tests/live",
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  workers: 1,
  retries: 0,
  timeout: 240_000,
  globalTimeout: 40 * 60_000,
  // The ops run names its results file (LIVE_RESULTS); the default stays in
  // the ignored test-results/ (the report holds this checkout's own paths).
  reporter: [["list"], ["json", { outputFile: process.env.LIVE_RESULTS ?? "test-results/live-results.json" }]],
  use: {
    baseURL: process.env.LIVE_BASE_URL,
    trace: "off",
    screenshot: "off",
    video: "off",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
