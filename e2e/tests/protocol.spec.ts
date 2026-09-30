import { test, expect } from "./support/fixtures";
import { login } from "./support/session";
import { probeListen, soundInput, upgradeStatus, wsBase } from "./support/wire";

// The UI protocol v2 on the wire (program spec §5.3): the hello handshake,
// the reload close for a page without a protocol, the talk-id binding of
// the talkback socket (X6), listening over /ws/audio and who may open it,
// the listen diagnostics (F28), and the X11 limiter.

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
  // The engine's taps carry the test sine only where a level lets it through:
  // `soundInput` raises hand2 for the test and puts the mix back after. The
  // engineer's tap is before the mix's mute; a member's after it, so member8's
  // mix (no other spec's) is unmuted too.
  test("listening to the engineer's mix: the first frame within 1 s, 30 in 3 s", async ({ page, baseURL }) => {
    await page.goto("/");
    const auth = await login(page, "engineer", true);
    const restore = await soundInput(baseURL, auth.token, "engineer", "hand2");
    try {
      const probe = await probeListen(page, auth.token, "engineer", 3000);
      expect(probe.status).toContainEqual({ status: "listening", target: "engineer" });
      expect(probe.status.map((s) => s.status)).not.toContain("no_source");
      expect(probe.frames).toBeGreaterThanOrEqual(30);
      expect(probe.bytes).toBeGreaterThan(1000);
      expect(probe.firstFrameMs).not.toBeNull();
      expect(probe.firstFrameMs!).toBeLessThan(1000);
    } finally {
      await restore();
    }
  });

  test("listening to a member's mix: the first frame within 1 s, 30 in 3 s", async ({ page, baseURL }) => {
    await page.goto("/");
    const auth = await login(page, "engineer", true);
    const restore = await soundInput(baseURL, auth.token, "member8", "hand2", { output: true });
    try {
      const probe = await probeListen(page, auth.token, "member8", 3000);
      expect(probe.status).toContainEqual({ status: "listening", target: "member8" });
      expect(probe.status.map((s) => s.status)).not.toContain("no_source");
      expect(probe.frames).toBeGreaterThanOrEqual(30);
      expect(probe.bytes).toBeGreaterThan(1000);
      expect(probe.firstFrameMs).not.toBeNull();
      expect(probe.firstFrameMs!).toBeLessThan(1000);
    } finally {
      await restore();
    }
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

test.describe("Listen socket access (F17)", () => {
  // Opened from the test runner: it sees the upgrade's status, a page only "failed".
  test("the audio socket refuses a member's token with 403", async ({ page, baseURL }) => {
    await page.goto("/");
    const member = await login(page, "member2");
    const engineer = await login(page, "engineer", true);
    const audio = `${wsBase(baseURL)}/ws/audio`;
    expect(await upgradeStatus(`${audio}?token=${member.token}`)).toBe(403);
    // The engineer's token opens it: the refusal is the role's, not the route's.
    expect(await upgradeStatus(`${audio}?token=${engineer.token}`)).toBe(101);
  });

  test("the audio socket refuses a missing or unreadable token with 401", async ({ baseURL }) => {
    expect(await upgradeStatus(`${wsBase(baseURL)}/ws/audio`)).toBe(401);
    expect(await upgradeStatus(`${wsBase(baseURL)}/ws/audio?token=not-a-token`)).toBe(401);
  });
});

test.describe("Listen diagnostics (F28)", () => {
  test("the diagnostics keep the predecessor's fields", async ({ page }) => {
    await page.goto("/");
    const auth = await login(page, "engineer", true);
    const resp = await page.request.get("/api/audio/diagnostics", {
      headers: { Authorization: `Bearer ${auth.token}` },
    });
    expect(resp.status()).toBe(200);
    const diag = await resp.json();
    for (const key of [
      "receiving_oiem",
      "receiving_vban",
      "packets_per_second",
      "opus_frames_per_second",
      "peak_db",
      "last_sequence",
      "sequence_gaps",
    ]) {
      expect(diag, key).toHaveProperty(key);
    }
    expect(typeof diag.receiving_oiem).toBe("boolean");
    expect(typeof diag.receiving_vban).toBe("boolean");
    expect(typeof diag.peak_db).toBe("number");
    expect(typeof diag.opus_frames_per_second).toBe("number");
    expect(typeof diag.sequence_gaps).toBe("number");
  });

  test("the diagnostics need a token (401)", async ({ page }) => {
    const resp = await page.request.get("/api/audio/diagnostics");
    expect(resp.status()).toBe(401);
  });

  test("the diagnostics refuse a member's token (403)", async ({ page }) => {
    await page.goto("/");
    const auth = await login(page, "member2");
    const resp = await page.request.get("/api/audio/diagnostics", {
      headers: { Authorization: `Bearer ${auth.token}` },
    });
    expect(resp.status()).toBe(403);
  });

  test("while listening, the diagnostics show frames and the signal", async ({ page, baseURL }) => {
    await page.goto("/");
    const auth = await login(page, "engineer", true);
    const restore = await soundInput(baseURL, auth.token, "engineer", "hand2");
    const read = async () => {
      const resp = await page.request.get("/api/audio/diagnostics", {
        headers: { Authorization: `Bearer ${auth.token}` },
      });
      expect(resp.status()).toBe(200);
      return resp.json();
    };
    try {
      const before = (await read()).frames_forwarded;
      expect(typeof before).toBe("number");
      // The page listens while the runner reads the diagnostics.
      await page.evaluate((token) => {
        const scheme = location.protocol === "https:" ? "wss:" : "ws:";
        const ws = new WebSocket(`${scheme}//${location.host}/ws/audio?token=${token}`);
        ws.binaryType = "arraybuffer";
        ws.onopen = () => ws.send(JSON.stringify({ cmd: "ListenStart", member_id: "engineer" }));
        (window as unknown as { __diagListen: WebSocket }).__diagListen = ws;
      }, auth.token);
      // Two seconds of this listener's frames (50 a second): the rate is
      // measured over a second of them and the peak is of the last one, so
      // no earlier test's values remain.
      await expect
        .poll(async () => (await read()).frames_forwarded - before, { timeout: 10_000 })
        .toBeGreaterThanOrEqual(100);
      const diag = await read();
      expect(diag.receiving_oiem).toBe(true);
      expect(diag.opus_frames_per_second).toBeGreaterThan(10);
      expect(diag.peak_db).toBeGreaterThan(-30);
      expect(typeof diag.sequence_gaps).toBe("number");
    } finally {
      await page.evaluate(() => {
        const ws = (window as unknown as { __diagListen?: WebSocket }).__diagListen;
        if (ws && ws.readyState === WebSocket.OPEN) {
          ws.send(JSON.stringify({ cmd: "ListenStop" }));
          ws.close();
        }
      });
      await restore();
    }
  });
});
