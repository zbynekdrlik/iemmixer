import { REAPER_ABSENT, test, expect } from "./support/fixtures";
import { MEMBER_PIN } from "./support/pins";

test.describe("Site links from the site config", () => {
  // REAPER absent in mock E2E until S5 (the mixer page loads REAPER-era state)
  test.use({ allowedConsole: REAPER_ABSENT });

  test("GET /api/site returns the test site's LAN URL and public host", async ({ request }) => {
    const site = await (await request.get("/api/site")).json();
    expect(site).toEqual({ lan_url: "http://10.0.0.10", public_host: "mixer.example.org" });
  });

  test("the member banner points at the configured LAN URL while the tunnel is down", async ({ page }) => {
    // CI runs no cloudflared, so the tunnel watchdog reports Down after its first poll.
    const members = (await (await page.request.get("/api/members")).json()) as Array<{ id: string }>;
    const member = members.find((m) => m.id !== "engineer")!.id;
    const auth = await (await page.request.post("/api/auth", { data: { member, pin: MEMBER_PIN } })).json();
    await page.goto("/");
    await page.evaluate(({ token, member, engineer }) => {
      localStorage.setItem("iem_token", JSON.stringify({ token, member, engineer }));
    }, auth);
    await page.goto(`/${member}`);
    await expect(page.getByTestId("tunnel-banner")).toContainText("http://10.0.0.10", { timeout: 15000 });
  });
});
