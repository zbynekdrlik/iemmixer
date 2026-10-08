// Imported as NodeWebSocket, never WebSocket: see tests/support/wire.ts.
import NodeWebSocket from "ws";
import type { APIRequestContext } from "@playwright/test";
import { expectBuild, live } from "./env";

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
/** How long the watch's socket may take to open, and the server to answer ListenStart. */
const OPEN_MS = 15_000;

export type Status = { status: string; at: number };

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * A runner-side `/ws/audio?…&hil=1` socket listening to the engineer's mix:
 * it sees every burst's edges and frames. One socket, opened after the build
 * check; a server close makes every later call throw (no reconnect).
 */
export class BurstWatch {
  /** Every `AudioStatus` with its arrival time (ms). */
  readonly statuses: Status[] = [];
  private frames = 0;
  private bad = 0;
  /** When the current burst's `probe` came; null outside a burst. */
  private probeAt: number | null = null;
  private broken: string | null = null;
  private closed = false;

  private constructor(private readonly ws: NodeWebSocket) {
    ws.on("message", (d: Buffer, binary: boolean) => this.take(d, binary));
    ws.on("close", () => this.breaks("the server closed the burst watch's socket (no reconnect)"));
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
    const watch = new BurstWatch(new NodeWebSocket(url.toString(), { origin: baseURL }));
    await watch.opened();
    try {
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

  private opened(): Promise<void> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.close();
        reject(new Error(`the burst watch's socket did not open within ${OPEN_MS / 1000} s`));
      }, OPEN_MS);
      this.ws.once("open", () => {
        clearTimeout(timer);
        resolve();
      });
      this.ws.once("unexpected-response", (req, res) => {
        clearTimeout(timer);
        this.closed = true;
        res.resume();
        req.destroy();
        reject(new Error(`the burst watch's socket was refused: HTTP ${res.statusCode}`));
      });
      this.ws.once("error", () => {
        clearTimeout(timer);
        reject(new Error("the burst watch's socket failed to open"));
      });
    });
  }

  private breaks(why: string): void {
    if (!this.closed) this.broken ??= why;
  }

  private take(d: Buffer, binary: boolean): void {
    const now = Date.now();
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
      if (status === "probe") this.probeAt = now;
      if (status === "listening") this.probeAt = null;
      return;
    }
    if (this.probeAt === null) return;
    this.frames += 1;
    if (!celt20msStereo(new Uint8Array(d.buffer, d.byteOffset, d.byteLength))) this.bad += 1;
  }

  /** Throws when the socket broke or the watch was closed. */
  check(): void {
    if (this.broken) throw new Error(this.broken);
    if (this.closed) throw new Error("the burst watch is closed");
  }

  /** Inside a burst: from its `probe` until `listening`, at most `BURST_MS`. */
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
   * such a burst, else at the next `probe`; throws after `within` ms.
   */
  async burst({ minLeftMs = 22_000, within = 180_000 }: { minLeftMs?: number; within?: number } = {}): Promise<void> {
    if (minLeftMs > BURST_MS) throw new Error(`a burst never has ${minLeftMs} ms left (at most ${BURST_MS})`);
    const deadline = Date.now() + within;
    for (;;) {
      if (this.leftMs() >= minLeftMs) return;
      if (Date.now() > deadline) throw new Error(`no burst within ${within / 1000} s`);
      await sleep(50);
    }
  }

  /** Binary frames received inside bursts. */
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
