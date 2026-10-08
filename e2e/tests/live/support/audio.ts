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

/** The tone the player plays now: the analyser's window (`ANALYSER_INIT`) at its context's rate. */
export async function readTone(page: Page): Promise<Tone> {
  const read = await page.evaluate(() => {
    const a = (window as unknown as { __live_analyser?: AnalyserNode }).__live_analyser;
    if (!a) return null;
    const buf = new Float32Array(a.fftSize);
    a.getFloatTimeDomainData(buf);
    return { samples: Array.from(buf), rate: a.context.sampleRate };
  });
  if (!read) throw new Error("the player's output has no analyser (no ANALYSER_INIT, or Listen never played)");
  return toneOf(read.samples, read.rate);
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
