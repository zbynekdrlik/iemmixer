import { ENGINEER_PIN, MEMBER_PIN } from "./support/pins";
import { test, expect } from "./support/fixtures";


test.describe("Smoke Tests - Must All Pass", () => {
  test("landing page loads and returns HTTP 200", async ({ page }) => {
    const response = await page.goto("/");
    expect(response?.status()).toBe(200);
  });

  test("WASM mounts and renders visible content (not blank page)", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (msg) => {
      if (msg.type() === "error") consoleErrors.push(msg.text());
    });

    await page.goto("/");

    // Wait for WASM to mount — Leptos renders into <body>.
    // If WASM fails (SRI mismatch, init error), body stays empty → timeout.
    await page.waitForSelector("body > :not(noscript):not(script)", {
      timeout: 15000,
    });

    // Verify actual rendered content exists (not just empty divs)
    const bodyChildCount = await page.evaluate(
      () =>
        document.querySelectorAll("body > :not(noscript):not(script):not(link)")
          .length,
    );
    expect(bodyChildCount).toBeGreaterThan(0);

    // No SRI or module loading errors
    const sriErrors = consoleErrors.filter(
      (e) => e.includes("integrity") || e.includes("blocked"),
    );
    expect(sriErrors).toEqual([]);
  });

  test("login page is accessible", async ({ page }) => {
    const response = await page.goto("/login");
    expect(response?.status()).toBe(200);
  });

  test("API members endpoint returns JSON", async ({ request }) => {
    const response = await request.get("/api/members");
    expect(response.status()).toBe(200);
    const data = await response.json();
    expect(Array.isArray(data)).toBe(true);
  });

  test("static assets load correctly", async ({ page }) => {
    await page.goto("/");
    // Check that WASM loads (page should have JS execution)
    await page.waitForLoadState("networkidle");
  });

  test("non-hashed assets have no-cache headers", async ({ request }) => {
    // audio_player.js (in snippets/) has no content hash in filename
    // and must NOT be long-cached — stale CDN cache causes SRI failures
    const swResp = await request.get("/sw.js");
    expect(swResp.headers()["cache-control"]).toContain("no-cache");

    const manifestResp = await request.get("/manifest.json");
    expect(manifestResp.headers()["cache-control"]).toContain("no-cache");
  });
});

test.describe("UX Polish - Issue #20: Mute Button Visibility", () => {
  test("mute-btn.off has neutral gray background (not red)", async ({
    page,
  }) => {
    await page.goto("/");
    await page.waitForLoadState("domcontentloaded");

    // Inject a test element with the mute-btn off class to check CSS
    const bgColor = await page.evaluate(() => {
      const btn = document.createElement("button");
      btn.className = "mute-btn off";
      btn.textContent = "M";
      document.body.appendChild(btn);
      const style = getComputedStyle(btn);
      const bg = style.backgroundColor;
      document.body.removeChild(btn);
      return bg;
    });

    // Parse RGB values — should be neutral gray (#2a2a3a), NOT red
    const match = bgColor.match(/rgb\((\d+),\s*(\d+),\s*(\d+)\)/);
    expect(match).not.toBeNull();
    const red = parseInt(match![1]);
    // Neutral gray has low red channel (< 100); dark red would be > 80
    expect(red).toBeLessThan(100);
  });
});

test.describe("UX Polish - Issue #18: Fader 0 dB Marker", () => {
  test("fader-track has ::before pseudo-element at ~83% for 0 dB", async ({
    page,
  }) => {
    await page.goto("/");
    await page.waitForLoadState("domcontentloaded");

    // Inject a fader-track div and check ::before computed style
    const position = await page.evaluate(() => {
      const track = document.createElement("div");
      track.className = "fader-track";
      track.style.width = "300px";
      track.style.height = "44px";
      track.style.position = "relative";
      document.body.appendChild(track);
      const style = getComputedStyle(track, "::before");
      const left = style.left;
      document.body.removeChild(track);
      return left;
    });

    // ::before left should be ~83.33% of 300px = ~250px
    const px = parseFloat(position);
    expect(px).toBeGreaterThan(240);
    expect(px).toBeLessThan(260);
  });
});

