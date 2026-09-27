# iemmixer S6 — ASIO Backend, Guard and HIL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Tasks 15–19 (pushes, CI waits, the PC, owner signals, report, PR) run in the main session, never in a subagent. A subagent never touches the PC.

**Goal:** iemmixer runs on the IEM PC.
- The ASIO backend sits behind `iem_audio_io::Process` at 32 samples.
- The guard (`iemmixer-guard`, `iemmode`) switches REAPER ↔ iemmixer on the owner's "ide event" / "event skončil", with handover checks both ways.
- Bundles are installed, pinned and reverted from attested CI zips.
- The server holds the band's usual address while iemmixer runs (P9).
- HIL v1 runs through the private ops repo (ticket #9, program #1).

**Architecture:**
- **`iem-win`** (new): safe Windows glue, stubs elsewhere.
- **`iem-audio-io`:**
  - portable `channels.rs`, `prefwin.rs`, `reset.rs`, `rtpanic.rs`;
  - `asio.rs` grows into `AsioStream<P>`.
- **`iem-engine`:** the ASIO backend, `[card]`, exit 3, `--hold`/`Arm`, role `supervisor`, Windows pipe hardening, `interlock` and `check-site`.
- **`iem-guard`** (new):
  - a pure core: planner, crash loop, bundles, verdicts, protocol;
  - `Pc` effects: `WinPc` on Windows, `FakePc` in tests;
  - the daemon and the `iemmode` CLI.
- **`iem-server`:** stage-only band activity, graceful stop, alarm link.
- **`iem-tray`:** without a server.
- **CI:** `bundle`, `attest`, `hil-dispatch`.
- **Ops repo:** `hil.yml`, the site tables, the PC runbook.
- **Dev box:** `scripts/iem-pc/iempc.py` (ssh control with the EVENT-NOW discipline) and `IemPc.psm1` (bootstrap on the PC).

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
  - On "ide event" the session first writes the flag (`date -Iseconds > ~/.config/iemmixer/EVENT-NOW`), then runs `iempc event` (from Task 14 on; before that, the event runbook). Then it confirms to the owner.
  - Never infer an event, never ask whether one runs, never switch on your own. A switch drill without an owner signal is switching: not allowed.
- **Nothing is force-ended (I8, P4):**
  - REAPER quits by 40004 after 40026; the predecessor app exits through its tray Exit command (design §5.3); the engine through `Shutdown`; the server and runner through Ctrl-Break to their own process group; the tray through `Quit`.
  - The integrity scan's force-kill words never appear in `crates/`, `scripts/`, `.github/`, `e2e/` — comments included. Write "force-end" in prose.
- **The card:**
  - Only 96 kHz and preferred 32 for iemmixer (`format::admit`). The preference holds 32 only inside the open window (design §3). REAPER never starts unless the preference reads back as the recorded original.
  - `set_sample_rate`, `set_clock_source`, `open_control_panel` never appear (integrity). Every output is zeroed (A1). Dante is never touched.
- **I3:** the engine refuses while `reaper.exe` exists; the guard never starts REAPER while an engine process exists or the driver module has any holder.
- **G7:** the predecessor's code, config, deployment and autostarts stay untouched. We start it only through our own `\iemmixer\iemmixer-StartApp` (its exe directly), never its launcher script.
- **P5/G8:** only a bundle zip from a green hosted `push` run on `dev`/`main`, attested by digest, reaches the PC. The first one is installed by hand after `gh attestation verify` on the dev box; later ones through `hil.yml`. `live --build` needs `main` + green `hil/iem-pc`.
- **P6:** no site value in this repository. That covers driver name, registry key, task paths other than our own `\iemmixer\…`, the predecessor's process/log names and exit command id, channel numbers, track counts, hosts, users and paths. They live in the ops `site.toml` (`[card]`, `[guard]`) and `~/.config/iemmixer/iem-pc.env`. Tests use synthetic values (driver `Test Card`, RX 101–132, TX 71–93, exit id 4242).
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
   - `AsioStream` calls `leave` right after `createBuffers`, on every open, success or failure. The guard's `PrefCheck` step precedes `ReaperStart` in every plan.
   - Tests: `prefwin::tests`, `plan::tests::every_event_plan_checks_the_preference_before_reaper`.
2. **Sound on the band's channels.**
   - Expected: every card output is zeroed each callback. TX gets processor output only after `Arm` (hold) or on respawn. The test signal cap is unchanged. Unknown RX/TX refuse the stream.
   - Tests: `channels::tests`, engine `hold_keeps_outputs_silent_until_arm`, HIL `test-signal`.
3. **Nothing force-ended.**
   - Expected: no kill path in the guard. Every stop is a request plus a bounded wait, then an alarm.
   - Tests: integrity scan; `crash::tests` (no respawn after exit 2/3 or at session end); `FakePc` records no force verb (there is none to record).
4. **"ide event" at any moment.**
   - Expected: `event` pre-empts after the current bounded step. A guard restart mid-switch re-plans to `event`, and HIL jobs are refused once the switch starts.
   - Tests: `daemon::tests::event_preempts_a_running_dev_switch`, `plan::tests::a_failed_dev_switch_unwinds_to_event`.
5. **Meter bridge.**
   - Expected: triggered exactly once, only while its state is empty; any other value refuses.
   - Tests: `handover::tests::bridge_*`.
6. **Predecessor exit.**
   - Expected: the guard posts only the configured command to a window of the configured class owned by the app's PID. Success needs the log line, process gone, ports free and no newer temp file. Anything else aborts the switch and restarts REAPER.
   - Tests: `handover::tests::app_exit_*`, `plan::tests`.
7. **Provenance (P5/G8).**
   - Expected: `install` verifies `SHA256SUMS` and `manifest.sha` equal to the directory name and never overwrites. `live` refuses non-`main` or non-green.
   - Tests: `bundle::tests`.
8. **Pipes.**
   - Expected: remote clients refused, the first instance only, DACL user + SYSTEM, non-blocking readers that close a superseded connection. The guard pipe is the same.
   - Tests: `pipes.rs` now on Windows too; `pipe::tests::sddl_*`.
9. **RT safety of the backend.**
   - Expected: `on_buffer` touches only preallocated buffers. The panic hook on RT threads writes atomics only.
   - Tests: `rtpanic::tests::the_rt_hook_does_not_allocate` (`assert_no_alloc` dev-dependency), code review.
10. **P6:** no site value in the diff; the pre-push denylist and CI `secrets`.

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
Cargo.toml                                   members + iem-win, iem-guard; version bump
crates/iem-win/{Cargo.toml,src/lib.rs}       safe Windows glue; non-Windows stubs return Unsupported
crates/iem-win/src/{process,window,console,token,power,spawn,registry}.rs  (cfg(windows) bodies)
crates/iem-audio-io/src/channels.rs          topology card numbers → card indices
crates/iem-audio-io/src/prefwin.rs           preferred-buffer window (PrefStore trait, enter/leave)
crates/iem-audio-io/src/reset.rs             ResetBudget, stall rule
crates/iem-audio-io/src/rtpanic.rs           RT-thread panic record (atomics only)
crates/iem-audio-io/src/asio.rs              AsioStream<P>: owner thread, callback, reopen, SEH, session end
crates/iem-audio-io/src/lib.rs               Process::discontinuity; StreamStats += missed, overruns, resets, parked
crates/iem-engine/src/site.rs                [card] table
crates/iem-engine/src/engine.rs              --backend asio, --hold, interlock, check-site, exit 3
crates/iem-engine/src/control.rs             supervisor role handling, Arm, Status fields
crates/iem-engine/src/pipe.rs                Windows listener options, SDDL, non-blocking readers
crates/iem-engine/src/rt.rs                  hold gate, discontinuity → fade-in
crates/iem-engine-proto/src/msg.rs           Role::Supervisor, Cmd::Arm, Status fields (additive)
crates/iem-server/src/{activity,console}.rs  watch [activity] inputs only (RED/GREEN)
crates/iem-server/src/bin/server.rs          graceful stop (Ctrl-Break / SIGTERM); alarm-link
crates/iem-server/src/alarm_link.rs (+route) one-time alarm subscription link
crates/iem-core/src/config.rs                ActivityConfig.inputs
crates/iem-guard/src/{lib,plan,crash,bundle,handover,proto,state,alarms}.rs   pure core
crates/iem-guard/src/pc.rs                   Pc trait + FakePc (tests)
crates/iem-guard/src/win/{mod,reaper,app,card,tasks,procs}.rs                  WinPc (Windows)
crates/iem-guard/src/daemon.rs               request loop, switch runner, watches, adoption
crates/iem-guard/src/bin/{iemmixer-guard,iemmode}.rs
crates/iem-tray/src/{lib,tray}.rs            no server; guard status, Quit
scripts/iem-pc/IemPc.psm1, Test-IemPc.ps1    PC bootstrap functions + self-test
scripts/iem-pc/iempc.py (+test_iempc.py)     dev-box control with EVENT-NOW discipline
scripts/check_integrity.py (+test)           I8 words in comments too; new crate dirs covered
scripts/engine-deps-allow.txt                + iem-win (+ its closure)
.cargo/mutants.toml                          exclude Windows effect modules with reasons
.github/workflows/ci.yml                     windows job widened; bundle, attest, hil-dispatch
.claude/rules/guard.md                       playbook rule (paths: crates/iem-guard/**, crates/iem-win/**, scripts/iem-pc/**)
CLAUDE.md                                    router line; "ide event" = iempc event
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
```

If `main` now carries `2.0.0-dev.8`, set `[workspace.package].version = "2.0.0-dev.9"` as the first commit: `chore: bump version to 2.0.0-dev.9`.

- [ ] **Step 2: Commit the design note and this plan** (`docs(s6): design note and implementation plan`). Before committing, run the denylist scan over both files (the pre-push hook does it again).

- [ ] **Step 3: Design summary on #9** (Slovak, plain). Write `$WP/s6-design-comment.md` covering:
  - the switch sequences;
  - the predecessor exit through its tray command;
  - the preference window;
  - the HIL decision;
  - the five approval-gated tests (listed, not asked).

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
  - whether `ureq`/`zip`/`rustls` are already locked.

  If `windows-registry` 0.100 cannot read the value kind, use `windows-sys` `RegQueryValueExW` inside `iem-win::registry` instead (the only change). Findings go on #9 the moment they land.

---

### Task 2: `iem-win` — safe Windows glue

**Files:**
- Create: `crates/iem-win/Cargo.toml`, `crates/iem-win/src/lib.rs` and one module per area.
- Modify: `Cargo.toml` (members), `scripts/engine-deps-allow.txt`, `.cargo/mutants.toml`.

- [ ] **Step 1: The crate skeleton.** `Cargo.toml`:

```toml
[package]
name = "iem-win"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
description = "iemmixer's Windows glue (S6): safe wrappers; every function returns Unsupported off Windows"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61", features = [
  "Win32_Foundation", "Win32_Security", "Win32_Security_Authorization", "Win32_System_Threading",
  "Win32_System_ProcessStatus", "Win32_System_Diagnostics_ToolHelp", "Win32_System_Console",
  "Win32_System_JobObjects", "Win32_UI_WindowsAndMessaging", "Win32_System_Memory",
  "Win32_System_Power", "Win32_System_SystemInformation" ] }
windows-registry = "=0.100.0"
```

`src/lib.rs`:

```rust
//! Safe Windows glue for the engine and the guard (S6 design note §3). Every
//! function has a portable signature; off Windows it returns `Unsupported`, so
//! callers keep their decisions testable on Linux. The only unsafe code of the
//! workspace besides `iem_audio_io::asio` lives in the `cfg(windows)` modules.

#![cfg_attr(not(windows), forbid(unsafe_code))]

use std::io;

pub mod console;
pub mod power;
pub mod process;
pub mod registry;
pub mod spawn;
pub mod token;
pub mod window;

pub(crate) fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "Windows only"))
}
```

- [ ] **Step 2: Module API** (each function: a `#[cfg(windows)]` body with `// SAFETY:` notes, and a `#[cfg(not(windows))]` body `crate::unsupported()`):

| Module | Functions |
|---|---|
| `process` | `exists(image: &str) -> io::Result<bool>`; `pids(image: &str) -> io::Result<Vec<u32>>`; `module_holders(module: &str) -> io::Result<Vec<(u32, String)>>` (Toolhelp32 snapshots of every process's modules; access-denied processes are skipped and counted); `start_time(pid) -> io::Result<u64>`; `image_path(pid) -> io::Result<String>`; `wait_gone(pid, Duration) -> io::Result<bool>`; `listening(port: u16) -> io::Result<Option<u32>>` (GetExtendedTcpTable, owning pid) |
| `power` | `set_high_priority()`, `disable_power_throttling()` (EXECUTION_SPEED and IGNORE_TIMER_RESOLUTION), `set_cpu_sets(ids: &[u32])`, `lock_min_working_set(extra_mb: usize)` (QUOTA_LIMITS_HARDWS_MIN_ENABLE, current + extra) |
| `token` | `current_user_sid() -> io::Result<String>` (ConvertSidToStringSidW) |
| `window` | `find_owned(class: &str, pid: u32) -> io::Result<Option<isize>>` (EnumWindows, GetClassNameW, GetWindowThreadProcessId); `post_command(hwnd: isize, id: u16) -> io::Result<()>` (PostMessageW WM_COMMAND, wParam = id); `has_dialog(pid) -> io::Result<bool>` (a visible top-level `#32770` window owned by pid); `SessionEndWindow` (a hidden window on the calling thread that sets an `Arc<AtomicBool>` on WM_ENDSESSION(TRUE), answers WM_QUERYENDSESSION TRUE, and holds a `ShutdownBlockReasonCreate` text while a supplied closure runs) |
| `console` | `ctrl_break(pid: u32) -> io::Result<()>` (GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) for a child started with CREATE_NEW_PROCESS_GROUP on our console) |
| `spawn` | `spawn_detached(cmd: &mut std::process::Command, new_group: bool) -> io::Result<std::process::Child>` (`CommandExt::creation_flags`: `CREATE_BREAKAWAY_FROM_JOB`, plus `CREATE_NEW_PROCESS_GROUP` when `new_group`, plus `CREATE_NO_WINDOW`). When the job forbids breakaway it returns the error; the caller alarms and does not start the child |
| `registry` | `Hkcu::read(key, name) -> io::Result<(Kind, String)>`, `Hkcu::write(key, name, Kind, &str)`, `Kind { Dword, Text }` |

No function in `iem-win` ends another process in any form.

- [ ] **Step 3: Tests.**
  - Portable: every stub returns `Unsupported` (one test per module on Linux).
  - Windows (the `windows` job):
    - `current_user_sid` starts with `S-1-5-21-`;
    - `exists("definitely-not-running.exe")` is false;
    - `listening` finds a `TcpListener` bound by the test;
    - `module_holders("kernel32.dll")` contains our own pid;
    - `registry` round-trips a DWORD and a string under `HKCU\Software\iemmixer-test\<uuid>`, which the test then deletes with `windows-registry` (it removes only its own test key).
    - `spawn_detached` + `ctrl_break` stops a child that waits on `tokio::signal::windows::ctrl_break`: `tests/ctrl_break.rs` with a helper bin `iem-win-ctrlbreak-helper` (dev only, `[[bin]] required-features = ["test-helper"]`).
- [ ] **Step 4: Allowlist and mutation scope.**
  - `iem-win` joins the engine closure: add `iem-win` and every new name from `cargo tree -p iem-engine --target x86_64-pc-windows-msvc -e normal,build --prefix none | sort -u` to `scripts/engine-deps-allow.txt`.
  - In `.cargo/mutants.toml` `exclude_globs`: `crates/iem-win/src/**` with the reason "Windows FFI wrappers, not compiled on Linux; the windows job runs their tests; decisions live in callers".

---

### Task 3: Portable backend pieces in `iem-audio-io`

**Files:**
- Create: `crates/iem-audio-io/src/{channels,prefwin,reset,rtpanic}.rs`.
- Modify: `crates/iem-audio-io/src/lib.rs` (module list; `Process::discontinuity`; `StreamStats` fields), `crates/iem-audio-io/src/nullrt.rs` (new fields default 0).

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

- [ ] **Step 2: `prefwin.rs`.**

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
```

  Tests (`FakeStore` holding a `Pref`, counting writes, optionally failing or corrupting the n-th write):
  - `enter` writes `"32"` with kind `Dword` when the original is a DWORD `"64"`, and `Text` for a text original;
  - `enter` refuses `NotOriginal` without writing when the store holds `"32"`, and also for the same digits with the other kind;
  - a corrupting store yields `ReadBack`;
  - `leave` restores byte-for-byte (`" 64"` stays `" 64"`);
  - a write error is `Write`.

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

/// Marks the calling thread real-time (the callback does this on entry; a
/// const thread-local, so no allocation on first use).
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
  - `record` keeps the last 96 bytes of a long path, and `latest` returns them with the count;
  - `the_rt_hook_does_not_allocate`: a test thread calls `mark_rt_thread`, then `assert_no_alloc::assert_no_alloc(|| record(file!(), line!(), column!()))`. Add `assert_no_alloc` as a dev-dependency (already locked for `iem-engine`), and note `rtpanic` in the crate's `#[global_allocator]` test harness;
  - `install` + a panic caught by `catch_unwind` on a marked thread increments the count and prints nothing (run in its own test binary, `tests/rtpanic_hook.rs`, since the hook is global).

- [ ] **Step 5: `lib.rs`.** Add the modules, then:
  - `Process::discontinuity(&mut self) {}` with the doc "called on the callback thread before the first block after a reopen; the engine restarts its fade-in";
  - to `StreamStats`: `missed`, `overruns`, `resets: u64`, `parked: bool` (NullRt keeps them 0/false).

  Update the module doc: S6 backend, preference window.

---

### Task 4: `asio.rs` — the backend `AsioStream<P>`

**Files:** `crates/iem-audio-io/src/asio.rs` (Windows only), `crates/iem-audio-io/Cargo.toml` (`iem-win` dependency on Windows).

- [ ] **Step 1: Keep the spike API** (`Host`, `Running`, `StreamConfig`) for `examples/asio_spike.rs` unchanged. Add the backend beside it.

```rust
pub struct CardConfig {
    pub driver: String,
    pub frames: i32,                          // 32 (I2)
    pub pref: Option<(String, String, Pref)>, // (key, value name, original); None only in tests
}

pub struct AsioStream<P: Process + 'static> { /* owner thread handle, shared atomics */ }

impl<P: Process + 'static> AsioStream<P> {
    /// Starts the owner thread; returns once the first callback ran or the open failed.
    pub fn start(card: CardConfig, rx: Vec<u16>, tx: Vec<u16>, processor: P) -> Result<Self, AsioError>;
    pub fn stats(&self) -> StreamStats;   // callbacks, late, missed, overruns, resets, parked, faulted, max_process_ns, fault
    pub fn session_ending(&self) -> bool; // WM_ENDSESSION seen on the owner thread
    pub fn force_reopen(&self);           // dev-only flag path (HIL); goes through ResetBudget
    pub fn stop(self) -> StopOutcome;     // Released | Parked
}
```

- [ ] **Step 2: The owner thread** (the only thread calling the driver):
  1. `rtpanic` is already installed by the engine. Create a `SessionEndWindow` on this thread.
  2. `open()`:
     - `prefwin::enter(store, original, 32)`, then `Host::open(driver)`, `info()`;
     - `ChannelMap::new(rx, tx, info.inputs, info.outputs)`;
     - `format::admit(rate, preferred, 32, types)`;
     - `create_buffers` for every card channel, then `prefwin::leave(store, original)`. `leave` runs on every exit of `open()` after a successful `enter`, success or not (a guard struct whose `Drop` calls `leave` and records a failure in an atomic the control loop turns into `Alarm{pref}` and exit 3).
     - Then `start()`.
  3. Loop every 5 ms:
     - pump messages;
     - if a reset or size request is flagged, or `reset::stalled(...)`, ask `ResetBudget`: `Reopen` → `finish` + `open` again with the processor carried over, then `processor.discontinuity()` before the first new block (a flag the callback consumes); `Fault` → faulted.
     - if the session is ending, set `session_ending` (the control loop does the save/fade/stop);
     - a stop request → `finish`.
  4. `finish`:
     - stop, clear `STREAM`, wait until `IN_FLIGHT == 0` (bounded `STOP_WAIT`, pumping);
     - on timeout set `parked`, leak the stream (never free under a callback), and keep the thread alive pumping messages;
     - otherwise dispose, release, and return the processor.
- [ ] **Step 3: The callback** (`on_buffer`, extending the spike's):
  1. `mark_rt_thread()` on the first entry (a flag in the stream).
  2. Telemetry.
  3. Zero every output half.
  4. Unless faulted:
     - for each `k`, `format.decode(read(input[map.rx[k]]), &mut self.inbuf[k*frames..])`;
     - build a `Block` over `inbuf`/`outbuf` and call `processor.process(&mut block)` in `catch_unwind`; on a panic set faulted and zero `outbuf`;
     - `format.encode(&outbuf[k*frames..], output[map.tx[k]])`.
  5. `output_ready`.

  The processor lives in an `UnsafeCell` inside the stream. ASIO callbacks never overlap, and the owner thread touches the processor only after `IN_FLIGHT == 0` (`// SAFETY:` note). `inbuf`/`outbuf` are preallocated `Vec<f64>` sized at open.
