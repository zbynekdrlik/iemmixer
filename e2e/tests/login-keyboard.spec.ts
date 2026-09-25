import { MEMBER_PIN } from "./support/pins";
import { REAPER_ABSENT, test, expect } from "./support/fixtures";

test.describe("Issue #106 - PIN Keyboard Input", () => {
  // REAPER absent in mock E2E until S5
  test.use({ allowedConsole: REAPER_ABSENT });

  test("can enter PIN digits using keyboard number keys", async ({ page }) => {
    // Navigate to login page for a member
    const membersRes = await page.request.get("/api/members");
    expect(membersRes.ok()).toBe(true);
    const members = await membersRes.json();
    expect(members.length).toBeGreaterThan(0);

    const member = members[0].id;
    await page.goto(`/login?member=${member}&next=/${member}`);

    // Wait for numpad to render (WASM hydration)
    const numpad = page.locator(".numpad");
    await expect(numpad).toBeVisible({ timeout: 10000 });

    // Type 2 digits via keyboard
    await page.keyboard.press("4");
    await page.keyboard.press("4");

    // Verify 2 dots filled
    const filled = page.locator(".pin-dot.filled");
    await expect(filled).toHaveCount(2);

    // Backspace removes last digit
    await page.keyboard.press("Backspace");
    await expect(filled).toHaveCount(1);

    // Escape clears all
    await page.keyboard.press("4");
    await page.keyboard.press("Escape");
    await expect(filled).toHaveCount(0);
  });

  test("keyboard ignores non-digit keys", async ({ page }) => {
    const membersRes = await page.request.get("/api/members");
    expect(membersRes.ok()).toBe(true);
    const members = await membersRes.json();
    expect(members.length).toBeGreaterThan(0);

    const member = members[0].id;
    await page.goto(`/login?member=${member}&next=/${member}`);

    const numpad = page.locator(".numpad");
    await expect(numpad).toBeVisible({ timeout: 10000 });

    // Press non-digit keys — should not enter any digits
    await page.keyboard.press("a");
    await page.keyboard.press("Enter");
    await page.keyboard.press(" ");
    await page.keyboard.press("F1");

    const filled = page.locator(".pin-dot.filled");
    await expect(filled).toHaveCount(0);
  });

  test("Delete key clears PIN", async ({ page }) => {
    const membersRes = await page.request.get("/api/members");
    expect(membersRes.ok()).toBe(true);
    const members = await membersRes.json();
    expect(members.length).toBeGreaterThan(0);

    const member = members[0].id;
    await page.goto(`/login?member=${member}&next=/${member}`);

    const numpad = page.locator(".numpad");
    await expect(numpad).toBeVisible({ timeout: 10000 });

    // Enter 3 digits then Delete to clear
    await page.keyboard.press("1");
    await page.keyboard.press("2");
    await page.keyboard.press("3");

    const filled = page.locator(".pin-dot.filled");
    await expect(filled).toHaveCount(3);

    await page.keyboard.press("Delete");
    await expect(filled).toHaveCount(0);
  });

  test("full keyboard PIN entry triggers auto-submit", async ({ page }) => {
    const membersRes = await page.request.get("/api/members");
    expect(membersRes.ok()).toBe(true);
    const members = await membersRes.json();
    expect(members.length).toBeGreaterThan(0);

    const member = members[0].id;
    await page.goto(`/login?member=${member}&next=/${member}`);

    const numpad = page.locator(".numpad");
    await expect(numpad).toBeVisible({ timeout: 10000 });

    // Type the member PIN — four digits auto-submit and redirect
    for (const digit of MEMBER_PIN) {
      await page.keyboard.press(digit);
    }

    // Should redirect to mixer page (or show error if wrong PIN)
    // Either way, something happens — we verify auto-submit triggered
    await page.waitForURL(`**/${member}`, { timeout: 5000 });
  });
});
