import type { Browser, Locator } from "@playwright/test";
import { test, expect, collectConsole, type Page } from "./support/fixtures";
import { withSocket } from "./support/mixer-socket";
import { ENGINEER_PIN } from "./support/pins";
import { openMixer, openPage, strip, tab } from "./support/session";

// The bands (#58) in the browser. The e2e job runs in roster mode: it puts
// config/test-roster.toml beside the site file, so `main` (member1 …
// member9 on their own sets, led by member1 and member3) plays at the start,
// and `youth` puts member10 on member1's set, member3 on their own and
// member11 on member4's, led by member3.
//
// This spec owns which band plays: its tests run in order, each switch is
// clicked by a leader or the engineer, and afterAll makes main play again,
// so every other spec runs while main plays (one worker runs one spec at a
// time). member10 and member11 are its people.

test.describe.configure({ mode: "serial" });

const BASE_URL = process.env.E2E_BASE_URL || "http://localhost:80";

const MAIN = "Main band";
const YOUTH = "Youth band";

/** The confirmation of a switch from `active` to `target`. */
function confirmation(active: string, target: string): string {
  return `Teraz hrá ${active}. Po prepnutí bude hrať ${target} a každý dostane svoj mix pre túto kapelu.`;
}

/** The notice of a person who does not play in `band`. */
function notice(band: string): string {
  return `Teraz hrá ${band}. V tejto kapele nehráš.`;
}

/**
 * Picks `target` in the band switch inside `scope`, checks the confirmation
 * names both bands (`body`) and confirms; returns once the server answered
 * that the band switched (204).
 */
async function switchTo(page: Page, scope: Locator, target: string, body: string): Promise<void> {
  const control = scope.getByTestId("lineup-switch");
  await control.getByTestId("lineup-target").selectOption(target);
  await control.getByTestId("lineup-switch-button").click();
  const dialog = control.locator(".confirm-overlay.visible");
  await expect(dialog.locator(".confirm-title")).toHaveText("Prepnúť kapelu?");
  await expect(dialog.locator(".confirm-body")).toHaveText(body);
  const confirm = dialog.locator(".confirm-btn-confirm");
  await expect(confirm).toHaveText("Prepnúť");
  const answer = page.waitForResponse(
    (r) => new URL(r.url()).pathname === "/api/lineup/switch" && r.request().method() === "POST",
  );
  await confirm.click();
  expect((await answer).status(), `the switch to ${target}`).toBe(204);
}

/** A page in a browser context of its own, its console guarded like the test's page. */
type OtherPage = { page: Page; problems: string[]; close: () => Promise<void> };

async function otherPage(browser: Browser): Promise<OtherPage> {
  const context = await browser.newContext();
  const page = await context.newPage();
  const problems = collectConsole(page, []);
  return {
    page,
    problems,
    close: async () => {
      await context.close();
      expect(problems, "the other page's console must stay clean").toEqual([]);
    },
  };
}

/**
 * The notice on `other`'s page, saying `text`, with a clean console: a page
 * that panicked when its mixer gave way to the notice (#61 second review C1)
 * shows its console as the failure, not only a missing notice.
 */
async function expectNotice(other: OtherPage, text: string, timeout = 10_000): Promise<void> {
  const shown = other.page.getByTestId("lineup-notice");
  try {
    await expect(shown).toBeVisible({ timeout });
  } catch (missing) {
    expect(other.problems, "the console of the page without its notice").toEqual([]);
    throw missing;
  }
  await expect(shown.locator(".lineup-notice-text")).toHaveText(text);
  expect(other.problems, "the console once the notice shows").toEqual([]);
}

/** The mixer sockets `page` opens to `/ws/<member>` from now on. */
function socketsTo(page: Page, member: string): string[] {
  const urls: string[] = [];
  page.on("websocket", (ws) => {
    if (new URL(ws.url()).pathname === `/ws/${member}`) urls.push(ws.url());
  });
  return urls;
}

/** Marks the document, so a later check can tell it was never reloaded. */
async function markDocument(page: Page): Promise<void> {
  await page.evaluate(() => {
    (window as unknown as { iemNotReloaded: boolean }).iemNotReloaded = true;
  });
}

