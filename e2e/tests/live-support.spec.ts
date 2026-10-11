import { EventEmitter } from "node:events";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium, type APIRequestContext } from "@playwright/test";
import { test, expect } from "./support/fixtures";
import { BurstWatch } from "./live/support/burst";
import { Desk, LiveMixer, RESTORE_MS, type Cmd } from "./live/support/desk";
import { SILENT_PEAK, continuity, dbOf, median, spread, talkbackLevel } from "./live/support/series";
import { apiAt, runMarker, storeLogin } from "./live/support/env";
import { guardConsole, redacted } from "./live/support/console";
import { Relay } from "./live/support/relay";
import { closeFacts } from "./live/support/socket";
import { PushLedger, bodyOf, endpointOf, isPostTo, postedEndpoint } from "./live/support/push";

// The live specs' support (S7, #10), run in the mock E2E job: the live specs
// themselves run only from the ops live run, against the real PC. The burst
// watch, the desk and its mixer socket run here on local stand-ins for the
// public host's sockets (the burst's edges, the frames and the server's
// answers are fed in); every series is synthetic with a known answer. Only
// the console guard and the stored login use a page (the login's on a local
// server).

/** What a stand-in socket was sent. */
type Sent = { cmd: string; [field: string]: unknown };

/** A socket stand-in: records what is sent, answers GetLimiterParams (the desk's barrier), feeds server events in. */
class FakeSocket extends EventEmitter {
  readonly sent: Sent[] = [];
  /** Answer each GetLimiterParams at once (false: the test answers with `limiter`). */
  autoAnswer = true;

  send(data: string): void {
    const sent = JSON.parse(data) as Sent;
    this.sent.push(sent);
    if (sent.cmd === "GetLimiterParams" && this.autoAnswer) setTimeout(() => this.limiter(0), 1);
  }

  /** A LimiterParams answer with the counter at `active_seconds`. */
  limiter(active_seconds: number): void {
    this.event("LimiterParams", { mix: "member1", limit_db: -6, limit_norm: 0, enabled: true, active_seconds });
  }

  /** A server event (a text frame). */
  event(event: string, data: unknown): void {
    this.emit("message", Buffer.from(JSON.stringify({ event, data })), false);
  }

  /** How many commands were sent when the client closed the socket; null while open. */
  closedAfter: number | null = null;

  close(): void {
    this.closedAfter ??= this.sent.length;
  }

  /** The changes sent, by their tag, without the barrier's requests. */
  changes(): string[] {
    return this.sent.filter((c) => c.cmd !== "GetLimiterParams").map((c) => String(c.tag));
  }

  /** How many barriers (GetLimiterParams) were sent. */
  barriers(): number {
    return this.sent.filter((c) => c.cmd === "GetLimiterParams").length;
  }
}

/** One listen frame: a CELT 20 ms stereo Opus packet (TOC config 31, s set, c = 0). */
const FRAME = Buffer.from([(31 << 3) | 0b100, 0x5a, 0x5a]);

/** 5 s of the slot's own 20 ms frames: what a watch sees between two bursts (30 s apart) at the least. */
const FIVE_SECONDS_OF_FRAMES = 250;

type RawWatch = {
  watch: BurstWatch;
  status: (status: string) => void;
  /** One listen frame. */
  frame: () => void;
  /** `n` listen frames. */
  frames: (n: number) => void;
};

/** A real BurstWatch on a stand-in socket, with functions that feed it an AudioStatus and listen frames. */
function rawWatch(): RawWatch {
  const socket = new FakeSocket();
  const watch = new (BurstWatch as unknown as new (ws: unknown) => BurstWatch)(socket);
  const frame = () => socket.emit("message", FRAME, true);
  return {
    watch,
    status: (status) => socket.event("AudioStatus", { status }),
    frame,
    frames: (n) => {
      for (let i = 0; i < n; i++) frame();
    },
  };
}

test("the watch counts a burst only when it saw the burst begin", async () => {
  const { watch, status, frame, frames } = rawWatch();
  // Opened in the middle of a burst: the ListenStart answer, then `probe`
  // with the first frame (the server's gate is per session).
  status("listening");
  status("probe");
  frame();
  frame();
  expect(watch.inBurst(), "a burst of unknown length left").toBe(false);
  expect(watch.leftMs()).toBe(0);
  expect(watch.burstFrames).toBe(0);
  await expect(watch.burst({ within: 200 })).rejects.toThrow("no burst within 0.2 s");

  // That burst ends; the slot's own frames come; the next burst begins in view.
  status("listening");
  frames(FIVE_SECONDS_OF_FRAMES);
  expect(watch.inBurst()).toBe(false);
  status("probe");
  frame();
  frame();
  frame();
  expect(watch.inBurst()).toBe(true);
  expect(watch.leftMs()).toBeGreaterThan(27_000);
  expect(watch.burstFrames).toBe(3);
  expect(watch.badFrames).toBe(0);
  await watch.burst({ within: 200 });

  // Its end: own frames are not the burst's.
  status("listening");
  frame();
  expect(watch.inBurst()).toBe(false);
  expect(watch.burstFrames).toBe(3);
});

