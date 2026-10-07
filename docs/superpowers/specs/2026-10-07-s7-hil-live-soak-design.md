# S7 — full HIL, live tests and soak: design note

**Ticket:** #10 (program #1). **Spec:** §3.5 (real time, CPU, talkback, listen, features), §4.2, §4.3, §6 (S7 row), P3, P5, P7, P9, P10. **Inputs:**
- the hand-offs on #10 (S0, S5, the parity report #25, the S1c/S6 list of 2026-10-07);
- S6 design §7, §9, §11;
- S1c design §10 and plan Task 20;
- the parity manifest.

Site values live only in the ops repo and `~/.config/iemmixer/`.

## Zhrnutie pre vlastníka

- S7 dokazuje, že iemmixer na PC vydrží:
  - 8 hodín pri 32 vzorkách bez jedinej vynechanej periódy, s pripojeným mixom a odposluchom;
  - prepnutie späť na REAPER stíši uši najviac na 60 s;
  - talkback a odposluch merajú presne.
- Všetko beží len vo vývojovom čase. „Ide event“ test korektne zruší a ďalší začne od nuly.
- Kapela nič nepočuje a nemusí nič riešiť:
  - testovací tón ide len na voľné výstupy karty;
  - do odposluchu v prehliadači ide len inžinierovi, ktorý test spustil;
  - kým tón znie, sú všetky in-ear výstupy kapely nulové.
- Nič nové sa na PC neinštaluje okrem malého testovacieho klienta v bundli. Prehliadač pre živé testy beží na GitHube, nie na PC.

## 1. Goal and acceptance

Prove on the real PC, through the guard's engine:
- 0 missed periods over ≥ 8 h;
- late ≤ 0.2 % against the S1a p99.9 interval;
- `process()` p99.9 ≤ 25 % of the period;
- talkback −8.4 ± 0.3 dB at the `ENG_MIC` meter;
- Listen ± 1 Hz, ± 0.5 dB through Opus to the browser;
- the in-ear silence of a switch back to REAPER ≤ 60 s;
- live E2E green with zero console errors.

