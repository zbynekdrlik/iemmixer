# S6 — ASIO backend, guard and HIL: design note

**Ticket:** #9 (program #1). **Spec:** §2.1–2.5, §4, §5.2, P4–P7, P9, P10, F27, F30. **Plan:** `docs/superpowers/plans/2026-09-27-s6-asio-guard-hil.md`. **Inputs:** the hand-offs on #9, the S1a results, the S1c design §5–§7 and §10. Site values live only in the ops repo and `~/.config/iemmixer/`.

## Zhrnutie pre vlastníka

- S6 prenesie iemmixer na iem PC: zvukový engine na karte pri 32 vzorkách, strážca `iemmode` a automatické testy na PC.
- Riadia to len tvoje dve správy. „Event skončil“ → uložím a ukončím REAPER, korektne ukončím aplikáciu predchodcu, spustím iemmixer. „Ide event“ → zastavím iemmixer, spustím REAPER, potom aplikáciu, overím odovzdanie a potvrdím ti to.
- Aplikáciu predchodcu ukončím tou istou cestou ako jej položka „Exit“ v tray menu, len bez klikania. Nič sa nezabíja.
- REAPER si vždy nechá svoj buffer 64. Hodnota 32 je zapísaná len na okamih, keď iemmixer otvára kartu, takže REAPER nájde svoju hodnotu aj po výpadku prúdu.
- Kým iemmixer beží, drží bežnú adresu kapely (v sieti aj verejnú). Kapela nič nerieši.
- Po reštarte je PC vždy v režime REAPER. Testy na PC bežia len vo vývojovom čase; „ide event“ ich korektne zruší.
- Teraz od teba nič netreba. Neskôr ti jednou otázkou pošlem na schválenie päť testov: reštart, zaseknutý engine, tvrdé ukončenie, chyba ovládača a meranie oneskorenia cez slučku Dante.

## 1. Goal and acceptance

