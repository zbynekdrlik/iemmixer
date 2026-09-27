import { test, expect, Page } from "./support/fixtures";
import { menu, openMixer, strip, tab } from "./support/session";

// EQ (F11) and limiter (F12) against the real engine; member4 owns mic4 and
// mic5 (X7: a member may edit the EQ of their own inputs).

async function closeEq(page: Page): Promise<void> {
  await page.locator(".eq-close-btn").click();
  await expect(page.locator(".eq-modal")).toHaveCount(0);
}

test.describe("EQ (F11)", () => {
  test("a member opens the EQ of their own input and a band toggle persists", async ({ page }) => {
    await openMixer(page, "member4");
    await menu(strip(page, "mic4"), "EQ");
    const modal = page.locator(".eq-modal");
    await expect(modal).toBeVisible();
    await expect(modal.locator(".eq-title")).toHaveText("EQ: MEMBER4 mic");
    await expect(modal.locator(".eq-band-card")).toHaveCount(5);

    // The high-pass is the first card (filters first); enabling it bends the curve.
    const hpf = modal.locator(".eq-band-card").first();
    await expect(hpf.locator(".eq-band-type")).toHaveText("highpass");
    const curve = modal.locator("path").first();
    const flat = await curve.getAttribute("d");
    const toggle = hpf.locator(".eq-band-toggle");
    const wasOn = ((await toggle.getAttribute("class")) ?? "").includes(" on");
    await toggle.click();
    await expect(toggle).toHaveClass(wasOn ? /off/ : /on/);
    await expect.poll(() => curve.getAttribute("d")).not.toBe(flat);

    // Reopen: the engine kept it.
    await closeEq(page);
    await menu(strip(page, "mic4"), "EQ");
    const again = page.locator(".eq-modal .eq-band-card").first().locator(".eq-band-toggle");
    await expect(again).toHaveClass(wasOn ? /off/ : /on/, { timeout: 5_000 });

    // Restore.
    await again.click();
    await expect(again).toHaveClass(wasOn ? /on/ : /off/);
    await closeEq(page);
  });

  test("another member's input offers no EQ (X7)", async ({ page }) => {
    await openMixer(page, "member4");
    await tab(page, "Mics");
    const other = strip(page, "mic1");
    await other.locator(".ch-menu-btn").click();
    await expect(other.locator(".ch-menu-item", { hasText: "Pin to Main" })).toBeVisible();
    await expect(other.locator(".ch-menu-item", { hasText: "EQ" })).toHaveCount(0);
  });

  test("IEM VOL opens the mix's output EQ", async ({ page }) => {
    await openMixer(page, "member4");
    await page.getByTestId("global-volume-fader").locator(".eq-btn-small").click();
    await expect(page.locator(".eq-modal .eq-title")).toHaveText("EQ: IEM VOL");
    await expect(page.locator(".eq-modal .eq-band-card")).toHaveCount(5);
    await closeEq(page);
  });
});

test.describe("Limiter (F12)", () => {
  test("the limiter of the member's mix switches and keeps its state", async ({ page }) => {
    await openMixer(page, "member4");
    await page.getByTestId("global-volume-fader").locator(".limiter-btn-small").click();
    const modal = page.locator(".limiter-modal");
    await expect(modal.locator(".limiter-title")).toHaveText("IEM VOL — Limiter");
    const toggle = modal.locator(".limiter-toggle-btn");
    await expect(toggle).toHaveText("ON");
    await toggle.click();
    await expect(toggle).toHaveText("OFF");
    await expect(modal.locator(".limiter-warning")).toHaveText("HEARING PROTECTION OFF");

    await modal.locator(".limiter-close-btn").click();
    await page.getByTestId("global-volume-fader").locator(".limiter-btn-small").click();
    await expect(page.locator(".limiter-modal .limiter-toggle-btn")).toHaveText("OFF");

    // Restore hearing protection.
    await page.locator(".limiter-modal .limiter-toggle-btn").click();
    await expect(page.locator(".limiter-modal .limiter-toggle-btn")).toHaveText("ON");
    await expect(page.locator(".limiter-modal .limiter-activity-label")).toContainText("limited");
    await page.locator(".limiter-modal .limiter-close-btn").click();
  });
});