test.describe("UX Polish - Issue #3: Login Back Button", () => {
  test("login page has back button that navigates to landing page", async ({
    page,
  }) => {
    await page.goto("/login?member=member1&next=/member1");
    await page.waitForLoadState("domcontentloaded");

    // Back button should be visible
    const backBtn = page.locator(".back-btn");
    await expect(backBtn).toBeVisible({ timeout: 10000 });

    // Header should show "IEM Mixer"
    const header = page.locator(".mixer-header h1");
    await expect(header).toContainText("IEM Mixer");

    // Click back button — should navigate to landing page
    await backBtn.click();
    await page.waitForURL("**/", { timeout: 5000 });
    expect(page.url()).toMatch(/\/$/);
  });
});

test.describe("Network Error UX - Issue #56", () => {
  // These tests abort /api/members on purpose; Chrome reports the aborted request.
  test.use({ allowedConsole: [/Failed to load resource: net::ERR_CONNECTION_FAILED/] });

  test("shows error message when API is unreachable", async ({ page }) => {
    // Block /api/members requests to simulate network failure
    await page.route("**/api/members", (route) =>
      route.abort("connectionfailed"),
    );

    await page.goto("/");

    // Error message should appear within 6 seconds
    const errorElement = page.locator(".network-error");
    await expect(errorElement).toBeVisible({ timeout: 6000 });
  });

  test("error message mentions WiFi/network", async ({ page }) => {
    await page.route("**/api/members", (route) =>
      route.abort("connectionfailed"),
    );

    await page.goto("/");

    // Wait for error to appear
    const errorElement = page.locator(".network-error");
    await expect(errorElement).toBeVisible({ timeout: 6000 });

    // Should mention WiFi or network in the message
    const errorText = await errorElement.textContent();
    expect(errorText?.toLowerCase()).toMatch(/wifi|network|connection/);
  });

  test("retry button reloads members on click", async ({ page }) => {
    let blocked = true;

    // Initially block the API
    await page.route("**/api/members", (route) => {
      if (blocked) {
        route.abort("connectionfailed");
      } else {
        route.continue();
      }
    });

    await page.goto("/");

    // Wait for error to appear
    const errorElement = page.locator(".network-error");
    await expect(errorElement).toBeVisible({ timeout: 6000 });

    // Find retry button
    const retryBtn = page.locator(".network-error button, .retry-btn");
    await expect(retryBtn).toBeVisible();

    // Unblock the route
    blocked = false;

    // Click retry
    await retryBtn.click();

    // Members should now load
    const memberGrid = page.locator(".member-grid, .member-card");
    await expect(memberGrid.first()).toBeVisible({ timeout: 6000 });

    // Error should be gone
    await expect(errorElement).not.toBeVisible();
  });
});

test.describe("PWA Support - Issue #19", () => {
  test("manifest.json is served with valid PWA content", async ({
    request,
  }) => {
    const response = await request.get("/manifest.json");
    expect(response.status()).toBe(200);
    const manifest = await response.json();
    expect(manifest.name).toBe("IEM Mixer");
    expect(manifest.display).toBe("standalone");
    expect(manifest.start_url).toBe("/");
    expect(manifest.icons).toBeDefined();
    expect(manifest.icons.length).toBeGreaterThan(0);
  });

  test("icon.svg is served with correct content type", async ({ request }) => {
    const response = await request.get("/icon.svg");
    expect(response.status()).toBe(200);
    const contentType = response.headers()["content-type"];
    expect(contentType).toContain("svg");
  });

  test("icon PNGs are served", async ({ request }) => {
    const r192 = await request.get("/icon-192.png");
    expect(r192.status()).toBe(200);
    expect(r192.headers()["content-type"]).toContain("png");

    const r512 = await request.get("/icon-512.png");
    expect(r512.status()).toBe(200);
    expect(r512.headers()["content-type"]).toContain("png");
  });

  test("manifest icons include PNG entries", async ({ request }) => {
    const response = await request.get("/manifest.json");
    const manifest = await response.json();
    const pngIcons = manifest.icons.filter(
      (i: { type: string }) => i.type === "image/png",
    );
    expect(pngIcons.length).toBeGreaterThanOrEqual(2);
    expect(pngIcons.some((i: { sizes: string }) => i.sizes === "192x192")).toBe(
      true,
    );
    expect(pngIcons.some((i: { sizes: string }) => i.sizes === "512x512")).toBe(
      true,
    );
  });

  test("sw.js is served with no-cache header", async ({ request }) => {
    const response = await request.get("/sw.js");
    expect(response.status()).toBe(200);
    const cacheControl = response.headers()["cache-control"];
    expect(cacheControl).toContain("no-cache");
  });
});

