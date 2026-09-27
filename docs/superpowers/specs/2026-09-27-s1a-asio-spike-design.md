# S1a — ASIO spike on the real card: design note

**Ticket:** #3 (program #1). **Spec:** program spec §2.2, §4.3, I2, I3, I8, P6, R1, R6. **Plan:** `docs/superpowers/plans/2026-09-27-s1a-asio-spike.md`. **Related:** #15 (S1c), #9 (S6). **Site values** live only in `~/.config/iemmixer/asio-spike.env` and the ops runbook `docs/s1a-pc-runbook.md`.

## 1. Goal

Prove azo 0.2.1 on the IEM PC's Dante card before S6 builds the backend on it. Done means a written report on #3 with measured values and a decision (azo or fallback), and a switch REAPER → iemmixer → REAPER verified both ways.

Measured, per buffer size the driver accepts (32, 48, 64 at 96 kHz):

- driver facts: channels, sample types, buffer min/max/preferred/granularity, rate, reported latencies, clock sources (read only);
- callback interval p50/p99/p99.9/max, **missed** (interval ≥ 2 periods), **late** (> 1.5 periods), **overruns** (callback longer than a period), gaps in the driver's sample position, driver messages (reset, resync, overload, latency or size change);
- callback CPU time, idle and under load (busy-work in the callback plus busy threads);
- card clock drift against QPC;
- reopen time per phase (stop, dispose, release, create + init, create buffers, start, first callback).

## 2. Constraints

- **I2 and the owner's correction on #3 (2026-09-26):** buffers are measured for iemmixer only; the target is 32 (#15). REAPER is never measured or changed. When the card goes back to REAPER, the driver's preferred buffer is restored to the value REAPER ran with, and read back.
- **The host changes nothing on the card:** no `set_sample_rate`, no clock source, no control panel; it refuses any rate but 96 kHz and any buffer but the driver's preferred one. The integrity scan fails on those three calls anywhere in `crates/`.
- **A1:** every output channel gets a buffer, zeroed before `start()` and on every callback. The "unbuffered outputs" test is dropped: it could play unknown data on the band's channels and cannot be observed without the D5 loopback.
- **I3:** the PC task refuses while `reaper.exe` runs or anything holds the driver module; REAPER never starts while the spike runs.
- **I8:** the spike stops through a stop file; a hung spike is reported, never killed.
- **D2:** a window starts only after the owner's "event skončil" and never while `~/.config/iemmixer/EVENT-NOW` exists. The session creates that flag on "ide event" and removes it on "event skončil".
- **Tier 0, P5:** hosted CI builds and tests; the PC gets only the artifact of a reviewed `dev` push.
- **P6:** raw reports stay in `~/.local/share/iemmixer/golden-raw/asio-spike/`; the public report gives numbers only.

## 3. Code

In `iem-audio-io`:

