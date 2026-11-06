import type { Page, Response } from "@playwright/test";
import { test, expect } from "./support/fixtures";
import { ENGINEER_PIN, MEMBER_PIN, wrongPin } from "./support/pins";
import { login, openMixer } from "./support/session";

// A real PIN change, through the PIN dialog and the login page. PINs are
// shared by the whole run, so every test leaves member8 with MEMBER_PIN, also
// when it fails: `putBack`. The logins act as a tunnel client from their own
// TEST-NET-3 address, so a refused one spends nothing of another spec's budget.
const MEMBER = "member8";
const NEW_PIN = wrongPin();

function tunnelClient(n: number): Record<string, string> {
  return { "CF-Connecting-IP": `203.0.113.${n}` };
}

const isChange = (r: Response) => r.url().endsWith("/api/auth/change-pin") && r.request().method() === "POST";
const isLogin = (r: Response) => r.url().endsWith("/api/auth") && r.request().method() === "POST";

async function openDialog(page: Page) {
  await page.locator(".settings-btn").click();
  await page.locator(".settings-modal .settings-action-btn", { hasText: "Change PIN" }).click();
  const dialog = page.locator(".pin-modal:not(.settings-modal)");
  await expect(dialog.locator("h2")).toHaveText("Change PIN");
  return dialog;
}

/** Types `pins` on the dialog's numpad, saves, and returns the answer's status. */
async function submit(page: Page, pins: string[]): Promise<number> {
  const dialog = page.locator(".pin-modal:not(.settings-modal)");
  for (const digit of pins.join("")) {
    await dialog.locator(".pin-numpad .numpad-btn").filter({ hasText: new RegExp(`^${digit}$`) }).click();
  }
  const answered = page.waitForResponse(isChange);
  await dialog.locator(".numpad-btn.save").click();
  return (await answered).status();
}

async function closeSuccess(page: Page) {
  const dialog = page.locator(".pin-modal:not(.settings-modal)");
  await expect(dialog.locator(".pin-success h2")).toHaveText("PIN Changed");
  await dialog.locator(".pin-done").click();
  await expect(dialog).toHaveCount(0);
}

/** The login page's own attempt: its status, the PIN typed on the keyboard. */
async function typeLogin(page: Page, pin: string): Promise<number> {
  const answered = page.waitForResponse(isLogin);
  for (const digit of pin) await page.keyboard.press(digit);
  return (await answered).status();
}

/** member8 has MEMBER_PIN again, whatever the test left. */
async function putBack(page: Page): Promise<void> {
  const engineer = await login(page, "engineer", true);
  const reset = await page.request.post("/api/auth/change-pin", {
    headers: { Authorization: `Bearer ${engineer.token}` },
    data: { member: MEMBER, new_pin: MEMBER_PIN },
  });
  expect(reset.status()).toBe(200);
}

test.describe("A member changes their PIN", () => {
  // The refused logins and the wrong current PIN are deliberate: Chrome
  // reports each rejected request.
  test.use({ allowedConsole: [/status of 401/] });

  test("through the dialog: the old PIN is refused, the new one logs in, and it goes back", async ({ page }) => {
    await page.setExtraHTTPHeaders(tunnelClient(21));
    try {
      await openMixer(page, MEMBER);
      await openDialog(page);
      expect(await submit(page, [MEMBER_PIN, NEW_PIN, NEW_PIN])).toBe(200);
      await closeSuccess(page);

      await page.locator(".settings-btn").click();
      await page.locator(".settings-modal .logout-btn").click();
      await expect(page).toHaveURL(/\/$/);

      await page.goto(`/login?member=${MEMBER}&next=/${MEMBER}`);
      await expect(page.locator(".numpad")).toBeVisible({ timeout: 10_000 });
      expect(await typeLogin(page, MEMBER_PIN)).toBe(401);
      await expect(page.getByText("Invalid PIN")).toBeVisible();
      await expect(page.locator(".pin-dot.filled")).toHaveCount(0);

      expect(await typeLogin(page, NEW_PIN)).toBe(200);
      await expect(page).toHaveURL(new RegExp(`/${MEMBER}$`));
      await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });

      await openDialog(page);
      expect(await submit(page, [NEW_PIN, MEMBER_PIN, MEMBER_PIN])).toBe(200);
      await closeSuccess(page);
      const back = await page.request.post("/api/auth", { data: { member: MEMBER, pin: MEMBER_PIN } });
      expect(back.status()).toBe(200);
    } finally {
      await putBack(page);
    }
  });

  test("a wrong current PIN changes nothing and the dialog says so", async ({ page }) => {
    await page.setExtraHTTPHeaders(tunnelClient(22));
    try {
      await openMixer(page, MEMBER);
      const dialog = await openDialog(page);
      expect(await submit(page, [NEW_PIN, NEW_PIN, NEW_PIN])).toBe(401);
      await expect(dialog.locator(".pin-error")).toHaveText("Wrong current PIN");
      const unchanged = await page.request.post("/api/auth", { data: { member: MEMBER, pin: MEMBER_PIN } });
      expect(unchanged.status()).toBe(200);
    } finally {
      await putBack(page);
    }
  });
});

test.describe("The engineer resets a member's PIN", () => {
  // The engineer's session subscribes to Web Push; Playwright's contexts are
  // incognito, where Chrome has no Push API. The refused login is deliberate.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/, /status of 401/], { scope: "test" }],
  });

  test("from the member's page, without the current PIN", async ({ page }) => {
    await page.setExtraHTTPHeaders(tunnelClient(23));
    try {
      await openMixer(page, "engineer", { engineer: true, path: MEMBER });
      const dialog = await openDialog(page);
      await expect(dialog.getByText("Current PIN")).toHaveCount(0);
      expect(await submit(page, [NEW_PIN, NEW_PIN])).toBe(200);
      await closeSuccess(page);

      const oldPin = await page.request.post("/api/auth", { data: { member: MEMBER, pin: MEMBER_PIN } });
      expect(oldPin.status()).toBe(401);
      const newPin = await page.request.post("/api/auth", { data: { member: MEMBER, pin: NEW_PIN } });
      expect(newPin.status()).toBe(200);
      expect((await newPin.json()).engineer).toBe(false);
      // The engineer's own PIN did not change.
      const own = await page.request.post("/api/auth", { data: { member: "engineer", pin: ENGINEER_PIN } });
      expect(own.status()).toBe(200);
    } finally {
      await putBack(page);
    }
  });
});
