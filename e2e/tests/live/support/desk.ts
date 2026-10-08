import type NodeWebSocket from "ws";
import { expect, type APIRequestContext } from "@playwright/test";
import type { Channel as PageChannel, ConsoleInput, EqBand } from "../../support/mixer-socket";
import type { BurstWatch } from "./burst";
import { expectBuild, live, type Who } from "./env";
import { liveSocket } from "./socket";

// The live specs' runner-side mixer sockets, and the one way a live spec
// changes the engine (S7, #10). The owner's rule (#9 2026-09-28): a live spec
// changes a mix or an input only inside a burst, while the engine holds every
// mix's TX at zero, and puts it back before the burst ends, checked against
// the server's `listening` status. `Desk.during` runs a spec's changing steps
// inside one burst; `Desk.change` refuses a change outside it, or with less
// than `RESTORE_MS` of it left, and keeps its undo (`Desk.track` keeps a
// page action's, e.g. Talk's release). The undos go back when the steps end,
// also when they fail, at once should the burst end first (a guard looks
// every 50 ms), and at the fixture's teardown should a timeout abandon the
// test body (a body's `finally` does not run then). Every command sent is in
// the engine before a socket closes: the server drops a closed socket's
// queued commands. A socket the server closes is a failure and is never
// opened again (the #10 decision of 2026-10-07): the changes it carried
// cannot go back then. No error here carries a URL, a token or a site value
// (P6): the page and input ids are site data. Restored values are the
// server's UI values (f32; a level at or below −60 dB comes back as off).

/** A changing step starts with at least this much of its burst left (Task 20). */
export const MIN_LEFT_MS = 22_000;
/** A change starts, and a wait inside the burst ends, at least this long before the burst does: the restore's time. */
export const RESTORE_MS = 2_000;
/** `during` finds its burst at the latest this long before the test's timeout: its steps and restores fit after it. */
const AFTER_BURST_MS = 75_000;
/** `during` waits at most this long for a burst (one comes every 60 s, a joined one is skipped). */
const BURST_WAIT_MS = 180_000;
/** How long a request waits for its answer. */
const ANSWER_MS = 5_000;
/** How often `during` looks whether its burst ended while changes are in place. */
const GUARD_MS = 50;
/** A page action's undo (Talk's release) may take this long; then the older undos go on without it. */
const ACT_MS = 2_000;
/** The desk's end waits this long at most for a restore already running (its acts, then a barrier per socket). */
const RUNNING_RESTORE_MS = 15_000;

