---
paths:
  - "crates/iem-soakclient/**"
  - "scripts/iem-pc/soak_verdict.py"
  - "scripts/iem-pc/test_soak_verdict.py"
  - "scripts/iem-pc/iempc_soak.py"
  - "scripts/iem-pc/test_iempc_soak.py"
---

# Soak harness and verdict (S7, #10)

Design note `docs/superpowers/specs/2026-10-07-s7-hil-live-soak-design.md` §4 and §9, plan `docs/superpowers/plans/2026-10-07-s7-telemetry-soak.md` Tasks 5–10.

- **The client (`iem-soakclient`) reads only.** It logs in once as the engineer (`POST /api/auth`), then opens one mixer socket (`/ws/<member>?token=…&proto=2`) and one listen socket (`/ws/audio`) on one member's mix, and counts. The only frames it sends are the listen socket's `ListenStart` and, at the end, `ListenStop`: never a mixer command. Its sockets close by being dropped; nothing in it ends a process. Without `--direct` it reads `/api/site`'s `lan_url` at `--base` and goes there. That URL must be `http://`: the client speaks no TLS, and LAN 443 serves an expired certificate (`not-http`). With `--direct` everything goes to `--base` itself (CI: the test site's `lan_url` is a placeholder).
- **The PIN and site values (P6).** The PIN comes only from `IEM_SOAK_PIN` (4 to 12 digits, read with `var_os`); `--pin` is refused. A usage error names flags, never a value, and `Args` is never printed (`{:?}` would show the member and the base). Stderr carries usage messages and reason codes; stdout carries the final summary as one JSON line. The summary holds numbers and fixed codes only: `error` is a `Reason` by type, so no error text, URL or host can reach it.
- **Reason codes** (`Reason::code`, the summary's `error`; the first one stays):
  - `site-unreadable`: `/api/site` could not be read or names no `lan_url`;
  - `not-http`: the `lan_url` is not plain `http://`;
  - `login-refused`: any answered refusal (a status other than 200, an unreadable answer);
  - `not-engineer`: the login is not the engineer's (the listen socket is engineer-only);
  - `server-gone`: a login with no answer, or a socket that could not be opened within the give-up bound (the bound runs from the start for a socket never opened, so a refused upgrade ends here too, and from the close for a reopen);
  - `cpu-sets`: the CPU Sets could not be set (no login; that summary is written).
  - A new code goes into `soak_verdict.HARNESS_REASONS` in the same change: `test_the_harness_reason_codes_are_the_clients_own` reads `Reason::code`'s arms from `lib.rs` and wants the same set.
- **Bounds** (`net::Limits::default`, the consts of `net.rs`):
  - `give_up` 120 s: a socket down that long ends the run (`server-gone`). This ends a PC soak at "ide event" only if nothing answers at the client's address for 120 s: see "Ide event" below.
  - `write_every` 60 s: the summary is handed to the writer, and once more at the end.
  - `read_timeout` 500 ms: a reading thread sees the end of the run within it.
  - `idle` 10 s: a socket that sent nothing that long is dropped and opened again, so a half-open socket, or a server that stopped serving it, shows as a reconnect. The real server sends meters every 100 ms, and repeats `no_source` every 5 s only while a STARTED listen hears nothing (`listen_ws.rs`). A refused `ListenStart` (the member tap held by another mix) answers `no_source` once and the socket then stays silent, so the client reopens it about every 11 s (idle, then the backoff): it shows as many reconnects and many `no_source` answers, not only as missing frames.
  - Each HTTP request at most 10 s (`HTTP_WAIT`). Each socket's connect, handshake and write at most 5 s (`OPEN_WAIT`): `tungstenite::connect` has no bound, so the client connects with its own timeout and then calls `tungstenite::client`.
  - Reopens wait 1, 2, 4, 8, then 10 s (`backoff`). No thread polls: the clock and the reopen waits sleep on a condition variable.
  - `--seconds` is 1 to 36000 (`MAX_SECONDS`, the ops soak job's 600 min; the plan's `soak.yml` asks for `hours·3600 + 180` s, at most 9 h). The run's clock starts at the first open of either socket; the run is complete `--seconds` after it.
- **Exit codes** (`main.rs`):
  - 0: the whole run, with its final summary written;
  - 1: a run that ended early (its code on stderr), or whose final summary could not be written (`summary-unwritable` on stderr, even after a whole run: without the file the run is no evidence);
  - 2: a usage error.
  - A failed write before the end never ends the run (a reader may hold the file on Windows); the next write tries again. `write_summary` writes `<out>.tmp`, syncs it and renames it over `<out>`, so a reader sees the last whole summary, never a part.
- **CPU Sets** (`--cpu-sets 256,257`, Windows only; elsewhere a usage error): `iem_win::power::set_cpu_sets` places the process on the S1c profile's housekeeping CPU Sets before the login (P10), as the engine places itself. The `windows` job's clippy step is the first compile of that path; its test step runs the crate's tests, the fake-server ones included, on Windows sockets. `main.rs` (argv, env, CPU Set and file glue) is excluded from mutation as a whole file (`.cargo/mutants.toml`, as the guard's mains are): its `#[cfg(windows)]` call is not compiled on the Linux runners. Its exit codes are checked by `tests/fake_server.rs`, which runs the binary; its decisions stay mutated in `lib.rs`, `net.rs` and `tally.rs`.
- **Windows' socket after a receive timeout.** A read that waits out `read_timeout` returns `WouldBlock` on Unix and `TimedOut` on Windows. Both count as a wait, and the socket is read on (`net::waited`). Microsoft documents a socket whose receive timed out (`SO_RCVTIMEO`) as indeterminate: data can be lost when the timeout cancels a receive at the moment it completes. Not seen on the PC yet; the `windows` job's fake-server tests are the first runs on Windows sockets. On the PC such a loss would break the WebSocket stream; tungstenite then returns an error, and the socket is dropped and opened again, so it shows as a reconnect (red) or a gap. Unexplained reconnects in a PC soak point here first.
- **Defender.** `Set-IemDefenderExclusion` gives every root `.exe` of the active bundle a Defender process exclusion by full path (`bundles\<sha>\`), so `iem-soakclient.exe` gets one too: the attested, summed copy that runs on the PC (P5).
- **"Ide event" during a PC soak: OPEN, settle before the first PC soak (plan Task 12).** The give-up bound ends the run only while nothing answers at the client's address for 120 s. After "ide event" the predecessor app serves the band's usual address (P9), where the client's sockets go, and it accepts the token the client already holds (`iem-migrate band` imports the predecessor's JWT secret). The client would then open its sockets there and send `ListenStart{member}` again, and the predecessor answers a member's listen start by muting every other member's send to the engineer's REAPER mix until a `ListenStop` or a disconnect (`reaperiem`, the `/ws/audio` handler). Its `no_source` repeats keep the socket from going idle, so this could last to the end of `--seconds`. Whether the client outlives the runner after the guard's Ctrl-Break is UNVERIFIED (plan Task 10). The client must never reach the predecessor; how it stops is a design decision on #10 (for example: end the run at the first close of a socket, with a new reason code, since any reconnect is red anyway; or check before every reopen that `/api/version` still names the build the run started on).
- **Counting** (`tally.rs`, pure). A listen frame counts only when Opus decodes it to 960 samples per channel (the engine's `FRAME_48K`). Anything else is a decode error, and no frame for the gap clock. The server encodes every tap frame the engine sends, with no silence suppression, so a silent mix still streams frames.
- **The summary** (`Summary`, `SCHEMA` 1, rewritten every minute and at the end; struct-level `#[serde(default)]`):
  - `schema`, `build` (`GITHUB_SHA` at build time, else `local`), `complete`;
  - `seconds`: since the first open;
  - `frames`; `expected_frames`: one per 20 ms from the first frame to the end, that one included;
  - `decode_errors`;
  - `gaps`: a wait over 60 ms between two frames, or from the last frame to the end; a run without a frame is one gap as long as the run. Gaps run across reopens: a reopen's silence is a gap like any other;
  - `max_gap_ms`: the longest wait without a frame, a gap or not;
  - `first_frame_ms`: `ListenStart` to the first frame;
  - `meter_frames`: `Meters` events on the mixer socket (`classify` reads the tag first, whatever the data's shape);
  - `reconnects`: sockets opened again after a close, both sockets;
  - `no_source`: `AudioStatus` `no_source` answers;
  - `error`: the reason code, or null.
- **The verdict** (`soak_verdict.py`, pure, stdlib). `report` is the ops `soak.yml` report job's step. It reads the PC job's record directory: `result.json`; `polls.jsonl`, one `{"t", "exit", "status"}` per `iemmode status` poll; `soakclient.json`. It prints one JSON object (`conclusion`, `summary` posted as `soak/iem-pc`, `first_failure`, `numbers`). Every check runs, so the numbers stay complete; the first failure leads `red: <it>; <numbers>`. The 12 checks, in order (`test_red_names_the_first_failing_number_in_order`):
  1. polls exist; each has exit 0, mode dev, no switch and an engine (a line that is no JSON, cut short or a status `iemmode` could not answer, is an unreadable poll; blank lines are skipped);
  2. every engine runs the SHA, all under one engine pid;
  3. the poll times are finite and never go back, span the hours (8), and have no hole over 300 s;
  4. missed +0;
  5. resets +0 (4 and 5 count last poll minus first: counts before the first poll never count);
  6. both histograms in the first and the last poll, their top above 347 µs, no bucket gone back;
  7. late: intervals of 347 µs or more (the S1a p99.9 at B = 32) at most 2 ‰ of the soak's intervals;
  8. the callback's own time at p99.9 at most 83 µs: the upper edge of the bucket at rank ⌈0.999·n⌉, in integer math, at least 1 (`iem_audio_io::hist::quantile_us`'s rule);
  9. the harness is complete and ran the hours;
  10. no gap;
  11. no reconnect;
  12. at least 99 % of the expected listen frames, and at least one frame (gaps are measured between frames, so gaps 0 alone cannot tell a thinned stream from a full one).
  - Information only, never deciding: the 1.5-period `late` counter and `overruns` (last minus first), the last poll's `process_max_us`, and the `tuning drift:` alarms raised during the soak (each id once).
- **Shown values round toward red**: a value never reads as its bound. Polled hours round down at 0.01 h, a hole up at 0.1 s, the late share up at 0.001 %, the harness's seconds down at 0.1 s, the frames' share down at 0.01 %; the p99.9 is a bucket's upper edge. `report` prints strict JSON (`allow_nan=False`): a NaN raises there and never reaches the job.
- **The report mapping** (`report`; "ide event" never makes red):
  - `cancelled`: a cancelled PC job, a missing record, or a record whose reason is `left-dev`. `result.json` is read first, so a left-dev run stays cancelled whatever `soakclient.json` holds. An unreadable `result.json` of a cancelled PC job is cancelled too.
  - `failure`: any other PC failure, `red: pc job <result> (<reason>)`; an unreadable `result.json` otherwise.
  - Else the verdict. A harness file that holds no JSON is check 9's "the harness summary is unreadable".
- **Reason-code sets.** A code is printed only when known. `PC_REASONS` (`finished`, `left-dev`, `not-finished`, `bundle-not-active`, `no-client`, `harness-did-not-end`) are the ops `soak.yml` job's `result.json` reasons; `HARNESS_REASONS` are `Reason::code`'s. A new code in the ops job or in the client goes into `soak_verdict.py` in the same change, else the summary prints `unknown` or leaves it out.
- **CI's harness check** (`soak_verdict.py harness`, `_ci_check`; the `e2e` job's soak step, `.claude/rules/e2e.md`). It runs the checks 9 to 12 with CI's bounds (`--min-seconds N-1`, `--max-gaps 3`), then wants `meter_frames > 0`. CI's run against the real server is the one place that proves the client reads the server's `Meters`; the PC verdict does not judge meters (`test_the_ci_step_needs_a_meter_frame_the_pc_verdict_does_not`).
  - The step runs after Playwright: `--direct` at the job's server, `--member member9`, the engineer PIN from `E2E_ENGINEER_PIN` through `IEM_SOAK_PIN`, 600 s on a push and 120 s on a pull request.
  - The verdict runs even when the client ended early, so the log names what failed. The step fails when the verdict or the client failed (the client's exit code counts only once the verdict passed). The step's own `timeout-minutes` (15) makes a hung client a failed step, not a cancelled job, so the summary is still uploaded on failure.
  - `--max-gaps 3` is the hosted runner's tolerance for scheduling stalls; the PC verdict wants 0. Record the measured gaps of the first ten push runs on #10, and lower the tolerance if they stay at 0.
- **Known effects while a soak runs.**
  - The soak's page socket on the member's mix keeps that mix's solo past the 10 s grace (`solo.rs`: `SoloJanitor` counts any page connection on the mix). Not changed.
  - The member listen tap is one slot (`listen_ws.rs`): while the client listens to its member's mix, an engineer's Listen to another member's mix answers `no_source`.
  - So the ops soak listens to a member not active in dev (`SOAK_MEMBER`), and CI's to member9 after Playwright.
- **Drift.** The design's 10-minute drift poll does not exist: `iemmode` has no tuning-state request. The guard's own check runs hourly (`DRIFT_EVERY`), and its `tuning drift: …` alarms ride on every poll; the verdict counts them as information.
- **`dispatch-soak`** (`iempc_soak.py`, plan Task 8) is not on `dev` yet; its guards are the plan's Task 8 and Review Focus 7. Add its rules here when it lands.