- [ ] **Step 4: SEH filter** (installed once by the engine, `iem_audio_io::asio::install_seh_filter()`):
  - on an exception it sets a `SEH` atomic that the owner thread sees (it stops the driver);
  - it waits ≤ 1 s for `STREAM` to be null, then returns `EXCEPTION_CONTINUE_SEARCH`;
  - if the stream is still set, the faulting thread sleeps forever (parked).

  Only the owner-approved `seh_ctl` test exercises it (design §10); the code carries no test hook beyond `--fault-injection`'s panic.
- [ ] **Step 5: `.cargo/mutants.toml`:** `asio.rs` stays excluded. The CI `windows` job runs `cargo clippy -p iem-audio-io --all-targets -D warnings` and `cargo test -p iem-audio-io`. `AsioStream::start` with driver `No Such Card` returns `AsioError::NotFound` — a test on the hosted runner, which has no ASIO driver (`NoDrivers` or `NotFound` both pass).

---

### Task 5: Engine — card, hold/arm, supervisor, interlock, check-site

**Files:** `crates/iem-engine-proto/src/msg.rs`, `crates/iem-engine/src/{site,engine,control,rt,core}.rs`, `crates/iem-engine/src/bin/iem-engine.rs`, `config/test-site.toml`, `crates/iem-engine/Cargo.toml`.

