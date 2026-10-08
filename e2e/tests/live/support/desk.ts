import type NodeWebSocket from "ws";
import { expect, type APIRequestContext } from "@playwright/test";
import type { BurstWatch } from "./burst";
import { expectBuild, live, type Who } from "./env";
import { liveSocket } from "./socket";

// The live specs' runner-side mixer sockets, and the one way a live spec
// changes the engine (S7, #10). The owner's rule (#9 2026-09-28): a live spec
// changes a mix or an input only inside a burst, while the engine holds every
// mix's TX at zero, and puts it back before the burst ends, checked against
// the server's `listening` status. `Desk.during` runs a spec's changing steps
// inside one burst; `Desk.change` refuses a change outside it and keeps its
// undo, and the undos go back when the steps end, also when they fail, at
// once should the burst end first (a guard looks every 50 ms), and at the
// fixture's teardown should a timeout abandon the test body (a body's
// `finally` does not run then). A socket the server closes is a failure and is
// never opened again (the #10 decision of 2026-10-07). No error here carries a
// URL, a token or a site value (P6): the page and input ids are site data.

/** A changing step starts with at least this much of its burst left (Task 20). */
export const MIN_LEFT_MS = 22_000;
/** How long a request waits for its answer. */
const ANSWER_MS = 5_000;

/** One UI command (`iem_core::ws::ClientMsg`). */
export type Cmd = { cmd: string; [field: string]: unknown };

/** One channel of a page, as its state and later updates show it (`iem_core::Channel`). */
export type Channel = { id: string; level_db: number; muted: boolean; pan: number };

/** One input on the engineer's console (`iem_core::ws::ConsoleInput`). */
export type ConsoleInput = { id: string; trim_db: number; muted: boolean; processing: boolean };

/** One EQ band as the server reports it (`iem_core::ws::EqBand`). */
export type EqBand = { band_type: string; freq_hz: number; gain_db: number; bw: number; enabled: boolean };

/** The page mix's limiter (`ServerMsg::LimiterParams`). */
export type LimiterParams = {
  mix: string;
  limit_db: number;
  limit_norm: number;
  enabled: boolean;
  active_seconds: number;
};

type Received = { event: string; data: unknown; at: number };

/** Resolves after `ms` (at once for 0 or less). */
export const pause = (ms: number): Promise<void> =>
  ms > 0 ? new Promise((resolve) => setTimeout(resolve, ms)) : Promise.resolve();

/**
 * A mixer page's socket (`/ws/<page>`, UI protocol 2) held by the runner: it
 * opens after the build check, records every JSON event with its arrival time
 * and sends commands. Its errors name it by `label`, never by its page.
 */
export class LiveMixer {
  private readonly events: Received[] = [];
  private broken: string | null = null;
  private closed = false;

  private constructor(
    private readonly ws: NodeWebSocket,
    readonly label: string,
  ) {
    ws.on("message", (d: Buffer, binary: boolean) => {
      if (!binary) this.take(d);
    });
    ws.on("close", () => this.breaks(`the server closed ${label} (no reconnect)`));
    // The error's own text can name the host: fixed words only.
    ws.on("error", () => this.breaks(`${label} failed (socket error)`));
  }

  /** Opens `page`'s socket with `who`'s token of the run, after the build check; resolves with the page's state. */
  static async open(request: APIRequestContext, who: Who, page: string): Promise<LiveMixer> {
    const { baseURL, tokens } = live();
    const label = `the runner's ${who} socket on ${page === "engineer" ? "the engineer's" : "a member's"} page`;
    await expectBuild(request);
    const url = new URL(`/ws/${encodeURIComponent(page)}`, baseURL);
    url.protocol = "wss:";
    url.searchParams.set("token", tokens[who]);
    url.searchParams.set("proto", "2");
    const { ws, opened } = liveSocket(url.toString(), label);
    // Listening before the open: the hello and the state follow the upgrade at once.
    const mixer = new LiveMixer(ws, label);
    try {
      await opened;
      await mixer.after(0, (e) => e.event === "State", "its state");
    } catch (e) {
      mixer.close();
      throw e;
    }
    return mixer;
  }