test("a burst's edge after a stall is no burst begun: it needs 5 s of the slot's own frames before it", async () => {
  const { watch, status, frame, frames } = rawWatch();
  status("listening");
  frames(FIVE_SECONDS_OF_FRAMES);
  status("probe");
  expect(watch.inBurst(), "a burst after 5 s of own frames").toBe(true);
  // A stall of the probe frames inside the burst (over the server gate's
  // 100 ms): `listening` with an own frame, then `probe` again. The burst
  // did not begin there, and may end at any moment.
  status("listening");
  frame();
  status("probe");
  expect(watch.inBurst(), "a burst after one own frame").toBe(false);
  expect(watch.leftMs()).toBe(0);
  // One frame short of 5 s is still not enough.
  status("listening");
  frames(FIVE_SECONDS_OF_FRAMES - 1);
  status("probe");
  expect(watch.inBurst()).toBe(false);
  status("listening");
  frames(FIVE_SECONDS_OF_FRAMES);
  status("probe");
  expect(watch.inBurst()).toBe(true);
});

// Live run 3 (#10, 2026-10-09): runner sockets closed from the far side while
// the PC's server.log shows no disconnect of their sessions, and a watch saw
// no burst for 163 s while bursts came. A failure now carries what the socket
// saw (numbers, fixed words and the UTC time to match the server's log; never
// the far side's reason text), so the next run tells the tunnel from the
// server and a silent socket from a miscount.

/** `<code> at <UTC time>, <s> s after it opened, <s> s after its last message`: the words of a far-side close. */
const CLOSE_FACTS = /code 1006 at \d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z, \d+\.\d s after it opened, \d+\.\d\d s after its last message/;

test("a far-side close of the watch names its code, its UTC time, and how long the socket was open and quiet", () => {
  const socket = new FakeSocket();
  const watch = new (BurstWatch as unknown as new (ws: unknown) => BurstWatch)(socket);
  socket.emit("open");
  socket.event("AudioStatus", { status: "listening" });
  socket.emit("message", FRAME, true);
  socket.emit("close", 1006, Buffer.from("the far side's own words"));
  expect(() => watch.check()).toThrow("the server closed the burst watch's socket (no reconnect): code 1006 at ");
  expect(() => watch.check()).toThrow(CLOSE_FACTS);
  expect(() => watch.check()).toThrow(/; the last status listening \d+\.\d s before$/);
  // The far side's reason is not the runner's to print (it can be anything).
  expect(() => watch.check()).not.toThrow(/own words/);
});

test("a close with no code and no message says so", () => {
  const socket = new FakeSocket();
  const watch = new (BurstWatch as unknown as new (ws: unknown) => BurstWatch)(socket);
  socket.emit("close");
  expect(() => watch.check()).toThrow(
    /\(no reconnect\): no code at \S+Z, before it opened, with no message before; no status before$/,
  );
});

test("a watch that saw no burst says what it got and where it stands", async () => {
  const { watch, status, frames } = rawWatch();
  status("listening");
  frames(7);
  // A burst it did not see begin: joined, so no burst counts.
  status("probe");
  frames(3);
  await expect(watch.burst({ within: 100 })).rejects.toThrow(
    /^no burst within 0\.1 s \(the watch: 10 binary and 2 text frames, the last \d+\.\d\d s ago; statuses listening 1, probe 1, no_source 0, stopped 0, other 0; 7 own frames since the last listening; inside a burst it did not see begin\)$/,
  );

  status("listening");
  status("odd");
  frames(4);
  await expect(watch.burst({ within: 100 })).rejects.toThrow(
    /statuses listening 2, probe 1, no_source 0, stopped 0, other 1; 4 own frames since the last listening; outside a burst\)$/,
  );

  frames(FIVE_SECONDS_OF_FRAMES);
  status("probe");
  // Counted, but with less left than asked: in the probe's own millisecond
  // all 28 s are left, so the ask comes a little later.
  await new Promise((r) => setTimeout(r, 20));
  await expect(watch.burst({ minLeftMs: 28_000, within: 100 })).rejects.toThrow(
    /; inside a burst begun \d+\.\d s ago\)$/,
  );
});

