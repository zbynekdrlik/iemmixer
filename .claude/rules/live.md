---
paths:
  - "e2e/tests/live/**"
  - "e2e/playwright.live.config.ts"
  - "e2e/tests/live-support.spec.ts"
  - "scripts/iem-pc/live_verdict.py"
  - "scripts/iem-pc/test_live_verdict.py"
  - "scripts/iem-pc/iempc_live.py"
  - "scripts/iem-pc/test_iempc_live.py"
---

# Live E2E on the IEM PC (S7, #10)

Design note `docs/superpowers/specs/2026-10-07-s7-hil-live-soak-design.md` §6, plan `docs/superpowers/plans/2026-10-07-s7-telemetry-soak.md` Part 4 (Tasks 16–27). The #10 comments of 2026-10-08 hold the lanes' decisions.

## Where they run

- Specs `e2e/tests/live/*.spec.ts`, config `e2e/playwright.live.config.ts`: one worker, no retries, trace, screenshot and video off (a trace would hold the tokens and the host). The mock config ignores `**/live/**`.
- Only from the ops `live.yml`, dispatched by `iempc dispatch-live --sha`: `verify` → `pc-begin` → `browser` and `pc` in parallel → `pc-end` (`if: always()`) → `report`. `browser` is a hosted Ubuntu runner with Chromium, against the band's public host through the tunnel (the members' real path); nothing of it runs on the PC.
- Public CI only lists them: the e2e job's `--list` step, with no `LIVE_*` and the mock PINs removed. Locally: `--list` of both configs. The pure support runs in the mock job (`tone.spec.ts`, `live-support.spec.ts`).

## Dispatch and the dispatch guards (`iempc_live.py`, Task 24)

