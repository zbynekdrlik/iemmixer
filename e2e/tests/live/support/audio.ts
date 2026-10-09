import type { Page } from "@playwright/test";
import { toneOf, type Tone } from "./tone";

// What the page plays and what it sends, read in the page (S7, #10). The init
// scripts run in the page before its own code (`page.addInitScript`): they
// may use no import or module constant, because Playwright's transpiler
// rewrites such references into names that do not exist in the page.

/**
 * `page.addInitScript(ANALYSER_INIT)`: the first node of each AudioContext
 * connected to its destination (the player's limiter, audio_player.js) is
 * also connected to an AnalyserNode of 32768 samples, kept in
 * `window.__live_analyser` (the newest context's).
 */
export const ANALYSER_INIT = (): void => {
  if (typeof AudioNode === "undefined" || typeof AudioDestinationNode === "undefined") return;
  const w = window as unknown as { __live_analyser?: AnalyserNode };
  const connect = AudioNode.prototype.connect;
  const tapped = new WeakSet<BaseAudioContext>();
  AudioNode.prototype.connect = function (this: AudioNode, ...args: unknown[]) {
    const result = (connect as (...a: unknown[]) => unknown).apply(this, args);
    if (args[0] instanceof AudioDestinationNode && !tapped.has(this.context)) {
      tapped.add(this.context);
      const analyser = this.context.createAnalyser();
      analyser.fftSize = 32768;
      (connect as (...a: unknown[]) => unknown).call(this, analyser);
      w.__live_analyser = analyser;
    }
    return result;
  } as typeof AudioNode.prototype.connect;
};

/**
 * The tone the live talkback spec plays into Chromium's fake microphone
 * (`toneWav`), and how long after the encoder's first frame the spec reads
 * it: Chromium's capture processing (talkback.js asks for AGC and noise
 * suppression) ramps the level for about 2.5 s (the last frame more than
 * 0.05 dB off the steady level came 2.49 to 2.52 s after the first in three
 * local runs), then holds it within 0.01 dB. The mock run checks this
 * (`tests/talkback-capture.spec.ts`).
 */
export const TALK_TONE = { hz: 1000, amplitude: 0.5, settleMs: 2_700 } as const;

/**
 * `page.addInitScript(TALK_INIT)`: each frame the page hands its talkback
 * encoder (talkback.js) is also measured: its peak (linear, plane 0) and
 * `performance.now()` go into `window.__live_talk_in`; a frame that cannot
 * be read leaves its error in `window.__live_talk_error`.
 */
export const TALK_INIT = (): void => {
  const w = window as unknown as {
    __live_talk_in: { peak: number; at: number }[];
    __live_talk_error?: string;
  };
  w.__live_talk_in = [];
  if (typeof AudioEncoder === "undefined") return;
  const encode = AudioEncoder.prototype.encode;
  AudioEncoder.prototype.encode = function (this: AudioEncoder, ...args: unknown[]) {
    const frame = args[0] as AudioData;
    try {
      const opts = { planeIndex: 0, format: "f32-planar" as AudioSampleFormat };
      const plane = new Float32Array(frame.allocationSize(opts) / 4);
      frame.copyTo(plane, opts);
      let peak = 0;
      for (let i = 0; i < plane.length; i++) peak = Math.max(peak, Math.abs(plane[i]));
      w.__live_talk_in.push({ peak, at: performance.now() });
    } catch (e) {
      w.__live_talk_error = String(e);
    }
    return (encode as (...a: unknown[]) => unknown).apply(this, args);
  } as typeof AudioEncoder.prototype.encode;
};

/**
 * The tone the player plays now: the analyser's window (`ANALYSER_INIT`) at
 * its context's rate. The window crosses the protocol as one base64 string
 * of its float32 bytes (~15 ms). Returned as an array of 32768 numbers it
 * took 200 to 500 ms, which held back the frames the runner's relay feeds
 * the page: the player, 80 ms ahead, ran dry, and the next window held the
 * gap (#10, live run 1; `tone.spec.ts` runs it on the real player).
 */
export async function readTone(page: Page): Promise<Tone> {
  const read = await page.evaluate(() => {
    const a = (window as unknown as { __live_analyser?: AnalyserNode }).__live_analyser;
    if (!a) return null;
    const buf = new Float32Array(a.fftSize);
    a.getFloatTimeDomainData(buf);
    const bytes = new Uint8Array(buf.buffer);
    let text = "";
    // In slices: one call with every byte as an argument would exceed the call stack.
    for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    return { base64: btoa(text), rate: a.context.sampleRate };
  });
  if (!read) throw new Error("the player's output has no analyser (no ANALYSER_INIT, or Listen never played)");
  // A fresh copy: a Float32Array needs its offset aligned to 4 bytes, which a pooled Buffer's need not be.
  const bytes = new Uint8Array(Buffer.from(read.base64, "base64"));
  return toneOf(new Float32Array(bytes.buffer), read.rate);
}

/** How often the player ran dry since Listen (`getStreamStats().dropouts`: a gap it played, not a late frame). */
export function playerDropouts(page: Page): Promise<number> {
  return page.evaluate(() => {
    const stats = (window as unknown as { __iem_stream_stats?: () => { dropouts: number } }).__iem_stream_stats;
    return typeof stats === "function" ? stats().dropouts : -1;
  });
}

/** The level (dB) of the audio the player decoded last; −150 while it plays nothing. */
export function audioLevel(page: Page): Promise<number> {
  return page.evaluate(() => {
    const level = (window as unknown as { __iem_audio_level?: () => number }).__iem_audio_level;
    return typeof level === "function" ? level() : -150;
  });
}

/** The player's last decoder error, or null. */
export function audioError(page: Page): Promise<string | null> {
  return page.evaluate(() => {
    const error = (window as unknown as { __iem_audio_error?: () => string | null }).__iem_audio_error;
    return typeof error === "function" ? error() : "the player is not loaded";
  });
}
