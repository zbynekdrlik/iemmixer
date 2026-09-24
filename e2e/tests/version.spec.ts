import { test, expect } from "./support/fixtures";

test.describe("Version label (version-on-dashboard)", () => {
  test("landing page shows the backend version as v<semver>", async ({ page }) => {
    const api = await (await page.request.get("/api/version")).json();
    await page.goto("/");
    const label = page.getByTestId("version").first();
    await expect(label).toBeVisible();
    const text = ((await label.textContent()) ?? "").trim();
    expect(text).toMatch(/^v\d+\.\d+\.\d+(-dev\.\d+)?$/);
    expect(text).toBe(`v${api.version}`);
  });
});