- [ ] **Step 1 (RED): proto and control tests first.**
  - `Role::Supervisor` round-trips as `"supervisor"`.
  - A supervisor may `Shutdown`, `SaveNow`, `Arm`, `GetState`, `Ping`, and the test-signal and fault ops (still refused without their flags). It gets `NotController` for mix changes.
  - A second supervisor replaces the first (the first gets `Superseded`); `control` is untouched.
  - `hold_keeps_outputs_silent_until_arm`: `Offline` with `Options { hold: true }` renders zeros until an `RtOp::Arm`, then fades in over 500 ms.
  - `discontinuity_restarts_the_fade_in`.
  - `Status` serialises `missed`, `overruns`, `resets`, `parked`, and an old client ignores them (additive).

  Commit: `test(engine): [red] supervisor role, hold until arm, discontinuity fade-in`.
- [ ] **Step 2 (GREEN): implement.**
  - `Role::Supervisor`; `Cmd::Arm` (add `"arm"` to `OPS`); `Cmd::is_supervisor(&self)`.
  - `Control` keeps `supervisor: Option<u64>`.
  - `rt::Options { hold }`: the fade stays at 0 until `RtOp::Arm`. `Processor::discontinuity` restarts the fade-in.
  - `Status` fields filled from `StreamStats`.

  Commit: `feat(engine): [green] supervisor role, hold until arm, discontinuity fade-in`.
- [ ] **Step 3: `[card]` in the site** (`site.rs`, `deny_unknown_fields` like `[engine]`):

