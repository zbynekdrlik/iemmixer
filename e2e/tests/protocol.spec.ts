import { test, expect } from "./support/fixtures";
import { login } from "./support/session";

// The UI protocol v2 on the wire (program spec §5.3): the hello handshake,
// the reload close for a page without a protocol, the talk-id binding of
// the talkback socket (X6), listening over /ws/audio, and the X11 limiter.

test.describe("Mixer socket handshake (§5.3)", () => {
  test("the first message is the server's hello", async ({ page }) => {
    await page.goto("/");
    const auth = await login(page, "member2");
    const first = await page.evaluate(async (token) => {
      const ws = new WebSocket(`ws://${location.host}/ws/member2?token=${token}&proto=2`);
      const msg = await new Promise<string>((resolve, reject) => {
        ws.onmessage = (e) => resolve(String(e.data));
        ws.onerror = () => reject(new Error("socket error"));
        setTimeout(() => reject(new Error("no message")), 5000);
      });
      ws.close();
      return JSON.parse(msg);
    }, auth.token);
    expect(first.event).toBe("Hello");
    expect(first.data.proto).toBe(2);
    expect(first.data.min_client_proto).toBeLessThanOrEqual(2);
    expect(typeof first.data.build).toBe("string");
  });

  test("a page that names no protocol is closed with the reload code", async ({ page }) => {
    await page.goto("/");
    const auth = await login(page, "member2");
    const code = await page.evaluate(async (token) => {
      const ws = new WebSocket(`ws://${location.host}/ws/member2?token=${token}`);
      return await new Promise<number>((resolve, reject) => {
        ws.onclose = (e) => resolve(e.code);
        setTimeout(() => reject(new Error("not closed")), 5000);
      });
    }, auth.token);
    expect(code).toBe(4001);
  });
});

test.describe("Talkback binding (X6)", () => {
  // The refused upgrade is the point of the first check; Chrome logs it.
  test.use({ allowedConsole: [/^WebSocket connection to '.*\/ws\/talkback\?.*' failed/] });

  test("a talkback socket without the held talk id is refused; the held id binds", async ({
    page,
  }) => {
    await page.goto("/");
    const auth = await login(page, "engineer", true);
    const result = await page.evaluate(async (token) => {
      const opens = (url: string) =>
        new Promise<boolean>((resolve) => {
          const ws = new WebSocket(url);
          ws.onopen = () => {
            ws.close();
            resolve(true);
          };
          ws.onerror = () => resolve(false);
          setTimeout(() => resolve(false), 4000);
        });
      const base = `ws://${location.host}`;
      const without = await opens(`${base}/ws/talkback?token=${token}`);

      const mixer = new WebSocket(`${base}/ws/engineer?token=${token}&proto=2`);
      const talkId = await new Promise<string>((resolve, reject) => {
        mixer.onmessage = (e) => {
          const m = JSON.parse(String(e.data));
          if (m.event === "Hello") mixer.send(JSON.stringify({ cmd: "TalkStart" }));
          if (m.event === "TalkAcquired") resolve(m.data.talk_id);
          if (m.event === "TalkBusy") reject(new Error("talk busy"));
        };
        setTimeout(() => reject(new Error("no TalkAcquired")), 5000);
      });
      const withId = await opens(`${base}/ws/talkback?token=${token}&talk=${talkId}`);
      mixer.send(JSON.stringify({ cmd: "TalkStop" }));
      mixer.close();
      return { without, withId, idLength: talkId.length };
    }, auth.token);
    expect(result.without).toBe(false);
    expect(result.withId).toBe(true);
    expect(result.idLength).toBe(32);
  });
});

test.describe("Listen (F17, X11)", () => {
  test("listening to a member's mix streams Opus frames", async ({ page }) => {
    await page.goto("/");
    const auth = await login(page, "engineer", true);
    const result = await page.evaluate(async (token) => {
      const ws = new WebSocket(`ws://${location.host}/ws/audio?token=${token}`);
      ws.binaryType = "arraybuffer";
      const status: string[] = [];
      let frames = 0;
      await new Promise<void>((resolve, reject) => {
        ws.onopen = () => ws.send(JSON.stringify({ cmd: "ListenStart", member_id: "member2" }));
        ws.onmessage = (e) => {
          if (typeof e.data === "string") {
            const m = JSON.parse(e.data);
            if (m.event === "AudioStatus") status.push(m.data.status);
          } else {
            frames += 1;
            if (frames >= 5) resolve();
          }
        };
        setTimeout(() => reject(new Error(`frames ${frames}, status ${status.join(",")}`)), 8000);
      });
      ws.send(JSON.stringify({ cmd: "ListenStop" }));
      ws.close();
      return { status, frames };
    }, auth.token);
    expect(result.status).toContain("listening");
    expect(result.frames).toBeGreaterThanOrEqual(5);
  });

  test("the listen limiter sits after the boost (X11)", async ({ page }) => {
    const worklet = await page.request.get("/listen-limiter-worklet.js");
    expect(worklet.status()).toBe(200);
    expect(await worklet.text()).toContain("registerProcessor('listen-limiter'");

    await page.goto("/");
    // A key press first (a user gesture that navigates nowhere): an
    // AudioContext starts with a user gesture.
    await page.keyboard.press("a");
    const ready = await page.evaluate(async () => {
      const player = await import("/audio_player.js");
      player.initAudioPlayer();
      for (let i = 0; i < 50; i++) {
        if ((window as unknown as { __iem_listen_limiter: () => boolean }).__iem_listen_limiter()) {
          player.stopAudioPlayer();
          return true;
        }
        await new Promise((r) => setTimeout(r, 100));
      }
      return false;
    });
    expect(ready).toBe(true);
  });
});