/** `promise`, or a failure naming `what` after `ms`. */
async function limited(promise: Promise<void>, ms: number, what: string): Promise<void> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${what} did not end within ${ms / 1000} s`)), ms);
  });
  try {
    await Promise.race([promise, late]);
  } finally {
    clearTimeout(timer);
  }
}

/** One UI command (`iem_core::ws::ClientMsg`). */
export type Cmd = { cmd: string; [field: string]: unknown };

/** One channel of a page as its state and later updates show it (an update carries no name). */
export type Channel = Omit<PageChannel, "name">;

export type { ConsoleInput, EqBand };

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
 * and sends commands; one request at a time, so each answer is its own. Its
 * errors name it by `label`, never by its page.
 */
export class LiveMixer {
  private readonly events: Received[] = [];
  private broken: string | null = null;
  private closed = false;
  /** The last request in flight: the next one starts after it. */
  private queue: Promise<unknown> = Promise.resolve();

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

  /**
   * Sends `cmd` once every earlier request was answered, and returns the
   * data of the first later `event` that `accept` takes: with one request
   * in flight, that answer is this request's.
   */
  private request(cmd: Cmd, event: string, accept: (data: unknown) => boolean = () => true): Promise<unknown> {
    const run = async () => {
      const from = this.events.length;
      this.send(cmd);
      return (await this.after(from, (e) => e.event === event && accept(e.data), `${event} answer`)).data;
    };
    const answer = this.queue.then(run, run);
    this.queue = answer.catch(() => null);
    return answer;
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
    let channel: Channel | undefined = (state.data as { channels: Channel[] }).channels.find((c) => c.id === id);
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

/** What puts back one change: commands in their own order, or a page action (Talk's release). */
type Undo = { mixer: LiveMixer; cmds: Cmd[] } | { act: () => Promise<void> };

/**
 * A test's runner-side mixer sockets and the changes made with them: each
 * change only inside the burst `during` waited for, each undone, newest
 * first, before that burst ends. Should the burst end first (its
 * `listening`, or the watch's cap), a guard puts the changes back at once
 * and the steps fail.
 */
export class Desk {
  /** The open sockets, one per token and page (no reconnect). */
  private readonly mixers = new Map<string, LiveMixer>();
  private readonly undos: Undo[] = [];
  /** How many statuses the watch had when `during` entered its burst; null outside `during`. */
  private since: number | null = null;
  private guard: ReturnType<typeof setInterval> | null = null;
  /** Why the guard had to put changes back after the burst's end; null while it did not. */
  private late: string | null = null;
  /** The sockets the guard sent undos on, not yet known to be in the engine. */
  private readonly unsettled = new Set<LiveMixer>();
  /** The test is over: nothing more may change. */
  private ending = false;
  /** The restore running now (or the last one): the desk's end waits for it before it closes. */
  private restoring: Promise<void> = Promise.resolve();

  /**
   * `deadline` (ms, `Date.now()` time): when `during` must have found its
   * burst, so its steps and restores end before the test's timeout; null
   * for none.
   */
  constructor(
    private readonly request: APIRequestContext,
    private readonly watch: BurstWatch,
    private readonly deadline: number | null = null,
  ) {}

  /** The deadline for a test of `timeoutMs` (0: none) that started at `startedAt` (`Date.now()` time). */
  static deadline(timeoutMs: number, startedAt = Date.now()): number | null {
    return timeoutMs > 0 ? startedAt + timeoutMs - AFTER_BURST_MS : null;
  }

  /** Opens `page`'s socket with `who`'s token (`LiveMixer.open`), once per test; closed after it. */
  async open(who: Who, page: string): Promise<LiveMixer> {
    const key = `${who}\n${page}`;
    if (this.mixers.has(key)) throw new Error(`a second runner ${who} socket on one page (no reconnect)`);
    const mixer = await LiveMixer.open(this.request, who, page);
    this.mixers.set(key, mixer);
    return mixer;
  }

  /**
   * Waits for a burst with at least `minLeftMs` left and runs `body` in it;
   * every change `body` made goes back when it ends, also when it fails
   * (then a failed restore is reported beside the body's failure).
   */
  async during<T>(body: () => Promise<T>, minLeftMs = MIN_LEFT_MS): Promise<T> {
    if (this.since !== null) throw new Error("desk.during does not nest");
    if (this.ending) throw new Error("desk.during after the test's end");
    if (this.deadline === null) {
      await this.watch.burst({ minLeftMs, within: BURST_WAIT_MS });
    } else {
      const within = Math.min(BURST_WAIT_MS, this.deadline - Date.now());
      if (within <= 0) throw new Error("no time left in the test to wait for a burst");
      await this.watch.burst({ minLeftMs, within });
    }
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
    for (const undo of this.undos.splice(0).reverse()) {
      if ("act" in undo) {
        undo.act().catch(() => {
          this.late = "the burst ended with a page action in place, and its undo failed";
        });
        continue;
      }
      for (const cmd of undo.cmds) {
        try {
          undo.mixer.send(cmd);
        } catch {
          unsent = true;
        }
      }
      // `restore` and `end` wait until the engine has them.
      this.unsettled.add(undo.mixer);
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
    if (this.ending) throw new Error(`${what} after the test's end`);
    if (this.since === null) throw new Error(`${what} outside desk.during`);
    if (!this.burstOn()) throw new Error(`${what} came after the burst's end`);
  }

  /** Throws unless `during`'s burst runs with at least `ms` of it left. */
  need(ms: number, what: string): void {
    this.inside(what);
    const left = this.watch.leftMs();
    if (left < ms) {
      throw new Error(`${what} needs ${(ms / 1000).toFixed(1)} s of the burst; ${(left / 1000).toFixed(1)} s are left`);
    }
  }

  /**
   * `ms`, cut to the time left in the burst less `RESTORE_MS`: a wait inside
   * `during` never runs into the restore's time (at least 1 ms: a timeout of
   * 0 is none at all).
   */
  bound(ms: number): number {
    return Math.max(1, Math.min(ms, this.watch.leftMs() - RESTORE_MS));
  }

  /**
   * Sends `cmds` on `mixer`, only inside the burst with `RESTORE_MS` of it
   * left; `undo` puts them back, in its own order (an EQ band's gain switches
   * the band on, so its switch goes back last). The undo is kept before the send.
   */
  change(mixer: LiveMixer, cmds: Cmd | Cmd[], undo: Cmd | Cmd[]): void {
    this.need(RESTORE_MS, "a change");
    this.undos.push({ mixer, cmds: ([] as Cmd[]).concat(undo) });
    for (const cmd of ([] as Cmd[]).concat(cmds)) mixer.send(cmd);
  }

  /**
   * Keeps `act` as the undo of a change a page makes (Talk held): it runs with
   * the other undos, newest first, and at once from the guard. It may run more
   * than once (a step that already undid it, then the restore): it must be
   * idempotent. Call it before the change.
   */
  track(what: string, act: () => Promise<void>): void {
    this.need(RESTORE_MS, what);
    this.undos.push({ act });
  }

  /** How many changes are in place: `restore(mark)` undoes the later ones. */
  mark(): number {
    return this.undos.length;
  }

  /**
   * Puts back every change since `from` (default: all), newest first, waits
   * until the engine has them (and what the guard sent), and checks they
   * landed inside the burst. A page action's undo gets `ACT_MS`, then the
   * older undos go on without it. A socket that broke cannot put its changes
   * back: the first such failure is thrown after the others went back.
   */
  restore(from = 0): Promise<void> {
    const run = this.restoreNow(from);
    this.restoring = run.catch(() => undefined);
    return run;
  }

  private async restoreNow(from: number): Promise<void> {
    const pending = this.undos.splice(from).reverse();
    const sent = new Set<LiveMixer>(this.unsettled);
    this.unsettled.clear();
    let failure: unknown = null;
    for (const undo of pending) {
      if ("act" in undo) {
        try {
          await limited(undo.act(), ACT_MS, "a page action's undo");
        } catch (e) {
          failure ??= e;
        }
        continue;
      }
      for (const cmd of undo.cmds) {
        try {
          undo.mixer.send(cmd);
        } catch (e) {
          failure ??= e;
        }
      }
      sent.add(undo.mixer);
    }
    for (const mixer of sent) {
      try {
        await mixer.applied();
      } catch (e) {
        failure ??= e;
      }
    }
    if (failure !== null) throw failure;
    if (pending.length > 0 && !this.burstOn()) throw new Error("the restore came after the burst's end");
  }

  /**
   * The test is over: nothing more may change; a restore a timed-out body
   * left running ends first (it holds undos already taken), then what the
   * body left goes back; every socket's commands are in the engine before it
   * closes (the server drops a closed socket's queued ones); a socket the
   * server closed fails the test.
   */
  async end(): Promise<void> {
    this.ending = true;
    let failure: unknown = null;
    try {
      await limited(this.restoring, RUNNING_RESTORE_MS, "the restore running at the test's end");
    } catch (e) {
      failure = e;
    }
    try {
      await this.restore();
    } catch (e) {
      failure ??= e;
    }
    this.stop();
    for (const mixer of this.mixers.values()) {
      try {
        mixer.check();
        await mixer.applied();
      } catch (e) {
        failure ??= e;
      }
      mixer.close();
    }
    this.mixers.clear();
    if (failure !== null) throw failure;
  }
}