```toml
# config/test-site.toml (synthetic)
[card]
driver = "Test Card"
frames = 32
pref_key = 'Software\ASIO\Test Card'
pref_name = "PrefBuffSize"
pref_original = { kind = "dword", raw = "64" }
```

  `frames` must be 32 (I2; anything else is a site error). The table is optional for `nullrt` and required for `--backend asio`.
- [ ] **Step 4: `run --backend asio|nullrt [--hold]`** (Windows only for `asio`; elsewhere a usage error).
  - `run` starts with `rtpanic::install()`, `asio::install_seh_filter()`, and `iem_win::power::{set_high_priority, disable_power_throttling}` (+ CPU Sets from `[card] cpu_sets` when present, S1c).
  - After 5 s of streaming it calls `lock_min_working_set(64)`.
  - Refusals map to the new `EngineError::Card(String)` → exit 3: `iem_win::process::exists("reaper.exe")` (I3), `AsioError::{NoDrivers, NotFound, Refused}`, `ChannelMap` errors, `PrefError`.
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

### Task 7: Server — stage-only band activity, graceful stop, alarm link

**Files:** `crates/iem-core/src/config.rs`, `crates/iem-server/src/{activity,console}.rs`, `crates/iem-server/src/bin/server.rs`, `crates/iem-server/src/lib.rs`, `crates/iem-server/src/alarm_link.rs`, `crates/iem-ui` (alarm page), `config/test-site.toml`, `e2e/`.

- [ ] **Step 1 (RED): band activity ignores non-stage inputs** (S1a finding).
  - `ActivityConfig` gains `inputs: Vec<String>` (engine input ids; default empty = every input with category `mics`).
  - `console::tests::program_input_signal_does_not_raise_band_activity`: a meter stream where only a `tech` input (the synthetic `content`) sits at −1 dBFS for 300 s → no `BandActivity{active:true}`.
  - `…stage_input_activity_still_raises_it`: the same on `mic1` → on after 120 s.

  Commit: `test(server): [red] band activity must ignore the program input`.
- [ ] **Step 2 (GREEN):** `max_input_peak(&m)` becomes `max_watched_peak(&m, &watched)`, where `watched` is the resolved input indices (explicit list or category `mics`). Unknown ids are reported at connect and left out. Commit: `fix(server): [green] band activity watches only the stage inputs`.
- [ ] **Step 3: Graceful stop.** `run_server` awaits `shutdown_signal()`:
  - Unix: SIGTERM or SIGINT; Windows: `tokio::signal::windows::ctrl_break()` or `ctrl_c()`;
  - then `axum::serve(...).with_graceful_shutdown(...)` with a 5 s bound; the backup daemon and the engine client close; exit 0.

  Test (Unix): spawn the server binary with a temp config, send SIGTERM, expect exit 0 within 6 s and the port free. The Windows variant is in the `windows` job with `iem_win::spawn` + `ctrl_break`.
- [ ] **Step 4: Alarm link.**
  - `iem-server alarm-link [--ttl-h 24]` writes one random 128-bit token (hash only) to `alarm_link.json` next to the config and prints `https://<https_domain>/alarms?t=<token>`.
  - `POST /api/alarms/subscribe {token, subscription}` accepts it once, unexpired, appends the subscription to `alarm_subscriptions.json` (atomic write) and deletes the token.
  - UI route `/alarms`: one button "Povoliť upozornenia" → push permission → POST, with a result message.
  - Tests: unit (token single use, expiry, bad token 403 without a timing difference beyond the hash compare), E2E (mock push subscription, zero console errors).

  Commit: `feat(server): one-time alarm subscription link for the owner (S6 bootstrap)`.
- [ ] **Step 5:** verify `notify::run_cli` only reads subscriptions (no write). If it prunes expired ones, keep that — it is the same atomic write the server uses — and document it in `server-engine.md`.

---

### Task 8: `iem-guard` — the pure core

**Files:** create `crates/iem-guard/{Cargo.toml,src/lib.rs,src/plan.rs,src/crash.rs,src/bundle.rs,src/handover.rs,src/proto.rs,src/state.rs,src/alarms.rs}`.

- [ ] **Step 1: `Cargo.toml`.**
  - Permissive; deps: `serde`, `serde_json`, `toml`, `sha2`, `thiserror`, `tracing`, `iem-win`, `iem-audio-io` (for `prefwin`), `interprocess` (guard pipe, sync), `zip` (deflate), `ureq`.
  - Two bins: `iemmixer-guard`, `iemmode`.
  - Not in the engine closure: `check_engine_deps.py` is unaffected.
- [ ] **Step 2: `plan.rs`.**

```rust
//! The switch planner (S6 design note §5.2). Pure: facts in, ordered steps out.
//! Every step is conditional on facts re-read before it runs, so re-running a
//! plan is safe; a failed or interrupted switch into dev/live unwinds with
//! `plan(current, Mode::Event, facts)`.

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
    ReaperSaveQuit,
    AppStop,
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
    ServerStop,
    TrayStop,
    TuningExit,
    PrefCheck,
    ReaperStart,
    ReaperHandover,
    AppStart,
    AppHandover,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Facts {
    pub reaper: bool,
    pub app: bool,
    pub engine: bool,
    pub server: bool,
    pub tray: bool,
    pub runner: bool,
    /// `live` before cutover (a rehearsal): the band is there on purpose.
    pub trial: bool,
    /// Owner-instructed `--force`: skips the interlock only.
    pub force: bool,
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
            if !f.reaper {
                out.push(Step::ReaperStart);
            }
            out.push(Step::ReaperHandover);
            if !f.app {
                out.push(Step::AppStart);
            }
            out.push(Step::AppHandover);
        }
        Mode::Dev | Mode::Live => {
            out.push(Step::Precheck);
            let band_there = to == Mode::Live && f.trial;
            let from_band = from == Mode::Event || (from == Mode::Live && to == Mode::Dev);
            if from_band && !band_there && !f.force {
                out.push(Step::Interlock);
            }
            if f.reaper {
                out.push(Step::ReaperSaveQuit);
            }
            if f.app {
                out.push(Step::AppStop);
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
```

  Tests:
  - `every_event_plan_checks_the_preference_before_reaper`: for all 64 fact combinations × 3 `from` modes, `PrefCheck` precedes `ReaperStart`/`ReaperHandover`, and `EngineStop` precedes `PrefCheck`;
  - `event_to_dev_quits_reaper_and_the_app_after_the_interlock`;
  - `a_trial_skips_the_interlock`; `force_skips_only_the_interlock`;
  - `live_to_dev_runs_the_interlock`; `dev_to_live_does_not`;
  - `the_runner_starts_only_in_dev`;
  - `event_in_event_only_checks` (`[TuningExit, PrefCheck, ReaperHandover, AppHandover]`);
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
  - `verify(dir_files: &[(name, sha256)], sums, manifest, dir_sha)` — every summed file present and equal, no unsummed file, `manifest.sha == dir_sha`;
  - `may_go_live(&Record) -> Result<(), String>` (`main` + `Green`);
  - `Pins { current: Option<String>, previous: Option<String> }` with `promote(sha)` and `revert()`.

  Tests cover each refusal plus path traversal in sums. The zip extraction (Task 10) uses `zip` with `enclosed_name()` and refuses any entry outside the directory.
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
    pub logged: bool,
    pub gone: bool,
    pub ports_free: bool,
    pub newer_temp: bool,
}

pub fn app_exit(f: AppExit) -> Result<(), Vec<&'static str>> {
    let mut bad = Vec::new();
    if !f.logged { bad.push("no tray-exit log line after the command"); }
    if !f.gone { bad.push("the app did not exit within 30 s"); }
    if !f.ports_free { bad.push("ports 80/443 still held"); }
    if f.newer_temp { bad.push("a temp file newer than the command: a write was cut"); }
    if bad.is_empty() { Ok(()) } else { Err(bad) }
}
```

  Tests:
  - `bridge_*`: `"1"`, `""`, `"0"`, `"2"`;
  - every single failure of `reaper_handover` is named, all −∞ → `Unconfirmed`, one stage at −40 → `Confirmed`;
  - every `app_exit` field on its own.
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
    TestSignal { input: String, dbfs: f64, ttl_s: f64 },
    Report { sha: String, hil: String, detail: String },
    JobBegin { run: u64 },
    JobEnd { run: u64 },
    InstallSite { path: String },
    ForceReopen,
    /// Dev only: stop the idle runner (bootstrap check, Task 16).
    RunnerStop,
    AlarmTest,
    AlarmAck { id: u64 },
    Quit,
    Subscribe,
}
```

  Replies are `{ok, mode, switching, alarms, detail}`. Tests: round trip of every variant; oversize and garbage frames are refused.
