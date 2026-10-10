import type NodeWebSocket from "ws";
import type { APIRequestContext } from "@playwright/test";
import { expectBuild, live } from "./env";
import { OPEN_MS, Wire, liveSocket } from "./socket";

// The bursts (S7, #10): while the browser job runs, the PC fires the HIL test
// signal with the listen probe in bursts of 30 s every 60 s. The server tells
// a `&hil=1` listen socket a burst's edges with `AudioStatus` `probe` (its
// first probe frame) and `listening` (the first own frame after it). The live
// specs measure only inside a burst, watched from the runner by `BurstWatch`.

/** An Opus packet of one frame: the TOC byte and 1 to 1275 bytes (RFC 6716 §3.2.2). */
const MIN_PACKET = 2;
const MAX_PACKET = 1276;

/**
 * Whether `packet` is one CELT frame of 20 ms in stereo, by its TOC byte
 * (RFC 6716 §3.1): config ≥ 16 (CELT only), config % 4 == 3 (20 ms), the s
 * bit set (stereo), c = 0 (one frame), and 2 to 1276 bytes in all.
 */
export function celt20msStereo(packet: Uint8Array): boolean {
  if (packet.length < MIN_PACKET || packet.length > MAX_PACKET) return false;
  const toc = packet[0];
  const config = toc >> 3;
  const stereo = (toc & 0b100) !== 0;
  const code = toc & 0b11;
  return config >= 16 && config % 4 === 3 && stereo && code === 0;
}

/** `inBurst()` ends at the latest this long after a burst's `probe` (a burst is 30 s). */
export const BURST_MS = 28_000;
/**
 * A `probe` begins a burst for the watch only after this many of the slot's
 * own frames since the last `listening`: 5 s of 20 ms frames. Bursts are 30 s
 * apart; a stall of the probe frames inside a burst gives one or a few.
 */
export const OWN_FRAMES_BEFORE_A_BURST = 250;

