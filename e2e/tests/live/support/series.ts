import { SILENCE_DBFS } from "./tone";

// The live specs' series (S7, #10): meter peaks the runner's socket got and
// the frame peaks the page handed its talkback encoder, reduced to the numbers
// a spec judges. Pure, and tested on synthetic series in the mock run
// (`tests/live-series.spec.ts`).

/** −60 dBFS, linear: a meter peak below it reads as silence. */
export const SILENT_PEAK = 0.001;

/** A linear peak in dB, never below `SILENCE_DBFS` (0 reads as silence). */
export function dbOf(peak: number): number {
  if (!Number.isFinite(peak) || peak < 0) throw new Error("dbOf: a peak is a finite number of at least 0");
  return peak > 0 ? Math.max(SILENCE_DBFS, 20 * Math.log10(peak)) : SILENCE_DBFS;
}

function nonEmpty(values: readonly number[], what: string): void {
  if (values.length === 0) throw new Error(`${what} of no values`);
}

/** The median: the middle value, or the mean of the two middle ones. */
export function median(values: readonly number[]): number {
  nonEmpty(values, "median");
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 1 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/** How far a series strays: its largest value less its smallest. */
export function spread(values: readonly number[]): number {
  nonEmpty(values, "spread");
  return Math.max(...values) - Math.min(...values);
}

/** The talkback at its input's meter against what the page encoded, in dB. */
export type TalkbackLevel = {
  /** `meterDb − inputDb`: the engine's talkback gain as measured (A4: −8.405 dB). */
  db: number;
  /** The median meter peak in dB. */
  meterDb: number;
  /** The median encoder frame peak in dB. */
  inputDb: number;
  /** The spread of the encoder frame peaks in dB: how steady the input was. */
  inputSpreadDb: number;
};

/**
 * The talkback's level from one window of both series (linear peaks): the
 * meter frames of the talkback input and the frames the page handed its
 * encoder. Medians, so a frame at a window's edge or a lone dropout does not
 * move it.
 */
export function talkbackLevel(meterPeaks: readonly number[], inputPeaks: readonly number[]): TalkbackLevel {
  const meter = meterPeaks.map(dbOf);
  const input = inputPeaks.map(dbOf);
  const meterDb = median(meter);
  const inputDb = median(input);
  return { db: meterDb - inputDb, meterDb, inputDb, inputSpreadDb: spread(input) };
}

/** How continuous a held signal was at a meter. */
export type Continuity = {
  /** Frames at or above `SILENT_PEAK`. */
  heard: number;
  /** The longest run of frames below it. */
  longestSilence: number;
};

/** The continuity of meter `peaks` (linear) against `SILENT_PEAK`. */
export function continuity(peaks: readonly number[]): Continuity {
  let heard = 0;
  let run = 0;
  let longestSilence = 0;
  for (const p of peaks) {
    if (p >= SILENT_PEAK) {
      heard += 1;
      run = 0;
    } else {
      run += 1;
      longestSilence = Math.max(longestSilence, run);
    }
  }
  return { heard, longestSilence };
}