- **`format.rs` (portable, tested):** the little-endian ASIO sample types to and from f64, with clipping and non-finite values as silence; `peak`; `admit(rate, preferred, expected, types)`, which enforces the refusals above.
- **`telemetry.rs` (portable, tested):** lock-free 1 µs histograms (0–5 ms plus overflow), counters, `classify`, drift, the `asioMessage` reply policy and its counters, and the activity guard (3 consecutive seconds above −50 dBFS). The callback is the only writer; it never allocates or locks.
- **`asio.rs` (Windows only, the crate's only unsafe code):**
  - `Host` is `!Send`: the thread that creates the driver (COM STA) makes every driver call and pumps its window messages.
  - Callbacks carry no user pointer, so one global slot holds the stream (one stream per process, claimed with `compare_exchange`). `Running::finish` stops the driver, clears the slot, waits until no callback is in flight, disposes the buffers, then frees the stream. The wait pumps messages and is bounded (2 s): a callback that never leaves is reported as hung, and the stream and buffers are leaked, never freed under it (R6).
  - The callback body runs inside `catch_unwind`; after a panic the outputs stay zeroed.
  - A reset or a buffer-size request sets a flag; the owner thread releases and recreates the driver.
  - The crate lint becomes `deny(unsafe_code)`, allowed only on this module.
- **`examples/asio_spike.rs`:** modes `probe`, `duplex` and `reopen`; a JSON report, a progress file every 5 s; `duplex` and `reopen` share the guards and stop on the stop file, band activity or a rate change, `duplex` also on the time limit or a caught fault (distinct exit codes; 8 = a hung stop). Every segment, reset and reopen cycle enters the report as it completes, so a failed run still reports what it measured. A stop file present at the start keeps the card closed.

**Dependencies:** `azo = "=0.2.1"` (MIT; builds `windows-bindgen` 0.100 at build time) and `windows-sys` features for the message pump, both for Windows targets only. Twelve crate names join `scripts/engine-deps-allow.txt`, and `bitflags` moves to 2.13.2. `asio.rs` and the example are excluded from mutation testing (like `dpapi.rs`): their decisions live in the two tested modules.

## 4. Verdicts

- **azo accepted** when `probe`, `duplex` and `reopen` all complete at the driver's preferred buffer, every reset is handled by reopening, and no crash or hang occurs. Missed periods are a property of the OS and driver (S1c), not of azo.
- **Fallback** (program spec §2.2: patch the fork, own IASIO host, then `asio-sys`) when the driver cannot be opened, buffers cannot be created, streaming does not start, or the process crashes inside azo.
- **Stable buffer:** 10 min duplex with 0 missed periods, overruns, position gaps and resets. The owner gets the numbers for all sizes; 32 stays the target, and #15 tunes the PC until it holds for 8 h.

## 5. The window

1. **Dev box driver `scripts/asio-spike/spike_window.py`:**
   - A state file holds the card owner (`reaper`/`switching`/`free`), the preferred buffer (original/current/restored) and the runs.
   - Every wait checks the EVENT-NOW flag every 2 s. A read-only PC call is abandoned at once. A changing call finishes first (each is bounded on the PC).
   - Then `preempt` runs: stop file, restore buffer, bring REAPER back.
2. **Bundle:** `fetch-bundle --sha` downloads the `asio-spike-<sha>` artifact of the successful `dev` push run for that SHA and checks `SHA256SUMS`. `setup` uploads it and registers the Interactive task `\iemmixer\iemmixer-asio-spike`, because ASIO runs in the console session like the future engine.
3. **Preflight (read-only):**
   - REAPER and the predecessor app run, and only `reaper.exe` holds the driver module;
   - the preferred buffer equals the recorded original (64 today), otherwise the window stops and the owner is told;
   - the bundle on the PC verifies.
4. **`to-dev`:** 60 s input interlock (below −50 dBFS), then REAPER saves (40026, project file changed) and quits (40004); the driver module is released. The predecessor app keeps running: its graceful exit is still open (§7).
5. **Runs:** `set-buffer --frames N` writes the preferred buffer with its registry kind kept and reads it back. `run --mode …` writes a request file and starts the task; the task checks hashes and I3, starts the spike at HIGH priority and ends only through the stop file. The report and stderr go to the raw directory, and the dev box prints a verdict per run.
6. **`to-event` / `preempt`:** while the card is free, always stop gracefully (60 s): the stop waits for the spike and its task, and a task that has not started the spike yet refuses on the stop file. Restore the preferred buffer with read-back whenever a `set-buffer` was recorded (a failed second write leaves the registry unknown). Then start REAPER through our own task (`\iemmixer\iemmixer-StartREAPER`, no 72 h limit); it refuses while a spike or the spike task runs and unless the preference reads back as the original. A failed step while the EVENT-NOW flag exists pre-empts too. Then the handover checks:
   - the project is loaded (track count);
   - the meter-bridge state is read, and the bridge is triggered once, only while that state is empty;
   - the heartbeat advances;
   - `reaper.exe` holds the driver module;
   - the app answers.

   These checks come from the 2026-09-27 lesson on #9. A spike that does not stop blocks REAPER (I3): alarm the owner; the last resort is the owner's reboot (event mode).

This driver is the interim switch script of #3: `to-dev` and `to-event` are "event skončil" and "ide event" until S6's `iemmode` exists.

## 6. Run plan (dev time, about 90 min)

1. `probe` at the current 64.
2. `duplex` 10 min at 64.
3. `reopen` × 5.
4. `duplex` 60 s with `--panic-at 50000` (fault caught, callbacks continue).
5. Then 32: `probe`, `duplex` 10 min idle, then 10 min with `--burn-us 100 --stress 4`.
6. 48 (if the driver accepts it): 10 min idle.
7. Restore 64, then `to-event`.

## 7. Decisions, deferrals, open items

**Decisions:**

- The buffer is set in the driver's registry preference (I2 streams at the preferred size; the spike never picks one). A driver that reads it only at load makes the spike refuse (`preferred … expected …`): a finding.
- Silent outputs only. The activity guard stops a run when the band plays.
- The predecessor app stays up. The window reuses the S1b PowerShell (`GoldenPc.psm1`) for save/quit, interlock and module holders.

**Deferred until the owner approves each one on #3 (not run):**

- an OS restart with the engine running;
- a reboot with the engine parked;
- the hard kill;
- SEH injection (`seh_ctl`);
- round-trip latency, which needs the D5 loopback.

**Open:**

- For the fork / S6: azo-sys 0.2.1 `I64Split` has no `#[repr(C)]` (it sits in the `#[repr(C)]` `TimeInfo` and is `getSamplePosition`'s out-parameter; rustc keeps its two `u32` in order today), and the fault run's panic hook allocates and locks stderr inside the callback (S6 needs a hook that does neither). The drift uses the callback entry time, not `TimeInfo.system_time` (a few ppm over 10 min).

- The graceful exit of the predecessor app (acceptance of #3). It is verified only when its tray Exit is reachable through the PC's remote-desktop MCP; otherwise it is handed to S6 on #9.
- The PC-only credential for the private denylist (ops issue 1): add it in the first window.

## 8. Risks

- **R1 (azo fails on this driver):** the fallbacks are in §4.
- **The driver may reject 48 or read the preference only at load:** findings; 32 and 64 decide.
- **R6 (a hang in `stop()`):** no kill. A callback still in flight after 2 s ends the spike with exit 8 (the driver is not called again); `spike_window.py` prints an owner alarm (also for exit 5, band activity). The owner may reboot.
- **A late "ide event":** the stop file within 2 s; REAPER back after the restore and the project load (≤ 2 min).
