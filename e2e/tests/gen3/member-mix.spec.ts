import type { APIRequestContext, Locator } from "@playwright/test";
import { test, expect, type Page } from "../live/support/live";
import { LiveMixer } from "../live/support/desk";
import { allowPlainHttp, apiGet, box, expectBuild, openLive, openStart } from "../live/support/env";

// The Gen 3 box's browser step (#72 phase C, plan Task 8): the box's own
// server and UI, plain HTTP on its LAN (plan choice 4), run one test at a
// time by `iempc gen3-mix` (scripts/iem-pc/iempc_gen3.py) between its bursts,
// and by hand in the plan's Live 1 and Live 2. The run's values come in the
// live run's variables (`../live/support/env.ts`): LIVE_BASE_URL (the
// private GEN3_UI_URL), LIVE_SHA (the box's build), LIVE_TOKENS (an engineer
// and a member token minted on the box, removed after the run), LIVE_MEMBER
// (a person of the test roster) and LIVE_TEST_INPUT (an input of the test
// site's [engine] table). Nothing logs in; no PIN. The member's page goes
// through the relay, opened after the build check; the runner's own socket on
// it (`LiveMixer`) reads back what the page's changes made of the engine's
// state. Not the burst desk: the box's engine feeds no band member. Errors
// name the variables, never a value (P6). Public CI only lists these tests
// (the e2e job's `--list` step).

allowPlainHttp();

/** A version line as the UI shows it (`iem_core::version_label`, after its `v`). */
const VERSION = /^\d+\.\d+\.\d+(-dev\.\d+)?$/;
/** A fader's label at its floor (−60 dB, the engine's off). */
const OFF = "-∞dB";
/** The category tabs a person's page lays inputs on (heard mixes are on "Mixes"). */
const TABS = ["Mics", "Stems", "Tech"] as const;
/** A UI step waits at most this long. */
const UI_MS = 8_000;
/** The runner's socket reads the page's changes back within this long. */
const ENGINE_MS = 10_000;

/** A strip's fader track. */
const track = (s: Locator): Locator => s.locator(".fader-track").first();

/** The dB label of a strip ("-∞dB", "-6.0dB", "+0.0dB"). */
async function dbText(s: Locator): Promise<string> {
  return ((await s.locator(".db-display").first().textContent()) ?? "").trim();
}

/** The strip of input `id` on the page: on Main, else on the category tab that holds it. */
async function stripOf(page: Page, id: string): Promise<Locator> {
  const s = page.locator(`.channel[data-channel="${id}"]`).first();
  const shown = async (): Promise<boolean> =>
    s.waitFor({ state: "visible", timeout: 3_000 }).then(
      () => true,
      () => false,
    );
  if (await shown()) return s;
  for (const label of TABS) {
    const tab = page.locator(".category-tab", { hasText: label }).first();
    if ((await tab.count()) === 0) continue;
    await tab.click();
    if (await shown()) return s;
  }
  throw new Error("LIVE_TEST_INPUT is no channel of LIVE_MEMBER's page");
}

/** A double-click animates a fader to 0 dB (F5); then the 300 ms guard after the animation. */
async function toZeroDb(page: Page, s: Locator): Promise<void> {
  await track(s).dblclick();
  await expect.poll(() => dbText(s), { timeout: UI_MS, message: "the fader rests at 0 dB" }).toBe("+0.0dB");
  await expect(track(s)).not.toHaveClass(/animating/, { timeout: UI_MS });
  await page.waitForTimeout(400);
}

/**
 * Drags a fader to its floor as a finger does (a 350 ms hold, then relative
 * moves across the track, right to left), again until it reads off: one drag
 * covers 0.9 of the track's 72 dB.
 */
async function toOff(page: Page, s: Locator): Promise<void> {
  for (let drag = 0; drag < 3 && (await dbText(s)) !== OFF; drag++) {
    await track(s).scrollIntoViewIfNeeded();
    const at = await track(s).boundingBox();
    if (!at) throw new Error("the test input's fader is not laid out");
    const y = at.y + at.height / 2;
    await page.mouse.move(at.x + at.width * 0.95, y);
    await page.mouse.down();
    await page.waitForTimeout(350);
    for (let step = 1; step <= 10; step++) {
      await page.mouse.move(at.x + at.width * (0.95 - 0.09 * step), y);
      await page.waitForTimeout(30);
    }
    await page.mouse.up();
    await page.waitForTimeout(400);
  }
  await expect.poll(() => dbText(s), { timeout: UI_MS, message: "the fader reads off" }).toBe(OFF);
}

