import { ENGINEER_PIN, MEMBER_PIN } from "./support/pins";
import { REAPER_ABSENT, test, expect, Page } from "./support/fixtures";

// Helper to get a JWT token via API
async function getToken(
  page: Page,
  member: string,
  pin: string = MEMBER_PIN,
): Promise<{ token: string; member: string; engineer: boolean } | null> {
  const response = await page.request.post("/api/auth", {
    data: { member, pin },
  });
  if (response.status() === 200) {
    return await response.json();
  }
  return null;
}

test.describe("Auto-redirect authenticated users (#84)", () => {
  // REAPER absent in mock E2E until S5
  test.use({ allowedConsole: REAPER_ABSENT });

  test("valid token + fresh session → redirects to member mixer", async ({
    page,
  }) => {
    // Get a valid token
    await page.goto("/");
    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(1);

    const member = members[0].id;
    const auth = await getToken(page, member);
    expect(auth).not.toBeNull();

    // Set auth in localStorage (simulating a returning user)
    await page.evaluate(
      ({ token, member, engineer }) => {
        localStorage.setItem(
          "iem_token",
          JSON.stringify({ token, member, engineer }),
        );
        // Ensure no sessionStorage flag (fresh session)
        sessionStorage.removeItem("iem_redirected");
      },
      { token: auth!.token, member: auth!.member, engineer: auth!.engineer },
    );

    // Navigate to landing page — should auto-redirect to /{member}
    await page.goto("/");

    // Wait for WASM hydration and redirect
    await page.waitForURL(`**/${member}`, { timeout: 10000 });
    expect(page.url()).toContain(`/${member}`);
  });

  test("expired token + fresh session → redirects to login with member param", async ({
    page,
  }) => {
    await page.goto("/");
    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(1);

    const member = members[0].id;

    // Create an expired JWT (exp in the past)
    const expiredPayload = btoa(
      JSON.stringify({
        sub: member,
        exp: Math.floor(Date.now() / 1000) - 3600,
      }),
    )
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
    const fakeToken = `eyJhbGciOiJIUzI1NiJ9.${expiredPayload}.fake_signature`;

    // Set expired auth in localStorage
    await page.evaluate(
      ({ token, member }) => {
        localStorage.setItem(
          "iem_token",
          JSON.stringify({ token, member, engineer: false }),
        );
        sessionStorage.removeItem("iem_redirected");
      },
      { token: fakeToken, member },
    );

    // Navigate to landing page — should redirect to login
    await page.goto("/");

    // Wait for redirect to login page with member param
    await page.waitForURL(/\/login/, { timeout: 10000 });
    const url = page.url();
    expect(url).toContain("/login");
    expect(url).toContain(`member=${member}`);
    expect(url).toContain(`next=/${member}`);
  });

  test("no token → stays on landing page", async ({ page }) => {
    // Ensure no auth state
    await page.goto("/");
    await page.evaluate(() => {
      localStorage.removeItem("iem_token");
      sessionStorage.removeItem("iem_redirected");
    });

    // Navigate to landing page
    await page.goto("/");

    // Wait for WASM to hydrate and content to load
    await page.waitForLoadState("domcontentloaded");

    // Should stay on landing page (wait a bit to ensure no redirect happens)
    await page.waitForTimeout(2000);
    expect(page.url()).toMatch(/\/$/);
  });

  test("after auto-redirect, back to / → shows landing page (no re-redirect)", async ({
    page,
  }) => {
    // Get a valid token
    await page.goto("/");
    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(1);

    const member = members[0].id;
    const auth = await getToken(page, member);
    expect(auth).not.toBeNull();

    // Set auth in localStorage
    await page.evaluate(
      ({ token, member, engineer }) => {
        localStorage.setItem(
          "iem_token",
          JSON.stringify({ token, member, engineer }),
        );
        sessionStorage.removeItem("iem_redirected");
      },
      { token: auth!.token, member: auth!.member, engineer: auth!.engineer },
    );

    // Navigate to landing → should auto-redirect
    await page.goto("/");
    await page.waitForURL(`**/${member}`, { timeout: 10000 });

    // Now navigate back to landing page
    await page.goto("/");

    // sessionStorage flag should prevent re-redirect
    await page.waitForLoadState("domcontentloaded");
    await page.waitForTimeout(2000);
    expect(page.url()).toMatch(/\/$/);
  });

  test("after logout (localStorage cleared) → stays on landing page", async ({
    page,
  }) => {
    // Simulate: user was logged in, then logged out
    await page.goto("/");
    await page.evaluate(() => {
      // Logout clears localStorage auth
      localStorage.removeItem("iem_token");
      // sessionStorage may still have the flag from a previous redirect
      sessionStorage.setItem("iem_redirected", "1");
    });

    // Navigate to landing page
    await page.goto("/");

    // No auth → should stay on landing
    await page.waitForLoadState("domcontentloaded");
    await page.waitForTimeout(2000);
    expect(page.url()).toMatch(/\/$/);
  });

  test("engineer token → redirects to member mixer", async ({ page }) => {
    // Get an engineer token
    await page.goto("/");
    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(1);

    const member = members[0].id;
    const auth = await getToken(page, member, ENGINEER_PIN); // Engineer PIN
    expect(auth).not.toBeNull();
    expect(auth!.engineer).toBe(true);

    // Set engineer auth in localStorage
    await page.evaluate(
      ({ token, member, engineer }) => {
        localStorage.setItem(
          "iem_token",
          JSON.stringify({ token, member, engineer }),
        );
        sessionStorage.removeItem("iem_redirected");
      },
      { token: auth!.token, member: auth!.member, engineer: auth!.engineer },
    );

    // Navigate to landing page — should redirect to /{auth.member}
    // Engineer token's member field may differ from the requested member
    await page.goto("/");
    await page.waitForURL(`**/${auth!.member}`, { timeout: 10000 });
    expect(page.url()).toContain(`/${auth!.member}`);
  });
});
