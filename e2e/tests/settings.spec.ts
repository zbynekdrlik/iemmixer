import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test, expect, Page } from "./support/fixtures";
import { MEMBER_PIN } from "./support/pins";
import { openMixer } from "./support/session";

// The settings modal: the fader preference (F23), the engineer's listen
// boost, console and backups, and logout with its push unsubscribe
// (reaperiem#188). Preferences live in the browser (`iem_settings_<member>`
// in localStorage) and every test has a fresh context; member7's mix is not
// touched.

async function openSettings(page: Page) {
  await page.locator(".settings-btn").click();
  const modal = page.locator(".settings-modal");
  await expect(modal).toBeVisible();
  return modal;
}

/** The browser's stored preferences of `member`. */
async function storedSettings(page: Page, member: string): Promise<Record<string, unknown> | null> {
  return page.evaluate((m) => {
    const raw = localStorage.getItem(`iem_settings_${m}`);
    return raw ? (JSON.parse(raw) as Record<string, unknown>) : null;
  }, member);
}

test.describe("Settings of a band member", () => {
  test("the preferences hold the fader double-tap toggle only, and it persists (F23)", async ({ page }) => {
    await openMixer(page, "member7");
    let modal = await openSettings(page);
    await expect(modal.locator(".settings-name", { hasText: "Fader double-tap" })).toBeVisible();
    // Pan double-tap is always on: no toggle for it.
    await expect(modal.locator(".settings-name", { hasText: "Pan double-tap" })).toHaveCount(0);
    let prefs = modal.locator(".settings-section").filter({ hasText: "Preferences" });
    await expect(prefs.locator(".settings-row")).toHaveCount(1);

    const toggle = () => page.locator(".settings-modal .settings-row .toggle-switch");
    await expect(toggle()).toHaveClass(/\bon\b/);
    await prefs.locator(".settings-row").click();
    await expect(toggle()).not.toHaveClass(/\bon\b/);
    expect(await storedSettings(page, "member7")).toMatchObject({ double_tap_fader: false });

    await page.reload();
    await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
    modal = await openSettings(page);
    await expect(toggle()).not.toHaveClass(/\bon\b/);
    prefs = modal.locator(".settings-section").filter({ hasText: "Preferences" });
    await prefs.locator(".settings-row").click();
    await expect(toggle()).toHaveClass(/\bon\b/);
    expect(await storedSettings(page, "member7")).toMatchObject({ double_tap_fader: true });
  });

  test("a member's settings have no listen boost", async ({ page }) => {
    await openMixer(page, "member7");
    const modal = await openSettings(page);
    await expect(modal.locator(".settings-section-title", { hasText: "Preferences" })).toBeVisible();
    await expect(page.getByTestId("listen-boost-section")).toHaveCount(0);
  });

  test("a member's settings have no backups and no console", async ({ page }) => {
    await openMixer(page, "member7");
    const modal = await openSettings(page);
    await expect(modal.locator(".settings-section-title", { hasText: "Session" })).toBeVisible();
    await expect(page.getByTestId("backup-section")).toHaveCount(0);
    await expect(page.getByTestId("console-section")).toHaveCount(0);
  });

  /** Signs in on the login page's numpad, as a band member does. */
  async function numpadLogin(page: Page, member: string): Promise<void> {
    await page.goto(`/login?member=${member}&next=/${member}`);
    await expect(page.locator(".numpad")).toBeVisible({ timeout: 10_000 });
    const answer = page.waitForResponse(
      (r) => r.url().endsWith("/api/auth") && r.request().method() === "POST",
    );
    for (const digit of MEMBER_PIN) {
      await page.locator(".numpad-btn", { hasText: new RegExp(`^${digit}$`) }).click();
    }
    expect((await answer).status()).toBe(200);
    await page.waitForURL(`**/${member}`);
    await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
  }

  test("the settings have a Logout button", async ({ page }) => {
    await numpadLogin(page, "member7");
    const modal = await openSettings(page);
    const logout = modal.locator(".logout-btn");
    await expect(logout).toBeVisible();
    await expect(logout).toHaveText("Logout");
  });

  test("Logout forgets the login and returns to the landing page", async ({ page }) => {
    await numpadLogin(page, "member7");
    expect(await page.evaluate(() => localStorage.getItem("iem_token"))).not.toBeNull();
    const modal = await openSettings(page);
    await modal.locator(".logout-btn").click();
    await expect(page).toHaveURL(/\/$/);
    await expect(page.locator(".member-card").first()).toBeVisible();
    expect(await page.evaluate(() => localStorage.getItem("iem_token"))).toBeNull();
  });
});

