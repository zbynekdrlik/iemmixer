// The live specs' tone estimate (S7, #10): what the player plays, read from
// the samples an AnalyserNode holds (`audio.ts` `readTone`). Pure, and tested
// on synthetic sines in the mock run (`tests/tone.spec.ts`).

/** One window's estimate: frequency, level and whether a dropout is in it. */
export type Tone = { hz: number; dbfs: number; gap: boolean };

/** The level of silence, as the player's own `getAudioLevel` reports it. */
export const SILENCE_DBFS = -150;
/** A gap: at least this many samples in a row under `GAP_LEVEL` (1 ms at 48 kHz). */
export const GAP_SAMPLES = 48;
export const GAP_LEVEL = 1e-5;

/**
 * The tone in `x` sampled at `rate`:
 * - `hz` from the rising zero crossings, each placed between its two samples
 *   by linear interpolation: (crossings − 1) periods over the time from the
 *   first crossing to the last; 0 with fewer than two crossings;
 * - `dbfs` as the peak of a sine of the window's RMS, 20·log10(√2·rms),
 *   never below `SILENCE_DBFS`;
 * - `gap` when `GAP_SAMPLES` or more samples in a row are under `GAP_LEVEL`.
 */
export function toneOf(x: ArrayLike<number>, rate: number): Tone {
  if (!Number.isFinite(rate) || rate <= 0) throw new Error("toneOf: the sample rate must be a positive number");
  let crossings = 0;
  let first = 0;
  let last = 0;
  let power = 0;
  let quiet = 0;
  let gap = false;
  for (let i = 0; i < x.length; i++) {
    const v = x[i];
    power += v * v;
    quiet = Math.abs(v) < GAP_LEVEL ? quiet + 1 : 0;
    if (quiet >= GAP_SAMPLES) gap = true;
    if (i > 0) {
      const prev = x[i - 1];
      if (prev < 0 && v >= 0) {
        const at = i - 1 + -prev / (v - prev);
        if (crossings === 0) first = at;
        last = at;
        crossings += 1;
      }
    }
  }
  const hz = crossings >= 2 ? ((crossings - 1) * rate) / (last - first) : 0;
  const rms = x.length > 0 ? Math.sqrt(power / x.length) : 0;
  const dbfs = rms > 0 ? Math.max(SILENCE_DBFS, 20 * Math.log10(Math.SQRT2 * rms)) : SILENCE_DBFS;
  return { hz, dbfs, gap };
}