test("a far-side close of a page's socket in the relay names its code and times", () => {
  const relay = new Relay();
  relay.farClosed("/ws/audio", "code 1006 at 2026-10-09T18:40:23.512Z, 22.6 s after it opened, 0.02 s after its last message");
  expect(() => relay.check()).toThrow(
    "relay: the server closed /ws/audio (no reconnect): code 1006 at 2026-10-09T18:40:23.512Z, 22.6 s after it opened, 0.02 s after its last message",
  );
  expect(relay.serverClosed).toEqual(["/ws/audio"]);
  const mixer = new Relay();
  // A mixer page's path names the page (site data, P6): only its kind is named.
  mixer.farClosed("/ws/member1", "no code at 2026-10-09T18:40:23.512Z, before it opened, with no message before");
  expect(() => mixer.check()).toThrow(/^relay: the server closed the page's mixer socket \(no reconnect\): no code at /);
});

test("closeFacts words the numbers and nothing else", () => {
  const at = Date.UTC(2026, 9, 9, 18, 40, 23, 512);
  expect(closeFacts({ code: 1001, at, openedAt: at - 22_600, lastAt: at - 20 })).toBe(
    "code 1001 at 2026-10-09T18:40:23.512Z, 22.6 s after it opened, 0.02 s after its last message",
  );
  expect(closeFacts({ code: "1006", at, openedAt: null, lastAt: null })).toBe(
    "no code at 2026-10-09T18:40:23.512Z, before it opened, with no message before",
  );
});

test("a far-side close of the runner's mixer socket names its code and times", () => {
  const { mixer, socket } = mixerOn();
  socket.emit("open");
  socket.event("Meters", { meters: {} });
  socket.emit("close", 1006, Buffer.from(""));
  expect(() => mixer.check()).toThrow("the server closed the test socket (no reconnect): code 1006 at ");
  expect(() => mixer.check()).toThrow(CLOSE_FACTS);
});

/** The engine's talkback gain into its input (program spec A4). */
const TALKBACK_GAIN = 0.379934;

/** `n` copies of `value`. */
function constant(value: number, n: number): number[] {
  return new Array<number>(n).fill(value);
}

test("dbOf reads a linear peak in dB with silence at -150 dB", () => {
  expect(dbOf(1)).toBe(0);
  expect(dbOf(0.1)).toBeCloseTo(-20, 10);
  expect(dbOf(SILENT_PEAK)).toBeCloseTo(-60, 10);
  expect(dbOf(2)).toBeCloseTo(6.0206, 4);
  // Silence and anything quieter than the floor read the floor.
  expect(dbOf(0)).toBe(-150);
  expect(dbOf(1e-9)).toBe(-150);
  expect(dbOf(10 ** (-149 / 20))).toBeCloseTo(-149, 10);
  // A peak is never negative and always finite.
  expect(() => dbOf(-0.1)).toThrow("dbOf");
  expect(() => dbOf(Number.NaN)).toThrow("dbOf");
  expect(() => dbOf(Number.POSITIVE_INFINITY)).toThrow("dbOf");
});

test("the median is the middle value, or the mean of the two middle ones, of an unsorted series", () => {
  expect(median([3, 1, 2])).toBe(2);
  expect(median([4, 1, 3, 2])).toBe(2.5);
  expect(median([-8])).toBe(-8);
  expect(median([5, 5, 1, 9, 9])).toBe(5);
  // A few outliers move it not at all.
  expect(median([-20, -20.1, -150, -19.9, -20, -150, -20])).toBe(-20);
  // The series is left as it was.
  const series = [3, 1, 2];
  median(series);
  expect(series).toEqual([3, 1, 2]);
  expect(() => median([])).toThrow("median of no values");
});

test("the spread is the largest value less the smallest", () => {
  expect(spread([1, 3, 2])).toBe(2);
  expect(spread([-6.1, -6.0, -5.95])).toBeCloseTo(0.15, 10);
  expect(spread([7])).toBe(0);
  expect(() => spread([])).toThrow("spread of no values");
});

test("the talkback level is the meter's median against the encoder's in dB: the A4 gain reads -8.406 dB", () => {
  // The page encodes a steady half-scale tone; the meter shows it at the engine's gain.
  const input = constant(0.5, 150);
  const meter = constant(0.5 * TALKBACK_GAIN, 30);
  const level = talkbackLevel(meter, input);
  expect(level.db).toBeCloseTo(-8.4058, 4);
  expect(level.inputDb).toBeCloseTo(-6.0206, 4);
  expect(level.meterDb).toBeCloseTo(-14.426, 3);
  expect(level.inputSpreadDb).toBe(0);

  // A frame at the window's edge and a lone dropout move neither median.
  const edged = [...meter, 0, 0.5 * TALKBACK_GAIN * 0.5];
  expect(talkbackLevel(edged, input).db).toBeCloseTo(-8.4058, 4);

  // An input 0.25 dB louder halfway through is not steady (spread > 0.2 dB),
  // one 0.1 dB louder is.
  const stepped = [...constant(0.5, 75), ...constant(0.5 * 10 ** (0.25 / 20), 75)];
  expect(talkbackLevel(meter, stepped).inputSpreadDb).toBeCloseTo(0.25, 10);
  const nudged = [...constant(0.5, 75), ...constant(0.5 * 10 ** (0.1 / 20), 75)];
  expect(talkbackLevel(meter, nudged).inputSpreadDb).toBeCloseTo(0.1, 10);

  // A meter 0.3 dB off the gain reads 0.3 dB off it.
  const off = constant(0.5 * TALKBACK_GAIN * 10 ** (0.3 / 20), 30);
  expect(talkbackLevel(off, input).db).toBeCloseTo(-8.1058, 4);

  expect(() => talkbackLevel([], input)).toThrow("median of no values");
  expect(() => talkbackLevel(meter, [])).toThrow("median of no values");
});

test("continuity counts the frames above -60 dB and the longest silent run", () => {
  const loud = 0.2;
  // 50 frames, every fifth silent: 40 heard, never two silent in a row.
  const fifths = Array.from({ length: 50 }, (_, i) => (i % 5 === 4 ? 0 : loud));
  expect(continuity(fifths)).toEqual({ heard: 40, longestSilence: 1 });
  // A hang of five frames in a row.
  const hang = [...constant(loud, 20), ...constant(0, 5), ...constant(loud, 25)];
  expect(continuity(hang)).toEqual({ heard: 45, longestSilence: 5 });
  // −60 dB itself is heard; just below it is silent.
  expect(continuity([SILENT_PEAK, SILENT_PEAK * 0.999])).toEqual({ heard: 1, longestSilence: 1 });
  // A silent end is a run too.
  expect(continuity([loud, 0, 0, 0])).toEqual({ heard: 1, longestSilence: 3 });
  expect(continuity([])).toEqual({ heard: 0, longestSilence: 0 });
});

/** A watch as `BurstWatch.open` leaves it: ListenStart answered `listening`, and the slot's own frames coming. */
function watchOn(): { watch: BurstWatch; status: (status: string) => void } {
  const { watch, status, frames } = rawWatch();
  status("listening");
  frames(FIVE_SECONDS_OF_FRAMES);
  return { watch, status };
}

/** A real LiveMixer on a stand-in socket. */
function mixerOn(): { mixer: LiveMixer; socket: FakeSocket } {
  const socket = new FakeSocket();
  const mixer = new (LiveMixer as unknown as new (ws: unknown, label: string) => LiveMixer)(socket, "the test socket");
  return { mixer, socket };
}

/** A command marked with `tag`, so the order of the sends reads back. */
const cmd = (tag: string): Cmd => ({ cmd: "SetLevel", tag });

/** A desk on `watch`; its `open` is not used here (it needs the public host). */
function deskOn(watch: BurstWatch): Desk {
  return new Desk(undefined as unknown as APIRequestContext, watch);
}

test("the desk refuses a change outside its burst and sends nothing then", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  expect(() => desk.change(mixer, cmd("a"), cmd("undo a"))).toThrow("a change outside desk.during");

  status("probe");
  await expect(
    desk.during(async () => {
      // The burst ends: `listening` comes before the change.
      status("listening");
      desk.change(mixer, cmd("b"), cmd("undo b"));
    }),
  ).rejects.toThrow("a change came after the burst's end");
  expect(socket.sent).toEqual([]);
  expect(watch.statuses.map((s) => s.status)).toEqual(["listening", "probe", "listening"]);
});

