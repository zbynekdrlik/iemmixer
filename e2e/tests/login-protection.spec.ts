import type { APIRequestContext, Response } from "@playwright/test";
import { test, expect } from "./support/fixtures";
import { MEMBER_PIN, wrongPin } from "./support/pins";

// Each test acts as a tunnel client from its own TEST-NET-3 address
// (CF-Connecting-IP from a loopback peer is trusted), so its failures never
// slow other tests; the run stays far below the 30-failure engineer budget.
function tunnelClient(n: number): Record<string, string> {
  return { "CF-Connecting-IP": `203.0.113.${n}` };
}

async function firstMember(request: APIRequestContext): Promise<string> {
  const members = (await (await request.get("/api/members")).json()) as Array<{ id: string }>;
  return members[0].id;
}

test.describe("Login protection (program spec §5.3)", () => {
  test("backoff after three failures, then the right PIN works again (never a lockout)", async ({ request }) => {
    const headers = tunnelClient(11);
    const member = await firstMember(request);
    for (let i = 0; i < 3; i++) {
      const failed = await request.post("/api/auth", { headers, data: { member, pin: wrongPin() } });
      expect(failed.status()).toBe(401);
    }
    const throttled = await request.post("/api/auth", { headers, data: { member, pin: MEMBER_PIN } });
    expect(throttled.status()).toBe(429);
    const retryAfter = Number(throttled.headers()["retry-after"]);
    expect(retryAfter).toBeGreaterThanOrEqual(1);
    expect(retryAfter).toBeLessThanOrEqual(60);
    await new Promise((resolve) => setTimeout(resolve, retryAfter * 1000 + 200));
    const ok = await request.post("/api/auth", { headers, data: { member, pin: MEMBER_PIN } });
    expect(ok.status()).toBe(200);
  });

  test("someone else's failures do not slow another client", async ({ request }) => {
    const member = await firstMember(request);
    for (let i = 0; i < 3; i++) {
      const failed = await request.post("/api/auth", { headers: tunnelClient(12), data: { member, pin: wrongPin() } });
      expect(failed.status()).toBe(401);
    }
    const other = await request.post("/api/auth", { headers: tunnelClient(13), data: { member, pin: MEMBER_PIN } });
    expect(other.status()).toBe(200);
    const lan = await request.post("/api/auth", { data: { member, pin: MEMBER_PIN } });
    expect(lan.status()).toBe(200);
  });

  test.describe("login page", () => {
    // The wrong PINs below are deliberate: Chrome reports each rejected request.
    test.use({ allowedConsole: [/status of 401/, /status of 429/] });

    test("shows the wait time after repeated wrong PINs", async ({ page }) => {
      await page.setExtraHTTPHeaders(tunnelClient(14));
      const member = await firstMember(page.request);
      await page.goto(`/login?member=${member}&next=/${member}`);
      await expect(page.locator(".numpad")).toBeVisible({ timeout: 10000 });
      const wrong = wrongPin();
      const isLogin = (r: Response) => r.url().endsWith("/api/auth") && r.request().method() === "POST";
      // Wait for each attempt's own response (a still-visible "Invalid PIN"
      // would race the next request). Three free failures, then 1, 2, 4 s:
      // the attempt typed right after the third failure is normally throttled;
      // on a slow runner the next one is, so allow up to six attempts.
      const statuses: number[] = [];
      while (!statuses.includes(429) && statuses.length < 6) {
        const response = page.waitForResponse(isLogin);
        for (const digit of wrong) await page.keyboard.press(digit);
        statuses.push((await response).status());
        if (statuses[statuses.length - 1] === 401) {
          await expect(page.getByText("Invalid PIN")).toBeVisible();
          await expect(page.locator(".pin-dot.filled")).toHaveCount(0);
        }
      }
      expect(statuses.slice(0, 3)).toEqual([401, 401, 401]);
      expect(statuses[statuses.length - 1]).toBe(429);
      await expect(page.getByText(/Too many attempts\. Try again in \d+ s/)).toBeVisible();
    });
  });
});
