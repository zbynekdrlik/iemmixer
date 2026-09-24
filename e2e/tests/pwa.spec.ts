/**
 * PWA Service Worker Tests — verify SW registers and caches hashed assets.
 *
 * The service worker caches content-hashed WASM/JS files (cache-first strategy)
 * for instant repeat loads. Only files matching /[a-f0-9]{16,}\.(js|wasm)$/ are
 * cached. index.html and unhashed files are NEVER cached in SW.
 *
 * Previous cache-ALL strategy caused blank pages after every deploy because
 * old WASM/JS assets were served from SW cache but didn't match new HTML references.
 * Current approach only caches immutable hashed files — safe across deploys.
 */

import { test, expect } from "@playwright/test";

const BASE_URL = process.env.E2E_BASE_URL || "http://localhost:8080";

test.describe("Service Worker — PWA with hashed asset caching", () => {
  test("service worker registers and activates", async ({ page }) => {
    // Navigate to the app to trigger SW registration
    await page.goto(BASE_URL, { waitUntil: "networkidle" });

    // Wait for SW to register and activate
    const swRegistered = await page.evaluate(async () => {
      if (!("serviceWorker" in navigator)) return "unsupported";

      try {
        const reg = await navigator.serviceWorker.getRegistration();
        if (!reg) return "not-registered";

        // Wait for the SW to become active
        const sw = reg.active || reg.waiting || reg.installing;
        if (!sw) return "no-worker";

        if (sw.state !== "activated") {
          await new Promise<void>((resolve) => {
            sw.addEventListener("statechange", () => {
              if (sw.state === "activated") resolve();
            });
            // Resolve immediately if already activated
            if (sw.state === "activated") resolve();
          });
        }

        return "active";
      } catch (e) {
        return `error: ${e}`;
      }
    });

    // Service Workers MUST be supported in the target browser (Chromium).
    // No silent skip — if the browser somehow lacks SW support, fail loudly.
    expect(swRegistered).not.toBe("unsupported");
    expect(swRegistered).toBe("active");
  });

  test("hashed WASM/JS assets are cached after navigation", async ({
    page,
  }) => {
    // Loop budget below is 40s (40 × 1s) to tolerate GitHub Actions queue
    // variance (see commit 28c0b42). Default test timeout is 30s, which
    // cuts the loop off mid-poll and produces flakes when SW cache takes
    // 25-40s to populate. Extend the test timeout to cover the loop plus
    // navigation + SW activation buffer.
    test.setTimeout(60_000);

    // Navigate to app — this triggers SW registration
    await page.goto(BASE_URL, { waitUntil: "networkidle" });

    // Wait for SW to reach `activated` state BEFORE reloading. Without
    // this, the reload can race ahead of SW activation on slow runners
    // and fetches bypass the SW entirely, leaving the cache unpopulated.
    // Uses the same pattern as the "service worker registers and activates"
    // test above.
    const swActivated = await page.evaluate(async () => {
      if (!("serviceWorker" in navigator)) return "unsupported";
      const reg = await navigator.serviceWorker.getRegistration();
      if (!reg) return "not-registered";
      const sw = reg.active || reg.waiting || reg.installing;
      if (!sw) return "no-worker";
      if (sw.state !== "activated") {
        await new Promise<void>((resolve) => {
          sw.addEventListener("statechange", () => {
            if (sw.state === "activated") resolve();
          });
          if (sw.state === "activated") resolve();
        });
      }
      return "active";
    });
    // Service Workers MUST be supported in the target browser (Chromium).
    expect(swActivated).not.toBe("unsupported");
    expect(swActivated).toBe("active");

    // Reload with the SW guaranteed to be active — it will now intercept
    // fetches for hashed assets and populate the cache.
    await page.reload({ waitUntil: "networkidle" });

    // After reload, page.reload may serve hashed assets from HTTP cache
    // (Cache-Control: immutable) WITHOUT triggering the SW fetch handler
    // — observed empirically as a recurring flake on ubuntu-latest. Force
    // SW interception by refetching the hashed assets explicitly with
    // `cache: "no-store"`, which bypasses HTTP cache and routes through
    // the SW fetch handler.
    await page.evaluate(async () => {
      // Wait until the SW is actually controlling this page (clients.claim
      // racing with the navigation can leave us briefly uncontrolled).
      if (!navigator.serviceWorker.controller) {
        await new Promise<void>((resolve) => {
          navigator.serviceWorker.addEventListener(
            "controllerchange",
            () => resolve(),
            { once: true },
          );
          if (navigator.serviceWorker.controller) resolve();
        });
      }
      // Find hashed asset URLs from the document and refetch with a
      // no-store hint. The SW fetch handler matches on URL pathname and
      // populates `iem-assets-v1`.
      const urls = new Set<string>();
      document.querySelectorAll("script[src]").forEach((el) => {
        const src = (el as HTMLScriptElement).src;
        if (/[a-f0-9]{16,}\.(js|wasm)$/.test(src)) urls.add(src);
      });
      document.querySelectorAll("link[href]").forEach((el) => {
        const href = (el as HTMLLinkElement).href;
        if (/[a-f0-9]{16,}\.(js|wasm)$/.test(href)) urls.add(href);
      });
      // Best-effort refetch — failures don't stop the test (the SW
      // intercept itself is what matters; cache.put inside the SW does
      // the work).
      await Promise.all(
        [...urls].map((url) =>
          fetch(url, { cache: "no-store" }).catch(() => {}),
        ),
      );
    });

    // Poll for SW cache to be populated. After the explicit refetch above
    // the SW should have written entries on the same tick; the loop is
    // belt-and-suspenders for slow runners.
    let cacheInfo = { exists: false, keys: [] as string[] };
    for (let attempt = 0; attempt < 40; attempt++) {
      await page.waitForTimeout(1000);
      cacheInfo = await page.evaluate(async () => {
        const names = await caches.keys();
        if (!names.includes("iem-assets-v1"))
          return { exists: false, keys: [] as string[] };
        const cache = await caches.open("iem-assets-v1");
        const requests = await cache.keys();
        return {
          exists: true,
          keys: requests.map((r) => new URL(r.url).pathname),
        };
      });
      if (cacheInfo.exists && cacheInfo.keys.length > 0) break;
    }

    expect(cacheInfo.exists).toBe(true);
    // Should have cached at least the WASM and JS loader files
    const hashedFiles = cacheInfo.keys.filter((k: string) =>
      /[a-f0-9]{16,}\.(js|wasm)$/.test(k),
    );
    expect(hashedFiles.length).toBeGreaterThanOrEqual(1);

    // Verify unhashed files are NOT in cache
    const unhashedFiles = cacheInfo.keys.filter(
      (k: string) => !/[a-f0-9]{16,}\.(js|wasm)$/.test(k),
    );
    expect(unhashedFiles).toHaveLength(0);
  });
});