One manual 72 h NullRt soak. The 7 PENDING parity rows close (the manifest's `--cutover` passes).

## 2. Constraints

- **P3 / §4.2:** every PC run happens in `dev`, on the self-hosted ops runner the guard starts there. "Ide event" cancels it (the guard stops the runner first). A soak cut short restarts from zero in the next dev window. No silence or activity check anywhere (#38).
- **The test signal never reaches a channel a band member hears** (owner, #9 2026-09-28):
  - it plays on the `[guard] hil_tx` spare outputs only;
  - while it plays, every mix TX is zero.
- **P10:** nothing heavy runs on the PC during a measurement. No browser and no Node there.
- **P5:** only CI bundles with verified provenance run on the PC.
- **P6:**
  - live specs live in this public repo with no site value (base URL, PINs and member ids come from the ops run's environment);
  - every PC artefact (logs, summaries) is posted as numbers only.
- **Tier 0:** no local compilation. The 72 h NullRt soak runs CI-built Linux binaries.

## 3. Telemetry (engine, guard)

The gates need distributions the engine does not export today. `Status` has `late` (interval > 1.5 periods) and `process_max_us`. `Reply.engine` has neither.

- **Engine:** two RT histograms in `iem-audio-io`, both relaxed atomic increments into preallocated arrays (RT contract I7 holds):
  - the callback interval;
  - `process()` time.
  - Buckets: 1 µs up to 2 periods, then an overflow bucket. Counted since the stream opened.
  - `Status` gains `interval_hist` and `process_hist` (additive JSON, a sparse `[[bucket, count], …]` form) and `overruns` (already there).
- **Guard:** `Reply.engine` carries `late`, `overruns`, `process_max_us` and the two histograms through unchanged (additive fields; an older engine leaves them absent).
- **Soak judgement (dev box):**
  - percentiles come from the histograms;
  - "late" for the gate counts intervals above the S1a baseline p99.9 at B = 32 (347 µs, `docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md` results). It must stay ≤ 0.2 % of callbacks.
  - The 1.5-period `late` counter stays as information.

## 4. Soak (≥ 8 h, PC)

- **Run:**
  - `iempc dispatch-soak --sha` (dev only, the bundle must be the active one) dispatches ops `soak.yml`.
  - Its `soak` job on the `iem-pc` runner has a 600 min timeout.
  - The soak changes no guard state: it starts no HIL job, so the engine keeps its production flags and the real topology. It only reads `iemmode status` and runs the client harness.
- **Client harness `iem-soakclient`** (Rust, a new crate, built by CI into the bundle):
  - It logs in as the engineer through the server's normal login (PIN from the ops secret in the job's environment).
  - It opens one mixer socket and one listen socket on one member mix, at the LAN address the server names.
  - It decodes the Opus frames and counts frames, gaps (> 60 ms without a frame), reconnects and meter frames, then writes a JSON summary.
  - Its own CPU Set is the profile's housekeeping set when one is given.
- **Polls:**
  - every 60 s: `iemmode status`;
  - every 10 min: the guard's `tuning state` drift, read through `status`;
  - alongside: a circular DPC trace (1 GB, at most 5 cuts) through `iempc trace` (#15 design, 2026-10-07).
- **Verdict:**
  - green, with ≥ 8 h of polls on one engine pid and one bundle, when:
    - missed +0 and resets +0;
    - late (§3) ≤ 0.2 %;
    - `process()` p99.9 ≤ 83 µs;
    - harness gaps 0 and reconnects 0;
  - any other end is red, naming the first failing number.
  - The ops `report` job posts `soak/iem-pc` on the SHA. `iemmode report` records it per bundle beside HIL.
- **"Ide event":** the guard stops the runner, and the job ends cancelled (never red, never green).

## 5. Switch timing

- **Guard:**
  - each switch keeps `{from, to, outcome, started, ended, steps: [{step, ms}]}` as `last_switch` in its state and in `status`; today `Switching.started` is dropped at the end;
  - the in-ear silence window is computed from the steps:
    - dev → event: the engine's fade-out to the REAPER handover passing;
    - event → dev: REAPER's save-and-quit to the engine's arm.
- **`iempc switch-test`** (dev time only, the owner's standing directive):
  - runs `event` then `dev` on the active bundle and reads both records;
  - green when dev → event silence ≤ 60 s and the handover checks finish within 90 s;
  - the numbers go to #10. The 120 s handover bound (S6 §11) is tightened to what was measured, with margin.

## 6. Live E2E

- **Specs:** `e2e/tests/live/*.spec.ts` in this repo.
  - The public config already ignores `**/live/**`.
  - They reuse `support/session.ts` and the zero-console fixture.
  - Each spec owns one member's mix and restores it (`.claude/rules/e2e.md`).
- **Where they run:** ops `live.yml`, two jobs dispatched together by `iempc dispatch-live --sha`.
  - **`browser`:** a hosted Ubuntu runner with Chromium and the 1 kHz fake microphone, against the band's public host through the tunnel. It is the members' real path. Nothing runs on the PC, and P7 is about runtime, not test clients.
  - **`pc`:** the `iem-pc` runner. It begins a guard HIL job, fires the test signal while `browser` runs, then ends the job and reads the PC-side evidence.
- **The listen probe** (Listen ± 1 Hz / ± 0.5 dB). Today the test signal silences both listen taps.
  - **Engine:** `HilTestSignal` gains `listen: bool` (supervisor only, under `--test-signal`). With it, both listen taps carry the sine at the test level, marked as probe frames. Every mix TX stays zero, so no in-ear hears it.
  - **Server:** probe frames go only to listen sockets of an engineer session opened with `&hil=1`. Every other listener gets the silent frames it gets today, so a member listening on a phone never hears the tone.
  - **Browser:** the spec takes the player's output through an `AnalyserNode` and estimates frequency and level over 1 s.
- **Sync without a channel between the jobs:**
  - `pc` polls the run's `browser` job through the GitHub API (`actions: read`);
  - while it runs, `pc` fires 30 s bursts every 60 s (`ttl` ≤ 60 s, −30 dBFS);
  - the spec waits for a burst, at most 3 min.
- **Talkback −8.4 ± 0.3 dB:** measured inside a burst, so every mix TX is zero and the talkback tone reaches no in-ear. The fake microphone goes through talkback to `ENG_MIC`; the spec reads the `ENG_MIC` input meter (pre-fader) on the mixer socket.
- **The 7 PENDING rows:**
  - Listen within 3 s;
  - talkback quality;
  - the limiter counter. During a burst a mix is driven over its limit by a talkback level step; the counter counts and Reset zeroes it.
  - push unsubscribe: a real Web Push subscription in Chromium;
  - tunnel status ×2: the real cloudflared;
  - the client error log: the spec sends the marker report, and `pc` reads the rolling log on the PC.

  Each becomes a spec here, cited by the manifest. The #25 repeats (listen signal, Opus frame validity, meters on the real card) ride along.
- **Verdict:** `live/iem-pc` on the SHA, from the ops `report` job.

## 7. HIL v2

`hil-v1.ps1` grows the checks HIL v1 left out:
- the first-instance flag;
- the tunnel peer address;
- the forced reopen's duration (≈ 100 ms);
- the fault callback time (< 1 ms).

`hil.yml` passes `-SiteChange` / `-SiteRevert`, so F30 runs.

The hwlat probe (S1c hand-off) is **not** added: it needs the spike executable and a CPU it can take. The soak's engine intervals and the DPC trace measure the same risk on the real engine.

## 8. 72 h NullRt soak (manual, once)

- **Purpose:** long-run growth in the engine, server and client paths that 8 h may not show:
  - memory;
  - handles and threads;
  - timer drift;
  - socket churn.

  NullRt counts no missed periods, so it judges growth, not real time.
- **Run on a dev box (not the PC):**
  - CI uploads a Linux artifact (`iem-engine`, `iem-server`, `iem-soakclient`) from the `e2e` job;
  - `scripts/soak/nullrt_soak.py` starts the three, samples every 60 s for 72 h (RSS, open fds, threads, `late`, the harness counters) and stops them gracefully.
- **Green when:** no exit; RSS after hour 1 grows ≤ 16 MB; fds and threads flat; harness gaps 0. The numbers go to #10.

## 9. Proof

- **Hosted CI (unit and mock E2E):**
  - histogram bucketing and the percentile math;
  - the probe-frame routing in the server (a non-HIL listener gets silence);
  - `iem-soakclient` against the NullRt engine and the server in the `e2e` job (10 min);
  - the soak and switch verdict functions (Python, synthetic records);
  - `iempc` dispatch guards (EVENT-NOW, dev only, active bundle).
- **On the PC:** the soak, `switch-test`, `live.yml` and HIL v2, each on a green `dev` SHA, then on the cutover candidate.
- **RED → GREEN per behaviour; the diff-scoped mutation gate.**

## 10. Order (plan tasks become lanes, one at a time)

1. Telemetry (§3) and switch records (§5, guard side).
2. Soak: `iem-soakclient`, `dispatch-soak`, `soak.yml`, the verdict. Then the first 8 h soak. This also gives #15 acceptance 1, after the #15 tuning lane.
3. `switch-test`, then the run.
4. Listen probe (engine + server), the live specs, `live.yml`, the 7 rows. Then the run.
5. HIL v2.
6. The NullRt soak tool, then the 72 h run on a dev box.

## 11. Deviations, risks, UNVERIFIED

- **Deviation:** the program spec counts ~3k TS of live specs. #25 already moved 211 of the 222 predecessor live tests to mock E2E or server tests. S7's live suite is the 7 PENDING rows plus the HIL measurements, and the manifest stays the measure.
- **Risk:** the hosted runner reaches the PC through the tunnel. Tunnel trouble makes `live` red without an engine fault. The spec names the failing hop (`/api/version` through the public host first).
- **Risk:** login protection on the public host (`.claude/rules/security-baseline.md`). The live run logs in once per member under the budgets `.claude/rules/e2e.md` names.
- **UNVERIFIED:**
  - Chromium's Web Push subscribe on a hosted runner (else that row runs from the PC side's server log only);
  - an `AnalyserNode` on the player's graph without a console warning;
  - the GitHub API's view of a running sibling job from the self-hosted runner's token.