/** Unmutes a strip (an input's or IEM VOL) through its mute button when it is muted. */
async function unmute(s: Locator): Promise<void> {
  if (await s.evaluate((el) => el.classList.contains("muted"))) await s.locator(".mute-btn").first().click();
  await expect(s).not.toHaveClass(/\bmuted\b/, { timeout: UI_MS });
}

/** LIVE_MEMBER's page, signed in with the member's token, and the runner's own socket on it. */
async function openMember(page: Page, request: APIRequestContext): Promise<LiveMixer> {
  await openLive(page, "member");
  return LiveMixer.open(request, "member", box().member);
}

/** The mix's soloed channels, once the runner's socket has its solo state. */
async function soloedOf(mixer: LiveMixer): Promise<string[]> {
  let soloed: string[] = [];
  await expect
    .poll(
      () => {
        mixer.check();
        try {
          soloed = mixer.soloed();
          return true;
        } catch {
          return false;
        }
      },
      { timeout: ENGINE_MS, message: "the runner's socket gets the mix's solo state" },
    )
    .toBe(true);
  return soloed;
}

/** The runner's socket reads `id` at 0 dB and unmuted, and IEM VOL at 0 dB and unmuted. */
function opened(mixer: LiveMixer, id: string): boolean {
  mixer.check();
  const channel = mixer.channel(id);
  const volume = mixer.volume();
  return (
    channel !== undefined &&
    channel.level_db === 0 &&
    !channel.muted &&
    volume !== undefined &&
    volume.level_db === 0 &&
    !volume.muted
  );
}

/** The runner's socket reads `id` off (at or below −60 dB). */
function closed(mixer: LiveMixer, id: string): boolean {
  mixer.check();
  const channel = mixer.channel(id);
  return channel !== undefined && channel.level_db <= -60;
}

test.describe("the Gen 3 box (#72 phase C)", () => {
  test("the box serves the deployed build", async ({ page, request, relay }) => {
    // /api/version names LIVE_SHA (its git_hash), and the page the same
    // server serves (the UI is built into it) shows that server's version.
    await expectBuild(request);
    const { status, body } = await apiGet(request, "member", "/api/version");
    expect(status, "GET /api/version").toBe(200);
    const version = (body as { version?: unknown } | null)?.version;
    if (typeof version !== "string" || !VERSION.test(version)) throw new Error("/api/version names no version");
    await openStart(page);
    await expect(page.getByTestId("version").first()).toHaveText(`v${version}`, { timeout: 15_000 });
    relay.check();
  });

  test("the member opens the test input in their mix", async ({ page, request, relay }) => {
    const { testInput } = box();
    const mixer = await openMember(page, request);
    try {
      // A solo masks every other channel, and its mask shows as their mute.
      const soloed = await soloedOf(mixer);
      if (soloed.length > 0) throw new Error("LIVE_MEMBER's mix has a solo: it would mask LIVE_TEST_INPUT");
      // IEM VOL first: it leads Main, and an input on another tab takes its place.
      const volume = page.getByTestId("global-volume-fader");
      await toZeroDb(page, volume);
      await unmute(volume);
      const input = await stripOf(page, testInput);
      await toZeroDb(page, input);
      await unmute(input);
      await mixer.applied();
      await expect
        .poll(() => opened(mixer, testInput), {
          timeout: ENGINE_MS,
          message: "the engine holds LIVE_TEST_INPUT and IEM VOL at 0 dB and unmuted in LIVE_MEMBER's mix",
        })
        .toBe(true);
      relay.check();
    } finally {
      mixer.close();
    }
  });

  test("the member closes the test input in their mix", async ({ page, request, relay }) => {
    const { testInput } = box();
    const mixer = await openMember(page, request);
    try {
      const input = await stripOf(page, testInput);
      await toOff(page, input);
      await mixer.applied();
      await expect
        .poll(() => closed(mixer, testInput), {
          timeout: ENGINE_MS,
          message: "the engine holds LIVE_TEST_INPUT off in LIVE_MEMBER's mix",
        })
        .toBe(true);
      relay.check();
    } finally {
      mixer.close();
    }
  });
});