iemmixer runs on the IEM PC: the ASIO backend at 32 samples, the guard (`iemmixer-guard`, `iemmode`) switching on the owner's messages, bundles with pin and revert, the server on the band's usual address, and HIL v1 via the ops repo. Acceptance (#9): a switch both ways verified by the handover checks on real owner signals, and HIL v1 green.

## 2. Constraints

D2 (owner messages only; reboot = `event`; PC work in dev time, EVENT-NOW pre-empts), I2 (32 for iemmixer, REAPER keeps its value), I3, I8/P4 (nothing force-ended; the integrity scan covers the new crates, comments included), G7, P5/G8, P6, P7, P9, P10, Tier 0. Hardening is the agent's job (owner, 2026-09-24).

## 3. ASIO backend (`iem-audio-io`)

`asio.rs` grows from the spike host into `AsioStream<P: Process>`.

- **Threads.** An owner thread creates the driver (COM STA), makes every driver call, pumps messages and runs the stall watchdog. The callback zeroes every output (A1), decodes the topology's RX channels via `format`, calls `process` in `catch_unwind` and encodes the TX channels. After a panic the outputs stay zero and the processor is never called again.
- **Channel map** (portable): topology card numbers → card indices, checked against the driver's counts. `format::admit` expects 32 (and 96 kHz).
- **Preference window** (deviation, §11). The driver reads `PrefBuffSize` at open (S1a). Before each open the backend writes 32 (registry kind kept, read back); right after `createBuffers` it writes the original back (read back). The registry holds REAPER's value at every other moment, so a crash, power loss or OS restart in dev never leaves REAPER at 32. Key, name and original come from `[card]` (ops); a mismatch refuses the stream (exit 3). Decisions in a portable `prefwin.rs`; writes via `windows-registry` (already in the closure).
- **Reset/reopen** on a driver request or a stall (no callback for 2 s): stop, dispose, release, reopen (~104 ms, S1a), processor kept, `Process::discontinuity()` restarts the 500 ms fade-in. Budget ≤ 1 per 5 min and ≤ 3 per process (§4.4); beyond = fault (exit 70). A callback still in flight 2 s after `stop()` leaks the stream (R6): the engine reports `parked` and the guard alarms.
- **Panic hook** (S1a: 4.29 ms per panic): on the RT thread it stores only the location and a counter in atomics — no formatting, allocation or stderr lock.
- **SEH filter:** asks the owner thread to release the driver, waits ≤ 1 s, then lets the process end or parks the faulting thread (§2.4); proven only by the owner-approved test (§10). **Session end:** a hidden window on the owner thread; the engine saves, fades out and stops the driver first.
- **`iem-win`** (new, MIT/Apache, the only unsafe outside `asio.rs`), shared with the guard: priority HIGH, power throttling off, CPU Set (S1c L5), locked minimum working set, user SID, process and module-holder queries, window messages, console control events, breakaway spawn. Non-Windows returns `Unsupported`.

## 4. Engine (`iem-engine`, `iem-engine-proto`)

- **`run --backend asio`** with `[card]` (driver name, preference key/name/original, `frames = 32`). New exit 3 = card refused (`reaper.exe` exists — I3, no driver, `admit`); the guard never respawns after 2 or 3.
- **`--hold`:** silent until the guard sends `Arm`, after 10 s with 0 missed periods (§4.3). Respawns start unheld.
- **Role `supervisor`** (additive, `PROTO` stays 1): one connection that may send `Shutdown`, `SaveNow`, `Arm`, test signal and fault injection (under their launch flags); it never supersedes `control`. `Status` gains `missed`, `overruns`, `resets`, `parked`.
- **Windows pipes** (interprocess 2.4.4): remote clients refused, first-instance flag, a DACL for the user and SYSTEM; non-blocking streams polled every 10 ms, so a superseded connection closes; pipe tests join the `windows` job.
- **`interlock --seconds 60`:** card open, outputs silent, stage inputs only; exit 0 quiet / 5 activity. **`check-site`** validates a site (I4, F30).

## 5. Guard and `iemmode` (`iem-guard`)

### 5.1 Processes and control path

- **Guard:** Interactive task `\iemmixer\iemmixer-guard` (logged-on user, Limited, no time limit, IgnoreNew), single-instance mutex; not started at boot before cutover. Its children (engine, server, tray, runner) start with job breakaway, so a guard restart never touches audio (I9); a restarted guard re-adopts them.
- **`iemmode`** is the only client: the agent over ssh (pipes are global, so session 0 reaches the session-1 guard; `iemmode` starts the guard task if needed), the engineer's button (`back_to_reaper` = `bin\iemmode.exe event`), HIL jobs, the tray.
- **Elevated work** (S1c tuning) only through `\iemmixer\iemmixer-tuning` (RunLevel Highest), which accepts four verbs from a request file (`enter`, `exit`, `state`, `apply-tier2`).

### 5.2 Modes and switching

A pure planner turns (from, to, facts) into steps with undos, persisted before each step, so a guard restart or "ide event" resumes or unwinds (S1a's `undo_plan`). `event` pre-empts after the current bounded step.

**Into `dev` / `live`:**

1. Precheck: bundle installed (`live`: `main` + green HIL), ≥ 1 alarm subscription, no engine.
2. 60 s interlock on the stage inputs (REAPER's stage-track meters, or `iem-engine interlock` without REAPER): activity refuses and alarms; `--force` only on the owner's word; trials skip it.
3. REAPER saves (project mtime changes) and quits; gone ≤ 30 s, driver module unheld.
4. Predecessor app stopped gracefully (§5.3); ports 80/443 free.
5. Tuning `enter` (S1c; a missing module is reported, not fatal, until S1c ships it).
6. `iem_migrate::stage::recover`; `dev` = shadow-import report + dev data directory, `live` = fresh import.
7. Engine `--hold`: `Hello.engine_build` = bundle SHA, callbacks advancing, 10 s with 0 missed → `Arm`.
8. Server (`IEMMIXER_MODE`) and tray; LAN 80/443 and the public host answer `/api/version` with the SHA; tunnel `/ready` > 0.

Any failure unwinds to `event`.

**Back to `event`:**

1. Cancel HIL jobs; stop the idle runner.
2. Engine `Shutdown` → `DriverReleased` ≤ 10 s (timeout: alarm, REAPER stays down) → process gone.
3. Server: Ctrl-Break to its process group (graceful shutdown); tray: `Quit`; ports free.
4. Tuning `exit` (failures alarm, never block); the preference reads back as the original, else restore (a failed restore keeps REAPER down).
5. No module holder; REAPER through `\iemmixer\iemmixer-StartREAPER`.
6. Handover checks, ≤ 120 s: track count loaded; no REAPER dialog window; meter bridge triggered exactly once and only while its state is empty (#9 lesson); heartbeat advancing; `reaper.exe` holds the driver module; input peaks not all −∞, else `UNCONFIRMED-AUDIO`.
7. Predecessor app through `\iemmixer\iemmixer-StartApp` (its exe directly, never its force-ending launcher script); it answers `/api/version`, its member list (read from REAPER) has the expected count, the public host answers.
8. The agent confirms (✅) or an alarm goes out.

In `event`, `iemmode event` only runs the checks.

### 5.3 The predecessor app

The pinned source: closing its window only hides it; there is no shutdown route; its tray menu's Exit calls `exit(0)`; data files are written temp-then-rename. Rejected: session-end messages (they end its event loop and leave the process to Windows) and a second instance (arguments are ignored).

**Chosen:** the guard (same user, session 1) finds the tray library's message window owned by the app and posts the `WM_COMMAND` the menu posts when Exit is clicked — the owner's click path, no input simulation. The id follows from the pinned source (items numbered in creation order) and stays in the private env.

**Verified every time:** the app's "exit requested from tray" log line after the post, process gone ≤ 30 s, ports 80/443 free, no newer temp file in its data directory. A wrong id is ignored or opens the window/copies the URL — harmless: the switch aborts, REAPER restarts, alarm. A changed binary hash is an informational alarm.

### 5.4 Safety net

- **Crash loop** (3 abnormal exits in 10 min): trial or `dev` → alarm and `event`; prod → the previous pin's engine (§4.1). Backoff 1 → 10 s.
- **Alarms:** a persistent file (shown by `iemmode` and the tray) plus `iem-server notify`. A `reaper.exe` or predecessor process appearing in `dev`/`live` (e.g. a predecessor deployment) alarms; nothing is ended.
- **Session end / logon:** the guard stops respawning while the engine releases itself; the elevated logon task `\iemmixer\iemmixer-logon` runs tuning `exit` and checks the preference (G1).
- **Band activity (S5 server):** must watch only `[activity] inputs` — the program input carries signal while the band is silent (S1a). Fixed here, RED/GREEN.

### 5.5 Bundles, pin, revert

- A bundle is one SHA (engine, server, guard, `iemmode`, tray, tuning module, `manifest.json`, `SHA256SUMS`), zipped and attested by digest.
- `iemmixer-guard install <zip>`: unpack into `bundles\<sha>.partial`, verify, rename, never overwrite. `activate <sha>` (dev only); `live` activates the pin; `current`/`previous` pointers are replaced atomically. Guard and `iemmode` run from `bin\` copies (a running exe is renamed, then replaced), so paths never change.
- Per bundle {sha, branch, run, HIL result}; `live --build` refuses unless `main` + green (G8); `revert` = previous pin.

## 6. PC layout, identity, tunnel, bootstrap

- **Layout:** `%LOCALAPPDATA%\iemmixer\` with a protected, inherited DACL (user, SYSTEM, Administrators), so `band`'s staging copies need no ACL work (#20); `secrets\` keeps the pepper apart from the PIN hashes (S0).
- **P9:** `iem-server` binds 0.0.0.0:80/443 with the migrated certificate, JWT secret, VAPID keys and PIN hashes; one port-based firewall rule.
- **Tunnel:** ingress unchanged; only the running app repairs it (predecessor in `event`, `iem-server` otherwise). Bootstrap grants the right to stop/start the tunnel service if missing (S0 hand-off 7). HIL proves the server sees tunnel requests from a loopback peer (S0 hand-off 1); a non-loopback ingress is our own security fix (same port, loopback form), proved at the next event handover.
- **Bootstrap** (dev time, agent over ssh, elevated): tasks, root DACL, firewall rule, service right, Defender exclusion (S1c G4), runner registration, the first bundle after a manual `gh attestation verify`. PINs come from `iem-migrate band` (P9). The owner's only step: a one-time alarm-subscription link on his phone (`iem-server alarm-link`); the engineer's imported subscriptions meet the ≥ 1 gate meanwhile.

## 7. HIL (decision)

**Chosen: a runner registered on the private ops repo, on the PC, started by the guard only in `dev` after a quiet interlock (G5).** It matches §2.1/§5.2, keeps the App key and dispatch token on GitHub's side, and brings logs and checks (framework first). **Rejected:** a pull agent in the guard (re-implements job orchestration, App key on the PC); a runner on the dev box driving the PC over ssh (HIL would depend on another machine).

- **Public CI:** `bundle` (Windows, no `id-token`); `attest` (by digest, no checkout or build); `hil-dispatch` (`dev`/`main` pushes) dispatches ops `hil.yml` with sha, branch, run id and digest, using a token that can dispatch nothing else.
- **`hil.yml`:** a hosted job runs `gh attestation verify` and skips a SHA that is no longer its branch head. The PC job (label `iem-pc`, concurrency 1) downloads the zip, checks the verified digest, acts only through `iemmode install|activate|test-signal|report`, and posts `hil/iem-pc` through the ops App. Entering `dev` dispatches the newest `dev` and `main` heads once.
- **HIL v1** (at 32): versions = SHA; 120 s on the card, 0 missed and 0 resets; server ↔ engine over Windows pipes (DACL, first-instance); LAN, public host, tunnel from loopback; test signal ≤ −20 dBFS on every TX meter within its TTL; panic → exit 70, release, respawn, fade-in, fault callback < 1 ms; forced reopen ≈ 100 ms; alarm push; F30.
- **F30:** after an owner-merged `[engine]` change, `iemmode install-site` runs `check-site`, reports dropped/added ids and restarts engine (dev only) and server. HIL applies and reverts a synthetic change (a muted mix on a spare TX, a rename).

## 8. Proof

Unit- and mutation-tested on Linux: channel map, preference window, reset budget, panic record, planner, crash loop, bundles and pin, handover and exit verdicts, guard protocol. Windows effects sit behind a `Pc` trait with a fake; the hosted `windows` job builds everything and runs the pipe and `iem-win` tests. The PC runs HIL only.

## 9. Hand-offs

- **S7 (#10):** runner, `hil.yml`, `iemmode report` for the ≥ 8 h soak (S1c W6); switch timing (≤ 60 s silence); live Playwright; activity thresholds on real signal.
- **S8 (#11):** `live --build`, pin/revert, crash-loop fallbacks, the server site tables written in S6; cutover pieces (guard at logon, old autostarts off).
- **S1c (#15):** the guard owns `enter`/`exit`/`state`, drift alarm and logon reconciliation; L5 via `iem-win`.

## 10. Tests awaiting the owner's approval (one future question)

Asked once, when HIL v1 is green and before long unattended engine runs, as one ~45 min dev-time session:

1. **OS restart with the engine running** — proves the session-end path; needed early, since the owner shuts the PC down after events.
2. **Reboot with the engine parked** — does it delay or block the restart; fixes the owner's recovery (R6). Before trials.
3. **Hard kill** (once, owner at the PC) — does the driver open again without a reboot. Before trials.
4. **SEH injection (`seh_ctl`)** — release ≤ 1 s or park; `catch_unwind` cannot catch it. Before trials.
5. **Round-trip latency over the D5(b) loopback** — real in→out latency and a first look at our output on the wire. For S8 sign-off.

## 11. Deviations, risks, UNVERIFIED

- **Deviations:** the preference holds 32 only while the driver opens (I2 wording); OS glue in `iem-win`, not `iem_audio_io::os` (S1c §10); additive exit 3, role `supervisor`, `Arm`.
- **UNVERIFIED** (checked at bootstrap or first use): the driver reads the preference only at open (else `admit` refuses; fallback: 32 while iemmixer holds the card, plus a logon restore); Ctrl-Break stops the server and an idle runner (else alarm); breakaway from the task job; a Limited guard starting the elevated task; the exit id.
- **Risks:** the first real `iemmode event` happens at an event — `--dry-run` first, S1a's `spike_window.py preempt` stays the fallback until one round trip passes, last resort the owner's reboot (= `event`).