test("the desk puts the changes back newest first, each in its own order, inside the burst", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  status("probe");
  const result = await desk.during(async () => {
    desk.change(mixer, [cmd("a1"), cmd("a2")], [cmd("undo a1"), cmd("undo a2")]);
    const mark = desk.mark();
    desk.change(mixer, cmd("b"), [cmd("undo b1"), cmd("undo b2")]);
    desk.change(mixer, cmd("c"), cmd("undo c"));
    // Only the changes after the mark go back here.
    await desk.restore(mark);
    expect(socket.changes()).toEqual(["a1", "a2", "b", "c", "undo c", "undo b1", "undo b2"]);
    expect(socket.barriers()).toBe(1);
    return 7;
  });
  expect(result).toBe(7);
  expect(socket.changes()).toEqual(["a1", "a2", "b", "c", "undo c", "undo b1", "undo b2", "undo a1", "undo a2"]);
  // Each restore waited for the engine: a barrier after its undos.
  expect(socket.barriers()).toBe(2);
  expect(socket.sent[socket.sent.length - 1].cmd).toBe("GetLimiterParams");
  // Nothing is left to put back at the end.
  await desk.end();
  expect(socket.changes().length).toBe(9);
});

test("a failing step still puts its changes back, and the step's failure is the test's", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  status("probe");
  await expect(
    desk.during(async () => {
      desk.change(mixer, cmd("a"), cmd("undo a"));
      throw new Error("the step failed");
    }),
  ).rejects.toThrow("the step failed");
  expect(socket.changes()).toEqual(["a", "undo a"]);
  expect(socket.barriers()).toBe(1);
});

test("a restore after the burst's end fails, though the changes went back", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  status("probe");
  await expect(
    desk.during(async () => {
      desk.change(mixer, cmd("a"), cmd("undo a"));
      status("listening");
    }),
  ).rejects.toThrow("the restore came after the burst's end");
  expect(socket.changes()).toEqual(["a", "undo a"]);
});

test("the guard puts the changes back the moment the burst ends, and the steps fail", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  status("probe");
  await expect(
    desk.during(async () => {
      desk.change(mixer, [cmd("a1"), cmd("a2")], [cmd("undo a1"), cmd("undo a2")]);
      desk.change(mixer, cmd("b"), cmd("undo b"));
      // The burst ends while the step waits: the guard does not wait for it.
      status("listening");
      await expect.poll(() => socket.changes().length, { timeout: 1_000, intervals: [10] }).toBe(6);
      expect(socket.changes()).toEqual(["a1", "a2", "b", "undo b", "undo a1", "undo a2"]);
    }),
  ).rejects.toThrow(
    /^the burst ended with changes in place: they went back after its end \(the steps ran \d+\.\d s in a burst entered with \d+\.\d s left; ended by its listening status\)$/,
  );
  // Nothing went back twice, and the steps' end waited until the engine had
  // the guard's undos (a barrier after them): the server drops a closed
  // socket's queued commands.
  expect(socket.changes().length).toBe(6);
  expect(socket.barriers()).toBe(1);
  expect(socket.sent[socket.sent.length - 1].cmd).toBe("GetLimiterParams");
  // The guard stopped with the steps.
  await new Promise((resolve) => setTimeout(resolve, 120));
  expect(socket.changes().length).toBe(6);
});

test("a page action's undo goes back in its place, and at once from the guard", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  // Talk's release, as the talkback spec keeps it: logged among the commands.
  const release = async (): Promise<void> => {
    socket.sent.push({ cmd: "Act", tag: "release" });
  };
  status("probe");
  await desk.during(async () => {
    desk.change(mixer, cmd("a"), cmd("undo a"));
    desk.track("Talk", release);
    desk.change(mixer, cmd("b"), cmd("undo b"));
  });
  expect(socket.changes()).toEqual(["a", "b", "undo b", "release", "undo a"]);

  // The burst ends while Talk is held: the guard releases it without waiting for the step.
  const second = watchOn();
  const guarded = deskOn(second.watch);
  const before = socket.changes().length;
  second.status("probe");
  await expect(
    guarded.during(async () => {
      guarded.track("Talk", release);
      second.status("listening");
      await expect.poll(() => socket.changes().length, { timeout: 1_000, intervals: [10] }).toBe(before + 1);
    }),
  ).rejects.toThrow("the burst ended with changes in place");
  expect(socket.changes().slice(before)).toEqual(["release"]);
});

