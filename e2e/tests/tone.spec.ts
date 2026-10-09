import { test, expect } from "./support/fixtures";
import { toneOf } from "./live/support/tone";
import { celt20msStereo } from "./live/support/burst";
import { namesBuild } from "./live/support/env";
import { ANALYSER_INIT, readTone } from "./live/support/audio";
import { LIVE_NUMBER_KEYS } from "./live/support/live";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { resolve } from "node:path";

// The live specs' pure estimators and their in-page analyser (S7, #10), run
// in the mock E2E job: the live specs themselves run only from the ops live
// run, against the real PC. Every input is synthetic with a known answer; only
// the analyser test uses a page (its own, served by a route, no server).

const RATE = 48_000;
/** The analyser's window in the live specs (AnalyserNode fftSize 32768). */
const WINDOW = 32_768;

/** `n` samples of a sine of `hz` at `dbfs` (peak) starting at `phase` rad. */
function sine(hz: number, dbfs: number, phase = 0, n = WINDOW): Float32Array {
  const amp = Math.pow(10, dbfs / 20);
  const x = new Float32Array(n);
  for (let i = 0; i < n; i++) x[i] = amp * Math.sin((2 * Math.PI * hz * i) / RATE + phase);
  return x;
}

/** An Opus TOC byte (RFC 6716 §3.1): config (5 bits), s (stereo), c (frame count code). */
function toc(config: number, stereo: boolean, code: number): number {
  return (config << 3) | (stereo ? 0b100 : 0) | code;
}

/** A packet of `length` bytes whose first byte is `first`. */
function packet(first: number, length: number): Uint8Array {
  const p = new Uint8Array(length).fill(0x5a);
  if (length > 0) p[0] = first;
  return p;
}

test("the tone estimate reads 1 kHz at -20 dBFS within 0.01 Hz and 0.01 dB", () => {
  for (const phase of [0, 0.7, 2.1, 4.0]) {
    const t = toneOf(sine(1000, -20, phase), RATE);
    expect(Math.abs(t.hz - 1000), `phase ${phase}: ${t.hz} Hz`).toBeLessThanOrEqual(0.01);
    expect(Math.abs(t.dbfs + 20), `phase ${phase}: ${t.dbfs} dBFS`).toBeLessThanOrEqual(0.01);
    expect(t.gap).toBe(false);
  }
  // The level is measured, not assumed: 12.5 dB lower reads 12.5 dB lower.
  const quieter = toneOf(sine(1000, -32.5, 1.3), RATE);
  expect(Math.abs(quieter.dbfs + 32.5)).toBeLessThanOrEqual(0.01);
  expect(Math.abs(quieter.hz - 1000)).toBeLessThanOrEqual(0.01);
  // The rate is the caller's: the same samples read at 44.1 kHz are a lower tone.
  expect(Math.abs(toneOf(sine(1000, -20), 44_100).hz - (1000 * 44_100) / RATE)).toBeLessThanOrEqual(0.01);
});

test("the tone estimate tells 1001 Hz from 1000 Hz", () => {
  const at1000 = toneOf(sine(1000, -20, 0.4), RATE).hz;
  const at1001 = toneOf(sine(1001, -20, 0.4), RATE).hz;
  const at999 = toneOf(sine(999, -20, 0.4), RATE).hz;
  expect(Math.abs(at1001 - 1001), `${at1001} Hz`).toBeLessThanOrEqual(0.01);
  expect(Math.abs(at999 - 999), `${at999} Hz`).toBeLessThanOrEqual(0.01);
  expect(Math.abs(at1001 - at1000 - 1)).toBeLessThanOrEqual(0.02);
  expect(Math.abs(at1000 - at999 - 1)).toBeLessThanOrEqual(0.02);
});