test.describe("Settings of the engineer", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  async function openBoost(page: Page) {
    await openMixer(page, "engineer", { engineer: true });
    await openSettings(page);
    await expect(page.getByTestId("listen-boost-section")).toBeVisible();
    return {
      value: page.getByTestId("boost-value"),
      plus: page.getByTestId("boost-plus"),
      minus: page.getByTestId("boost-minus"),
    };
  }

  test("the engineer's settings have the listen boost, the console and backups", async ({ page }) => {
    const boost = await openBoost(page);
    await expect(boost.minus).toBeVisible();
    await expect(boost.plus).toBeVisible();
    await expect(boost.value).toHaveText("0 dB");
    await expect(page.getByTestId("console-section")).toBeVisible();
    await expect(page.getByTestId("backup-section")).toBeVisible();
  });

  test("the listen boost steps by 3 dB and stops at 0 dB", async ({ page }) => {
    const boost = await openBoost(page);
    await expect(boost.value).toHaveText("0 dB");
    await boost.plus.click();
    await expect(boost.value).toHaveText("+3 dB");
    await boost.plus.click();
    await expect(boost.value).toHaveText("+6 dB");
    await boost.minus.click();
    await expect(boost.value).toHaveText("+3 dB");
    await boost.minus.click();
    await expect(boost.value).toHaveText("0 dB");
    await boost.minus.click();
    await expect(boost.value).toHaveText("0 dB");
    expect(await storedSettings(page, "engineer")).toMatchObject({ listen_boost_db: 0 });
  });

  test("the listen boost is kept in localStorage and survives a reload", async ({ page }) => {
    const boost = await openBoost(page);
    for (let i = 0; i < 4; i++) await boost.plus.click();
    await expect(boost.value).toHaveText("+12 dB");
    expect(await storedSettings(page, "engineer")).toMatchObject({ listen_boost_db: 12 });

    await page.reload();
    await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
    await openSettings(page);
    await expect(page.getByTestId("boost-value")).toHaveText("+12 dB");
  });

  test("the listen boost stops at +24 dB", async ({ page }) => {
    const boost = await openBoost(page);
    for (let i = 0; i < 8; i++) await boost.plus.click();
    await expect(boost.value).toHaveText("+24 dB");
    await boost.plus.click();
    await expect(boost.value).toHaveText("+24 dB");
    expect(await storedSettings(page, "engineer")).toMatchObject({ listen_boost_db: 24 });
  });

  // The CI half of gen1 live/push-unsubscribe.spec.ts: without a Push API
  // there is nothing to revoke, and logout still completes.
  test("Logout without a Push API logs out and revokes nothing (reaperiem#188)", async ({ page }) => {
    const unsubscribes: string[] = [];
    page.on("request", (r) => {
      if (r.method() === "POST" && r.url().endsWith("/api/push/unsubscribe")) unsubscribes.push(r.url());
    });
    const pushLogs: string[] = [];
    page.on("console", (m) => {
      if (m.text().startsWith("[push] unsubscribe")) pushLogs.push(m.text());
    });
    await openMixer(page, "engineer", { engineer: true });
    const modal = await openSettings(page);
    await modal.locator(".logout-btn").click();
    await expect(page).toHaveURL(/\/$/);
    await expect(page.locator(".member-card").first()).toBeVisible();
    expect(await page.evaluate(() => localStorage.getItem("iem_token"))).toBeNull();
    // The unsubscribe ran to its graceful end: no subscription, no POST.
    await expect.poll(() => pushLogs.length, { message: "the unsubscribe path logged its end" }).toBeGreaterThan(0);
    await page.waitForTimeout(1_000);
    expect(unsubscribes).toEqual([]);
  });
});

