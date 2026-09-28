# iemmixer S6 — ASIO Backend, Guard and HIL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Tasks 15–19 (pushes, CI waits, the PC, owner signals, report, PR) run in the main session, never in a subagent. A subagent never touches the PC.

**Goal:** iemmixer runs on the IEM PC.
- The ASIO backend sits behind `iem_audio_io::Process` at 32 samples.
- The guard (`iemmixer-guard`, `iemmode`) switches REAPER ↔ iemmixer on the owner's "ide event" / "event skončil", with handover checks both ways.
- Bundles are installed, pinned and reverted from attested CI zips.
- The server holds the band's usual address while iemmixer runs (P9).
- HIL v1 runs through the private ops repo (ticket #9, program #1).

**Architecture:**
- **`iem-win`** (new): safe Windows glue, stubs elsewhere, plus the portable `prefwin` (shared by engine and guard, so the guard never links the ASIO host).
- **`iem-audio-io`:**
  - portable `channels.rs`, `reset.rs`, `rtpanic.rs` (+ `tests/rt.rs` with an allocation-disabling harness);
  - `asio.rs` grows into `AsioStream<P>` (measured period, `RELEASED` flag, crash-dialog suppression, `VirtualLock`).
- **`iem-engine`:** the ASIO backend, `[card]`, exit 3 (any driver-module holder, measured period ≠ 32), `--hold`/`Arm`, role `supervisor`, the card-masked HIL test signal, Windows pipe hardening, `interlock` and `check-site`.
- **`iem-guard`** (new):
  - a pure core: planner, event error policy, crash loop, bundles, verdicts, protocol, reboot mode reset;
  - `Pc` effects with a cancel token: `WinPc` on Windows, `FakePc` in tests;
  - the daemon, the `iemmode` CLI and its `--direct` fallback.
- **`iem-server`:** stage-only band activity, graceful stop, alarm link and alarm recipients, `pin_changes = false`, `CF-Connecting-IP` from the host's own addresses.
- **`iem-tray`:** without a server.
- **CI:** `bundle` (with `install --verify-only`) and `attest`. No dispatch job: the dev box dispatches HIL.
- **Ops repo:** `hil.yml` (validated inputs; `verify` → `pc` without secrets → `report`), the site tables, the PC runbook.
- **Dev box:** `scripts/iem-pc/iempc.py` (ssh control with the EVENT-NOW discipline, `dispatch-hil`, the `--direct` fallback) and `IemPc.psm1` (bootstrap on the PC).

**Tech Stack:**
- Rust 1.98.1 (edition 2024). Crates already locked: azo 0.2.1, `windows-sys` 0.61, `windows-registry` 0.100.0 (already in the closure via azo), interprocess 2.4.4, tokio, serde, toml.
- New crates:
  - `ureq` (guard HTTP; default-features off, `rustls` only if the public-host check needs TLS — decide in Task 1 from the lockfile);
  - `zip` (guard install; deflate only);
  - `sha2` (already locked).
- Windows PowerShell 5.1 (the PC and CI), Python 3.12 stdlib (dev box), GitHub Actions (hosted; plus the ops repo's runner on the PC).

**Spec:** program spec §2.1–2.5, §4, §5.2, P4–P7, P9, P10, F27, F30.
- Design note: `docs/superpowers/specs/2026-09-27-s6-asio-guard-hil-design.md` .
- Hand-offs: #9 comments (S0, S3, #20, S5, the 2026-09-27 lesson, S1a).
- S1c: `docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md` §5–§7, §10.

**Detail sources (private, never committed):**
- `~/.config/iemmixer/asio-spike.env` and `~/devel/iemmixer-ops/docs/s1a-pc-runbook.md` (PC access, driver, preference key, task names, handover values);
- `~/.config/iemmixer/event-runbook.md` (event signals, the handover lesson);
- `~/devel/iemmixer-ops/site/site.toml` (topology, stage inputs);
- `~/devel/reaperiem` at pin `03be5b97deafdc5d765516014c3f59370edd534b`, read only (its tray code: `iem-mixer/src-tauri/src/{lib,tray}.rs`; its log directory; its data writes);
- `~/.claude/work-products/iemmixer-gen2/05-fact-iem-pc.md` (processes, tasks, ports, autostarts).

## Global Constraints

- **The PC only in dev time:**
  - No PC step runs unless the owner's latest signal in this conversation is "event skončil" and `~/.config/iemmixer/EVENT-NOW` does not exist.
  - On "ide event" the session first writes the flag (`date -Iseconds > ~/.config/iemmixer/EVENT-NOW`), then runs the interim switch until Task 16 Step 3 (guard installed) and `iempc event` from Task 16 Step 3 on — one rule, used everywhere in this plan. `iempc event` itself first runs `spike_window.py preempt` when an S1a/S1c window is open, and falls back to `iemmode event --direct` when the guard is unreachable (exit 4). Then it confirms to the owner.
  - Never infer an event, never ask whether one runs, never switch on your own. A switch drill without an owner signal is switching: not allowed.
- **Nothing is force-ended (I8, P4):**
  - REAPER quits by 40004 after 40026 (and only after the app is gone, design §5.2 step 3–4); the predecessor app exits through its tray Exit command (design §5.3); the engine through `Shutdown`; the server and runner through Ctrl-Break delivered to their own console (`iem_win::console::ctrl_break`, design §5.5); the tray through `Quit`.
  - A crash dialog never holds the card: the engine suppresses Windows error UI at start (design §3).
  - The integrity scan's force-kill words never appear in `crates/`, `scripts/`, `.github/`, `e2e/` — comments included. Write "force-end" in prose.
- **The card:**
  - Only 96 kHz and preferred 32 for iemmixer (`format::admit`), and a **measured** period of 32 (design §3). The preference holds 32 only inside the open window (design §3). REAPER never starts unless the preference reads back as the recorded original — the one exception is the `[guard] on_pref_fail` choice after three failed restores at an event, recorded on #9 in Task 13 Step 2 (design §5.2 step 5).
  - `set_sample_rate`, `set_clock_source`, `open_control_panel` never appear (integrity). Every output is zeroed (A1). Dante is never touched.
- **I3:** the engine refuses while any process holds the driver module (`[card] module`; no process names — purpose-built); the guard never starts REAPER while an engine process exists or the driver module has any holder.
- **G7:** the predecessor's code, config, deployment and autostarts stay untouched, and so does the shared tunnel's ingress (read back only). We start the app only through our own `\iemmixer\iemmixer-StartApp` (its exe directly), never its launcher script.
- **P9 data:** every `dev`/`live` entry refreshes the band's identity data from the stopped predecessor (`iem-migrate band`); the server refuses PIN set/reset before cutover (`pin_changes = false`).
- **P5/G8:** only a bundle zip from a green hosted `push` run on `dev`/`main`, attested by digest, reaches the PC. The first one is installed by hand after `gh attestation verify` on the dev box; later ones through `hil.yml`. `live --build` needs `main` + green `hil/iem-pc`; `live --trial` also needs `[guard] pc_tests_passed = true`.
- **P6:** no site value in this repository. That covers driver name and module, registry key, task paths other than our own `\iemmixer\…`, the predecessor's process/log names, log line and exit command id, the app exe hash, channel numbers, track counts, hosts, users and paths. They live in the ops `site.toml` (`[card]`, `[guard]`) and `~/.config/iemmixer/iem-pc.env`. The predecessor's image, log file and log line are code identifiers already public through the import provenance (`docs/provenance/`, the S0 plan), so they are configuration, not denylist terms; the denylist keeps names, hosts, channels and credentials (ROZHODNUTÉ on #9, 2026-09-27). Tests use synthetic values (driver `Test Card`, module `testcard.dll`, RX 101–132, TX 71–93, exit id 4242).
- **Tier 0:** no local cargo compilation. Locally only `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p`, Python and `git`. Everything Rust and PowerShell is proven in hosted CI:
  - one push per cycle, one fix commit per failing cycle;
  - foreground bounded waits (≤ 9 min per Bash call), never `run_in_background`.
- **Tests:**
  - Every change ships tests that can fail; bug fixes go RED commit → GREEN commit.
  - No `#[ignore]`, skips or `continue-on-error`. The coverage floor never drops.
  - Windows-only effect code is excluded from mutation with a reason; its decisions live in portable, mutated modules.
- **RT contract (I7):** the ASIO callback allocates, locks, logs and makes syscalls never, except the driver's own `output_ready` / `sample_position`. Decode/encode buffers are preallocated in the stream.
- **Branches and identity:**
  - `dev` only until Task 19. Noreply identity.
  - Every commit carries `Refs #9` and ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - The PR body ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- **Durable state:** decisions and findings go on #9 the moment they land. Bundles, raw HIL logs and the dev-box state live outside the repo and outside `/tmp` (`~/.local/share/iemmixer/iem-pc/`, chmod 700).

## Review Focus

1. **REAPER's buffer.**
   - Expected: `prefwin::enter` refuses unless the store holds the original, and writes 32 with the original's kind. `leave` writes the original. Both read back.
   - `AsioStream` calls `leave` right after `createBuffers`, on every open, success or failure. The guard's `PrefCheck` step precedes `ReaperStart` in every plan; a failed `PrefCheck` follows `[guard] on_pref_fail` only after three restore attempts.
   - Tests: `prefwin::tests`, `plan::tests::every_event_plan_checks_the_preference_before_reaper`, `daemon::tests::a_failed_pref_check_follows_on_pref_fail`.
2. **Sound on the band's channels.**
   - Expected: every card output is zeroed each callback. TX gets processor output only after `Arm` (hold) or on respawn. The test signal cap is unchanged; the HIL test signal reaches only the card-masked outputs, and a HIL job starts only after 5 min of band quiet plus a 60 s stage-peak check. Unknown RX/TX refuse the stream.
   - Tests: `channels::tests`, engine `hold_keeps_outputs_silent_until_arm`, `hil_test_signal_reaches_only_masked_outputs`, `daemon::tests::job_begin_needs_a_quiet_stage`, HIL `test-signal`.
   - Superseded on #9 (owner, 2026-09-28): the HIL signal goes only to spare card outputs no mix uses (`[guard] hil_tx`, opened after the topology's TX), every mix's TX and both listen taps stay silent while it runs; the test is now `hil_test_signal_reaches_only_the_masked_spare_outputs` (with `a_hil_signal_leaves_the_listen_taps_silent`). The Task 5 steps below keep the original card_tx [72] / per-TX mask as a record.
3. **Nothing force-ended.**
   - Expected: no kill path in the guard. Every stop is a request plus a bounded wait, then an alarm.
   - Tests: integrity scan; `crash::tests` (no respawn after exit 2/3 or at session end); `FakePc` records no force verb (there is none to record).
4. **"ide event" at any moment.**
   - Expected: `event` pre-empts within 1 s during any waiting step (cancel token), after a mutating step otherwise. A guard restart mid-switch re-plans to `event`; after a reboot the mode is `event`; HIL jobs are refused once the switch starts; without a guard `iemmode event --direct` runs the same planner.
   - Tests: `daemon::tests::{event_preempts_a_running_dev_switch, preempt_during_interlock_starts_event_within_1s, a_reboot_resets_the_mode_to_event, direct_event_runs_without_a_guard}`, `plan::tests::a_failed_dev_switch_unwinds_to_event`.
5. **Never silence the band by our own hand.**
   - Expected: a failed engine release at "ide event" keeps a healthy iemmixer serving and never stops the server or tray or starts the app without REAPER; a dead or parked engine stops the plan and the owner gets the prepared ❓. A REAPER or app that runs but does not serve is restarted.
   - Tests: `plan::tests::{failed_engine_stop_never_starts_the_app_without_reaper, a_stale_reaper_is_restarted, an_app_that_does_not_serve_is_restarted}`, `daemon::tests::a_healthy_engine_keeps_serving_when_release_times_out`.
6. **Dev entry order.**
   - Expected: the interlock runs whenever REAPER or the app runs (any `from`); `AppStop` precedes `ReaperSaveQuit`; a dialog after 40026 aborts before 40004.
   - Tests: `plan::tests::{dev_entry_with_reaper_running_always_runs_the_interlock, the_app_stops_before_reaper_saves}`.
7. **Meter bridge and fingerprint.**
   - Expected: the bridge is triggered exactly once, only while its state is empty; any other value refuses. Every event plan ends with the S1c fingerprint (alarm only).
   - Tests: `handover::tests::bridge_*`, `plan::tests::every_event_plan_ends_with_the_fingerprint`.
8. **Predecessor exit.**
   - Expected: the precheck refuses a changed app exe hash before REAPER is touched. The guard posts only the configured command to a window of the configured class owned by the app's PID, waits on a process handle opened before the post, and needs exit code 0, ports free and no newer temp file; the log line only corroborates. Anything else aborts the switch and restarts REAPER.
   - Tests: `handover::tests::app_exit_*`, `plan::tests`.
9. **Provenance (P5/G8) and HIL.**
   - Expected: `install` verifies `SHA256SUMS` (itself exempt) and `manifest.sha` equal to the directory name and never overwrites; CI runs `install --verify-only` on every zip. `live` refuses non-`main` or non-green. `hil.yml` validates every input from `env:`; the `pc` job holds no secret.
   - Tests: `bundle::tests`, the `bundle` job's verify step.
10. **Pipes and mutex.**
    - Expected: remote clients refused, the first instance only, DACL user + SYSTEM, non-blocking readers that close a superseded connection. The guard pipe is the same; the guard mutex is `Global\iemmixer-guard`.
    - Tests: `pipes.rs` now on Windows too; `pipe::tests::sddl_*`.
11. **RT safety of the backend.**
    - Expected: `on_buffer` touches only preallocated buffers and marks its thread RT on every entry. The panic hook on RT threads writes atomics only.
    - Tests: `crates/iem-audio-io/tests/rt.rs` (`AllocDisabler`, a positive control), code review.
12. **P6:** no site value in the diff; the pre-push denylist and CI `secrets`.

## Shell variables (paste at the start of every task)

```bash
export WORK="$HOME/devel/iemmixer"
export PRED="$HOME/devel/reaperiem"
export PIN_PRED=03be5b97deafdc5d765516014c3f59370edd534b
export PRIV="$HOME/.config/iemmixer"
export OPS="$HOME/devel/iemmixer-ops"
export WP="$HOME/.claude/work-products/iemmixer-gen2"
export STATE="$HOME/.local/share/iemmixer/iem-pc"
export PC_ENV="$PRIV/iem-pc.env"
export REPO=zbynekdrlik/iemmixer
export OPS_REPO=zbynekdrlik/iemmixer-ops
export P="python3 $WORK/scripts/iem-pc/iempc.py"
```

## File Structure

```
Cargo.toml                                   members + iem-win, iem-guard; version bump (computed from main)
crates/iem-win/{Cargo.toml,src/lib.rs}       safe Windows glue; non-Windows stubs return Unsupported
crates/iem-win/src/{process,window,console,token,power,spawn,registry,errmode,sync}.rs  (cfg(windows) bodies)
crates/iem-win/src/prefwin.rs                preferred-buffer window (PrefStore trait, enter/leave) — portable, mutated
crates/iem-audio-io/src/channels.rs          topology card numbers → card indices
crates/iem-audio-io/src/reset.rs             ResetBudget, stall rule
crates/iem-audio-io/src/rtpanic.rs           RT-thread panic record (atomics only)
crates/iem-audio-io/src/period.rs            measured frames from sample positions
crates/iem-audio-io/tests/rt.rs              AllocDisabler harness for rtpanic (+ positive control)
crates/iem-audio-io/src/asio.rs              AsioStream<P>: owner thread, callback, reopen, SEH + RELEASED, session end, VirtualLock
crates/iem-audio-io/src/lib.rs               Process::discontinuity; StreamStats += frames, missed, overruns, resets, parked
crates/iem-engine/src/site.rs                [card] table (driver, module, pref, frames)
crates/iem-engine/src/engine.rs              --backend asio, --hold, interlock, check-site, exit 3
crates/iem-engine/src/control.rs             supervisor role handling, Arm, HIL test signal mask, Status fields
crates/iem-engine/src/pipe.rs                Windows listener options, SDDL, non-blocking readers
crates/iem-engine/src/rt.rs                  hold gate, discontinuity → fade-in, card-output mask
crates/iem-engine-proto/src/msg.rs           Role::Supervisor, Cmd::Arm, Cmd::HilTestSignal, Status fields (additive)
crates/iem-server/src/{activity,console}.rs  watch [activity] inputs only (RED/GREEN)
crates/iem-server/src/bin/server.rs          graceful stop (Ctrl-Break / SIGTERM); alarm-link
crates/iem-server/src/alarm_link.rs (+route) one-time alarm subscription link → alarm recipients
crates/iem-server/src/{auth,login_guard}.rs  pin_changes = false; CF-Connecting-IP from the host's own addresses only
crates/iem-core/src/config.rs                ActivityConfig.inputs; ServerConfig.pin_changes
crates/iem-guard/src/{lib,plan,crash,bundle,handover,proto,state,alarms,cancel}.rs   pure core (plan.rs carries the error policy)
crates/iem-guard/src/pc.rs                   Pc trait + FakePc (tests)
crates/iem-guard/src/win/{mod,reaper,app,card,tasks,procs}.rs                  WinPc (Windows)
crates/iem-guard/src/daemon.rs               request loop, run_switch, watches, adoption, reboot reset, --direct
crates/iem-guard/src/bin/{iemmixer-guard,iemmode}.rs
crates/iem-tray/src/{lib,tray}.rs            no server; guard status, Quit
scripts/iem-pc/IemPc.psm1, Test-IemPc.ps1    PC bootstrap functions + self-test
scripts/iem-pc/hil-v1.ps1                    HIL v1 steps (ships in the bundle, run from bundles\<sha>\)
scripts/iem-pc/iempc.py (+test_iempc.py)     dev-box control with EVENT-NOW discipline, dispatch-hil, --direct fallback
scripts/check_integrity.py (+test)           I8 words in comments too; new crate dirs covered
scripts/engine-deps-allow.txt                + iem-win (+ its closure)
.cargo/mutants.toml                          exclude Windows effect modules with reasons (prefwin stays mutated)
.github/workflows/ci.yml                     windows job widened; bundle (+ verify-only), attest
.claude/rules/guard.md                       playbook rule (paths: crates/iem-guard/**, crates/iem-win/**, scripts/iem-pc/**)
CLAUDE.md                                    router line; "ide event" = iempc event from Task 16 Step 3
docs/superpowers/specs/2026-09-27-s6-asio-guard-hil-design.md, plans/2026-09-27-s6-asio-guard-hil.md
private: $OPS/site/site.toml ([card], [guard], server tables), $OPS/.github/workflows/hil.yml,
         $OPS/docs/s6-pc-runbook.md, $PC_ENV, $PRIV/event-runbook.md, ops CLAUDE.md
```

---

### Task 1: Start — sync, version, design on the ticket, investigate the libraries

**Files:** `docs/superpowers/specs/2026-09-27-s6-asio-guard-hil-design.md`, `docs/superpowers/plans/2026-09-27-s6-asio-guard-hil.md` (copied from `$WP/15-s6-design.md` and `$WP/16-s6-plan.md`); `Cargo.toml` (version).

- [ ] **Step 1: Wait for PR #24 (S1a + S1c design) to be merged or closed.** S6 never pushes while it runs CI. Then sync and check the version:

```bash
cd "$WORK" && gh pr view 24 -R "$REPO" --json state --jq .state
git fetch origin && git checkout dev && git merge --ff-only origin/dev && git status -sb
python3 scripts/check_version.py || echo "BUMP NEEDED"
MAINV=$(git show origin/main:Cargo.toml | sed -n 's/^version = "\(.*\)"/\1/p' | head -1)
DEVV=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1); echo "main $MAINV dev $DEVV"
```

If `dev` is not strictly above `main`, the first commit sets `[workspace.package].version` to `main`'s `2.0.0-dev.N` + 1 (computed from `$MAINV`, never a hard-coded number): `chore: bump version to 2.0.0-dev.<N+1>`.

- [ ] **Step 2: Commit the design note and this plan** (`docs(s6): design note and implementation plan`). Run the denylist scan over both files first (the pre-push hook does it again). The predecessor's image, log file and log line stay out of the denylist: they are public code identifiers (Global Constraints, P6); a Slovak word that collides with a denylist name is reworded.

- [ ] **Step 3: Design summary on #9** (Slovak, plain). Write `$WP/s6-design-comment.md` covering:
  - the switch sequences;
  - the predecessor exit through its tray command;
  - the preference window;
  - the HIL decision (dispatched from the dev box, no public dispatch token);
  - the two owner steps, stated honestly: the one-time alarm link right after the first dev entry (iem-server serves it only in dev and live), and the five approval-gated tests asked right after HIL v1 is green (listed here, not asked);
  - the no-agent fallbacks (the engineer's "Späť na REAPER" button, a reboot);
  - the declared spec deviations (design §11).

  Then post it: `gh issue comment 9 -R "$REPO" --body-file "$WP/s6-design-comment.md"`.

- [ ] **Step 4: Library facts (read the locked sources, record on #9):**

```bash
R=~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f
cargo metadata --locked --format-version 1 >/dev/null   # populates the registry sources
sed -n 50,100p $R/interprocess-2.4.4/src/os/windows/named_pipe/listener/options.rs   # accept_remote, security_descriptor
sed -n 40,70p  $R/interprocess-2.4.4/src/os/windows/security_descriptor/owned.rs     # SecurityDescriptor::deserialize (SDDL)
grep -n 'pub fn set_nonblocking' $R/interprocess-2.4.4/src/os/windows/named_pipe/stream/impl.rs
ls $R | grep -E '^windows-registry-0\.100' && grep -n 'pub fn \(get\|set\)_\(u32\|string\)\|pub fn get_type\|pub fn create\|pub fn open' $R/windows-registry-0.100.0/src/key.rs
grep -n 'ctrl_break' $R/tokio-*/src/signal/windows.rs | head -3
grep -n '^name = "ureq"\|^name = "zip"\|^name = "rustls"' Cargo.lock
```

  Record on #9:
  - that `PipeListenerOptions` refuses remote clients by default, takes an SDDL descriptor, and sets the first-instance flag on the first instance;
  - the `windows-registry` value API (kind-preserving read/write for DWORD and string);
  - `tokio::signal::windows::ctrl_break`;
  - whether `ureq`/`zip`/`rustls` are already locked;
  - the `windows-sys` 0.61 feature that carries each new call (`SetErrorMode`, `WerSetFlags`, `GetExtendedTcpTable`, `AttachConsole`/`FreeConsole`/`SetConsoleCtrlHandler`, `VirtualLock`, `CreateMutexW`, `GetExitCodeProcess`): `grep -rln 'pub fn <Name>' $R/windows-sys-0.61*/src/Windows/` — the Task 2 feature list follows these paths.

  If `windows-registry` 0.100 cannot read the value kind, use `windows-sys` `RegQueryValueExW` inside `iem-win::registry` instead (the only change). Findings go on #9 the moment they land.

---

### Task 2: `iem-win` — safe Windows glue and the portable preference window

**Files:**
- Create: `crates/iem-win/Cargo.toml`, `crates/iem-win/src/lib.rs`, one module per area, and `crates/iem-win/src/prefwin.rs` (portable).
- Modify: `Cargo.toml` (members), `scripts/engine-deps-allow.txt`, `.cargo/mutants.toml`.

- [ ] **Step 1: The crate skeleton.** `Cargo.toml` (the feature list is checked against the Task 1 Step 4 paths; add any feature a call needs there):

```toml
[package]
name = "iem-win"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "iemmixer's Windows glue (S6): safe wrappers (Unsupported off Windows) and the portable preferred-buffer window"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = [
  "Win32_Foundation", "Win32_Security", "Win32_Security_Authorization", "Win32_System_Threading",
  "Win32_System_ProcessStatus", "Win32_System_Diagnostics_ToolHelp", "Win32_System_Diagnostics_Debug",
  "Win32_System_ErrorReporting", "Win32_System_Console", "Win32_System_JobObjects",
  "Win32_UI_WindowsAndMessaging", "Win32_System_Memory", "Win32_System_Power",
  "Win32_System_SystemInformation", "Win32_NetworkManagement_IpHelper", "Win32_Networking_WinSock" ] }
windows-registry = "=0.100.0"
```

`src/lib.rs`:

```rust
//! Safe Windows glue for the engine and the guard (S6 design note §3), plus the
//! portable preferred-buffer window both of them use. Every effect has a
//! portable signature; off Windows it returns `Unsupported`, so callers keep
//! their decisions testable on Linux. The only unsafe code of the workspace
//! besides `iem_audio_io::asio` lives in the `cfg(windows)` modules.

#![cfg_attr(not(windows), forbid(unsafe_code))]

use std::io;

pub mod console;
pub mod errmode;
pub mod power;
pub mod prefwin;
pub mod process;
pub mod registry;
pub mod spawn;
pub mod sync;
pub mod token;
pub mod window;

pub(crate) fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Windows only"))
}
```

- [ ] **Step 2: Module API** (each function: a `#[cfg(windows)]` body with `// SAFETY:` notes, and a `#[cfg(not(windows))]` body `crate::unsupported()`):

| Module | Functions |
|---|---|
| `process` | `exists(image: &str) -> io::Result<bool>`; `pids(image: &str) -> io::Result<Vec<u32>>` (a process-list snapshot only — the guard's once-a-second read); `module_holders(module: &str) -> io::Result<Vec<(u32, String)>>` (Toolhelp32 module snapshots of every process; access-denied processes are skipped and counted; switch steps only); `start_time(pid) -> io::Result<u64>`; `image_path(pid) -> io::Result<String>`; `Handle::open_waitable(pid) -> io::Result<Handle>` (`SYNCHRONIZE \| PROCESS_QUERY_LIMITED_INFORMATION`); `Handle::wait(Duration) -> io::Result<Option<u32>>` (the exit code via `GetExitCodeProcess` once signalled, `None` on timeout); `listening(port: u16) -> io::Result<Option<u32>>` (GetExtendedTcpTable, owning pid); `boot_time() -> io::Result<SystemTime>` |
| `power` | `set_high_priority()`, `disable_power_throttling()` (EXECUTION_SPEED and IGNORE_TIMER_RESOLUTION), `set_cpu_sets(ids: &[u32])`, `lock_min_working_set(extra_mb: usize)` (QUOTA_LIMITS_HARDWS_MIN_ENABLE, current + extra), `virtual_lock(ptr: *const u8, len: usize)` (after the working set is raised; S1c hand-off) |
| `errmode` | `quiet_crashes()`: `SetErrorMode(SEM_FAILCRITICALERRORS \| SEM_NOGPFAULTERRORBOX)` and `WerSetFlags(WER_FAULT_REPORTING_NO_UI)`, so a crash never leaves a dialog holding the card |
| `token` | `current_user_sid() -> io::Result<String>` (ConvertSidToStringSidW) |
| `window` | `find_owned(class: &str, pid: u32) -> io::Result<Option<isize>>` (EnumWindows, GetClassNameW, GetWindowThreadProcessId); `post_command(hwnd: isize, id: u16) -> io::Result<()>` (PostMessageW WM_COMMAND, wParam = id); `dialog_titles(pid) -> io::Result<Vec<String>>` (the titles of the visible top-level `#32770` windows owned by pid; #9 2026-09-28, was `has_dialog`); `SessionEndWindow` — a **hidden top-level** window on the calling thread (never `HWND_MESSAGE`: message-only windows never receive `WM_QUERYENDSESSION`) that sets an `Arc<AtomicBool>` on WM_ENDSESSION(TRUE), answers WM_QUERYENDSESSION TRUE, and holds a `ShutdownBlockReasonCreate` text while a supplied closure runs |
| `console` | `ctrl_break(pid: u32) -> io::Result<()>`. Children run with `CREATE_NO_WINDOW`, so each has its own console, and the guard has none; under a process-wide `Mutex<()>`: `FreeConsole()` → `AttachConsole(pid)` → `SetConsoleCtrlHandler(None, TRUE)` → `GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid)` → `FreeConsole()` → `SetConsoleCtrlHandler(None, FALSE)`. The handler is restored on every path (a drop guard) |
| `spawn` | `spawn_detached(cmd: &mut std::process::Command, new_group: bool) -> io::Result<std::process::Child>` (`CommandExt::creation_flags`: `CREATE_BREAKAWAY_FROM_JOB`, plus `CREATE_NEW_PROCESS_GROUP` when `new_group`, plus `CREATE_NO_WINDOW`). When the job forbids breakaway it returns the error; the caller alarms and does not start the child |
| `sync` | `GlobalMutex::try_take(name: &str) -> io::Result<Option<GlobalMutex>>` (`CreateMutexW` on `Global\<name>`; `None` when another session holds it; released on drop) |
| `registry` | `Hkcu::read(key, name) -> io::Result<(Kind, String)>`, `Hkcu::write(key, name, Kind, &str)`, `Kind { Dword, Text }`; `HkcuPref { key, name }` implements `prefwin::PrefStore` |

No function in `iem-win` ends another process in any form.

- [ ] **Step 3: `prefwin.rs`** (portable, mutation-tested; the engine's backend and the guard's `PrefCheck` both use it, so the guard never depends on `iem-audio-io`):

```rust
//! The driver's preferred buffer holds 32 only while the driver opens (S6
//! design note §3): REAPER's value stays in the registry at every other moment,
//! so a crash or power loss in dev never leaves REAPER at 32. The driver reads
//! the value when it is opened (S1a). Kind (DWORD or text) is always kept.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dword,
    Text,
}

/// A registry value as read: its kind and its decimal text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pref {
    pub kind: Kind,
    pub raw: String,
}

pub trait PrefStore {
    fn read(&mut self) -> Result<Pref, String>;
    fn write(&mut self, value: &Pref) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefError {
    Read(String),
    Write(String),
    /// The store does not hold the original: an earlier window did not close.
    NotOriginal { found: Pref },
    ReadBack { wrote: Pref, read: Pref },
}

fn write_checked(store: &mut impl PrefStore, want: &Pref) -> Result<(), PrefError> {
    store.write(want).map_err(PrefError::Write)?;
    let got = store.read().map_err(PrefError::Read)?;
    if got == *want {
        Ok(())
    } else {
        Err(PrefError::ReadBack { wrote: want.clone(), read: got })
    }
}

/// Opens the window: refuses unless the store holds `original`, then writes
/// `frames` with the original's kind and reads it back.
pub fn enter(store: &mut impl PrefStore, original: &Pref, frames: u32) -> Result<(), PrefError> {
    let now = store.read().map_err(PrefError::Read)?;
    if now != *original {
        return Err(PrefError::NotOriginal { found: now });
    }
    write_checked(store, &Pref { kind: original.kind, raw: frames.to_string() })
}

/// Closes the window: the original back, read back.
pub fn leave(store: &mut impl PrefStore, original: &Pref) -> Result<(), PrefError> {
    write_checked(store, original)
}

/// The guard's restore (design §5.2 step 5): up to `attempts` writes of the
/// original, each read back; Ok as soon as the store holds the original.
pub fn restore(store: &mut impl PrefStore, original: &Pref, attempts: u32) -> Result<u32, PrefError> {
    let mut last = PrefError::Read("no attempt".into());
    for n in 1..=attempts {
        match store.read() {
            Ok(now) if now == *original => return Ok(n - 1),
            _ => {}
        }
        match leave(store, original) {
            Ok(()) => return Ok(n),
            Err(e) => last = e,
        }
    }
    Err(last)
}
```

  Tests (`FakeStore` holding a `Pref`, counting writes, optionally failing or corrupting the n-th write):
  - `enter` writes `"32"` with kind `Dword` when the original is a DWORD `"64"`, and `Text` for a text original;
  - `enter` refuses `NotOriginal` without writing when the store holds `"32"`, and also for the same digits with the other kind;
  - a corrupting store yields `ReadBack`;
  - `leave` restores byte-for-byte (`" 64"` stays `" 64"`);
  - a write error is `Write`;
  - `restore` writes nothing when the original is already there (returns 0), succeeds on the second attempt after one failed write (returns 2), and fails after exactly 3 failing attempts.
- [ ] **Step 4: Tests.**
  - Portable: every effect stub returns `Unsupported` (one test per module on Linux); `prefwin` as above.
  - Windows (the `windows` job):
    - `current_user_sid` starts with `S-1-5-21-`;
    - `exists("definitely-not-running.exe")` is false;
    - `listening` finds a `TcpListener` bound by the test;
    - `module_holders("kernel32.dll")` contains our own pid;
    - `registry` round-trips a DWORD and a string under `HKCU\Software\iemmixer-test\<uuid>`, which the test then deletes with `windows-registry` (it removes only its own test key);
    - `GlobalMutex::try_take` twice in one test: the second is `None`;
    - `Handle::open_waitable` on a helper that exits 0 returns `Some(0)`;
    - `spawn_detached` (so the child has `CREATE_NO_WINDOW` and its own console, exactly as on the PC) + `ctrl_break` stops a child that waits on `tokio::signal::windows::ctrl_break`, and the test process itself survives: `tests/ctrl_break.rs` with a helper bin `iem-win-ctrlbreak-helper` (dev only, `[[bin]] required-features = ["test-helper"]`).
- [ ] **Step 5: Allowlist and mutation scope.**
  - `iem-win` joins the engine closure: add `iem-win` and every new name from `cargo tree -p iem-engine --target x86_64-pc-windows-msvc -e normal,build --prefix none | sort -u` to `scripts/engine-deps-allow.txt`.
  - In `.cargo/mutants.toml` `exclude_globs`: `crates/iem-win/src/{console,errmode,power,process,registry,spawn,sync,token,window}.rs` with the reason "Windows FFI wrappers, not compiled on Linux; the windows job runs their tests; decisions live in callers". `prefwin.rs` and `lib.rs` stay mutated.

---

### Task 3: Portable backend pieces in `iem-audio-io`

**Files:**
- Create: `crates/iem-audio-io/src/{channels,period,reset,rtpanic}.rs`, `crates/iem-audio-io/tests/{rt,rtpanic_hook}.rs`.
- Modify: `crates/iem-audio-io/src/lib.rs` (module list; `Process::discontinuity`; `StreamStats` fields), `crates/iem-audio-io/src/nullrt.rs` (new fields default 0), `crates/iem-audio-io/Cargo.toml` (`assert_no_alloc` dev-dependency, `iem-win`).

- [ ] **Step 1: `channels.rs`.**

```rust
//! Topology card channels (numbered from 1) → card buffer indices (S6 design
//! note §3). Built once per stream from `Topology::rx`/`tx` and the driver's
//! channel counts; a channel the card lacks refuses the stream.

use core::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelMap {
    rx: Vec<usize>,
    tx: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapError {
    Zero { side: &'static str },
    Missing { side: &'static str, channel: u16, card: usize },
}

impl fmt::Display for MapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero { side } => write!(f, "{side} channel 0: card channels count from 1"),
            Self::Missing { side, channel, card } => {
                write!(f, "{side} channel {channel} is not on the card ({card} channels)")
            }
        }
    }
}

fn indices(side: &'static str, list: &[u16], card: usize) -> Result<Vec<usize>, MapError> {
    list.iter()
        .map(|&c| {
            let i = usize::from(c).checked_sub(1).ok_or(MapError::Zero { side })?;
            if i < card { Ok(i) } else { Err(MapError::Missing { side, channel: c, card }) }
        })
        .collect()
}

impl ChannelMap {
    pub fn new(rx: &[u16], tx: &[u16], card_in: usize, card_out: usize) -> Result<Self, MapError> {
        Ok(Self { rx: indices("rx", rx, card_in)?, tx: indices("tx", tx, card_out)? })
    }
    /// Card input index of engine input slot `k`, in `Topology::rx` order.
    pub fn rx(&self) -> &[usize] { &self.rx }
    /// Card output index of engine output slot `k`, in `Topology::tx` order.
    pub fn tx(&self) -> &[usize] { &self.tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_numbers_count_from_one() {
        let m = ChannelMap::new(&[1, 128], &[71, 72], 128, 128).unwrap();
        assert_eq!((m.rx(), m.tx()), (&[0usize, 127][..], &[70usize, 71][..]));
    }

    #[test]
    fn channels_the_card_lacks_refuse() {
        assert_eq!(ChannelMap::new(&[0], &[], 128, 128), Err(MapError::Zero { side: "rx" }));
        assert_eq!(
            ChannelMap::new(&[129], &[], 128, 128),
            Err(MapError::Missing { side: "rx", channel: 129, card: 128 })
        );
        assert_eq!(
            ChannelMap::new(&[], &[129], 128, 128),
            Err(MapError::Missing { side: "tx", channel: 129, card: 128 })
        );
        assert!(ChannelMap::new(&[128], &[128], 128, 128).is_ok());
    }
}
```

- [ ] **Step 2: `period.rs`** (the measured period, design §3; the preference window moved to `iem_win::prefwin`, Task 2).

```rust
//! The period the driver really delivers, measured from the first callbacks'
//! sample positions (S6 design note §3). The driver re-reads its preference at
//! open, so the configured 32 is a request, not a fact: `Status.frames` carries
//! this measurement and anything but the expected size refuses the stream.

/// Sample positions of consecutive callbacks → the frames per callback, once
/// `need` consecutive deltas agree. `None` while undecided or inconsistent.
pub fn measured(positions: &[u64], need: usize) -> Option<u32> {
    let deltas: Vec<u64> = positions.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
    let tail = deltas.len().checked_sub(need).map(|s| &deltas[s..])?;
    let first = *tail.first()?;
    (first > 0 && tail.iter().all(|d| *d == first)).then(|| u32::try_from(first).ok()).flatten()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodVerdict {
    Undecided,
    Ok(u32),
    Wrong { expected: u32, measured: u32 },
}

pub fn verdict(positions: &[u64], need: usize, expected: u32) -> PeriodVerdict {
    match measured(positions, need) {
        None => PeriodVerdict::Undecided,
        Some(m) if m == expected => PeriodVerdict::Ok(m),
        Some(m) => PeriodVerdict::Wrong { expected, measured: m },
    }
}
```

  The owner thread (not the callback) collects the first 16 positions from an atomic the callback writes and decides; `Wrong` is an open failure (exit 3). Tests: steady 32-sample steps → `Ok(32)`; steady 64 → `Wrong`; a jittered start that settles → decided only on the settled tail; too few positions → `Undecided`; a zero delta → `Undecided`.

- [ ] **Step 3: `reset.rs`.**

```rust
//! Driver reopen budget and the stall rule (program spec §4.4; S6 design note
//! §3): at most one reopen per 5 minutes and three per process; a stall is no
//! callback for 2 s while the stream should run.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub const STALL: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Reopen,
    /// Over budget: the stream faults and the guard respawns the engine.
    Fault,
}

#[derive(Debug, Clone)]
pub struct ResetBudget {
    window: Duration,
    per_window: usize,
    per_process: u32,
    used: u32,
    recent: VecDeque<Instant>,
}

impl Default for ResetBudget {
    fn default() -> Self {
        Self::new(Duration::from_secs(300), 1, 3)
    }
}

impl ResetBudget {
    pub fn new(window: Duration, per_window: usize, per_process: u32) -> Self {
        Self { window, per_window, per_process, used: 0, recent: VecDeque::new() }
    }

    pub fn ask(&mut self, now: Instant) -> Verdict {
        while self.recent.front().is_some_and(|t| now.saturating_duration_since(*t) >= self.window) {
            self.recent.pop_front();
        }
        if self.used >= self.per_process || self.recent.len() >= self.per_window {
            return Verdict::Fault;
        }
        self.used += 1;
        self.recent.push_back(now);
        Verdict::Reopen
    }

    pub fn used(&self) -> u32 {
        self.used
    }
}

/// Whether the stream stalled: running, and no callback since `last` for `STALL`.
pub fn stalled(running: bool, last: Instant, now: Instant) -> bool {
    running && now.saturating_duration_since(last) >= STALL
}
```

  Tests use synthetic instants:
  - the first reopen is allowed and a second within 300 s faults;
  - at exactly 300 s it is allowed again;
  - the fourth reopen faults even when spaced by 301 s;
  - `stalled` is false at 1.999 s, true at 2 s, and false when not running.

- [ ] **Step 4: `rtpanic.rs`.**

```rust
//! Panics on the real-time thread (S1a finding: the default hook formats and
//! locks stderr, 4.29 ms in the callback). On a thread marked real-time the
//! hook only stores the location and a count in atomics; the control thread
//! reads and logs them. Other threads keep the previous hook.

use core::cell::Cell;
use core::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

const FILE_MAX: usize = 96;

thread_local! {
    static RT: Cell<bool> = const { Cell::new(false) };
}

static COUNT: AtomicU64 = AtomicU64::new(0);
static LINE: AtomicU32 = AtomicU32::new(0);
static COL: AtomicU32 = AtomicU32::new(0);
static FILE_LEN: AtomicUsize = AtomicUsize::new(0);
static FILE: [AtomicU8; FILE_MAX] = [const { AtomicU8::new(0) }; FILE_MAX];

/// Marks the calling thread real-time. The callback calls it on every entry:
/// after a reopen the driver may call back on a new thread (a const
/// thread-local, so no allocation on first use and one store per call).
pub fn mark_rt_thread() {
    RT.with(|f| f.set(true));
}

pub fn is_rt_thread() -> bool {
    RT.with(Cell::get)
}

/// Records a location without allocating or locking.
pub fn record(file: &str, line: u32, col: u32) {
    let bytes = file.as_bytes();
    let tail = bytes.len().saturating_sub(FILE_MAX);
    let src = bytes.get(tail..).unwrap_or_default();
    for (slot, b) in FILE.iter().zip(src) {
        slot.store(*b, Ordering::Relaxed);
    }
    FILE_LEN.store(src.len(), Ordering::Relaxed);
    LINE.store(line, Ordering::Relaxed);
    COL.store(col, Ordering::Relaxed);
    COUNT.fetch_add(1, Ordering::Release);
}

/// Installs the hook once (engine start-up, not the RT thread).
pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_rt_thread() {
            if let Some(l) = info.location() {
                record(l.file(), l.line(), l.column());
            } else {
                record("", 0, 0);
            }
        } else {
            previous(info);
        }
    }));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtPanic {
    pub count: u64,
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// The latest RT panic, for the control thread (which may allocate).
pub fn latest() -> Option<RtPanic> {
    let count = COUNT.load(Ordering::Acquire);
    (count > 0).then(|| {
        let n = FILE_LEN.load(Ordering::Relaxed).min(FILE_MAX);
        let bytes: Vec<u8> = FILE.iter().take(n).map(|b| b.load(Ordering::Relaxed)).collect();
        RtPanic {
            count,
            file: String::from_utf8_lossy(&bytes).into_owned(),
            line: LINE.load(Ordering::Relaxed),
            col: COL.load(Ordering::Relaxed),
        }
    })
}
```

  Tests:
  - unit: `record` keeps the last 96 bytes of a long path, and `latest` returns them with the count;
  - `crates/iem-audio-io/tests/rt.rs`, following `crates/iem-dsp/tests/rt.rs` (the crate has no allocation harness today, and `assert_no_alloc` in warn mode never fails on its own):

```rust
use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use iem_audio_io::rtpanic::{mark_rt_thread, record};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![0u8; 8]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 8);
}

#[test]
fn marking_and_recording_on_the_rt_thread_do_not_allocate() {
    std::thread::spawn(|| {
        reset_violation_count();
        assert_no_alloc(|| {
            mark_rt_thread();
            record(file!(), line!(), column!());
        });
        assert_eq!(violation_count(), 0, "the RT panic path allocated");
    })
    .join()
    .unwrap();
}
```

    `assert_no_alloc` becomes a dev-dependency (already locked for `iem-dsp`/`iem-engine`, same features);
  - `tests/rtpanic_hook.rs` (its own binary, since the hook is global): `install` + a panic caught by `catch_unwind` on a marked thread increments the count and prints nothing.

- [ ] **Step 5: `lib.rs`.** Add the modules, then:
  - `Process::discontinuity(&mut self) {}` with the doc "called on the callback thread before the first block after a reopen; the engine restarts its fade-in";
  - to `StreamStats`: `frames: u32` (measured; NullRt reports its block size), `missed`, `overruns`, `resets: u64`, `parked: bool` (NullRt keeps them 0/false).

  Update the module doc: S6 backend, measured period (the preference window lives in `iem_win::prefwin`).

---

### Task 4: `asio.rs` — the backend `AsioStream<P>`

**Files:** `crates/iem-audio-io/src/asio.rs` (Windows only), `crates/iem-audio-io/Cargo.toml` (`iem-win` dependency on Windows).

- [ ] **Step 1: Keep the spike API** (`Host`, `Running`, `StreamConfig`) for `examples/asio_spike.rs` unchanged. Add the backend beside it.

```rust
pub struct CardConfig {
    pub driver: String,
    pub module: String,                       // the driver DLL: any other holder refuses (I3)
    pub frames: i32,                          // 32 (I2); the measured period must match
    pub pref: Option<(String, String, iem_win::prefwin::Pref)>, // (key, value name, original); None only in tests
}

pub struct AsioStream<P: Process + 'static> { /* owner thread handle, shared atomics */ }

impl<P: Process + 'static> AsioStream<P> {
    /// Starts the owner thread; returns once the first callback ran or the open failed.
    pub fn start(card: CardConfig, rx: Vec<u16>, tx: Vec<u16>, processor: P) -> Result<Self, AsioError>;
    pub fn stats(&self) -> StreamStats;   // frames (measured), callbacks, late, missed, overruns, resets, parked, faulted, max_process_ns, fault
    pub fn session_ending(&self) -> bool; // WM_ENDSESSION seen on the owner thread
    pub fn force_reopen(&self);           // dev-only flag path (HIL); goes through ResetBudget
    pub fn lock_buffers(&self) -> io::Result<()>; // VirtualLock of inbuf/outbuf (after the engine raised its working set)
    pub fn stop(self) -> StopOutcome;     // Released | Parked
}
```

- [ ] **Step 2: The owner thread** (the only thread calling the driver):
  1. `rtpanic` and `errmode::quiet_crashes` are already installed by the engine. Create a `SessionEndWindow` (hidden top-level, never `HWND_MESSAGE`) on this thread.
  2. `open()`:
     - `prefwin::enter(store, original, 32)`, then `Host::open(driver)`, `info()`;
     - `ChannelMap::new(rx, tx, info.inputs, info.outputs)`;
     - `format::admit(rate, preferred, 32, types)`;
     - `create_buffers` for every card channel, then `prefwin::leave(store, original)`. `leave` runs on every exit of `open()` after a successful `enter`, success or not (a guard struct whose `Drop` calls `leave` and records a failure in an atomic the control loop turns into `Alarm{pref}` and exit 3).
     - Then `start()`, and decide the period from the first 16 callbacks' sample positions (`period::verdict(.., need = 8, expected = 32)`, ≤ 1 s): `Wrong`, or still `Undecided` after 1 s → `finish` and `AsioError::Refused("period")` (exit 3); `Ok(n)` → `stats.frames = n`. The module-holder check (I3) runs before `enter`, never after our own open.
  3. Loop every 5 ms:
     - pump messages;
     - if a reset or size request is flagged, or `reset::stalled(...)`, ask `ResetBudget`: `Reopen` → `finish` + `open` again with the processor carried over, then `processor.discontinuity()` before the first new block (a flag the callback consumes); `Fault` → faulted.
     - if the session is ending, set `session_ending` (the control loop does the save/fade/stop);
     - a stop request → `finish`.
  4. `finish`:
     - stop, clear `STREAM`, wait until `IN_FLIGHT == 0` (bounded `STOP_WAIT`, pumping);
     - on timeout set `parked`, leak the stream (never free under a callback), and keep the thread alive pumping messages;
     - otherwise dispose, release, drop the host, **then** set `RELEASED` (a static `AtomicBool`; cleared at the next `open`), and return the processor.
- [ ] **Step 3: The callback** (`on_buffer`, extending the spike's):
  1. `mark_rt_thread()` on **every** entry (one thread-local store; a reopen may bring a new driver thread, which would otherwise fall back to the allocating default hook).
  2. Telemetry, including the sample position into the period ring for the owner thread (first 16 callbacks only).
  3. Zero every output half.
  4. Unless faulted:
     - for each `k`, `format.decode(read(input[map.rx[k]]), &mut self.inbuf[k*frames..])`;
     - build a `Block` over `inbuf`/`outbuf` and call `processor.process(&mut block)` in `catch_unwind`; on a panic set faulted and zero `outbuf`;
     - `format.encode(&outbuf[k*frames..], output[map.tx[k]])`.
  5. `output_ready`.

  The processor lives in an `UnsafeCell` inside the stream. ASIO callbacks never overlap, and the owner thread touches the processor only after `IN_FLIGHT == 0` (`// SAFETY:` note). `inbuf`/`outbuf` are preallocated `Vec<f64>` sized at open.
- [ ] **Step 4: SEH filter** (installed once by the engine, `iem_audio_io::asio::install_seh_filter()`):
  - on an exception it sets a `SEH` atomic that the owner thread sees (it runs `finish`);
  - it waits ≤ 1 s for `RELEASED` (set only after `dispose` and after the host is dropped — `STREAM` is cleared earlier, so waiting on it would let the process end with the driver still held), then returns `EXCEPTION_CONTINUE_SEARCH`;
  - if `RELEASED` is still false, the faulting thread sleeps forever (parked);
  - with `quiet_crashes` in force, the ending process shows no Windows Error Reporting dialog that could keep the card held in session 1.

  Only the owner-approved `seh_ctl` test exercises it (design §10); the code carries no test hook beyond `--fault-injection`'s panic.
- [ ] **Step 5: `.cargo/mutants.toml`:** `asio.rs` stays excluded. The CI `windows` job runs `cargo clippy -p iem-audio-io --all-targets -D warnings` and `cargo test -p iem-audio-io`. `AsioStream::start` with driver `No Such Card` returns `AsioError::NotFound` — a test on the hosted runner, which has no ASIO driver (`NoDrivers` or `NotFound` both pass). `period.rs` carries the portable decision and stays mutated.

---

### Task 5: Engine — card, hold/arm, supervisor, interlock, check-site

**Files:** `crates/iem-engine-proto/src/msg.rs`, `crates/iem-engine/src/{site,engine,control,rt,core}.rs`, `crates/iem-engine/src/bin/iem-engine.rs`, `config/test-site.toml`, `crates/iem-engine/Cargo.toml`.

- [ ] **Step 1 (RED): proto and control tests first.**
  - `Role::Supervisor` round-trips as `"supervisor"`.
  - A supervisor may `Shutdown`, `SaveNow`, `Arm`, `GetState`, `Ping`, `HilTestSignal`, and the test-signal and fault ops (still refused without their flags), and it receives the `Meters` events. It gets `NotController` for mix changes; a `control` client gets `NotSupervisor` for `Arm` and `HilTestSignal`.
  - A second supervisor replaces the first (the first gets `Superseded`); `control` is untouched.
  - `hold_keeps_outputs_silent_until_arm`: `Offline` with `Options { hold: true }` renders zeros until an `RtOp::Arm`, then fades in over 500 ms.
  - `discontinuity_restarts_the_fade_in`.
  - `hil_test_signal_reaches_only_masked_outputs` (superseded on #9, 2026-09-28: spare outputs only, see the rule under "Sound on the band's channels"): `Offline` with a `HilTestSignal { input, hz, dbfs, ttl_s, card_tx: [72] }` → the mix meters of every TX whose mix routes that input show the signal (internal routing), the rendered card output 72 carries it, every other card output is exactly zero until the TTL ends, then normal output resumes; `dbfs` above the existing test-signal cap is refused.
  - `Status` serialises `frames`, `missed`, `overruns`, `resets`, `parked`, and an old client ignores them (additive).

  Commit: `test(engine): [red] supervisor role, hold until arm, discontinuity fade-in, HIL output mask`.
- [ ] **Step 2 (GREEN): implement.**
  - `Role::Supervisor`; `Cmd::Arm`, `Cmd::HilTestSignal { input, hz, dbfs, ttl_s, card_tx: Vec<u16> }` (add `"arm"`, `"hil_test_signal"` to `OPS`); `Cmd::is_supervisor(&self)`.
  - `Control` keeps `supervisor: Option<u64>`.
  - `rt::Options { hold }`: the fade stays at 0 until `RtOp::Arm`. `Processor::discontinuity` restarts the fade-in.
  - `rt`: a preallocated card-output mask (a fixed `[bool; MAX_TX]`, set through `RtOp`, no allocation) zeroes every unmasked output slot at encode while the HIL TTL runs; the existing test-signal generator and cap are reused.
  - `Status` fields filled from `StreamStats` (`frames` = the measured period).

  Commit: `feat(engine): [green] supervisor role, hold until arm, discontinuity fade-in, HIL output mask`.
- [ ] **Step 3: `[card]` in the site** (`site.rs`, `deny_unknown_fields` like `[engine]`):

```toml
# config/test-site.toml (synthetic)
[card]
driver = "Test Card"
module = "testcard.dll"
frames = 32
pref_key = 'Software\ASIO\Test Card'
pref_name = "PrefBuffSize"
pref_original = { kind = "dword", raw = "64" }
```

  `frames` must be 32 (I2; anything else is a site error). The table is optional for `nullrt` and required for `--backend asio`.
- [ ] **Step 4: `run --backend asio|nullrt [--hold]`** (Windows only for `asio`; elsewhere a usage error).
  - `run` starts with `iem_win::errmode::quiet_crashes()`, `rtpanic::install()`, `asio::install_seh_filter()`, and `iem_win::power::{set_high_priority, disable_power_throttling}` (+ CPU Sets from `[card] cpu_sets` when present, S1c).
  - After 5 s of streaming it calls `lock_min_working_set(64)`, then `AsioStream::lock_buffers()` (`VirtualLock` of the preallocated RT buffers; a failure is logged and reported in `Status`, never fatal).
  - Refusals map to the new `EngineError::Card(String)` → exit 3: any holder of `[card] module` (`iem_win::process::module_holders`, I3 — purpose-built, no process names; this also covers a spike window), `AsioError::{NoDrivers, NotFound, Refused}` (incl. a measured period ≠ 32), `ChannelMap` errors, `PrefError`.
  - `AsioDriver` implements `control::Driver`; `Control::tick` also exits through the shutdown path when `session_ending()`.
  - Update `USAGE` and the exit-code line: `0 shut down, 1 i/o, 2 usage or site, 3 card refused, 70 RT fault`.
- [ ] **Step 5: `interlock --site <site.toml> --seconds 60`** (Windows):
  - opens the card with a processor that only records peaks of the `[activity] inputs` (engine input ids → their RX channels) and writes nothing (outputs stay zero);
  - uses `telemetry::ActivityGuard` (−50 dBFS, 3 s consecutive);
  - prints a JSON line `{"quiet": bool, "loudest": [[channel, dbfs], …5]}`; exit 0 quiet, 5 activity, 3 card refused.

  **`check-site --site <file>`:** loads and compiles (I4), prints `{"topology": hash, "inputs": n, "groups": n, "mixes": n}`; exit 0 or 2.
- [ ] **Step 6:** the parser tests for the new flags and subcommands (portable). `examples/bench.rs` is unchanged. Commit: `feat(engine): ASIO backend, card table, interlock and check-site (S6)`.

---

### Task 6: Engine — Windows pipes hardened

**Files:** `crates/iem-engine/src/pipe.rs`, `crates/iem-engine/tests/pipes.rs`, `.github/workflows/ci.yml` (`windows` job).

- [ ] **Step 1: The listener** on Windows via `interprocess::os::windows::named_pipe::PipeListenerOptions`:

```rust
#[cfg(windows)]
fn sddl_for(sid: &str) -> String {
    // Protected DACL: the logged-on user and SYSTEM, nobody else (S3 hand-off, spec §2.3).
    format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)")
}
```

  - `SecurityDescriptor::deserialize(&U16CString::from_str(sddl_for(&iem_win::token::current_user_sid()?)))`;
  - `accept_remote: false`; first instance (the default path sets the flag);
  - a name that already exists → `EngineError::Io` with "pipe name taken" (exit 1; the guard alarms).

  A portable test for `sddl_for`'s shape, and a helper `pipe::sddl_is_private(&str) -> bool` used by HIL.
- [ ] **Step 2: Readers.** On Windows, every accepted stream gets `set_nonblocking(true)`. `read_loop` treats `WouldBlock` like the Unix receive timeout (sleep 10 ms, check the close flag). This is the same loop shape, so the Unix tests cover the logic.
- [ ] **Step 3:** remove the `cfg(unix)` gate from `tests/pipes.rs` where the only reason was the missing timeout (pipe names come from `control_name`). The `windows` job runs `cargo test --locked -p iem-engine --test pipes`. Add Windows tests:
  - a second listener on the same name fails;
  - the pipe's security descriptor read back (`GetSecurityInfo` via `iem_win::token::pipe_sddl(name)`) contains only the user SID and `SY`.

  Commit: `feat(engine): Windows pipes — private DACL, first instance, non-blocking readers (S3 hand-off)`.

---

### Task 7: Server — stage-only band activity, graceful stop, alarm link and recipients, PIN freeze, tunnel peer

**Files:** `crates/iem-core/src/config.rs`, `crates/iem-server/src/{activity,console,auth,login_guard,notify}.rs`, `crates/iem-server/src/bin/server.rs`, `crates/iem-server/src/lib.rs`, `crates/iem-server/src/alarm_link.rs`, `crates/iem-ui` (alarm page), `config/test-site.toml`, `e2e/`.

- [ ] **Step 1 (RED): band activity ignores non-stage inputs** (S1a finding).
  - `ActivityConfig` gains `inputs: Vec<String>` (engine input ids; default empty = every input with category `mics`).
  - `console::tests::program_input_signal_does_not_raise_band_activity`: a meter stream where only a `tech` input (the synthetic `content`) sits at −1 dBFS for 300 s → no `BandActivity{active:true}`.
  - `…stage_input_activity_still_raises_it`: the same on `mic1` → on after 120 s.

  Commit: `test(server): [red] band activity must ignore the program input`.
- [ ] **Step 2 (GREEN):** `max_input_peak(&m)` becomes `max_watched_peak(&m, &watched)`, where `watched` is the resolved input indices (explicit list or category `mics`). Unknown ids are reported at connect and left out. Commit: `fix(server): [green] band activity watches only the stage inputs`.
- [ ] **Step 3: Graceful stop.** `run_server` awaits `shutdown_signal()`:
  - Unix: SIGTERM or SIGINT; Windows: `tokio::signal::windows::ctrl_break()` or `ctrl_c()`;
  - then `axum::serve(...).with_graceful_shutdown(...)` with a 5 s bound; the backup daemon and the engine client close; exit 0.

  Test (Unix): spawn the server binary with a temp config, send SIGTERM, expect exit 0 within 6 s and the port free. The Windows variant is in the `windows` job: the server is started with `iem_win::spawn::spawn_detached` (so `CREATE_NO_WINDOW`, its own console — the PC's exact shape) and stopped with `iem_win::console::ctrl_break`.
- [ ] **Step 4: Alarm link.**
  - `iem-server alarm-link [--ttl-h 24]` writes one random 128-bit token (hash only) to `alarm_link.json` next to the config and prints `https://<https_domain>/alarms?t=<token>`.
  - `POST /api/alarms/subscribe {token, subscription}` accepts it once, unexpired, appends the subscription to `alarm_subscriptions.json` (atomic write) and deletes the token.
  - UI route `/alarms`: one button "Povoliť upozornenia" → push permission → POST, with a result message.
  - Tests: unit (token single use, expiry, bad token 403 without a timing difference beyond the hash compare), E2E (mock push subscription, zero console errors).

  Commit: `feat(server): one-time alarm subscription link for the owner (S6 bootstrap)`.
- [ ] **Step 5: Alarm recipients vs the engineer** (P9, design §5.4; deviation from spec §4.2 recorded in the design note).
  - `iem-server notify --to alarm|band-activity <title> <body>`: `alarm` sends only to `alarm_subscriptions.json` (exit 3 when it is empty, so the guard's precheck and its alarm path can see it); `band-activity` sends only to the engineer's subscriptions. No other audience exists.
  - `iem-server notify --count alarm` prints the number of alarm recipients (the guard's precheck gate for `live` and trials, ≥ 1; a `dev` entry only names a missing one, #9 2026-09-28).
  - Verify `notify::run_cli` only reads subscriptions (no write). If it prunes expired ones, keep that — it is the same atomic write the server uses — and document it in `server-engine.md`.
  - Tests: an alarm with engineer subscriptions only reaches nobody and exits 3; a band-activity notice never reaches an alarm recipient.
- [ ] **Step 6: PIN changes frozen before cutover** (P9, design §5.4).
  - `ServerConfig.pin_changes: bool` (default `true` for the library; the guard writes `false` into every `dev`/trial config).
  - `auth`: with `pin_changes = false`, the change and reset routes answer 409 with the Slovak text "PIN sa zatiaľ mení v pôvodnej aplikácii" and change nothing.
  - Tests (RED first): `pin_change_is_refused_while_frozen`, `pin_reset_is_refused_while_frozen`, `login_still_works_while_frozen`; an E2E case that the UI shows the text and no console error.
- [ ] **Step 7: The tunnel peer** (design §6; the ingress itself is never edited).
  - `login_guard` keys attempts by the socket peer. Only when the peer is one of the host's own interface addresses (read at start: loopback plus the host's own IPs) does it take `CF-Connecting-IP` as the client address; from any other peer the header is ignored.
  - Tests: a loopback peer with `CF-Connecting-IP: 198.51.100.7` is limited per that address; a peer `192.0.2.50` (not the host) sending the same header is limited per `192.0.2.50` (the forged header changes nothing); a malformed header falls back to the peer.

  Commit per step, RED before GREEN for Steps 1–2 and 6.

---

### Task 8: `iem-guard` — the pure core

**Files:** create `crates/iem-guard/{Cargo.toml,src/lib.rs,src/plan.rs,src/crash.rs,src/bundle.rs,src/handover.rs,src/proto.rs,src/state.rs,src/alarms.rs,src/cancel.rs}`.

- [ ] **Step 1: `Cargo.toml`.**
  - Permissive; deps: `serde`, `serde_json`, `toml`, `sha2`, `thiserror`, `tracing`, `iem-win` (effects and `prefwin` — never `iem-audio-io`, so the guard does not link the ASIO host or azo), `interprocess` (guard pipe, sync), `zip` (deflate), `ureq`.
  - Two bins: `iemmixer-guard`, `iemmode`.
  - Not in the engine closure: `check_engine_deps.py` is unaffected.
- [ ] **Step 2: `plan.rs`** — the planner and the event error policy.

```rust
//! The switch planner and its error policy (S6 design note §5.2). Pure: facts
//! in, ordered steps out. Every step re-reads its own facts before it acts, so
//! re-running a plan is safe; a failed or interrupted switch into dev/live
//! unwinds with `plan(current, Mode::Event, facts)`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Event,
    Dev,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Precheck,
    Interlock,
    AppStop,
    ReaperSaveQuit,
    TuningEnter,
    Data,
    EngineStart,
    EngineArm,
    ServerStart,
    TrayStart,
    IdentityCheck,
    RunnerStart,
    JobsCancel,
    RunnerStop,
    EngineStop,
    /// Inserted by the runner after a failed `EngineStop` (never planned).
    EngineHealth,
    ServerStop,
    TrayStop,
    TuningExit,
    PrefCheck,
    HolderGone,
    ReaperStart,
    ReaperHandover,
    AppStart,
    AppHandover,
    Fingerprint,
}

/// Read once per plan (module holders and ports included); the once-a-second
/// watch reads the process list only (design §5.1, P10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    pub reaper: bool,
    pub app: bool,
    pub engine: bool,
    pub server: bool,
    pub tray: bool,
    pub runner: bool,
    /// REAPER holds the driver module: it opened the card.
    pub reaper_holds_module: bool,
    /// The app owns ports 80/443 (the listening pid is the app's).
    pub app_serves: bool,
    /// A process other than REAPER and our engine holds the driver module.
    pub other_module_holder: bool,
    /// `live` before cutover (a rehearsal): the band is there on purpose.
    pub trial: bool,
    /// Owner-instructed `--force`: skips the interlock only.
    pub force: bool,
}

pub const FACT_BITS: u32 = 11;

impl Facts {
    /// Every combination for the exhaustive tests (2^11 = 2048).
    pub fn from_bits(b: u32) -> Self {
        let bit = |n: u32| b & (1 << n) != 0;
        Self {
            reaper: bit(0),
            app: bit(1),
            engine: bit(2),
            server: bit(3),
            tray: bit(4),
            runner: bit(5),
            reaper_holds_module: bit(6),
            app_serves: bit(7),
            other_module_holder: bit(8),
            trial: bit(9),
            force: bit(10),
        }
    }
}

fn stop_iemmixer(f: &Facts, out: &mut Vec<Step>) {
    if f.runner {
        out.extend([Step::JobsCancel, Step::RunnerStop]);
    }
    if f.engine {
        out.push(Step::EngineStop);
    }
    if f.server {
        out.push(Step::ServerStop);
    }
    if f.tray {
        out.push(Step::TrayStop);
    }
}

pub fn plan(from: Mode, to: Mode, f: &Facts) -> Vec<Step> {
    let mut out = Vec::new();
    match to {
        Mode::Event => {
            stop_iemmixer(f, &mut out);
            out.extend([Step::TuningExit, Step::PrefCheck]);
            if f.other_module_holder {
                out.push(Step::HolderGone);
            }
            // A REAPER that runs without the card (its time trigger, or a start
            // while our engine held it) is saved, quit and started again.
            let reaper_ok = f.reaper && f.reaper_holds_module;
            if f.reaper && !reaper_ok {
                out.push(Step::ReaperSaveQuit);
            }
            if !reaper_ok {
                out.push(Step::ReaperStart);
            }
            out.push(Step::ReaperHandover);
            // An app that runs but does not serve (redeployed while iem-server
            // held the ports) is stopped through its tray command and started again.
            let app_ok = f.app && f.app_serves;
            if f.app && !app_ok {
                out.push(Step::AppStop);
            }
            if !app_ok {
                out.push(Step::AppStart);
            }
            out.extend([Step::AppHandover, Step::Fingerprint]);
        }
        Mode::Dev | Mode::Live => {
            out.push(Step::Precheck);
            let band_there = to == Mode::Live && f.trial;
            // A running REAPER or app means the band's system is up, whatever
            // the saved mode says (a reboot restores `event`, spec §4.1).
            let from_band =
                f.reaper || f.app || from == Mode::Event || (from == Mode::Live && to == Mode::Dev);
            if from_band && !band_there && !f.force {
                out.push(Step::Interlock);
            }
            // The app first: after it nothing writes to REAPER, so the save
            // cannot be dirtied before the quit (deviation from spec §4.3).
            if f.app {
                out.push(Step::AppStop);
            }
            if f.reaper {
                out.push(Step::ReaperSaveQuit);
            }
            stop_iemmixer(f, &mut out);
            out.extend([
                Step::TuningEnter,
                Step::Data,
                Step::EngineStart,
                Step::EngineArm,
                Step::ServerStart,
                Step::TrayStart,
                Step::IdentityCheck,
            ]);
            if to == Mode::Dev {
                out.push(Step::RunnerStart);
            }
        }
    }
    out
}

/// `[guard] on_pref_fail` — required in the site, decided on #9 (Task 16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefFail {
    StartReaperWithAlarm,
    KeepReaperDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Dead,
    Parked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    /// Into dev/live: alarm, then the event plan.
    Unwind,
    /// Event plan: alarm and go on with the next step.
    Continue,
    /// Event plan: alarm and drop these later steps (they would act on a stale process).
    Skip(&'static [Step]),
    /// Event plan: iemmixer keeps serving the band; alarm; the plan ends here.
    KeepServing,
    /// Event plan: alarm, the plan ends here, the agent sends the prepared ❓.
    StopAskOwner,
}

/// What a failed step means. `health` is read only after a failed `EngineStop`.
pub fn on_error(to: Mode, step: Step, health: Option<Health>, pref_fail: PrefFail) -> OnError {
    if to != Mode::Event {
        return OnError::Unwind;
    }
    match step {
        Step::EngineStop | Step::EngineHealth => match health {
            Some(Health::Healthy) => OnError::KeepServing,
            _ => OnError::StopAskOwner,
        },
        Step::PrefCheck => match pref_fail {
            PrefFail::StartReaperWithAlarm => OnError::Continue,
            PrefFail::KeepReaperDown => OnError::StopAskOwner,
        },
        Step::HolderGone | Step::ReaperSaveQuit | Step::ReaperStart => OnError::StopAskOwner,
        Step::AppStop => OnError::Skip(&[Step::AppStart]),
        _ => OnError::Continue,
    }
}
```

  Tests (exact bodies for the safety-critical ones):

```rust
fn at(p: &[Step], s: Step) -> Option<usize> {
    p.iter().position(|x| *x == s)
}

fn every(mut check: impl FnMut(Mode, Facts, Vec<Step>)) {
    for bits in 0..(1u32 << FACT_BITS) {
        let f = Facts::from_bits(bits);
        for from in [Mode::Event, Mode::Dev, Mode::Live] {
            check(from, f, plan(from, Mode::Event, &f));
        }
    }
}

#[test]
fn every_event_plan_checks_the_preference_before_reaper() {
    every(|from, f, p| {
        let pref = at(&p, Step::PrefCheck).expect("PrefCheck in every event plan");
        for s in [Step::ReaperStart, Step::ReaperHandover, Step::AppStart] {
            if let Some(i) = at(&p, s) {
                assert!(pref < i, "{from:?} {f:?}: {s:?} before PrefCheck");
            }
        }
        if let Some(e) = at(&p, Step::EngineStop) {
            assert!(e < pref, "{from:?} {f:?}: EngineStop after PrefCheck");
        }
    });
}

#[test]
fn every_event_plan_ends_with_the_fingerprint() {
    every(|_, _, p| assert_eq!(p.last(), Some(&Step::Fingerprint)));
}

#[test]
fn failed_engine_stop_never_starts_the_app_without_reaper() {
    for pf in [PrefFail::StartReaperWithAlarm, PrefFail::KeepReaperDown] {
        for h in [None, Some(Health::Healthy), Some(Health::Dead), Some(Health::Parked)] {
            let e = on_error(Mode::Event, Step::EngineStop, h, pf);
            assert!(matches!(e, OnError::KeepServing | OnError::StopAskOwner), "{h:?}: {e:?}");
        }
    }
    assert_eq!(
        on_error(Mode::Event, Step::EngineStop, Some(Health::Healthy), PrefFail::KeepReaperDown),
        OnError::KeepServing
    );
}

#[test]
fn dev_entry_with_reaper_running_always_runs_the_interlock() {
    for from in [Mode::Event, Mode::Dev, Mode::Live] {
        for f in [Facts { reaper: true, ..Facts::default() }, Facts { app: true, ..Facts::default() }] {
            let p = plan(from, Mode::Dev, &f);
            assert!(at(&p, Step::Interlock).is_some(), "{from:?} {f:?}");
        }
    }
}

#[test]
fn the_app_stops_before_reaper_saves() {
    let f = Facts { reaper: true, app: true, reaper_holds_module: true, app_serves: true, ..Facts::default() };
    let p = plan(Mode::Event, Mode::Dev, &f);
    let (i, a, r) = (at(&p, Step::Interlock).unwrap(), at(&p, Step::AppStop).unwrap(), at(&p, Step::ReaperSaveQuit).unwrap());
    assert!(i < a && a < r, "{p:?}");
}
```

  And (bodies follow the same shape):
  - `a_trial_skips_the_interlock`; `force_skips_only_the_interlock`;
  - `live_to_dev_runs_the_interlock`; `dev_to_live_does_not` (REAPER and app down);
  - `the_runner_starts_only_in_dev`;
  - `event_in_event_only_checks`: REAPER and app up and serving → `[TuningExit, PrefCheck, ReaperHandover, AppHandover, Fingerprint]`;
  - `a_stale_reaper_is_restarted`: `reaper` without `reaper_holds_module` → `ReaperSaveQuit` then `ReaperStart`, both after `PrefCheck`;
  - `an_app_that_does_not_serve_is_restarted`: `app` without `app_serves` → `AppStop` then `AppStart`;
  - `another_holder_blocks_reaper`: `other_module_holder` → `HolderGone` before any REAPER step, and `on_error(Event, HolderGone, ..) == StopAskOwner`;
  - `a_failed_pref_check_follows_the_choice`: `Continue` for `StartReaperWithAlarm`, `StopAskOwner` for `KeepReaperDown`;
  - `a_failed_app_stop_skips_the_app_start`;
  - `every_dev_or_live_error_unwinds`: `on_error(Dev|Live, s, ..) == Unwind` for every step;
  - `a_failed_dev_switch_unwinds_to_event`: the facts after a failure at `EngineStart` (REAPER and app down, no engine) → the event plan starts REAPER, then the app.
- [ ] **Step 3: `crash.rs`.**

```rust
//! What the guard does when the engine exits (spec §2.4, §4.1; design §5.4).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::plan::Mode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum After {
    Respawn(Duration),
    /// 0 = asked to stop; 2/3 = configuration or card: respawning cannot help.
    Stay { alarm: Option<&'static str> },
    /// Crash loop before cutover or in dev: back to REAPER.
    ToEvent,
    /// Crash loop in prod: the previous pin's engine.
    PreviousPin,
}

pub struct CrashLoop {
    exits: VecDeque<Instant>,
}

impl CrashLoop {
    pub const WINDOW: Duration = Duration::from_secs(600);
    pub const LIMIT: usize = 3;

    pub fn new() -> Self {
        Self { exits: VecDeque::new() }
    }

    /// Records an abnormal exit; whether this makes a loop.
    pub fn record(&mut self, now: Instant) -> bool {
        self.exits.push_back(now);
        while self.exits.front().is_some_and(|t| now.saturating_duration_since(*t) >= Self::WINDOW) {
            self.exits.pop_front();
        }
        self.exits.len() >= Self::LIMIT
    }
}

pub fn backoff(abnormal_in_window: usize) -> Duration {
    let s = 1u64 << abnormal_in_window.saturating_sub(1).min(4);
    Duration::from_secs(s.min(10))
}

pub fn after_exit(code: Option<i32>, mode: Mode, prod: bool, session_ending: bool, looped: bool, n: usize) -> After {
    match code {
        Some(0) => After::Stay { alarm: None },
        Some(2) => After::Stay { alarm: Some("engine site or usage error") },
        Some(3) => After::Stay { alarm: Some("the card refused the engine") },
        _ if session_ending => After::Stay { alarm: None },
        _ if looped && mode == Mode::Live && prod => After::PreviousPin,
        _ if looped => After::ToEvent,
        _ => After::Respawn(backoff(n)),
    }
}
```

  Tests:
  - `backoff` gives 1, 2, 4, 8, 10, 10;
  - three exits within 600 s loop, and the third at 600 s does not;
  - codes 0/2/3 never respawn;
  - session end never respawns;
  - a looped trial or dev goes to event, a looped prod goes to the previous pin;
  - `None` (a signal or unknown death) respawns.
- [ ] **Step 4: `bundle.rs`:** manifest, records, install verification, pin rules.
  - `Manifest { sha, branch, version, run }`;
  - `Record { sha, branch, run, installed_at, hil: Hil }`, with `Hil::{Pending, Green, Red}`;
  - `valid_sha` (40 lowercase hex);
  - `parse_sums(text) -> Result<Vec<(String, String)>, String>` (the `sha256  name` lines of `SHA256SUMS`; names without `/`, `\`, or `..`);
  - `verify(dir_files: &[(name, sha256)], sums, manifest, dir_sha)` — every summed file present and equal, no unsummed file except `SHA256SUMS` itself (it cannot sum itself), `manifest.sha == dir_sha`;
  - `REQUIRED`: the bundle must contain `iem-engine.exe`, `iem-server.exe`, `iemmixer-guard.exe`, `iemmode.exe`, `iem-tray.exe`, `iem-migrate.exe`, `hil-v1.ps1`, `IemPc.psm1`, `manifest.json`;
  - `may_go_live(&Record) -> Result<(), String>` (`main` + `Green`);
  - `Pins { current: Option<String>, previous: Option<String> }` with `promote(sha)` and `revert()`.

  Tests cover each refusal, a missing required file, `SHA256SUMS` present but unsummed (accepted), any other unsummed file (refused), plus path traversal in sums. The zip extraction (Task 10) uses `zip` with `enclosed_name()` and refuses any entry outside the directory; it accepts both separators `Compress-Archive` may write.
- [ ] **Step 5: `handover.rs`.**

```rust
//! Handover verdicts (design §5.2, §5.3; the 2026-09-27 lesson on #9).

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bridge {
    Running,
    TriggerOnce,
    Refuse(String),
}

/// The meter bridge may be triggered only while its state is empty: a second
/// trigger opens a blocking dialog in REAPER.
pub fn bridge(state: &str) -> Bridge {
    match state {
        "1" => Bridge::Running,
        "" => Bridge::TriggerOnce,
        other => Bridge::Refuse(format!("meter bridge state {other:?}: not triggered")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReaperFacts {
    pub tracks: Option<u32>,
    pub expected_tracks: u32,
    pub dialog: bool,
    pub heartbeat_advanced: bool,
    pub holds_module: bool,
    /// Stage-input peaks in dBFS (REAPER meters).
    pub peaks: Vec<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audio {
    Confirmed,
    Unconfirmed,
}

pub fn reaper_handover(f: &ReaperFacts) -> Result<Audio, Vec<String>> {
    let mut bad = Vec::new();
    if f.tracks != Some(f.expected_tracks) {
        bad.push(format!("tracks {:?}, expected {}", f.tracks, f.expected_tracks));
    }
    if f.dialog {
        bad.push("a REAPER dialog is open".into());
    }
    if !f.heartbeat_advanced {
        bad.push("the meter heartbeat does not advance".into());
    }
    if !f.holds_module {
        bad.push("REAPER does not hold the driver module".into());
    }
    if !bad.is_empty() {
        return Err(bad);
    }
    Ok(if f.peaks.iter().any(|p| p.is_finite() && *p > -150.0) { Audio::Confirmed } else { Audio::Unconfirmed })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppExit {
    /// From a handle opened before the post: `None` = not signalled within 30 s.
    pub exit_code: Option<u32>,
    pub ports_free: bool,
    pub newer_temp: bool,
    /// Corroboration only: the app's buffered logger may never flush this line.
    pub logged: bool,
}

pub fn app_exit(f: AppExit) -> Result<(), Vec<&'static str>> {
    let mut bad = Vec::new();
    match f.exit_code {
        None => bad.push("the app did not exit within 30 s"),
        Some(0) => {}
        Some(_) => bad.push("the app exited with a non-zero code: not the tray Exit path"),
    }
    if !f.ports_free { bad.push("ports 80/443 still held"); }
    if f.newer_temp { bad.push("a temp file newer than the command: a write was cut"); }
    if bad.is_empty() { Ok(()) } else { Err(bad) }
}

/// The precheck's binary identity (design §5.3): refuse before REAPER is quit.
pub fn app_binary(recorded: &str, now: &str) -> Result<(), String> {
    if recorded.eq_ignore_ascii_case(now) {
        Ok(())
    } else {
        Err(format!("predecessor exe changed ({now}); the exit id must be re-derived"))
    }
}
```

  Tests:
  - `bridge_*`: `"1"`, `""`, `"0"`, `"2"`;
  - every single failure of `reaper_handover` is named, all −∞ → `Unconfirmed`, one stage at −40 → `Confirmed`;
  - every `app_exit` field on its own; `logged == false` alone still passes (corroboration only); exit code 1 fails;
  - `app_binary`: equal (case-insensitive) passes, different refuses.
- [ ] **Step 6: `proto.rs`:** guard pipe messages (u32 LE length + JSON ≤ 64 KiB, like the engine).

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Event { dry_run: bool },
    Dev { build: Option<String>, force: bool, dry_run: bool },
    Live { build: String, trial: bool, dry_run: bool },
    Install { zip: String },
    Activate { sha: String },
    /// Card-masked to `[guard] hil_tx` by the guard (design §4); `card_tx` from the
    /// request is ignored.
    TestSignal { input: String, dbfs: f64, ttl_s: f64 },
    Report { sha: String, hil: String, detail: String },
    /// Refused unless dev, not switching, band activity quiet for 5 min and a
    /// 60 s stage-input peak check from the engine's meters is quiet.
    JobBegin { run: u64 },
    JobEnd { run: u64 },
    InstallSite { path: String },
    ForceReopen,
    /// Dev only: stop the idle runner (bootstrap check, Task 16).
    RunnerStop,
    /// Dev only: start `\iemmixer\iemmixer-probe` from the guard (Task 16, design §5.1).
    ProbeTask,
    /// Dev only: the teardown half of the event plan without REAPER, then back
    /// into dev (Task 17; never starts REAPER, so it is not a switch).
    RehearseTeardown,
    AlarmTest,
    AlarmAck { id: u64 },
    Quit,
    Subscribe,
}
```

  Replies are `{ok, mode, switching, alarms, detail}`. Tests: round trip of every variant; oversize and garbage frames are refused.
- [ ] **Step 7: `state.rs`, `alarms.rs`.**
  - The persistent guard state: `mode`, `switching: Option<{from, to, done: Vec<Step>, started}>`, `written_at` (seconds since the epoch), `pids`, `bundles`, `pins`, `interlock_retry: Option<{target, refusals, next_at}>`. It is written atomically (temp, fsync, rename) and loaded with defaults.
  - The reboot rule (design §5.2, spec §4.1):

```rust
/// Whether a starting guard must forget its saved mode: after a reboot, or
/// when the band's system is up without our engine, the PC is in `event`.
pub fn reset_to_event(st: &GuardState, boot_time: u64, reaper_or_app: bool, engine: bool) -> bool {
    boot_time > st.written_at || (reaper_or_app && !engine)
}
```

    The daemon applies it before anything else: `mode = Event`, `switching = None`, `interlock_retry = None`, then the event plan runs (checks, plus a start of anything that runs but does not serve).
  - Alarms: an append-only list with ids, ack, and the newest 50 kept; each carries `notified: bool` (sent to the alarm recipients) and `owner_question: bool` (the agent must send the prepared ❓).
  - `cancel.rs`: `Cancel` (an `Arc<AtomicBool>`): `preempt`, `preempted`, `clear`, and `sleep(d) -> Result<(), Preempted>` in ≤ 100 ms slices, so every wait ends ≤ 1 s after a pre-emption.
  - Tests: round trip; a corrupt file → defaults + an alarm "guard state unreadable"; `a_reboot_resets_the_mode_to_event` (saved `dev`, boot later than `written_at` → reset; boot earlier, REAPER up, no engine → reset; boot earlier, engine up → keep); `cancel_sleep_returns_within_a_slice`.

  Commit: `feat(guard): pure core — planner, crash loop, bundles, handover verdicts, protocol (S6)`.

---

### Task 9: `iem-guard` — the effects (`Pc`, `WinPc`, `FakePc`)

**Files:** `crates/iem-guard/src/pc.rs`, `crates/iem-guard/src/win/{mod,reaper,app,card,tasks,procs}.rs`.

- [ ] **Step 1: The trait.** Every method is bounded in time, no method ends a process, and every method that waits takes the cancel token and returns `Err(Preempted)` within 1 s of a pre-emption (design §5.2). Mutating calls finish their mutation first (a save, a quit command, a registry write), then honour the token.

```rust
pub type R<T> = Result<T, StepError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepError {
    Preempted,
    Failed(String),
}

pub trait Pc {
    /// Once a second: the process list only (P10) — the pids of REAPER, the app,
    /// engine, server, tray and runner.
    fn procs(&mut self) -> Procs;
    /// Once per plan: processes, driver-module holders, port owners (design §5.1).
    fn facts(&mut self) -> Facts;
    fn precheck(&mut self, to: Mode, trial: bool) -> R<Option<String>>; // bundle, HIL, pc_tests_passed (trial), ≥ 1 alarm recipient (live and trials; dev: Some(note)), no engine, app exe hash
    fn reaper_meters(&mut self, seconds: u32, c: &Cancel) -> R<Vec<f64>>; // stage tracks, max dBFS each
    fn engine_interlock(&mut self, seconds: u32, c: &Cancel) -> R<(bool, String)>;
    fn reaper_save_quit(&mut self, c: &Cancel) -> R<()>;              // 40026; mtime changed ≤ 15 s; no dialog; 40004; gone ≤ 30 s; module unheld
    fn app_stop(&mut self, c: &Cancel) -> R<AppExit>;                 // handle first, post the tray command, observe
    fn tuning(&mut self, verb: &str, c: &Cancel) -> R<String>;        // the elevated task; "absent" when no module
    fn tuning_drift(&mut self) -> R<Option<String>>;                  // native reads (plan GUID, service state); mode change + hourly
    fn pref_check(&mut self) -> R<u32>;                               // prefwin::restore(.., 3): attempts used
    fn data(&mut self, mode: Mode, c: &Cancel) -> R<String>;          // iem-migrate band, recover, shadow report / import
    fn engine_start(&mut self, hold: bool) -> R<u32>;
    fn engine_ready(&mut self, secs: u32, c: &Cancel) -> R<Status>;   // frames 32 measured, missed == 0 for secs; one restart of the window
    fn engine_arm(&mut self) -> R<()>;
    fn engine_stop(&mut self, c: &Cancel) -> R<()>;                   // Shutdown, DriverReleased ≤ 10 s, gone ≤ 5 s
    fn engine_health(&mut self) -> R<Health>;                         // supervisor Status twice 1 s apart: callbacks advancing, !faulted, !parked
    fn engine_stage_peaks(&mut self, seconds: u32, c: &Cancel) -> R<Vec<f64>>; // from the Meters events; no card reopen
    fn server_start(&mut self, mode: Mode) -> R<u32>;                 // config with pin_changes = false before cutover
    fn server_stop(&mut self, c: &Cancel) -> R<()>;                   // console::ctrl_break, gone ≤ 10 s, ports free
    fn band_quiet_for(&mut self) -> R<Duration>;                      // the server's band-activity state
    fn tray_start(&mut self) -> R<()>;
    fn tray_stop(&mut self, c: &Cancel) -> R<()>;                     // Quit over the guard pipe
    fn identity(&mut self, sha: &str, c: &Cancel) -> R<Option<String>>; // LAN 80/443 + public host /api/version, tunnel ready; LAN 443 = tls::check, the server's own cert (identity, not validity; Some(note) outside its validity)
    fn runner_start(&mut self) -> R<()>;
    fn runner_stop(&mut self, c: &Cancel) -> R<()>;                   // only when idle; console::ctrl_break
    fn holder_gone(&mut self, c: &Cancel) -> R<()>;                   // ≤ 30 s for a foreign module holder to leave
    fn reaper_start(&mut self) -> R<()>;                              // our task (or the direct spawn, design §5.1); refuses with an engine or a module holder
    fn reaper_facts(&mut self, c: &Cancel) -> R<ReaperFacts>;         // ≤ 120 s wait for the track count; bridge once
    fn app_start(&mut self) -> R<()>;
    fn app_answers(&mut self, c: &Cancel) -> R<()>;                   // /api/version, /api/members count, public host
    fn fingerprint(&mut self) -> R<()>;                               // S1c REAPER-mode fingerprint via the tuning task (`state`)
    fn probe_task(&mut self) -> R<()>;                                // \iemmixer\iemmixer-probe from the guard
    fn notify(&mut self, audience: Audience, title: &str, body: &str) -> R<()>; // Audience::{Alarm, BandActivity}
}
```

  `FakePc` records `(Step or call, Instant)`, has scripted results per call, and can block a waiting call until its `Cancel` fires (the way `WinPc` waits). `daemon` tests use it.
- [ ] **Step 2: `WinPc`.** Settings come from `[guard]` and `[card]` of the site plus `$LOCALAPPDATA\iemmixer\guard\pc.toml` (paths).
  - **Facts (`win/procs.rs`):** `procs()` = `process::pids` for the configured images only. `facts()` adds `module_holders([card] module)` (→ `reaper_holds_module`, `other_module_holder`) and `listening(80/443)` compared with the app's pid (→ `app_serves`).
  - **REAPER edge (`win/reaper.rs`):**
    - `ureq` against the configured control URL (`/_/40026`, `/_/40004`, `/_/NTRACK`, `/_/GET/EXTSTATE/<section>/<key>`, `/_/<action>`, `/_/TRACK` for peaks of the stage tracks);
    - the project file's mtime;
    - `iem_win::window::dialog_titles(pid)` right after 40026, sorted by `handover::dialogs`: a dialog other than REAPER's evaluation notice (`About REAPER …`, left open, #9 2026-09-28) aborts before 40004 (alarm with the dialog's presence; the app is already down, so nothing new can dirty the project);
    - `iem_win::process::module_holders`.

    The parser for REAPER's tab lines is a portable function with tests, a port of `ConvertFrom-SpikeReaperLine`.
  - **App edge (`win/app.rs`):**
    1. `pids(app_image)` → exactly one, else error;
    2. `find_owned(tray_class, pid)` → hwnd (the tray library's top-level window);
    3. `Handle::open_waitable(pid)` **before** the post (a recycled pid can never answer for it);
    4. note the time, `post_command(hwnd, exit_id)`;
    5. `handle.wait(30 s)` in 1 s slices (cancel) → the exit code; success needs `Some(0)`;
    6. `listening(80)`/`listening(443)` = None;
    7. no `*.tmp` newer than the post under the app data dir;
    8. read the app's newest log file for `exit_log_line` with a timestamp ≥ the post (a portable parser, tested) — recorded as corroboration, never required.

    `precheck` computes the exe's SHA-256 and compares it with `[guard] app_exe_sha256` (`handover::app_binary`); a mismatch refuses the switch before REAPER is touched.
  - **Card edge (`win/card.rs`):** `iem_win::registry::HkcuPref` for `prefwin::restore(.., 3)` (each attempt read back); holders.
  - **Tasks (`win/tasks.rs`):** `schtasks.exe /Run /TN <name>` (argv, no shell); `/Query /TN <name> /FO CSV /V` for status (a portable parser). When `[guard] start_direct = true` (the probe task was refused at bootstrap, design §5.1), `reaper_start`/`app_start` use `spawn_detached` of the configured exe with its working directory instead.
  - **Processes (`win/procs.rs`):**
    - `spawn_detached` for engine/server/tray/runner, with `CREATE_NEW_PROCESS_GROUP` for server and runner;
    - `tray_start` (hand-off from Task 11) sets `IEMMIXER_CONFIG` to the server's site file and starts the tray in the server's working directory: the tray reads `port` and `https_domain`/`lan_url` from it. Without it Open Mixer falls back to port 80 and Copy URL is disabled;
    - stops through `iem_win::console::ctrl_break(pid)` (attach to the child's console, design §5.5);
    - pid files and adoption (pid + image path + start time must match);
    - the supervisor pipe client (sync `interprocess`, hello `role: supervisor`), which also collects `Meters` for `engine_stage_peaks`.
  - **Drift (`tuning_drift`):** the active power plan GUID and the service start types S1c records, read natively (no PowerShell, no elevation).
- [ ] **Step 3:** `.cargo/mutants.toml` excludes `crates/iem-guard/src/win/**` (reason: Windows effects, not compiled on Linux; decisions are in `plan`/`crash`/`bundle`/`handover`/parsers). The portable parsers stay mutated. Commit: `feat(guard): PC effects behind the Pc trait (Windows) and a fake for tests`.

---

### Task 10: The guard daemon and `iemmode`

**Files:** `crates/iem-guard/src/daemon.rs`, `crates/iem-guard/src/bin/{iemmixer-guard,iemmode}.rs`, `crates/iem-guard/src/install.rs`.

- [ ] **Step 1: Daemon loop** (`iemmixer-guard run`):
  - Take `iem_win::sync::GlobalMutex::try_take("iemmixer-guard")` (`Global\`, so a session-0 `run` over ssh meets the same mutex); if taken, exit 0 with "already running".
  - Load state. Apply `state::reset_to_event(..)` with `process::boot_time()` and `procs()` first (design §5.2: after a reboot the PC is in `event`); then adopt children; if `switching` is still set, re-plan to `Event` unless the recorded target was `Event` (then resume it).
  - Open the guard pipe: the same hardening as the engine (Task 6), name `iemmixer-guard`. CLI mutations (`install`, `activate`, switches) run only through this pipe; `iemmixer-guard install <zip>` without a running guard takes the same global mutex.
  - A `SessionEndWindow` (hidden top-level): on session end, stop respawning, wait ≤ 10 s for the engine's own exit, and stop the server and tray.
  - **Subscriptions (the tray; hand-off from Task 11):** a `Request::Subscribe` connection gets a `State` frame (`proto::Update::State`) at once and after every change of mode, switch or alarms. `TrayStop` delivers `Update::Quit` also to a tray that (re)subscribes during the stop wait: the tray may be in its 2 s retry sleep when `TrayStop` runs. `switching` (with `started`) stays set until `g.finish`, so the tray started at step 8 announces the entry's own alarms (`view::Seen`).
  - Every 1 s: `procs()` only (the process list — P10):
    - watch the children and apply `crash::after_exit`;
    - watch for `reaper.exe` or the app appearing in `dev`/`live` (alarm once per appearance);
    - a due `interlock_retry` starts its switch again (below).
  - On each mode change and hourly: `tuning_drift()` (native reads; drift → alarm). Module holders and ports are read only by `facts()` inside a switch.
- [ ] **Step 2: Switch runner — exact code of the error policy** (design §5.2; `plan::on_error`):

```rust
/// How a switch ended; the mode is in `g.state.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// "ide event" found a healthy engine that did not release: iemmixer keeps serving.
    KeptServing,
    /// The plan stopped; the owner gets the prepared ❓ (alarm flagged `owner_question`).
    NeedsOwner,
}

pub fn run_switch(pc: &mut dyn Pc, g: &mut Guard, from: Mode, to: Mode) -> Outcome {
    let steps = plan(from, to, &pc.facts());
    g.begin(from, to, &steps); // persists `switching`
    let mut skip: Vec<Step> = Vec::new();
    for step in steps {
        if skip.contains(&step) {
            continue;
        }
        if to != Mode::Event && g.cancel.preempted() {
            return back_to_event(pc, g, "pre-empted by event");
        }
        g.persist_before(step);
        match run_step(pc, g, step, to) {
            Ok(()) => g.done(step),
            Err(StepError::Preempted) if to != Mode::Event => {
                return back_to_event(pc, g, "pre-empted by event");
            }
            Err(e) => {
                let why = match &e {
                    StepError::Failed(s) => s.clone(),
                    StepError::Preempted => "pre-empted inside the event plan".into(),
                };
                let health = (to == Mode::Event && step == Step::EngineStop).then(|| {
                    g.persist_before(Step::EngineHealth);
                    pc.engine_health().unwrap_or(Health::Dead)
                });
                match on_error(to, step, health, g.site.on_pref_fail) {
                    OnError::Unwind => {
                        g.alarm(step, &why, false);
                        return back_to_event(pc, g, &why);
                    }
                    OnError::Continue => g.alarm(step, &why, false),
                    OnError::Skip(later) => {
                        g.alarm(step, &why, false);
                        skip.extend_from_slice(later);
                    }
                    OnError::KeepServing => {
                        g.alarm(step, &format!("{why}; engine healthy, iemmixer keeps serving"), true);
                        return g.finish(Outcome::KeptServing, from);
                    }
                    OnError::StopAskOwner => {
                        g.alarm(step, &format!("{why}; health {health:?}"), true);
                        return g.finish(Outcome::NeedsOwner, Mode::Event);
                    }
                }
            }
        }
    }
    g.finish(Outcome::Done, to)
}

fn back_to_event(pc: &mut dyn Pc, g: &mut Guard, why: &str) -> Outcome {
    g.cancel.clear();
    g.info(&format!("unwinding to event: {why}"));
    let now = g.state.mode;
    run_switch(pc, g, now, Mode::Event)
}
```

  `run_step` maps each `Step` to exactly one `Pc` call with `&g.cancel` (`AppStop` also applies `handover::app_exit`; `Interlock` uses `reaper_meters` when REAPER runs, else `engine_interlock`; `EngineArm` = `engine_ready(10)` then `engine_arm`; `ReaperHandover` = `reaper_facts` + `handover::reaper_handover`; `Fingerprint` = `fingerprint`). `g.alarm(.., owner_question)` writes the alarm file and sends `notify(Audience::Alarm, ..)`; the agent turns an `owner_question` alarm into the prepared Slovak ❓ (ops runbook texts). `g.finish` sets `mode`, clears `switching`, persists, and runs `tuning_drift` once.
  - Requests arriving meanwhile:
    - `Event` → `g.cancel.preempt()`; a waiting step returns `Preempted` within 1 s, a mutating step finishes first; then `back_to_event`. During an event plan `Event` is answered `already switching to event` and never touches the token;
    - `Status` is answered;
    - `JobBegin`, `Install`, `Activate`, `TestSignal`, `Report`, `ProbeTask`, `RehearseTeardown` are refused with `switching`;
    - `Dev`/`Live` get `busy`.
  - **Interlock refusals** (activity, not an error of the check): `interlock_retry = {target, refusals + 1, next_at = now + 15 min}`; on the fourth refusal one `Alarm` notice to the owner ("na pódiu je signál, prepnutie čaká"); after the eighth the retry is dropped and `iemmode status` says so; "ide event" or a new request clears it.
  - **`engine_ready`**: one warm-up miss restarts the 10 s window once; a second miss fails the step.
  - **`JobBegin`**: refused unless `mode == Dev`, not switching, `band_quiet_for() ≥ 5 min`, and `engine_stage_peaks(60)` all below −50 dBFS.
  - **`TestSignal`**: sent to the engine as `HilTestSignal` with `card_tx = [guard] hil_tx`.
  - **`RehearseTeardown`** (dev only): `EngineStop` → `ServerStop` → `TrayStop` → `TuningExit` → `PrefCheck` with the event error policy, then asserts `facts()` shows no module holder, `prefwin` reads the original, and ports 80/443 are free, then `run_switch(pc, g, Mode::Dev, Mode::Dev)`. It never plans a REAPER or app step.
  - `--dry-run` prints the plan and runs read-only checks only (facts, preference read, bundle record, alarm recipients).
- [ ] **Step 3: `install.rs`** (`iemmixer-guard install <zip> [--verify-only]`; the same code behind `Request::Install`):
  - extract into `bundles\<sha>.partial` (`enclosed_name`), verify (`bundle::verify`, `REQUIRED`), rename; `--verify-only` extracts into a temp directory, verifies and deletes only that temp directory (CI's check, Task 12);
  - an existing `<sha>` with identical sums is a no-op; different sums → refuse (alarm);
  - `activate` copies `iemmixer-guard.exe` and `iemmode.exe` into `bin\`: rename the running file to `*.old-<sha>`, copy, and delete old copies at the next start; it asks `\iemmixer\iemmixer-exclude` for the new SHA's Defender process exclusions (design §5.1);
  - the guard then hands over: it spawns the new `bin\iemmixer-guard.exe run` detached and exits 0 after releasing the mutex (the new one waits ≤ 10 s for it).
- [ ] **Step 4: `iemmode`.**
  - Connect to the guard pipe; if absent, `schtasks /Run /TN \iemmixer\iemmixer-guard` and retry for ≤ 15 s.
  - `event --direct`: when no guard answers, take the global mutex (free only when no guard runs), load the state, and run `run_switch(WinPc, .., Event)` in this process; if the mutex is taken, exit 1 ("a guard runs; use the pipe"). `iempc event` uses it on exit 4.
  - Subcommands:

    ```
    iemmode status | event [--dry-run] [--direct] | dev [--build SHA] [--force] [--dry-run]
    iemmode live --build SHA [--trial] [--dry-run] | install <zip> | activate <sha>
    iemmode test-signal <input> <dbfs> <ttl> | report <sha> <green|red> <detail>
    iemmode job-begin <run> | job-end <run> | install-site <file> | force-reopen | inject-fault | runner-stop
    iemmode probe-task | rehearse-teardown | alarm-test | alarm-ack <id> | quit
    ```

  - Output: one JSON reply, and the alarms list on every call (spec §4.2). Exit 0 ok, 1 refused/failed, 2 usage, 4 guard unreachable.
- [ ] **Step 5: Tests** (`FakePc`, a temp state dir, the Unix socket variant of the guard pipe). Exact bodies for the safety-critical ones:

```rust
fn band_up() -> Facts {
    Facts { reaper: true, app: true, reaper_holds_module: true, app_serves: true, ..Facts::default() }
}

fn iemmixer_up() -> Facts {
    Facts { engine: true, server: true, tray: true, ..Facts::default() }
}

#[test]
fn preempt_during_interlock_starts_event_within_1s() {
    let (mut pc, mut g) = (FakePc::new(band_up()), Guard::for_test(Mode::Event));
    pc.block_until_cancel(Call::ReaperMeters);
    let c = g.cancel.clone();
    let fired = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        c.preempt();
        Instant::now()
    });
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    let at = fired.join().unwrap();
    assert!(!pc.called(Call::AppStop) && !pc.called(Call::ReaperSaveQuit));
    let first_event_call = pc.first_after(at).expect("the event plan ran");
    assert!(first_event_call.1.duration_since(at) < Duration::from_secs(1));
    assert_eq!(g.state.mode, Mode::Event);
}

#[test]
fn event_preempts_a_running_dev_switch() {
    let (mut pc, mut g) = (FakePc::new(Facts::default()), Guard::for_test(Mode::Event));
    pc.delay(Call::EngineStart, Duration::from_millis(300)); // a mutating step finishes
    let c = g.cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        c.preempt();
    });
    run_switch(&mut pc, &mut g, Mode::Event, Mode::Dev);
    assert!(pc.called(Call::EngineStart));
    assert!(!pc.called(Call::EngineArm));
    assert!(pc.index(Call::EngineStop) > pc.index(Call::EngineStart));
    assert!(pc.called(Call::ReaperStart) && pc.called(Call::AppStart));
}

#[test]
fn a_healthy_engine_keeps_serving_when_release_times_out() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Healthy);
    assert_eq!(run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event), Outcome::KeptServing);
    for c in [Call::ServerStop, Call::TrayStop, Call::ReaperStart, Call::AppStart] {
        assert!(!pc.called(c), "{c:?} after a failed release");
    }
    assert_eq!(g.state.mode, Mode::Dev);
    assert!(g.alarms.last().unwrap().owner_question);
}

#[test]
fn a_parked_engine_stops_the_plan_and_asks_the_owner() {
    let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
    pc.fail(Call::EngineStop, "no DriverReleased within 10 s");
    pc.health(Health::Parked);
    assert_eq!(run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event), Outcome::NeedsOwner);
    assert_eq!(pc.calls_after(Call::EngineHealth), Vec::<Call>::new());
}

#[test]
fn a_failed_pref_check_follows_on_pref_fail() {
    for (choice, starts) in [(PrefFail::KeepReaperDown, false), (PrefFail::StartReaperWithAlarm, true)] {
        let (mut pc, mut g) = (FakePc::new(iemmixer_up()), Guard::for_test(Mode::Dev));
        g.site.on_pref_fail = choice;
        pc.fail(Call::PrefCheck, "3 restores failed");
        run_switch(&mut pc, &mut g, Mode::Dev, Mode::Event);
        assert_eq!(pc.called(Call::ReaperStart), starts, "{choice:?}");
        assert!(g.alarms.iter().any(|a| a.step == Some(Step::PrefCheck)));
    }
}
```

  And (bodies in the same shape):
  - `a_restarted_guard_unwinds_a_half_done_dev_switch`;
  - `a_reboot_resets_the_mode_to_event` (daemon start with saved `dev`, boot later than `written_at` → mode `event`, `switching` dropped, the event plan's checks ran);
  - `direct_event_runs_without_a_guard` (the `--direct` path with `FakePc` runs the same event plan; with the mutex taken it refuses);
  - `jobs_are_refused_while_switching`; `job_begin_needs_a_quiet_stage` (4 min quiet refused; 5 min quiet but a −30 dBFS stage peak refused; quiet both → accepted);
  - `interlock_refusals_retry_every_15_min_and_alarm_once`;
  - `engine_ready_restarts_its_window_once`;
  - `rehearse_teardown_never_starts_reaper` (and it re-enters dev);
  - `session_end_stops_respawning`;
  - `dry_run_changes_nothing` (the fake records no mutating call);
  - install happy path, tampered file, traversal entry, missing required file, existing SHA with different sums, `--verify-only` leaves no bundle directory.

- [ ] **Step 6: What HIL v1 needs from the guard** (amended after the Task 12 review, 2026-09-27; `hil-v1.ps1` already relies on it):
  - `Request::InjectFault` / `iemmode inject-fault` (in `proto.rs` since Task 12): refused unless `dev`, not switching and a HIL job is active. The guard starts the engine with `--fault-injection` only in `dev` while a job is active (`activate` inside the job restarts it so; never in `live`) and forwards `Cmd::InjectFault` over the supervisor connection. The engine's exit 70 goes through `crash::after_exit` (respawn after the backoff, the fade-in by `Process::discontinuity`). Test: `inject_fault_is_refused_outside_a_dev_job` and, with `FakePc`, one exit 70 → exactly one respawn, `spawns` + 1, `last_exit = Some(70)`.
  - `Reply.engine: Option<EngineStatus>` (in `proto.rs` since Task 12) on every reply while an engine runs: `build` (`Hello.engine_build`), `frames`, `callbacks`, `missed`, `resets`, `parked`, `faulted` (the supervisor `Status`, Task 5's fields), `pipe_private` (the engine pipe's DACL read back through `iem_win`: only the user and SYSTEM), `spawns` (engines this guard started) and `last_exit`. `None` while no engine runs.
  - `iemmixer-guard install <zip> --verify-only` prints every `bundle::verify` problem, one per line on stderr, and exits non-zero; CI's tampered-bundle check requires the line `hil-v1.ps1: sha256 <64 hex>, SHA256SUMS says <64 hex>`.
  - The elevated tasks answer in `%ProgramData%\iemmixer\tasks\out\<kind>.result.json` (admin-only; the user reads it), not in the user's root; the requests stay `<root>\guard\tasks\<kind>.request.json` (`IemPc.psm1`, `.claude/rules/guard.md`). The guard resolves `%ProgramData%` from the known folder.
  - HIL v1 scope: the pipes' first-instance flag (the `windows` job's pipe tests prove it), the tunnel's peer address, the forced reopen's duration (≈ 100 ms) and the fault callback's time (< 1 ms) are not reported by the guard and not checked by HIL v1; S7 (#10) picks them up with the switch-timing work.

  Commit: `feat(guard): daemon, switch runner with pre-emption and error policy, install/activate, iemmode CLI`.

---

### Task 11: The tray without a server (F27)

**Files:** `crates/iem-tray/src/{lib,tray}.rs`, `crates/iem-tray/Cargo.toml`.

- [ ] **Step 1:** remove the embedded server (runtime, `start_server`, config dir); the tray reads `lan_url`/`https_domain` from the site config for Open Mixer / Copy URL.
- [ ] **Step 2:** a background thread subscribes to the guard (`Request::Subscribe`): mode and alarms update the tooltip ("iemmixer — dev", "… 2 alarmy") and raise a notification for a new alarm. `Quit` from the guard → `app.exit(0)`. The menu Exit exits the tray only (F27).
- [ ] **Step 3:** the `windows` job builds and lints it (as today); the portable tooltip-text function is tested. Commit: `feat(tray): tray without a server — status and alarms from the guard (F27)`.
- Decided in review (2026-09-27):
  - Open Mixer opens the local server, `http://localhost:<port>` (`Config::mixer_url`), as the predecessor and the old tray did, never the LAN URL. Loopback always reaches the server (the HTTPS redirect applies to the public host only), and Copy URL runs `navigator.clipboard` in the same window, which exists only in a secure context (a plain-http LAN address has none). Copy URL copies `share_url()` (the public host, else the LAN URL).
  - The tray's first reply announces the unacknowledged alarms raised since the tray started, or since the start of the switch in progress when that is earlier (the guard starts the tray at dev-entry step 8, after the entry's own alarms). Older unacknowledged alarms are only counted in the tooltip; they also reach the owner's phone through `iem-server notify`. A guard that lost its state (the newest alarm seen is no longer in its list) has only new alarms.

---

### Task 12: CI — windows job, bundle, attest; integrity; PC bootstrap module; rule

**Files:** `.github/workflows/ci.yml`, `scripts/check_integrity.py` (+test), `scripts/iem-pc/IemPc.psm1`, `scripts/iem-pc/Test-IemPc.ps1`, `scripts/iem-pc/hil-v1.ps1`, `.claude/rules/guard.md`, `CLAUDE.md`.

- [ ] **Step 1: `windows` job.**
  - Add `cargo clippy --locked -p iem-win -p iem-guard --all-targets -- -D warnings`.
  - Add `cargo test --locked -p iem-win -p iem-guard -p iem-audio-io` and `cargo test --locked -p iem-engine --test pipes`.
  - Run `powershell -NoProfile -File scripts/iem-pc/Test-IemPc.ps1` (Windows PowerShell 5.1) and a parse check of `hil-v1.ps1` (`[System.Management.Automation.Language.Parser]::ParseFile`, zero errors).
- [ ] **Step 2: `bundle` job** (windows-2025; `needs: [windows]`; every push and PR, but uploads only on `push` to `dev`/`main`).

```yaml
  bundle:
    name: bundle
    needs: [windows]
    runs-on: windows-2025
    timeout-minutes: 40
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - name: Build (release)
        # iem-server.exe needs `standalone`; the tray no longer pulls in tls
        # and audio (Task 11, F27).
        run: cargo build --locked --release -p iem-engine -p iem-server -p iem-guard -p iem-tray -p iem-migrate --features iem-server/standalone,iem-server/tls,iem-server/audio
      - name: Zip with manifest and sums
        shell: pwsh
        run: |
          $ErrorActionPreference = 'Stop'
          $b = Join-Path $env:RUNNER_TEMP 'bundle'; New-Item -ItemType Directory -Force -Path $b | Out-Null
          Copy-Item target/release/iem-engine.exe, target/release/iem-server.exe, target/release/iemmixer-guard.exe, target/release/iemmode.exe, target/release/iem-tray.exe, target/release/iem-migrate.exe -Destination $b
          Copy-Item scripts/iem-pc/hil-v1.ps1, scripts/iem-pc/IemPc.psm1 -Destination $b
          Copy-Item crates/iem-engine/LICENSE -Destination (Join-Path $b 'LICENSE-iem-engine')
          if (Test-Path scripts/pc-tuning) { Copy-Item -Recurse scripts/pc-tuning (Join-Path $b 'tuning') }
          $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
          $branch = if ($env:GITHUB_REF -like 'refs/heads/*') { $env:GITHUB_REF.Substring(11) } else { 'pr' }
          @{ sha = $env:GITHUB_SHA; branch = $branch; version = $version; run = [int64]$env:GITHUB_RUN_ID } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $b 'manifest.json')
          $lines = Get-ChildItem -LiteralPath $b -File -Recurse | Sort-Object FullName | ForEach-Object { '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.FullName.Substring($b.Length + 1).Replace('\','/') }
          [IO.File]::WriteAllText((Join-Path $b 'SHA256SUMS'), (($lines -join "`n") + "`n"))
          Compress-Archive -Path (Join-Path $b '*') -DestinationPath (Join-Path $env:RUNNER_TEMP "iemmixer-$env:GITHUB_SHA.zip")
      - name: Verify the zip with the code that installs it
        shell: pwsh
        run: |
          $ErrorActionPreference = 'Stop'
          & target/release/iemmixer-guard.exe install --verify-only (Join-Path $env:RUNNER_TEMP "iemmixer-$env:GITHUB_SHA.zip")
          if ($LASTEXITCODE -ne 0) { throw "bundle verify failed ($LASTEXITCODE)" }
      - name: Upload (dev/main pushes only, P5)
        if: github.event_name == 'push'
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: iemmixer-bundle-${{ github.sha }}
          path: ${{ runner.temp }}/iemmixer-${{ github.sha }}.zip
          retention-days: 30
          if-no-files-found: error
```

  `bundle.rs` must accept `/`-separated names in sums for the `tuning/` subdirectory: adjust `parse_sums` to allow one level of `tuning/<name>` and still refuse `..` and absolute paths (test).
- [ ] **Step 3: `attest`** (no dispatch job: the agent dispatches HIL from the dev box, design §7). Resolve the pins first; the integrity scan requires full SHAs:

```bash
for a in actions/download-artifact actions/attest-build-provenance; do
  tag=$(gh api repos/$a/releases/latest --jq .tag_name); sha=$(gh api repos/$a/git/ref/tags/$tag --jq .object.sha)
  echo "$a@$sha # $tag"; done
```

```yaml
  attest:
    name: attest
    if: github.event_name == 'push'
    needs: [bundle]
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    permissions:
      id-token: write
      attestations: write
      contents: read
    outputs:
      digest: ${{ steps.d.outputs.digest }}
    steps:
      - uses: actions/download-artifact@<sha> # <tag>
        with:
          name: iemmixer-bundle-${{ github.sha }}
      - id: d
        run: echo "digest=sha256:$(sha256sum "iemmixer-${GITHUB_SHA}.zip" | cut -d' ' -f1)" >> "$GITHUB_OUTPUT"
      - uses: actions/attest-build-provenance@<sha> # <tag>
        with:
          subject-name: iemmixer-${{ github.sha }}.zip
          subject-digest: ${{ steps.d.outputs.digest }}
```

  No secret is added to the public repository; the owner creates no token. `iempc dispatch-hil` (Task 14) dispatches ops `hil.yml` with the green run's SHA, branch, run id and the `attest` digest (read from the run's job output via `gh run view --json`), using the dev box's existing `gh` authentication. Deviation from spec §5.2, recorded in the design note (§7, §11). The `asio-spike` job stays (S1c uses it).
- [ ] **Step 4: Integrity.**
  - `check_integrity.py` scans the new crate directories (already covered by `crates/`).
  - It now also fails on `CREATE_BREAKAWAY_FROM_JOB` outside `crates/iem-win/`.
  - Test: `test_force_kill_words_in_comments_are_refused` (already true — assert it for the new crates).
- [ ] **Step 5: `IemPc.psm1`** (bootstrap on the PC; runs elevated over ssh; every function idempotent with a read-back). Exports:
  - `Register-IemTasks` (guard, StartREAPER unchanged, StartApp, probe (`cmd /c exit 0`), tuning (Highest), exclude (Highest), logon (Highest, at logon of the user): Interactive, no time limit `PT0S`, `IgnoreNew`, no idle/battery stop, **restart on failure** 3 × `PT1M` (spec §2.1)). Every task is registered through `Schedule.Service` → `RegisterTaskDefinition(path, def, 6 /* create or update */, $user, $null, 3 /* interactive token */, $sddl)` (Highest tasks: their own run level and principal, same SDDL) with `$sddl = "D:(A;;GRGX;;;$userSid)(A;;FA;;;BA)(A;;FA;;;SY)"`, so the Limited guard may run it; the read-back compares `GetSecurityDescriptor(4)` with that string;
  - `Set-IemRootAcl` (protected DACL: user, SYSTEM, Administrators; inheritance on);
  - `Add-IemFirewallRule` (TCP 80,443 inbound, private/domain profiles, named `iemmixer-http`);
  - `Test-IemServiceRight` / `Grant-IemServiceRight -Service <name>` (adds `RPWPLO` for the user SID to the service SDDL; prints before/after);
  - `Set-IemDefenderExclusion -Sha <40 hex>` (S1c G4, run by the `exclude` task): re-verifies `bundles\<sha>\SHA256SUMS` itself, adds **process** exclusions for exactly that directory's `*.exe` (full paths), removes those of SHAs that are neither `current` nor `previous`; never a folder exclusion on the user-writable root;
  - `Register-IemRunner` (unzip the pinned runner, `config.cmd --unattended --url https://github.com/zbynekdrlik/iemmixer-ops --labels iem-pc --work _work --replace` with the one-time registration token in the process environment variable `ACTIONS_RUNNER_INPUT_TOKEN`, never on the command line, removed right after);
  - `Get-IemTunnelOrigin` (read-only: the tunnel's configured origin host and port as the connector reports it; no write);
  - `Get-IemPredecessorFacts` (read-only: the `StartREAPER` task's triggers and whether any can fire outside events, the app's version resource and exe SHA-256);
  - `Get-IemBootstrapState`.

  `Test-IemPc.ps1` runs them against an HKCU test root, a test task folder `\iemmixer-test\` (the SDDL read-back included), a temp directory, and a disabled test firewall rule, then removes only its own test objects. The Defender and runner functions run in `-WhatIf` mode there (the hosted runner has no Defender service to change and no ops token).

  `hil-v1.ps1` (public, no site values; reads everything through `iemmode status`) is written here and ships in the bundle: `job-begin`, `activate` (the `pc` job already installed the verified zip, which is how this script's own directory exists), the design §7 checks through `iemmode`/HTTP, `result.json`, `report`, `job-end`. It never talks to GitHub.
- [ ] **Step 6: Playbook rule `.claude/rules/guard.md`** (`paths:` `crates/iem-guard/**`, `crates/iem-win/**`, `scripts/iem-pc/**`, `crates/iem-audio-io/src/{asio,period,channels,reset,rtpanic}.rs`):
  - the preference window (and `on_pref_fail`);
  - the stop verbs, Ctrl-Break through an attached console;
  - the planner invariants (app before REAPER on entry; the interlock whenever REAPER or the app runs; `PrefCheck` before REAPER; the fingerprint last; stale REAPER/app restarted) and the error policy (a failed release never tears down a healthy iemmixer);
  - the reboot mode reset and the cancel token (every wait ≤ 1 s behind a pre-emption);
  - the predecessor exit path and its verification (handle before the post, exit code 0, the exe hash at the precheck);
  - that site values live in `[card]`/`[guard]` and the env;
  - the EVENT-NOW discipline for `iempc`, the one switch-over rule, the `--direct` fallback.

  In the CLAUDE.md router add: "Guard, iemmode, PC install → `.claude/rules/guard.md`". The always-apply event line becomes: "'ide event' → `iempc event` once the guard is installed (S6 Task 16 Step 3); before that the interim switch".

  Commit: `ci(s6): bundle with verify, attest; PC bootstrap module; guard playbook rule`.

---

### Task 13: Ops repo — site tables, `hil.yml`, runbook (private)

**Files (ops repo, `dev` branch, owner-merged PR to `main`):**
- `site/site.toml`: `[card]`, `[guard]`, `[activity] inputs`, server tables;
- `.github/workflows/hil.yml`;
- `docs/s6-pc-runbook.md`;
- `security/denylist-terms.txt` only if a new name-like site value appears (+ `$PRIV/denylist.txt` and the public repo's `DENYLIST` secret in the same session, per the ops `CLAUDE.md`);
- `CLAUDE.md` (router, event section);
- `$PC_ENV`, `$PRIV/event-runbook.md`.

- [ ] **Step 1: `site.toml`.**
  - `[card]`: the real driver name, the driver module (DLL name), preference key/name, original `{kind = "dword", raw = "64"}` — from the S1a env.
  - `[guard]`:
    - the REAPER control URL, the project path, the expected track count, the stage track indices;
    - the meter-bridge state/heartbeat/action keys;
    - the predecessor image name, tray class, exit command id, `app_exe_sha256`, log directory and exit line, data directory, member count, start task;
    - the public host;
    - `hil_tx` (the D5(b) loopback pair or a spare TX — the only card outputs a HIL test signal may reach);
    - `pc_tests_passed = false` (set to `true` only after the owner-approved tests pass, design §10);
    - `start_direct = false` (set by Task 16 Step 4 if the probe task is refused);
    - `on_pref_fail` (Step 2 below).
  - `[activity] inputs`: the stage input ids (`mic1`…`mic10`, `hand1`…`hand3`, `eng_mic`).
  - The server tables the S5 hand-off asked for: `[[members]]`, `[[inputs]]` with categories, `back_to_reaper = ['<bin>\iemmode.exe', 'event']`, `pin_changes = false`. These are the S8 hand-off's items that S6 needs to run the server on the PC.
  - **The exit command id** (design §5.3). The deployed app is newer than the pin, so the id comes from the deployed commit, read-only in `$PRED`:
    1. find the commit of the deployed version (its tag or the version bump in `git log`), `DEP=<sha>`;
    2. `git -C "$PRED" diff --quiet $PIN_PRED $DEP -- iem-mixer/src-tauri/src/tray.rs iem-mixer/src-tauri/src/lib.rs` and the same for `iem-mixer/src-tauri/Cargo.lock` restricted to the `muda`, `tray-icon` and `tauri` entries (`git show $DEP:…/Cargo.lock | grep -A2 '^name = "\(muda\|tray-icon\|tauri\)"'` on both sides);
    3. if all equal: the menu library numbers items from its counter's start in creation order, so the id is the counter start plus the Exit item's position among the created items (the review read it from the locked sources); if anything differs, re-derive from `$DEP`'s sources the same way;
    4. `app_exe_sha256` is filled in Task 16 Step 1 from the PC (read-only) and re-checked against the deployed version.

    Record the derivation in the ops runbook only. It is confirmed at the first real stop (Task 17).
  - **Validation:** `tools/check_import.sh` as today. The `iem-engine check-site` run needs the Task 15 bundle on the PC and moves to Task 16 Step 3.
- [ ] **Step 2: Decide `on_pref_fail` and record it on #9** (design §5.2 step 5; this is the agent's own rule, not an owner ruling, so the agent decides and records it before Task 18). The choice after three failed restores at an event is between silence (REAPER down) and REAPER possibly at 32 with an alarm. **Decision: `start_reaper_with_alarm`** — silence is the one failure the band certainly notices (P9); REAPER at an unexpected buffer still plays, the handover checks still run, and the owner is told at once. Post it on #9 as `ROZHODNUTÉ:` with this reasoning, then write the value into `[guard]`.
- [ ] **Step 3: Denylist terms** (P6). Add any `[guard]` value written now that is a name (not a number or hash) to the same three places in the same session, and re-run the denylist scan over the committed design note and plan.
- [ ] **Step 4: `hil.yml`.** Every input is read from `env:` and validated before any use (no `${{ inputs.* }}` inside a script); the self-hosted `pc` job holds no secret and no token.

```yaml
name: hil
on:
  workflow_dispatch:
    inputs:
      sha: { required: true, type: string }
      branch: { required: true, type: string }
      run: { required: true, type: string }
      digest: { required: true, type: string }
permissions:
  contents: read
concurrency:
  group: hil-iem-pc
  cancel-in-progress: false
jobs:
  verify:
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    outputs:
      head: ${{ steps.h.outputs.head }}
    steps:
      - name: Validate inputs
        env: { SHA: "${{ inputs.sha }}", BRANCH: "${{ inputs.branch }}", RUN: "${{ inputs.run }}", DIGEST: "${{ inputs.digest }}" }
        run: |
          set -euo pipefail
          [[ "$SHA" =~ ^[0-9a-f]{40}$ ]] && [[ "$BRANCH" =~ ^(dev|main)$ ]] && [[ "$RUN" =~ ^[0-9]+$ ]] && [[ "$DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]]
      - id: app
        uses: actions/create-github-app-token@<sha> # <tag>
        with: { app-id: "${{ vars.OPS_APP_ID }}", private-key: "${{ secrets.OPS_APP_KEY }}", owner: zbynekdrlik, repositories: iemmixer }
      - id: h
        env: { GH_TOKEN: "${{ steps.app.outputs.token }}", SHA: "${{ inputs.sha }}", BRANCH: "${{ inputs.branch }}", RUN: "${{ inputs.run }}", DIGEST: "${{ inputs.digest }}" }
        run: |
          set -euo pipefail
          head=$(gh api "repos/zbynekdrlik/iemmixer/git/ref/heads/$BRANCH" --jq .object.sha)
          if [ "$head" != "$SHA" ]; then echo "head=no" >> "$GITHUB_OUTPUT"; exit 0; fi
          gh run download "$RUN" -R zbynekdrlik/iemmixer -n "iemmixer-bundle-$SHA"
          test "sha256:$(sha256sum "iemmixer-$SHA.zip" | cut -d' ' -f1)" = "$DIGEST"
          gh attestation verify "iemmixer-$SHA.zip" -R zbynekdrlik/iemmixer \
            --signer-workflow zbynekdrlik/iemmixer/.github/workflows/ci.yml --source-ref "refs/heads/$BRANCH" --deny-self-hosted-runners
          echo "head=yes" >> "$GITHUB_OUTPUT"
      - if: steps.h.outputs.head == 'yes'
        uses: actions/upload-artifact@<sha> # <tag>
        with: { name: verified-bundle, path: "iemmixer-${{ inputs.sha }}.zip", retention-days: 7, if-no-files-found: error }
  pc:
    needs: verify
    if: needs.verify.outputs.head == 'yes'
    runs-on: [self-hosted, iem-pc]
    timeout-minutes: 30
    permissions: {}
    steps:
      - uses: actions/download-artifact@<sha> # <tag>
        with: { name: verified-bundle }
      - name: HIL v1
        shell: powershell
        env: { SHA: "${{ inputs.sha }}", BRANCH: "${{ inputs.branch }}", DIGEST: "${{ inputs.digest }}", JOBRUN: "${{ github.run_id }}" }
        run: |
          $ErrorActionPreference = 'Stop'
          '{"conclusion":"failure","summary":"HIL did not finish"}' | Set-Content -Encoding ascii -Path result.json
          if ($env:SHA -notmatch '^[0-9a-f]{40}$' -or $env:DIGEST -notmatch '^sha256:[0-9a-f]{64}$' -or $env:BRANCH -notmatch '^(dev|main)$') { throw 'bad input' }
          $zip = Join-Path $PWD "iemmixer-$env:SHA.zip"
          if (('sha256:' + (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()) -ne $env:DIGEST) { throw 'digest mismatch' }
          & "$env:LOCALAPPDATA\iemmixer\bin\iemmode.exe" install $zip
          if ($LASTEXITCODE -ne 0) { throw "install refused ($LASTEXITCODE)" }
          & "$env:LOCALAPPDATA\iemmixer\bundles\$env:SHA\hil-v1.ps1" -Sha $env:SHA -Branch $env:BRANCH -JobRun $env:JOBRUN -Out (Join-Path $PWD 'result.json')
      - if: always()
        uses: actions/upload-artifact@<sha> # <tag>
        with: { name: hil-result, path: result.json, if-no-files-found: warn }
  report:
    needs: [verify, pc]
    if: always() && needs.verify.outputs.head == 'yes'
    runs-on: ubuntu-24.04
    timeout-minutes: 5
    steps:
      - uses: actions/download-artifact@<sha> # <tag>
        with: { name: hil-result }
      - id: app
        uses: actions/create-github-app-token@<sha> # <tag>
        with: { app-id: "${{ vars.OPS_APP_ID }}", private-key: "${{ secrets.OPS_APP_KEY }}", owner: zbynekdrlik, repositories: iemmixer }
      - name: Post hil/iem-pc
        env: { GH_TOKEN: "${{ steps.app.outputs.token }}", SHA: "${{ inputs.sha }}", PC: "${{ needs.pc.result }}" }
        run: |
          set -euo pipefail
          [[ "$SHA" =~ ^[0-9a-f]{40}$ ]]
          c=$(jq -r '.conclusion' result.json 2>/dev/null || echo failure)
          [ "$PC" = success ] || c=$([ "$PC" = cancelled ] && echo cancelled || echo failure)
          case "$c" in success|failure|cancelled) ;; *) c=failure;; esac
          gh api "repos/zbynekdrlik/iemmixer/check-runs" -f name=hil/iem-pc -f head_sha="$SHA" -f status=completed -f conclusion="$c" \
            -f "output[title]=HIL v1" -f "output[summary]=$(jq -r '.summary // "no result"' result.json 2>/dev/null | head -c 60000)"
```

  Pin every action to a full SHA with the Task 12 Step 3 loop (add `actions/create-github-app-token`, `actions/upload-artifact`, `actions/download-artifact`). The `pc` job writes a failure `result.json` before anything else, so every started job reports; a job that never started (no runner, the guard refused) leaves no artifact, the report job fails loudly and the missing `hil/iem-pc` counts as not green, which is what `live --build` needs.

  `hil-v1.ps1` (Task 12, shipped in the bundle, run from the verified `bundles\<sha>\`) runs:
  1. `iemmode job-begin` (refused unless dev, not switching, band quiet 5 min, stage peaks quiet 60 s);
  2. `iemmode activate` (the guard switches the bundle in dev);
  3. the checks of design §7 through `iemmode`/HTTP (measured frames 32; the card-masked test signal on `hil_tx` with per-TX routing from the engine's meters);
  4. `result.json` (`conclusion`, `summary`, the numbers);
  5. `iemmode report`;
  6. `iemmode job-end`.

  Any refused `iemmode` (a switch to `event` started) ends the job as `cancelled`, never as success.
- [ ] **Step 5: `docs/s6-pc-runbook.md`.**
  - The env keys (`$PC_ENV`: `PC_SSH`, `PC_BIN`, `PC_ROOT`, the public host).
  - The bootstrap sequence (Task 16), "ide event" / "event skončil" with `iempc` (from Task 16 Step 3), the `--direct` fallback, and the fallback before that (S1a `spike_window.py preempt`, then the event runbook's manual steps).
  - The alarm texts in Slovak, and the **prepared owner ❓ blocks** (Slovak, self-contained, one decision each) for the three stop cases: the engine did not release but plays (iemmixer keeps serving — information plus "chceš REAPER hneď? = reštart PC"), the engine is dead or parked (reboot offer), REAPER could not start (holder / start / save-quit failure).
  - The five approval-gated tests (asked in Task 17, not run before approval).

  Update `$PRIV/event-runbook.md` and the ops `CLAUDE.md` event section to point at `iempc event` / `iempc dev` from Task 16 Step 3 on (the one switch-over rule of the Global Constraints).

---

### Task 14: The dev-box control tool `iempc.py`

**Files:** `scripts/iem-pc/iempc.py`, `scripts/iem-pc/test_iempc.py`.

- [ ] **Step 1: Commands.**
  - `status`, `event [--dry-run]`, `dev [--build SHA] [--dry-run]`, `rehearse-teardown`, `probe-task` — ssh `"$PC_BIN\iemmode.exe" …`, JSON out;
  - `event` first runs `spike_window.py preempt` when an S1a/S1c spike window is open (its state file), then `iemmode event`; on exit 4 (guard unreachable) it runs `iemmode event --direct`;
  - `fetch-bundle --sha` — the green `push` run's artifact, digest check, `gh attestation verify`;
  - `bootstrap <step>` — PowerShell `IemPc.psm1` functions over ssh;
  - `install --sha` — scp the verified zip, then `iemmode install`;
  - `dispatch-hil [--sha SHA]` — dispatches ops `hil.yml` with the dev box's `gh` authentication (no token in the public repo, design §7): the SHA must be the head of `dev`/`main` with a green `push` run; branch, run id and the `attest` job's digest come from that run; once per SHA per dev entry, recorded in `$STATE`;
  - `handover-s1a` — marks the S1a window closed when the card is free, no spike runs and the preference reads the original (read-only checks, then the S1a state file updated).
- [ ] **Step 2: EVENT-NOW discipline** (from `spike_window.py`'s `guarded()`):
  - every wait polls the flag every 2 s;
  - a read-only call is abandoned;
  - `dev`/`install`/`dispatch-hil`/`rehearse-teardown` refuse when the flag exists;
  - with the flag present, `event` runs even if another command is running (it is the pre-emption).
- [ ] **Step 3: Tests** (fake ssh runner, fake `gh`):
  - the flag refuses `dev` and lets `event` through;
  - `event` pre-empts an open spike window first, and falls back to `--direct` on exit 4 only;
  - parsing of the `iemmode` JSON, `fetch-bundle` digest mismatch refused;
  - `dispatch-hil` refuses a SHA that is not a branch head or has no green run, and fires once per SHA per dev entry;
  - no site value in the module (reads `$PC_ENV`).

  Add to the CI `integrity` job: `python3 -m unittest discover -s scripts/iem-pc -p 'test_*.py' -v`. Commit: `feat(iem-pc): dev-box control with the event pre-emption and HIL dispatch`.

---

### Task 15: First push, CI green (main session)

- [ ] **Step 1: Pre-push, then push.**

```bash
cd "$WORK" && git fetch origin && git merge --ff-only origin/dev && git status -sb
cargo fmt --all -- --check && python3 scripts/check_integrity.py && python3 scripts/check_engine_deps.py && python3 scripts/check_version.py
python3 -m unittest discover -s scripts -p 'test_*.py' 2>&1 | tail -1
python3 -m unittest discover -s scripts/iem-pc -p 'test_*.py' 2>&1 | tail -1
git push origin dev    # the shard budget comes from CI's mutants-list job (cargo mutants runs in CI only)
```

- [ ] **Step 2: Wait for every job** with one foreground bounded loop per Bash call. It includes `windows`, `bundle` (with its verify step), `attest`, `supply-chain`, `mutants-list`, `integrity`:

```bash
RUN=$(gh run list -R "$REPO" --branch dev --event push --limit 1 --json databaseId --jq '.[0].databaseId'); echo "$RUN"
for i in $(seq 1 53); do s=$(gh run view "$RUN" -R "$REPO" --json status,conclusion --jq '.status+" "+(.conclusion // "")'); echo "$(date +%T) $s"; case "$s" in completed*) break;; esac; sleep 10; done
gh run view "$RUN" -R "$REPO" --json jobs --jq '.jobs[] | .name+": "+(.conclusion // .status)'
```

  On failure: `gh run view "$RUN" -R "$REPO" --log-failed`, ONE fix commit, push, wait again.
- [ ] **Step 3:** after every job is green: `$P dispatch-hil --sha "$(git rev-parse HEAD)"`. The ops `hil.yml` run for this SHA shows `verify` green (inputs validated, attestation verified, `verified-bundle` uploaded) and `pc` queued (no runner yet). Record the run ids on #9. Download the bundle on the dev box with `$P fetch-bundle --sha "$(git rev-parse HEAD)"`.

---

### Task 16: PC bootstrap (dev time only; main session)

**Precondition:** the owner's latest signal is "event skončil", and `EVENT-NOW` does not exist. On "ide event" at any step: write the flag, then the interim switch (the event runbook, `spike_window.py preempt` when a window is open) until Step 3 is done, and `$P event` from Step 3 on — the one switch-over rule of the Global Constraints. Each step's output (no site values) goes to #9.

- [ ] **Step 1: Read-only state and facts.**
  - `$P bootstrap Get-IemBootstrapState`: REAPER running or not, the app running, the driver module holders, the preference, whether our tasks exist, and the S1a window state (`spike_window.py status`).
  - `$P bootstrap Get-IemTunnelOrigin`: the origin host and port (read only; the ingress is never edited, G7). Record on #9 whether the origin peer is loopback; if not, the Task 7 Step 7 `CF-Connecting-IP` path is the one that applies.
  - `$P bootstrap Get-IemPredecessorFacts`: the `StartREAPER` task's triggers — record on #9 whether any can fire in dev time (design §5.2: a REAPER started without the card is restarted at "ide event"); the deployed app version and exe SHA-256 → `[guard] app_exe_sha256` (ops PR), and the version is the one the Task 13 exit-id derivation used — else re-derive before any switch.
- [ ] **Step 2: Elevated setup, idempotent, with read-back:** `Set-IemRootAcl`, `Register-IemTasks` (security descriptors, restart on failure), `Add-IemFirewallRule`, `Test-IemServiceRight` (→ `Grant-IemServiceRight` if missing). Defender exclusions come per bundle with Step 3.
- [ ] **Step 3: First bundle by hand** (spec §5.2), then the site check:

```bash
$P fetch-bundle --sha "$SHA"     # includes gh attestation verify on the dev box
$P install --sha "$SHA"          # the guard verifies; activation asks the exclude task for this SHA's process exclusions
$P status                        # mode event (or dev time with REAPER down), bundle recorded, pending HIL
```

  Then the site validation moved here from Task 13: the ops `site.toml` copied to the PC's private config and `iem-engine check-site` from the installed bundle (`$P` over ssh; a CI binary, not a local build — Tier 0). **From the end of this step "ide event" means `$P event`** (the guard is installed; `iemmode` starts it, `--direct` covers a guard that cannot start).
- [ ] **Step 4: The UNVERIFIED items**, one at a time, read-only or self-contained:
  - `iemmode dev --dry-run` and `iemmode event --dry-run` (plans printed; facts; preference read = original);
  - `$P probe-task`: the guard (Limited, session 1) starts `\iemmixer\iemmixer-probe`. Refused → `[guard] start_direct = true` (ops PR) and record on #9 that REAPER and the app are started by the guard's detached spawn;
  - a Limited `schtasks /Run` of the tuning task with verb `state` (or "absent" before S1c);
  - breakaway: `iemmode status` reports `breakaway: ok` from a test spawn of `iemmode.exe --version`;
  - `Register-IemRunner` with a one-time token in `ACTIONS_RUNNER_INPUT_TOKEN`, then a Ctrl-Break stop of the idle runner through the guard (`iemmode runner-stop`, dev only; the attach-console path) — the process exits within 10 s.

  Findings go on #9.
- [ ] **Step 5: The owner's alarm link — sent after the first dev entry** (the first of the two owner steps named in the design summary). The link (`/alarms`) is served by iem-server, which runs only in dev and live; in event the predecessor holds the band's address, so the owner cannot open it before the first `iemmode dev` (found on the PC, #9 2026-09-28). `dev` needs no alarm recipient (its precheck names a missing one in the switch report and `iemmode status`; the alarms stay in the guard's alarm file, which the agent reads); `live` and `live --trial` need ≥ 1. So nothing waits for the link here: it is made and sent in Task 17 Step 1, right after the first dev entry is done. There: `iem-server alarm-link` → a Slovak ❓ owner-action block (`needs-owner-action` on #9): what the link is, that he opens it once on his phone and taps "Povoliť upozornenia", and why (the guard can warn him; `live` needs ≥ 1 alarm recipient). The link's TTL is 24 h; a new one is made if it expires. Once `iem-server notify --count alarm` ≥ 1, `iemmode alarm-test` proves the path (the status then drops the note); every `live` entry waits for it; answer-independent work continues meanwhile.

---

### Task 17: First `iemmode dev`, HIL v1 green, rehearsal, the owner's test question (dev time; main session)

- [ ] **Step 1: Take the card** (no alarm recipient needed yet: the precheck names a missing one, #9 2026-09-28).
  - If the S1a window is still open with the card free, run `$P handover-s1a` first.
  - The dev entry's precheck is the running guard's code, so the guard on the PC runs `$SHA`'s build first (#9, 2026-09-28; design §5.5): after the first bundle (Task 16 Step 3, activated offline) `$P install --sha "$SHA"`, then, still in event, `$P activate --sha "$SHA"`. The guard allows it while none of iemmixer's processes runs and no switch, HIL job or interlock retry waits; it copies the bins, pins, sets the exclusions and hands over, and touches neither REAPER nor the app. `$P activate` waits until `iemmode status` names `$SHA` as `guard_build` (≤ 90 s); a refusal or a timeout is reported on #9 and nothing else runs. A guard built before this rule refuses `activate` in event (`activate is for dev; the mode is event`); for such a guard (the PC's first one) run `$P activate --sha "$SHA" --offline` instead: a graceful `iemmode quit`, its processes read until none runs (≤ 60 s, never forced), then the bundle's own `bundles\<sha>\iemmixer-guard.exe activate <sha>` (it holds the guard's mutex, activates an idle event only: bins, pin, exclusions, saved; it starts no guard), then the same wait for `guard_build` (the first `iemmode status` starts the new guard). A refused offline step starts the old guard again; report it on #9.
  - Then run `$P dev --build "$SHA"`. It prints each step. With REAPER already down (the dev-time case) the plan is: precheck (incl. the app exe hash), interlock through `iem-engine interlock` (the app runs, so the interlock always runs), app stop, tuning enter, data (`iem-migrate band` + recover), engine held, arm, server, tray, identity, runner.
  - **Expected:** app stop verdict ok — exit code 0 on the handle, ports free, no newer temp; the log line noted as corroboration: record "predecessor exit path verified on the deployed binary" on #9 (S1a acceptance item); engine `Status` measured frames 32, `missed` 0 after 10 s; the preference reads back 64 while the engine runs (the window closed); LAN and public host answer `/api/version` = SHA.
  - Any failure unwinds to event by itself: report it, fix, retry only in dev time.
  - Right after the entry is done, while iem-server serves the band's address: the owner's alarm link (Task 16 Step 5), then `iemmode alarm-test` once he has opened it.
- [ ] **Step 2: HIL.** `$P dispatch-hil` (or the queued run from Task 15) → the runner takes the job → wait for `hil/iem-pc` on the SHA:

```bash
for i in $(seq 1 53); do c=$(gh api repos/$REPO/commits/$SHA/check-runs --jq '.check_runs[] | select(.name=="hil/iem-pc") | .status+" "+(.conclusion // "")'); echo "$(date +%T) $c"; case "$c" in completed*) break;; esac; sleep 10; done
```

  Every HIL v1 check (design §7) green. Record its numbers on #9: measured frames, callbacks, missed, resets, callback CPU p50/p99.9, fault callback duration, reopen gap, pipe DACL, the masked test signal's per-TX meter readings.
- [ ] **Step 3: F30 in HIL** (a synthetic `install-site` change and revert) green.
- [ ] **Step 4: Rehearse the event path** (before the first real "ide event"; not a switch):
  - `$P event --dry-run`: the plan printed ends with `Fingerprint`; no mutating call;
  - `$P rehearse-teardown`: engine stop, server Ctrl-Break, tray stop, tuning exit, preference check; asserted: module unheld, preference = original, ports 80/443 free; then back in `dev` with the same SHA. It never starts REAPER;
  - `$P probe-task` from the running guard once more.

  Record the timings on #9. A failure is fixed and rehearsed again in dev time before anything else.
- [ ] **Step 5: The owner's test question — asked now** (design §10: before any long unattended engine run, and dev mode is one). Load the `user-questions-slovak` skill, then post the one self-contained Slovak `❓` block: the five tests in plain words, one decision — approve a ~45 min dev-time session with the owner at the PC for the hard kill (D5(b) loopback included). Track it on #9 (`needs-answer` + the question). Until the tests pass, `[guard] pc_tests_passed` stays `false`, so `live --trial` is refused; dev work continues.

---

### Task 18: First `iemmode event` on the owner's signal; report; hand-offs (main session)

- [ ] **Step 1:** on the owner's next "ide event": write the flag, then run `$P event`.
  - **Expected:** jobs cancelled, runner stopped, engine released ≤ 10 s, server and tray stopped, tuning exit, preference = original, no other module holder, REAPER through our task (a REAPER already running without the card is saved, quit and restarted), the handover checks (tracks, no dialog, bridge once, heartbeat, module held by REAPER, peaks or `UNCONFIRMED-AUDIO`), the app through our task answering with the member count (an app that runs without serving is restarted), public host 200, the S1c fingerprint (alarm only).
  - Confirm to the owner (✅, Slovak, one line).
  - A release timeout with a healthy engine: iemmixer keeps serving; send the prepared ❓ (Task 13 Step 5). A dead or parked engine, or a REAPER that cannot start: the prepared ❓ with the reboot offer. Never force.
- [ ] **Step 2:** on the next "event skončil": `$P dev --build <latest green dev SHA>`, the full event → dev path: the interlock through REAPER's meters, the app stopped first, then REAPER's save (no dialog) and quit, the data refresh. **Acceptance box 1 is met only after both directions pass on real signals.**
- [ ] **Step 3: Report on #9** (Slovak, plain, numbers):
  - both switches with their checks and durations (the handover time against the ≤ 120 s bound and spec §4.3's 90 s);
  - HIL v1 and the rehearsal;
  - the predecessor exit path;
  - the preference window read-backs;
  - the UNVERIFIED items resolved.

  Add `## 12. Results` to the design note (`docs(s6): results`). Tick the acceptance boxes only on evidence.
- [ ] **Step 4: Hand-offs** (comments):
  - **#10 (S7):** runner, `hil.yml`, `iemmode report`, the soak inputs, the measured handover time;
  - **#11 (S8):** `live --build`, pins, the site tables, the cutover pieces, `pin_changes` on at cutover, the `pc_tests_passed` gate;
  - **#15 (S1c):** the guard's tuning calls, drift checks and fingerprint call.

---

### Task 19: PR and merge (only when the run's orchestrator asks for it)

- [ ] **Step 1:** update the required checks: `bundle` now, and `hil/iem-pc` from the ops App once the runner has produced one green result on `main`'s candidate (S0 plan Task 15 Step 6; a skipped required job counts as passing, so `needs` chains stay required).
- [ ] **Step 2:** open the `dev` → `main` PR. Its body: summary, the two switches, HIL v1, checks, #9. Wait for every check including the mutation shards; kill survivors in the portable modules. Merge with `gh pr merge --merge` per `pr-merge-policy`, then bump `dev` first thing.

## Hand-off to later sub-projects

- **S7 (#10):** the runner and `hil.yml`, `iemmode report` for soak summaries, the switch timing (target ≤ 60 s silence; tighten the ≤ 120 s handover bound toward spec §4.3's 90 s), live Playwright against the PC, band-activity thresholds on real signal, S1c W6 (8 h at 32 with engine, server and stream).
- **S8 (#11):**
  - `live --build` (G8) and the trial crash loop to `event`; trials need `pc_tests_passed`;
  - pins and revert;
  - the server site tables in the ops repo;
  - cutover pieces not built here: guard task at logon, the predecessor's autostarts disabled with values exported, the tunnel repair switch, PIN changes enabled (`pin_changes = true`).
- **S1c (#15):** the guard calls `enter`/`exit`/`state` and the fingerprint through the elevated task; drift on mode change and hourly (native reads); logon reconciliation; L5 through `iem-win`.