  private breaks(why: string): void {
    if (!this.closed) this.broken ??= why;
  }

  private take(d: Buffer): void {
    let msg: { event?: unknown; data?: unknown };
    try {
      msg = JSON.parse(d.toString());
    } catch {
      this.breaks(`${this.label} got a text frame that is not JSON`);
      return;
    }
    if (typeof msg?.event === "string") this.events.push({ event: msg.event, data: msg.data, at: Date.now() });
  }

  /** Throws when the socket broke or was closed. */
  check(): void {
    if (this.broken) throw new Error(this.broken);
    if (this.closed) throw new Error(`${this.label} is closed`);
  }

  send(cmd: Cmd): void {
    this.check();
    this.ws.send(JSON.stringify(cmd));
  }

  /** The first event at or after `from` that `accept` takes; `what` names it in a failure. */
  private async after(from: number, accept: (e: Received) => boolean, what: string): Promise<Received> {
    const deadline = Date.now() + ANSWER_MS;
    for (;;) {
      const hit = this.events.slice(from).find(accept);
      if (hit) return hit;
      this.check();
      if (Date.now() > deadline) throw new Error(`${this.label}: no ${what} within ${ANSWER_MS / 1000} s`);
      await pause(10);
    }
  }

  /** Sends `cmd` and returns the data of the first later `event` that `accept` takes. */
  private async request(cmd: Cmd, event: string, accept: (data: unknown) => boolean = () => true): Promise<unknown> {
    const from = this.events.length;
    this.send(cmd);
    return (await this.after(from, (e) => e.event === event && accept(e.data), `${event} answer`)).data;
  }

  /**
   * Waits until every command sent before it is in the engine: the server
   * runs a socket's commands one at a time, each until the engine applied it,
   * so the answer to a request sent last comes after all of them.
   */
  async applied(): Promise<void> {
    await this.limiter();
  }

  /** The page mix's limiter, fresh from the server. */
  async limiter(): Promise<LimiterParams> {
    return (await this.request({ cmd: "GetLimiterParams" }, "LimiterParams")) as LimiterParams;
  }

  /** The page mix's limiter counter (X14: seconds of gain reduction below −1 dB since its reset). */
  async activeSeconds(): Promise<number> {
    return (await this.limiter()).active_seconds;
  }

  /** Input `id` on the engineer's console, fresh from the server (engineer tokens only); undefined when it has none. */
  async consoleInput(id: string): Promise<ConsoleInput | undefined> {
    const data = (await this.request({ cmd: "GetConsole" }, "Console")) as { inputs: ConsoleInput[] };
    return data.inputs.find((i) => i.id === id);
  }

  /** The bands of `target`'s EQ, fresh from the server. */
  async eq(target: string): Promise<EqBand[]> {
    const data = (await this.request(
      { cmd: "GetEqParams", target },
      "EqParams",
      (d) => (d as { target?: unknown }).target === target,
    )) as { bands: EqBand[] };
    return data.bands;
  }

  /** The data of the newest `event`; undefined before the first. */
  private newest(event: string): { data: unknown; index: number } | undefined {
    for (let i = this.events.length - 1; i >= 0; i--) {
      if (this.events[i].event === event) return { data: this.events[i].data, index: i };
    }
    return undefined;
  }

  /** The page's mix id (its EQ target and limiter), from its state. */
  mixId(): string {
    const mix = (this.newest("State")?.data as { mix?: unknown } | undefined)?.mix;
    if (typeof mix !== "string") throw new Error(`${this.label}: the page's state names no mix`);
    return mix;
  }

