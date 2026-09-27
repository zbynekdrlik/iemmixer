import { test, expect, Page } from "./support/fixtures";
import { login, openMixer } from "./support/session";
import { soundInput } from "./support/wire";

// The engineer's Listen button (F15, F17): the toolbar button opens
// /ws/audio, the page decodes the Opus frames with WebCodecs and plays them
// (audio_player.js). The engine's taps carry its −20 dBFS test sine only
// where a level lets it through, so the tests that need a signal raise hand2
// in the engineer's mix for their duration (`soundInput`). A member's mix is
// heard on member8's page (no other spec's mix).

type StreamStats = { dropouts: number; frames: number; bufferMs: number; quality: string };

function listenButton(page: Page) {
  return page.locator(".toolbar-btn-listen");
}

/** Stops listening from the toolbar and waits for the idle button. */
async function stopListening(page: Page): Promise<void> {
  const listen = listenButton(page);
  await listen.click();
  await expect(listen).not.toHaveClass(/listening/);
  await expect(listen).toHaveText("🔊 Listen");
}

test.describe("Listen button (F17)", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  test("on a member's page Listen sends ListenStart with that member's id", async ({ page }) => {
    const sent: string[] = [];
    const received: string[] = [];
    page.on("websocket", (ws) => {
      if (!ws.url().includes("/ws/audio")) return;
      ws.on("framesent", (f) => {
        if (typeof f.payload === "string") sent.push(f.payload);
      });
      ws.on("framereceived", (f) => {
        if (typeof f.payload === "string") received.push(f.payload);
      });
    });
    await openMixer(page, "engineer", { engineer: true, path: "member8" });
    const listen = listenButton(page);
    await expect(listen).toHaveText("🔊 Listen");
    await listen.click();
    await expect(listen).toHaveClass(/listening/, { timeout: 8_000 });

    const commands = () => sent.map((p) => JSON.parse(p));
    await expect
      .poll(() => commands().filter((m) => m.cmd === "ListenStart"))
      .toEqual([{ cmd: "ListenStart", member_id: "member8" }]);
    await expect
      .poll(() => received.map((p) => JSON.parse(p)))
      .toContainEqual({ event: "AudioStatus", data: { status: "listening", target: "member8" } });
    // The listening button names whose mix it is (then its stream stats).
    await expect(listen).toContainText("🔊 Member8");

    await stopListening(page);
    await expect.poll(commands).toContainEqual({ cmd: "ListenStop" });
  });

  test("Listen reaches the listening state with a clean console", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    const listen = listenButton(page);
    await listen.click();
    await expect(listen).toHaveClass(/listening/, { timeout: 8_000 });
    await expect(listen).not.toHaveClass(/no-source/);
    await stopListening(page);
  });

  test("Listen on the engineer's page plays the mix within 3 s", async ({ page, baseURL }) => {
    const auth = await openMixer(page, "engineer", { engineer: true });
    const restore = await soundInput(baseURL, auth.token, "engineer", "hand2");
    try {
      await listenButton(page).click();
      // The level of the decoded audio the page plays (−150 dB: silence).
      await expect
        .poll(() => page.evaluate(() => (window as unknown as { __iem_audio_level: () => number }).__iem_audio_level()), {
          timeout: 3_000,
          intervals: [200],
        })
        .toBeGreaterThan(-100);
      await stopListening(page);
    } finally {
      await restore();
    }
  });

  test("a stopped Listen stays stopped: no reconnect", async ({ page }) => {
    let audioSockets = 0;
    page.on("websocket", (ws) => {
      if (ws.url().includes("/ws/audio")) audioSockets += 1;
    });
    await openMixer(page, "engineer", { engineer: true, path: "member8" });
    const listen = listenButton(page);
    await listen.click();
    await expect(listen).toHaveClass(/listening/, { timeout: 8_000 });
    await stopListening(page);
    const opened = audioSockets;
    expect(opened).toBe(1);

    await page.waitForTimeout(5_000);
    await expect(listen).toHaveText("🔊 Listen");
    await expect(listen).not.toHaveClass(/listening|reconnecting/);
    expect(audioSockets, "no new audio socket after the stop").toBe(opened);
  });

  test("Listen goes from idle to listening and back with no decoder error", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    const listen = listenButton(page);
    await expect(listen).toBeVisible();
    // WebCodecs is there: the button is not the Unsupported one.
    await expect(listen).toHaveText("🔊 Listen");
    await expect(listen).toBeEnabled();

    await listen.click();
    await expect(listen).toHaveClass(/listening|no-source/, { timeout: 10_000 });
    await expect(listen).not.toContainText("No Source");
    await expect(listen).toHaveClass(/listening/);
    // Frames keep coming and decoding.
    await page.waitForTimeout(2_000);
    const decoderError = await page.evaluate(() =>
      (window as unknown as { __iem_audio_error: () => string | null }).__iem_audio_error(),
    );
    expect(decoderError).toBeNull();
    await stopListening(page);
  });
});

