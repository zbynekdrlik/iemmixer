import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test, expect } from "./support/fixtures";

// The owner's one-time alarm link (S6 bootstrap). The e2e job creates it with
// `iem-server alarm-link` (E2E_ALARM_LINK) next to the site file
// (IEMMIXER_CONFIG). Playwright's contexts are incognito, where Chrome has no
// Push API, so a stand-in PushManager.subscribe answers with a fixed
// subscription; the page, the permission, the service worker, the POST and
// the server's recipient store are the real ones.

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} must be set (by the e2e job in .github/workflows/ci.yml)`);
  }
  return value;
}

const SUBSCRIPTION = {
  endpoint: "https://push.example.invalid/e2e-alarm-link",
  expirationTime: null,
  keys: { p256dh: "BE2E-p256dh", auth: "e2e-auth" },
};

test.describe("The owner's alarm link", () => {
  test("one tap makes this phone an alarm recipient, and the link works once", async ({ page, context }) => {
    const token = new URL(requiredEnv("E2E_ALARM_LINK")).searchParams.get("t");
    expect(token).toMatch(/^[A-Za-z0-9_-]{22}$/);
    const recipientsFile = join(dirname(requiredEnv("IEMMIXER_CONFIG")), "alarm_subscriptions.json");

    await context.grantPermissions(["notifications"]);
    await page.addInitScript((subscription) => {
      PushManager.prototype.subscribe = async () => subscription as unknown as PushSubscription;
    }, SUBSCRIPTION);

    await page.goto(`/alarms?t=${token}`);
    const button = page.getByTestId("alarm-enable");
    await expect(button).toHaveText("Povoliť upozornenia");
    const posted = page.waitForResponse(
      (r) => r.url().endsWith("/api/alarms/subscribe") && r.request().method() === "POST",
    );
    await button.click();
    const response = await posted;
    expect(response.status()).toBe(200);
    expect(response.request().postDataJSON()).toEqual({ token, subscription: SUBSCRIPTION });
    await expect(page.getByTestId("alarm-result")).toHaveText("Hotovo: tento telefón dostane upozornenia.");
    await expect(button).toBeDisabled();

    // The server keeps the phone as an alarm recipient.
    const recipients = JSON.parse(readFileSync(recipientsFile, "utf8")) as Array<{ endpoint: string }>;
    expect(recipients.map((r) => r.endpoint)).toContain(SUBSCRIPTION.endpoint);

    // A second use of the link is refused.
    const again = await page.request.post("/api/alarms/subscribe", {
      data: { token, subscription: SUBSCRIPTION },
    });
    expect(again.status()).toBe(403);
  });

  test("a link without its token says so and offers no button", async ({ page }) => {
    await page.goto("/alarms");
    await expect(page.locator(".login-error")).toHaveText("Odkaz je neúplný. Otvor celý odkaz zo správy.");
    await expect(page.getByTestId("alarm-enable")).toHaveCount(0);
  });
});
