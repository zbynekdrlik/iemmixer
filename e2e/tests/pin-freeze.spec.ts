import { test, expect } from "./support/fixtures";
import { MEMBER_PIN, wrongPin } from "./support/pins";
import { openMixer } from "./support/session";

// Until the cutover PINs change only in the predecessor app (P9): every dev
// site runs with `pin_changes = false`, and so does the E2E site
// (config/test-site.toml). The server answers the PIN dialog with 409 and
// the Slovak reason, which the dialog shows; the PIN stays as it was.
test.describe("PIN changes before the cutover", () => {
  // The refused change is deliberate: Chrome reports the 409 response.
  test.use({ allowedConsole: [/status of 409/] });

  test("a member's PIN change is refused with the reason and the PIN still works", async ({ page }) => {
    const member = "member8";
    await openMixer(page, member);
    await page.locator(".settings-btn").click();
    await page.locator(".settings-modal .settings-action-btn", { hasText: "Change PIN" }).click();
    const dialog = page.locator(".pin-modal:not(.settings-modal)");
    await expect(dialog.locator("h2")).toHaveText("Change PIN");

    const type = async (pin: string) => {
      for (const digit of pin) {
        await dialog.locator(".pin-numpad .numpad-btn").filter({ hasText: new RegExp(`^${digit}$`) }).click();
      }
    };
    const next = wrongPin();
    await type(MEMBER_PIN); // current
    await type(next); // new
    await type(next); // confirm
    const answered = page.waitForResponse(
      (r) => r.url().endsWith("/api/auth/change-pin") && r.request().method() === "POST",
    );
    await dialog.locator(".numpad-btn.save").click();
    expect((await answered).status()).toBe(409);
    await expect(dialog.locator(".pin-error")).toHaveText("PIN sa zatiaľ mení v pôvodnej aplikácii");

    // Nothing changed: the old PIN still logs in.
    const oldPin = await page.request.post("/api/auth", { data: { member, pin: MEMBER_PIN } });
    expect(oldPin.status()).toBe(200);
  });
});
