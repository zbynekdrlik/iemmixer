import { readFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test, expect } from "./support/fixtures";
import { TALK_INIT, TALK_TONE } from "./live/support/audio";
import { dbOf, median, spread } from "./live/support/series";
import { toneWav } from "./support/wav";

// The live talkback spec's settling (S7, #10), checked in the mock E2E job:
// the page's own talkback.js and its worklet capture Chromium's fake
// microphone playing the live spec's tone, with the page's constraints (AGC
// and noise suppression), and TALK_INIT measures the frames the page hands
// its encoder. From `TALK_TONE.settleMs` after the first frame the level must
// hold within the live spec's 0.2 dB for its 3 s window, or that spec would
// read Chromium's ramp as an unsteady input. Served from a local server of
// this test (a secure context, as getUserMedia and AudioWorklet need); the
// talkback socket is held by the test, no server is involved.

const MIC_WAV = toneWav(join(tmpdir(), "iemmixer-e2e-talkback-capture.wav"), TALK_TONE.hz, TALK_TONE.amplitude);

// Launch options are the worker's: top level, never in a describe.
test.use({
  launchOptions: {
    args: [
      "--use-fake-ui-for-media-stream",
      "--use-fake-device-for-media-stream",
      `--use-file-for-fake-audio-capture=${MIC_WAV}`,
    ],
  },
  permissions: ["microphone"],
});

const UI = resolve(__dirname, "../../crates/iem-ui");
const SCRIPTS = new Set(["/talkback.js", "/talkback-worklet.js"]);

/** The live talkback spec's window and steadiness. */
const WINDOW_MS = 3_000;
const STEADY_DB = 0.2;

let server: Server;
let origin = "";

test.beforeAll(async () => {
  server = createServer((req, res) => {
    const path = new URL(req.url ?? "/", "http://localhost").pathname;
    if (SCRIPTS.has(path)) {
      res.writeHead(200, { "content-type": "text/javascript" });
      res.end(readFileSync(join(UI, path.slice(1)), "utf8"));
      return;
    }
    res.writeHead(200, { "content-type": "text/html" });
    res.end("<!doctype html><title>talkback capture</title>");
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  origin = `http://localhost:${(server.address() as AddressInfo).port}`;
});

test.afterAll(async () => {
  await new Promise((done) => server.close(done));
});

test("the fake microphone's tone reaches the talkback encoder steady within 0.2 dB from the live spec's settling on", async ({
  page,
}) => {
  test.setTimeout(60_000);
  // The talkback socket: held open by the test, its frames counted.
  let sent = 0;
  await page.routeWebSocket(/\/ws\/talkback/, (ws) => {
    ws.onMessage(() => {
      sent += 1;
    });
  });
  await page.addInitScript(TALK_INIT);
  await page.goto(`${origin}/`);
  // The page's own module, started as its Talk button starts it.
  await page.evaluate(
    async ({ script, url }) => {
      const talkback = (await import(script)) as { startTalkback: (url: string) => Promise<void> };
      await talkback.startTalkback(url);
    },
    { script: "/talkback.js", url: `${origin.replace(/^http/, "ws")}/ws/talkback?talk=capture` },
  );

  const read = () =>
    page.evaluate(() => {
      const w = window as unknown as { __live_talk_in: { peak: number; at: number }[]; __live_talk_error?: string };
      return { frames: w.__live_talk_in.slice(), error: w.__live_talk_error ?? null };
    });
  await expect.poll(async () => (await read()).frames.length, { message: "the encoder gets frames" }).toBeGreaterThan(0);
  const first = (await read()).frames[0].at;
  await expect
    .poll(async () => (await read()).frames.some((f) => f.at >= first + TALK_TONE.settleMs + WINDOW_MS), {
      message: "frames past the window",
      timeout: 15_000,
    })
    .toBe(true);
  const { frames, error } = await read();
  expect(error, "every encoder frame was read").toBeNull();

  const from = first + TALK_TONE.settleMs;
  const window = frames.filter((f) => f.at >= from && f.at < from + WINDOW_MS).map((f) => dbOf(f.peak));
  // 20 ms frames: 150 in 3 s.
  expect(window.length).toBeGreaterThanOrEqual(140);
  expect(median(window), "the tone reaches the encoder").toBeGreaterThan(-60);
  expect(spread(window), "the encoder's input over the window (dB)").toBeLessThanOrEqual(STEADY_DB);
  expect(sent, "the page sent Opus frames").toBeGreaterThan(100);
});
