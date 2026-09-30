import { test, expect } from "./support/fixtures";

// The app's own download can fail: the WiFi drops while a phone loads the
// page, or the page is left before the WASM module arrived (tunnel.spec.ts
// navigated on after 14 ms and got "[pageerror] Failed to fetch", run
// 36349014194). The loading shell then says so and offers a reload; the
// console gets no uncaught error.

/** The WASM module Trunk emits (`iem-ui-<hash>_bg.wasm`, sw.js `HASH_RE`). */
const APP_WASM = /\/iem-ui-[0-9a-f]{1,16}_bg\.wasm$/;

test.describe("A failed app download", () => {
  // The WASM download is aborted on purpose and Chrome reports the failed
  // request. The service worker is kept out so the route sees the page's own
  // request, never one the worker makes; Playwright warns that it blocked
  // the registration.
  test.use({
    serviceWorkers: "block",
    allowedConsole: [
      [/^Failed to load resource: net::ERR_CONNECTION_FAILED$/, /^Service Worker registration blocked by Playwright$/],
      { scope: "test" },
    ],
  });

  test("the shell shows the WiFi hint and a retry that loads the app, with no uncaught error", async ({ page }) => {
    let blocked = true;
    await page.route(APP_WASM, (route) => (blocked ? route.abort("connectionfailed") : route.continue()));
    await page.goto("/");

    const failed = page.getByTestId("load-error");
    await expect(failed).toBeVisible({ timeout: 10_000 });
    await expect(failed).toContainText("band WiFi network");
    await expect(page.locator(".shell-spinner")).toBeHidden();

    blocked = false;
    await failed.getByRole("button", { name: "Try Again" }).click();
    await expect(page.locator(".member-card").first()).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId("load-error")).toHaveCount(0);
  });
});