  /**
   * Channel `id` of the page as other sessions left it: the newest state with
   * the channel updates after it (the server echoes no session's own change).
   * Its `muted` is the level's own mute or a solo's mask.
   */
  channel(id: string): Channel | undefined {
    const state = this.newest("State");
    if (!state) return undefined;
    let channel = (state.data as { channels: Channel[] }).channels.find((c) => c.id === id);
    if (!channel) return undefined;
    for (const e of this.events.slice(state.index + 1)) {
      if (e.event !== "ChannelUpdate") continue;
      const update = e.data as Channel;
      if (update.id === id) channel = update;
    }
    const { level_db, muted, pan } = channel;
    return { id, level_db, muted, pan };
  }

  /** The page mix's soloed channels (the newest `SoloUpdate`, sent on connect). */
  soloed(): string[] {
    const solo = this.newest("SoloUpdate")?.data as { soloed?: unknown } | undefined;
    if (!Array.isArray(solo?.soloed)) throw new Error(`${this.label}: no solo state`);
    return solo.soloed as string[];
  }

  /** The `Meters` frames that arrived in `[from, to)` (ms). */
  meterFrames(from: number, to = Infinity): number {
    return this.events.filter((e) => e.event === "Meters" && e.at >= from && e.at < to).length;
  }

  /** The peaks (linear, the louder side) of `id` in the `Meters` frames that arrived in `[from, to)` (ms). */
  peaks(id: string, from: number, to = Infinity): number[] {
    const out: number[] = [];
    for (const e of this.events) {
      if (e.event !== "Meters" || e.at < from || e.at >= to) continue;
      const meter = (e.data as { meters: Record<string, [number, number]> }).meters[id];
      if (meter) out.push(Math.max(meter[0], meter[1]));
    }
    return out;
  }

  /** Closes the socket (the test is over); every later call throws. */
  close(): void {
    this.closed = true;
    this.ws.close();
  }
}

/** The commands that put back one change, in their own order. */
type Undo = { mixer: LiveMixer; cmds: Cmd[] };

/** How often `during` looks whether its burst ended while changes are in place. */
const GUARD_MS = 50;

/**
 * A test's runner-side mixer sockets and the changes made with them: each
 * change only inside the burst `during` waited for, each undone, newest
 * first, before that burst ends. Should the burst end first (its
 * `listening`, or the watch's cap), a guard puts the changes back at once
 * and the steps fail.
 */
export class Desk {
  private readonly mixers: LiveMixer[] = [];
  private readonly undos: Undo[] = [];
  /** How many statuses the watch had when `during` entered its burst; null outside `during`. */
  private since: number | null = null;
  private guard: ReturnType<typeof setInterval> | null = null;
  /** Why the guard had to put changes back after the burst's end; null while it did not. */
  private late: string | null = null;

  constructor(
    private readonly request: APIRequestContext,
    private readonly watch: BurstWatch,
  ) {}

  /** Opens `page`'s socket with `who`'s token (`LiveMixer.open`); closed after the test. */
  async open(who: Who, page: string): Promise<LiveMixer> {
    const mixer = await LiveMixer.open(this.request, who, page);
    this.mixers.push(mixer);
    return mixer;
  }

  /**
   * Waits for a burst with at least `minLeftMs` left and runs `body` in it;
   * every change `body` made goes back when it ends, also when it fails
   * (then a failed restore is reported beside the body's failure).
   */
  async during<T>(body: () => Promise<T>, minLeftMs = MIN_LEFT_MS): Promise<T> {
    if (this.since !== null) throw new Error("desk.during does not nest");
    await this.watch.burst({ minLeftMs });
    this.since = this.watch.statuses.length;
    this.late = null;
    this.guard = setInterval(() => this.guardBurst(), GUARD_MS);
    let result: T;
    try {
      result = await body();
    } catch (e) {
      const problem = await this.restore().then(
        () => this.late,
        (r: Error) => r.message,
      );
      expect.soft(problem, "the restore after the failure").toBeNull();
      this.stop();
      throw e;
    }
    try {
      await this.restore();
      if (this.late !== null) throw new Error(this.late);
    } finally {
      this.stop();
    }
    return result;
  }