- [ ] **Step 7: `state.rs`, `alarms.rs`.**
  - The persistent guard state: `mode`, `switching: Option<{from, to, done: Vec<Step>, started}>`, `pids`, `bundles`, `pins`, `app_exit_hashes`. It is written atomically (temp, fsync, rename) and loaded with defaults.
  - Alarms: an append-only list with ids, ack, and the newest 50 kept.
  - Tests: round trip; a corrupt file → defaults + an alarm "guard state unreadable".

  Commit: `feat(guard): pure core — planner, crash loop, bundles, handover verdicts, protocol (S6)`.

---

### Task 9: `iem-guard` — the effects (`Pc`, `WinPc`, `FakePc`)

**Files:** `crates/iem-guard/src/pc.rs`, `crates/iem-guard/src/win/{mod,reaper,app,card,tasks,procs}.rs`.

- [ ] **Step 1: The trait** (every method bounded in time; no method ends a process):

```rust
pub trait Pc {
    fn facts(&mut self) -> Facts;
    fn reaper_meters(&mut self, seconds: u32) -> Result<Vec<f64>, String>;   // stage tracks, max dBFS each
    fn engine_interlock(&mut self, seconds: u32) -> Result<(bool, String), String>;
    fn reaper_save_quit(&mut self) -> Result<(), String>;      // 40026, mtime changed ≤ 15 s; 40004; gone ≤ 30 s; module unheld
    fn app_stop(&mut self) -> Result<AppExit, String>;         // post the tray command, observe
    fn tuning(&mut self, verb: &str) -> Result<String, String>;// the elevated task; "absent" when no module
    fn pref_check(&mut self) -> Result<(), String>;            // read; restore original if not (read back)
    fn data(&mut self, mode: Mode) -> Result<String, String>;  // recover, shadow report / import
    fn engine_start(&mut self, hold: bool) -> Result<u32, String>;
    fn engine_ready(&mut self, secs: u32) -> Result<Status, String>; // supervisor pipe; missed == 0 for secs
    fn engine_arm(&mut self) -> Result<(), String>;
    fn engine_stop(&mut self) -> Result<(), String>;           // Shutdown, DriverReleased ≤ 10 s, gone ≤ 5 s
    fn server_start(&mut self, mode: Mode) -> Result<u32, String>;
    fn server_stop(&mut self) -> Result<(), String>;           // ctrl-break, gone ≤ 10 s, ports free
    fn tray_start(&mut self) -> Result<(), String>;
    fn tray_stop(&mut self) -> Result<(), String>;             // Quit over the guard pipe
    fn identity(&mut self, sha: &str) -> Result<(), String>;   // LAN 80/443 + public host /api/version, tunnel ready
    fn runner_start(&mut self) -> Result<(), String>;
    fn runner_stop(&mut self) -> Result<(), String>;           // only when idle; ctrl-break
    fn reaper_start(&mut self) -> Result<(), String>;          // \iemmixer\iemmixer-StartREAPER; refuses with an engine or a module holder
    fn reaper_facts(&mut self) -> Result<ReaperFacts, String>; // ≤ 120 s wait for the track count; bridge once
    fn app_start(&mut self) -> Result<(), String>;             // \iemmixer\iemmixer-StartApp
    fn app_answers(&mut self) -> Result<(), String>;           // /api/version, /api/members count, public host
    fn notify(&mut self, title: &str, body: &str) -> Result<i32, String>;
}
```

  `FakePc` records calls and has scripted results. `daemon` tests use it.
- [ ] **Step 2: `WinPc`.** Settings come from `[guard]` and `[card]` of the site plus `$LOCALAPPDATA\iemmixer\guard\pc.toml` (paths).
  - **REAPER edge (`win/reaper.rs`):**
    - `ureq` against the configured control URL (`/_/40026`, `/_/40004`, `/_/NTRACK`, `/_/GET/EXTSTATE/<section>/<key>`, `/_/<action>`, `/_/TRACK` for peaks of the stage tracks);
    - the project file's mtime;
    - `iem_win::window::has_dialog(pid)`;
    - `iem_win::process::module_holders`.

    The parser for REAPER's tab lines is a portable function with tests, a port of `ConvertFrom-SpikeReaperLine`.
  - **App edge (`win/app.rs`):**
    1. `pids(app_image)` → exactly one, else error;
    2. `find_owned(tray_class, pid)` → hwnd;
    3. note the time, `post_command(hwnd, exit_id)`;
    4. `wait_gone(pid, 30 s)`;
    5. read the app's newest log file for `exit_log_line` with a timestamp ≥ the post (a portable parser, tested);
    6. `listening(80)`/`listening(443)` = None;
    7. no `*.tmp` newer than the post under the app data dir;
    8. record the exe's SHA-256 in the state.
  - **Card edge (`win/card.rs`):** `PrefStore` over `iem_win::registry` for `prefwin::leave` (restore) and read; holders.
  - **Tasks (`win/tasks.rs`):** `schtasks.exe /Run /TN <name>` (argv, no shell); `/Query /TN <name> /FO CSV /V` for status (a portable parser).
  - **Processes (`win/procs.rs`):**
    - `spawn_detached` for engine/server/tray/runner, with `CREATE_NEW_PROCESS_GROUP` for server and runner;
    - pid files and adoption (pid + image path + start time must match);
    - the supervisor pipe client (sync `interprocess`, hello `role: supervisor`).
- [ ] **Step 3:** `.cargo/mutants.toml` excludes `crates/iem-guard/src/win/**` (reason: Windows effects, not compiled on Linux; decisions are in `plan`/`crash`/`bundle`/`handover`/parsers). The portable parsers stay mutated. Commit: `feat(guard): PC effects behind the Pc trait (Windows) and a fake for tests`.

---

### Task 10: The guard daemon and `iemmode`

**Files:** `crates/iem-guard/src/daemon.rs`, `crates/iem-guard/src/bin/{iemmixer-guard,iemmode}.rs`, `crates/iem-guard/src/install.rs`.

- [ ] **Step 1: Daemon loop** (`iemmixer-guard run`):
  - Take the single-instance mutex `Local\iemmixer-guard`; if taken, exit 0 with "already running".
  - Load state; adopt children; if `switching` is set, re-plan to `Event` unless the recorded target was `Event` (then resume it).
  - Open the guard pipe: the same hardening as the engine (Task 6), name `iemmixer-guard`.
  - A `SessionEndWindow`: on session end, stop respawning, wait ≤ 10 s for the engine's own exit, and stop the server and tray.
  - Every 1 s:
    - watch the children and apply `crash::after_exit`;
    - watch for `reaper.exe` or the app appearing in `dev`/`live` (alarm once per appearance);
    - poll tuning `state` every 60 s (drift → alarm).
- [ ] **Step 2: Switch runner.**
  - One worker thread runs `plan(...)` step by step, persisting `switching.done` before each step. Each step is one `Pc` call.
  - Requests arriving meanwhile:
    - `Event` → sets `preempt`; the runner stops after the current step and runs `plan(current, Event, facts())`;
    - `Status` is answered;
    - `JobBegin`, `Install`, `Activate`, `TestSignal` and `Report` are refused with `switching`;
    - `Dev`/`Live` get `busy`.
  - Any step error → alarm with the step name, then the event plan (for a dev/live target). An error in the event plan → alarm (`G3`: fail loudly), continue with the remaining steps whose preconditions hold, and never start REAPER after a failed `PrefCheck` or `EngineStop`.
  - `--dry-run` prints the plan and runs read-only checks only (facts, preference read, bundle record, subscriptions).
