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
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