  /** Whether `during`'s burst still runs: no `listening` since it was entered (its end), and the watch still in it. */
  private burstOn(): boolean {
    const since = this.since;
    if (since === null) return false;
    const ended = this.watch.statuses.slice(since).some((s) => s.status === "listening");
    return !ended && this.watch.inBurst();
  }

  /** The guard's look: the burst is over with changes in place, so they go back now, not at the steps' next look. */
  private guardBurst(): void {
    if (this.undos.length === 0) return;
    let on: boolean;
    try {
      on = this.burstOn();
    } catch {
      // A broken watch: its burst can no longer be trusted.
      on = false;
    }
    if (on) return;
    let unsent = false;
    for (const { mixer, cmds } of this.undos.splice(0).reverse()) {
      for (const cmd of cmds) {
        try {
          mixer.send(cmd);
        } catch {
          unsent = true;
        }
      }
    }
    this.late ??= unsent
      ? "the burst ended with changes in place, and a broken socket could not put its change back"
      : "the burst ended with changes in place: they went back after its end";
  }

  private stop(): void {
    if (this.guard !== null) clearInterval(this.guard);
    this.guard = null;
    this.since = null;
  }

  /** Throws unless `during` is running and its burst still is. */
  inside(what: string): void {
    if (this.since === null) throw new Error(`${what} outside desk.during`);
    if (!this.burstOn()) throw new Error(`${what} came after the burst's end`);
  }

  /**
   * `ms`, cut to the time left in the burst: a wait inside `during` never
   * outlasts it (at least 1 ms: a timeout of 0 is none at all).
   */
  bound(ms: number): number {
    return Math.max(1, Math.min(ms, this.watch.leftMs()));
  }

  /**
   * Sends `cmds` on `mixer`, only inside the burst; `undo` puts them back, in
   * its own order (an EQ band's gain switches the band on, so its switch goes
   * back last). The undo is kept before the send.
   */
  change(mixer: LiveMixer, cmds: Cmd | Cmd[], undo: Cmd | Cmd[]): void {
    this.inside("a change");
    this.undos.push({ mixer, cmds: ([] as Cmd[]).concat(undo) });
    for (const cmd of ([] as Cmd[]).concat(cmds)) mixer.send(cmd);
  }

  /** How many changes are in place: `restore(mark)` undoes the later ones. */
  mark(): number {
    return this.undos.length;
  }

  /**
   * Puts back every change since `from` (default: all), newest first, waits
   * until the engine has them, and checks they landed inside the burst. A
   * socket that broke cannot put its changes back: the first such failure is
   * thrown after the others went back.
   */
  async restore(from = 0): Promise<void> {
    const pending = this.undos.splice(from).reverse();
    if (pending.length === 0) return;
    let failure: unknown = null;
    for (const { mixer, cmds } of pending) {
      for (const cmd of cmds) {
        try {
          mixer.send(cmd);
        } catch (e) {
          failure ??= e;
        }
      }
    }
    for (const mixer of new Set(pending.map((u) => u.mixer))) {
      try {
        await mixer.applied();
      } catch (e) {
        failure ??= e;
      }
    }
    if (failure !== null) throw failure;
    this.inside("the restore");
  }

  /** The test is over: puts back what a timed-out body left, then checks and closes every socket. */
  async end(): Promise<void> {
    let failure: unknown = null;
    try {
      await this.restore();
    } catch (e) {
      failure = e;
    }
    this.stop();
    for (const mixer of this.mixers) {
      if (failure === null) {
        try {
          mixer.check();
        } catch (e) {
          failure = e;
        }
      }
      mixer.close();
    }
    this.mixers.length = 0;
    if (failure !== null) throw failure;
  }
}