test("a page action's undo that hangs holds back no older undo", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  status("probe");
  await expect(
    desk.during(async () => {
      desk.change(mixer, cmd("a"), cmd("undo a"));
      // A release that never ends (a hung page).
      desk.track("Talk", () => new Promise<void>(() => undefined));
    }),
  ).rejects.toThrow("a page action's undo did not end within 2 s");
  expect(socket.changes()).toEqual(["a", "undo a"]);
  expect(socket.barriers()).toBe(1);
});

test("the desk's end waits for a restore a timed-out step left running, and closes after it", async () => {
  const { watch, status } = watchOn();
  const desk = deskOn(watch);
  const { mixer, socket } = mixerOn();
  const open = LiveMixer.open;
  try {
    LiveMixer.open = async () => mixer;
    await desk.open("engineer", "engineer");
  } finally {
    LiveMixer.open = open;
  }
  status("probe");
  let releasing = false;
  // The steps end; their restore waits on Talk's release (a slow page) with the card's undo taken.
  const steps = desk
    .during(async () => {
      desk.change(mixer, cmd("card"), cmd("undo card"));
      desk.track("Talk", () => {
        releasing = true;
        return new Promise<void>(() => undefined);
      });
    })
    .then(
      () => "ended",
      (e: Error) => e.message,
    );
  await expect.poll(() => releasing).toBe(true);
  // The test's timeout: the desk's end, while that restore still waits.
  await desk.end();
  // The card's undo went out, and reached the engine, before the close.
  expect(socket.changes()).toEqual(["card", "undo card"]);
  expect(socket.closedAfter).toBe(socket.sent.length);
  expect(socket.sent[socket.sent.length - 1].cmd).toBe("GetLimiterParams");
  expect(socket.sent.map((c) => String(c.tag ?? c.cmd))).toEqual([
    "card",
    "undo card",
    "GetLimiterParams",
    "GetLimiterParams",
  ]);
  expect(await steps).toBe("a page action's undo did not end within 2 s");
});

test("a step that needs more of the burst than is left is refused before it starts", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  status("probe");
  await desk.during(async () => {
    desk.need(20_000, "the drive");
    expect(() => desk.need(29_000, "the drive")).toThrow(/^the drive needs 29\.0 s of the burst; 2[78]\.\d s are left$/);
    expect(() => desk.track("Talk", async () => undefined)).not.toThrow();
  });
  expect(socket.sent).toEqual([]);
  expect(() => desk.need(1, "a step")).toThrow("a step outside desk.during");
  void mixer;
});

test("the desk's sockets: one per page, and each one's commands reach the engine before it closes", async () => {
  const { watch } = watchOn();
  const desk = deskOn(watch);
  const first = mixerOn();
  const second = mixerOn();
  const open = LiveMixer.open;
  try {
    // The public host's open, stood in for: the sockets above.
    const queue = [first.mixer, second.mixer];
    LiveMixer.open = async () => queue.shift() as LiveMixer;
    expect(await desk.open("engineer", "engineer")).toBe(first.mixer);
    await expect(desk.open("engineer", "engineer")).rejects.toThrow(
      "a second runner engineer socket on one page (no reconnect)",
    );
    expect(await desk.open("engineer", "member1")).toBe(second.mixer);
  } finally {
    LiveMixer.open = open;
  }
  first.mixer.send(cmd("a"));
  await desk.end();
  // A barrier after every command sent, then the close.
  expect(first.socket.closedAfter).toBe(2);
  expect(first.socket.sent.map((c) => c.cmd)).toEqual(["SetLevel", "GetLimiterParams"]);
  expect(second.socket.closedAfter).toBe(1);
  expect(second.socket.barriers()).toBe(1);
  expect(() => first.mixer.send(cmd("b"))).toThrow("the test socket is closed");
});

test("the mixer sends one request at a time, so each answer is its own", async () => {
  const { mixer, socket } = mixerOn();
  socket.autoAnswer = false;
  const a = mixer.limiter();
  const b = mixer.activeSeconds();
  await expect.poll(() => socket.barriers(), { intervals: [10] }).toBe(1);
  // The second waits for the first's answer before it is sent.
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(socket.barriers()).toBe(1);
  socket.limiter(1.5);
  expect((await a).active_seconds).toBe(1.5);
  await expect.poll(() => socket.barriers(), { intervals: [10] }).toBe(2);
  socket.limiter(2.5);
  expect(await b).toBe(2.5);
});

test("the desk's end puts back what an abandoned step left in place", async () => {
  const { watch, status } = watchOn();
  const { mixer, socket } = mixerOn();
  const desk = deskOn(watch);
  status("probe");
  // A step that does not end, as one a test timeout abandons.
  let changed = false;
  let finish = (): void => undefined;
  const abandoned = desk.during(async () => {
    desk.change(mixer, cmd("a"), cmd("undo a"));
    changed = true;
    await new Promise<void>((resolve) => (finish = resolve));
  });
  await expect.poll(() => changed).toBe(true);
  await desk.end();
  expect(socket.changes()).toEqual(["a", "undo a"]);
  expect(() => desk.change(mixer, cmd("b"), cmd("undo b"))).toThrow("a change after the test's end");
  // The abandoned step ends later: nothing is left for it to put back.
  finish();
  await abandoned;
  expect(socket.changes()).toEqual(["a", "undo a"]);
});