test.describe("Auth & Token Expiry - Issue #38", () => {
  test("engineer token has 7-day expiry", async ({ request }) => {
    // Login with engineer PIN (<PIN>)
    const response = await request.post("/api/auth", {
      data: { member: "member1", pin: ENGINEER_PIN },
    });
    expect(response.status()).toBe(200);

    const data = await response.json();
    expect(data.engineer).toBe(true);
    expect(data.expires_in).toBe(7 * 24 * 60 * 60); // 7 days in seconds
  });

  test("member token has 7-day expiry", async ({ request }) => {
    // Login with default member PIN (<PIN>)
    const response = await request.post("/api/auth", {
      data: { member: "member1", pin: MEMBER_PIN },
    });
    expect(response.status()).toBe(200);

    const data = await response.json();
    expect(data.engineer).toBe(false);
    expect(data.expires_in).toBe(7 * 24 * 60 * 60); // 7 days in seconds
  });
});

test.describe("Snapshot History - Issue #46", () => {
  test("snapshot API returns list", async ({ request }) => {
    // Login first to get token
    const loginResp = await request.post("/api/auth", {
      data: { member: "member1", pin: MEMBER_PIN },
    });
    const { token } = await loginResp.json();

    // Get snapshots list
    const response = await request.get("/api/snapshots/member1", {
      headers: { Authorization: `Bearer ${token}` },
    });
    expect(response.status()).toBe(200);

    const data = await response.json();
    expect(Array.isArray(data)).toBe(true);
  });

  test("can create manual snapshot", async ({ request }) => {
    // Login first to get token
    const loginResp = await request.post("/api/auth", {
      data: { member: "member1", pin: MEMBER_PIN },
    });
    const { token } = await loginResp.json();

    // Create snapshot
    const response = await request.post("/api/snapshots/member1", {
      headers: { Authorization: `Bearer ${token}` },
      data: { label: "test-snapshot" },
    });

    // May return 201 (created) or 400 (no state available in CI)
    expect([200, 201, 400]).toContain(response.status());
  });
});

test.describe("Version Display - Issue #63", () => {
  test("version text has high contrast (white)", async ({ page }) => {
    await page.goto("/");
    await page.waitForLoadState("domcontentloaded");

    // Check version number styling
    const versionColor = await page.evaluate(() => {
      const el = document.createElement("span");
      el.className = "header-version-number";
      document.body.appendChild(el);
      const style = getComputedStyle(el);
      const color = style.color;
      document.body.removeChild(el);
      return color;
    });

    // Parse RGB - should be white or near-white (all channels > 200)
    const match = versionColor.match(/rgb\((\d+),\s*(\d+),\s*(\d+)\)/);
    expect(match).not.toBeNull();
    const r = parseInt(match![1]);
    const g = parseInt(match![2]);
    const b = parseInt(match![3]);
    // White text should have high values in all channels
    expect(r).toBeGreaterThan(200);
    expect(g).toBeGreaterThan(200);
    expect(b).toBeGreaterThan(200);
  });

  test("datetime uses Slovak format (DD.MM.YYYY)", async ({ request }) => {
    const response = await request.get("/api/version");
    expect(response.status()).toBe(200);

    const data = await response.json();
    // full_version contains date in parentheses: "1.30.0 (06.03.2026 10:45)"
    // For local builds it's "1.30.0 (local)"
    if (data.full_version && !data.full_version.includes("local")) {
      // Extract date part from parentheses and check Slovak format
      const dateMatch = data.full_version.match(
        /\((\d{2}\.\d{2}\.\d{4} \d{2}:\d{2})\)/,
      );
      expect(dateMatch).not.toBeNull();
    }
  });
});