test("a dropout inside the window is a gap", () => {
  expect(toneOf(sine(1000, -20), RATE).gap).toBe(false);

  // One lost 20 ms frame in the middle of the window.
  const frame = sine(1000, -20);
  frame.fill(0, 16_000, 16_960);
  expect(toneOf(frame, RATE).gap).toBe(true);

  // The bound is 48 samples under 1e-5 in a row. At phase 0 sample 12 is the
  // sine's peak and so is sample 60 (one 48-sample period later): zeroing
  // 13..59 leaves a run of exactly 47, zeroing 13..60 a run of 48.
  const run47 = sine(1000, -20);
  run47.fill(0, 13, 60);
  expect(Math.abs(run47[12]) > 0.09 && Math.abs(run47[60]) > 0.09).toBe(true);
  expect(toneOf(run47, RATE).gap).toBe(false);
  const run48 = sine(1000, -20);
  run48.fill(0, 13, 61);
  expect(Math.abs(run48[61])).toBeGreaterThan(0.09);
  expect(toneOf(run48, RATE).gap).toBe(true);

  // Under 1e-5 counts as silent, at 1e-5 does not (in doubles: a float32
  // cannot hold 1e-5 exactly).
  const quiet = Float64Array.from(sine(1000, -20));
  quiet.fill(0.99e-5, 1_000, 1_048);
  expect(toneOf(quiet, RATE).gap).toBe(true);
  const audible = Float64Array.from(sine(1000, -20));
  audible.fill(1e-5, 1_000, 1_048);
  expect(toneOf(audible, RATE).gap).toBe(false);
});

test("silence reads -150 dBFS", () => {
  const silence = toneOf(new Float32Array(WINDOW), RATE);
  expect(silence).toEqual({ hz: 0, dbfs: -150, gap: true });
  // No period to count: one rising crossing reads 0 Hz, never NaN.
  const step = new Float32Array(WINDOW).fill(0.1);
  step.fill(-0.1, 0, WINDOW / 2);
  expect(toneOf(step, RATE).hz).toBe(0);
  // A tone below the floor reads the floor, never less.
  expect(toneOf(sine(1000, -170), RATE).dbfs).toBe(-150);
  // Just above the floor it is measured.
  expect(Math.abs(toneOf(sine(1000, -140), RATE).dbfs + 140)).toBeLessThanOrEqual(0.01);
});

test("a CELT 20 ms stereo packet passes the TOC check and SILK or 10 ms or mono does not", () => {
  // CELT-only configs are 16..31; config % 4 is the frame size: 2.5, 5, 10, 20 ms.
  for (const config of [19, 23, 27, 31]) {
    expect(celt20msStereo(packet(toc(config, true, 0), 160)), `config ${config}`).toBe(true);
  }
  // One frame of 1 to 1275 bytes after the TOC byte (code 0).
  expect(celt20msStereo(packet(toc(31, true, 0), 2))).toBe(true);
  expect(celt20msStereo(packet(toc(31, true, 0), 1276))).toBe(true);
  expect(celt20msStereo(packet(toc(31, true, 0), 1))).toBe(false);
  expect(celt20msStereo(packet(toc(31, true, 0), 1277))).toBe(false);
  expect(celt20msStereo(new Uint8Array(0))).toBe(false);

  // SILK (0..11, 20 ms at config % 4 == 1; config 3 is SILK 60 ms) and hybrid
  // (12..15; 15 is hybrid FB 20 ms, config % 4 == 3) are not CELT.
  for (const config of [1, 3, 5, 9, 11, 13, 15]) {
    expect(celt20msStereo(packet(toc(config, true, 0), 160)), `config ${config}`).toBe(false);
  }
  // CELT 2.5, 5 and 10 ms.
  for (const config of [28, 29, 30, 16, 17, 18]) {
    expect(celt20msStereo(packet(toc(config, true, 0), 160)), `config ${config}`).toBe(false);
  }
  // Mono.
  expect(celt20msStereo(packet(toc(31, false, 0), 160))).toBe(false);
  // Two or more frames in the packet (codes 1, 2, 3).
  for (const code of [1, 2, 3]) {
    expect(celt20msStereo(packet(toc(31, true, code), 160)), `code ${code}`).toBe(false);
  }
});