test("a wait inside the burst is cut to the time left in it less the restore's, and never to 0 (no timeout)", async () => {
  const { watch, status } = watchOn();
  const desk = deskOn(watch);
  expect(desk.bound(5_000)).toBe(1);
  status("probe");
  expect(desk.bound(5_000)).toBe(5_000);
  // 28 s of the watch's burst, less RESTORE_MS.
  const left = desk.bound(60_000);
  expect(left).toBeGreaterThan(25_000);
  expect(left).toBeLessThanOrEqual(28_000 - RESTORE_MS);
  status("listening");
  expect(desk.bound(5_000)).toBe(1);
});

test("a desk whose test has no time left for a burst fails before it waits", async () => {
  const { watch, status } = watchOn();
  status("probe");
  // A test of 60 s: its burst would have had to come 15 s before the desk was made.
  const desk = new Desk(undefined as unknown as APIRequestContext, watch, Desk.deadline(60_000));
  let ran = false;
  await expect(
    desk.during(async () => {
      ran = true;
    }),
  ).rejects.toThrow("no time left in the test to wait for a burst");
  expect(ran).toBe(false);
  expect(Desk.deadline(0)).toBeNull();
  const deadline = Desk.deadline(240_000) as number;
  expect(deadline - Date.now()).toBeGreaterThan(164_000);
  expect(deadline - Date.now()).toBeLessThanOrEqual(165_000);
});

test("the mixer reads a channel as the newest state and the updates after it show it", async () => {
  const { mixer, socket } = mixerOn();
  const state = (level_db: number, mix: string) => ({
    channels: [{ id: "mic1", level_db, muted: false, pan: 0.5 }],
    connected: true,
    mix,
  });
  expect(mixer.channel("mic1")).toBeUndefined();
  expect(() => mixer.soloed()).toThrow("no solo state");
  socket.event("State", state(-6, "member1"));
  socket.event("SoloUpdate", { soloed: [] });
  expect(mixer.channel("mic1")).toEqual({ id: "mic1", level_db: -6, muted: false, pan: 0.5 });
  socket.event("ChannelUpdate", { id: "mic1", level_db: 3, muted: true, pan: 0.25 });
  socket.event("ChannelUpdate", { id: "mic2", level_db: 9, muted: false, pan: 0.5 });
  expect(mixer.channel("mic1")).toEqual({ id: "mic1", level_db: 3, muted: true, pan: 0.25 });
  expect(mixer.channel("mic2")).toBeUndefined();
  // A new state (an engine resync) replaces what came before it.
  socket.event("State", state(0, "member2"));
  expect(mixer.channel("mic1")).toEqual({ id: "mic1", level_db: 0, muted: false, pan: 0.5 });
  expect(mixer.mixId()).toBe("member2");
  expect(mixer.soloed()).toEqual([]);
  socket.event("SoloUpdate", { soloed: ["mic1"] });
  expect(mixer.soloed()).toEqual(["mic1"]);
});

test("the mixer reads meters by arrival, the louder side, and a server close fails it for good", async () => {
  const { mixer, socket } = mixerOn();
  const t0 = Date.now();
  socket.event("Meters", { meters: { mic1: [0.1, 0.3], mic2: [0.5, 0.5] } });
  socket.event("Meters", { meters: { mic2: [0.2, 0.1] } });
  expect(mixer.meterFrames(t0)).toBe(2);
  expect(mixer.meterFrames(t0, t0)).toBe(0);
  expect(mixer.peaks("mic1", t0)).toEqual([0.3]);
  expect(mixer.peaks("mic2", t0)).toEqual([0.5, 0.2]);
  expect(mixer.peaks("mic2", Date.now() + 1)).toEqual([]);

  socket.emit("close");
  expect(() => mixer.check()).toThrow("the server closed the test socket (no reconnect)");
  expect(() => mixer.send(cmd("a"))).toThrow("the server closed the test socket (no reconnect)");
  expect(socket.sent).toEqual([]);
});

test("the run's marker is built from the run id and attempt, and a refusal names the variable, never its value", () => {
  expect(runMarker({ GITHUB_RUN_ID: "18342255120", GITHUB_RUN_ATTEMPT: "2" })).toBe(
    "iemmixer-live-marker-18342255120-2",
  );
  expect(() => runMarker({ GITHUB_RUN_ATTEMPT: "1" })).toThrow("GITHUB_RUN_ID is not set");
  expect(() => runMarker({ GITHUB_RUN_ID: "", GITHUB_RUN_ATTEMPT: "1" })).toThrow("GITHUB_RUN_ID is not set");
  expect(() => runMarker({ GITHUB_RUN_ID: "7" })).toThrow("GITHUB_RUN_ATTEMPT is not set");
  for (const bad of ["1 ", "-1", "1.5", "1e3", "0x1f", "1;zyxqwvn", "1".repeat(21)]) {
    let message = "";
    try {
      runMarker({ GITHUB_RUN_ID: "7", GITHUB_RUN_ATTEMPT: bad });
    } catch (e) {
      message = (e as Error).message;
    }
    expect(message, `attempt ${JSON.stringify(bad)}`).toBe("GITHUB_RUN_ATTEMPT is not a run number (digits)");
  }
  expect(() => runMarker({ GITHUB_RUN_ID: "zyxqwvn", GITHUB_RUN_ATTEMPT: "1" })).toThrow(
    "GITHUB_RUN_ID is not a run number (digits)",
  );
});

