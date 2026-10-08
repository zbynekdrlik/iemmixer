import { writeFileSync } from "node:fs";

// Chromium's fake microphone plays a WAV file
// (`--use-file-for-fake-audio-capture=<file>`): the mock talkback spec and the
// live one (S7, #10) write theirs with `toneWav`.

/** A loop-safe tone (a whole number of cycles) as 16-bit mono PCM WAV. */
export function toneWav(file: string, hz: number, amplitude: number, rate = 48_000): string {
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
