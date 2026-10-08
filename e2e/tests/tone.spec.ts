import { test, expect } from "./support/fixtures";
import { toneOf } from "./live/support/tone";
import { celt20msStereo } from "./live/support/burst";

// The live specs' pure estimators (S7, #10), run in the mock E2E job: the
// live specs themselves run only from the ops live run, against the real PC.
// No page is used; every input is synthetic with a known answer.

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

  // Under 1e-5 counts as silent, at 1e-5 does not.
  const quiet = sine(1000, -20);
  quiet.fill(0.99e-5, 1_000, 1_048);
  expect(toneOf(quiet, RATE).gap).toBe(true);
  const audible = sine(1000, -20);
  audible.fill(1.01e-5, 1_000, 1_048);
  expect(toneOf(audible, RATE).gap).toBe(false);
});

test("silence reads -150 dBFS", () => {
  const silence = toneOf(new Float32Array(WINDOW), RATE);
  expect(silence).toEqual({ hz: 0, dbfs: -150, gap: true });
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