export type Status = { status: string; at: number };

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The statuses a failure counts by name; any other is counted as `other` (the server's words, never printed). */
const KNOWN_STATUSES = ["listening", "probe", "no_source", "stopped"] as const;

/** `ms` in seconds with `digits` after the point. */
const seconds = (ms: number, digits: number): string => (ms / 1000).toFixed(digits);

/** A status as a failure may name it: one of `KNOWN_STATUSES`, else `other`. */
const statusName = (status: string): string => ((KNOWN_STATUSES as readonly string[]).includes(status) ? status : "other");

/**
 * A runner-side `/ws/audio?…&hil=1` socket listening to the engineer's mix:
 * it sees every burst's edges and frames. One socket, opened after the build
 * check; a server close makes every later call throw (no reconnect).
 *
 * A burst counts only when the watch saw it begin: 5 s of the slot's own
 * frames came after the last `listening` (the ListenStart answer or a
 * burst's end) and before its `probe`. The server's probe gate is per
 * session, so a watch opened in the middle of a burst gets `probe` with its
 * first frame, and a stall of the probe frames over the gate's 100 ms hold
 * gives `listening`, an own frame and `probe` again in the middle of one:
 * neither tells how much of the burst is left, and the watch waits for the
 * next one.
 */
export class BurstWatch {
  /** Every `AudioStatus` with its arrival time (ms). */
  readonly statuses: Status[] = [];
  private frames = 0;
  private bad = 0;
  /** Every binary and text frame the socket got (a failure names them). */
  private binaries = 0;
  private texts = 0;
  /** When the socket opened and last got a message (`closeFacts`). */
  private readonly wire: Wire;
  /** When the current burst's `probe` came; null outside a burst it saw begin. */
  private probeAt: number | null = null;
  /** The slot's own frames since the last `listening`. */
  private ownFrames = 0;
  /** Inside a burst the watch did not see begin (opened in its middle, or after a stall). */
  private joined = false;
  private broken: string | null = null;
  private closed = false;

  private constructor(private readonly ws: NodeWebSocket) {
    this.wire = new Wire(ws);
    ws.on("message", (d: Buffer, binary: boolean) => this.take(d, binary));
    // "The server" is the far side: the server, or the tunnel between (live
    // run 3, #10). The code, the time and the last status tell them apart
    // against the PC's server.log.
    ws.on("close", (code: unknown) =>
      this.breaks(`the server closed the burst watch's socket (no reconnect): ${this.wire.closed(code)}; ${this.lastStatus()}`),
    );
    // The error's own text can name the host: fixed words only.
    ws.on("error", () => this.breaks("the burst watch's socket failed"));
  }

  /** Opens the watch with `token` (default: the run's engineer token) and starts listening. */
  static async open(request: APIRequestContext, token?: string): Promise<BurstWatch> {
    const { baseURL, tokens } = live();
    await expectBuild(request);
    const url = new URL("/ws/audio", baseURL);
    url.protocol = "wss:";
    url.searchParams.set("token", token ?? tokens.engineer);
    url.searchParams.set("hil", "1");
    const { ws, opened } = liveSocket(url.toString(), "the burst watch's socket");
    // Listening before the open: nothing the server sends first is missed.
    const watch = new BurstWatch(ws);
    try {
      await opened;
      watch.ws.send(JSON.stringify({ cmd: "ListenStart", member_id: "engineer" }));
      const deadline = Date.now() + OPEN_MS;
      for (;;) {
        watch.check();
        const answer = watch.statuses.find((s) => s.status === "listening" || s.status === "no_source");
        if (answer?.status === "listening") return watch;
        if (answer) throw new Error("the burst watch's ListenStart got no_source");
        if (Date.now() > deadline) throw new Error(`the burst watch's ListenStart got no answer within ${OPEN_MS / 1000} s`);
        await sleep(20);
      }
    } catch (e) {
      watch.close();
      throw e;
    }
  }

  private breaks(why: string): void {
    if (!this.closed) this.broken ??= why;
  }

  /** The newest status and how long before now it came, for a failure. */
  private lastStatus(now = Date.now()): string {
    const last = this.statuses[this.statuses.length - 1];
    return last ? `the last status ${statusName(last.status)} ${seconds(now - last.at, 1)} s before` : "no status before";
  }

  /**
   * The watch's own account, for a failure: what the socket got, the
   * statuses by name, the slot's own frames since the last `listening`, and
   * where it stands. Numbers and fixed words only.
   */
  private account(now = Date.now()): string {
    const last = this.wire.lastAt === null ? "none yet" : `the last ${seconds(now - this.wire.lastAt, 2)} s ago`;
    const counts = [...KNOWN_STATUSES, "other"]
      .map((name) => `${name} ${this.statuses.filter((s) => statusName(s.status) === name).length}`)
      .join(", ");
    const where = this.joined
      ? "inside a burst it did not see begin"
      : this.probeAt !== null
        ? `inside a burst begun ${seconds(now - this.probeAt, 1)} s ago`
        : "outside a burst";
    return `the watch: ${this.binaries} binary and ${this.texts} text frames, ${last}; statuses ${counts}; ${this.ownFrames} own frames since the last listening; ${where}`;
  }

  private take(d: Buffer, binary: boolean): void {
    const now = Date.now();
    if (binary) this.binaries += 1;
    else this.texts += 1;
    if (!binary) {
      let msg: { event?: unknown; data?: { status?: unknown } };
      try {
        msg = JSON.parse(d.toString());
      } catch {
        this.breaks("the burst watch got a text frame that is not JSON");
        return;
      }
      if (msg?.event !== "AudioStatus" || typeof msg.data?.status !== "string") return;
      const status = msg.data.status;
      this.statuses.push({ status, at: now });
      if (status === "probe") {
        if (this.ownFrames >= OWN_FRAMES_BEFORE_A_BURST) this.probeAt = now;
        else this.joined = true;
      }
      if (status === "listening") {
        this.probeAt = null;
        this.joined = false;
        this.ownFrames = 0;
      }
      return;
    }
    // A joined burst's frames are probe frames too: neither the slot's own nor counted.
    if (this.joined) return;
    if (this.probeAt === null) {
      this.ownFrames += 1;
      return;
    }
    this.frames += 1;
    if (!celt20msStereo(new Uint8Array(d.buffer, d.byteOffset, d.byteLength))) this.bad += 1;
  }

  /** Throws when the socket broke or the watch was closed. */
  check(): void {
    if (this.broken) throw new Error(this.broken);
    if (this.closed) throw new Error("the burst watch is closed");
  }

  /** Inside a burst the watch saw begin: from its `probe` until `listening`, at most `BURST_MS`. */
  inBurst(): boolean {
    this.check();
    return this.probeAt !== null && Date.now() - this.probeAt < BURST_MS;
  }

  /** How long the current burst still counts (0 outside one). */
  leftMs(): number {
    return this.inBurst() && this.probeAt !== null ? this.probeAt + BURST_MS - Date.now() : 0;
  }

  /**
   * Resolves inside a burst with at least `minLeftMs` of it left: at once in
   * such a burst, else at the next `probe` the watch sees begin; throws
   * after `within` ms.
   */
  async burst({ minLeftMs = 22_000, within = 180_000 }: { minLeftMs?: number; within?: number } = {}): Promise<void> {
    if (minLeftMs > BURST_MS) throw new Error(`a burst never has ${minLeftMs} ms left (at most ${BURST_MS})`);
    const deadline = Date.now() + within;
    for (;;) {
      if (this.leftMs() >= minLeftMs) return;
      if (Date.now() > deadline) throw new Error(`no burst within ${within / 1000} s (${this.account()})`);
      await sleep(50);
    }
  }

  /** Binary frames received inside the bursts the watch saw begin. */
  get burstFrames(): number {
    this.check();
    return this.frames;
  }

  /** Of those, the frames that fail `celt20msStereo`. */
  get badFrames(): number {
    this.check();
    return this.bad;
  }

  /** Closes the socket (the test is over); every later call throws. */
  close(): void {
    this.closed = true;
    this.ws.close();
  }
}