async function notReloaded(page: Page): Promise<boolean> {
  return page.evaluate(() => (window as unknown as { iemNotReloaded?: boolean }).iemNotReloaded === true);
}

/** The landing grid's children in order: a tile's link, or the divider's test id. */
async function tileOrder(page: Page): Promise<string[]> {
  const grid = page.locator(".member-grid");
  await expect(grid.locator(".member-card").first()).toBeVisible();
  return grid
    .locator(":scope > *")
    .evaluateAll((els) => els.map((e) => e.getAttribute("href") ?? e.getAttribute("data-testid") ?? "?"));
}

test.describe("Bands (#58)", () => {
  test.afterAll(async ({ playwright }) => {
    // Main plays again for every spec after this one, whatever happened here.
    const api = await playwright.request.newContext({ baseURL: BASE_URL });
    try {
      const auth = await api.post("/api/auth", { data: { member: "engineer", pin: ENGINEER_PIN } });
      expect(auth.status()).toBe(200);
      const { token } = await auth.json();
      const back = await api.post("/api/lineup/switch", {
        headers: { Authorization: `Bearer ${token}` },
        data: { to: "main" },
      });
      if (back.status() === 409) {
        expect((await back.json()).code).toBe("ALREADY_PLAYING");
      } else {
        expect(back.status()).toBe(204);
      }
    } finally {
      await api.dispose();
    }
  });

  test("only leaders get the Mixes tab and the switch", async ({ page, browser }) => {
    // member4 plays in main and leads no band.
    await openMixer(page, "member4");
    await expect(page.getByTestId("lineup-badge")).toHaveText(MAIN);
    await expect(page.locator(".category-tab.mixes")).toHaveClass(/tab-hidden/);
    await page.locator(".settings-btn").click();
    await expect(page.locator(".settings-modal")).toBeVisible();
    await expect(page.getByTestId("lineup-section")).toHaveCount(0);
    await expect(page.getByTestId("lineup-switch")).toHaveCount(0);

    // member3 leads main (and youth).
    const leader = await otherPage(browser);
    await openMixer(leader.page, "member3");
    await expect(leader.page.locator(".category-tab.mixes")).not.toHaveClass(/tab-hidden/);
    await leader.page.locator(".settings-btn").click();
    const section = leader.page.getByTestId("lineup-section");
    await expect(section.locator(".settings-section-title")).toHaveText("Kapela");
    await expect(section.getByTestId("lineup-target").locator("option")).toHaveText([YOUTH]);
    await expect(section.getByTestId("lineup-switch-button")).toHaveText("Prepnúť kapelu");
    await expect(section.getByTestId("lineup-switch-button")).toBeEnabled();
    await leader.close();
  });

  // The e2e job runs in roster mode; a site without a roster answers
  // /api/lineup 204, which the page here gets in its place (#61 review).
  test("a site without a roster keeps the page as it was (legacy smoke)", async ({ page }) => {
    await page.route(
      (url) => url.pathname === "/api/lineup",
      (route) => route.fulfill({ status: 204 }),
    );
    // member3 leads both bands: in roster mode the badge and the switch.
    await openMixer(page, "member3");
    await expect(page.locator(".mixer-header .status-dot")).toHaveClass(/\bconnected\b/);
    await expect(page.getByTestId("lineup-badge")).toHaveCount(0);
    await expect(page.getByTestId("lineup-notice")).toHaveCount(0);
    await page.locator(".settings-btn").click();
    await expect(page.locator(".settings-modal")).toBeVisible();
    await expect(page.getByTestId("lineup-section")).toHaveCount(0);
    await expect(page.getByTestId("lineup-switch")).toHaveCount(0);
  });

  test("the login tiles show the band that plays first, then the others", async ({ page }) => {
    await page.goto("/");
    const nine = Array.from({ length: 9 }, (_, i) => `/member${i + 1}`);
    expect(await tileOrder(page)).toEqual([...nine, "/engineer", "other-band", "/member10", "/member11"]);
    await expect(page.getByTestId("other-band")).toHaveText("Iná kapela");
    const others = page.locator(".member-card.not-playing");
    await expect(others).toHaveCount(2);
    await expect(others.nth(0)).toHaveAttribute("href", "/member10");
    await expect(page.locator('.member-card[href="/member1"]')).not.toHaveClass(/not-playing/);
  });

  test("a leader switches to the youth band, and an open page follows without a reload", async ({
    page,
    browser,
  }) => {
    // member1's page is open while main plays; member1 plays no part of youth.
    const away = await otherPage(browser);
    await openMixer(away.page, "member1");
    await expect(away.page.getByTestId("lineup-badge")).toHaveText(MAIN);
    await markDocument(away.page);

    // member3 leads both bands and switches in the settings.
    const sockets = socketsTo(page, "member3");
    await openMixer(page, "member3");
    await markDocument(page);
    await expect(page.getByTestId("lineup-badge")).toHaveText(MAIN);
    await page.locator(".settings-btn").click();
    const section = page.getByTestId("lineup-section");
    await switchTo(page, section, "youth", confirmation(MAIN, YOUTH));
    await expect(section.getByTestId("lineup-switch-message")).toHaveText(`Hrá ${YOUTH}.`);
    await expect(page.getByTestId("lineup-badge")).toHaveText(YOUTH);
    // The other band is offered now.
    await expect(section.getByTestId("lineup-target").locator("option")).toHaveText([MAIN]);
    await page.locator(".settings-modal .modal-close").click();

    // member3 plays in youth on their own set: the page connected again (the
    // server closed its socket with 4003), never reloaded, never showed
    // "Reconnecting...", and its Mixes tab lists youth's sets only.
    await expect.poll(() => sockets.length).toBe(2);
    await expect(page.getByTestId("global-volume-fader")).toBeVisible();
    await expect(page.locator(".mixer-header .status-dot")).toHaveClass(/\bconnected\b/);
    await expect(page.locator(".disconnected-banner")).toHaveCount(0);
    expect(await notReloaded(page)).toBe(true);
    await tab(page, "Mixes");
    await expect(strip(page, "member1")).toBeVisible();
    await expect(strip(page, "member4")).toBeVisible();
    await expect(strip(page, "member2")).toHaveCount(0);

    // member1's open page shows the notice in place of the mixer, without a
    // reload, a spinner, "Reconnecting..." or an error in its console.
    await expectNotice(away, notice(YOUTH));
    const shown = away.page.getByTestId("lineup-notice");
    await expect(away.page.getByTestId("lineup-badge")).toHaveText(YOUTH);
    await expect(away.page.getByTestId("global-volume-fader")).toHaveCount(0);
    await expect(away.page.locator(".spinner")).toHaveCount(0);
    await expect(away.page.locator(".disconnected-banner")).toHaveCount(0);
    expect(await notReloaded(away.page)).toBe(true);
    expect(new URL(away.page.url()).pathname).toBe("/member1");
    // member1 leads main: the notice offers the switch back.
    await expect(shown.getByTestId("lineup-target").locator("option")).toHaveText([MAIN]);
    await away.close();
  });

  test("a youth member hears their own mix on a borrowed set", async ({ page, baseURL }) => {
    const auth = await openMixer(page, "member10");
    await expect(page.getByTestId("lineup-badge")).toHaveText(YOUTH);
    // Their own channel is member1's mic, under youth's name for it.
    const own = strip(page, "mic1");
    await expect(own.locator(".ch-name")).toHaveText("MEMBER10");
    // member10 leads no part of youth: no Mixes tab, no switch.
    await expect(page.locator(".category-tab.mixes")).toHaveClass(/tab-hidden/);
    await page.locator(".settings-btn").click();
    await expect(page.locator(".settings-modal")).toBeVisible();
    await expect(page.getByTestId("lineup-switch")).toHaveCount(0);
    // The server gives member10's page member1's set: the borrowed mix.
    const state = await withSocket(baseURL, "member10", auth.token, (s) => s.state());
    expect(state.mix).toBe("member1");
  });

  test("a person not in the band sees the notice, not a spinner", async ({ page }) => {
    await openPage(page, "member2");
    const shown = page.getByTestId("lineup-notice");
    await expect(shown).toBeVisible({ timeout: 5_000 });
    await expect(shown.locator(".lineup-notice-text")).toHaveText(notice(YOUTH));
    await expect(page.getByTestId("lineup-badge")).toHaveText(YOUTH);
    await expect(page.getByTestId("global-volume-fader")).toHaveCount(0);
    await expect(page.locator(".spinner")).toHaveCount(0);
    // No connection dot either (#61 M1): the page has no socket on purpose.
    await expect(page.locator(".mixer-header .status-dot")).toHaveCount(0);
    // member2 leads no band: no switch on the notice.
    await expect(shown.getByTestId("lineup-switch")).toHaveCount(0);
    // It stays the notice when the page reads the band again (every 5 s).
    const reread = page.waitForResponse((r) => new URL(r.url()).pathname === "/api/lineup", { timeout: 10_000 });
    expect((await reread).status()).toBe(200);
    await expect(shown).toBeVisible();
    await expect(page.getByTestId("global-volume-fader")).toHaveCount(0);
  });

  test.describe("a read that fails", () => {
    // The notice page's next read of the band is answered 502 once, as
    // cloudflared answers while the server restarts; Chrome logs that answer.
    test.use({ allowedConsole: [/status of 502/] });

    test("a failed read keeps the notice: no socket, no Reconnecting (#61 C2)", async ({ page }) => {
      const sockets = socketsTo(page, "member2");
      await openPage(page, "member2");
      const shown = page.getByTestId("lineup-notice");
      await expect(shown).toBeVisible();
      let failures = 0;
      await page.route(
        (url) => url.pathname === "/api/lineup",
        async (route) => {
          if (failures === 0) {
            failures += 1;
            await route.fulfill({ status: 502, body: "Bad Gateway" });
          } else {
            await route.continue();
          }
        },
      );
      const lineupAnswer = (status: number) =>
        page.waitForResponse((r) => new URL(r.url()).pathname === "/api/lineup" && r.status() === status, {
          timeout: 12_000,
        });
      await lineupAnswer(502);
      await expect(shown).toBeVisible();
      // The page reads again and the server answers: still the notice.
      await lineupAnswer(200);
      await expect(shown).toBeVisible();
      await expect(shown.locator(".lineup-notice-text")).toHaveText(notice(YOUTH));
      await expect(page.getByTestId("global-volume-fader")).toHaveCount(0);
      await expect(page.locator(".disconnected-banner")).toHaveCount(0);
      expect(sockets, "no socket the server would refuse").toHaveLength(0);
    });
  });

  test("while youth plays, its people come first on the login tiles", async ({ page }) => {
    await page.goto("/");
    const others = [1, 2, 4, 5, 6, 7, 8, 9].map((n) => `/member${n}`);
    expect(await tileOrder(page)).toEqual(["/member10", "/member3", "/member11", "/engineer", "other-band", ...others]);
    await expect(page.locator(".member-card.not-playing")).toHaveCount(8);
  });

  test("a leader switches back from the notice, and the page turns into the mixer", async ({ page, browser }) => {
    // member10's page, open while youth plays, misses the switch (#61 I1):
    // its socket's LineupChanged is dropped and the server's 4003 close
    // reaches the page as a plain close, as for a phone locked during the
    // switch. Later sockets pass through untouched.
    const missed = await otherPage(browser);
    let first = true;
    await missed.page.routeWebSocket(
      (url) => url.pathname === "/ws/member10",
      (ws) => {
        const server = ws.connectToServer();
        if (!first) return;
        first = false;
        server.onMessage((message) => {
          if (typeof message === "string" && message.includes('"LineupChanged"')) return;
          ws.send(message);
        });
        server.onClose(() => ws.close({ code: 1000 }));
      },
    );
    await openMixer(missed.page, "member10");
    await expect(missed.page.getByTestId("lineup-badge")).toHaveText(YOUTH);

    const sockets = socketsTo(page, "member1");
    await openPage(page, "member1");
    const shown = page.getByTestId("lineup-notice");
    await expect(shown).toBeVisible();
    await markDocument(page);
    expect(sockets).toHaveLength(0);
    await switchTo(page, shown, "main", confirmation(YOUTH, MAIN));
    // member1 plays in main: the notice gives way to their mixer.
    await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 10_000 });
    await expect(shown).toHaveCount(0);
    await expect(page.getByTestId("lineup-badge")).toHaveText(MAIN);
    await expect(page.locator(".category-tab.mixes")).not.toHaveClass(/tab-hidden/);
    expect(sockets).toHaveLength(1);
    expect(await notReloaded(page)).toBe(true);

    // member10 plays no part of main: the page that missed the switch reads
    // the band before it reconnects, and shows the notice.
    await expectNotice(missed, notice(MAIN), 15_000);
    await expect(missed.page.getByTestId("global-volume-fader")).toHaveCount(0);
    await missed.close();
  });

  test.describe("the engineer", () => {
    // The engineer page subscribes to Web Push; Playwright's browser contexts
    // are incognito, where Chrome has no Push API: its error and the page's
    // `[push] …` warnings are expected here.
    test.use({
      allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
    });

    test("the engineer switches on /engineer, and the page stays open", async ({ page }) => {
      const sockets = socketsTo(page, "engineer");
      await openMixer(page, "engineer", { engineer: true });
      await markDocument(page);
      await expect(page.getByTestId("lineup-badge")).toHaveText(MAIN);
      await page.locator(".settings-btn").click();
      const section = page.getByTestId("lineup-section");
      await switchTo(page, section, "youth", confirmation(MAIN, YOUTH));
      await expect(page.getByTestId("lineup-badge")).toHaveText(YOUTH);
      await expect(section.getByTestId("lineup-switch-message")).toHaveText(`Hrá ${YOUTH}.`);
      await expect(section.getByTestId("lineup-target").locator("option")).toHaveText([MAIN]);
      await switchTo(page, section, "main", confirmation(YOUTH, MAIN));
      await expect(page.getByTestId("lineup-badge")).toHaveText(MAIN);
      await expect(section.getByTestId("lineup-switch-message")).toHaveText(`Hrá ${MAIN}.`);
      // The engineer's page is never closed by a switch: one socket, no reload.
      expect(sockets).toHaveLength(1);
      expect(await notReloaded(page)).toBe(true);
      await page.locator(".settings-modal .modal-close").click();
      await expect(page.getByTestId("global-volume-fader")).toBeVisible();
    });

    test("the engineer on the page of a person who does not play sees the notice (#61 I2)", async ({ page }) => {
      await openMixer(page, "engineer", { engineer: true });
      await page.locator(".settings-btn").click();
      await switchTo(page, page.getByTestId("lineup-section"), "youth", confirmation(MAIN, YOUTH));
      await expect(page.getByTestId("lineup-badge")).toHaveText(YOUTH);

      // The engineer opens member2's dimmed tile: member2 plays no part of
      // youth, so the page shows the notice and opens no socket.
      const sockets = socketsTo(page, "member2");
      await page.goto("/");
      await page.locator('.member-card.not-playing[href="/member2"]').click();
      const shown = page.getByTestId("lineup-notice");
      await expect(shown).toBeVisible({ timeout: 5_000 });
      await expect(shown.locator(".lineup-notice-text")).toHaveText(notice(YOUTH));
      await expect(page.getByTestId("global-volume-fader")).toHaveCount(0);
      expect(sockets).toHaveLength(0);

      // The engineer may switch: back to main from the notice, and member2's
      // mixer opens on the same page.
      await switchTo(page, shown, "main", confirmation(YOUTH, MAIN));
      await expect(page.getByTestId("global-volume-fader")).toBeVisible({ timeout: 10_000 });
      await expect(page.getByTestId("lineup-badge")).toHaveText(MAIN);
      expect(sockets).toHaveLength(1);
    });
  });
});