- [ ] **Step 3: `install.rs`** (`iemmixer-guard install <zip>`; the same code behind `Request::Install`):
  - extract into `bundles\<sha>.partial` (`enclosed_name`), verify (`bundle::verify`), rename;
  - an existing `<sha>` with identical sums is a no-op; different sums → refuse (alarm);
  - `activate` copies `iemmixer-guard.exe` and `iemmode.exe` into `bin\`: rename the running file to `*.old-<sha>`, copy, and delete old copies at the next start;
  - the guard then hands over: it spawns the new `bin\iemmixer-guard.exe run` detached and exits 0 after releasing the mutex (the new one waits ≤ 10 s for it).
- [ ] **Step 4: `iemmode`.**
  - Connect to the guard pipe; if absent, `schtasks /Run /TN \iemmixer\iemmixer-guard` and retry for ≤ 15 s.
  - Subcommands:

    ```
    iemmode status | event [--dry-run] | dev [--build SHA] [--force] [--dry-run]
    iemmode live --build SHA [--trial] [--dry-run] | install <zip> | activate <sha>
    iemmode test-signal <input> <dbfs> <ttl> | report <sha> <green|red> <detail>
    iemmode job-begin <run> | job-end <run> | install-site <file> | force-reopen | runner-stop
    iemmode alarm-test | alarm-ack <id> | quit
    ```

  - Output: one JSON reply, and the alarms list on every call (spec §4.2). Exit 0 ok, 1 refused/failed, 2 usage, 4 guard unreachable.
- [ ] **Step 5: Tests** (`FakePc`, a temp state dir, the Unix socket variant of the guard pipe):
  - `event_preempts_a_running_dev_switch` (a blocked `EngineStart` fake step, then `Event` → after it the event plan runs);
  - `a_restarted_guard_unwinds_a_half_done_dev_switch`;
  - `jobs_are_refused_while_switching`;
  - `session_end_stops_respawning`;
  - `dry_run_changes_nothing` (the fake records no mutating call);
  - install happy path, tampered file, traversal entry, existing SHA with different sums.

  Commit: `feat(guard): daemon, switch runner with pre-emption, install/activate, iemmode CLI`.

---

### Task 11: The tray without a server (F27)

**Files:** `crates/iem-tray/src/{lib,tray}.rs`, `crates/iem-tray/Cargo.toml`.

- [ ] **Step 1:** remove the embedded server (runtime, `start_server`, config dir); the tray reads `lan_url`/`https_domain` from the site config for Open Mixer / Copy URL.
- [ ] **Step 2:** a background thread subscribes to the guard (`Request::Subscribe`): mode and alarms update the tooltip ("iemmixer — dev", "… 2 alarmy") and raise a notification for a new alarm. `Quit` from the guard → `app.exit(0)`. The menu Exit exits the tray only (F27).
- [ ] **Step 3:** the `windows` job builds and lints it (as today); the portable tooltip-text function is tested. Commit: `feat(tray): tray without a server — status and alarms from the guard (F27)`.

---

### Task 12: CI — windows job, bundle, attest, dispatch; integrity; rule

**Files:** `.github/workflows/ci.yml`, `scripts/check_integrity.py` (+test), `scripts/iem-pc/IemPc.psm1`, `scripts/iem-pc/Test-IemPc.ps1`, `.claude/rules/guard.md`, `CLAUDE.md`.

- [ ] **Step 1: `windows` job.**
  - Add `cargo clippy --locked -p iem-win -p iem-guard --all-targets -- -D warnings`.
  - Add `cargo test --locked -p iem-win -p iem-guard -p iem-audio-io` and `cargo test --locked -p iem-engine --test pipes`.
  - Run `powershell -NoProfile -File scripts/iem-pc/Test-IemPc.ps1` (Windows PowerShell 5.1).
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
        run: cargo build --locked --release -p iem-engine -p iem-server -p iem-guard -p iem-tray
      - name: Zip with manifest and sums
        shell: pwsh
        run: |
          $ErrorActionPreference = 'Stop'
          $b = Join-Path $env:RUNNER_TEMP 'bundle'; New-Item -ItemType Directory -Force -Path $b | Out-Null
          Copy-Item target/release/iem-engine.exe, target/release/iem-server.exe, target/release/iemmixer-guard.exe, target/release/iemmode.exe, target/release/iem-tray.exe -Destination $b
          Copy-Item crates/iem-engine/LICENSE -Destination (Join-Path $b 'LICENSE-iem-engine')
          if (Test-Path scripts/pc-tuning) { Copy-Item -Recurse scripts/pc-tuning (Join-Path $b 'tuning') }
          $version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
          $branch = if ($env:GITHUB_REF -like 'refs/heads/*') { $env:GITHUB_REF.Substring(11) } else { 'pr' }
          @{ sha = $env:GITHUB_SHA; branch = $branch; version = $version; run = [int64]$env:GITHUB_RUN_ID } | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $b 'manifest.json')
          $lines = Get-ChildItem -LiteralPath $b -File -Recurse | Sort-Object FullName | ForEach-Object { '{0}  {1}' -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.FullName.Substring($b.Length + 1).Replace('\','/') }
          [IO.File]::WriteAllText((Join-Path $b 'SHA256SUMS'), (($lines -join "`n") + "`n"))
          Compress-Archive -Path (Join-Path $b '*') -DestinationPath (Join-Path $env:RUNNER_TEMP "iemmixer-$env:GITHUB_SHA.zip")
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
- [ ] **Step 3: `attest` and `hil-dispatch`.** Resolve the pins first; the integrity scan requires full SHAs:

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

  hil-dispatch:
    name: hil-dispatch
    if: github.event_name == 'push'
    needs: [attest]
    runs-on: ubuntu-24.04
    timeout-minutes: 5
    permissions: {}
    steps:
      - name: Dispatch the ops HIL workflow (its only dispatchable workflow)
        env:
          GH_TOKEN: ${{ secrets.HIL_DISPATCH_TOKEN }}
          DIGEST: ${{ needs.attest.outputs.digest }}
        run: |
          set -euo pipefail
          gh workflow run hil.yml -R zbynekdrlik/iemmixer-ops -f sha="$GITHUB_SHA" -f branch="${GITHUB_REF#refs/heads/}" -f run="$GITHUB_RUN_ID" -f digest="$DIGEST"
```

  `HIL_DISPATCH_TOKEN` is a fine-grained token with `actions: write` on the ops repo only. The owner's account creates it; the agent stores it with `gh secret set` via `airuleset.py secret exec`, never in chat. The ops repo has one `workflow_dispatch` workflow, so the token can trigger nothing else (spec §5.2). The `asio-spike` job stays (S1c uses it).
- [ ] **Step 4: Integrity.**
  - `check_integrity.py` scans the new crate directories (already covered by `crates/`).
  - It now also fails on `CREATE_BREAKAWAY_FROM_JOB` outside `crates/iem-win/`.
  - Test: `test_force_kill_words_in_comments_are_refused` (already true — assert it for the new crates).
