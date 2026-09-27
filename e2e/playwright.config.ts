import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  testIgnore: ["**/live/**"],
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: 0, // No retries - tests must pass first time
  workers: 1,
  reporter: [["html"], ["list"]],
  use: {
    baseURL: process.env.E2E_BASE_URL || "http://localhost:80",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      testIgnore: ["**/live/**", "**/mobile.spec.ts"],
      use: { ...devices["Desktop Chrome"] },
    },
    {
      // The band's phones: 375 × 667 with touch (isMobile + hasTouch, so
      // `pointer: coarse` matches), in Chromium, the only browser the e2e job
      // installs. Runs mobile.spec.ts only.
      name: "phone",
      testMatch: "**/mobile.spec.ts",
      use: { ...devices["iPhone SE (3rd gen)"], browserName: "chromium" },
    },
  ],
});
