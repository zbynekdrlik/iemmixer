import { expect, Page, Locator } from "@playwright/test";
import { ENGINEER_PIN, MEMBER_PIN } from "./pins";

export type Auth = { token: string; member: string; engineer: boolean };

/** Logs in through the API (member PIN, or the engineer PIN when `engineer`). */
export async function login(page: Page, member: string, engineer = false): Promise<Auth> {
  const resp = await page.request.post("/api/auth", {
    data: { member, pin: engineer ? ENGINEER_PIN : MEMBER_PIN },
  });
  expect(resp.status(), `login ${member}`).toBe(200);
  const body = await resp.json();
  return { token: body.token, member: body.member, engineer: body.engineer };
}

/**
 * Signs in as `member` and opens the mixer page `path` (default: the member's
 * own page); returns once the page's state has arrived (IEM VOL is shown).
 */
export async function openMixer(
  page: Page,
  member: string,
  opts: { engineer?: boolean; path?: string } = {},
): Promise<Auth> {
  await page.goto("/");
  const auth = await login(page, member, opts.engineer ?? false);
  await page.evaluate((a) => {
    localStorage.setItem("iem_token", JSON.stringify(a));
    sessionStorage.setItem("iem_redirected", "1");
  }, auth);
  await page.goto(`/${opts.path ?? member}`);
  await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 15_000 });
  return auth;
}

/** The strip of channel `id` (an engine input or heard mix). */
export function strip(page: Page, id: string): Locator {
  return page.locator(`.channel[data-channel="${id}"]`);
}

/** Selects a category tab by its label. */
export async function tab(page: Page, label: "Main" | "Mics" | "Stems" | "Tech" | "Mixes"): Promise<void> {
  await page.locator(".category-tab", { hasText: label }).first().click();
}

/** The dB label of a strip ("-∞dB", "-6.0dB", "+0.0dB"). */
export async function dbText(s: Locator): Promise<string> {
  return (await s.locator(".db-display").first().textContent())?.trim() ?? "";
}

/**
 * Drags the fader of `s` by `fraction` of its width (relative movement after
 * the 150 ms activation hold, as a finger does).
 */
export async function dragFader(page: Page, s: Locator, fraction: number): Promise<void> {
  const track = s.locator(".fader-track").first();
  const box = await track.boundingBox();
  if (!box) throw new Error("fader track not laid out");
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.waitForTimeout(300);
  await page.mouse.move(x + box.width * fraction, y, { steps: 10 });
  await page.mouse.up();
}

/** Opens the kebab menu of a strip and clicks the item with `label`. */
export async function menu(s: Locator, label: string): Promise<void> {
  await s.locator(".ch-menu-btn").click();
  await s.locator(".ch-menu-item", { hasText: label }).click();
}