- `iempc dispatch-live --sha <bundle>` (dev time, locked, a PC command; iempc.py keeps the call site only, #36) dispatches `live.yml -f sha -f branch -f run` with this box's gh. Any refusal dispatches nothing. In order:
  1. Before any call: the EVENT-NOW flag at the start, a full SHA, a live run of this SHA in this dev entry already (`<state>/live.json`, `{"runs": [{sha, branch, run, entry, at}]}`, the newest 200; a file of another shape is an error to check by hand; a failed ops run is `gh run rerun <id> -R <ops repo>`, never a second dispatch), another live run of this entry that may still run (it would queue behind the first in `live.yml`'s concurrency group and outlive its window), a soak of this entry that may still run (`iempc_soak.running_soak`: `pc-begin` restarts the engine in a HIL job).
  2. `iemmode status` (a new flag abandons it, the event path follows): `live_refusal`, the soak's rule (`iempc_soak.runs_refusal`): ok, dev, no switch, no HIL job, active bundle and engine build the SHA, engine neither parked nor faulted.
  3. `green_run` (P5); 4. the flag again right before the dispatch; 5. `gh workflow run`, then the record (a failed dispatch records nothing).
- **The reverse guards:** `dispatch-soak` and `switch-test` call `refuse_while_live(ip, <command>)` first, before any call: a live run of this dev entry that may still run refuses them (a soak would meet the run's engine restart, a switch would end the run, `left-dev`).
- **The window:** a run may still run `WINDOW_S` (5400 s) after its dispatch, the sum of `JOB_MINUTES`: `verify` and the pick-up 5, `pc-begin` 15, `pc` 60 (`browser`'s 45 beside it, Playwright's `globalTimeout` 40 inside that), `pc-end` 10 (`report` runs on a hosted runner). **`live.yml`'s `timeout-minutes` must match `JOB_MINUTES`** (`test_the_window_covers_live_ymls_jobs`): a longer job there needs a longer window here.
- Fail safe: a record whose time (no ISO time, no zone) or dev entry (not an integer) cannot be read may still run. Known limits: only this box's `live.json` is read (a run dispatched elsewhere is not seen); a run that waited longer than 5 min for the PC's runner outlives its window; a run of an earlier dev entry is not looked at (a switch out of dev ends it).
- Tests: `test_iempc_live.py` on `test_iempc.Base`; every guard on both sides (no Python mutation gate runs in CI).

## Variables, tokens and P6

- `support/env.ts` reads every value on first use (`live()`, `runMarker()`), so `--list` needs none; a refusal names the variable, never its value. A new `LIVE_*` goes there, with validation.
  - `LIVE_BASE_URL` a bare `https://` origin; `LIVE_SHA` the 40-hex build; `LIVE_TOKENS` the path of a JSON `{engineer, member}` of JWT-shaped tokens; `LIVE_MEMBER`, `LIVE_TEST_INPUT`, `LIVE_TALKBACK_INPUT` ids; `LIVE_BURST_DBFS` at most −20; `LIVE_RESULTS` the JSON report's path (config).
  - `GITHUB_RUN_ID`, `GITHUB_RUN_ATTEMPT` (digits) build the client-error marker.
- **No band PIN in GitHub (decision 1):** `pc-begin` mints an engineer token and one member token on the PC (`iem-soakclient token`, expiry 3600 s) and hands them to `browser` in a private one-day artifact that `report` deletes. Nothing logs in; `openLive(page, who)` stores the token as the UI does.
- **Import `test` from `./support/live`, never `support/session.ts`:** its `pins.ts` throws at import without PINs (the CI list step catches it).
- **No error carries a URL, a token, a push endpoint or a site value.** `navigate`, `apiGet`/`apiPost` (`apiAt`), the relay and `liveSocket` throw fixed words. Compare a value that may be secret or site data as a boolean (`expect(a === b, "…").toBe(true)`), never `toBe(value)`, and poll a boolean (`expect.poll` prints what it received); never `toHaveURL`; read a request's body with `bodyOf`, never `postDataJSON()` (its error holds the body). The live console guard redacts URLs (`support/console.ts`, `redacted`); a page outside the fixtures uses `guardConsole`.

## Relay and no reconnect

- After "ide event" the predecessor answers at the same address with the same secret (#10, 2026-10-07).
- Every page socket goes through the `relay` fixture (`relaySockets`): one real socket per path and test, opened only after `/api/version` names `LIVE_SHA` (`expectBuild`); a second attempt is refused and never reaches the server; a server close fails the test. A persistent context calls `relaySockets(page)` itself.
- Runner sockets (`BurstWatch`, the desk's `LiveMixer`) follow the same rule, through one handshake (`liveSocket`: the page's Origin and User-Agent, 15 s bound).
- A request that writes server state (a push subscribe or revoke, a client error), or whose answer a test judges (`/api/tunnel`), comes after an `expectBuild`: nothing is written to the predecessor, and a failure names the hop.
- **A runner socket lost mid-burst leaves the spec's changes in place** (no other socket may undo them). `pc-end` always runs a fresh `iem-migrate import` of the saved project into the engine state (the command of `pc.toml` `data_live`), and the relay's fixed failure code makes the browser job red (ROZHODNUTÉ, #10, 2026-10-08, for Task 25).

## Bursts and the burst-only rule

- `pc` polls the run's `browser` job and, while it runs, fires `iemmode test-signal … --listen` in bursts of 30 s every 60 s (ttl ≤ 60 s) at −20 dBFS, the HIL ceiling (decision 4).
- The server tells only a `&hil=1` listen socket a burst's edges: `AudioStatus` `probe` (its first probe frame) and `listening` (the first own frame after). The probe gate is per socket, so the edges are per socket.
- **`BurstWatch` counts a burst only after 5 s of the slot's own frames before its `probe`.** A watch opened mid-burst gets `probe` with its first frame, and a probe stall over the gate's 100 ms hold gives `listening` and `probe` again: neither tells the time left, so the watch waits for the next burst. `inBurst()` ends at most 28 s after the `probe`.
- **`desk.ts` is the only way a live spec changes the engine.** A change happens only inside a burst (every mix TX is zero) and goes back before it ends, checked against `listening`: `desk.during` runs the steps inside one burst, `desk.change` refuses a change outside it or with under 2 s left and keeps its undo, `desk.track` keeps a page action's undo. The undos run newest first when the steps end, at once when the burst ends (a 50 ms guard), and at the fixture's teardown (a body's `finally` does not run after a timeout).
- **The server drops a closed socket's queued commands:** every socket waits for its own barrier (`applied()`) before it closes.
- Tunnel, push and client error change no engine state and need no burst.

## Measurements (row → spec)

- Numbers leave a test only as `live_number` annotations whose keys are `LIVE_NUMBER_KEYS`, the verdict's `NUMBER_KEYS` (`tone.spec.ts` compares them).
- **Listen (row 570, #25):** inside a burst the engineer clicks Listen; the first decoded audio within 3 s (`first_audio_ms`). The player's output through an `AnalyserNode`, `toneOf` over 1 s: 1 kHz ± 1 Hz at `LIVE_BURST_DBFS` ± 0.5 dB (`listen_hz`, `listen_dbfs`). Every probe frame CELT, 20 ms, stereo, and decodes (`opus_frames`).
- **Talkback (A4, row 731):** Chromium's fake microphone plays 1 kHz at half scale, the page encodes it onto `/ws/talkback`; the runner reads the `LIVE_TALKBACK_INPUT` meter (its trim off, inside the burst). Median meter against the encoder's level: −8.4 ± 0.3 dB (`talkback_db`). **Chromium's AGC ramps the capture for ~2.5 s** (`talkback-capture.spec.ts` checks it), so the window starts 2.7 s after the encoder's first frame. Talk is held at most 8 s; while held, never 5 silent meter frames in a row.
- **Limiter (row 611, X14):** the burst's sine, its input dry and open, at the mix's level +12 dB and the mix EQ's first peak band at +12.04 dB (1 kHz): +4 dBFS into a limit of −6 dB. The counter grows ≥ 1.5 s within 5 s, and Reset in the page zeros it (`limiter_active_s`). A solo fails it early.
- **Meters (#25):** the runner's socket only: ≥ 27 `Meters` frames in 3 s, and the burst input's median peak within 0.5 dB of the burst level, the input dry and open (`meter_fps`, `burst_input_dbfs`).
- **Tunnel (rows 735, 736):** `/api/tunnel` reads Ok with ≥ 1 ready connection; the engineer's `tunnel-status` reads `Vonkajší prístup: OK` (class `ok`); a member's page shows no `tunnel-banner` and no indicator once a `TunnelStatus` frame came (the relay's `events`).
- **Push (row 717):** a persistent profile (`chromium.launchPersistentContext`, notifications granted) on the full Chromium, `channel: "chromium"`: the headless shell has no push service (subscribe fails "push service not available", measured 2026-10-09), so the ops browser job installs full Chromium, never `--only-shell`. Its own console guard allows nothing. The subscribe answers 200 with an `https://` endpoint the browser holds; the logout's unsubscribe answers 200 for it, and `getSubscription()` is null. A `PushLedger` on the context keeps every posted endpoint until an unsubscribe of it answers 200; the fixture's teardown (it runs also after a timeout) unsubscribes in the browser, closes the profile (nothing more can be posted), then, after `expectBuild`, revokes the rest on the server. `pc-end` compares the server's subscription counts (`push_before`/`push_after`) as the backstop.
- **Client error (row 555):** the start page posts `{panic_message: iemmixer-live-marker-<run id>-<attempt>}` to `/api/client-error`, which answers 204; `pc-end` finds the marker in `<root>\logs\server.log`, not in the tray's rolling log.

## The verdict (`live_verdict.py`, `live/iem-pc`)

- Pure, stdlib. The record contract is its module docstring: `begin.json`, `pc.json`, `bursts.jsonl`, `evidence.json`, `results.json` (the report).
- Cancelled, never red: a cancelled PC job, a failed one without a record, `left-dev`/`not-free`. A cancelled browser job alone is red, and so is a PC job that succeeded without a record.
- The checks, in order: begin ready; the pc job `browser-done` with ≥ 1 burst, each exit 0; every expected title passed; the client-log marker found; `push_after` ≤ `push_before`; the job end ok. Each job's result comes last in its check.
- **Titles are read with the parity checker's `test(` rule:** a spec's tests are called through `test(` with a literal title (a `test.extend` result is named `test` too), and no comment writes `test(`.
- The summary holds fixed codes, numbers and spec titles, never a test's error text.

## The manifest rows

- The 7 PENDING rows: 555 client error, 570 Listen within 3 s (`listen-probe.spec.ts`), 611 limiter, 717 push, 731 held Talk (`talkback.spec.ts`), 735 and 736 tunnel.
- Task 27 cites the live titles after the first green `live/iem-pc`; then `check_parity_manifest.py --cutover` passes. Never before.