test.describe("Listen stream quality (F17)", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  const stats = (page: Page): Promise<StreamStats> =>
    page.evaluate(() => (window as unknown as { __iem_stream_stats: () => StreamStats }).__iem_stream_stats());

  test("the stream stats are zero and good while idle", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    expect(await stats(page)).toEqual({ dropouts: 0, frames: 0, bufferMs: 0, quality: "good" });
  });

  test("the stream stats show only while listening", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    const shown = page.getByTestId("stream-stats");
    await expect(shown).toHaveCount(0);

    await listenButton(page).click();
    await expect(listenButton(page)).toHaveClass(/listening/, { timeout: 8_000 });
    await expect(shown).toBeVisible();
    await expect(shown).toHaveText(/^\d+ drops \| buf \d+ms$/);

    await stopListening(page);
    await expect(shown).toHaveCount(0);
  });

  test("the jitter buffer is 0 ms while idle and at least 80 ms while listening", async ({ page }) => {
    await openMixer(page, "engineer", { engineer: true });
    expect((await stats(page)).bufferMs).toBe(0);

    await listenButton(page).click();
    await expect(listenButton(page)).toHaveClass(/listening/, { timeout: 8_000 });
    await expect.poll(async () => (await stats(page)).frames).toBeGreaterThan(0);
    expect((await stats(page)).bufferMs).toBeGreaterThanOrEqual(80);

    await stopListening(page);
    expect(await stats(page)).toEqual({ dropouts: 0, frames: 0, bufferMs: 0, quality: "good" });
  });

  test("a gap in the frames counts a dropout and grows the buffer", async ({ page, baseURL }) => {
    await page.goto("/");
    const auth = await login(page, "engineer", true);
    const restore = await soundInput(baseURL, auth.token, "engineer", "hand2");
    try {
      // A key press first (a user gesture that navigates nowhere): the
      // player's AudioContext starts with a user gesture.
      await page.keyboard.press("a");
      const result = await page.evaluate(async (token) => {
        const player = await import("/audio_player.js");
        player.initAudioPlayer();
        const scheme = location.protocol === "https:" ? "wss:" : "ws:";
        const ws = new WebSocket(`${scheme}//${location.host}/ws/audio?token=${token}`);
        ws.binaryType = "arraybuffer";
        let feeding = true;
        ws.onmessage = (e) => {
          if (feeding && e.data instanceof ArrayBuffer) player.feedOpusFrame(e.data);
        };
        await new Promise<void>((resolve, reject) => {
          ws.onopen = () => resolve();
          ws.onerror = () => reject(new Error("the audio socket did not open"));
        });
        ws.send(JSON.stringify({ cmd: "ListenStart", member_id: "engineer" }));
        const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
        // Two seconds of frames as they come, then 400 ms of none (a network
        // gap: the 80 ms buffer runs dry), then frames again.
        await sleep(2_000);
        const before = player.getStreamStats();
        const level = player.getAudioLevel();
        feeding = false;
        await sleep(400);
        feeding = true;
        await sleep(1_000);
        const after = player.getStreamStats();
        ws.send(JSON.stringify({ cmd: "ListenStop" }));
        ws.close();
        player.stopAudioPlayer();
        return { before, after, level };
      }, auth.token);
      // The frames were decoded and played before the gap.
      expect(result.level).toBeGreaterThan(-100);
      expect(result.before.frames).toBeGreaterThan(50);
      expect(result.after.frames).toBeGreaterThan(result.before.frames);
      expect(result.after.dropouts).toBeGreaterThan(result.before.dropouts);
      expect(result.after.bufferMs).toBeGreaterThanOrEqual(Math.min(result.before.bufferMs + 40, 500));
    } finally {
      await restore();
    }
  });
});
