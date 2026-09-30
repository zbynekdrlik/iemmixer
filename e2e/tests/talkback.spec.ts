import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test, expect } from "./support/fixtures";
import { openMixer, tab } from "./support/session";
import { PageSocket } from "./support/wire";

// The engineer's push-to-talk (F18; X5, X6) with Chromium's fake microphone
// playing a 1 kHz tone: the page encodes Opus onto /ws/talkback, the server
// plays it into the engine's talkback input (eng_mic on the test site), and
// it stops when Talk is released. On NullRt the card input of eng_mic also
// carries the −20 dBFS test sine, so the test takes the card away (eng_mic's
// trim off, which comes before the talkback in the input) and puts it back
// after: what eng_mic's meter then shows is the talkback alone.

/** A loop-safe tone (a whole number of cycles) as 16-bit mono PCM WAV. */
function toneWav(file: string, hz: number, amplitude: number, rate = 48_000): string {
  const n = rate;
  const data = Buffer.alloc(n * 2);
  for (let i = 0; i < n; i++) {
    data.writeInt16LE(Math.round(amplitude * 32767 * Math.sin((2 * Math.PI * hz * i) / rate)), i * 2);
  }
  const header = Buffer.alloc(44);
  header.write("RIFF", 0);
  header.writeUInt32LE(36 + data.length, 4);
  header.write("WAVE", 8);
  header.write("fmt ", 12);
  header.writeUInt32LE(16, 16);
  header.writeUInt16LE(1, 20); // PCM
  header.writeUInt16LE(1, 22); // mono
  header.writeUInt32LE(rate, 24);
  header.writeUInt32LE(rate * 2, 28);
  header.writeUInt16LE(2, 32);
  header.writeUInt16LE(16, 34);
  header.write("data", 36);
  header.writeUInt32LE(data.length, 40);
  writeFileSync(file, Buffer.concat([header, data]));
  return file;
}

const MIC_WAV = toneWav(join(tmpdir(), "iemmixer-e2e-talkback-1k.wav"), 1000, 0.9);

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

/** −60 dBFS, linear: below it a meter reads as silence. */
const SILENCE = 0.001;

type TalkDiagnostics = {
  recv_vst_addr: string | null;
  packets_in: number;
  packets_out: number;
  seq_gaps: number;
  buffer_overflows: number;
  bitrate_kbps: number;
};

test.describe("Talkback (F18)", () => {
  // The engineer page subscribes to Web Push; Playwright's browser contexts
  // are incognito, where Chrome has no Push API: its error and the page's
  // `[push] …` warnings are expected here.
  test.use({
    allowedConsole: [[/^\[push\] /, /does not support the Push API in incognito mode/], { scope: "test" }],
  });

  test("held Talk reaches the talkback input continuously and stops within 600 ms of release", async ({
    page,
    baseURL,
  }) => {
    test.setTimeout(60_000);
    const auth = await openMixer(page, "engineer", { engineer: true });
    const talk = page.locator(".toolbar-btn-talk");
    await expect(talk).toHaveText("🎤 Talk");
    await expect(talk).not.toHaveClass(/unsupported|mic-blocked|in-use/);
    // A tap first: the page's vibration and audio need a user gesture.
    await tab(page, "Mics");

    const diagnostics = async (): Promise<TalkDiagnostics> => {
      const resp = await page.request.get("/api/talkback/diagnostics", {
        headers: { Authorization: `Bearer ${auth.token}` },
      });
      expect(resp.status()).toBe(200);
      return resp.json();
    };

    // The runner watches the engineer's page for eng_mic's meter and sets
    // the input from the console.
    const watch = await PageSocket.open(baseURL, "engineer", auth.token);
    const before = (await watch.console()).inputs.find((i: { id: string }) => i.id === "eng_mic");
    expect(before, "eng_mic is on the console").toBeTruthy();
    const talkFramesSent: number[] = [];
    page.on("websocket", (ws) => {
      if (!ws.url().includes("/ws/talkback")) return;
      ws.on("framesent", (f) => {
        if (typeof f.payload !== "string") talkFramesSent.push(f.payload.length);
      });
    });
    try {
      watch.send({ cmd: "SetInput", input: "eng_mic", trim_db: -150, processing: true });
      await watch.applied();
      // The card's sine is gone: eng_mic is silent before Talk.
      await expect
        .poll(() => {
          const recent = watch.peaks("eng_mic", Date.now() - 300);
          return recent.length > 0 && recent.every((p) => p < SILENCE);
        }, { timeout: 5_000 })
        .toBe(true);
      const start = await diagnostics();

      // A real press: the button needs pointerdown with a pointer id.
      await talk.hover();
      await page.mouse.down();
      await expect(talk).toHaveClass(/live/, { timeout: 5_000 });
      // Socket, microphone and encoder settle before the samples.
      await page.waitForTimeout(2_500);

      // 50 meter frames (100 ms each: 5 s) while Talk is held.
      const from = Date.now();
      await expect.poll(() => watch.peaks("eng_mic", from).length, { timeout: 10_000 }).toBeGreaterThanOrEqual(50);
      const samples = watch.peaks("eng_mic", from).slice(0, 50);

      await page.mouse.up();
      await expect(talk).toHaveText("🎤 Talk");
      await page.waitForTimeout(600);
      const released = await diagnostics();
      await page.waitForTimeout(300);
      const later = await diagnostics();

      // Signal present: at least 40 of 50 above −60 dB.
      const heard = samples.filter((p) => p >= SILENCE).length;
      expect(heard, `samples ${JSON.stringify(samples)}`).toBeGreaterThanOrEqual(40);
      // No hang: never 5 silent frames (500 ms) in a row.
      let run = 0;
      let worst = 0;
      for (const p of samples) {
        run = p < SILENCE ? run + 1 : 0;
        worst = Math.max(worst, run);
      }
      expect(worst, `samples ${JSON.stringify(samples)}`).toBeLessThan(5);

      // Clean release: nothing more goes to the engine 600 ms after it.
      expect(later.packets_out - released.packets_out).toBe(0);
      await expect
        .poll(() => {
          const recent = watch.peaks("eng_mic", Date.now() - 300);
          return recent.length > 0 && recent.every((p) => p < SILENCE);
        }, { timeout: 3_000 })
        .toBe(true);

      // The page sent Opus frames, the server took them in and played them out.
      expect(talkFramesSent.length).toBeGreaterThan(200);
      expect(later.packets_in - start.packets_in).toBeGreaterThan(200);
      expect(later.packets_out - start.packets_out).toBeGreaterThan(200);
      expect(later.seq_gaps).toBe(0);
      expect(later.buffer_overflows).toBe(0);
      expect(later.bitrate_kbps).toBe(96);
      expect(later.recv_vst_addr).toBeTruthy();
      expect(later.recv_vst_addr).not.toBe("none");
    } finally {
      watch.send({
        cmd: "SetInput",
        input: "eng_mic",
        trim_db: before.trim_db,
        processing: before.processing,
      });
      await watch.applied();
      watch.close();
    }
  });
});