test("the build check takes a git_hash of 7 or more lowercase hex digits that starts the run's SHA", () => {
  // Synthetic: no build has this SHA.
  const sha = "0123456789abcdef0123456789abcdef01234567";
  expect(namesBuild("0123456", sha)).toBe(true);
  expect(namesBuild(sha, sha)).toBe(true);
  expect(namesBuild("012345", sha), "6 digits").toBe(false);
  expect(namesBuild("0123457", sha), "not a prefix").toBe(false);
  expect(namesBuild("123456789a", sha), "inside the SHA, not at its start").toBe(false);
  expect(namesBuild("0123456789ABCDEF", "0123456789ABCDEF" + sha.slice(16)), "upper case").toBe(false);
  expect(namesBuild(`${sha}8`, sha), "longer than the SHA").toBe(false);
  expect(namesBuild("0123456 ", sha), "a trailing space").toBe(false);
  expect(namesBuild("unknown", sha)).toBe(false);
  expect(namesBuild("", sha)).toBe(false);
  expect(namesBuild(123456789, sha)).toBe(false);
  expect(namesBuild(undefined, sha)).toBe(false);
  // Not a string, though its text would pass: JSON never coerces into a build.
  expect(namesBuild(["0123456"], sha)).toBe(false);
  expect(namesBuild(1234567, `1234567${sha.slice(7)}`)).toBe(false);
});

test("the analyser init script reads what a real AudioContext plays: 1 kHz at -20 dBFS", async ({ page }) => {
  // The live specs' in-page path, run here in the same Chromium: the init
  // script as Playwright transpiles and injects it, the tap on the node that
  // feeds the destination, and readTone over the page's analyser.
  await page.route("http://tone.test/**", (route) =>
    route.fulfill({ contentType: "text/html", body: "<!doctype html><title>tone</title>" }),
  );
  await page.addInitScript(ANALYSER_INIT);
  await page.goto("http://tone.test/");
  const state = await page.evaluate(async () => {
    const ctx = new AudioContext({ sampleRate: 48_000 });
    const osc = ctx.createOscillator();
    osc.frequency.value = 1000;
    const gain = ctx.createGain();
    gain.gain.value = 0.1;
    osc.connect(gain);
    gain.connect(ctx.destination);
    osc.start();
    await ctx.resume();
    return ctx.state;
  });
  expect(state).toBe("running");
  // The analyser's window (0.68 s) then holds only the steady tone.
  await page.waitForTimeout(1_500);
  const tone = await readTone(page);
  expect(Math.abs(tone.hz - 1000), `${tone.hz} Hz`).toBeLessThanOrEqual(0.5);
  expect(Math.abs(tone.dbfs + 20), `${tone.dbfs} dBFS`).toBeLessThanOrEqual(0.1);
  expect(tone.gap).toBe(false);
});

/** The player and its limiter worklet, served as the server serves them (a worklet's module never reaches `page.route`). */
const PLAYER_FILES: Record<string, string> = {
  "/audio_player.js": resolve(__dirname, "../../crates/iem-ui/audio_player.js"),
  "/listen-limiter-worklet.js": resolve(__dirname, "../../crates/iem-ui/listen-limiter-worklet.js"),
};