// The engineer's device with a push subscription: a stand-in PushManager
// answers subscribe and getSubscription (incognito has no Push API, so no
// console allowance is needed); the page, the service worker, the POSTs and
// the server's subscription store are the real ones.
test.describe("Logout of a subscribed engineer device", () => {
  const ENDPOINT = "https://push.example.invalid/e2e-engineer-logout";

  function requiredEnv(name: string): string {
    const value = process.env[name];
    if (!value) throw new Error(`${name} must be set (by the e2e job in .github/workflows/ci.yml)`);
    return value;
  }

  function storedEndpoints(): string[] {
    const file = join(dirname(requiredEnv("IEMMIXER_CONFIG")), "push_subscriptions.json");
    return (JSON.parse(readFileSync(file, "utf8")) as Array<{ endpoint: string }>).map((s) => s.endpoint);
  }

  test("Logout revokes the subscription in the browser and on the server (reaperiem#188)", async ({ page }) => {
    await page.addInitScript((endpoint) => {
      let current: PushSubscription | null = null;
      PushManager.prototype.getSubscription = async () => current;
      PushManager.prototype.subscribe = async () => {
        const json = { endpoint, expirationTime: null, keys: { p256dh: "BE2E-p256dh", auth: "e2e-auth" } };
        const sub = Object.create(PushSubscription.prototype) as PushSubscription;
        Object.defineProperties(sub, {
          endpoint: { value: endpoint },
          toJSON: { value: () => json },
          unsubscribe: {
            value: async () => {
              current = null;
              return true;
            },
          },
        });
        current = sub;
        return sub;
      };
    }, ENDPOINT);
    const pushLogs: string[] = [];
    page.on("console", (m) => {
      if (m.text().startsWith("[push]")) pushLogs.push(m.text());
    });

    const subscribed = page.waitForResponse(
      (r) => r.url().endsWith("/api/push/subscribe") && r.request().method() === "POST",
      { timeout: 20_000 },
    );
    await openMixer(page, "engineer", { engineer: true });
    const subscribe = await subscribed;
    expect(subscribe.status()).toBe(200);
    expect(subscribe.request().postDataJSON()).toMatchObject({ endpoint: ENDPOINT });
    await expect.poll(() => pushLogs).toContain("[push] engineer subscribed to Web Push");
    expect(storedEndpoints()).toContain(ENDPOINT);

    const unsubscribed = page.waitForResponse(
      (r) => r.url().endsWith("/api/push/unsubscribe") && r.request().method() === "POST",
    );
    const modal = await openSettings(page);
    await modal.locator(".logout-btn").click();
    const unsubscribe = await unsubscribed;
    expect(unsubscribe.status()).toBe(200);
    expect(unsubscribe.request().headers()["authorization"]).toMatch(/^Bearer .+/);
    expect(unsubscribe.request().postDataJSON()).toEqual({ endpoint: ENDPOINT });
    await expect.poll(() => pushLogs).toContain("[push] unsubscribed (browser)");
    await expect.poll(() => pushLogs).toContain("[push] unsubscribed (server)");

    await expect(page).toHaveURL(/\/$/);
    expect(await page.evaluate(() => localStorage.getItem("iem_token"))).toBeNull();
    expect(storedEndpoints()).not.toContain(ENDPOINT);
  });
});
