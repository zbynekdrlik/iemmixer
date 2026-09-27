import WebSocket from "ws";

/**
 * A second device on a mixer page: the page's socket (`/ws/<page>`, UI
 * protocol 2) opened from the test process. Specs use it to set the engine
 * up and to read the server's state back independently of the page under
 * test. The server handles one socket's commands in order, so a request's
 * answer comes after every command sent before it.
 */

/** One server event (`iem_core::ws::ServerMsg`). */
export type ServerEvent = { event: string; data?: unknown };

/** One EQ band as the server reports it (`iem_core::ws::EqBand`). */
export type EqBand = {
  band_type: string;
  freq_hz: number;
  gain_db: number;
  bw: number;
  enabled: boolean;
};

/** One channel of the page's state (`iem_core::Channel`). */
export type Channel = { id: string; name: string; level_db: number; pan: number; muted: boolean };

/** The page's state sent on connect (`iem_core::ws::ServerMsg::State`). */
export type PageState = {
  channels: Channel[];
  connected: boolean;
  /** The page mix's volume (IEM VOL); absent on a page without a mix. */
  global_level_db?: number;
  global_muted?: boolean;
};

/** One input on the engineer's console (`iem_core::ws::ConsoleInput`). */
export type ConsoleInput = { id: string; name: string; trim_db: number; muted: boolean; processing: boolean };

const WAIT_MS = 5_000;

export class MixerSocket {
  private readonly events: ServerEvent[] = [];
  private failure: string | null = null;
  private closed = false;

  private constructor(private readonly ws: WebSocket) {
    // Text frames arrive as a Buffer.
    ws.on("message", (data: { toString(): string }) => {
      this.events.push(JSON.parse(data.toString()) as ServerEvent);
    });
    ws.on("error", (e: Error) => {
      this.failure = e.message;
    });
    ws.on("close", (code: number) => {
      this.closed = true;
      this.failure ??= `closed with code ${code}`;
    });
  }

  /** Opens `/ws/<page>` with `token` and waits for the server's hello. */
  static async open(baseURL: string | undefined, page: string, token: string): Promise<MixerSocket> {
    if (!baseURL) throw new Error("baseURL is not set (playwright.config.ts use.baseURL)");
    const url = new URL(`/ws/${page}`, baseURL);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    url.searchParams.set("token", token);
    url.searchParams.set("proto", "2");
    const socket = new MixerSocket(new WebSocket(url.toString()));
    await socket.after(0, (m) => m.event === "Hello", "Hello");
    return socket;
  }

  send(cmd: Record<string, unknown>): void {
    this.ws.send(JSON.stringify(cmd));
  }

  /** Sends `cmd` and returns the first later event that `accept` takes. */
  async request(
    cmd: Record<string, unknown>,
    accept: (m: ServerEvent) => boolean,
    what: string,
  ): Promise<ServerEvent> {
    const from = this.events.length;
    this.send(cmd);
    return this.after(from, accept, what);
  }

  /** The bands of `target`'s EQ as the server holds them now. */
  async eq(target: string): Promise<EqBand[]> {
    const m = await this.request(
      { cmd: "GetEqParams", target },
      (e) => e.event === "EqParams" && (e.data as { target: string }).target === target,
      `EqParams of ${target}`,
    );
    return (m.data as { bands: EqBand[] }).bands;
  }

  /** Sets one EQ value (`param`: freq_hz, gain_db, bw_oct or enabled). */
  setEq(target: string, band: number, param: string, value: number): void {
    this.send({ cmd: "SetEqBand", target, band, param, value });
  }

  /** Puts every band of `target` back to `bands`; returns what the server then holds. */
  async restoreEq(target: string, bands: EqBand[]): Promise<EqBand[]> {
    bands.forEach((b, i) => {
      this.setEq(target, i, "freq_hz", b.freq_hz);
      this.setEq(target, i, "bw_oct", b.bw);
      this.setEq(target, i, "gain_db", b.gain_db);
      // Last: a gain change may enable a band (FG-2).
      this.setEq(target, i, "enabled", b.enabled ? 1 : 0);
    });
    return this.eq(target);
  }

  /** The page's state sent on connect. */
  async state(): Promise<PageState> {
    const m = await this.after(0, (e) => e.event === "State", "State");
    return m.data as PageState;
  }

  /** The channels of the page's state sent on connect. */
  async channels(): Promise<Channel[]> {
    return (await this.state()).channels;
  }

  /**
   * Waits until every command sent before it is in the engine: the server
   * runs a socket's commands one at a time, each until the engine applied it,
   * so the answer to a request sent last comes after all of them.
   */
  async applied(): Promise<void> {
    await this.limiterActiveSeconds();
  }

  /** The page mix's limiter counter (`LimiterParams.active_seconds`). */
  async limiterActiveSeconds(): Promise<number> {
    const m = await this.request({ cmd: "GetLimiterParams" }, (e) => e.event === "LimiterParams", "LimiterParams");
    return (m.data as { active_seconds: number }).active_seconds;
  }

  /** The engineer's console inputs (engineer sockets only). */
  async consoleInputs(): Promise<ConsoleInput[]> {
    const m = await this.request({ cmd: "GetConsole" }, (e) => e.event === "Console", "Console");
    return (m.data as { inputs: ConsoleInput[] }).inputs;
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.ws.close();
    const deadline = Date.now() + WAIT_MS;
    while (!this.closed && Date.now() < deadline) await sleep(10);
  }

  private async after(from: number, accept: (m: ServerEvent) => boolean, what: string): Promise<ServerEvent> {
    const deadline = Date.now() + WAIT_MS;
    for (;;) {
      const hit = this.events.slice(from).find(accept);
      if (hit) return hit;
      if (this.failure) throw new Error(`mixer socket ${this.failure} while waiting for ${what}`);
      if (Date.now() > deadline) throw new Error(`mixer socket: no ${what} within ${WAIT_MS} ms`);
      await sleep(10);
    }
  }
}

/** Runs `fn` with a socket on `page` and closes it afterwards. */
export async function withSocket<T>(
  baseURL: string | undefined,
  page: string,
  token: string,
  fn: (s: MixerSocket) => Promise<T>,
): Promise<T> {
  const socket = await MixerSocket.open(baseURL, page, token);
  try {
    return await fn(socket);
  } finally {
    await socket.close();
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