- [ ] **Step 5: `IemPc.psm1`** (bootstrap on the PC; runs elevated over ssh; every function idempotent with a read-back). Exports:
  - `Register-IemTasks` (guard, StartREAPER unchanged, StartApp, tuning (Highest), logon (Highest, at logon of the user): Interactive, no time limit `PT0S`, `IgnoreNew`, no idle/battery stop);
  - `Set-IemRootAcl` (protected DACL: user, SYSTEM, Administrators; inheritance on);
  - `Add-IemFirewallRule` (TCP 80,443 inbound, private/domain profiles, named `iemmixer-http`);
  - `Test-IemServiceRight` / `Grant-IemServiceRight -Service <name>` (adds `RPWPLO` for the user SID to the service SDDL; prints before/after);
  - `Add-IemDefenderExclusion -Path` (S1c G4; the root only);
  - `Register-IemRunner` (unzip the pinned runner, `config.cmd --unattended --url https://github.com/zbynekdrlik/iemmixer-ops --labels iem-pc --work _work --replace` with a one-time registration token passed via stdin, never on the command line);
  - `Get-IemBootstrapState`.

  `Test-IemPc.ps1` runs them against an HKCU test root, a test task folder `\iemmixer-test\`, a temp directory, and a disabled test firewall rule, then removes only its own test objects.
- [ ] **Step 6: Playbook rule `.claude/rules/guard.md`** (`paths:` `crates/iem-guard/**`, `crates/iem-win/**`, `scripts/iem-pc/**`, `crates/iem-audio-io/src/{asio,prefwin,channels,reset,rtpanic}.rs`):
  - the preference window;
  - the stop verbs;
  - the planner invariants;
  - the predecessor exit path and its verification;
  - that site values live in `[card]`/`[guard]` and the env;
  - the EVENT-NOW discipline for `iempc`.

  In the CLAUDE.md router add: "Guard, iemmode, PC install → `.claude/rules/guard.md`". The always-apply event line becomes: "'ide event' → `iempc event` (after S6 Task 14)".

  Commit: `ci(s6): bundle, attest, HIL dispatch; PC bootstrap module; guard playbook rule`.

---

### Task 13: Ops repo — site tables, `hil.yml`, runbook (private)

**Files (ops repo, `dev` branch, owner-merged PR to `main`):**
- `site/site.toml`: `[card]`, `[guard]`, `[activity] inputs`, server tables;
- `.github/workflows/hil.yml`;
- `docs/s6-pc-runbook.md`;
- `CLAUDE.md` (router, event section);
- `$PC_ENV`, `$PRIV/event-runbook.md`.

- [ ] **Step 1: `site.toml`.**
  - `[card]`: the real driver name, preference key/name, original `{kind = "dword", raw = "64"}` — from the S1a env.
  - `[guard]`:
    - the REAPER control URL, the project path, the expected track count, the stage track indices;
    - the meter-bridge state/heartbeat/action keys;
    - the predecessor image name, tray class, exit command id, log directory and exit line, data directory, member count, start task;
    - the public host.
  - `[activity] inputs`: the stage input ids (`mic1`…`mic10`, `hand1`…`hand3`, `eng_mic`).
  - The server tables the S5 hand-off asked for: `[[members]]`, `[[inputs]]` with categories, `back_to_reaper = ['<bin>\iemmode.exe', 'event']`. These are the S8 hand-off's items that S6 needs to run the server on the PC.
  - **The exit command id:** computed from the pinned tray code (`git -C "$PRED" show $PIN_PRED:iem-mixer/src-tauri/src/tray.rs` and the locked menu library's creation-order numbering). Record the derivation in the ops runbook only. It is confirmed at the first real stop (Task 17).
  - **Validation:** `tools/check_import.sh` as today, plus `iem-engine check-site` from the Task 15 bundle, run on the PC (`$P` over ssh). It is a CI binary, not a local build (Tier 0).
- [ ] **Step 2: `hil.yml`.**

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
      - id: app
        uses: actions/create-github-app-token@<sha> # <tag>
        with: { app-id: ${{ vars.OPS_APP_ID }}, private-key: ${{ secrets.OPS_APP_KEY }}, owner: zbynekdrlik, repositories: iemmixer }
      - id: h
        env: { GH_TOKEN: ${{ steps.app.outputs.token }} }
        run: |
          set -euo pipefail
          case "${{ inputs.branch }}" in dev|main) ;; *) echo "not a branch head"; exit 1;; esac
          head=$(gh api repos/zbynekdrlik/iemmixer/git/ref/heads/${{ inputs.branch }} --jq .object.sha)
          echo "head=$([ "$head" = "${{ inputs.sha }}" ] && echo yes || echo no)" >> "$GITHUB_OUTPUT"
          gh run download "${{ inputs.run }}" -R zbynekdrlik/iemmixer -n "iemmixer-bundle-${{ inputs.sha }}"
          test "sha256:$(sha256sum iemmixer-${{ inputs.sha }}.zip | cut -d' ' -f1)" = "${{ inputs.digest }}"
          gh attestation verify "iemmixer-${{ inputs.sha }}.zip" -R zbynekdrlik/iemmixer \
            --signer-workflow zbynekdrlik/iemmixer/.github/workflows/ci.yml --source-ref "refs/heads/${{ inputs.branch }}" --deny-self-hosted-runners
  pc:
    needs: verify
    if: needs.verify.outputs.head == 'yes'
    runs-on: [self-hosted, iem-pc]
    timeout-minutes: 30
    steps:
      - id: app
        uses: actions/create-github-app-token@<sha> # <tag>
        with: { app-id: ${{ vars.OPS_APP_ID }}, private-key: ${{ secrets.OPS_APP_KEY }}, owner: zbynekdrlik, repositories: iemmixer }
      - name: HIL v1
        shell: powershell
        env: { GH_TOKEN: ${{ steps.app.outputs.token }} }
        run: |
          $ErrorActionPreference = 'Stop'
          . "$env:LOCALAPPDATA\iemmixer\bin\hil-v1.ps1" -Sha '${{ inputs.sha }}' -Branch '${{ inputs.branch }}' -Run '${{ inputs.run }}' -Digest '${{ inputs.digest }}' -JobRun '${{ github.run_id }}'
```

  `hil-v1.ps1` ships in the bundle (`scripts/iem-pc/hil-v1.ps1`, public, no site values; it reads everything through `iemmode status`). It runs:
  1. `iemmode job-begin`;
  2. the artifact download (`gh` on the PC with the App token) and digest check;
  3. `iemmode install` and `activate` (the guard switches the bundle in dev);
  4. the checks of design §7 through `iemmode`/HTTP;
  5. the `hil/iem-pc` check run posted with `gh api repos/zbynekdrlik/iemmixer/check-runs -f name=hil/iem-pc -f head_sha=… -f conclusion=…`;
  6. `iemmode report`;
  7. `iemmode job-end`.

  Any refused `iemmode` (a switch to `event` started) ends the job as `cancelled`, never as success.
- [ ] **Step 3: `docs/s6-pc-runbook.md`.**
  - The env keys (`$PC_ENV`: `PC_SSH`, `PC_BIN`, `PC_ROOT`, the public host).
  - The bootstrap sequence (Task 16), "ide event" / "event skončil" with `iempc`, and the fallback (S1a `spike_window.py preempt`, then the event runbook's manual steps).
  - The alarm texts in Slovak, and the five approval-gated tests (not run).

  Update `$PRIV/event-runbook.md` and the ops `CLAUDE.md` event section to point at `iempc event` / `iempc dev` from Task 17 on.

---

### Task 14: The dev-box control tool `iempc.py`

**Files:** `scripts/iem-pc/iempc.py`, `scripts/iem-pc/test_iempc.py`.

- [ ] **Step 1: Commands.**
  - `status`, `event [--dry-run]`, `dev [--build SHA] [--dry-run]` — ssh `"$PC_BIN\iemmode.exe" …`, JSON out;
  - `fetch-bundle --sha` — the green `push` run's artifact, digest check, `gh attestation verify`;
  - `bootstrap <step>` — PowerShell `IemPc.psm1` functions over ssh;
  - `install --sha` — scp the verified zip, then `iemmode install`;
  - `dispatch-hil` — the newest `dev` and `main` heads, once per dev entry, recorded in `$STATE`;
  - `handover-s1a` — marks the S1a window closed when the card is free, no spike runs and the preference reads the original (read-only checks, then the S1a state file updated).
- [ ] **Step 2: EVENT-NOW discipline** (from `spike_window.py`'s `guarded()`):
  - every wait polls the flag every 2 s;
  - a read-only call is abandoned;
  - `dev`/`install` refuse when the flag exists;
  - with the flag present, `event` runs even if another command is running (it is the pre-emption).
- [ ] **Step 3: Tests** (fake ssh runner):
  - the flag refuses `dev` and lets `event` through;
  - parsing of the `iemmode` JSON, `fetch-bundle` digest mismatch refused;
  - `dispatch-hil` fires once per dev entry;
  - no site value in the module (reads `$PC_ENV`).

  Add to the CI `integrity` job: `python3 -m unittest discover -s scripts/iem-pc -p 'test_*.py' -v`. Commit: `feat(iem-pc): dev-box control with the event pre-emption`.

---

### Task 15: First push, CI green (main session)

- [ ] **Step 1: Pre-push, then push.**

```bash
cd "$WORK" && git fetch origin && git merge --ff-only origin/dev && git status -sb
cargo fmt --all -- --check && python3 scripts/check_integrity.py && python3 scripts/check_engine_deps.py && python3 scripts/check_version.py
python3 -m unittest discover -s scripts -p 'test_*.py' 2>&1 | tail -1
python3 -m unittest discover -s scripts/iem-pc -p 'test_*.py' 2>&1 | tail -1
cargo mutants --list --in-diff <(git diff origin/main...HEAD) | wc -l    # shard budget (ci-rust-toolchain rule)
git push origin dev
```

- [ ] **Step 2: Wait for every job** with one foreground bounded loop per Bash call. It includes `windows`, `bundle`, `attest`, `hil-dispatch`, `supply-chain`, `mutants-list`, `integrity`:

```bash
RUN=$(gh run list -R "$REPO" --branch dev --event push --limit 1 --json databaseId --jq '.[0].databaseId'); echo "$RUN"
for i in $(seq 1 53); do s=$(gh run view "$RUN" -R "$REPO" --json status,conclusion --jq '.status+" "+(.conclusion // "")'); echo "$(date +%T) $s"; case "$s" in completed*) break;; esac; sleep 10; done
gh run view "$RUN" -R "$REPO" --json jobs --jq '.jobs[] | .name+": "+(.conclusion // .status)'
```

  On failure: `gh run view "$RUN" -R "$REPO" --log-failed`, ONE fix commit, push, wait again. `hil-dispatch` fails until the token exists: create it before this push (Task 12 Step 3).
- [ ] **Step 3:** the ops `hil.yml` run for this SHA shows `verify` green and `pc` queued (no runner yet). Record the run ids on #9. Download the bundle on the dev box with `$P fetch-bundle --sha "$(git rev-parse HEAD)"`.

---

### Task 16: PC bootstrap (dev time only; main session)

**Precondition:** the owner's latest signal is "event skončil", and `EVENT-NOW` does not exist. On "ide event" at any step: write the flag, then the event runbook (the guard is not active yet) or `$P event` from Step 5 on. Each step's output (no site values) goes to #9.

- [ ] **Step 1: Read-only state.** `$P bootstrap Get-IemBootstrapState`. It shows:
  - REAPER running or not, the app running, the driver module holders, the preference;
  - whether our tasks exist, and the S1a window state (`spike_window.py status`).
- [ ] **Step 2: Elevated setup, idempotent, with read-back:** `Set-IemRootAcl`, `Register-IemTasks`, `Add-IemFirewallRule`, `Test-IemServiceRight` (→ `Grant-IemServiceRight` if missing), `Add-IemDefenderExclusion`.
- [ ] **Step 3: First bundle by hand** (spec §5.2):

```bash
$P fetch-bundle --sha "$SHA"     # includes gh attestation verify on the dev box
$P install --sha "$SHA"
$P status                        # mode event (or dev time with REAPER down), bundle recorded, pending HIL
```

- [ ] **Step 4: The UNVERIFIED items**, one at a time, read-only or self-contained:
  - `iemmode dev --dry-run` (plan printed; facts; preference read = original);
  - a Limited `schtasks /Run` of the tuning task with verb `state` (or "absent" before S1c);
  - breakaway: `iemmode status` reports `breakaway: ok` from a test spawn of `iemmode.exe --version`;
  - `Register-IemRunner` with a one-time token, then a Ctrl-Break stop of the idle runner through the guard (`iemmode runner-stop`, dev only) — the process exits within 10 s;
  - `iem-server alarm-link` → the link for the owner (sent with the first report in Task 18, not as a question).

  Findings go on #9.
- [ ] **Step 5:** from here on "ide event" means `$P event`.

---

### Task 17: First `iemmode dev`, HIL v1 green (dev time; main session)

- [ ] **Step 1: Take the card.**
  - If the S1a window is still open with the card free, run `$P handover-s1a` first.
  - Then run `$P dev --build "$SHA"`. It prints each step. With REAPER already down (the dev-time case) the plan is: interlock through `iem-engine interlock`, app stop, tuning enter, data, engine held, arm, server, tray, identity, runner.
  - **Expected:** app stop verdict ok — the log line, gone, ports free, no newer temp: record "predecessor exit path verified" on #9 (S1a acceptance item); engine `Status` frames 32, `missed` 0 after 10 s; the preference reads back 64 while the engine runs (the window closed); LAN and public host answer `/api/version` = SHA.
  - Any failure unwinds to event by itself: report it, fix, retry only in dev time.
- [ ] **Step 2: HIL.** `$P dispatch-hil` (or the queued run from Task 15) → the runner takes the job → wait for `hil/iem-pc` on the SHA:

```bash
for i in $(seq 1 53); do c=$(gh api repos/$REPO/commits/$SHA/check-runs --jq '.check_runs[] | select(.name=="hil/iem-pc") | .status+" "+(.conclusion // "")'); echo "$(date +%T) $c"; case "$c" in completed*) break;; esac; sleep 10; done
```

  Every HIL v1 check (design §7) green. Record its numbers on #9: callbacks, missed, resets, callback CPU p50/p99.9, fault callback duration, reopen gap, pipe DACL.
- [ ] **Step 3: F30 in HIL** (a synthetic `install-site` change and revert) green.

---

### Task 18: First `iemmode event` on the owner's signal; report; hand-offs (main session)

- [ ] **Step 1:** on the owner's next "ide event": write the flag, then run `$P event`.
  - **Expected:** jobs cancelled, runner stopped, engine released ≤ 10 s, server and tray stopped, tuning exit, preference = original, REAPER through our task, the handover checks (tracks, no dialog, bridge once, heartbeat, module held by REAPER, peaks or `UNCONFIRMED-AUDIO`), the app through our task answering with the member count, public host 200.
  - Confirm to the owner (✅, Slovak, one line).
  - On any failure: alarm (❓) with the failing check, then the fallback: the event runbook's manual steps (never force).
- [ ] **Step 2:** on the next "event skončil": `$P dev --build <latest green dev SHA>`, the full event → dev path including the REAPER save/quit and the interlock through REAPER's meters. **Acceptance box 1 is met only after both directions pass on real signals.**
- [ ] **Step 3: Report on #9** (Slovak, plain, numbers):
  - both switches with their checks and durations;
  - HIL v1;
  - the predecessor exit path;
  - the preference window read-backs;
  - the UNVERIFIED items resolved.

  Add `## 12. Results` to the design note (`docs(s6): results`). Tick the acceptance boxes only on evidence.
- [ ] **Step 4: Hand-offs** (comments):
  - **#10 (S7):** runner, `hil.yml`, `iemmode report`, the soak inputs;
  - **#11 (S8):** `live --build`, pins, the site tables, the cutover pieces;
  - **#15 (S1c):** the guard's tuning calls.
- [ ] **Step 5: The owner question — prepared, not asked now.** Write `$WP/s6-owner-tests-question.md` with the one `❓` block for the five tests in design §10 (one proposal, one decision: approve a ~45 min dev-time session with the owner at the PC for the hard kill; D5(b) loopback included). Post it only when the program reaches that point (before S7's long soak). Record on #9 that it is prepared. The alarm link goes to the owner as information with the report.

---

### Task 19: PR and merge (only when the run's orchestrator asks for it)

- [ ] **Step 1:** update the required checks: `bundle` now, and `hil/iem-pc` from the ops App once the runner has produced one green result on `main`'s candidate (S0 plan Task 15 Step 6; a skipped required job counts as passing, so `needs` chains stay required).
- [ ] **Step 2:** open the `dev` → `main` PR. Its body: summary, the two switches, HIL v1, checks, #9. Wait for every check including the mutation shards; kill survivors in the portable modules. Merge with `gh pr merge --merge` per `pr-merge-policy`, then bump `dev` first thing.

## Hand-off to later sub-projects

- **S7 (#10):** the runner and `hil.yml`, `iemmode report` for soak summaries, the switch timing (target ≤ 60 s silence), live Playwright against the PC, band-activity thresholds on real signal, S1c W6 (8 h at 32 with engine, server and stream).
- **S8 (#11):**
  - `live --build` (G8) and the trial crash loop to `event`;
  - pins and revert;
  - the server site tables in the ops repo;
  - cutover pieces not built here: guard task at logon, the predecessor's autostarts disabled with values exported, the tunnel repair switch;
  - the owner question on the five tests (prepared in Task 18).
- **S1c (#15):** the guard calls `enter`/`exit`/`state` through the elevated task; logon reconciliation; L5 through `iem-win`.
