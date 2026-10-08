import { EventEmitter } from "node:events";
import { test, expect } from "./support/fixtures";
import { BurstWatch } from "./live/support/burst";

// The live specs' support (S7, #10), run in the mock E2E job: the live specs
// themselves run only from the ops live run, against the real PC. The burst
// watch runs here on a local stand-in for the public host's socket (the
// burst's edges and the frames are fed in). No page is used.

/** What a stand-in socket was sent. */
type Sent = { cmd: string; [field: string]: unknown };

/** A socket stand-in: records what is sent, answers GetLimiterParams (the desk's barrier), feeds server events in. */
class FakeSocket extends EventEmitter {
  readonly sent: Sent[] = [];

  send(data: string): void {
    const sent = JSON.parse(data) as Sent;
    this.sent.push(sent);
    if (sent.cmd === "GetLimiterParams") {
      const answer = { mix: "member1", limit_db: -6, limit_norm: 0, enabled: true, active_seconds: 0 };
      setTimeout(() => this.event("LimiterParams", answer), 1);
    }
  }

  /** A server event (a text frame). */
  event(event: string, data: unknown): void {
    this.emit("message", Buffer.from(JSON.stringify({ event, data })), false);
  }

  close(): void {
    this.emit("closed-by-client");
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

/** A real BurstWatch on a stand-in socket, with functions that feed it an AudioStatus and a listen frame. */
function rawWatch(): { watch: BurstWatch; status: (status: string) => void; frame: () => void } {
  const socket = new FakeSocket();
  const watch = new (BurstWatch as unknown as new (ws: unknown) => BurstWatch)(socket);
  return {
    watch,
    status: (status) => socket.event("AudioStatus", { status }),
    frame: () => socket.emit("message", FRAME, true),
  };
}

test("the watch counts a burst only when it saw the burst begin", async () => {
  const { watch, status, frame } = rawWatch();
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
  frame();
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