test("a push body's endpoint: any non-empty string the server stores, and an https:// URL for the browser's", () => {
  const endpoint = "https://push.example.invalid/send/zyxqwvn";
  expect(endpointOf({ endpoint, keys: { p256dh: "k", auth: "a" } })).toBe(endpoint);
  expect(endpointOf({ endpoint })).toBe(endpoint);
  expect(endpointOf({ endpoint: "http://push.example.invalid/send/zyxqwvn" })).toBeNull();
  expect(endpointOf({ endpoint: "not a url" })).toBeNull();
  expect(endpointOf({ endpoint: 7 })).toBeNull();
  expect(endpointOf({})).toBeNull();
  expect(endpointOf(null)).toBeNull();
  expect(endpointOf(endpoint)).toBeNull();
  // What the server stores is any non-empty endpoint (routes.rs push_subscribe), so cleanup reads it so.
  expect(postedEndpoint({ endpoint: "not a url" })).toBe("not a url");
  expect(postedEndpoint({ endpoint: "http://push.example.invalid/x" })).toBe("http://push.example.invalid/x");
  expect(postedEndpoint({ endpoint: "" })).toBeNull();
  expect(postedEndpoint({ endpoint: 7 })).toBeNull();
  expect(postedEndpoint(null)).toBeNull();
});

/** A request stand-in: a method, a URL and a raw body. */
function sentReq(method: string, url: string, body: string | null) {
  return { method: () => method, url: () => url, postData: () => body };
}

test("a request's body is read without throwing, whatever it holds", () => {
  expect(bodyOf(sentReq("POST", "https://mixer.example.org/a", '{"endpoint":"e"}'))).toEqual({ endpoint: "e" });
  expect(bodyOf(sentReq("POST", "https://mixer.example.org/a", "{not json zyxqwvn"))).toBeNull();
  expect(bodyOf(sentReq("GET", "https://mixer.example.org/a", null))).toBeNull();
});

test("the ledger keeps every posted endpoint until an unsubscribe of it answers 200", () => {
  const ledger = new PushLedger();
  const sub = (endpoint: string) => sentReq("POST", "https://mixer.example.org/api/push/subscribe", JSON.stringify({ endpoint }));
  const unsub = (endpoint: string, status: number) => ({
    status: () => status,
    request: () => sentReq("POST", "https://mixer.example.org/api/push/unsubscribe", JSON.stringify({ endpoint })),
  });
  ledger.sent(sub("https://push.example.invalid/a"));
  // A body that is not JSON, a GET, another route: nothing recorded, nothing thrown.
  ledger.sent(sentReq("POST", "https://mixer.example.org/api/push/subscribe", "{zyxqwvn"));
  ledger.sent(sentReq("GET", "https://mixer.example.org/api/push/subscribe", '{"endpoint":"x"}'));
  ledger.sent(sentReq("POST", "https://mixer.example.org/api/client-error", '{"endpoint":"y"}'));
  expect(ledger.pending()).toEqual(["https://push.example.invalid/a"]);
  // A refused unsubscribe leaves it on the server.
  ledger.answered(unsub("https://push.example.invalid/a", 403));
  expect(ledger.pending()).toEqual(["https://push.example.invalid/a"]);
  // A re-subscribe (the page drops the old one in the browser only) adds the new one.
  ledger.sent(sub("https://push.example.invalid/b"));
  ledger.answered(unsub("https://push.example.invalid/b", 200));
  expect(ledger.pending()).toEqual(["https://push.example.invalid/a"]);
  // A 200 subscribe answer is no revoke.
  ledger.answered({ status: () => 200, request: () => sub("https://push.example.invalid/a") });
  expect(ledger.pending()).toEqual(["https://push.example.invalid/a"]);
  ledger.answered(unsub("https://push.example.invalid/a", 200));
  expect(ledger.pending()).toEqual([]);
});

test("a request that gets no answer fails with the method and the path, never the URL or the token", async () => {
  // A stand-in, not a credential: the test checks it never reaches the error.
  const standIn = ["zyxqwvn", "stand", "in"].join(".");
  const at = { origin: "https://mixer.example.org", token: standIn };
  let seen: { url: string; method?: string; auth?: string } | null = null;
  const failing = {
    fetch: async (url: string, options?: { method?: string; headers?: Record<string, string> }) => {
      seen = { url, method: options?.method, auth: options?.headers?.Authorization };
      throw new Error(`apiRequestContext.fetch: ${url} Authorization: Bearer ${standIn}`);
    },
  } as unknown as Parameters<typeof apiAt>[0];
  let message = "";
  try {
    await apiAt(failing, at, "POST", "/api/push/unsubscribe", { endpoint: "https://push.example.invalid/zyxqwvn" });
  } catch (e) {
    message = (e as Error).message;
  }
  expect(message).toBe("POST /api/push/unsubscribe got no answer");
  expect(seen).toEqual({
    url: "https://mixer.example.org/api/push/unsubscribe",
    method: "POST",
    auth: `Bearer ${standIn}`,
  });
});

test("the live console guard keeps errors, warnings and page errors with their URLs redacted", async ({ browser }) => {
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    const problems = guardConsole(page);
    await page.evaluate(() => {
      console.log("plain log https://mixer.example.org/x");
      console.warn("[push] failed for wss://mixer.example.org/ws/engineer?token=zyxqwvn");
      console.error("boom https://push.example.invalid/send/zyxqwvn");
      setTimeout(() => {
        throw new Error("thrown at https://mixer.example.org/engineer?token=zyxqwvn");
      }, 0);
    });
    await expect.poll(() => problems.length).toBe(3);
    expect(problems).toEqual([
      "[warning] [push] failed for <url>",
      "[error] boom <url>",
      "[pageerror] thrown at <url>",
    ]);
  } finally {
    await context.close();
  }
});

