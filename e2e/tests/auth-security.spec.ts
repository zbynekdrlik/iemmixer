import { ENGINEER_PIN, MEMBER_PIN } from "./support/pins";
import { REAPER_ABSENT, test, expect, Page } from "./support/fixtures";

// Helper to login and get a JWT token
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

// Helper to login as a specific member and navigate to their mixer page
async function loginAs(page: Page, member: string) {
  await page.goto("/");
  const auth = await getToken(page, member);
  if (!auth) return;
  await page.evaluate(
    ({ token, member, engineer }) => {
      localStorage.setItem(
        "iem_token",
        JSON.stringify({ token, member, engineer }),
      );
    },
    { token: auth.token, member: auth.member, engineer: auth.engineer },
  );
}

test.describe("Cross-member access prevention (#77)", () => {
  // REAPER absent in mock E2E until S5
  test.use({ allowedConsole: REAPER_ABSENT });

  test("member cannot access another member's mixer API", async ({ page }) => {
    await page.goto("/");

    // Login as first available member
    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(2);

    const memberA = members[0].id;
    const memberB = members[1].id;

    // Get token for member A
    const authA = await getToken(page, memberA);
    expect(authA).not.toBeNull();

    // Try to access member B's mixer with member A's token → 403
    const resp = await page.request.get(`/api/mixer/${memberB}`, {
      headers: { Authorization: `Bearer ${authA!.token}` },
    });
    expect(resp.status()).toBe(403);
  });

  test("member can access own mixer API", async ({ page }) => {
    await page.goto("/");

    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(1);

    const member = members[0].id;
    const auth = await getToken(page, member);
    expect(auth).not.toBeNull();

    // Access own mixer → should succeed (200 or other non-auth error, NOT 401/403)
    const resp = await page.request.get(`/api/mixer/${member}`, {
      headers: { Authorization: `Bearer ${auth!.token}` },
    });
    // Could be 200 or 502 (REAPER offline), but NOT 401 or 403
    expect(resp.status()).not.toBe(401);
    expect(resp.status()).not.toBe(403);
  });

  test("no token returns 401 on protected endpoint", async ({ page }) => {
    await page.goto("/");

    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(1);

    const member = members[0].id;

    // No auth header → 401
    const resp = await page.request.get(`/api/mixer/${member}`);
    expect(resp.status()).toBe(401);
  });

  test("member cannot access another member's presets", async ({ page }) => {
    await page.goto("/");

    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(2);

    const memberA = members[0].id;
    const memberB = members[1].id;

    const authA = await getToken(page, memberA);
    expect(authA).not.toBeNull();

    // Try to access member B's presets → 403
    const resp = await page.request.get(`/api/presets/${memberB}`, {
      headers: { Authorization: `Bearer ${authA!.token}` },
    });
    expect(resp.status()).toBe(403);
  });

  test("member cannot access another member's customization", async ({
    page,
  }) => {
    await page.goto("/");

    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(2);

    const memberA = members[0].id;
    const memberB = members[1].id;

    const authA = await getToken(page, memberA);
    expect(authA).not.toBeNull();

    // Try to access member B's customization → 403
    const resp = await page.request.get(`/api/mixer/${memberB}/customization`, {
      headers: { Authorization: `Bearer ${authA!.token}` },
    });
    expect(resp.status()).toBe(403);
  });

  test("engineer token can access any member's mixer", async ({ page }) => {
    await page.goto("/");

    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(1);

    const member = members[0].id;

    // Login with engineer PIN (<PIN>)
    const authEng = await getToken(page, member, ENGINEER_PIN);
    expect(authEng).not.toBeNull();
    expect(authEng!.engineer).toBe(true);

    // Engineer can access any member → NOT 401/403
    const resp = await page.request.get(`/api/mixer/${member}`, {
      headers: { Authorization: `Bearer ${authEng!.token}` },
    });
    expect(resp.status()).not.toBe(401);
    expect(resp.status()).not.toBe(403);
  });

  test("engineer member rejects a member PIN", async ({ page }) => {
    await page.goto("/");

    // Try to login as "engineer" with a member PIN — must be rejected
    const resp = await page.request.post("/api/auth", {
      data: { member: "engineer", pin: MEMBER_PIN },
    });

    // Must be rejected — engineer requires engineer PIN (<PIN>)
    expect(resp.status()).toBe(401);
  });

  test("engineer member accepts engineer PIN", async ({ page }) => {
    await page.goto("/");

    // Login as "engineer" with engineer PIN (<PIN>)
    const resp = await page.request.post("/api/auth", {
      data: { member: "engineer", pin: ENGINEER_PIN },
    });

    // Must succeed with engineer flag
    expect(resp.status()).toBe(200);
    const body = await resp.json();
    expect(body.engineer).toBe(true);
  });

  test("engineer token expires in 7 days (same as members)", async ({
    page,
  }) => {
    await page.goto("/");

    // Login as engineer
    const engResp = await page.request.post("/api/auth", {
      data: { member: "engineer", pin: ENGINEER_PIN },
    });
    expect(engResp.status()).toBe(200);
    const engBody = await engResp.json();
    expect(engBody.expires_in).toBe(604800); // 7 days

    // Login as member
    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(1);

    const memberResp = await page.request.post("/api/auth", {
      data: { member: members[0].id, pin: MEMBER_PIN },
    });
    expect(memberResp.status()).toBe(200);
    const memberBody = await memberResp.json();

    // Both should have same expiry
    expect(engBody.expires_in).toBe(memberBody.expires_in);
  });

  test("cross-member access blocked on control endpoints", async ({ page }) => {
    await page.goto("/");

    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(2);

    const memberA = members[0].id;
    const memberB = members[1].id;

    const authA = await getToken(page, memberA);
    expect(authA).not.toBeNull();

    // Try to set level on member B's mixer → 403
    const levelResp = await page.request.post(
      `/api/mixer/${memberB}/track/1/level`,
      {
        headers: { Authorization: `Bearer ${authA!.token}` },
        data: { level_db: -10 },
      },
    );
    expect(levelResp.status()).toBe(403);

    // Try to set mute on member B's mixer → 403
    const muteResp = await page.request.post(
      `/api/mixer/${memberB}/track/1/mute`,
      {
        headers: { Authorization: `Bearer ${authA!.token}` },
        data: { muted: true },
      },
    );
    expect(muteResp.status()).toBe(403);

    // Try to set pan on member B's mixer → 403
    const panResp = await page.request.post(
      `/api/mixer/${memberB}/track/1/pan`,
      {
        headers: { Authorization: `Bearer ${authA!.token}` },
        data: { pan: 0.5 },
      },
    );
    expect(panResp.status()).toBe(403);
  });

  test("frontend redirects to login page on cross-member access", async ({
    page,
  }) => {
    await page.goto("/");

    const membersResp = await page.request.get("/api/members");
    const members = await membersResp.json();
    expect(members.length).toBeGreaterThanOrEqual(2);

    const memberA = members[0].id;
    const memberB = members[1].id;

    // Login as member A
    await loginAs(page, memberA);

    // Try to navigate to member B's mixer page
    await page.goto(`/${memberB}`);

    // Should be redirected to login page with member param due to JWT sub mismatch
    await page.waitForURL(/\/login/, { timeout: 5000 });

    // Verify we're on the login page with correct member param
    const url = page.url();
    expect(url).toContain("/login");
    expect(url).toContain(`member=${memberB}`);
  });
});