test("three windows read 300 ms apart leave the real player playing: no dropout, the player never ran dry", async ({
  page,
}) => {
  // The live listen probe's measurement (#10, live run 1: window 1 clean,
  // window 2 a dropout) on the real player, fed as the live relay feeds it: a
  // 20 ms Opus frame every 20 ms from this process through
  // `page.routeWebSocket`, at 1 kHz and -20 dBFS. Reading a window must not
  // starve that feed or the page: the player keeps only 80 ms ahead.
  test.setTimeout(60_000);
  const server = createServer((req, res) => {
    const file = PLAYER_FILES[req.url ?? ""];
    if (file) {
      res.writeHead(200, { "Content-Type": "text/javascript" });
      res.end(readFileSync(file));
      return;
    }
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end("<!doctype html><title>player</title>");
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  let frames: Buffer[] = [];
  let feeding = true;
  try {
    await page.routeWebSocket(/\/ws\/audio$/, (ws) => {
      const start = Date.now();
      let sent = 0;
      const tick = () => {
        if (!feeding) return;
        if (frames.length > 0) ws.send(frames[sent % frames.length]);
        sent += 1;
        // Drift-free: frame n leaves at start + 20·n ms.
        setTimeout(tick, Math.max(0, start + sent * 20 - Date.now()));
      };
      tick();
    });
    await page.addInitScript(ANALYSER_INIT);
    await page.goto(`${origin}/`);
    // 2 s of 1 kHz at -20 dBFS in 20 ms Opus frames (20 periods each, so the loop is seamless before coding).
    const encoded = await page.evaluate(async () => {
      const out: string[] = [];
      const encoder = new AudioEncoder({
        output: (chunk) => {
          const bytes = new Uint8Array(chunk.byteLength);
          chunk.copyTo(bytes);
          out.push(btoa(String.fromCharCode(...bytes)));
        },
        error: (e) => {
          throw e;
        },
      });
      encoder.configure({ codec: "opus", sampleRate: 48_000, numberOfChannels: 2, bitrate: 128_000 });
      const n = 960;
      for (let f = 0; f < 100; f++) {
        const data = new Float32Array(2 * n);
        for (let i = 0; i < n; i++) data[i] = data[n + i] = 0.1 * Math.sin((2 * Math.PI * 1000 * (f * n + i)) / 48_000);
        encoder.encode(
          new AudioData({ format: "f32-planar", sampleRate: 48_000, numberOfFrames: n, numberOfChannels: 2, timestamp: f * 20_000, data }),
        );
      }
      await encoder.flush();
      return out;
    });
    // The encoder's first frames hold its start-up: the loop takes the steady ones.
    frames = encoded.slice(10).map((b) => Buffer.from(b, "base64"));
    expect(frames.length).toBeGreaterThanOrEqual(80);
    await page.evaluate(async () => {
      const player = await import("/audio_player.js");
      player.initAudioPlayer();
      const ws = new WebSocket(`ws://${location.host}/ws/audio`);
      ws.binaryType = "arraybuffer";
      ws.onmessage = (m) => player.feedOpusFrame(m.data);
    });
    const stats = () =>
      page.evaluate(() => (window as unknown as { __iem_stream_stats: () => { dropouts: number } }).__iem_stream_stats());
    await expect
      .poll(() => page.evaluate(() => (window as unknown as { __iem_audio_level: () => number }).__iem_audio_level()))
      .toBeGreaterThan(-100);
    // As the live spec: a second of settling, then three windows 300 ms apart.
    await page.waitForTimeout(1_000);
    for (let k = 1; k <= 3; k++) {
      if (k > 1) await page.waitForTimeout(300);
      const tone = await readTone(page);
      expect(tone.gap, `window ${k} has no dropout`).toBe(false);
      expect(Math.abs(tone.dbfs + 20), `window ${k}: ${tone.dbfs} dBFS`).toBeLessThanOrEqual(1);
    }
    expect((await stats()).dropouts, "the player never ran dry").toBe(0);
  } finally {
    feeding = false;
    server.close();
  }
});

test("the live specs' number keys are the live verdict's NUMBER_KEYS", () => {
  // The verdict keeps a number only under one of its keys: a key the specs
  // spell differently would vanish from the report without an error.
  const verdict = readFileSync(resolve(__dirname, "../../scripts/iem-pc/live_verdict.py"), "utf8");
  const tuple = /^NUMBER_KEYS = \(([^)]*)\)/m.exec(verdict);
  expect(tuple, "NUMBER_KEYS in live_verdict.py").not.toBeNull();
  const keys = [...(tuple as RegExpExecArray)[1].matchAll(/"([a-z_]+)"/g)].map((m) => m[1]);
  expect(keys.length).toBeGreaterThan(0);
  expect([...LIVE_NUMBER_KEYS]).toEqual(keys);
});