test("a push request is a POST to the route's path, whatever its host or query", () => {
  const req = (method: string, url: string) => ({ method: () => method, url: () => url });
  const path = "/api/push/subscribe";
  expect(isPostTo(req("POST", "https://mixer.example.org/api/push/subscribe"), path)).toBe(true);
  expect(isPostTo(req("POST", "http://10.0.0.10/api/push/subscribe?x=1"), path)).toBe(true);
  expect(isPostTo(req("GET", "https://mixer.example.org/api/push/subscribe"), path)).toBe(false);
  expect(isPostTo(req("POST", "https://mixer.example.org/api/push/unsubscribe"), path)).toBe(false);
  expect(isPostTo(req("POST", "https://mixer.example.org/api/push/subscribe/x"), path)).toBe(false);
  expect(isPostTo(req("POST", "not a url"), path)).toBe(false);
});

test("a console line keeps its words and loses every URL", () => {
  expect(redacted("WebSocket connection to 'wss://mixer.example.org/ws/engineer?token=zyxqwvn' failed")).toBe(
    "WebSocket connection to '<url>' failed",
  );
  expect(redacted("[push] a: https://push.example.invalid/send/zyxqwvn b: http://10.0.0.10/x")).toBe(
    "[push] a: <url> b: <url>",
  );
  expect(redacted('fetch "HTTPS://Mixer.example.org/api/auth" failed')).toBe('fetch "<url>" failed');
  expect(redacted("[push] engineer subscribed to Web Push")).toBe("[push] engineer subscribed to Web Push");
});

/** The app's manifest and its icons, as the server serves them. */
const UI_FILES: Record<string, { file: string; type: string }> = {
  "/manifest.json": { file: "manifest.json", type: "application/json" },
  "/icon.svg": { file: "icon.svg", type: "image/svg+xml" },
  "/icon-192.png": { file: "icon-192.png", type: "image/png" },
  "/icon-512.png": { file: "icon-512.png", type: "image/png" },
};

test("storing the login leaves the next page no manifest icon download to cut (a console warning)", async () => {
  // Live run 1 (#10): the push spec's full Chromium warned "Error while trying
  // to use the following icon from the Manifest: … (Download error or
  // resource isn't a valid image)". Chromium's install check fetches the
  // manifest's icon when a page with a manifest has loaded; the next
  // navigation cuts that download once its answer has begun, and Chromium
  // logs the cut into the new page. Through the tunnel the answer is slow, so
  // the page the login is stored from must start no such download. Here an
  // icon's answer is held after its first bytes, as the tunnel's latency
  // holds it, until the next page is asked for.
  test.setTimeout(60_000);
  let target = false;
  let iconHeld!: () => void;
  const held = new Promise<void>((done) => (iconHeld = done));
  let iconServed!: () => void;
  let iconsServed = 0;
  const served = new Promise<void>((done) => (iconServed = done));
  const open: ServerResponse[] = [];
  const html = (title: string) =>
    `<!doctype html><html><head><title>${title}</title><link rel="manifest" href="/manifest.json"></head><body>${title}</body></html>`;
  const server = createServer((req, res) => {
    const path = req.url ?? "";
    const ui = UI_FILES[path];
    if (ui) {
      const body = readFileSync(resolve(__dirname, "../../crates/iem-ui", ui.file));
      res.writeHead(200, { "Content-Type": ui.type, "Content-Length": body.length });
      if (path !== "/manifest.json" && !target) {
        res.write(body.subarray(0, 64));
        open.push(res);
        iconHeld();
        return;
      }
      res.end(body, () => {
        if (path === "/manifest.json") return;
        iconsServed += 1;
        iconServed();
      });
      return;
    }
    if (path === "/api/version") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end('{"version":"2.0.0-dev.1"}');
      return;
    }
    if (path === "/engineer") target = true;
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(html(path === "/engineer" ? "engineer" : "start"));
  });
  await new Promise<void>((done) => server.listen(0, "127.0.0.1", done));
  const origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  const dir = mkdtempSync(join(tmpdir(), "iemmixer-login-"));
  // The push spec's browser: the full Chromium in a persistent profile (the headless shell runs no install check).
  const context = await chromium.launchPersistentContext(dir, { channel: "chromium" });
  try {
    const tab = context.pages()[0] ?? (await context.newPage());
    const problems = guardConsole(tab);
    // A stand-in, not a credential (the page only stores it).
    const auth = { token: ["zyxqwvn", "stand", "in"].join("."), member: "engineer", engineer: true };
    await storeLogin(tab, origin, auth);
    // As the tunnel's latency would: the next page is asked for while an icon's answer is under way, if one is.
    await Promise.race([held, new Promise((done) => setTimeout(done, 3_000))]);
    await tab.goto(`${origin}/engineer`);
    // The new page's own install check fetches its icon: wait for it, so a cut would have been logged.
    await Promise.race([served, new Promise((done) => setTimeout(done, 5_000))]);
    await tab.waitForTimeout(500);
    expect(await tab.evaluate(() => localStorage.getItem("iem_token"))).toBe(JSON.stringify(auth));
    expect(await tab.evaluate(() => sessionStorage.getItem("iem_redirected"))).toBe("1");
    // The check ran: the new page's install check fetched its icon whole (else the test would prove nothing).
    expect(iconsServed, "manifest icons served to the next page").toBeGreaterThanOrEqual(1);
    expect(problems).toEqual([]);
  } finally {
    for (const res of open) res.destroy();
    await context.close();
    server.close();
    rmSync(dir, { recursive: true, force: true });
  }
});
