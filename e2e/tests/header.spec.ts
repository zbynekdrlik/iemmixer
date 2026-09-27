import type { WebSocketRoute } from "@playwright/test";
import { test, expect } from "./support/fixtures";
import { openMixer, strip } from "./support/session";
import { PageSocket } from "./support/wire";

// The page headers (F10) against the real engine: the landing title, the
// mixer header's version and build date, the connection dot and the
// reconnect banner. member3's page, read-only: nothing here changes a mix.

/** The average of a computed `rgb(r, g, b)` colour's channels. */
function channelAverage(color: string): number {
  const m = /rgba?\((\d+),\s*(\d+),\s*(\d+)/.exec(color);
  if (!m) throw new Error(`not an rgb colour: ${color}`);
  return (Number(m[1]) + Number(m[2]) + Number(m[3])) / 3;
}

test.describe("Header (F10)", () => {
  test("the landing header reads IEM Mixer, set in capitals", async ({ page }) => {
    await page.goto("/");
    const title = page.locator("header.header h1");
    await expect(title).toHaveText("IEM Mixer");
    // What the band sees on the screen.
    await expect(title).toHaveText("IEM MIXER", { useInnerText: true });
  });

  test("the mixer header shows the version and the build date", async ({ page }) => {
    const api = await (await page.request.get("/api/version")).json();
    await openMixer(page, "member3");
    const header = page.locator(".mixer-header");
    await expect(header.locator("h1")).toHaveText("Member3");
    const block = header.locator(".header-version");
    await expect(block).toBeVisible();
    await expect(block.locator(".header-version-number")).toHaveText(`v${api.version}`);
    const date = block.locator(".header-version-date");
    await expect(date).toBeVisible();
    await expect(date).toHaveText(/^\d{2}\.\d{2}\.\d{4} \d{2}:\d{2}$/);
  });

  test("the status dot shows the connection and stays a small dot", async ({ page }) => {
    await openMixer(page, "member3");
    const dot = page.locator(".mixer-header .status-dot");
    await expect(dot).toHaveClass(/\bconnected\b/);
    await expect(dot).not.toHaveClass(/disconnected/);
    const box = await dot.boundingBox();
    expect(box).not.toBeNull();
    expect(box!.width).toBeGreaterThan(0);
    expect(box!.width).toBeLessThanOrEqual(15);
    expect(box!.height).toBeLessThanOrEqual(15);
  });

  test("the status dot pulses with the meter traffic", async ({ page }) => {
    await openMixer(page, "member3");
    const dot = page.locator(".mixer-header .status-dot");
    // Every applied meter frame flips pulse-a and pulse-b, which restarts the
    // pulse animation: both must show while the engine sends meters.
    const seen = new Set<string>();
    await expect
      .poll(
        async () => {
          const m = /pulse-[ab]/.exec((await dot.getAttribute("class")) ?? "");
          if (m) seen.add(m[0]);
          return seen.size;
        },
        { timeout: 5_000, intervals: [40] },
      )
      .toBe(2);
    const animation = await dot.evaluate((el) => getComputedStyle(el).animationName);
    expect(animation).toMatch(/^dot-pulse-[ab]$/);
  });

  test("the version date is readable (brighter than #555)", async ({ page }) => {
    await openMixer(page, "member3");
    const date = page.locator(".mixer-header .header-version-date");
    await expect(date).toBeVisible();
    const color = await date.evaluate((el) => getComputedStyle(el).color);
    // #555 averages 85, white 255.
    expect(channelAverage(color)).toBeGreaterThan(100);
  });

  test("a lost connection shows the amber Reconnecting banner; the reconnect hides it", async ({
    page,
    baseURL,
  }) => {
    // The page's mixer socket goes through a route: while the "server" is
    // gone, the page's reconnect attempts are closed at once. It stays gone
    // for MAX_WS_FAILURES (3) failed sockets in a row — the drop and two
    // refused retries (the next 2 s tick, then 8 s later), where the page
    // used to give up while its token was still valid — and comes back
    // before the next retry, 15 s later, which also asks whether the token
    // holds. About 30 s from the drop to the reconnect.
    test.setTimeout(90_000);
    let serverGone = false;
    let refused = 0;
    let live: WebSocketRoute | undefined;
    await page.routeWebSocket(/\/ws\/member3\?/, async (ws) => {
      if (serverGone) {
        refused += 1;
        await ws.close({ code: 1000, reason: "server gone (test)" });
        return;
      }
      ws.connectToServer();
      live = ws;
    });
    // The token check after MAX_WS_FAILURES (the page's only GET of its mixer).
    const tokenChecks: number[] = [];
    page.on("request", (r) => {
      if (r.method() === "GET" && new URL(r.url()).pathname === "/api/mixer/member3") tokenChecks.push(Date.now());
    });
    const auth = await openMixer(page, "member3");
    const banner = page.locator(".disconnected-banner");
    const dot = page.locator(".mixer-header .status-dot");
    await expect(dot).toHaveClass(/\bconnected\b/);
    await expect(banner).toHaveCount(0);
    expect(live, "the mixer socket went through the route").toBeDefined();
    const dropped = live!;

    serverGone = true;
    await dropped.close({ code: 1000, reason: "server gone (test)" });
    await expect(dot).toHaveClass(/disconnected/);
    // Debounced: a short drop shows nothing.
    await page.waitForTimeout(1_500);
    await expect(banner).toHaveCount(0);

    await expect(banner).toBeVisible({ timeout: 5_000 });
    await expect(banner).toHaveText("Reconnecting...");
    // Amber (the solo-yellow token), not the old red warning.
    await expect(page.locator(".disconnected-warning")).toHaveCount(0);
    const colors = await banner.evaluate((el) => {
      const probe = document.createElement("span");
      probe.style.color = "var(--solo-yellow)";
      document.body.appendChild(probe);
      const amber = getComputedStyle(probe).color;
      probe.remove();
      return { banner: getComputedStyle(el).color, amber };
    });
    expect(colors.banner).toBe(colors.amber);
    const green = Number(/rgba?\(\d+,\s*(\d+)/.exec(colors.banner)?.[1]);
    expect(green, "amber has a strong green channel; the mute red has not").toBeGreaterThan(150);

    // Two refused retries: three failed sockets in a row with the drop.
    await expect.poll(() => refused, { timeout: 20_000 }).toBe(2);
    const backAt = Date.now();
    serverGone = false;
    await expect(banner).toHaveCount(0, { timeout: 25_000 });
    await expect(dot).toHaveClass(/\bconnected\b/);
    expect(live, "the page opened a new socket").not.toBe(dropped);
    expect(
      tokenChecks.filter((at) => at >= backAt).length,
      "the retry after three failed sockets asked whether the token holds",
    ).toBeGreaterThan(0);
    // The token still holds: the page stays, it is not sent to the login.
    await expect(page).toHaveURL(/\/member3$/);

    // A mixer change made elsewhere arrives through the new socket.
    const other = await PageSocket.open(baseURL, "member3", auth.token);
    const state = await other.next("State");
    const mic3 = state.channels.find((c: { id: string }) => c.id === "mic3");
    expect(mic3, "member3's own channel").toBeDefined();
    const own = strip(page, "mic3");
    const showsMuted = async (muted: boolean) => {
      if (muted) await expect(own).toHaveClass(/\bmuted\b/);
      else await expect(own).not.toHaveClass(/\bmuted\b/);
    };
    await showsMuted(mic3.muted);
    other.send({ cmd: "SetMute", id: "mic3", muted: !mic3.muted });
    await other.applied();
    await showsMuted(!mic3.muted);
    // Put it back (member3's mix is shared by the mixer-page specs).
    other.send({ cmd: "SetMute", id: "mic3", muted: mic3.muted });
    await other.applied();
    await showsMuted(mic3.muted);
    other.close();
  });

  test("a connected mixer never shows the Reconnecting banner", async ({ page }) => {
    await openMixer(page, "member3");
    const banner = page.locator(".disconnected-banner");
    await expect(page.locator(".mixer-header .status-dot")).toHaveClass(/\bconnected\b/);
    await expect(banner).toHaveCount(0);
    // Longer than the 3 s debounce, with meters flowing all the time.
    await page.waitForTimeout(5_000);
    await expect(banner).toHaveCount(0);
    await expect(page.locator("#iem-panic-overlay")).toHaveCount(0);
  });
});
