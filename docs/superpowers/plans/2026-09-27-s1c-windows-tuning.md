# iemmixer S1c — Windows Tuning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) or superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Tasks 13–20 (pushes, CI waits, PC windows, the reboot, results, PR) run in the main session, never in a subagent.

**Goal:** Make iemmixer run stably at B = 32 / 96 kHz on the IEM PC (ticket #15, program #1).

- Measure first: DPC/ISR (xperf), callback telemetry, per-core interrupts, hwlat.
- Then tune Windows in tiers, each with an idempotent, reversible, read-back lever.
- Keep REAPER mode exactly as it is: the fingerprint, mode levers, and one owner-approved reboot.
- Prove it with 8 h runs.

**Architecture:** Four layers on top of S1a.

- **`iem-audio-io`:**
  - `telemetry.rs` gains a glitch log, callback-CPU counters and a gap scan (portable, tested);
  - a new portable `cpuset.rs`;
  - a new Windows-only `os.rs`: CPU Sets, power throttling, thread priority, QPC, trace markers;
  - the spike gains CPU Set flags, trace markers, an `hwlat` mode and 10 h runs.
- **PC:** `scripts/pc-tuning/IemTuning.psm1` (lever items, journal, tiers, mode enter/exit, fingerprint, inventory) and `IemMeasure.psm1` (xperf, counters, System log, WPT install), both shipped in the S1a bundle.
- **Dev box:**
  - `scripts/pc-tuning/tuning_window.py` drives the windows on top of `spike_window.py`, which gains unwind steps (`trace-stop`, `tuning-exit`, `fingerprint`) and a poll hook;
  - `latency_report.py` summarizes each step.
- **CI:** the `asio-spike` job runs the tuning self-test on Windows PowerShell 5.1 and bundles the modules; `integrity` runs the new Python tests.

**Tech Stack:**

- Rust 1.98.1 (edition 2024), with no new crates: `windows-sys` 0.61 gains the features `Win32_System_Threading`, `Win32_System_SystemInformation`, `Win32_System_Performance` and `Win32_System_Diagnostics_Etw`.
- Windows PowerShell 5.1 with inline C# (`Add-Type`) for powrprof, CPU Sets and the timer.
- Python 3.12 (stdlib).
- Windows Performance Toolkit `xperf`, from ADK 10.1.26100.9457, installed on the PC in dev time.
- GitHub Actions (hosted only).

**Spec:** `docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md` (this plan argues from it). Program spec I2, I3, I8, P5, P6, P10, R3, D2, G1–G7. S1a design note and plan (`docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md`, `docs/superpowers/plans/2026-09-27-s1a-asio-spike.md`).

**Detail sources (private, never committed):**

- `~/.config/iemmixer/asio-spike.env` (S1a window, extended in Task 12);
- `~/.config/iemmixer/pc-tuning.json` (the profile, Task 12);
- `~/devel/iemmixer-ops/docs/s1a-pc-runbook.md` and the new `docs/s1c-pc-runbook.md`;
- `~/.config/iemmixer/event-runbook.md`;
- the research inventory `~/.claude/work-products/iemmixer-gen2/05-fact-iem-pc.md`.

## Global Constraints

- **The PC only in dev time** (S1a rules, unchanged):
  - a window opens only after the owner's "event skončil" arrived after the last "ide event", and while `~/.config/iemmixer/EVENT-NOW` does not exist;
  - the session creates the flag the moment "ide event" arrives and removes it on "event skončil";
  - never infer an event, never ask whether one runs.
- **"ide event" during any S1c step:** the flag pre-empts within 2 s. `preempt` runs `stop-spike` → `trace-stop` → `tuning-exit` → `restore-buffer` → `bring-back` → `fingerprint`. A failed `tuning-exit` or `fingerprint` is an owner alarm, never a reason to hold REAPER back (design note §5.2).
- **REAPER stays exactly as it is:**
  - the fingerprint (design note §5.1) must equal the W1 baseline after every `to-event` and every reboot;
  - global changes are limited to the declared Tiers 2–3 (design note §6.3, §6.4);
  - Tier 4 items and anything else REAPER-sensitive need an owner decision (❓) first.
- **Reboots:**
  - only with the owner's explicit approval;
  - all reboot-bound levers in one reboot; the same question pre-approves one revert reboot;
  - graceful `shutdown.exe /r /t 60` only;
  - after a reboot the PC is in event mode and development continues only after the next "event skončil" (G1, D2).
- **I8, never force:**
  - services stop with `Stop-Service` without `-Force`; processes are never ended;
  - the words the integrity scan refuses never appear in code or comments.
- **Dante:** no Dante setting, no other Dante device. On the card's driver only the preferred buffer changes (S1a `set-buffer`).
- **G7:** the predecessor's app and CI runner may be placed on CPUs (L4), never stopped by the tuning.
- **Security:** nothing lowers security without an owner decision. The inventory never reads process command lines, service image paths or task actions (a token appeared on a command line before).
- **P5:**
  - only the `asio-spike-<sha>` artifact of a green `dev` push reaches the PC, including both new modules;
  - the WPT installer is Microsoft's bootstrapper, verified by its Authenticode signature on the PC.
- **P6:**
  - no site value in this repository: device instance paths, hardware ids of site devices, the adapter name, process, task and service names of site software, the REAPER-mode plan GUID, file paths, host, user;
  - they live in `pc-tuning.json`, the env file and the ops runbook;
  - raw measurements stay in `$RAW/pc-tuning/` (chmod 700); public reports carry numbers and generic names.
- **Tier 0:**
  - no local cargo compilation; locally only `cargo fmt`, `cargo metadata`, `cargo tree` and Python;
  - Rust and PowerShell are proven in hosted CI;
  - one push per cycle, one fix commit per failing cycle, foreground bounded waits (≤ 9 min per Bash call), never `run_in_background`.
- **Tests:**
  - every change ships tests that can fail; no `#[ignore]`, no skips, no `continue-on-error`;
  - the coverage floor never drops;
  - the mutation gate is diff-scoped: resize the shard matrix when `mutants-list` says so, never raise a timeout;
  - `os.rs` joins the mutation exclusions (Windows-only FFI, like `asio.rs`); `cpuset.rs` and `telemetry.rs` are mutated.
- **Branches and identity:**
  - `dev` only until Task 20;
  - noreply identity; every commit carries `Refs #15` and ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`;
  - the PR body ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- **Durable state:** decisions and findings go on #15 the moment they land (numbers only, Slovak for the owner). The window state lives in `~/.local/state/iemmixer/spike-window.json`. Raw data lives in `$RAW/pc-tuning/`.

## Review Focus

1. **"ide event" in the middle of a measurement.**
   - Expected: the kernel trace is stopped and the mode levers are reverted *before* REAPER starts, even when the spike run itself raised `EventNow`. The flags `trace` and `tuning_mode` are recorded before their actions.
   - Tests: `test_spike_window.py::UndoPlanTests::test_trace_and_tuning_unwind_before_the_buffer_and_reaper`, `test_flags_recorded_before_the_action`.
2. **A crash or reboot while iemmixer holds the card.**
   - Expected: the journal on the PC says `entered` with every before-value, so `Exit-IemTuningMode` in a new session restores from the journal alone. A placed process that has exited, or whose pid was reused, is skipped, never re-placed.
   - Tests: `Test-IemTuning.ps1` cases `exit-from-journal-in-a-new-session`, `exit-skips-a-process-that-ended`, `cpusets-refuse-a-reused-pid`.
3. **Apply twice, then undo.**
   - Expected: the second apply writes nothing (`kept`). Undo restores the original, including values that were absent (they are deleted again) and services that were running (started again).
   - Tests: `Test-IemTuning.ps1` cases `tier2-apply-is-idempotent`, `tier2-undo-restores-the-original`, `tier3-undo-deletes-absent-values`.
4. **A stale profile pointing at the wrong device.**
   - Expected: the Tier 3 apply refuses before any write when the instance is missing or its hardware id does not match.
   - Tests: `Test-IemTuning.ps1` case `tier3-refuses-a-mismatched-device`.
5. **Glitch bookkeeping under a burst.**
   - Expected: the ring keeps exactly its capacity and counts the rest as dropped. The spike's report keeps at most 10 000 per segment and counts the rest. Kinds and values survive the packing.
   - Tests: `telemetry::tests::glitch_log_keeps_its_capacity_and_counts_drops`, `glitch_kinds_and_large_values_round_trip`, `asio_spike` `glitch_list_is_capped`.

## Shell variables (paste at the start of every task)

```bash
export WORK="$HOME/devel/iemmixer"
export PRIV="$HOME/.config/iemmixer"
export OPS="$HOME/devel/iemmixer-ops"
export RAW="$HOME/.local/share/iemmixer/golden-raw"
export WP="$HOME/.claude/work-products/iemmixer-gen2"
export SPIKE_ENV="$PRIV/asio-spike.env"
export TUNING_PROFILE="$PRIV/pc-tuning.json"
export REPO=zbynekdrlik/iemmixer
export S="python3 $WORK/scripts/asio-spike/spike_window.py"
export T="python3 $WORK/scripts/pc-tuning/tuning_window.py"
```

## File Structure

```
crates/iem-audio-io/Cargo.toml                 windows-sys features + Threading, SystemInformation, Performance, Diagnostics_Etw
crates/iem-audio-io/src/lib.rs                 pub mod cpuset; os (Windows, allow unsafe)
crates/iem-audio-io/src/telemetry.rs           GlitchLog, Glitch/GlitchKind, callback CPUs + thread, GapScan
crates/iem-audio-io/src/cpuset.rs              parse_lps, ids_for (portable, tested)
crates/iem-audio-io/src/os.rs                  CPU Sets, power throttling, TIME_CRITICAL, QPC, Markers (Windows FFI)
crates/iem-audio-io/src/asio.rs                on_thread per callback; qpc_base; drain_glitches
crates/iem-audio-io/examples/asio_spike.rs     hwlat, --audio-cpus/--stress-cpus, markers, glitch lists, 10 h
.cargo/mutants.toml                            exclude os.rs
scripts/asio-spike/SpikePc.psm1 (+Test)        hwlat and CPU Set arguments
scripts/asio-spike/spike_window.py (+test)     10 h limits, hwlat requests, trace-stop/tuning-exit/fingerprint unwind, poll hook
scripts/pc-tuning/IemTuning.psm1               items, journal, tiers, mode, fingerprint, inventory
scripts/pc-tuning/IemMeasure.psm1              xperf, dpcisr/dumper, CPU samples, System log, WPT install
scripts/pc-tuning/Test-IemTuning.ps1           self-test on real backends (CI asio-spike)
scripts/pc-tuning/latency_report.py (+test)    summaries: dpcisr, CPU rates, glitches, hwlat, near-glitch
scripts/pc-tuning/tuning_window.py (+test)     S1c window commands
.github/workflows/ci.yml                       integrity: pc-tuning tests; asio-spike: tuning self-test, bundle
.claude/rules/pc-tuning.md, asio-spike.md      playbook
CLAUDE.md                                      router line
private: $PRIV/pc-tuning.json, $SPIKE_ENV (+3 keys), $OPS/docs/s1c-pc-runbook.md, $PRIV/event-runbook.md
```

---

### Task 1: Start — sync, version, the design on the ticket

**Files:** none new (the design note and this plan are committed on `dev`).

- [ ] **Step 1: Sync and check the version.**

```bash
cd "$WORK" && git fetch origin && git status -sb && git log --oneline -3
python3 scripts/check_version.py
git show origin/main:Cargo.toml | grep -m1 '^version'; grep -m1 '^version' Cargo.toml
```

Expected: a clean `dev` tracking `origin/dev`, and `dev` above `main`. If `main` has caught up (S1a's PR merged), bump `[workspace.package].version` to the next `2.0.0-dev.N` first, as its own commit `chore: bump version to 2.0.0-dev.N` (`Refs #15`).

- [x] **Step 2: Design summary on #15** (posted by the planning session, 2026-09-27): what is measured first, the tiers, the reboot-bound levers, the open questions.

- [ ] **Step 3: S1a precondition.** `gh issue view 3 -R "$REPO" --comments | tail -40`.
  - If S1a Task 12 (the first PC window) has not run, W1 (Task 14) runs it first in the same window: S1a Task 12 Steps 1–6, then Task 14 from Step 3.
  - The S1c code tasks (2–13) do not wait for it.

---

### Task 2: `telemetry.rs` — glitch log, callback CPUs and thread, gap scan

**Files:**
- Modify: `crates/iem-audio-io/src/telemetry.rs`

**Interfaces:**
- Produces:
  - `pub enum GlitchKind { Late, Missed, Overrun, PositionGap }` with `pub fn name(self) -> &'static str`;
  - `pub struct Glitch { pub kind: GlitchKind, pub at_ns: u64, pub value: u64 }`;
  - `pub const GLITCH_CAPACITY: usize`, and `pub struct GlitchLog` with `new(capacity)`, `push(&self, Glitch)`, `drain(&self, &mut Vec<Glitch>)`, `dropped(&self) -> u64`;
  - `Telemetry::on_thread(&self, cpu: u32, thread_id: u32)` and `Telemetry::drain_glitches(&self, out: &mut Vec<Glitch>)`;
  - new `Snapshot` fields `callback_cpus: Vec<(u32, u64)>`, `cpu_other: u64`, `callback_thread: u32`, `thread_switches: u64`, `glitches_dropped: u64`;
  - `pub const LARGEST: usize = 32`, `pub struct GapScan` with `new(threshold_ns: u64)`, `observe(&mut self, prev_ns: u64, now_ns: u64)`, `summary(&self) -> GapSummary`;
  - `pub struct GapSummary { pub reads: u64, pub over: u64, pub gaps: HistogramSnapshot, pub largest: Vec<(u64, u64)> }`.

- [ ] **Step 1: The glitch types and ring.** Add after `pub fn reply(...)` (before `pub struct Histogram`):

```rust
/// What went wrong at one callback (S1c design note §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlitchKind {
    /// Interval above 1.5 periods.
    Late,
    /// Interval of at least 2 periods.
    Missed,
    /// Callback longer than one period.
    Overrun,
    /// The driver's sample position did not advance by one buffer.
    PositionGap,
}

impl GlitchKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Late => "late",
            Self::Missed => "missed",
            Self::Overrun => "overrun",
            Self::PositionGap => "position-gap",
        }
    }

    fn code(self) -> u64 {
        match self {
            Self::Late => 0,
            Self::Missed => 1,
            Self::Overrun => 2,
            Self::PositionGap => 3,
        }
    }

    fn from_code(code: u64) -> Self {
        match code {
            1 => Self::Missed,
            2 => Self::Overrun,
            3 => Self::PositionGap,
            _ => Self::Late,
        }
    }
}

/// One glitch: the callback's entry on the stream clock (ns) and the interval
/// (late, missed), the callback's duration (overrun) or the position step in
/// frames (position gap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glitch {
    pub kind: GlitchKind,
    pub at_ns: u64,
    pub value: u64,
}

/// Glitches kept between two drains; more are counted as dropped.
pub const GLITCH_CAPACITY: usize = 4_096;
const VALUE_BITS: u32 = 62;
const VALUE_MASK: u64 = (1 << VALUE_BITS) - 1;

/// Single-producer (the callback) single-consumer (the owner thread) ring of
/// glitches: preallocated, lock-free, never blocking the producer.
pub struct GlitchLog {
    at: Box<[AtomicU64]>,
    packed: Box<[AtomicU64]>,
    head: AtomicU64,
    tail: AtomicU64,
    dropped: AtomicU64,
}

impl GlitchLog {
    pub fn new(capacity: usize) -> Self {
        let slots = || (0..capacity.max(1)).map(|_| AtomicU64::new(0)).collect();
        Self {
            at: slots(),
            packed: slots(),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    fn slot(&self, n: u64) -> usize {
        usize::try_from(n % self.at.len() as u64).unwrap_or(0)
    }

    /// Callback thread only.
    pub fn push(&self, g: Glitch) {
        let head = self.head.load(Relaxed);
        if head.wrapping_sub(self.tail.load(Acquire)) >= self.at.len() as u64 {
            self.dropped.fetch_add(1, Relaxed);
            return;
        }
        let i = self.slot(head);
        if let (Some(at), Some(p)) = (self.at.get(i), self.packed.get(i)) {
            at.store(g.at_ns, Relaxed);
            p.store((g.kind.code() << VALUE_BITS) | g.value.min(VALUE_MASK), Relaxed);
        }
        self.head.store(head.wrapping_add(1), Release);
    }

    /// Owner thread only: moves every glitch since the last drain into `out`,
    /// oldest first.
    pub fn drain(&self, out: &mut Vec<Glitch>) {
        let tail = self.tail.load(Relaxed);
        let head = self.head.load(Acquire);
        let mut n = tail;
        while n != head {
            let i = self.slot(n);
            if let (Some(at), Some(p)) = (self.at.get(i), self.packed.get(i)) {
                let packed = p.load(Relaxed);
                out.push(Glitch {
                    kind: GlitchKind::from_code(packed >> VALUE_BITS),
                    at_ns: at.load(Relaxed),
                    value: packed & VALUE_MASK,
                });
            }
            n = n.wrapping_add(1);
        }
        self.tail.store(head, Release);
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Relaxed)
    }
}

/// Logical processors counted per callback; a higher index goes to `cpu_other`.
pub const CPU_SLOTS: usize = 64;
```

- [ ] **Step 2: Wire it into `Telemetry` and `Snapshot`.**

1. **`Snapshot`:** add after `pub drift_ppm: Option<f64>,`:

```rust
    /// (logical processor, callbacks it ran), in processor order.
    pub callback_cpus: Vec<(u32, u64)>,
    pub cpu_other: u64,
    /// The first callback's thread id (0 = none yet).
    pub callback_thread: u32,
    /// Callbacks on a thread other than the first one.
    pub thread_switches: u64,
    pub glitches_dropped: u64,
```

2. **The `Telemetry` struct:** add after `input_peak: AtomicU64,`:

```rust
    glitches: GlitchLog,
    cpus: Box<[AtomicU64]>,
    cpu_other: AtomicU64,
    thread: AtomicU64,
    thread_switches: AtomicU64,
```

3. **`Telemetry::new`:** add after `input_peak: AtomicU64::new(0),`:

```rust
            glitches: GlitchLog::new(GLITCH_CAPACITY),
            cpus: (0..CPU_SLOTS).map(|_| AtomicU64::new(0)).collect(),
            cpu_other: AtomicU64::new(0),
            thread: AtomicU64::new(0),
            thread_switches: AtomicU64::new(0),
```

4. **`on_callback` and `on_done`:** replace both with:

```rust
    /// At the entry of callback: `entry_ns` on the host clock (> 0), the
    /// driver's sample position when it reported one. Positions count only
    /// after the warm-up (the drift is anchored there, not on the priming
    /// burst); a callback without one leaves nothing to compare the next
    /// position with, so no gap is judged across it. Every judged glitch also
    /// enters the glitch log.
    pub fn on_callback(&self, entry_ns: u64, position: Option<i64>) {
        let n = self.callbacks.fetch_add(1, Relaxed);
        let prev = self.last_ns.swap(entry_ns, Relaxed);
        if n == 0 {
            self.first_ns.store(entry_ns, Relaxed);
        } else if n >= WARMUP {
            let dt = entry_ns.saturating_sub(prev);
            self.interval.record(dt);
            let kind = match classify(dt, self.period_ns) {
                Gap::Missed => {
                    self.missed.fetch_add(1, Relaxed);
                    Some(GlitchKind::Missed)
                }
                Gap::Late => {
                    self.late.fetch_add(1, Relaxed);
                    Some(GlitchKind::Late)
                }
                Gap::OnTime => None,
            };
            if let Some(kind) = kind {
                self.glitches.push(Glitch {
                    kind,
                    at_ns: entry_ns,
                    value: dt,
                });
            }
        }
        let before = self.prev_pos.swap(position.unwrap_or(NO_POSITION), Relaxed);
        if n >= WARMUP
            && let Some(pos) = position
        {
            let step = pos.wrapping_sub(before);
            if before != NO_POSITION && step != self.frames {
                self.position_gaps.fetch_add(1, Relaxed);
                self.glitches.push(Glitch {
                    kind: GlitchKind::PositionGap,
                    at_ns: entry_ns,
                    value: step.unsigned_abs(),
                });
            }
            // The end first: a reader that sees the anchor also sees an end.
            self.last_pos_ns.store(entry_ns, Relaxed);
            self.last_pos.store(pos, Relaxed);
            if self.first_pos.load(Relaxed) == NO_POSITION {
                self.first_pos_ns.store(entry_ns, Relaxed);
                self.first_pos.store(pos, Release);
            }
        }
    }

    /// At the exit of a callback: how long it took.
    pub fn on_done(&self, duration_ns: u64) {
        self.duration.record(duration_ns);
        if duration_ns > self.period_ns {
            self.overruns.fetch_add(1, Relaxed);
            self.glitches.push(Glitch {
                kind: GlitchKind::Overrun,
                at_ns: self.last_ns.load(Relaxed),
                value: duration_ns,
            });
        }
    }

    /// At the entry of a callback, from the host: the logical processor it
    /// runs on and its thread id (both read without a system call).
    pub fn on_thread(&self, cpu: u32, thread_id: u32) {
        match usize::try_from(cpu).ok().and_then(|i| self.cpus.get(i)) {
            Some(c) => c.fetch_add(1, Relaxed),
            None => self.cpu_other.fetch_add(1, Relaxed),
        };
        let id = u64::from(thread_id);
        if let Err(first) = self.thread.compare_exchange(0, id, Relaxed, Relaxed)
            && first != id
        {
            self.thread_switches.fetch_add(1, Relaxed);
        }
    }

    /// Owner thread only: the glitches since the last call, oldest first.
    pub fn drain_glitches(&self, out: &mut Vec<Glitch>) {
        self.glitches.drain(out);
    }
```

5. **`snapshot()`:** add after `drift_ppm: drift,`:

```rust
            callback_cpus: self
                .cpus
                .iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    let n = c.load(Relaxed);
                    (n > 0).then(|| (u32::try_from(i).unwrap_or(u32::MAX), n))
                })
                .collect(),
            cpu_other: self.cpu_other.load(Relaxed),
            callback_thread: u32::try_from(self.thread.load(Relaxed)).unwrap_or(u32::MAX),
            thread_switches: self.thread_switches.load(Relaxed),
            glitches_dropped: self.glitches.dropped(),
```

- [ ] **Step 3: The gap scan.** Add after `impl ActivityGuard { ... }` (before `#[cfg(test)]`):

```rust
/// The largest gaps a scan keeps with their times.
pub const LARGEST: usize = 32;

/// The hwlat scan (S1c design note §4.1): a thread that reads the clock in a
/// tight loop sees every stall of its CPU (interrupt, DPC, a higher-priority
/// thread, firmware) as a gap between two reads. Keeps a histogram of the
/// gaps at or above the threshold and the largest ones with their times; no
/// allocation after `new`.
pub struct GapScan {
    threshold_ns: u64,
    gaps: Histogram,
    reads: u64,
    over: u64,
    largest: Vec<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapSummary {
    pub reads: u64,
    pub over: u64,
    pub gaps: HistogramSnapshot,
    /// (time of the read before the gap, gap), largest first.
    pub largest: Vec<(u64, u64)>,
}

impl GapScan {
    pub fn new(threshold_ns: u64) -> Self {
        Self {
            threshold_ns,
            gaps: Histogram::default(),
            reads: 0,
            over: 0,
            largest: Vec::with_capacity(LARGEST),
        }
    }

    /// Two consecutive clock reads, in ns since the scan's start.
    pub fn observe(&mut self, prev_ns: u64, now_ns: u64) {
        self.reads += 1;
        let gap = now_ns.saturating_sub(prev_ns);
        if gap < self.threshold_ns {
            return;
        }
        self.over += 1;
        self.gaps.record(gap);
        if self.largest.len() < LARGEST {
            self.largest.push((prev_ns, gap));
        } else if let Some(last) = self.largest.last_mut()
            && gap > last.1
        {
            *last = (prev_ns, gap);
        } else {
            return;
        }
        self.largest.sort_unstable_by(|a, b| b.1.cmp(&a.1));
    }

    pub fn summary(&self) -> GapSummary {
        GapSummary {
            reads: self.reads,
            over: self.over,
            gaps: self.gaps.snapshot(),
            largest: self.largest.clone(),
        }
    }
}
```

- [ ] **Step 4: Tests.** Append inside `mod tests`:

```rust
    fn g(kind: GlitchKind, at_ns: u64, value: u64) -> Glitch {
        Glitch { kind, at_ns, value }
    }

    #[test]
    fn glitch_log_keeps_its_capacity_and_counts_drops() {
        let log = GlitchLog::new(3);
        for i in 0..4 {
            log.push(g(GlitchKind::Late, i, 10 + i));
        }
        let mut out = Vec::new();
        log.drain(&mut out);
        assert_eq!(out.iter().map(|x| x.at_ns).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(log.dropped(), 1);
        // After a drain the ring takes three more and wraps around its slots.
        for i in 10..13 {
            log.push(g(GlitchKind::Missed, i, i));
        }
        out.clear();
        log.drain(&mut out);
        assert_eq!(out.iter().map(|x| x.at_ns).collect::<Vec<_>>(), [10, 11, 12]);
        out.clear();
        log.drain(&mut out);
        assert!(out.is_empty());
        assert_eq!(log.dropped(), 1);
    }

    #[test]
    fn glitch_kinds_and_large_values_round_trip() {
        let log = GlitchLog::new(8);
        let all = [
            g(GlitchKind::Late, 1, 500_001),
            g(GlitchKind::Missed, 2, 700_000),
            g(GlitchKind::Overrun, 3, 400_000),
            g(GlitchKind::PositionGap, 4, 64),
        ];
        for x in all {
            log.push(x);
        }
        log.push(g(GlitchKind::Missed, 5, u64::MAX));
        let mut out = Vec::new();
        log.drain(&mut out);
        assert_eq!(&out[..4], &all);
        assert_eq!(out[4], g(GlitchKind::Missed, 5, (1 << 62) - 1));
        assert_eq!(
            [GlitchKind::Late, GlitchKind::Missed, GlitchKind::Overrun, GlitchKind::PositionGap].map(GlitchKind::name),
            ["late", "missed", "overrun", "position-gap"]
        );
    }

    #[test]
    fn judged_glitches_enter_the_log_with_their_times() {
        let t = Telemetry::new(32, 96_000.0);
        let mut at = 1_000;
        let mut pos = 0;
        for _ in 0..WARMUP {
            t.on_callback(at, Some(pos));
            at += P;
            pos += 32;
        }
        let prev = at - P;
        let late_at = prev + P * 3 / 2 + 1;
        t.on_callback(late_at, Some(pos));
        let missed_at = late_at + 2 * P;
        t.on_callback(missed_at, Some(pos + 32 + 64));
        t.on_done(P + 1);
        t.on_done(P);
        let mut out = Vec::new();
        t.drain_glitches(&mut out);
        assert_eq!(
            out,
            [
                g(GlitchKind::Late, late_at, P * 3 / 2 + 1),
                g(GlitchKind::Missed, missed_at, 2 * P),
                g(GlitchKind::PositionGap, missed_at, 96),
                g(GlitchKind::Overrun, missed_at, P + 1),
            ]
        );
        assert_eq!(t.snapshot().glitches_dropped, 0);
    }

    #[test]
    fn warmup_callbacks_leave_no_glitch() {
        let t = Telemetry::new(32, 96_000.0);
        for i in 0..WARMUP {
            t.on_callback(1_000 + i * 10 * P, Some(i as i64 * 7));
        }
        let mut out = Vec::new();
        t.drain_glitches(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn callback_cpus_and_thread_switches_are_counted() {
        let t = Telemetry::new(32, 96_000.0);
        t.on_thread(14, 900);
        t.on_thread(14, 900);
        t.on_thread(3, 900);
        t.on_thread(63, 900);
        t.on_thread(64, 901);
        let s = t.snapshot();
        assert_eq!(s.callback_cpus, [(3, 1), (14, 2), (63, 1)]);
        assert_eq!((s.cpu_other, s.callback_thread, s.thread_switches), (1, 900, 1));
        let fresh = Telemetry::new(32, 96_000.0).snapshot();
        assert_eq!((fresh.callback_cpus.len(), fresh.callback_thread, fresh.thread_switches), (0, 0, 0));
    }

    #[test]
    fn gap_scan_counts_gaps_at_the_threshold_and_keeps_the_largest() {
        let mut s = GapScan::new(10_000);
        s.observe(0, 9_999);
        s.observe(9_999, 19_999);
        assert_eq!((s.summary().reads, s.summary().over), (2, 1));
        for i in 0..40_u64 {
            s.observe(1_000_000 * i, 1_000_000 * i + 20_000 + i);
        }
        let sum = s.summary();
        assert_eq!((sum.reads, sum.over, sum.gaps.total()), (42, 41, 41));
        assert_eq!(sum.largest.len(), LARGEST);
        assert_eq!(sum.largest[0], (39_000_000, 20_039));
        assert_eq!(sum.largest[LARGEST - 1], (8_000_000, 20_008));
        assert!(sum.largest.windows(2).all(|w| w[0].1 >= w[1].1));
        // A gap smaller than, or equal to, the smallest kept one changes nothing.
        s.observe(0, 10_001);
        s.observe(5, 5 + 20_008);
        assert_eq!(s.summary().largest, sum.largest);
    }
```

- [ ] **Step 5: Format and commit.**

```bash
cd "$WORK" && cargo fmt --all -- --check
git add crates/iem-audio-io/src/telemetry.rs
git commit -m "feat(s1c): glitch log, callback CPUs and thread, gap scan in telemetry

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

CI proves the tests (Task 13).

---

### Task 3: `cpuset.rs` and the Windows `os.rs`

**Files:**
- Create: `crates/iem-audio-io/src/cpuset.rs`, `crates/iem-audio-io/src/os.rs`
- Modify: `crates/iem-audio-io/src/lib.rs`, `crates/iem-audio-io/Cargo.toml`, `.cargo/mutants.toml`, `.claude/rules/asio-spike.md`

**Interfaces:**
- Produces:
  - `cpuset::CpuSet { id: u32, group: u16, lp: u8, core: u8, realtime: bool }`;
  - `cpuset::parse_lps(&str) -> Result<Vec<u8>, String>` and `cpuset::ids_for(&[u8], &[CpuSet]) -> Result<Vec<u32>, String>`;
  - `os::MARKER_PROVIDER`, `os::system_cpu_sets() -> io::Result<Vec<CpuSet>>`;
  - `os::set_process_cpus(&[u8]) -> io::Result<Vec<u32>>` and `os::set_thread_cpus(&[u8]) -> io::Result<Vec<u32>>`;
  - `os::disable_power_throttling() -> io::Result<()>` and `os::set_thread_time_critical() -> io::Result<()>`;
  - `os::current_processor() -> u32`, `os::current_thread_id() -> u32`, `os::qpc() -> io::Result<(i64, i64)>`;
  - `os::Markers::register() -> io::Result<Markers>` and `Markers::write(&self, &str)`.

- [ ] **Step 1: `cpuset.rs` with its tests.** Create:

```rust
//! CPU placement (S1c design note §6.1, §6.2 L5): logical-processor lists as
//! the tuning profile and the spike's flags write them ("14", "6-13",
//! "0,1,6-13") and their mapping to Windows CPU Set IDs. Portable and tested;
//! the Windows calls live in `os`.

/// One entry of the system's CPU Set information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSet {
    /// The ID the CPU Set functions take.
    pub id: u32,
    pub group: u16,
    /// The logical processor's index within its group.
    pub lp: u8,
    pub core: u8,
    /// Reserved for real-time work (`ReservedCpuSets`, design note §6.5 X1).
    pub realtime: bool,
}

/// Parses a list of group-0 logical processors: comma-separated numbers and
/// `a-b` ranges, each processor at most once, 0..=63. Empty text is an empty
/// list. The result is ascending.
pub fn parse_lps(text: &str) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (lo, hi) = part.split_once('-').unwrap_or((part, part));
        let num = |s: &str| {
            s.trim()
                .parse::<u8>()
                .ok()
                .filter(|&n| n < 64)
                .ok_or_else(|| format!("{s:?} in {text:?} is not a processor 0..63"))
        };
        let (lo, hi) = (num(lo)?, num(hi)?);
        if lo > hi {
            return Err(format!("range {part:?} in {text:?} runs backwards"));
        }
        for lp in lo..=hi {
            if out.contains(&lp) {
                return Err(format!("{text:?} names processor {lp} twice"));
            }
            out.push(lp);
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// The CPU Set IDs of `lps` in group 0; every processor must exist.
pub fn ids_for(lps: &[u8], system: &[CpuSet]) -> Result<Vec<u32>, String> {
    lps.iter()
        .map(|&lp| {
            system
                .iter()
                .find(|c| c.group == 0 && c.lp == lp)
                .map(|c| c.id)
                .ok_or_else(|| format!("logical processor {lp} is not in group 0 of this machine"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system() -> Vec<CpuSet> {
        (0..16_u8)
            .map(|lp| CpuSet { id: 0x100 + u32::from(lp), group: 0, lp, core: lp / 2, realtime: false })
            .chain([CpuSet { id: 0x200, group: 1, lp: 0, core: 0, realtime: false }])
            .collect()
    }

    #[test]
    fn lists_and_ranges_parse_sorted() {
        assert_eq!(parse_lps("14"), Ok(vec![14]));
        assert_eq!(parse_lps(" 6-9 , 0,1 "), Ok(vec![0, 1, 6, 7, 8, 9]));
        assert_eq!(parse_lps("3-3"), Ok(vec![3]));
        assert_eq!(parse_lps("63"), Ok(vec![63]));
        assert_eq!(parse_lps(""), Ok(vec![]));
        assert_eq!(parse_lps(" , "), Ok(vec![]));
    }

    #[test]
    fn bad_lists_are_refused() {
        for bad in ["64", "-1", "a", "5-3", "1,1", "0-2,2", "1-", "1-2-3", "256"] {
            assert!(parse_lps(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ids_come_from_group_zero_and_must_exist() {
        let s = system();
        assert_eq!(ids_for(&[0, 14], &s), Ok(vec![0x100, 0x10e]));
        assert_eq!(ids_for(&[], &s), Ok(vec![]));
        assert!(ids_for(&[16], &s).is_err());
        let only_group_one = [CpuSet { id: 0x200, group: 1, lp: 0, core: 0, realtime: false }];
        assert!(ids_for(&[0], &only_group_one).is_err());
    }
}
```

- [ ] **Step 2: `os.rs`.** Create:

```rust
//! Windows process placement, priority, clock and trace markers (S1c design
//! note §4.1, §6.2 L5) for the spike and, in S6, the engine. The driver's
//! callback thread is never re-prioritised: it inherits the process's default
//! CPU Set like every other thread of the process, nothing else.

use core::ffi::c_void;
use core::ptr;
use std::io;

use windows_sys::Win32::System::Diagnostics::Etw::{
    EventRegister, EventUnregister, EventWriteString, REGHANDLE,
};
use windows_sys::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows_sys::Win32::System::SystemInformation::{
    GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION, SYSTEM_CPU_SET_INFORMATION_REALTIME,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessorNumber, GetCurrentThread, GetCurrentThreadId,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
    ProcessPowerThrottling, SetProcessDefaultCpuSets, SetProcessInformation, SetThreadPriority,
    SetThreadSelectedCpuSets, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows_sys::core::{BOOL, GUID};

use crate::cpuset::{self, CpuSet};

/// The trace-marker provider; `scripts/pc-tuning/IemMeasure.psm1` enables it
/// by this GUID.
pub const MARKER_PROVIDER: GUID = GUID::from_u128(0x3b6c_1e0a_5d2f_4c8e_9a71_0e4f_2d9b_8c11);
/// ETW level "information".
const LEVEL_INFO: u8 = 4;

fn check(ok: BOOL, what: &str) -> io::Result<()> {
    if ok == 0 {
        let e = io::Error::last_os_error();
        return Err(io::Error::new(e.kind(), format!("{what}: {e}")));
    }
    Ok(())
}

fn len32(ids: &[u32]) -> u32 {
    u32::try_from(ids.len()).unwrap_or(u32::MAX)
}

fn list(ids: &[u32]) -> *const u32 {
    if ids.is_empty() { ptr::null() } else { ids.as_ptr() }
}

/// The system's CPU Sets (every group).
pub fn system_cpu_sets() -> io::Result<Vec<CpuSet>> {
    let mut len = 0_u32;
    // SAFETY: a null buffer of length 0 only asks for the needed length.
    unsafe { GetSystemCpuSetInformation(ptr::null_mut(), 0, &mut len, ptr::null_mut(), 0) };
    let entry = size_of::<SYSTEM_CPU_SET_INFORMATION>();
    let count = usize::try_from(len).unwrap_or(0).div_ceil(entry).max(1);
    let mut buf = vec![SYSTEM_CPU_SET_INFORMATION::default(); count];
    let bytes = u32::try_from(count.saturating_mul(entry)).unwrap_or(u32::MAX);
    // SAFETY: `buf` holds `bytes` bytes of aligned entries for the call.
    let ok = unsafe {
        GetSystemCpuSetInformation(buf.as_mut_ptr(), bytes, &mut len, ptr::null_mut(), 0)
    };
    check(ok, "GetSystemCpuSetInformation")?;
    let filled = usize::try_from(len).unwrap_or(0) / entry;
    Ok(buf
        .iter()
        .take(filled)
        // Type 0 (CpuSetInformation) entries of the size this build knows.
        .filter(|e| e.Type == 0 && usize::try_from(e.Size).ok() == Some(entry))
        .map(|e| {
            // SAFETY: a Type 0 entry holds the CpuSet member of the union.
            let c = unsafe { e.Anonymous.CpuSet };
            // SAFETY: AllFlags is the byte view of the flags union.
            let flags = unsafe { c.Anonymous1.AllFlags };
            CpuSet {
                id: c.Id,
                group: c.Group,
                lp: c.LogicalProcessorIndex,
                core: c.CoreIndex,
                realtime: u32::from(flags) & SYSTEM_CPU_SET_INFORMATION_REALTIME != 0,
            }
        })
        .collect())
}

fn ids(lps: &[u8]) -> io::Result<Vec<u32>> {
    cpuset::ids_for(lps, &system_cpu_sets()?).map_err(io::Error::other)
}

/// Makes `lps` this process's default CPU Set: every thread without its own
/// selection (the driver's callback thread included) runs there. Empty = no
/// default. Returns the IDs.
pub fn set_process_cpus(lps: &[u8]) -> io::Result<Vec<u32>> {
    let ids = ids(lps)?;
    // SAFETY: `ids` lives for the call; a null list with count 0 clears the default.
    let ok = unsafe { SetProcessDefaultCpuSets(GetCurrentProcess(), list(&ids), len32(&ids)) };
    check(ok, "SetProcessDefaultCpuSets").map(|()| ids)
}

/// Selects `lps` for the calling thread. Returns the IDs.
pub fn set_thread_cpus(lps: &[u8]) -> io::Result<Vec<u32>> {
    let ids = ids(lps)?;
    // SAFETY: as in `set_process_cpus`, for the pseudo-handle of this thread.
    let ok = unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), list(&ids), len32(&ids)) };
    check(ok, "SetThreadSelectedCpuSets").map(|()| ids)
}

/// Never EcoQoS, and timer-resolution requests are always honoured (Windows
/// 11 ignores them for hidden processes otherwise).
pub fn disable_power_throttling() -> io::Result<()> {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
            | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        StateMask: 0,
    };
    let size = u32::try_from(size_of::<PROCESS_POWER_THROTTLING_STATE>()).unwrap_or(0);
    // SAFETY: `state` is a valid PROCESS_POWER_THROTTLING_STATE of `size` bytes.
    let ok = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            ptr::from_ref(&state).cast::<c_void>(),
            size,
        )
    };
    check(ok, "SetProcessInformation(ProcessPowerThrottling)")
}

/// The calling thread at TIME_CRITICAL (the hwlat scanner only).
pub fn set_thread_time_critical() -> io::Result<()> {
    // SAFETY: the pseudo-handle of the calling thread and a valid priority.
    let ok = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
    check(ok, "SetThreadPriority")
}

/// The logical processor the caller runs on (no system call).
pub fn current_processor() -> u32 {
    // SAFETY: no arguments; reads the processor number.
    unsafe { GetCurrentProcessorNumber() }
}

/// The caller's thread id (no system call).
pub fn current_thread_id() -> u32 {
    // SAFETY: no arguments; reads the thread environment block.
    unsafe { GetCurrentThreadId() }
}

/// (QPC count, QPC frequency).
pub fn qpc() -> io::Result<(i64, i64)> {
    let (mut count, mut freq) = (0_i64, 0_i64);
    // SAFETY: both out-parameters are valid for the calls.
    check(unsafe { QueryPerformanceCounter(&mut count) }, "QueryPerformanceCounter")?;
    // SAFETY: as above.
    check(unsafe { QueryPerformanceFrequency(&mut freq) }, "QueryPerformanceFrequency")?;
    Ok((count, freq))
}

/// Trace markers: one string event per glitch on [`MARKER_PROVIDER`]; cheap
/// when no trace session listens.
pub struct Markers(REGHANDLE);

impl Markers {
    pub fn register() -> io::Result<Self> {
        let mut handle: REGHANDLE = 0;
        // SAFETY: the GUID and the out-handle outlive the call; no callback.
        let status = unsafe { EventRegister(&MARKER_PROVIDER, None, ptr::null(), &mut handle) };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(i32::try_from(status).unwrap_or(-1)));
        }
        Ok(Self(handle))
    }

    pub fn write(&self, text: &str) {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        // SAFETY: `wide` is NUL-terminated and lives for the call.
        unsafe { EventWriteString(self.0, LEVEL_INFO, 0, wide.as_ptr()) };
    }
}

impl Drop for Markers {
    fn drop(&mut self) {
        // SAFETY: the handle came from EventRegister and is released once.
        unsafe { EventUnregister(self.0) };
    }
}
```

- [ ] **Step 3: Modules, features, mutation scope, rule.**
  1. **`lib.rs`:** after `pub mod asio;` add:

```rust
pub mod cpuset;
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "Windows placement, priority, clock and trace markers (S1c design note §6.2 L5)"
)]
pub mod os;
```

  In the crate docs, replace `and, on Windows only, \`asio\` — the crate's only unsafe code.` with `\`cpuset\` (CPU lists) and, on Windows only, \`asio\` and \`os\` — the crate's only unsafe code.`.

  2. **`Cargo.toml`:** change the `windows-sys` line to:

```toml
# The driver thread's message pump; S1c: CPU Sets, power throttling, thread
# priority, QPC and trace markers (src/os.rs).
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_UI_WindowsAndMessaging", "Win32_System_Threading", "Win32_System_SystemInformation", "Win32_System_Performance", "Win32_System_Diagnostics_Etw"] }
```

  3. **`.cargo/mutants.toml`:** after the `asio.rs` exclusion add:

```toml
  # Windows-only placement/priority/clock/marker FFI (S1c): not compiled on
  # the Linux runners; its decisions live in the mutated cpuset.rs.
  "crates/iem-audio-io/src/os.rs",
```

  4. **`.claude/rules/asio-spike.md`:**
     - add `"crates/iem-audio-io/src/os.rs"` and `"crates/iem-audio-io/src/cpuset.rs"` to `paths`;
     - change the first bullet's start to: `` `asio.rs` and `os.rs` are the crate's only unsafe code (`deny(unsafe_code)` at the root, `allow` on the two modules).``

- [ ] **Step 4: Check the features resolve and commit.**

```bash
cd "$WORK" && cargo fmt --all -- --check && cargo metadata --format-version 1 --locked > /dev/null && git diff --stat Cargo.lock
python3 scripts/check_engine_deps.py
git add crates/iem-audio-io/src/cpuset.rs crates/iem-audio-io/src/os.rs crates/iem-audio-io/src/lib.rs crates/iem-audio-io/Cargo.toml .cargo/mutants.toml .claude/rules/asio-spike.md
git commit -m "feat(s1c): CPU lists and the Windows placement, priority, clock and marker calls

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Expected:
- `Cargo.lock` is unchanged: features add no crates;
- `check_engine_deps.py` passes.

---

### Task 4: The ASIO host and the spike — glitches, markers, CPU Sets, `hwlat`, 10 h

**Files:**
- Modify: `crates/iem-audio-io/src/asio.rs`, `crates/iem-audio-io/examples/asio_spike.rs`

**Interfaces:**
- Consumes: Task 2 (`Glitch`, `GapScan`, `on_thread`, `drain_glitches`) and Task 3 (`cpuset::parse_lps`, the `os::*` functions).
- Produces:
  - `Running::qpc_base(&self) -> Option<(i64, i64)>` and `Running::drain_glitches(&self, out: &mut Vec<Glitch>)`;
  - spike mode `hwlat`, flags `--audio-cpus LIST`, `--stress-cpus LIST`, `--cpu N`, `--threshold-us U`, and `--seconds` up to 36000.
  - **Report fields:**
    - `process.{power_throttling, audio_cpus, stress_cpus, topology}`;
    - per segment `glitches[{kind, at_ns, value}]`, `glitches_unreported`, `qpc.{base, freq}`;
    - `telemetry.{callback_cpus, cpu_other, callback_thread, thread_switches, glitches_dropped}`;
    - `hwlat.{cpu, threshold_us, placed, priority, reads, over, gaps_us, largest}`.
  - **Progress:** `callback_thread`.
  - **Marker text:** `iemmixer-glitch kind=<k> at_qpc=<n> emit_qpc=<n> freq=<n> value=<n>`.

- [ ] **Step 1: The host.** In `asio.rs`:
  1. **Imports:** `use crate::os;` and extend `use crate::telemetry::{self, Snapshot, Telemetry};` to `{self, Glitch, Snapshot, Telemetry}`.
  2. **`struct Stream`:** after `base: Instant,` add:

```rust
    /// The QPC count read right after `base` and the QPC frequency (glitch
    /// times in QPC for the trace markers, S1c design note §4.1).
    base_qpc: i64,
    qpc_freq: i64,
```

  3. **`Host::start`:** directly before `let stream = Box::new(Stream {`, add:

```rust
        // The stream clock's zero and its QPC count, read back to back.
        let base = Instant::now();
        let (base_qpc, qpc_freq) = os::qpc().unwrap_or((0, 0));
```

  In the `Stream` literal, replace `base: Instant::now(),` with `base, base_qpc, qpc_freq,`.

  4. **`impl Running<'_>`:** after `pub fn snapshot(...)` add:

```rust
    /// The QPC count at the stream clock's zero and the QPC frequency.
    pub fn qpc_base(&self) -> Option<(i64, i64)> {
        self.stream().map(|s| (s.base_qpc, s.qpc_freq))
    }

    /// Moves the glitches since the last call into `out` (owner thread only).
    pub fn drain_glitches(&self, out: &mut Vec<Glitch>) {
        if let Some(s) = self.stream() {
            s.telemetry.drain_glitches(out);
        }
    }
```

  5. **`Stream::on_buffer`:** after `let entry = self.base.elapsed();` add:

```rust
        self.telemetry
            .on_thread(os::current_processor(), os::current_thread_id());
```

- [ ] **Step 2: The spike's arguments.** In `asio_spike.rs`:
  1. **Imports:** `use iem_audio_io::cpuset;` and `use iem_audio_io::telemetry::Glitch;` next to the existing imports. The tests import `GlitchKind` themselves: a top-level import used only by tests fails clippy's `unused_imports` under `-D warnings`.
  2. **Replace `USAGE`:**

```rust
const USAGE: &str = "usage: asio_spike probe|duplex|reopen|hwlat --report <file> --stop-file <file> \
[--driver <name>] [--progress <file>] [--frames 32|48|64] [--seconds S] [--burn-us U] [--stress T] \
[--panic-at K] [--cycles C] [--audio-cpus LIST] [--stress-cpus LIST] [--cpu N] [--threshold-us U]";

/// The longest run: an 8 h soak with margin (S1c design note §8 W4).
const MAX_SECONDS: u64 = 36_000;
```

  3. **`Mode`:** add after `Reopen,`:

```rust
    /// One TIME_CRITICAL thread on `--cpu` reads the clock in a loop and
    /// records its gaps (S1c design note §4.1); the card is never opened.
    Hwlat,
```

  4. **`Args`:** add after `cycles: u32,`:

```rust
    audio_cpus: Vec<u8>,
    stress_cpus: Vec<u8>,
    cpu: Option<u8>,
    threshold_us: u64,
```

  5. **`parse`:**
     - add `Some("hwlat") => Mode::Hwlat,` to the mode match;
     - add `audio_cpus: Vec::new(), stress_cpus: Vec::new(), cpu: None, threshold_us: 10,` to the `Args` literal;
     - in the flag match, change `"--seconds" => a.seconds = num(3600)?,` to `"--seconds" => a.seconds = num(MAX_SECONDS)?,` and add:

```rust
            "--audio-cpus" => a.audio_cpus = cpuset::parse_lps(value)?,
            "--stress-cpus" => a.stress_cpus = cpuset::parse_lps(value)?,
            "--cpu" => a.cpu = Some(u8::try_from(num(63)?).unwrap_or(0)),
            "--threshold-us" => a.threshold_us = num(1000)?,
```

     - Replace the checks after the loop (from `if a.driver.is_empty() ...` to the `seconds`/`cycles` check) with:

```rust
    if a.report.as_os_str().is_empty() || a.stop_file.as_os_str().is_empty() {
        return Err("--report and --stop-file are required".to_owned());
    }
    if a.mode != Mode::Hwlat && a.driver.is_empty() {
        return Err("--driver is required (every mode but hwlat)".to_owned());
    }
    if matches!(a.mode, Mode::Duplex | Mode::Reopen) && ![32, 48, 64].contains(&a.frames) {
        return Err("--frames must be 32, 48 or 64".to_owned());
    }
    if a.mode == Mode::Hwlat && (a.cpu.is_none() || a.threshold_us == 0) {
        return Err("hwlat needs --cpu 0..63 and --threshold-us 1..1000".to_owned());
    }
    if a.seconds == 0 || a.cycles == 0 {
        return Err("--seconds and --cycles must be positive".to_owned());
    }
```

- [ ] **Step 3: Portable helpers and placed stress threads.** Replace `impl Stress { fn start ... }` with:

```rust
#[cfg_attr(not(windows), allow(dead_code))]
impl Stress {
    fn start(n: u32, cpus: &[u8]) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..n)
            .map(|_| {
                let stop = Arc::clone(&stop);
                let cpus = cpus.to_vec();
                std::thread::spawn(move || {
                    place_thread(&cpus);
                    while !stop.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                })
            })
            .collect();
        Self { stop, threads }
    }
}

/// Puts the calling thread on `cpus` (Windows; empty = anywhere). A failure
/// leaves the thread where Windows puts it; the report's `process.topology`
/// shows whether those processors exist.
#[cfg_attr(not(windows), allow(dead_code))]
fn place_thread(cpus: &[u8]) {
    #[cfg(windows)]
    if !cpus.is_empty() {
        let _ = iem_audio_io::os::set_thread_cpus(cpus);
    }
    #[cfg(not(windows))]
    let _ = cpus;
}

/// The most glitches one segment's report lists; more are only counted.
#[cfg_attr(not(windows), allow(dead_code))]
const GLITCH_REPORT_CAP: usize = 10_000;

/// Adds `new` to a segment's list up to the cap; returns how many did not fit.
#[cfg_attr(not(windows), allow(dead_code))]
fn keep_glitches(list: &mut Vec<Glitch>, new: &[Glitch]) -> usize {
    let take = new.len().min(GLITCH_REPORT_CAP.saturating_sub(list.len()));
    list.extend(new.iter().take(take).copied());
    new.len() - take
}

/// A glitch's QPC count: the stream's QPC base plus its stream-clock time.
#[cfg_attr(not(windows), allow(dead_code))]
fn glitch_qpc(at_ns: u64, base: i64, freq: i64) -> i64 {
    let ticks = u128::from(at_ns) * u128::try_from(freq).unwrap_or(0) / 1_000_000_000;
    base.saturating_add(i64::try_from(ticks).unwrap_or(i64::MAX))
}

/// The trace marker of one glitch (`latency_report.py` parses it): the
/// glitch's QPC, the QPC when the marker was written, the frequency, the value.
#[cfg_attr(not(windows), allow(dead_code))]
fn marker_text(g: &Glitch, base: i64, freq: i64, emit: i64) -> String {
    format!(
        "iemmixer-glitch kind={} at_qpc={} emit_qpc={emit} freq={freq} value={}",
        g.kind.name(),
        glitch_qpc(g.at_ns, base, freq),
        g.value
    )
}
```

- [ ] **Step 4: Windows side.** In `mod spike`:
  1. **Imports:** extend `use iem_audio_io::telemetry::{Snapshot, dbfs};` to `{GapScan, Glitch, Snapshot, dbfs}`, and add:

```rust
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use iem_audio_io::os;
```

  Extend `use super::{...}` with `keep_glitches, marker_text`.

  2. **`main`:** replace the whole function with:

```rust
    pub fn main(a: &Args) -> ExitCode {
        let mut report = json!({
            "tool": "asio_spike",
            "version": env!("CARGO_PKG_VERSION"),
            "build_sha": option_env!("GITHUB_SHA"),
            "mode": format!("{:?}", a.mode).to_lowercase(),
            "frames": a.frames, "seconds": a.seconds, "burn_us": a.burn_us,
            "stress": a.stress, "panic_at": a.panic_at, "cycles": a.cycles,
            "audio_cpus": a.audio_cpus, "stress_cpus": a.stress_cpus,
            "cpu": a.cpu, "threshold_us": a.threshold_us,
        });
        let (process, refused) = process_setup(a);
        report["process"] = process;
        let code = if let Some(why) = refused {
            report["outcome"] = json!("refused");
            report["error"] = json!(why);
            4
        } else {
            match run(a, &mut report) {
                Ok(code) => code,
                Err(e) => {
                    let (outcome, code) = match e {
                        AsioError::NoDrivers(_) | AsioError::NotFound { .. } => ("no-driver", 3),
                        AsioError::Refused(_) => ("refused", 4),
                        _ => ("error", 1),
                    };
                    report["outcome"] = json!(outcome);
                    report["error"] = json!(e.to_string());
                    code
                }
            }
        };
        match write_json(&a.report, &report) {
            Ok(()) => ExitCode::from(code),
            Err(e) => {
                eprintln!("asio_spike: writing {}: {e}", a.report.display());
                ExitCode::from(1)
            }
        }
    }
```

  3. **`run`:**
     - as its first statement, add `if a.mode == Mode::Hwlat { return Ok(hwlat(a, report)); }`: hwlat never opens the card;
     - in the final `match a.mode`, add the arm `Mode::Hwlat => Ok(hwlat(a, report)),`. It keeps the match exhaustive without `unreachable!`; the early return above means it is never reached.

  4. **New functions** (after `run`):

```rust
    /// In-process levers (S1c design note §6.2 L5): power throttling off and
    /// the audio CPU Set as the process default (every thread without its own
    /// selection, the driver's included, runs there; no priority changes). A
    /// requested CPU Set that cannot be applied refuses the run: a
    /// measurement on the wrong processors would mislead.
    fn process_setup(a: &Args) -> (Value, Option<String>) {
        let throttling = os::disable_power_throttling()
            .map_or_else(|e| json!(e.to_string()), |()| json!("off"));
        let topology = match os::system_cpu_sets() {
            Ok(sets) => json!(sets
                .iter()
                .map(|c| json!({ "id": c.id, "group": c.group, "lp": c.lp, "core": c.core, "realtime": c.realtime }))
                .collect::<Vec<_>>()),
            Err(e) => json!({ "error": e.to_string() }),
        };
        let (audio, refused) = if a.audio_cpus.is_empty() {
            (json!(null), None)
        } else {
            match os::set_process_cpus(&a.audio_cpus) {
                Ok(ids) => (json!({ "lps": a.audio_cpus, "ids": ids }), None),
                Err(e) => (
                    json!({ "lps": a.audio_cpus, "error": e.to_string() }),
                    Some(format!("audio CPU Set: {e}")),
                ),
            }
        };
        (
            json!({ "power_throttling": throttling, "audio_cpus": audio, "stress_cpus": a.stress_cpus, "topology": topology }),
            refused,
        )
    }

    /// hwlat (S1c design note §4.1): one thread at TIME_CRITICAL on `--cpu`
    /// reads the clock in a tight loop; every gap of at least the threshold
    /// is a stall of that processor. The card is never opened.
    fn hwlat(a: &Args, report: &mut Value) -> u8 {
        if a.stop_file.exists() {
            report["outcome"] = json!("stopped");
            return 0;
        }
        let Some(cpu) = a.cpu else {
            report["outcome"] = json!("error");
            return 2;
        };
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let threshold_ns = a.threshold_us.saturating_mul(1_000);
        let end = Duration::from_secs(a.seconds);
        let scanner = std::thread::spawn(move || {
            let placed = os::set_thread_cpus(&[cpu])
                .map_or_else(|e| json!(e.to_string()), |ids| json!(ids));
            let priority = os::set_thread_time_critical()
                .map_or_else(|e| json!(e.to_string()), |()| json!("time-critical"));
            let mut scan = GapScan::new(threshold_ns);
            let t0 = Instant::now();
            let mut prev = 0_u64;
            loop {
                let now = t0.elapsed();
                let ns = u64::try_from(now.as_nanos()).unwrap_or(u64::MAX);
                scan.observe(prev, ns);
                prev = ns;
                if now >= end || flag.load(Ordering::Relaxed) {
                    break;
                }
            }
            (scan.summary(), placed, priority)
        });
        let mut outcome = "done";
        while !scanner.is_finished() {
            std::thread::sleep(Duration::from_millis(100));
            if a.stop_file.exists() && !stop.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
                outcome = "stopped";
            }
        }
        match scanner.join() {
            Ok((s, placed, priority)) => {
                let q = |v: [f64; 4]| json!({ "p50": v[0], "p99": v[1], "p999": v[2], "max": v[3] });
                report["hwlat"] = json!({
                    "cpu": cpu, "threshold_us": a.threshold_us, "placed": placed, "priority": priority,
                    "reads": s.reads, "over": s.over, "gaps_us": q(s.gaps.summary_us()),
                    "largest": s.largest.iter().map(|&(at, gap)| json!({ "at_us": at as f64 / 1e3, "gap_us": gap as f64 / 1e3 })).collect::<Vec<_>>(),
                });
                report["outcome"] = json!(outcome);
                0
            }
            Err(_) => {
                report["outcome"] = json!("error");
                1
            }
        }
    }

    /// Drains the stream's new glitches: one trace marker each, then into the
    /// segment's list (capped). Returns how many did not fit the list.
    fn take_glitches(
        running: &Running<'_>,
        fresh: &mut Vec<Glitch>,
        kept: &mut Vec<Glitch>,
        markers: Option<&os::Markers>,
        qpc: Option<(i64, i64)>,
    ) -> usize {
        fresh.clear();
        running.drain_glitches(fresh);
        if let (Some(m), Some((base, freq))) = (markers, qpc) {
            let emit = os::qpc().map_or(0, |q| q.0);
            for g in fresh.iter() {
                m.write(&marker_text(g, base, freq, emit));
            }
        }
        keep_glitches(kept, fresh)
    }

    fn glitches_json(list: &[Glitch]) -> Value {
        json!(list
            .iter()
            .map(|g| json!({ "kind": g.kind.name(), "at_ns": g.at_ns, "value": g.value }))
            .collect::<Vec<_>>())
    }
```

  5. **`duplex`:**
     - Replace `let _stress = Stress::start(a.stress);` with:

```rust
        let _stress = Stress::start(a.stress, &a.stress_cpus);
        let markers = os::Markers::register().ok();
```

     - Inside the segment loop, after `let latency = host.latencies()?;`, add:

```rust
            let qpc = running.qpc_base();
            let mut glitches: Vec<Glitch> = Vec::new();
            let mut fresh: Vec<Glitch> = Vec::with_capacity(1_024);
            let mut unreported = 0_usize;
```

     - As the first statement after `std::thread::sleep(Duration::from_millis(10));` in the inner loop, add `unreported += take_glitches(&running, &mut fresh, &mut glitches, markers.as_ref(), qpc);`.
     - Before `let (snap, stop) = running.finish();`, add the same line once more: it drains the last glitches before the stream is freed.
     - In the segment's `json!({...})`, after `"stop": stop_json(stop),`, add:

```rust
                    "glitches": glitches_json(&glitches), "glitches_unreported": unreported,
                    "qpc": qpc.map(|(base, freq)| json!({ "base": base, "freq": freq })),
```

  6. **`telemetry_json`:** after `"drift_ppm": s.drift_ppm,` add:

```rust
            "callback_cpus": s.callback_cpus.iter().map(|&(lp, n)| (lp.to_string(), json!(n))).collect::<serde_json::Map<_, _>>(),
            "cpu_other": s.cpu_other, "callback_thread": s.callback_thread,
            "thread_switches": s.thread_switches, "glitches_dropped": s.glitches_dropped,
```

  7. **`progress_json`:** add `"callback_thread": s.callback_thread,` after `"resets": s.resets,`.

- [ ] **Step 5: Tests (`asio_spike.rs` `mod tests`).**
  1. **`stress_threads_stop_when_dropped`:** change the call to `Stress::start(2, &[])`.
  2. **`parses_a_duplex_run_under_load`:**
     - append ` --audio-cpus 14 --stress-cpus 6-13` to the argument string;
     - add `audio_cpus: vec![14], stress_cpus: vec![6, 7, 8, 9, 10, 11, 12, 13], cpu: None, threshold_us: 10,` to the expected `Args`.
  3. **`bad_input_is_refused`:**
     - change `"duplex --driver D1 --report r --stop-file s --frames 32 --seconds 3601",` to `... --seconds 36001",`;
     - add to the bad list:

```rust
            "hwlat --report r --stop-file s",
            "hwlat --report r --stop-file s --cpu 64",
            "hwlat --report r --stop-file s --cpu 3 --threshold-us 0",
            "hwlat --report r --stop-file s --cpu 3 --threshold-us 1001",
            "duplex --driver D1 --report r --stop-file s --frames 32 --audio-cpus 1,1",
            "duplex --driver D1 --report r --stop-file s --frames 32 --stress-cpus 70",
```

     - and after the loop:

```rust
        assert!(parse(&argv("duplex --driver D1 --report r --stop-file s --frames 32 --seconds 36000")).is_ok());
        let h = parse(&argv("hwlat --report r --stop-file s --cpu 14 --seconds 30")).unwrap();
        assert_eq!((h.mode, h.cpu, h.threshold_us, h.seconds, h.driver.is_empty()), (Mode::Hwlat, Some(14), 10, 30, true));
```

  4. **New tests:**

```rust
    use iem_audio_io::telemetry::GlitchKind;

    fn glitch(kind: GlitchKind, at_ns: u64, value: u64) -> Glitch {
        Glitch { kind, at_ns, value }
    }

    #[test]
    fn glitch_list_is_capped() {
        let mut list = vec![glitch(GlitchKind::Late, 0, 1); GLITCH_REPORT_CAP - 2];
        let new = [glitch(GlitchKind::Missed, 1, 2); 5];
        assert_eq!(keep_glitches(&mut list, &new), 3);
        assert_eq!(list.len(), GLITCH_REPORT_CAP);
        assert_eq!(keep_glitches(&mut list, &new), 5);
        let mut empty = Vec::new();
        assert_eq!(keep_glitches(&mut empty, &new), 0);
        assert_eq!(empty.len(), 5);
    }

    #[test]
    fn glitch_times_convert_to_qpc_and_markers_carry_them() {
        assert_eq!(glitch_qpc(1_000_000_000, 100, 10_000_000), 10_000_100);
        assert_eq!(glitch_qpc(333_333, 0, 10_000_000), 3_333);
        assert_eq!(glitch_qpc(5, 7, 0), 7);
        assert_eq!(
            marker_text(&glitch(GlitchKind::Missed, 1_000_000_000, 700_000), 100, 10_000_000, 10_050_000),
            "iemmixer-glitch kind=missed at_qpc=10000100 emit_qpc=10050000 freq=10000000 value=700000"
        );
    }
```

- [ ] **Step 6: Format and commit.**

```bash
cd "$WORK" && cargo fmt --all -- --check
git add crates/iem-audio-io/src/asio.rs crates/iem-audio-io/examples/asio_spike.rs
git commit -m "feat(s1c): spike records glitch times, callback CPUs and markers; hwlat mode; CPU Sets; 10 h runs

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `IemTuning.psm1` — items, journal, tiers, mode, fingerprint, inventory

**Files:**
- Create: `scripts/pc-tuning/IemTuning.psm1`

**Interfaces:**
- **Consumed by:** Task 6 (`IemMeasure.psm1` imports it), Task 7 (self-test), Task 10 (`tuning_window.py` over ssh).
- **Profile schema** (JSON; `Read-IemProfile` refuses any missing key):
  - `version`, `journal`, `registry_root`;
  - `layout.{housekeeping,card,nic,audio}`;
  - `plan.{guid,source}`, `governor`;
  - `placement[]`, `services_disable[]`, `services_mode[]`;
  - `updates.{services[],tasks[]}`, `maintenance.{off,tasks[]}`, `defender.{paths[],processes[]}`;
  - `devices[{id,instance,hwid,lps[],enabled}]`;
  - `nic.{adapter,properties{},rss{base,max},pnp_capabilities}` (optional `nic.key` for tests);
  - `fingerprint.{files[],keys[]}`.
- **Exported functions:**
  - `Read-IemProfile -Path`;
  - `Invoke-IemTuningApply -ProfilePath -Tier 2|3 [-Only groups]` → rows `{key, tier, group, action: kept|written|absent|failed, before, value, error}`;
  - `Undo-IemTuning -ProfilePath -Tier 2|3 [-Only groups]` → rows `{key, action, value, error}`;
  - `Enter-IemTuningMode -ProfilePath [-Only plan,governor,placement,services] [-Idle default|c1|disable]`;
  - `Exit-IemTuningMode -ProfilePath` (throws after restoring everything it can when anything failed);
  - `Get-IemTuningState -ProfilePath`;
  - `Get-IemReaperFingerprint -ProfilePath`, `Compare-IemFingerprint -Baseline -Current`;
  - `Get-IemInventory -ProfilePath`, `Get-IemBootTime`.
- **Groups:**
  - Tier 2: `services`, `updates`, `maintenance`, `defender`;
  - Tier 3: `irq` (enabled devices) or `irq:<id>` (one device), `nic`.

- [ ] **Step 1: Create the module.**

```powershell
#Requires -Version 5.1
# S1c Windows tuning (docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md §6, §7).
# Every change is an item: a kind with arguments and a desired value, read by
# Get-IemValue and written by Set-IemValue. Apply writes only what differs,
# journals the value before the first write and reads back; undo and exit
# write the journaled value back and read back. Values are strings; $null is
# "absent". Nothing here ends a process, forces a service or restarts Windows.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:Schema = 1
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false

if (-not ('IemPower' -as [type])) {
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class IemPower {
    [DllImport("powrprof.dll")] static extern uint PowerGetActiveScheme(IntPtr root, out IntPtr scheme);
    [DllImport("powrprof.dll")] static extern uint PowerSetActiveScheme(IntPtr root, ref Guid scheme);
    [DllImport("powrprof.dll")] static extern uint PowerReadACValueIndex(IntPtr root, ref Guid scheme, ref Guid sub, ref Guid setting, out uint value);
    [DllImport("powrprof.dll")] static extern uint PowerWriteACValueIndex(IntPtr root, ref Guid scheme, ref Guid sub, ref Guid setting, uint value);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr p);

    public static string Active() {
        IntPtr p;
        uint rc = PowerGetActiveScheme(IntPtr.Zero, out p);
        if (rc != 0) throw new Win32Exception((int)rc);
        try { return ((Guid)Marshal.PtrToStructure(p, typeof(Guid))).ToString(); } finally { LocalFree(p); }
    }
    public static void Activate(string scheme) {
        Guid g = new Guid(scheme);
        uint rc = PowerSetActiveScheme(IntPtr.Zero, ref g);
        if (rc != 0) throw new Win32Exception((int)rc);
    }
    // -1 when the scheme or the setting does not exist.
    public static long Read(string scheme, string sub, string setting) {
        Guid a = new Guid(scheme), b = new Guid(sub), c = new Guid(setting);
        uint v;
        return PowerReadACValueIndex(IntPtr.Zero, ref a, ref b, ref c, out v) == 0 ? (long)v : -1;
    }
    public static void Write(string scheme, string sub, string setting, uint value) {
        Guid a = new Guid(scheme), b = new Guid(sub), c = new Guid(setting);
        uint rc = PowerWriteACValueIndex(IntPtr.Zero, ref a, ref b, ref c, value);
        if (rc != 0) throw new Win32Exception((int)rc);
    }
}

public static class IemCpuSets {
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetSystemCpuSetInformation(IntPtr info, uint length, out uint returned, IntPtr process, uint flags);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool GetProcessDefaultCpuSets(IntPtr process, uint[] ids, uint count, out uint required);
    [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetProcessDefaultCpuSets(IntPtr process, uint[] ids, uint count);
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    const uint QueryLimited = 0x1000, SetLimited = 0x2000;

    // Group 0: logical processor index -> CPU Set ID.
    public static Dictionary<int, uint> Map() {
        uint len;
        GetSystemCpuSetInformation(IntPtr.Zero, 0, out len, IntPtr.Zero, 0);
        IntPtr buf = Marshal.AllocHGlobal((int)len);
        try {
            if (!GetSystemCpuSetInformation(buf, len, out len, IntPtr.Zero, 0)) throw new Win32Exception();
            var map = new Dictionary<int, uint>();
            int off = 0;
            while (off + 16 <= (int)len) {
                int size = Marshal.ReadInt32(buf, off);
                if (size <= 0) break;
                if (Marshal.ReadInt32(buf, off + 4) == 0 && Marshal.ReadInt16(buf, off + 12) == 0) {
                    map[Marshal.ReadByte(buf, off + 14)] = (uint)Marshal.ReadInt32(buf, off + 8);
                }
                off += size;
            }
            return map;
        } finally { Marshal.FreeHGlobal(buf); }
    }
    static IntPtr Open(int pid, uint access) {
        IntPtr h = OpenProcess(access, false, pid);
        if (h == IntPtr.Zero) throw new Win32Exception();
        return h;
    }
    public static uint[] Get(int pid) {
        IntPtr h = Open(pid, QueryLimited);
        try {
            uint needed;
            if (GetProcessDefaultCpuSets(h, null, 0, out needed)) return new uint[0];
            if (Marshal.GetLastWin32Error() != 122) throw new Win32Exception();
            uint[] ids = new uint[needed];
            if (!GetProcessDefaultCpuSets(h, ids, needed, out needed)) throw new Win32Exception();
            return ids;
        } finally { CloseHandle(h); }
    }
    public static void Set(int pid, uint[] ids) {
        IntPtr h = Open(pid, SetLimited);
        try {
            if (!SetProcessDefaultCpuSets(h, ids.Length == 0 ? null : ids, (uint)ids.Length)) throw new Win32Exception();
        } finally { CloseHandle(h); }
    }
}

public static class IemTimer {
    [DllImport("ntdll.dll")] static extern int NtQueryTimerResolution(out uint coarsest, out uint finest, out uint current);
    // 100 ns units: coarsest, finest, current.
    public static uint[] Query() {
        uint a, b, c;
        int rc = NtQueryTimerResolution(out a, out b, out c);
        if (rc != 0) throw new Win32Exception(rc);
        return new uint[] { a, b, c };
    }
}
'@
}

# The iemmixer plan's settings (design note §6.2 L2): subgroup, setting, AC value.
$script:PlanSettings = @(
    @{ name = 'proc-min'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = '893dee8e-2bef-41e0-89c6-b55d0929964c'; value = 100 },
    @{ name = 'proc-max'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = 'bc5038f7-23e0-4960-96da-33abaf5935ec'; value = 100 },
    @{ name = 'park-min-cores'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = '0cc5b647-c1df-4637-891a-dec35c318583'; value = 100 },
    @{ name = 'park-max-cores'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = 'ea062031-0e34-4ff1-9b6d-eb1059334028'; value = 100 },
    @{ name = 'epp'; sub = '54533251-82be-4824-96c1-47b60b740d00'; setting = '36687f9e-e3a5-4dbf-b1dc-15eb381c6863'; value = 0 },
    @{ name = 'pcie-aspm'; sub = '501a4d13-42af-4429-9fd1-a8218c268e20'; setting = 'ee12f906-d277-404b-b6da-e5fa1a576df5'; value = 0 },
    @{ name = 'usb-suspend'; sub = '2a737441-1930-4402-8d77-b2bebba308a3'; setting = '48e6b7a6-50f5-4782-a5d4-53bb8f07e226'; value = 0 },
    @{ name = 'display-off'; sub = '7516b95f-f776-4464-8c53-06167f40cc99'; setting = '3c0bc021-c8a8-4e07-a973-6b14cbcb2b7e'; value = 0 },
    @{ name = 'disk-off'; sub = '0012ee47-9041-4b5d-9b77-535fba8b1442'; setting = '6738e2c4-e8a5-4a42-b16a-e040e769756e'; value = 0 },
    @{ name = 'sleep'; sub = '238c9fa8-0aad-41ed-83f4-97be242c8f20'; setting = '29f6c1db-86da-48c5-9fdb-f2b67b1f44da'; value = 0 },
    @{ name = 'hibernate'; sub = '238c9fa8-0aad-41ed-83f4-97be242c8f20'; setting = '9d7815a6-7ee4-497e-8888-515a05f02364'; value = 0 }
)
$script:ProcessorSub = '54533251-82be-4824-96c1-47b60b740d00'
$script:IdleDisable = '5d76a2ca-e8c0-402f-a133-2158492d58ad'
$script:IdleStateMax = '9943e905-9a30-4ec1-9b99-44dd3b76f7a2'
$script:NetClass = 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e972-e325-11ce-bfc1-08002be10318}'

function Read-IemProfile {
    param([Parameter(Mandatory)][string]$Path)
    $p = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    foreach ($k in 'version', 'journal', 'registry_root', 'layout', 'plan', 'governor', 'placement', 'services_disable', 'services_mode',
                   'updates', 'maintenance', 'defender', 'devices', 'nic', 'fingerprint') {
        if (-not $p.PSObject.Properties[$k]) { throw "profile ${Path}: missing '$k'" }
    }
    if ([int]$p.version -lt 1) { throw "profile ${Path}: version must be a positive integer" }
    return $p
}

function Get-IemRegPath {
    # Tests map HKLM:\... and HKCU:\... under a test key (profile registry_root).
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)][string]$Path)
    if (-not $Profile.registry_root) { return $Path }
    return Join-Path $Profile.registry_root ($Path -replace '^(HKLM|HKCU):\\', '$1\')
}

function Get-IemBootTime {
    (Get-CimInstance -ClassName Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().ToString('o')
}

function Get-IemTextHash {
    param([AllowEmptyString()][string]$Text)
    $sha = [Security.Cryptography.SHA256]::Create()
    return (($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Text)) | ForEach-Object { $_.ToString('x2') }) -join '')
}

function Test-IemSame {
    param([AllowNull()]$A, [AllowNull()]$B)
    if ($null -eq $A) { return $null -eq $B }
    if ($null -eq $B) { return $false }
    return [string]$A -eq [string]$B
}

function New-IemItem {
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Kind, [Parameter(Mandatory)][hashtable]$Arguments,
          [AllowNull()]$Desired, [int]$Tier = 0, [string]$Group = '', [switch]$Reboot)
    [pscustomobject]@{ key = $Key; kind = $Kind; args = $Arguments; tier = $Tier; group = $Group; reboot = [bool]$Reboot
                       desired = $(if ($null -eq $Desired) { $null } else { [string]$Desired }) }
}

function Test-IemPlan {
    param([Parameter(Mandatory)][string]$Guid)
    return [bool](@(& powercfg.exe /list) -match [regex]::Escape($Guid))
}

function Get-IemDefenderList {
    param([Parameter(Mandatory)][ValidateSet('ExclusionPath', 'ExclusionProcess')][string]$Name)
    return @((Get-MpPreference).$Name | Where-Object { $_ })
}

function Assert-IemSameProcess {
    # A journaled pid is only this process while its name and start time match.
    param([Parameter(Mandatory)]$Arguments)
    $p = Get-Process -Id ([int]$Arguments.pid) -ErrorAction SilentlyContinue
    if (-not $p -or $p.ProcessName -ne $Arguments.name -or $p.StartTime.ToUniversalTime().Ticks -ne [long]$Arguments.start) {
        throw "process $($Arguments.name) ($($Arguments.pid)) is gone or its pid was reused"
    }
}

function Get-IemValue {
    param([Parameter(Mandatory)]$Item)
    $a = $Item.args
    switch ($Item.kind) {
        'reg' {
            if (-not (Test-Path -LiteralPath $a.path)) { return $null }
            $v = (Get-Item -LiteralPath $a.path).GetValue($a.name, $null, 'DoNotExpandEnvironmentNames')
            if ($null -eq $v) { return $null }
            if ($v -is [byte[]]) { return (($v | ForEach-Object { $_.ToString('x2') }) -join '') }
            return [string]$v
        }
        'svc-start' {
            $key = "HKLM:\SYSTEM\CurrentControlSet\Services\$($a.name)"
            if (-not (Test-Path -LiteralPath $key)) { return $null }
            $k = Get-Item -LiteralPath $key
            $start = [int]$k.GetValue('Start', -1)
            if ($start -eq 2 -and [int]$k.GetValue('DelayedAutostart', 0) -eq 1) { return 'delayed-auto' }
            $names = @{ 0 = 'boot'; 1 = 'system'; 2 = 'auto'; 3 = 'demand'; 4 = 'disabled' }
            if ($names.ContainsKey($start)) { return $names[$start] }
            return "start-$start"
        }
        'svc-state' {
            $s = Get-Service -Name $a.name -ErrorAction SilentlyContinue
            if (-not $s) { return $null }
            if ($s.Status -eq 'Running') { return 'running' }
            return 'stopped'
        }
        'task' {
            $t = Get-ScheduledTask -TaskPath $a.path -TaskName $a.name -ErrorAction SilentlyContinue
            if (-not $t) { return $null }
            if ("$($t.State)" -eq 'Disabled') { return 'disabled' }
            return 'enabled'
        }
        'plan-exists' { if (Test-IemPlan -Guid $a.guid) { return 'present' }; return $null }
        'plan-value' {
            $v = [IemPower]::Read($a.guid, $a.sub, $a.setting)
            if ($v -lt 0) { return $null }
            return [string]$v
        }
        'plan-active' { return [IemPower]::Active() }
        'defender-path' { if ((Get-IemDefenderList -Name 'ExclusionPath') -contains $a.value) { return 'present' }; return $null }
        'defender-process' { if ((Get-IemDefenderList -Name 'ExclusionProcess') -contains $a.value) { return 'present' }; return $null }
        'cpusets' {
            try { Assert-IemSameProcess -Arguments $a } catch { return $null }
            return ((@([IemCpuSets]::Get([int]$a.pid)) | Sort-Object) -join ',')
        }
        default { throw "unknown item kind '$($Item.kind)'" }
    }
}

function Set-IemValue {
    param([Parameter(Mandatory)]$Item, [AllowNull()]$Value)
    $a = $Item.args
    switch ($Item.kind) {
        'reg' {
            if ($null -eq $Value) {
                if ((Test-Path -LiteralPath $a.path) -and $null -ne (Get-Item -LiteralPath $a.path).GetValue($a.name, $null)) {
                    Remove-ItemProperty -LiteralPath $a.path -Name $a.name
                }
                return
            }
            if (-not (Test-Path -LiteralPath $a.path)) { New-Item -Path $a.path -Force | Out-Null }
            $data = switch ($a.type) {
                'DWord' { [int]$Value }
                'QWord' { [long]$Value }
                'String' { [string]$Value }
                'Binary' { [byte[]]@(for ($i = 0; $i -lt $Value.Length; $i += 2) { [Convert]::ToByte($Value.Substring($i, 2), 16) }) }
                default { throw "registry type '$($a.type)' refused" }
            }
            New-ItemProperty -LiteralPath $a.path -Name $a.name -Value $data -PropertyType $a.type -Force | Out-Null
        }
        'svc-start' {
            if (@('auto', 'delayed-auto', 'demand', 'disabled') -notcontains [string]$Value) { throw "service start type '$Value' refused for $($a.name)" }
            $out = & sc.exe config $a.name start= ([string]$Value) 2>&1
            if ($LASTEXITCODE -ne 0) { throw "sc.exe config $($a.name) start= ${Value}: $($out -join ' ')" }
        }
        'svc-state' {
            $s = Get-Service -Name $a.name
            if ($Value -eq 'running') { Start-Service -InputObject $s; $s.WaitForStatus('Running', [TimeSpan]::FromSeconds(60)) }
            elseif ($Value -eq 'stopped') { Stop-Service -InputObject $s; $s.WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60)) }
            else { throw "service state '$Value' refused" }
        }
        'task' {
            if ($Value -eq 'disabled') { Disable-ScheduledTask -TaskPath $a.path -TaskName $a.name | Out-Null }
            elseif ($Value -eq 'enabled') { Enable-ScheduledTask -TaskPath $a.path -TaskName $a.name | Out-Null }
            else { throw "task state '$Value' refused" }
        }
        'plan-exists' {
            if ($Value -eq 'present') {
                $out = & powercfg.exe /duplicatescheme $a.source $a.guid 2>&1
                if ($LASTEXITCODE -ne 0) { throw "powercfg /duplicatescheme: $($out -join ' ')" }
            } else {
                if ([IemPower]::Active() -eq $a.guid) { throw "plan $($a.guid) is active: not deleted" }
                $out = & powercfg.exe /delete $a.guid 2>&1
                if ($LASTEXITCODE -ne 0) { throw "powercfg /delete: $($out -join ' ')" }
            }
        }
        'plan-value' {
            if ($null -eq $Value) { throw 'a plan value cannot be removed' }
            [IemPower]::Write($a.guid, $a.sub, $a.setting, [uint32]$Value)
        }
        'plan-active' { [IemPower]::Activate([string]$Value) }
        'defender-path' { if ($Value -eq 'present') { Add-MpPreference -ExclusionPath $a.value } else { Remove-MpPreference -ExclusionPath $a.value } }
        'defender-process' { if ($Value -eq 'present') { Add-MpPreference -ExclusionProcess $a.value } else { Remove-MpPreference -ExclusionProcess $a.value } }
        'cpusets' {
            Assert-IemSameProcess -Arguments $a
            $ids = if ([string]::IsNullOrEmpty([string]$Value)) { [uint32[]]@() } else { [uint32[]]@(([string]$Value) -split ',' | ForEach-Object { [uint32]$_ }) }
            [IemCpuSets]::Set([int]$a.pid, $ids)
        }
        default { throw "unknown item kind '$($Item.kind)'" }
    }
}

function Read-IemJournal {
    param([Parameter(Mandatory)][string]$Path)
    $j = @{ schema = $script:Schema; version = 0; entered = $false; global = @{}; mode = @{}; reverted = @{}; order = @{ global = @(); mode = @() } }
    if (-not (Test-Path -LiteralPath $Path)) { return $j }
    $o = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    if ([int]$o.schema -ne $script:Schema) { throw "journal ${Path}: schema $($o.schema), this module $($script:Schema)" }
    $j.version = [int]$o.version
    $j.entered = [bool]$o.entered
    foreach ($s in 'global', 'mode', 'reverted') {
        foreach ($p in $o.$s.PSObject.Properties) { $j[$s][$p.Name] = $p.Value }
    }
    foreach ($s in 'global', 'mode') { $j.order[$s] = @($o.order.$s | Where-Object { $_ }) }
    return $j
}

function Write-IemJournal {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][hashtable]$Journal)
    $dir = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Force -Path $dir | Out-Null }
    $tmp = "$Path.tmp"
    [IO.File]::WriteAllText($tmp, ($Journal | ConvertTo-Json -Depth 8), $script:Utf8NoBom)
    Move-Item -LiteralPath $tmp -Destination $Path -Force
}

function ConvertTo-IemItem {
    # An item rebuilt from its journal entry, desired = the journaled before-value.
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)]$Entry)
    [pscustomobject]@{ key = $Key; kind = $Entry.kind; args = $Entry.args; tier = [int]$Entry.tier; group = [string]$Entry.group
                       reboot = [bool]$Entry.reboot; desired = $Entry.before }
}

function Invoke-IemItem {
    # Write one item only when it differs; journal its value before the first
    # write (saved before the write), then read back. An optional target that
    # does not exist (a task or service missing on this edition) is 'absent'.
    param([Parameter(Mandatory)]$Item, [Parameter(Mandatory)][hashtable]$Journal, [Parameter(Mandatory)][string]$Section,
          [Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Boot)
    $row = [ordered]@{ key = $Item.key; tier = $Item.tier; group = $Item.group; action = ''; before = $null; value = $null; error = $null }
    try {
        $before = Get-IemValue -Item $Item
        $row.before = $before
        if ($null -eq $before -and @('task', 'svc-start', 'svc-state', 'cpusets') -contains $Item.kind) { $row.action = 'absent'; return [pscustomobject]$row }
        if (Test-IemSame $before $Item.desired) { $row.action = 'kept'; $row.value = $before; return [pscustomobject]$row }
        if (-not $Journal[$Section].ContainsKey($Item.key)) {
            $Journal[$Section][$Item.key] = @{ kind = $Item.kind; args = $Item.args; before = $before; tier = $Item.tier; group = $Item.group
                                               reboot = $Item.reboot; at = (Get-Date).ToUniversalTime().ToString('o'); boot = $Boot }
            $Journal.order[$Section] = @($Journal.order[$Section]) + $Item.key
            Write-IemJournal -Path $Path -Journal $Journal
        }
        Set-IemValue -Item $Item -Value $Item.desired
        $after = Get-IemValue -Item $Item
        if (-not (Test-IemSame $after $Item.desired)) { throw "read back '$after' after writing '$($Item.desired)'" }
        $row.action = 'written'; $row.value = $after
    } catch { $row.action = 'failed'; $row.error = "$_" }
    return [pscustomobject]$row
}

function Restore-IemItem {
    # Write the journaled before-value back and read it back. A placed process
    # that ended (or whose pid was reused) is 'gone': nothing to restore.
    param([Parameter(Mandatory)]$Item)
    $now = Get-IemValue -Item $Item
    if ($Item.kind -eq 'cpusets' -and $null -eq $now) { return [pscustomobject]@{ key = $Item.key; action = 'gone'; value = $null; error = $null } }
    if (Test-IemSame $now $Item.desired) { return [pscustomobject]@{ key = $Item.key; action = 'kept'; value = $now; error = $null } }
    Set-IemValue -Item $Item -Value $Item.desired
    $after = Get-IemValue -Item $Item
    if (-not (Test-IemSame $after $Item.desired)) { throw "$($Item.key): read back '$after' after restoring '$($Item.desired)'" }
    return [pscustomobject]@{ key = $Item.key; action = 'restored'; value = $after; error = $null }
}

function ConvertTo-IemMask {
    param([Parameter(Mandatory)][int[]]$Lps)
    [long]$m = 0
    foreach ($lp in $Lps) {
        if ($lp -lt 0 -or $lp -gt 62) { throw "logical processor $lp outside 0..62" }
        $m = $m -bor ([long]1 -shl $lp)
    }
    return $m
}

function Assert-IemDevice {
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)]$Device)
    $key = Get-IemRegPath $Profile "HKLM:\SYSTEM\CurrentControlSet\Enum\$($Device.instance)"
    if (-not (Test-Path -LiteralPath $key)) { throw "device $($Device.id): instance not found (profile stale?)" }
    $hw = @((Get-Item -LiteralPath $key).GetValue('HardwareID', [string[]]@()))
    if (-not ($hw | Where-Object { $_ -like "$($Device.hwid)*" })) { throw "device $($Device.id): hardware id does not match the profile" }
}

function Get-IemNicKey {
    param([Parameter(Mandatory)]$Profile)
    if ($Profile.nic.PSObject.Properties['key']) { return Get-IemRegPath $Profile $Profile.nic.key }
    $guid = "$((Get-NetAdapter -Name $Profile.nic.adapter).InterfaceGuid)"
    foreach ($k in Get-ChildItem -LiteralPath $script:NetClass -ErrorAction SilentlyContinue) {
        if ("$($k.GetValue('NetCfgInstanceId', ''))" -eq $guid) { return $k.PSPath }
    }
    throw "adapter '$($Profile.nic.adapter)': driver key not found"
}

function Select-IemGroup {
    param([string[]]$Only, [Parameter(Mandatory)][string]$Group)
    return (@($Only).Count -eq 0) -or (@($Only) -contains $Group)
}

function Get-IemGlobalItems {
    # Tier 2 (no reboot) and Tier 3 (reboot) items of the profile. -Check
    # verifies each device before its items are built (apply only).
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)][int]$Tier, [string[]]$Only = @(), [switch]$Check)
    $items = @()
    if ($Tier -eq 2) {
        if (Select-IemGroup $Only 'services') {
            foreach ($n in @($Profile.services_disable)) {
                # Stopped first, then disabled: undo runs newest first, so the start
                # type is restored before the service is started again.
                $items += New-IemItem -Key "svc:${n}:state" -Kind 'svc-state' -Arguments @{ name = $n } -Desired 'stopped' -Tier 2 -Group 'services'
                $items += New-IemItem -Key "svc:${n}:start" -Kind 'svc-start' -Arguments @{ name = $n } -Desired 'disabled' -Tier 2 -Group 'services'
            }
        }
        if (Select-IemGroup $Only 'updates') {
            foreach ($n in @($Profile.updates.services)) {
                # Stopped first, then disabled: undo runs newest first, so the start
                # type is restored before the service is started again.
                $items += New-IemItem -Key "svc:${n}:state" -Kind 'svc-state' -Arguments @{ name = $n } -Desired 'stopped' -Tier 2 -Group 'updates'
                $items += New-IemItem -Key "svc:${n}:start" -Kind 'svc-start' -Arguments @{ name = $n } -Desired 'disabled' -Tier 2 -Group 'updates'
            }
            foreach ($t in @($Profile.updates.tasks)) { $items += New-IemTaskItem -Task $t -Group 'updates' }
        }
        if (Select-IemGroup $Only 'maintenance') {
            if ($Profile.maintenance.off) {
                $items += New-IemItem -Key 'reg:maintenance-disabled' -Kind 'reg' -Tier 2 -Group 'maintenance' -Desired 1 `
                    -Arguments @{ path = (Get-IemRegPath $Profile 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\Maintenance'); name = 'MaintenanceDisabled'; type = 'DWord' }
            }
            foreach ($t in @($Profile.maintenance.tasks)) { $items += New-IemTaskItem -Task $t -Group 'maintenance' }
        }
        if (Select-IemGroup $Only 'defender') {
            foreach ($p in @($Profile.defender.paths)) { $items += New-IemItem -Key "defender:path:$p" -Kind 'defender-path' -Arguments @{ value = $p } -Desired 'present' -Tier 2 -Group 'defender' }
            foreach ($p in @($Profile.defender.processes)) { $items += New-IemItem -Key "defender:process:$p" -Kind 'defender-process' -Arguments @{ value = $p } -Desired 'present' -Tier 2 -Group 'defender' }
        }
    } elseif ($Tier -eq 3) {
        foreach ($d in @($Profile.devices)) {
            $wanted = (@($Only) -contains "irq:$($d.id)") -or ([bool]$d.enabled -and (Select-IemGroup $Only 'irq'))
            if (-not $wanted) { continue }
            if ($Check) { Assert-IemDevice -Profile $Profile -Device $d }
            $key = Get-IemRegPath $Profile "HKLM:\SYSTEM\CurrentControlSet\Enum\$($d.instance)\Device Parameters\Interrupt Management\Affinity Policy"
            $items += New-IemItem -Key "irq:$($d.id):policy" -Kind 'reg' -Arguments @{ path = $key; name = 'DevicePolicy'; type = 'DWord' } -Desired 4 -Tier 3 -Group "irq:$($d.id)" -Reboot
            $items += New-IemItem -Key "irq:$($d.id):mask" -Kind 'reg' -Arguments @{ path = $key; name = 'AssignmentSetOverride'; type = 'QWord' } -Desired (ConvertTo-IemMask @($d.lps)) -Tier 3 -Group "irq:$($d.id)" -Reboot
        }
        if (Select-IemGroup $Only 'nic') {
            $nk = Get-IemNicKey -Profile $Profile
            foreach ($p in $Profile.nic.properties.PSObject.Properties) {
                $items += New-IemItem -Key "nic:$($p.Name)" -Kind 'reg' -Arguments @{ path = $nk; name = $p.Name; type = 'String' } -Desired $p.Value -Tier 3 -Group 'nic' -Reboot
            }
            $items += New-IemItem -Key 'nic:*RssBaseProcNumber' -Kind 'reg' -Arguments @{ path = $nk; name = '*RssBaseProcNumber'; type = 'String' } -Desired $Profile.nic.rss.base -Tier 3 -Group 'nic' -Reboot
            $items += New-IemItem -Key 'nic:*RssMaxProcNumber' -Kind 'reg' -Arguments @{ path = $nk; name = '*RssMaxProcNumber'; type = 'String' } -Desired $Profile.nic.rss.max -Tier 3 -Group 'nic' -Reboot
            $items += New-IemItem -Key 'nic:PnPCapabilities' -Kind 'reg' -Arguments @{ path = $nk; name = 'PnPCapabilities'; type = 'DWord' } -Desired $Profile.nic.pnp_capabilities -Tier 3 -Group 'nic' -Reboot
        }
    } else { throw "tier $Tier refused (2 or 3)" }
    return ,$items
}

function New-IemTaskItem {
    param([Parameter(Mandatory)][string]$Task, [Parameter(Mandatory)][string]$Group)
    $i = $Task.LastIndexOf('\')
    New-IemItem -Key "task:$Task" -Kind 'task' -Arguments @{ path = $Task.Substring(0, $i + 1); name = $Task.Substring($i + 1) } -Desired 'disabled' -Tier 2 -Group $Group
}

function Get-IemModeItems {
    # Mode levers (design note §6.2): L2 plan, L3 governor, L6 services, L4 placement.
    param([Parameter(Mandatory)]$Profile, [string[]]$Only = @('plan', 'governor', 'placement'), [ValidateSet('default', 'c1', 'disable')][string]$Idle = 'default')
    $items = @()
    $guid = $Profile.plan.guid
    if (@($Only) -contains 'plan') {
        $items += New-IemItem -Key 'plan:exists' -Kind 'plan-exists' -Arguments @{ guid = $guid; source = $Profile.plan.source } -Desired 'present' -Group 'plan'
        $values = @($script:PlanSettings) + @(
            @{ name = 'idle-disable'; sub = $script:ProcessorSub; setting = $script:IdleDisable; value = $(if ($Idle -eq 'disable') { 1 } else { 0 }) },
            @{ name = 'idle-state-max'; sub = $script:ProcessorSub; setting = $script:IdleStateMax; value = $(if ($Idle -eq 'c1') { 1 } else { 0 }) })
        foreach ($s in $values) {
            $items += New-IemItem -Key "plan:$($s.name)" -Kind 'plan-value' -Arguments @{ guid = $guid; sub = $s.sub; setting = $s.setting } -Desired $s.value -Group 'plan'
        }
        $items += New-IemItem -Key 'plan:active' -Kind 'plan-active' -Arguments @{} -Desired $guid -Group 'plan'
    }
    if (@($Only) -contains 'governor') {
        $items += New-IemItem -Key 'governor' -Kind 'svc-state' -Arguments @{ name = $Profile.governor } -Desired 'stopped' -Group 'governor'
    }
    if (@($Only) -contains 'services') {
        foreach ($n in @($Profile.services_mode)) { $items += New-IemItem -Key "mode-svc:$n" -Kind 'svc-state' -Arguments @{ name = $n } -Desired 'stopped' -Group 'services' }
    }
    if (@($Only) -contains 'placement') {
        $map = [IemCpuSets]::Map()
        $ids = @(@($Profile.layout.housekeeping) | ForEach-Object { $map[[int]$_] }) | Sort-Object
        foreach ($name in @($Profile.placement)) {
            foreach ($p in @(Get-Process -Name $name -ErrorAction SilentlyContinue)) {
                $items += New-IemItem -Key "placement:${name}:$($p.Id)" -Kind 'cpusets' -Group 'placement' -Desired ($ids -join ',') `
                    -Arguments @{ pid = $p.Id; name = $p.ProcessName; start = $p.StartTime.ToUniversalTime().Ticks }
            }
        }
    }
    return ,$items
}

function Invoke-IemTuningApply {
    param([Parameter(Mandatory)][string]$ProfilePath, [Parameter(Mandatory)][ValidateSet(2, 3)][int]$Tier, [string[]]$Only = @())
    $profile = Read-IemProfile -Path $ProfilePath
    $j = Read-IemJournal -Path $profile.journal
    $boot = Get-IemBootTime
    $rows = @(foreach ($item in (Get-IemGlobalItems -Profile $profile -Tier $Tier -Only $Only -Check)) {
        Invoke-IemItem -Item $item -Journal $j -Section 'global' -Path $profile.journal -Boot $boot
    })
    $j.version = [int]$profile.version
    Write-IemJournal -Path $profile.journal -Journal $j
    return ,$rows
}

function Undo-IemTuning {
    param([Parameter(Mandatory)][string]$ProfilePath, [Parameter(Mandatory)][ValidateSet(2, 3)][int]$Tier, [string[]]$Only = @())
    $profile = Read-IemProfile -Path $ProfilePath
    $j = Read-IemJournal -Path $profile.journal
    $boot = Get-IemBootTime
    $keys = @($j.order.global); [array]::Reverse($keys)
    $rows = @()
    foreach ($k in $keys) {
        $e = $j.global[$k]
        if ($null -eq $e -or [int]$e.tier -ne $Tier) { continue }
        $g = [string]$e.group
        if (@($Only).Count -gt 0 -and -not ((@($Only) -contains $g) -or (@($Only) -contains ($g -replace ':.*$', '')))) { continue }
        try {
            $rows += Restore-IemItem -Item (ConvertTo-IemItem -Key $k -Entry $e)
            if ([bool]$e.reboot) { $j.reverted[$k] = $boot }
            $j.global.Remove($k)
            $j.order.global = @($j.order.global | Where-Object { $_ -ne $k })
            Write-IemJournal -Path $profile.journal -Journal $j
        } catch { $rows += [pscustomobject]@{ key = $k; action = 'failed'; value = $null; error = "$_" } }
    }
    return ,$rows
}

function Enter-IemTuningMode {
    param([Parameter(Mandatory)][string]$ProfilePath, [string[]]$Only = @('plan', 'governor', 'placement'),
          [ValidateSet('default', 'c1', 'disable')][string]$Idle = 'default')
    $profile = Read-IemProfile -Path $ProfilePath
    $j = Read-IemJournal -Path $profile.journal
    $j.entered = $true
    Write-IemJournal -Path $profile.journal -Journal $j   # before any write: an exit after a crash finds it
    $boot = Get-IemBootTime
    $rows = @(foreach ($item in (Get-IemModeItems -Profile $profile -Only $Only -Idle $Idle)) {
        Invoke-IemItem -Item $item -Journal $j -Section 'mode' -Path $profile.journal -Boot $boot
    })
    return ,$rows
}

function Exit-IemTuningMode {
    # Restores every mode item from the journal alone (newest first), carrying
    # on past failures; throws at the end when anything could not be restored.
    param([Parameter(Mandatory)][string]$ProfilePath)
    $profile = Read-IemProfile -Path $ProfilePath
    $j = Read-IemJournal -Path $profile.journal
    $keys = @($j.order.mode); [array]::Reverse($keys)
    $rows = @(); $failed = @()
    foreach ($k in $keys) {
        $e = $j.mode[$k]
        if ($null -eq $e) { continue }
        try {
            $rows += Restore-IemItem -Item (ConvertTo-IemItem -Key $k -Entry $e)
            $j.mode.Remove($k)
            $j.order.mode = @($j.order.mode | Where-Object { $_ -ne $k })
            Write-IemJournal -Path $profile.journal -Journal $j
        } catch { $failed += "${k}: $_" }
    }
    if ($failed.Count -gt 0) { throw ("tuning exit left $($failed.Count) item(s): " + ($failed -join '; ')) }
    $j.entered = $false
    Write-IemJournal -Path $profile.journal -Journal $j
    return ,$rows
}

function Get-IemTuningState {
    param([Parameter(Mandatory)][string]$ProfilePath)
    $profile = Read-IemProfile -Path $ProfilePath
    $j = Read-IemJournal -Path $profile.journal
    $boot = Get-IemBootTime
    $rows = @()
    foreach ($tier in 2, 3) {
        foreach ($item in (Get-IemGlobalItems -Profile $profile -Tier $tier)) {
            $e = $j.global[$item.key]
            $actual = try { Get-IemValue -Item $item } catch { "error: $_" }
            $rows += [pscustomobject]@{
                key = $item.key; tier = $tier; group = $item.group; desired = $item.desired; actual = $actual
                ok = (Test-IemSame $actual $item.desired); journaled = [bool]$e; before = $(if ($e) { $e.before } else { $null })
                pending = [bool]($e -and $item.reboot -and [string]$e.boot -eq $boot)
                revert_pending = [bool]($j.reverted.ContainsKey($item.key) -and [string]$j.reverted[$item.key] -eq $boot)
            }
        }
    }
    [pscustomobject]@{
        version = [int]$profile.version; applied_version = $j.version; boot = $boot
        drift = [bool]($j.global.Count -gt 0 -and $j.version -ne [int]$profile.version)
        entered = $j.entered; mode_items = @($j.order.mode); items = $rows
    }
}

function Get-IemFileDigest {
    # The file's SHA-256, or of its lines matching any key when keys are given.
    param([Parameter(Mandatory)][string]$Path, [string[]]$Keys = @())
    if (-not (Test-Path -LiteralPath $Path)) { return 'absent' }
    $lines = @(Get-Content -LiteralPath $Path)
    if (@($Keys).Count -gt 0) { $lines = @($lines | Where-Object { $l = $_; @($Keys | Where-Object { $l -match $_ }).Count -gt 0 }) }
    return Get-IemTextHash -Text ($lines -join "`n")
}

function Get-IemRegText {
    param([Parameter(Mandatory)]$Profile, [Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Name)
    Get-IemValue -Item (New-IemItem -Key 'r' -Kind 'reg' -Arguments @{ path = (Get-IemRegPath $Profile $Path); name = $Name; type = 'String' } -Desired $null)
}

function Get-IemReaperFingerprint {
    # Everything REAPER mode depends on (design note §5.1), read only.
    param([Parameter(Mandatory)][string]$ProfilePath)
    $profile = Read-IemProfile -Path $ProfilePath
    $f = [ordered]@{}
    $f['plan.active'] = [IemPower]::Active()
    $f['plan.reaper.settings'] = Get-IemTextHash -Text ((@(& powercfg.exe /qh $profile.plan.source)) -join "`n")
    $gov = Get-Service -Name $profile.governor -ErrorAction SilentlyContinue
    $f['governor.state'] = $(if ($gov) { "$($gov.Status)" } else { 'absent' })
    $f['governor.start'] = Get-IemValue -Item (New-IemItem -Key 'g' -Kind 'svc-start' -Arguments @{ name = $profile.governor } -Desired $null)
    $n = 0
    foreach ($file in @($profile.fingerprint.files)) { $n++; $f["file.$n"] = Get-IemFileDigest -Path $file -Keys @($profile.fingerprint.keys) }
    $r = @(Get-Process -Name reaper -ErrorAction SilentlyContinue)
    if ($r.Count -eq 1) {
        $f['reaper.priority'] = "$($r[0].PriorityClass)"
        $f['reaper.affinity'] = "$([long]$r[0].ProcessorAffinity)"
        $f['reaper.cpusets'] = ((@([IemCpuSets]::Get($r[0].Id)) | Sort-Object) -join ',')
    } else { $f['reaper.priority'] = "instances=$($r.Count)" }
    $mm = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile'
    foreach ($v in 'SystemResponsiveness', 'NetworkThrottlingIndex') { $f["mmcss.$v"] = Get-IemRegText $profile $mm $v }
    foreach ($v in 'Affinity', 'Background Only', 'Clock Rate', 'GPU Priority', 'Priority', 'Scheduling Category', 'SFIO Priority') {
        $f["mmcss.proaudio.$v"] = Get-IemRegText $profile "$mm\Tasks\Pro Audio" $v
    }
    $f['kernel.ReservedCpuSets'] = Get-IemRegText $profile 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\kernel' 'ReservedCpuSets'
    $f['bcd'] = Get-IemTextHash -Text ((@(& bcdedit.exe /enum '{current}')) -join "`n")
    $dg = Get-CimInstance -Namespace root\Microsoft\Windows\DeviceGuard -ClassName Win32_DeviceGuard -ErrorAction SilentlyContinue
    $f['deviceguard.running'] = $(if ($dg) { (@($dg.SecurityServicesRunning) -join ',') } else { 'unavailable' })
    $f['tuning.entered'] = "$((Read-IemJournal -Path $profile.journal).entered)"
    return [pscustomobject]$f
}

function Compare-IemFingerprint {
    param([Parameter(Mandatory)]$Baseline, [Parameter(Mandatory)]$Current)
    $names = @(@($Baseline.PSObject.Properties.Name) + @($Current.PSObject.Properties.Name) | Sort-Object -Unique)
    $diff = @()
    foreach ($n in $names) {
        $a = $Baseline.PSObject.Properties[$n]; $b = $Current.PSObject.Properties[$n]
        $va = $(if ($a) { [string]$a.Value } else { '<absent>' }); $vb = $(if ($b) { [string]$b.Value } else { '<absent>' })
        if ($va -ne $vb) { $diff += [pscustomobject]@{ key = $n; baseline = $va; current = $vb } }
    }
    return ,$diff
}

function Get-IemDeviceInventory {
    # PCI devices: driver, MSI and affinity registry values, allocated IRQs
    # (a negative IRQ number is an MSI).
    $irq = @{}
    foreach ($r in @(Get-CimInstance -ClassName Win32_PnPAllocatedResource -ErrorAction SilentlyContinue)) {
        if ($r.Antecedent.CimSystemProperties.ClassName -eq 'Win32_IRQResource') {
            $id = [string]$r.Dependent.DeviceID
            $n = [BitConverter]::ToInt32([BitConverter]::GetBytes([uint32]$r.Antecedent.IRQNumber), 0)
            $irq[$id] = @($irq[$id] | Where-Object { $null -ne $_ }) + [string]$n
        }
    }
    foreach ($d in @(Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -like 'PCI\*' })) {
        $enum = "HKLM:\SYSTEM\CurrentControlSet\Enum\$($d.InstanceId)\Device Parameters\Interrupt Management"
        $read = { param($k, $n) if (Test-Path -LiteralPath $k) { (Get-Item -LiteralPath $k).GetValue($n, $null) } }
        $ver = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverVersion' -ErrorAction SilentlyContinue).Data
        $date = (Get-PnpDeviceProperty -InstanceId $d.InstanceId -KeyName 'DEVPKEY_Device_DriverDate' -ErrorAction SilentlyContinue).Data
        [ordered]@{
            instance = $d.InstanceId; name = $d.FriendlyName; class = $d.Class; status = "$($d.Status)"; driver = $ver; driver_date = "$date"
            msi = & $read "$enum\MessageSignaledInterruptProperties" 'MSISupported'
            msi_limit = & $read "$enum\MessageSignaledInterruptProperties" 'MessageNumberLimit'
            policy = & $read "$enum\Affinity Policy" 'DevicePolicy'
            mask = & $read "$enum\Affinity Policy" 'AssignmentSetOverride'
            irqs = @($irq[$d.InstanceId] | Where-Object { $null -ne $_ })
        }
    }
}

function Get-IemInventory {
    # Inventory M0 (design note §4.2), read only. Never reads process command
    # lines, service image paths or task actions: they can carry tokens.
    param([Parameter(Mandatory)][string]$ProfilePath)
    $profile = Read-IemProfile -Path $ProfilePath
    $os = Get-CimInstance -ClassName Win32_OperatingSystem
    $bios = Get-CimInstance -ClassName Win32_BIOS
    $map = [IemCpuSets]::Map()
    $tpm = try { Get-Tpm | Select-Object TpmPresent, TpmReady, ManufacturerIdTxt, ManufacturerVersion } catch { "$_" }
    $dg = Get-CimInstance -Namespace root\Microsoft\Windows\DeviceGuard -ClassName Win32_DeviceGuard -ErrorAction SilentlyContinue
    $defender = try { $p = Get-MpPreference; [ordered]@{ exclusion_paths = @($p.ExclusionPath); exclusion_processes = @($p.ExclusionProcess)
                                                         scan_day = $p.ScanScheduleDay; realtime_off = $p.DisableRealtimeMonitoring } } catch { "$_" }
    $since = (Get-Date).AddDays(-365)
    $cpusets = [ordered]@{}   # ConvertTo-Json needs string keys
    foreach ($k in ($map.Keys | Sort-Object)) { $cpusets["$k"] = $map[$k] }
    [ordered]@{
        at = (Get-Date).ToUniversalTime().ToString('o')
        os = [ordered]@{ caption = $os.Caption; version = $os.Version; build = $os.BuildNumber; boot = $os.LastBootUpTime.ToUniversalTime().ToString('o') }
        bios = [ordered]@{ vendor = $bios.Manufacturer; version = $bios.SMBIOSBIOSVersion; date = "$($bios.ReleaseDate)" }
        cpu = @(Get-CimInstance -ClassName Win32_Processor | ForEach-Object { [ordered]@{ name = $_.Name; cores = $_.NumberOfCores; logical = $_.NumberOfLogicalProcessors } })
        cpusets = $cpusets
        tpm = $tpm
        deviceguard = $(if ($dg) { [ordered]@{ vbs = $dg.VirtualizationBasedSecurityStatus; running = @($dg.SecurityServicesRunning) } } else { 'unavailable' })
        bcd = @(& bcdedit.exe /enum '{current}')
        timer_100ns = [IemTimer]::Query()
        power = [ordered]@{ active = [IemPower]::Active(); list = @(& powercfg.exe /list); active_settings = @(& powercfg.exe /qh) }
        devices = @(Get-IemDeviceInventory)
        nics = @(Get-NetAdapter | ForEach-Object {
            [ordered]@{ name = $_.Name; description = $_.InterfaceDescription; status = "$($_.Status)"; speed = "$($_.LinkSpeed)"; driver = $_.DriverVersion
                        advanced = @(Get-NetAdapterAdvancedProperty -Name $_.Name -ErrorAction SilentlyContinue | ForEach-Object { [ordered]@{ keyword = $_.RegistryKeyword; value = "$($_.RegistryValue)"; display = $_.DisplayName } })
                        rss = (Get-NetAdapterRss -Name $_.Name -ErrorAction SilentlyContinue | Select-Object Enabled, BaseProcessorNumber, MaxProcessorNumber, MaxProcessors, NumberOfReceiveQueues)
                        pm = (Get-NetAdapterPowerManagement -Name $_.Name -ErrorAction SilentlyContinue | Select-Object AllowComputerToTurnOffDevice) } })
        services = @(Get-CimInstance -ClassName Win32_Service | ForEach-Object { [ordered]@{ name = $_.Name; start = $_.StartMode; state = $_.State } })
        tasks = @(Get-ScheduledTask | Where-Object { "$($_.State)" -ne 'Disabled' } | ForEach-Object {
            $i = $_ | Get-ScheduledTaskInfo -ErrorAction SilentlyContinue
            [ordered]@{ path = $_.TaskPath; name = $_.TaskName; state = "$($_.State)"; last = $(if ($i) { "$($i.LastRunTime)" } else { '' }) } })
        defender = $defender
        processes = @(Get-Process | ForEach-Object {
            $pc = try { "$($_.PriorityClass)" } catch { 'denied' }
            $af = try { "$([long]$_.ProcessorAffinity)" } catch { 'denied' }
            [ordered]@{ name = $_.ProcessName; id = $_.Id; session = $_.SessionId; priority = $pc; affinity = $af } })
        governor_lines = @(foreach ($file in @($profile.fingerprint.files)) { if (Test-Path -LiteralPath $file) {
            @(Get-Content -LiteralPath $file | Where-Object { $_ -match 'IdleSaver|ProBalance|Gaming|Performance|PowerPlan|Priorit|Affinit|CpuSet|SmartTrim|Exclu' }) } })
        mmcss = @(Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile' |
                  Select-Object SystemResponsiveness, NetworkThrottlingIndex)
        history = [ordered]@{
            hotfixes = @(Get-HotFix | ForEach-Object { [ordered]@{ id = $_.HotFixID; installed = "$($_.InstalledOn)" } })
            drivers = @(Get-CimInstance -ClassName Win32_PnPSignedDriver | Where-Object { $_.DriverDate } | ForEach-Object { [ordered]@{ device = $_.DeviceName; version = $_.DriverVersion; date = "$($_.DriverDate)" } })
            services_installed = @(Get-WinEvent -FilterHashtable @{ LogName = 'System'; Id = 7045; StartTime = $since } -ErrorAction SilentlyContinue | ForEach-Object { [ordered]@{ at = $_.TimeCreated.ToUniversalTime().ToString('o'); service = "$($_.Properties[0].Value)" } })
        }
    }
}

Export-ModuleMember -Function *-Iem*
```

- [ ] **Step 2: The integrity scan passes** (no refused words), then commit.

```bash
cd "$WORK" && python3 scripts/check_integrity.py
git add scripts/pc-tuning/IemTuning.psm1
git commit -m "feat(s1c): PC tuning module (items, journal, tiers, mode levers, fingerprint, inventory)

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: `IemMeasure.psm1` and the spike's PC arguments

**Files:**
- Create: `scripts/pc-tuning/IemMeasure.psm1`
- Modify: `scripts/asio-spike/SpikePc.psm1` (`New-SpikeArguments`)

**Interfaces:**
- **Produces (PowerShell):**
  - `New-IemTraceArguments -Dir [-CSwitch] [-CircularMB n]` → string[];
  - `Start-IemTrace -Xperf -Dir [-CSwitch] [-CircularMB n]` and `Stop-IemTrace -Xperf -Dir [-Merge] [-Name trace.etl]` → `{stopped: [...]}`;
  - `ConvertFrom-IemLoggers -Text` → string[] session names;
  - `Invoke-IemDpcIsr -Xperf -Dir [-Name trace.etl]` → the report path `dpcisr.txt` (or `<name>.dpcisr.txt` for cuts);
  - `Export-IemNearGlitch -Xperf -Dir` → `{lines, path}`;
  - `Get-IemCpuSample` → `{cpus: [{lp, t100ns, interrupts, dpcs, dpc_time, int_time, idle_time, c1_time, c2_time, c3_time}], freq: [{name, mhz, perf_pct}]}`;
  - `Get-IemPollSample -ProfilePath [-SpikePid n -ThreadId n]` → `{cpu, plan, governor, thread: {base, current}|null}`;
  - `Get-IemSystemEvents -Since iso` → `[{provider, id, count}]`;
  - `Install-IemWpt -Setup -Xperf` → `{installed, version}`;
  - `Get-IemNow` → ISO UTC.
- **Spike requests** accept `audio_cpus`, `stress_cpus`, `cpu` and `threshold_us`, and the mode `hwlat`.

- [ ] **Step 1: Create `IemMeasure.psm1`.**

```powershell
#Requires -Version 5.1
# S1c measurement on the PC (docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md §4.1):
# the xperf kernel trace with the spike's glitch markers, the dpcisr and
# dumper reports, per-CPU counter samples over WMI (language-neutral), the
# System log and the WPT install. Changes no Windows setting.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'IemTuning.psm1') -Force -Global
# crates/iem-audio-io/src/os.rs MARKER_PROVIDER.
$script:MarkerProvider = '3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11'
$script:MarkerSession = 'IemMarkers'
$script:KernelSession = 'NT Kernel Logger'
$script:NearEvents = @('DPC', 'TimedDPC', 'ThreadedDPC', 'Interrupt', 'CSwitch', 'ReadyThread')

function Get-IemNow { (Get-Date).ToUniversalTime().ToString('o') }

function New-IemTraceArguments {
    param([Parameter(Mandatory)][string]$Dir, [switch]$CSwitch, [int]$CircularMB = 0)
    $flags = 'PROC_THREAD+LOADER+DPC+INTERRUPT'
    if ($CSwitch) { $flags += '+CSWITCH+DISPATCHER' }
    $a = @('-on', $flags, '-BufferSize', '1024', '-MinBuffers', '256', '-MaxBuffers', '1024')
    if ($CircularMB -gt 0) { $a += @('-FileMode', 'Circular', '-MaxFile', "$CircularMB") }
    $a += @('-f', (Join-Path $Dir 'kernel.etl'), '-start', $script:MarkerSession, '-on', $script:MarkerProvider, '-f', (Join-Path $Dir 'markers.etl'))
    return ,$a
}

function Invoke-IemXperf {
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string[]]$Arguments)
    if (-not (Test-Path -LiteralPath $Xperf)) { throw "xperf not found at $Xperf (run wpt-install)" }
    $out = & $Xperf @Arguments 2>&1
    if ($LASTEXITCODE -ne 0) { throw "xperf $($Arguments -join ' ') (exit $LASTEXITCODE): $($out -join ' ')" }
    return ,@($out | ForEach-Object { "$_" })
}

function ConvertFrom-IemLoggers {
    # Session names from `xperf -Loggers` ("Logger Name : <name>" lines).
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text)
    return ,@($Text -split "`n" | ForEach-Object { if ($_ -match '^\s*Logger Name\s*:\s*(.+?)\s*$') { $Matches[1] } })
}

function Start-IemTrace {
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir, [switch]$CSwitch, [int]$CircularMB = 0)
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments (New-IemTraceArguments -Dir $Dir -CSwitch:$CSwitch -CircularMB $CircularMB))
    [pscustomobject]@{ dir = $Dir; started = Get-IemNow }
}

function Stop-IemTrace {
    # Stops whichever of the two sessions runs (none is fine: pre-emption may
    # come twice). -Merge merges both into -Name; without it the raw files stay.
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir, [switch]$Merge, [string]$Name = 'trace.etl')
    $running = ConvertFrom-IemLoggers -Text ((Invoke-IemXperf -Xperf $Xperf -Arguments @('-Loggers')) -join "`n")
    $a = @()
    if ($running -contains $script:KernelSession) { $a += '-stop' }
    if ($running -contains $script:MarkerSession) { $a += @('-stop', $script:MarkerSession) }
    if ($a.Count -eq 0) { return [pscustomobject]@{ stopped = @() } }
    if ($Merge) { $a += @('-d', (Join-Path $Dir $Name)) }
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments $a)
    [pscustomobject]@{ stopped = @($running | Where-Object { @($script:KernelSession, $script:MarkerSession) -contains $_ }) }
}

function Invoke-IemDpcIsr {
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir, [string]$Name = 'trace.etl')
    $out = Join-Path $Dir $(if ($Name -eq 'trace.etl') { 'dpcisr.txt' } else { [IO.Path]::GetFileNameWithoutExtension($Name) + '.dpcisr.txt' })
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments @('-i', (Join-Path $Dir $Name), '-o', $out, '-a', 'dpcisr'))
    return $out
}

function Export-IemNearGlitch {
    # The dumper's header plus the DPC/ISR/context-switch rows and the glitch
    # markers, streamed into near.txt; the full dump is deleted.
    param([Parameter(Mandatory)][string]$Xperf, [Parameter(Mandatory)][string]$Dir)
    $dump = Join-Path $Dir 'dumper.txt'; $near = Join-Path $Dir 'near.txt'
    [void](Invoke-IemXperf -Xperf $Xperf -Arguments @('-i', (Join-Path $Dir 'trace.etl'), '-o', $dump, '-a', 'dumper'))
    $r = New-Object IO.StreamReader($dump); $w = New-Object IO.StreamWriter($near, $false, (New-Object Text.UTF8Encoding $false))
    $n = 0; $header = $false
    try {
        while ($null -ne ($line = $r.ReadLine())) {
            $t = $line.Trim()
            if ($t -eq 'BeginHeader') { $header = $true }
            $first = ($t -split ',', 2)[0].Trim()
            if ($header -or $script:NearEvents -contains $first -or $line.Contains('iemmixer-glitch') -or $line.Contains($script:MarkerProvider)) { $w.WriteLine($line); $n++ }
            if ($t -eq 'EndHeader') { $header = $false }
        }
    } finally { $r.Close(); $w.Close() }
    Remove-Item -LiteralPath $dump
    [pscustomobject]@{ lines = $n; path = $near }
}

function Get-IemCpuSample {
    # Raw cumulative per-CPU counters (rates are computed on the dev box).
    $raw = @(Get-CimInstance -ClassName Win32_PerfRawData_PerfOS_Processor | Where-Object { $_.Name -match '^\d+$' })
    $fmt = @(Get-CimInstance -ClassName Win32_PerfFormattedData_Counters_ProcessorInformation -ErrorAction SilentlyContinue | Where-Object { $_.Name -notmatch '_Total' })
    [pscustomobject]@{
        cpus = @($raw | ForEach-Object { [pscustomobject]@{
            lp = [int]$_.Name; t100ns = [uint64]$_.Timestamp_Sys100NS; interrupts = [uint64]$_.InterruptsPersec; dpcs = [uint64]$_.DPCsQueuedPersec
            dpc_time = [uint64]$_.PercentDPCTime; int_time = [uint64]$_.PercentInterruptTime; idle_time = [uint64]$_.PercentIdleTime
            c1_time = [uint64]$_.PercentC1Time; c2_time = [uint64]$_.PercentC2Time; c3_time = [uint64]$_.PercentC3Time } })
        freq = @($fmt | ForEach-Object { [pscustomobject]@{ name = $_.Name; mhz = [uint32]$_.ProcessorFrequency; perf_pct = [uint32]$_.PercentProcessorPerformance } })
    }
}

function Get-IemPollSample {
    # One sentinel sample (design note §4.1 item 5): CPU counters, the active
    # plan, the governor's state and, while a spike runs, the priority of its
    # callback thread (read from outside; the driver's thread is never touched).
    param([Parameter(Mandatory)][string]$ProfilePath, [int]$SpikePid = 0, [int]$ThreadId = 0)
    $profile = Read-IemProfile -Path $ProfilePath
    $gov = Get-Service -Name $profile.governor -ErrorAction SilentlyContinue
    $thread = $null
    if ($SpikePid -gt 0 -and $ThreadId -gt 0) {
        $p = Get-Process -Id $SpikePid -ErrorAction SilentlyContinue
        if ($p) { $t = @($p.Threads | Where-Object { $_.Id -eq $ThreadId }); if ($t.Count -eq 1) { $thread = [pscustomobject]@{ base = $t[0].BasePriority; current = $t[0].CurrentPriority } } }
    }
    [pscustomobject]@{ at = Get-IemNow; cpu = Get-IemCpuSample; plan = [IemPower]::Active(); governor = $(if ($gov) { "$($gov.Status)" } else { 'absent' }); thread = $thread }
}

function Get-IemSystemEvents {
    # Warnings and errors of the System log since -Since, by provider and id
    # (no message text: it can name hosts).
    param([Parameter(Mandatory)][string]$Since)
    $start = [datetime]::Parse($Since, [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::RoundtripKind).ToLocalTime()
    $ev = @(Get-WinEvent -FilterHashtable @{ LogName = 'System'; Level = 1, 2, 3; StartTime = $start } -ErrorAction SilentlyContinue)
    return ,@($ev | Group-Object -Property ProviderName, Id | ForEach-Object { [pscustomobject]@{ provider = $_.Group[0].ProviderName; id = $_.Group[0].Id; count = $_.Count } })
}

function Install-IemWpt {
    # The ADK bootstrapper (Microsoft-signed) installs only the Windows
    # Performance Toolkit; no reboot, no service, no driver.
    param([Parameter(Mandatory)][string]$Setup, [Parameter(Mandatory)][string]$Xperf)
    if (Test-Path -LiteralPath $Xperf) { return [pscustomobject]@{ installed = 'already'; version = (Get-Item -LiteralPath $Xperf).VersionInfo.FileVersion } }
    $sig = Get-AuthenticodeSignature -LiteralPath $Setup
    if ($sig.Status -ne 'Valid' -or "$($sig.SignerCertificate.Subject)" -notlike '*O=Microsoft Corporation*') {
        throw "adksetup signature: $($sig.Status) $($sig.SignerCertificate.Subject)"
    }
    $p = Start-Process -FilePath $Setup -ArgumentList '/quiet', '/norestart', '/ceip', 'off', '/features', 'OptionId.WindowsPerformanceToolkit' -PassThru -Wait
    if ($p.ExitCode -ne 0) { throw "adksetup exit $($p.ExitCode)" }
    if (-not (Test-Path -LiteralPath $Xperf)) { throw "xperf not found at $Xperf after the install" }
    [pscustomobject]@{ installed = 'now'; version = (Get-Item -LiteralPath $Xperf).VersionInfo.FileVersion }
}

Export-ModuleMember -Function *-Iem*
```

- [ ] **Step 2: Spike arguments.** In `SpikePc.psm1`:
  1. Add before `New-SpikeArguments`:

```powershell
function Get-SpikeCpuArguments {
    # --audio-cpus / --stress-cpus from a request (absent or empty = none), S1c.
    param([Parameter(Mandatory)]$Request)
    $a = @()
    foreach ($pair in @(@('audio_cpus', '--audio-cpus'), @('stress_cpus', '--stress-cpus'))) {
        $p = $Request.PSObject.Properties[$pair[0]]
        if ($p -and "$($p.Value)") {
            if ("$($p.Value)" -notmatch '^[0-9]+(-[0-9]+)?(,[0-9]+(-[0-9]+)?)*$') { throw "$($pair[0]) '$($p.Value)' refused" }
            $a += @($pair[1], "$($p.Value)")
        }
    }
    return ,$a
}
```

  2. In `New-SpikeArguments`, `duplex` branch, after the existing `$a += @('--frames', ...)` line, add `$cpu = Get-SpikeCpuArguments -Request $Request; $a += $cpu`.
  3. Add a branch before `default`:

```powershell
        'hwlat' {
            $cpu = [int]$Request.cpu; $thr = [int]$Request.threshold_us
            if ($cpu -lt 0 -or $cpu -gt 63) { throw "cpu $cpu refused" }
            if ($thr -lt 1 -or $thr -gt 1000) { throw "threshold $thr refused" }
            $a += @('--cpu', $cpu, '--seconds', [int]$Request.seconds, '--threshold-us', $thr)
        }
```

- [ ] **Step 3: Scan and commit.**

```bash
cd "$WORK" && python3 scripts/check_integrity.py
git add scripts/pc-tuning/IemMeasure.psm1 scripts/asio-spike/SpikePc.psm1
git commit -m "feat(s1c): PC measurement module (xperf, dpcisr, dumper filter, CPU samples, System log, WPT); spike hwlat and CPU Set arguments

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: PC self-tests on real backends

**Files:**
- Create: `scripts/pc-tuning/Test-IemTuning.ps1`
- Modify: `scripts/asio-spike/Test-SpikePc.ps1`

**Interfaces:** consumes Tasks 5 and 6. Runs in CI `asio-spike` (hosted `windows-2025`, an ephemeral administrator runner) on Windows PowerShell 5.1.

- [ ] **Step 1: Create `Test-IemTuning.ps1`.**

```powershell
#Requires -Version 5.1
# Self-test of the S1c tuning modules on Windows PowerShell 5.1 (CI job asio-spike,
# an ephemeral administrator runner): real backends — registry values under an
# HKCU test root, two services (Spooler: no start triggers, for the disable-and-stop
# case; W32Time as the governor stand-in), a scheduled task, a duplicated power plan,
# Defender exclusions and the CPU Sets of child processes.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
foreach ($f in (Get-ChildItem -LiteralPath $here -File | Where-Object { @('.ps1', '.psm1') -contains $_.Extension })) {
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "parse errors in $($f.Name): $($errors[0].Message)" }
}
Import-Module (Join-Path $here 'IemMeasure.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" }; Write-Host "ok  $what" }
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }
function Rows($rows, $action) { @($rows | Where-Object { $_.action -eq $action }) }
# Read-IemJournal is exported (every *-Iem* function is); the test reads the flag the module wrote.
function Read-IemJournalState($profilePath) { $p = Read-IemProfile -Path $profilePath; (Read-IemJournal -Path $p.journal).entered }

$id = [guid]::NewGuid().ToString('N')
$root = "HKCU:\Software\iemmixer-tuning-test-$id"
$dir = Join-Path ([IO.Path]::GetTempPath()) "tuning-test-$id"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
foreach ($s in 'Spooler', 'W32Time') {
    $svc = Get-Service -Name $s   # both exist on the runner; a missing one fails the test
    if ($svc.Status -ne 'Running') { Start-Service -InputObject $svc; $svc.WaitForStatus('Running', [TimeSpan]::FromSeconds(60)) }
}
$spoolStart = (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start')
$activeBefore = [IemPower]::Active()
$testPlan = [guid]::NewGuid().ToString()
$taskPath = '\iemmixer-test\'; $taskName = "t-$id"
Register-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Action (New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0') | Out-Null
$enum = "$root\HKLM\SYSTEM\CurrentControlSet\Enum\PCI\VEN_TEST&DEV_0001\0"
New-Item -Path $enum -Force | Out-Null
New-ItemProperty -LiteralPath $enum -Name 'HardwareID' -PropertyType MultiString -Value @('PCI\VEN_TEST&DEV_0001&SUBSYS_1', 'PCI\VEN_TEST&DEV_0001') | Out-Null
$nic = "$root\HKLM\NIC"
New-Item -Path $nic -Force | Out-Null
New-ItemProperty -LiteralPath $nic -Name 'PowerSaving' -PropertyType String -Value '1' | Out-Null
$mm = "$root\HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Multimedia\SystemProfile"
New-Item -Path "$mm\Tasks\Pro Audio" -Force | Out-Null
New-ItemProperty -LiteralPath $mm -Name 'SystemResponsiveness' -PropertyType DWord -Value 0 | Out-Null
$ping = "$env:SystemRoot\System32\PING.EXE"
$child = Start-Process -FilePath $ping -ArgumentList '-n', '240', '127.0.0.1' -PassThru -WindowStyle Hidden

function New-TestProfile([string]$Hwid) {
    $p = [ordered]@{
        version = 1; journal = (Join-Path $dir 'journal.json'); registry_root = $root
        layout = [ordered]@{ housekeeping = @(0); card = @(0); nic = @(0); audio = @(0) }
        plan = [ordered]@{ guid = $testPlan; source = $activeBefore }
        governor = 'W32Time'; placement = @('PING'); services_disable = @('Spooler'); services_mode = @()
        updates = [ordered]@{ services = @(); tasks = @() }
        maintenance = [ordered]@{ off = $true; tasks = @("$taskPath$taskName", '\iemmixer-test\no-such-task') }
        defender = [ordered]@{ paths = @($dir); processes = @() }
        devices = @([ordered]@{ id = 'card'; instance = 'PCI\VEN_TEST&DEV_0001\0'; hwid = $Hwid; lps = @(0, 2); enabled = $true })
        nic = [ordered]@{ adapter = 'unused'; key = 'HKLM:\NIC'; properties = [ordered]@{ PowerSaving = '0' }; rss = [ordered]@{ base = 4; max = 5 }; pnp_capabilities = 24 }
        fingerprint = [ordered]@{ files = @(); keys = @() }
    }
    $path = Join-Path $dir "profile-$([guid]::NewGuid().ToString('N')).json"
    [IO.File]::WriteAllText($path, ($p | ConvertTo-Json -Depth 6))
    return $path
}
$pp = New-TestProfile 'PCI\VEN_TEST&DEV_0001'
$maint = "$root\HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\Maintenance"

try {
    # Tier 2: services, a task (plus an absent one), the maintenance switch, a Defender exclusion.
    $r = Invoke-IemTuningApply -ProfilePath $pp -Tier 2
    Assert ((Rows $r 'failed').Count -eq 0) "tier2-apply-has-no-failure ($(@(Rows $r 'failed') | ForEach-Object { $_.error }))"
    Assert ((Get-Service Spooler).Status -eq 'Stopped' -and (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start') -eq 4) 'tier2-service-disabled-and-stopped'
    Assert ("$((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State)" -eq 'Disabled') 'tier2-task-disabled'
    Assert ((Rows $r 'absent').Count -eq 1) 'tier2-a-missing-task-is-absent-not-an-error'
    Assert ((Get-Item -LiteralPath $maint).GetValue('MaintenanceDisabled') -eq 1) 'tier2-maintenance-off'
    Assert (@((Get-MpPreference).ExclusionPath) -contains $dir) 'tier2-defender-exclusion'
    $again = Invoke-IemTuningApply -ProfilePath $pp -Tier 2
    Assert ((Rows $again 'written').Count -eq 0 -and (Rows $again 'failed').Count -eq 0) 'tier2-apply-is-idempotent'
    $u = Undo-IemTuning -ProfilePath $pp -Tier 2
    Assert ((Rows $u 'failed').Count -eq 0) 'tier2-undo-has-no-failure'
    Assert ((Get-Service Spooler).Status -eq 'Running' -and (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Services\Spooler').GetValue('Start') -eq $spoolStart) 'tier2-undo-restores-the-original'
    Assert ("$((Get-ScheduledTask -TaskPath $taskPath -TaskName $taskName).State)" -ne 'Disabled') 'tier2-undo-enables-the-task'
    Assert ($null -eq (Get-Item -LiteralPath $maint).GetValue('MaintenanceDisabled', $null)) 'tier2-undo-deletes-absent-values'
    Assert (-not (@((Get-MpPreference).ExclusionPath) -contains $dir)) 'tier2-undo-removes-the-exclusion'

    # Tier 3: affinity policy under the device's key, NIC values; pending until a reboot.
    $bad = New-TestProfile 'PCI\VEN_OTHER'
    Throws { Invoke-IemTuningApply -ProfilePath $bad -Tier 3 -Only @('irq') } 'tier3-refuses-a-mismatched-device'
    Assert (-not (Test-Path -LiteralPath "$enum\Device Parameters")) 'tier3-refusal-writes-nothing'
    $r3 = Invoke-IemTuningApply -ProfilePath $pp -Tier 3
    Assert ((Rows $r3 'failed').Count -eq 0) 'tier3-apply-has-no-failure'
    $ap = Get-Item -LiteralPath "$enum\Device Parameters\Interrupt Management\Affinity Policy"
    Assert ($ap.GetValue('DevicePolicy') -eq 4 -and $ap.GetValue('AssignmentSetOverride') -eq 5) 'tier3-affinity-policy-and-mask'
    Assert ((Get-Item -LiteralPath $nic).GetValue('PowerSaving') -eq '0' -and (Get-Item -LiteralPath $nic).GetValue('*RssBaseProcNumber') -eq '4') 'tier3-nic-values'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.tier -eq 3 -and -not $_.pending }).Count -eq 0) 'tier3-items-are-pending-until-a-reboot'
    $u3 = Undo-IemTuning -ProfilePath $pp -Tier 3
    Assert ((Rows $u3 'failed').Count -eq 0) 'tier3-undo-has-no-failure'
    Assert ($null -eq $ap.GetValue('DevicePolicy', $null) -and (Get-Item -LiteralPath $nic).GetValue('PowerSaving') -eq '1') 'tier3-undo-deletes-absent-values'
    $st = Get-IemTuningState -ProfilePath $pp
    Assert (@($st.items | Where-Object { $_.key -eq 'irq:card:policy' -and $_.revert_pending }).Count -eq 1) 'tier3-undo-is-pending-until-a-reboot'

    # Mode levers: plan (C1 only), governor stand-in, placement.
    $e = Enter-IemTuningMode -ProfilePath $pp -Only @('plan', 'governor', 'placement') -Idle 'c1'
    Assert ((Rows $e 'failed').Count -eq 0) "enter-has-no-failure ($(@(Rows $e 'failed') | ForEach-Object { $_.key + ': ' + $_.error }))"
    Assert ([IemPower]::Active() -eq $testPlan) 'enter-activates-the-plan'
    Assert ([IemPower]::Read($testPlan, '54533251-82be-4824-96c1-47b60b740d00', '9943e905-9a30-4ec1-9b99-44dd3b76f7a2') -eq 1) 'enter-limits-idle-to-c1'
    Assert ([IemPower]::Read($testPlan, '54533251-82be-4824-96c1-47b60b740d00', '893dee8e-2bef-41e0-89c6-b55d0929964c') -eq 100) 'enter-sets-processor-min-100'
    Assert ((Get-Service W32Time).Status -eq 'Stopped') 'enter-pauses-the-governor'
    $hk = [IemCpuSets]::Map()[0]
    Assert ((@([IemCpuSets]::Get($child.Id)) -join ',') -eq "$hk") 'enter-places-the-process'
    $e2 = Enter-IemTuningMode -ProfilePath $pp -Only @('plan', 'governor', 'placement') -Idle 'c1'
    Assert ((Rows $e2 'written').Count -eq 0) 'enter-is-idempotent'
    # A new session restores from the journal alone.
    Remove-Module IemMeasure, IemTuning
    Import-Module (Join-Path $here 'IemMeasure.psm1') -Force
    $x = Exit-IemTuningMode -ProfilePath $pp
    Assert ([IemPower]::Active() -eq $activeBefore) 'exit-from-journal-in-a-new-session'
    Assert (-not (@(& powercfg.exe /list) -match $testPlan)) 'exit-deletes-the-plan'
    Assert ((Get-Service W32Time).Status -eq 'Running') 'exit-restarts-the-governor'
    Assert ((@([IemCpuSets]::Get($child.Id)) -join ',') -eq '') 'exit-clears-the-placement'
    Assert (-not (Read-IemJournalState $pp)) 'exit-clears-entered'
    Assert ((Exit-IemTuningMode -ProfilePath $pp).Count -eq 0) 'exit-twice-is-harmless'

    # A placed process that ended is skipped; a reused pid is refused.
    $short = Start-Process -FilePath $ping -ArgumentList '-n', '2', '127.0.0.1' -PassThru -WindowStyle Hidden
    [void](Enter-IemTuningMode -ProfilePath $pp -Only @('placement'))
    $short.WaitForExit(10000) | Out-Null
    $x = Exit-IemTuningMode -ProfilePath $pp
    Assert ((Rows $x 'gone').Count -ge 1) 'exit-skips-a-process-that-ended'
    Throws { Set-IemValue -Item ([pscustomobject]@{ key = 'k'; kind = 'cpusets'; args = @{ pid = $child.Id; name = 'PING'; start = 1 } }) -Value '' } 'cpusets-refuse-a-reused-pid'

    # Fingerprint: stable, and a change is named.
    $f1 = Get-IemReaperFingerprint -ProfilePath $pp
    Assert ((Compare-IemFingerprint -Baseline $f1 -Current (Get-IemReaperFingerprint -ProfilePath $pp)).Count -eq 0) 'fingerprint-is-stable'
    Set-ItemProperty -LiteralPath $mm -Name 'SystemResponsiveness' -Value 10
    $d = Compare-IemFingerprint -Baseline $f1 -Current (Get-IemReaperFingerprint -ProfilePath $pp)
    Assert ($d.Count -eq 1 -and $d[0].key -eq 'mmcss.SystemResponsiveness') 'fingerprint-names-a-change'

    # Inventory: read only, serializable, no command lines or image paths.
    $inv = Get-IemInventory -ProfilePath $pp
    $json = $inv | ConvertTo-Json -Depth 8
    Assert ($json.Length -gt 1000 -and $json -notmatch 'PathName|CommandLine') 'inventory-serializes-without-command-lines'

    # Measurement helpers that need no xperf.
    $a = New-IemTraceArguments -Dir 'C:\t' -CSwitch -CircularMB 1024
    Assert (($a -join ' ') -eq '-on PROC_THREAD+LOADER+DPC+INTERRUPT+CSWITCH+DISPATCHER -BufferSize 1024 -MinBuffers 256 -MaxBuffers 1024 -FileMode Circular -MaxFile 1024 -f C:\t\kernel.etl -start IemMarkers -on 3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11 -f C:\t\markers.etl') 'trace-arguments'
    $l = ConvertFrom-IemLoggers -Text "Logger Name           : NT Kernel Logger`r`nLogger Mode Settings (11)`r`nLogger Name           : IemMarkers`r`n"
    Assert ($l.Count -eq 2 -and $l[0] -eq 'NT Kernel Logger' -and $l[1] -eq 'IemMarkers') 'loggers-parse'
    $c = Get-IemCpuSample
    Assert ($c.cpus.Count -ge 1 -and $c.cpus[0].t100ns -gt 0) 'cpu-sample-reads-raw-counters'
    $ps = Get-IemPollSample -ProfilePath $pp
    Assert ($ps.plan -eq $activeBefore -and $ps.governor -eq 'Running') 'poll-sample-reads-the-sentinels'
    [void](Get-IemSystemEvents -Since ((Get-Date).AddHours(-1).ToUniversalTime().ToString('o')))
    Write-Host 'ok  system-events-read'
} finally {
    try { [void](Exit-IemTuningMode -ProfilePath $pp) } catch { Write-Host "cleanup exit: $_" }
    foreach ($t in 2, 3) { try { [void](Undo-IemTuning -ProfilePath $pp -Tier $t) } catch { Write-Host "cleanup undo: $_" } }
    if ([IemPower]::Active() -ne $activeBefore) { [IemPower]::Activate($activeBefore) }
    if (@(& powercfg.exe /list) -match $testPlan) { & powercfg.exe /delete $testPlan | Out-Null }
    Unregister-ScheduledTask -TaskPath $taskPath -TaskName $taskName -Confirm:$false -ErrorAction SilentlyContinue
    if (@((Get-MpPreference).ExclusionPath) -contains $dir) { Remove-MpPreference -ExclusionPath $dir }
    Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Host 'Test-IemTuning: all passed'
```

  The child `PING` process ends by itself (240 s). The test never ends a process.

- [ ] **Step 2: Spike self-test additions.** In `Test-SpikePc.ps1`, after the `arguments-refuse-an-unknown-mode` case, add:

```powershell
# S1c: CPU Sets and hwlat.
$req = [pscustomobject]@{ id = 'spike-2'; mode = 'duplex'; driver = 'Some Card'; frames = 32; seconds = 28800; burn_us = 40; stress = 4; panic_at = 0; cycles = 5; audio_cpus = '14'; stress_cpus = '6-13' }
Assert (((New-SpikeArguments -Request $req -Root 'C:\r') -join ' ') -like '*--seconds 28800 *--audio-cpus 14 --stress-cpus 6-13') 'arguments-cpu-sets'
$req.audio_cpus = '14;calc'
Throws { New-SpikeArguments -Request $req -Root 'C:\r' } 'arguments-refuse-a-bad-cpu-list'
$h = [pscustomobject]@{ id = 'spike-3'; mode = 'hwlat'; driver = 'Some Card'; frames = 0; seconds = 30; burn_us = 0; stress = 0; panic_at = 0; cycles = 1; cpu = 14; threshold_us = 10 }
$ha = New-SpikeArguments -Request $h -Root 'C:\r'
Assert ($ha[0] -eq 'hwlat' -and (($ha -join ' ') -like '*--cpu 14 --seconds 30 --threshold-us 10')) 'arguments-hwlat'
$h.cpu = 64
Throws { New-SpikeArguments -Request $h -Root 'C:\r' } 'arguments-refuse-cpu-64'
$h.cpu = 3; $h.threshold_us = 0
Throws { New-SpikeArguments -Request $h -Root 'C:\r' } 'arguments-refuse-threshold-0'
```

- [ ] **Step 3: Scan and commit.**

```bash
cd "$WORK" && python3 scripts/check_integrity.py
git add scripts/pc-tuning/Test-IemTuning.ps1 scripts/asio-spike/Test-SpikePc.ps1
git commit -m "test(s1c): tuning self-test on real backends; spike hwlat and CPU Set arguments

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: `latency_report.py` — the summaries

**Files:**
- Create: `scripts/pc-tuning/latency_report.py`, `scripts/pc-tuning/test_latency_report.py`

**Interfaces:**
- **Consumes:**
  - spike reports (Task 4 fields);
  - xperf `dpcisr` text;
  - `Get-IemPollSample` rows (Task 6);
  - `xperf -a dumper` rows via `near.txt`.
- **Produces:**
  - `parse_dpcisr(text) -> {"dpc": {module: {"count", "max_us", "open", "over": {"64","128","256","512"}}}, "isr": {...}, "usage": {"dpc"|"isr": {module: {cpu: usec}}}}`;
  - `budget_findings(parsed, watch_lps) -> list[str]`;
  - `cpu_rates(samples) -> {lp: {...}}`;
  - `glitch_counts(report) -> dict`, `callback_view(report, polls) -> dict`, `sentinel_changes(polls) -> list`;
  - `hwlat_summary(report) -> dict`;
  - `parse_dumper(text) -> (fields, rows)`, `near_glitch(text, period_us, window_periods=2) -> list`;
  - `summarize(label, verdict, report, dpcisr_text, polls, events, watch_lps) -> dict`.

- [ ] **Step 1: Tests first.** Create `scripts/pc-tuning/test_latency_report.py`:

```python
"""Tests for scripts/pc-tuning/latency_report.py (pure functions over the
xperf texts, the spike's report and the PC samples)."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import latency_report as lr  # noqa: E402

DPCISR = """
--------------------------
DPC Info
--------------------------
Total = 3000 for module yaic.sys
Elapsed Time, >        0 usecs AND <=        1 usecs,      0, or   0.00%
Elapsed Time, >        8 usecs AND <=       16 usecs,   2990, or  99.67%
Elapsed Time, >       64 usecs AND <=      128 usecs,     10, or   0.33%
Total = 20 for module dxgkrnl.sys
Elapsed Time, >      128 usecs AND <=      256 usecs,     15, or  75.00%
Elapsed Time, >      512 usecs AND <=     1024 usecs,      5, or  25.00%
yaic.sys: 45000 usec (0.10% CPU 2 usage)
dxgkrnl.sys: 3000 usec (0.01% CPU 14 usage)
--------------------------
Interrupt Info
--------------------------
Total = 3000 for module yaic.sys
Elapsed Time, >        2 usecs AND <=        4 usecs,   3000, or 100.00%
Total = 2 for module ndis.sys
Elapsed Time, >     2048 usecs,      2, or 100.00%
"""


class DpcIsrTests(unittest.TestCase):
    def test_modules_maxima_and_counts_above_the_limits(self) -> None:
        d = lr.parse_dpcisr(DPCISR)
        self.assertEqual(d["dpc"]["yaic.sys"], {"count": 3000, "max_us": 128, "open": False, "over": {"64": 10, "128": 0, "256": 0, "512": 0}})
        self.assertEqual(d["dpc"]["dxgkrnl.sys"]["max_us"], 1024)
        self.assertEqual(d["dpc"]["dxgkrnl.sys"]["over"], {"64": 20, "128": 20, "256": 5, "512": 5})
        self.assertEqual(d["isr"]["ndis.sys"], {"count": 2, "max_us": 2048, "open": True, "over": {"64": 2, "128": 2, "256": 2, "512": 2}})
        self.assertEqual(d["usage"]["dpc"], {"yaic.sys": {"2": 45000}, "dxgkrnl.sys": {"14": 3000}})

    def test_budget_names_modules_over_the_limits(self) -> None:
        d = lr.parse_dpcisr(DPCISR)
        findings = lr.budget_findings(d, watch_lps=[2, 14])
        self.assertIn("dpc dxgkrnl.sys: up to 1024 us on a watched CPU (budget 128)", findings)
        self.assertIn("dpc dxgkrnl.sys: up to 1024 us (a full period is 333)", findings)
        self.assertIn("isr ndis.sys: above 2048 us (a full period is 333)", findings)
        self.assertFalse([f for f in findings if "yaic.sys" in f])

    def test_empty_text(self) -> None:
        self.assertEqual(lr.parse_dpcisr(""), {"dpc": {}, "isr": {}, "usage": {"dpc": {}, "isr": {}}})


def cpu(lp, t, ints, dpcs, dpc_t=0, int_t=0, idle=0, c1=0, c2=0, c3=0):
    return {"lp": lp, "t100ns": t, "interrupts": ints, "dpcs": dpcs, "dpc_time": dpc_t, "int_time": int_t,
            "idle_time": idle, "c1_time": c1, "c2_time": c2, "c3_time": c3}


class CpuRateTests(unittest.TestCase):
    def test_rates_between_samples(self) -> None:
        s = [{"cpus": [cpu(2, 0, 0, 0), cpu(3, 0, 5, 5)]},
             {"cpus": [cpu(2, 100_000_000, 30_000, 30_000, dpc_t=1_000_000, idle=90_000_000, c1=50_000_000), cpu(3, 100_000_000, 5, 5, idle=100_000_000)]},
             {"cpus": [cpu(2, 200_000_000, 90_000, 60_000, dpc_t=3_000_000, idle=180_000_000, c1=50_000_000)]}]
        r = lr.cpu_rates(s)
        self.assertEqual(r["2"]["int_s_mean"], 4500.0)
        self.assertEqual(r["2"]["int_s_max"], 6000.0)
        self.assertEqual(r["2"]["dpc_s_mean"], 3000.0)
        self.assertEqual(r["2"]["dpc_pct_max"], 2.0)
        self.assertEqual(r["2"]["busy_pct_mean"], 10.0)
        self.assertEqual(r["3"]["int_s_mean"], 0.0)
        self.assertEqual(r["3"]["busy_pct_mean"], 0.0)

    def test_fewer_than_two_samples_give_nothing(self) -> None:
        self.assertEqual(lr.cpu_rates([]), {})
        self.assertEqual(lr.cpu_rates([{"cpus": [cpu(0, 1, 1, 1)]}]), {})


REPORT = {"outcome": "done", "segments": [
    {"telemetry": {"callback_cpus": {"14": 900, "15": 100}, "callback_thread": 4242, "thread_switches": 0, "glitches_dropped": 1},
     "glitches": [{"kind": "late", "at_ns": 5, "value": 600000}, {"kind": "missed", "at_ns": 9, "value": 700000}], "glitches_unreported": 2},
    {"telemetry": {"callback_cpus": {"14": 50}, "callback_thread": 4243, "thread_switches": 1, "glitches_dropped": 0},
     "glitches": [{"kind": "missed", "at_ns": 11, "value": 800000}], "glitches_unreported": 0}]}


class ReportTests(unittest.TestCase):
    def test_glitch_counts_include_the_uncounted(self) -> None:
        g = lr.glitch_counts(REPORT)
        self.assertEqual(g["by_kind"], {"late": 1, "missed": 2})
        self.assertEqual((g["unreported"], g["dropped"]), (2, 1))
        self.assertEqual(g["first"][0], {"kind": "late", "at_ns": 5, "value": 600000})

    def test_callback_view_merges_segments_and_polled_priorities(self) -> None:
        polls = [{"thread": {"base": 15, "current": 15}}, {"thread": None}, {"thread": {"base": 15, "current": 26}}]
        v = lr.callback_view(REPORT, polls)
        self.assertEqual(v["cpus"], {"14": 950, "15": 100})
        self.assertEqual(v["threads"], [4242, 4243])
        self.assertEqual(v["thread_switches"], 1)
        self.assertEqual(v["priority"], {"base_min": 15, "base_max": 15, "current_min": 15, "current_max": 26})

    def test_sentinel_changes_list_each_new_value_once(self) -> None:
        polls = [{"at": "t1", "plan": "a", "governor": "Stopped"}, {"at": "t2", "plan": "a", "governor": "Stopped"},
                 {"at": "t3", "plan": "b", "governor": "Running"}]
        self.assertEqual(lr.sentinel_changes(polls), [{"at": "t1", "plan": "a", "governor": "Stopped"}, {"at": "t3", "plan": "b", "governor": "Running"}])

    def test_hwlat_summary(self) -> None:
        r = {"outcome": "done", "hwlat": {"cpu": 14, "reads": 10, "over": 3, "gaps_us": {"p50": 12.0, "p99": 40.0, "p999": 40.0, "max": 55.5},
                                          "largest": [{"at_us": 1.0, "gap_us": 55.5}, {"at_us": 2.0, "gap_us": 40.0}]}}
        self.assertEqual(lr.hwlat_summary(r), {"cpu": 14, "outcome": "done", "reads": 10, "over": 3, "max_us": 55.5, "p999_us": 40.0, "largest_us": [55.5, 40.0]})


DUMPER = """BeginHeader
                    DPC,  TimeStamp,    CPU, ElapsedTime,  Routine
              Interrupt,  TimeStamp,    CPU, ElapsedTime,  Routine
                CSwitch,  TimeStamp, New Process Name ( PID),  New TID, NPri, CPU
EndHeader
                    DPC,       1000,      2,         12,  yaic.sys!0x10
                    DPC,       1400,     14,        300,  dxgkrnl.sys!0x20
              Interrupt,       1500,      2,          3,  yaic.sys!0x30
                CSwitch,       1600, asio_spike.exe (100),     4242,   26,  14
   UnknownEvent/Classic,       1650, iemmixer-glitch kind=missed at_qpc=1000 emit_qpc=1200 freq=10000000 value=700000
                    DPC,       5000,      2,         12,  yaic.sys!0x10
"""


class NearGlitchTests(unittest.TestCase):
    def test_header_driven_rows(self) -> None:
        fields, rows = lr.parse_dumper(DUMPER)
        self.assertEqual(fields["DPC"][3], "ElapsedTime")
        self.assertEqual(len(rows), 6)

    def test_activity_in_the_periods_before_each_glitch(self) -> None:
        near = lr.near_glitch(DUMPER, period_us=333)
        self.assertEqual(len(near), 1)
        g = near[0]
        # emit 1200, glitch 1000 ticks at 10 MHz: the glitch lies 20 us before the marker (1650 - 20 = 1630).
        self.assertEqual((g["kind"], g["at_us"], g["exact"]), ("missed", 1630.0, True))
        # The window is the 2 periods (666 us) before 1630: 964..1630.
        self.assertEqual([(e["event"], e["cpu"], e["us"]) for e in g["events"]],
                         [("DPC", "2", 12.0), ("DPC", "14", 300.0), ("Interrupt", "2", 3.0), ("CSwitch", "14", None)])
        self.assertEqual(g["events"][3]["what"], "asio_spike.exe (100)")

    def test_a_marker_without_its_text_widens_the_window(self) -> None:
        text = DUMPER.replace("iemmixer-glitch kind=missed at_qpc=1000 emit_qpc=1200 freq=10000000 value=700000", "3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11")
        near = lr.near_glitch(text, period_us=333)
        self.assertEqual((near[0]["kind"], near[0]["exact"]), ("unknown", False))
        self.assertEqual(len(near[0]["events"]), 4)


class SummaryTests(unittest.TestCase):
    def test_summary_without_a_trace(self) -> None:
        s = lr.summarize("idle-32", {"stable": True}, REPORT, None, [], [], watch_lps=[2, 14])
        self.assertEqual((s["label"], s["dpcisr"], s["findings"]), ("idle-32", None, ["no trace"]))
        self.assertEqual(s["glitches"]["by_kind"]["missed"], 2)

    def test_summary_with_a_trace_lists_the_worst_modules_first(self) -> None:
        s = lr.summarize("load-32", {"stable": False}, REPORT, DPCISR, [], [{"provider": "WHEA-Logger", "id": 19, "count": 1}], watch_lps=[2, 14])
        self.assertEqual(s["dpcisr"]["dpc"][0]["module"], "dxgkrnl.sys")
        self.assertEqual(s["system_events"], [{"provider": "WHEA-Logger", "id": 19, "count": 1}])


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run the tests to see them fail.**

```bash
cd "$WORK" && python3 -m unittest scripts/pc-tuning/test_latency_report.py 2>&1 | tail -3
```

Expected: `ModuleNotFoundError: No module named 'latency_report'`.

- [ ] **Step 3: Create `latency_report.py`.**

```python
#!/usr/bin/env python3
"""S1c measurement summaries (design note §4): one JSON per step from the
spike's report, xperf's dpcisr text, the PC's poll samples (CPU counters,
active plan, governor, callback-thread priority) and the System log; plus the
hwlat and near-glitch views. Pure functions; tuning_window.py feeds them."""
from __future__ import annotations

import re

PERIOD_US = 333          # B = 32 at 96 kHz
WATCH_BUDGET_US = 128    # the xperf bucket edge above 100 us (design note §4.4)
LIMITS_US = (64, 128, 256, 512)

_SECTION = re.compile(r"^\s*(DPC|Interrupt|ISR)\s+Info\s*$", re.I)
_TOTAL = re.compile(r"^\s*Total\s*=\s*(\d+)\s+for module\s+(\S+)")
_BUCKET = re.compile(r"^\s*Elapsed Time,\s*>\s*(\d+)\s*usecs(?:\s+AND\s+<=\s*(\d+)\s*usecs)?,\s*(\d+)")
_USAGE = re.compile(r"^\s*(\S+):\s*(\d+)\s*usec\s*\(\s*[\d.]+%\s*CPU\s*(\d+)\s*usage\)")
_MARKER = re.compile(r"iemmixer-glitch kind=(\S+) at_qpc=(-?\d+) emit_qpc=(-?\d+) freq=(\d+) value=(\d+)")
_MARKER_ID = "3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11"
NEAR_EVENTS = ("DPC", "TimedDPC", "ThreadedDPC", "Interrupt", "CSwitch", "ReadyThread")


def parse_dpcisr(text: str) -> dict:
    """Per kind (dpc/isr) and module: the count, the upper edge of the highest
    non-empty bucket (an open last bucket reports its lower edge with
    open=True) and the counts in buckets starting at or above each limit;
    per-CPU usage lines as {kind: {module: {cpu: usec}}}."""
    out: dict = {"dpc": {}, "isr": {}, "usage": {"dpc": {}, "isr": {}}}
    kind = None
    module = None
    for line in text.splitlines():
        m = _SECTION.match(line)
        if m:
            kind = "dpc" if m.group(1).lower() == "dpc" else "isr"
            module = None
            continue
        if kind is None:
            continue
        if m := _TOTAL.match(line):
            module = m.group(2)
            out[kind][module] = {"count": int(m.group(1)), "max_us": 0, "open": False, "over": {str(x): 0 for x in LIMITS_US}}
        elif (m := _BUCKET.match(line)) and module is not None:
            lo, hi, n = int(m.group(1)), m.group(2), int(m.group(3))
            if n == 0:
                continue
            entry = out[kind][module]
            edge = int(hi) if hi is not None else lo
            if edge >= entry["max_us"]:
                entry["max_us"], entry["open"] = edge, hi is None
            for limit in LIMITS_US:
                if lo >= limit:
                    entry["over"][str(limit)] += n
        elif m := _USAGE.match(line):
            out["usage"][kind].setdefault(m.group(1), {})[m.group(3)] = int(m.group(2))
    return out


def budget_findings(parsed: dict, watch_lps: list[int]) -> list[str]:
    """Modules above the budget on a watched CPU (the card's, the audio one)
    and modules reaching a full period anywhere."""
    watched = {str(x) for x in watch_lps}
    findings = []
    for kind in ("dpc", "isr"):
        for module, e in parsed[kind].items():
            shown = f"above {e['max_us']}" if e["open"] else f"up to {e['max_us']}"
            cpus = set(parsed["usage"][kind].get(module, {}))
            if e["max_us"] > WATCH_BUDGET_US and cpus & watched:
                findings.append(f"{kind} {module}: {shown} us on a watched CPU (budget {WATCH_BUDGET_US})")
            if e["max_us"] >= PERIOD_US:
                findings.append(f"{kind} {module}: {shown} us (a full period is {PERIOD_US})")
    return findings


def cpu_rates(samples: list[dict]) -> dict[str, dict]:
    """Per logical processor from consecutive raw samples: interrupts/s and
    DPCs/s (mean, and the highest interval for interrupts), % DPC and %
    interrupt time (mean, max), % busy and % C1/C2/C3 (mean)."""
    series: dict[int, dict[str, list[float]]] = {}
    for a, b in zip(samples, samples[1:]):
        prev = {c["lp"]: c for c in a["cpus"]}
        for c in b["cpus"]:
            p = prev.get(c["lp"])
            dt = c["t100ns"] - p["t100ns"] if p else 0
            if dt <= 0:
                continue
            d = series.setdefault(c["lp"], {k: [] for k in ("int_s", "dpc_s", "dpc_pct", "int_pct", "busy_pct", "c1_pct", "c2_pct", "c3_pct")})
            d["int_s"].append((c["interrupts"] - p["interrupts"]) / (dt / 1e7))
            d["dpc_s"].append((c["dpcs"] - p["dpcs"]) / (dt / 1e7))
            d["dpc_pct"].append(100 * (c["dpc_time"] - p["dpc_time"]) / dt)
            d["int_pct"].append(100 * (c["int_time"] - p["int_time"]) / dt)
            d["busy_pct"].append(100 - 100 * (c["idle_time"] - p["idle_time"]) / dt)
            for k in ("c1", "c2", "c3"):
                d[f"{k}_pct"].append(100 * (c[f"{k}_time"] - p[f"{k}_time"]) / dt)
    out = {}
    for lp, d in sorted(series.items()):
        mean = {k: round(sum(v) / len(v), 1) for k, v in d.items()}
        out[str(lp)] = {"int_s_mean": mean["int_s"], "int_s_max": round(max(d["int_s"]), 1), "dpc_s_mean": mean["dpc_s"],
                        "dpc_pct_mean": mean["dpc_pct"], "dpc_pct_max": round(max(d["dpc_pct"]), 1),
                        "int_pct_mean": mean["int_pct"], "int_pct_max": round(max(d["int_pct"]), 1),
                        "busy_pct_mean": mean["busy_pct"], "c1_pct_mean": mean["c1_pct"], "c2_pct_mean": mean["c2_pct"], "c3_pct_mean": mean["c3_pct"]}
    return out


def glitch_counts(report: dict) -> dict:
    by_kind: dict[str, int] = {}
    first: list[dict] = []
    unreported = dropped = 0
    for seg in report.get("segments", []):
        for g in seg.get("glitches", []):
            by_kind[g["kind"]] = by_kind.get(g["kind"], 0) + 1
            if len(first) < 20:
                first.append(g)
        unreported += seg.get("glitches_unreported", 0)
        dropped += (seg.get("telemetry") or {}).get("glitches_dropped", 0)
    return {"by_kind": by_kind, "unreported": unreported, "dropped": dropped, "first": first}


def callback_view(report: dict, polls: list[dict]) -> dict:
    cpus: dict[str, int] = {}
    threads: list[int] = []
    switches = 0
    for seg in report.get("segments", []):
        t = seg.get("telemetry") or {}
        for lp, n in (t.get("callback_cpus") or {}).items():
            cpus[lp] = cpus.get(lp, 0) + n
        if t.get("callback_thread") and t["callback_thread"] not in threads:
            threads.append(t["callback_thread"])
        switches += t.get("thread_switches", 0)
    seen = [p["thread"] for p in polls if p.get("thread")]
    priority = None
    if seen:
        priority = {"base_min": min(s["base"] for s in seen), "base_max": max(s["base"] for s in seen),
                    "current_min": min(s["current"] for s in seen), "current_max": max(s["current"] for s in seen)}
    return {"cpus": cpus, "threads": threads, "thread_switches": switches, "priority": priority}


def sentinel_changes(polls: list[dict]) -> list[dict]:
    """Each new (plan, governor) pair with the time it was first seen."""
    out: list[dict] = []
    for p in polls:
        row = {"at": p.get("at"), "plan": p.get("plan"), "governor": p.get("governor")}
        if not out or (out[-1]["plan"], out[-1]["governor"]) != (row["plan"], row["governor"]):
            out.append(row)
    return out


def hwlat_summary(report: dict) -> dict:
    h = report.get("hwlat") or {}
    gaps = h.get("gaps_us") or {}
    return {"cpu": h.get("cpu"), "outcome": report.get("outcome"), "reads": h.get("reads"), "over": h.get("over"),
            "max_us": gaps.get("max"), "p999_us": gaps.get("p999"), "largest_us": [x["gap_us"] for x in h.get("largest", [])[:5]]}


def parse_dumper(text: str) -> tuple[dict[str, list[str]], list[list[str]]]:
    """xperf -a dumper: the header's field names per event (between
    BeginHeader and EndHeader) and the event rows, comma-split and stripped."""
    fields: dict[str, list[str]] = {}
    rows: list[list[str]] = []
    in_header = False
    for line in text.splitlines():
        s = line.strip()
        if s in ("BeginHeader", "EndHeader"):
            in_header = s == "BeginHeader"
            continue
        if not s:
            continue
        cols = [c.strip() for c in s.split(",")]
        if in_header:
            fields[cols[0]] = cols
        else:
            rows.append(cols)
    return fields, rows


def _col(fields: dict[str, list[str]], row: list[str], *names: str) -> str | None:
    header = fields.get(row[0], [])
    for n in names:
        if n in header:
            i = header.index(n)
            return row[i] if i < len(row) else None
    return None


def near_glitch(text: str, period_us: float, window_periods: int = 2) -> list[dict]:
    """For each glitch marker: DPC/ISR/context-switch rows in the window
    before the glitch. The marker's text maps it to the glitch's time exactly
    (the marker is written up to 10 ms later); without the text the window is
    the 11 ms before the marker."""
    fields, rows = parse_dumper(text)
    out = []
    for row in rows:
        line = ", ".join(row)
        if "iemmixer-glitch" not in line and _MARKER_ID not in line:
            continue
        t_marker = float(row[1])
        m = _MARKER.search(line)
        if m:
            kind, at_qpc, emit_qpc, freq = m.group(1), int(m.group(2)), int(m.group(3)), int(m.group(4))
            at_us = t_marker - (emit_qpc - at_qpc) * 1e6 / freq
            start, end, exact = at_us - window_periods * period_us, at_us, True
        else:
            kind, at_us, start, end, exact = "unknown", t_marker, t_marker - 11_000, t_marker, False
        events = []
        for r in rows:
            if r[0] not in NEAR_EVENTS or not start <= float(r[1]) <= end:
                continue
            us = _col(fields, r, "ElapsedTime", "Elapsed Time", "Duration")
            events.append({"event": r[0], "t_us": float(r[1]), "cpu": _col(fields, r, "CPU"),
                           "us": float(us) if us not in (None, "") else None,
                           "what": _col(fields, r, "Routine", "Image!Function", "New Process Name ( PID)")})
        out.append({"kind": kind, "at_us": round(at_us, 1), "exact": exact, "events": events})
    return out


def top_modules(parsed: dict, n: int = 12) -> dict:
    return {kind: sorted(({"module": m, **e} for m, e in parsed[kind].items()), key=lambda x: (-x["max_us"], -x["count"]))[:n]
            for kind in ("dpc", "isr")}


def summarize(label: str, verdict: dict | None, report: dict, dpcisr_text: str | None, polls: list[dict],
              events: list[dict], watch_lps: list[int]) -> dict:
    parsed = parse_dpcisr(dpcisr_text) if dpcisr_text is not None else None
    return {
        "label": label,
        "verdict": verdict,
        "glitches": glitch_counts(report),
        "callback": callback_view(report, polls),
        "process": report.get("process"),
        "dpcisr": top_modules(parsed) if parsed else None,
        "findings": budget_findings(parsed, watch_lps) if parsed else ["no trace"],
        "cpu": cpu_rates([p["cpu"] for p in polls if p.get("cpu")]),
        "sentinels": sentinel_changes(polls),
        "system_events": events,
    }
```

- [ ] **Step 4: Run the tests to see them pass.**

```bash
cd "$WORK" && python3 -m unittest scripts/pc-tuning/test_latency_report.py -v 2>&1 | tail -3
```

Expected: `OK`.

- [ ] **Step 5: Commit.**

```bash
git add scripts/pc-tuning/latency_report.py scripts/pc-tuning/test_latency_report.py
git commit -m "feat(s1c): latency summaries (dpcisr, CPU rates, glitches, sentinels, hwlat, near-glitch)

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: `spike_window.py` — unwind steps, poll hook, hwlat, 10 h

**Files:**
- Modify: `scripts/asio-spike/spike_window.py`, `scripts/asio-spike/test_spike_window.py`

**Interfaces:**
- **Produces:**
  - `MAX_SECONDS = 36_000`, and `check_request(mode, frames, seconds, burn_us, stress, cycles, cpu=None, threshold_us=10, audio_cpus="", stress_cpus="")`;
  - `run_timeout("hwlat", s, c) == s + 60`;
  - `undo_plan` orders `stop-spike`, `trace-stop`, `tuning-exit`, `restore-buffer`, `bring-back`, `fingerprint`;
  - `unwind(env, state, running, bring_back=True)`;
  - `cmd_run(env, args, on_poll=None) -> {"run", "exit", "verdict", "report"}`;
  - `bring_back(env, state) -> dict`;
  - `BUNDLE_FILES` gains `IemMeasure.psm1` and `IemTuning.psm1`.
- **State keys it reads:**
  - `trace` (the PC run dir; set by `tuning_window.py` before a trace starts);
  - `tuning_mode` (true before `Enter-IemTuningMode`);
  - `fingerprint` (the local baseline file).
- **Env keys** (optional unless those state keys are set): `PC_TUNING_ROOT`, `PC_XPERF`.

- [ ] **Step 1: Failing tests.** In `test_spike_window.py`:
  1. **`RequestTests.test_limits`:** replace the first two accept lines and the `3601` refusal with:

```python
        sw.check_request("probe", None, 600, 0, 0, 5)
        sw.check_request("duplex", 32, 36000, 300, 8, 20)
        sw.check_request("hwlat", None, 30, 0, 0, 1, cpu=14, threshold_us=10)
        sw.check_request("duplex", 32, 600, 40, 4, 5, audio_cpus="14", stress_cpus="0,1,6-13")
```

  and in the bad list change `("duplex", 32, 3601, 0, 0, 5)` to `("duplex", 32, 36001, 0, 0, 5)`. Add after the loop:

```python
        for kw in ({"cpu": None}, {"cpu": 64}, {"cpu": 3, "threshold_us": 0}, {"cpu": 3, "threshold_us": 1001}):
            with self.assertRaises(sw.StepError, msg=str(kw)):
                sw.check_request("hwlat", None, 30, 0, 0, 1, **kw)
        for bad in ("14;x", "a", "1,,2"):
            with self.assertRaises(sw.StepError, msg=bad):
                sw.check_request("duplex", 32, 600, 0, 0, 5, audio_cpus=bad)
```

  2. **`test_timeouts`:** append `self.assertEqual(sw.run_timeout("hwlat", 30, 1), 90)`.
  3. **Add to `UndoPlanTests`:**

```python
    def test_trace_and_tuning_unwind_before_the_buffer_and_reaper(self) -> None:
        state = {"card": "free", "pref_current": 32, "trace": "C:\\t\\runs\\x", "tuning_mode": True, "fingerprint": "/b.json"}
        self.assertEqual(sw.undo_plan(state, spike_running=True),
                         ["stop-spike", "trace-stop", "tuning-exit", "restore-buffer", "bring-back", "fingerprint"])

    def test_no_fingerprint_without_bringing_reaper_back(self) -> None:
        state = {"card": "reaper", "pref_current": None, "tuning_mode": True, "fingerprint": "/b.json"}
        self.assertEqual(sw.undo_plan(state, spike_running=False), ["tuning-exit"])

    def test_flags_recorded_before_the_action(self) -> None:
        # tuning_window records the flag first; a flag alone (the action may have failed half-way) still unwinds.
        self.assertIn("trace-stop", sw.undo_plan({"card": "free", "trace": "d"}, spike_running=False))
        self.assertIn("tuning-exit", sw.undo_plan({"card": "free", "tuning_mode": True}, spike_running=False))
        self.assertNotIn("trace-stop", sw.undo_plan({"card": "free", "trace": None}, spike_running=False))
```

  4. **`PreflightTests.GOOD`:** the bundle grows from four to six files (the two S1c modules). Change `"files": 4` to `"files": 6` (a new requirement, not a weakened check: preflight still demands every bundle file).

  Run: `python3 -m unittest discover -s scripts/asio-spike -p 'test_*.py' 2>&1 | tail -3`. Expected: failures in `test_limits`, `test_timeouts`, the three new tests and the two preflight tests. Commit:

```bash
git add scripts/asio-spike/test_spike_window.py
git commit -m "test(s1c): [red] 10 h runs, hwlat requests and the tuning unwind steps

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 2: Implementation.** In `spike_window.py`:
  1. **Constants:**
     - `BUNDLE_FILES = ("GoldenPc.psm1", "IemMeasure.psm1", "IemTuning.psm1", "SpikePc.psm1", "asio_spike.exe", "spike-task.ps1")`;
     - add `MAX_SECONDS = 36_000` (an 8 h soak with margin, S1c design note §8 W4);
     - add `CPU_LIST = re.compile(r"[0-9]+(-[0-9]+)?(,[0-9]+(-[0-9]+)?)*")`.
  2. **Replace `check_request` and `run_timeout`:**

```python
def check_request(mode: str, frames: int | None, seconds: int, burn_us: int, stress: int, cycles: int,
                  cpu: int | None = None, threshold_us: int = 10, audio_cpus: str = "", stress_cpus: str = "") -> None:
    if mode not in ("probe", "duplex", "reopen", "hwlat"):
        raise StepError(f"unknown mode {mode}")
    if mode in ("duplex", "reopen") and frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if mode == "hwlat" and not (cpu is not None and 0 <= cpu <= 63 and 1 <= threshold_us <= 1000):
        raise StepError("hwlat needs --cpu 0..63 and --threshold-us 1..1000")
    if not (1 <= seconds <= MAX_SECONDS and 0 <= burn_us <= 300 and 0 <= stress <= 8 and 1 <= cycles <= 20):
        raise StepError(f"limits: seconds 1..{MAX_SECONDS}, burn-us 0..300, stress 0..8, cycles 1..20")
    for text in (audio_cpus, stress_cpus):
        if text and not CPU_LIST.fullmatch(text):
            raise StepError("CPU lists look like 14 or 0,1,6-13")


def run_timeout(mode: str, seconds: int, cycles: int) -> int:
    """Seconds after which the PC task writes the stop file itself."""
    return {"probe": 60, "duplex": seconds + 60, "reopen": 30 * cycles + 60, "hwlat": seconds + 60}[mode]
```

  3. **Replace `undo_plan`:**

```python
def undo_plan(state: dict, spike_running: bool) -> list[str]:
    """What leaving the window (or "ide event") must do, in order. While the
    card is free a spike may be starting (the task has not launched it yet),
    so the graceful stop always runs; it is harmless when none runs. A kernel
    trace stops and the S1c mode levers revert before the buffer and REAPER
    (S1c design note §5.2); the fingerprint is read after REAPER is back."""
    plan: list[str] = []
    card_away = state.get("card") in ("switching", "free")
    if spike_running or card_away:
        plan.append("stop-spike")
    if state.get("trace"):
        plan.append("trace-stop")
    if state.get("tuning_mode"):
        plan.append("tuning-exit")
    if buffer_touched(state):
        plan.append("restore-buffer")
    if card_away:
        plan.append("bring-back")
        if state.get("fingerprint"):
            plan.append("fingerprint")
    return plan
```

  4. **Split the bring-back out of `unwind` and add the new steps.** Replace `unwind` with:

```python
def tuning_body(env: dict[str, str], body: str) -> str:
    """A PC body that loads the S1c modules from the verified bundle first."""
    for k in ("PC_TUNING_ROOT", "PC_XPERF"):
        if not env.get(k):
            raise StepError(f"{k} missing in the private env (S1c plan Task 12)")
    return (f"Import-Module (Join-Path {ps_quote(env['PC_ROOT'])} 'bin\\IemMeasure.psm1') -Force -Global ; {body}")


def tuning_profile(env: dict[str, str]) -> str:
    return ps_quote(env["PC_TUNING_ROOT"] + "\\profile.json")


def bring_back(env: dict[str, str], state: dict) -> dict:
    """REAPER through our own start task (only if it does not run), then the
    S1a handover checks."""
    return ps(env, "Invoke-SpikeBringBack " + " ".join([
        f"-Http {ps_quote(env['PC_REAPER_HTTP'])}",
        f"-StartTaskPath {ps_quote(env['PC_REAPER_START_TASK_PATH'])} -StartTask {ps_quote(env['PC_REAPER_START_TASK'])}",
        f"-NTrack {int(env['PC_NTRACK'])} -BridgeState {ps_quote(env['PC_METER_BRIDGE'])}",
        f"-BridgeAction {ps_quote(env['PC_METER_ACTION'])} -Heartbeat {ps_quote(env['PC_METER_HEARTBEAT'])}",
        f"-AsioModule {ps_quote(env['PC_ASIO_MODULE'])} -AppHttp {ps_quote(env['PC_APP_HTTP'])}",
        f"-BufferKey {ps_quote(env['PC_BUFFER_KEY'])} -BufferName {ps_quote(env['PC_BUFFER_NAME'])} {buffer_args(state)}",
    ]), timeout=240, event="ignore")


def fingerprint_diff(baseline: dict, current: dict) -> list[dict]:
    keys = sorted(set(baseline) | set(current))
    return [{"key": k, "baseline": baseline.get(k, "<absent>"), "current": current.get(k, "<absent>")}
            for k in keys if str(baseline.get(k, "<absent>")) != str(current.get(k, "<absent>"))]


def alarm(text: str) -> None:
    print(f"OWNER ALARM: {text}", file=sys.stderr, flush=True)


def unwind(env: dict[str, str], state: dict, running: bool, bring_back_reaper: bool = True) -> list:
    """Stop the spike, stop a trace, revert the S1c mode levers, restore the
    buffer (read back), bring REAPER back (it reads the buffer again and
    refuses while a spike or its task runs), compare the fingerprint. A failed
    trace stop, tuning exit or fingerprint alarms the owner and never holds
    REAPER back (S1c design note §5.2). Without bring_back_reaper (before an
    approved reboot) the card stays free and the window stays open."""
    done = []
    gone = True
    for step in undo_plan(state, running):
        if step == "stop-spike":
            gone = bool(ps(env, f"(Stop-SpikeGracefully -Root {ps_quote(env['PC_ROOT'])} -Seconds 60).gone", timeout=120, event="ignore"))
            done.append({"stop-spike": gone})
        elif step == "trace-stop":
            try:
                r = ps(env, tuning_body(env, f"Stop-IemTrace -Xperf {ps_quote(env['PC_XPERF'])} -Dir {ps_quote(state['trace'])}"), timeout=120, event="ignore")
                state["trace"] = None
                save_state(state)
                done.append({"trace-stop": r})
            except StepError as e:
                alarm(f"the kernel trace did not stop ({e}); stop it with xperf -stop -stop IemMarkers")
                done.append({"trace-stop": {"error": str(e)}})
        elif step == "tuning-exit":
            try:
                r = ps(env, tuning_body(env, f"Exit-IemTuningMode -ProfilePath {tuning_profile(env)}"), timeout=240, event="ignore")
                state["tuning_mode"] = False
                save_state(state)
                done.append({"tuning-exit": r})
            except StepError as e:
                alarm(f"the S1c mode levers were not all reverted ({e}); REAPER still comes back")
                done.append({"tuning-exit": {"error": str(e)}})
        elif step == "restore-buffer":
            r = ps(env, f"Set-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])} "
                        f"-Value {state['pref_original']} {buffer_args(state)}", event="ignore")
            state["pref_current"], state["pref_restored"] = state["pref_original"], True
            save_state(state)
            done.append({"restore-buffer": r})
        elif step == "bring-back":
            if not bring_back_reaper:
                break
            if not gone:
                raise StepError("the spike did not stop within 60 s, so REAPER cannot start (I3): alarm the owner now; "
                                "the last resort is the owner's reboot, which comes back in event mode")
            r = bring_back(env, state)
            state["card"] = "reaper"
            save_state(state)
            done.append({"bring-back": r})
        elif step == "fingerprint":
            try:
                current = ps(env, tuning_body(env, f"Get-IemReaperFingerprint -ProfilePath {tuning_profile(env)}"), timeout=120, event="ignore")
                diff = fingerprint_diff(json.loads(Path(state["fingerprint"]).read_text(encoding="utf-8")), current)
                if diff:
                    alarm(f"REAPER mode differs from the baseline: {json.dumps(diff)}")
                done.append({"fingerprint": diff})
            except (StepError, OSError, ValueError) as e:
                alarm(f"the fingerprint could not be read ({e})")
                done.append({"fingerprint": {"error": str(e)}})
    if bring_back_reaper:
        state["closed"] = True
        save_state(state)
    return done
```

  Keyword name: the parameter is `bring_back_reaper`, not `bring_back`: `bring_back` is the function above.

  5. **`cmd_run`:**
     - Signature: `def cmd_run(env, args, on_poll=None) -> dict:`.
     - Replace its `check_request(...)` call with:

```python
    check_request(args.mode, args.frames, args.seconds, args.burn_us, args.stress, args.cycles,
                  getattr(args, "cpu", None), getattr(args, "threshold_us", 10),
                  getattr(args, "audio_cpus", "") or "", getattr(args, "stress_cpus", "") or "")
```

     - Change `if args.mode != "probe" and args.frames != current:` to `if args.mode in ("duplex", "reopen") and args.frames != current:`.
     - Add to `fields` (after `"cycles": args.cycles,`):

```python
              "cpu": -1 if getattr(args, "cpu", None) is None else args.cpu, "threshold_us": getattr(args, "threshold_us", 10),
              "audio_cpus": getattr(args, "audio_cpus", "") or "", "stress_cpus": getattr(args, "stress_cpus", "") or "",
```

     - In the watch loop, after `st = ps(env, watch, timeout=60, event="abandon")`, add:

```python
            if on_poll is not None:
                on_poll(st)
```

     - Replace `v = verdict(report)` with `v = verdict(report) if args.mode != "hwlat" else None`.
     - End the function with:

```python
    return {"run": rid, "exit": code, "verdict": v, "report": str(out / f"{rid}.report.json")}
```

  6. **`cmd_to_event` and `cmd_preempt`:** they call `unwind(env, state, running=...)` unchanged; the default brings REAPER back.
  7. **`main`:** the `run` subparser's `--mode` choices become `("probe", "duplex", "reopen", "hwlat")`. Add:

```python
    run.add_argument("--cpu", type=int)
    run.add_argument("--threshold-us", type=int, default=10)
    run.add_argument("--audio-cpus", default="")
    run.add_argument("--stress-cpus", default="")
```

- [ ] **Step 3: Tests pass; commit.**

```bash
cd "$WORK" && python3 -m unittest discover -s scripts/asio-spike -p 'test_*.py' 2>&1 | tail -3 && python3 scripts/check_integrity.py
git add scripts/asio-spike/spike_window.py
git commit -m "feat(s1c): [green] window unwinds traces and tuning before REAPER, hwlat requests, 10 h runs, poll hook

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Expected: `OK`, and `integrity: clean`.

---

### Task 10: `tuning_window.py` — the S1c commands

**Files:**
- Create: `scripts/pc-tuning/tuning_window.py`, `scripts/pc-tuning/test_tuning_window.py`

**Interfaces:**
- **Consumes:**
  - `spike_window` (`load_env`, `open_state`, `save_state`, `ps`, `guarded`, `ssh_cmd`, `scp`, `remote`, `raw_dir`, `cmd_run`, `unwind`, `undo_plan`, `bring_back`, `fingerprint_diff`, `tuning_body`, `tuning_profile`, `spike_running`, `cmd_preempt`, `event_now`, `EventNow`, `StepError`, `alarm`);
  - `latency_report`;
  - the PC functions of Tasks 5 and 6.
- **Commands:** `tuning-setup`, `inventory`, `fingerprint --baseline|--check`, `wpt-install`, `enter [--only] [--idle]`, `exit`, `apply --tier [--only]`, `undo --tier [--only]`, `state`, `measure`, `hwlat`, `reboot-prepare`, `reboot --approval`, `post-boot`.
- **Pure functions** (tested): `load_profile(path)`, `parse_lps(text)`, `mode_only(text)`, `label_ok(text)`, `should_cut(progress, seen, cuts, circular)`, `post_boot_verdict(checks)`, `check_approval(text)`, `watch_lps(profile, audio)`.

- [ ] **Step 1: Tests first.** Create `scripts/pc-tuning/test_tuning_window.py`:

```python
"""Tests for scripts/pc-tuning/tuning_window.py (pure parts; ssh is the PC)."""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tuning_window as tw  # noqa: E402

PROFILE = {"version": 1, "journal": "C:\\j.json", "registry_root": "",
           "layout": {"housekeeping": [0, 1, 6, 7, 8, 9, 10, 11, 12, 13], "card": [2], "nic": [4, 5], "audio": [14]},
           "plan": {"guid": "6c1b0d6e-0a39-4f55-9c2a-1e5a3b7d9c01", "source": "00000000-0000-0000-0000-000000000001"},
           "governor": "gov", "placement": [], "services_disable": [], "services_mode": [],
           "updates": {"services": [], "tasks": []}, "maintenance": {"off": True, "tasks": []},
           "defender": {"paths": [], "processes": []}, "devices": [], "nic": {"adapter": "a", "properties": {}, "rss": {"base": 4, "max": 5}, "pnp_capabilities": 24},
           "fingerprint": {"files": [], "keys": []}}


def write(obj) -> Path:
    p = Path(tempfile.mkdtemp()) / "pc-tuning.json"
    p.write_text(json.dumps(obj), encoding="utf-8")
    return p


class ProfileTests(unittest.TestCase):
    def test_a_complete_profile_loads(self) -> None:
        self.assertEqual(tw.load_profile(write(PROFILE))["layout"]["audio"], [14])

    def test_missing_keys_and_overlapping_layout_are_refused(self) -> None:
        bad = dict(PROFILE); del bad["governor"]
        with self.assertRaisesRegex(tw.StepError, "missing governor"):
            tw.load_profile(write(bad))
        overlap = json.loads(json.dumps(PROFILE)); overlap["layout"]["audio"] = [2]
        with self.assertRaisesRegex(tw.StepError, "processor 2 has two roles"):
            tw.load_profile(write(overlap))
        wide = json.loads(json.dumps(PROFILE)); wide["layout"]["card"] = [64]
        with self.assertRaisesRegex(tw.StepError, "0..63"):
            tw.load_profile(write(wide))

    def test_watch_lps_are_the_card_and_the_audio_cpus(self) -> None:
        self.assertEqual(tw.watch_lps(PROFILE, ""), [2, 14])
        self.assertEqual(tw.watch_lps(PROFILE, "3"), [2, 3])


class ArgumentTests(unittest.TestCase):
    def test_lists_levers_labels(self) -> None:
        self.assertEqual(tw.parse_lps("0,1,6-8"), [0, 1, 6, 7, 8])
        self.assertEqual(tw.parse_lps(""), [])
        for bad in ("5-3", "64", "1,1", "x"):
            with self.assertRaises(tw.StepError, msg=bad):
                tw.parse_lps(bad)
        self.assertEqual(tw.mode_only("plan,governor"), ["plan", "governor"])
        with self.assertRaises(tw.StepError):
            tw.mode_only("plan,reboot")
        self.assertTrue(tw.label_ok("tier1-c1-load"))
        for bad in ("", "a b", "x/../y", "a" * 41):
            self.assertFalse(tw.label_ok(bad), bad)

    def test_the_reboot_approval_quotes_the_owner(self) -> None:
        tw.check_approval("owner, 14:05: áno, reštartuj")
        for bad in ("", "yes", "reštartuj"):
            with self.assertRaises(tw.StepError, msg=bad):
                tw.check_approval(bad)


class CutTests(unittest.TestCase):
    def test_a_new_glitch_cuts_a_circular_trace_at_most_five_times(self) -> None:
        p = {"missed": 1, "overruns": 0, "position_gaps": 0}
        self.assertEqual(tw.should_cut(p, seen=0, cuts=0, circular=True), (True, 1))
        self.assertEqual(tw.should_cut(p, seen=1, cuts=1, circular=True), (False, 1))
        self.assertEqual(tw.should_cut({"missed": 2, "overruns": 1}, seen=1, cuts=5, circular=True), (False, 3))
        self.assertEqual(tw.should_cut(p, seen=0, cuts=0, circular=False), (False, 1))
        self.assertEqual(tw.should_cut(None, seen=4, cuts=0, circular=True), (False, 4))


class PostBootTests(unittest.TestCase):
    def test_every_check_must_hold(self) -> None:
        ok = {"booted_after_request": True, "reaper": True, "handover": {"asio": "reaper"}, "fingerprint": [], "pending": [], "failed_items": []}
        self.assertEqual(tw.post_boot_verdict(ok), [])
        self.assertEqual(tw.post_boot_verdict({**ok, "fingerprint": [{"key": "plan.active"}]}), ["REAPER mode differs: plan.active"])
        self.assertEqual(tw.post_boot_verdict({**ok, "pending": ["irq:card:mask"]}), ["still pending after the reboot: irq:card:mask"])
        self.assertEqual(tw.post_boot_verdict({**ok, "booted_after_request": False}), ["the PC did not reboot after the request"])
        self.assertEqual(tw.post_boot_verdict({**ok, "handover": {"error": "no meters"}}), ["handover checks failed: no meters"])


if __name__ == "__main__":
    unittest.main()
```

  Run: `python3 -m unittest scripts/pc-tuning/test_tuning_window.py 2>&1 | tail -3`. Expected: `ModuleNotFoundError: No module named 'tuning_window'`.

- [ ] **Step 2: Create `tuning_window.py`.**

```python
#!/usr/bin/env python3
"""S1c tuning window on the dev box (design note §7, §8): the PC tuning
modules (IemTuning.psm1, IemMeasure.psm1 from the verified spike bundle) and
the measurement set, on top of spike_window.py's window, state, event guard
and unwind. A window opens only with spike_window's `new --signal`; every
command here checks the "ide event" flag and pre-empts like spike_window.
Site values come only from the private env ($SPIKE_ENV) and profile
($TUNING_PROFILE). Nothing is ever ended by force; a reboot happens only on
the owner's quoted approval."""
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "asio-spike"))
sys.path.insert(0, str(HERE))
import latency_report as lr  # noqa: E402
import spike_window as sw  # noqa: E402
from golden_window import StepError, ps_quote  # noqa: E402

PROFILE = Path(os.environ.get("TUNING_PROFILE", str(Path.home() / ".config/iemmixer/pc-tuning.json")))
ADK_URL = "https://go.microsoft.com/fwlink/?linkid=2289980"  # ADK 10.1.26100.9457 (September 2026), design note [7]
PROFILE_KEYS = ("version", "journal", "registry_root", "layout", "plan", "governor", "placement", "services_disable",
                "services_mode", "updates", "maintenance", "defender", "devices", "nic", "fingerprint")
LAYOUT_ROLES = ("housekeeping", "card", "nic", "audio")
MODE_LEVERS = ("plan", "governor", "placement", "services")
MAX_CUTS = 5
LABEL = re.compile(r"[a-z0-9][a-z0-9-]{0,39}")
APPROVAL = re.compile(r".*\d{1,2}:\d{2}.*\S.*")


def parse_lps(text: str) -> list[int]:
    out: list[int] = []
    for part in (p.strip() for p in text.split(",") if p.strip()):
        lo, _, hi = part.partition("-")
        try:
            a, b = int(lo), int(hi or lo)
        except ValueError:
            raise StepError(f"bad processor list {text!r}") from None
        if not (0 <= a <= b <= 63):
            raise StepError(f"bad range {part!r}: processors are 0..63, ascending")
        for lp in range(a, b + 1):
            if lp in out:
                raise StepError(f"{text!r} names processor {lp} twice")
            out.append(lp)
    return sorted(out)


def load_profile(path: Path) -> dict:
    if not path.is_file():
        raise StepError(f"{path}: missing (private profile, plan Task 12)")
    p = json.loads(path.read_text(encoding="utf-8"))
    missing = [k for k in PROFILE_KEYS if k not in p]
    if missing:
        raise StepError(f"{path}: missing {', '.join(missing)}")
    roles: dict[int, str] = {}
    for role in LAYOUT_ROLES:
        for lp in p["layout"].get(role, []):
            if not 0 <= int(lp) <= 63:
                raise StepError(f"{path}: layout {role} processor {lp} outside 0..63")
            if lp in roles:
                raise StepError(f"{path}: processor {lp} has two roles ({roles[lp]}, {role})")
            roles[lp] = role
    return p


def watch_lps(profile: dict, audio_cpus: str) -> list[int]:
    """The CPUs whose DPC/ISR budget is watched: the card's and the audio one
    (the spike's --audio-cpus, else the profile's)."""
    audio = parse_lps(audio_cpus) if audio_cpus else list(profile["layout"]["audio"])
    return sorted(set(profile["layout"]["card"]) | set(audio))


def mode_only(text: str) -> list[str]:
    levers = [x.strip() for x in text.split(",") if x.strip()]
    bad = [x for x in levers if x not in MODE_LEVERS]
    if bad or not levers:
        raise StepError(f"--only takes {', '.join(MODE_LEVERS)}")
    return levers


def label_ok(text: str) -> bool:
    return bool(LABEL.fullmatch(text))


def check_approval(text: str) -> None:
    """The owner's approval of the reboot, quoted with its time (HH:MM)."""
    if not APPROVAL.fullmatch(text.strip()) or len(text.strip()) < 12:
        raise StepError("quote the owner's approval with its time, e.g. 'owner, 14:05: áno, reštartuj'")


def should_cut(progress: dict | None, seen: int, cuts: int, circular: bool) -> tuple[bool, int]:
    """A new missed period, overrun or position gap cuts a circular soak
    trace (at most MAX_CUTS times); returns (cut, glitches seen now)."""
    if not progress:
        return False, seen
    total = sum(int(progress.get(k, 0)) for k in ("missed", "overruns", "position_gaps"))
    return (circular and total > seen and cuts < MAX_CUTS), total


def post_boot_verdict(c: dict) -> list[str]:
    problems = []
    if not c["booted_after_request"]:
        problems.append("the PC did not reboot after the request")
    if not c["reaper"]:
        problems.append("REAPER did not start by itself within 5 min")
    if "error" in (c.get("handover") or {}):
        problems.append(f"handover checks failed: {c['handover']['error']}")
    if c["fingerprint"]:
        problems.append("REAPER mode differs: " + ", ".join(d["key"] for d in c["fingerprint"]))
    if c["pending"]:
        problems.append("still pending after the reboot: " + ", ".join(c["pending"]))
    if c["failed_items"]:
        problems.append("items not as applied: " + ", ".join(c["failed_items"]))
    return problems


# ---- PC access (the PC is the external dependency; no unit tests below) ----

def tps(env: dict[str, str], body: str, **kw):
    return sw.ps(env, sw.tuning_body(env, body), **kw)


def as_list(value) -> list:
    """PowerShell returns one row as an object and none as null."""
    if value is None:
        return []
    return value if isinstance(value, list) else [value]


def baseline_path(env: dict[str, str]) -> Path:
    """The REAPER-mode fingerprint baseline: one file across windows."""
    return Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / "fingerprint-baseline.json"


def xperf(env: dict[str, str]) -> str:
    return ps_quote(env["PC_XPERF"])


def need_free(state: dict) -> None:
    if state["card"] != "free":
        raise StepError("the card is not free (run spike_window to-dev first)")


def raw(env: dict[str, str], state: dict) -> Path:
    d = Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / state["id"]
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    return d


def stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def cmd_tuning_setup(env, args) -> None:
    state = sw.open_state()
    profile = load_profile(PROFILE)
    root = ps_quote(env["PC_TUNING_ROOT"])
    sw.guarded(sw.ssh_cmd(env), f"New-Item -ItemType Directory -Force -Path {root}, (Join-Path {root} 'runs') | Out-Null ; "
               f"& icacls.exe {root} /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' | Out-Null\n", 60, "finish")
    sw.scp(str(PROFILE), f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/profile.json")
    r = tps(env, f"(Read-IemProfile -Path {sw.tuning_profile(env)}).version", timeout=60)
    if baseline_path(env).is_file():
        state["fingerprint"] = str(baseline_path(env))   # to-event and preempt compare against it
        sw.save_state(state)
    print(json.dumps({"tuning-setup": {"profile_version": r, "local_version": profile["version"], "window": state["id"],
                                       "fingerprint": state.get("fingerprint")}}))


def cmd_inventory(env, args) -> None:
    state = sw.open_state()
    f = env["PC_TUNING_ROOT"] + f"\\inventory-{stamp()}.json"
    tps(env, f"$i = Get-IemInventory -ProfilePath {sw.tuning_profile(env)} ; [IO.File]::WriteAllText({ps_quote(f)}, ($i | ConvertTo-Json -Depth 8)) ; 'ok'",
        timeout=900, event="abandon")
    local = raw(env, state) / Path(f.replace("\\", "/")).name
    sw.scp(f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/{local.name}", str(local))
    print(json.dumps({"inventory": str(local), "bytes": local.stat().st_size}))


def cmd_fingerprint(env, args) -> None:
    state = sw.open_state()
    current = tps(env, f"Get-IemReaperFingerprint -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="abandon")
    if args.baseline:
        if state["card"] != "reaper":
            raise StepError("the baseline is read while REAPER runs (before to-dev)")
        path = baseline_path(env)
        path.parent.mkdir(parents=True, exist_ok=True)
        text = json.dumps(current, indent=1)
        (raw(env, state) / f"fingerprint-baseline-{stamp()}.json").write_text(text, encoding="utf-8")
        path.write_text(text, encoding="utf-8")
        state["fingerprint"] = str(path)
        sw.save_state(state)
        print(json.dumps({"fingerprint-baseline": str(path), "keys": len(current)}))
        return
    if not state.get("fingerprint"):
        raise StepError("no baseline in this window (fingerprint --baseline)")
    diff = sw.fingerprint_diff(json.loads(Path(state["fingerprint"]).read_text(encoding="utf-8")), current)
    print(json.dumps({"fingerprint-check": diff}))
    if diff:
        raise StepError("the fingerprint differs from the baseline: " + ", ".join(d["key"] for d in diff))


def cmd_wpt_install(env, args) -> None:
    state = sw.open_state()
    if state["card"] != "reaper":
        raise StepError("install WPT while REAPER still holds the card (before to-dev)")
    local = Path(env["RAW_DIR"]).expanduser() / "pc-tuning" / "adksetup.exe"
    local.parent.mkdir(parents=True, exist_ok=True)
    if not local.is_file():
        subprocess.run(["curl", "-fsSL", "-o", str(local), ADK_URL], check=True, timeout=300)
    sw.scp(str(local), f"{env['PC_SSH']}:{env['PC_TUNING_ROOT_SCP']}/adksetup.exe")
    r = tps(env, f"Install-IemWpt -Setup {ps_quote(env['PC_TUNING_ROOT'] + chr(92) + 'adksetup.exe')} -Xperf {xperf(env)}", timeout=1800)
    print(json.dumps({"wpt-install": r}))


def cmd_enter(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    only = mode_only(args.only)
    state["tuning_mode"] = True   # recorded before the action: preempt reverts even a half-done enter
    sw.save_state(state)
    rows = as_list(tps(env, f"Enter-IemTuningMode -ProfilePath {sw.tuning_profile(env)} -Only @({', '.join(ps_quote(x) for x in only)}) -Idle {ps_quote(args.idle)}", timeout=240))
    state.setdefault("tuning_steps", []).append({"enter": only, "idle": args.idle, "at": stamp()})
    sw.save_state(state)
    print(json.dumps({"enter": rows}))
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} mode item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_exit(env, args) -> None:
    state = sw.open_state()
    rows = as_list(tps(env, f"Exit-IemTuningMode -ProfilePath {sw.tuning_profile(env)}", timeout=240))
    state["tuning_mode"] = False
    sw.save_state(state)
    print(json.dumps({"exit": rows}))


def only_arg(text: str) -> str:
    groups = [x.strip() for x in text.split(",") if x.strip()]
    if any(not re.fullmatch(r"[a-z]+(:[a-z0-9-]+)?", g) for g in groups):
        raise StepError("--only takes group names such as services,updates or irq:card")
    return "@(" + ", ".join(ps_quote(g) for g in groups) + ")"


def cmd_apply(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    rows = as_list(tps(env, f"Invoke-IemTuningApply -ProfilePath {sw.tuning_profile(env)} -Tier {args.tier} -Only {only_arg(args.only)}", timeout=600))
    state.setdefault("tuning_steps", []).append({"apply": args.tier, "only": args.only, "at": stamp()})
    sw.save_state(state)
    print(json.dumps({"apply": rows}))
    failed = [r for r in rows if r.get("action") == "failed"]
    if failed:
        raise StepError(f"{len(failed)} item(s) failed: " + "; ".join(f"{r['key']}: {r['error']}" for r in failed))


def cmd_undo(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    rows = as_list(tps(env, f"Undo-IemTuning -ProfilePath {sw.tuning_profile(env)} -Tier {args.tier} -Only {only_arg(args.only)}", timeout=600))
    state.setdefault("tuning_steps", []).append({"undo": args.tier, "only": args.only, "at": stamp()})
    sw.save_state(state)
    print(json.dumps({"undo": rows}))


def cmd_state(env, args) -> None:
    print(json.dumps({"state": tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="abandon")}, indent=1))


def cmd_measure(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    profile = load_profile(PROFILE)
    if not label_ok(args.label):
        raise StepError("--label: lower-case letters, digits and dashes, at most 40")
    current = state["pref_current"] if state["pref_current"] is not None else state["pref_original"]
    if args.frames != current:
        raise StepError(f"the driver's preferred buffer is {current}: spike_window set-buffer --frames {args.frames} first")
    run_dir = env["PC_TUNING_ROOT"] + f"\\runs\\{args.label}-{stamp()}"
    since = tps(env, "Get-IemNow", timeout=60, event="abandon")
    tracing = args.trace != "none"
    if tracing:
        state["trace"] = run_dir   # recorded before the start: preempt stops it
        sw.save_state(state)
        opt = (" -CSwitch" if args.trace == "diag" else "") + (f" -CircularMB {args.circular_mb}" if args.circular_mb else "")
        tps(env, f"Start-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)}{opt}", timeout=120)
    polls: list[dict] = []
    cut = {"n": 0, "seen": 0}

    def on_poll(st: dict) -> None:
        status, progress = st.get("status") or {}, st.get("progress")
        pid = next((r.get("pid") for r in status.get("results") or [] if isinstance(r, dict) and r.get("pid")), 0)
        tid = (progress or {}).get("callback_thread", 0)
        polls.append(tps(env, f"Get-IemPollSample -ProfilePath {sw.tuning_profile(env)} -SpikePid {int(pid or 0)} -ThreadId {int(tid or 0)}",
                         timeout=60, event="abandon"))
        do_cut, cut["seen"] = should_cut(progress, cut["seen"], cut["n"], bool(tracing and args.circular_mb))
        if do_cut:
            cut["n"] += 1
            tps(env, f"Stop-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)} -Merge -Name 'cut-{cut['n']}.etl' ; "
                     f"Start-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)} -CircularMB {args.circular_mb}", timeout=300)

    run_args = argparse.Namespace(mode="duplex", frames=args.frames, seconds=args.seconds, burn_us=args.burn_us, stress=args.stress,
                                  panic_at=0, cycles=5, cpu=None, threshold_us=10, audio_cpus=args.audio_cpus, stress_cpus=args.stress_cpus)
    result = sw.cmd_run(env, run_args, on_poll=on_poll)
    state = sw.load_state()   # cmd_run saved its own changes (the run list): never overwrite them
    out = raw(env, state) / Path(run_dir.replace("\\", "/")).name
    out.mkdir(exist_ok=True)
    dpcisr_text = None
    if tracing:
        extra = " ; Export-IemNearGlitch -Xperf {x} -Dir {d}".format(x=xperf(env), d=ps_quote(run_dir)) if args.trace == "diag" else ""
        cuts = " ; ".join(f"Invoke-IemDpcIsr -Xperf {xperf(env)} -Dir {ps_quote(run_dir)} -Name 'cut-{i}.etl'" for i in range(1, cut["n"] + 1))
        tps(env, f"Stop-IemTrace -Xperf {xperf(env)} -Dir {ps_quote(run_dir)} -Merge ; Invoke-IemDpcIsr -Xperf {xperf(env)} -Dir {ps_quote(run_dir)}"
                 + (f" ; {cuts}" if cuts else "") + extra, timeout=1800)
        state["trace"] = None
        sw.save_state(state)
        scp_dir = env["PC_TUNING_ROOT_SCP"] + "/runs/" + out.name
        names = ["dpcisr.txt"] + [f"cut-{i}.dpcisr.txt" for i in range(1, cut["n"] + 1)] + (["near.txt"] if args.trace == "diag" else [])
        for name in names:
            sw.scp(f"{env['PC_SSH']}:{scp_dir}/{name}", str(out / name))
        dpcisr_text = (out / "dpcisr.txt").read_text(encoding="utf-8", errors="replace")
    events = as_list(tps(env, f"Get-IemSystemEvents -Since {ps_quote(since)}", timeout=120, event="abandon"))
    report = json.loads(Path(result["report"]).read_text(encoding="utf-8"))
    summary = lr.summarize(args.label, result["verdict"], report, dpcisr_text, polls, events, watch_lps(profile, args.audio_cpus))
    summary["cuts"] = [lr.budget_findings(lr.parse_dpcisr((out / f"cut-{i}.dpcisr.txt").read_text(encoding="utf-8", errors="replace")),
                                          watch_lps(profile, args.audio_cpus)) for i in range(1, cut["n"] + 1)]
    if args.trace == "diag":
        summary["near_glitch"] = lr.near_glitch((out / "near.txt").read_text(encoding="utf-8", errors="replace"), period_us=lr.PERIOD_US)
    (out / "summary.json").write_text(json.dumps(summary, indent=1), encoding="utf-8")
    state.setdefault("measurements", []).append({"label": args.label, "summary": str(out / "summary.json"), "stable": (result["verdict"] or {}).get("stable")})
    sw.save_state(state)
    print(json.dumps(summary))


def cmd_hwlat(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    rows = []
    for lp in parse_lps(args.lps):
        ns = argparse.Namespace(mode="hwlat", frames=None, seconds=args.seconds, burn_us=0, stress=0, panic_at=0, cycles=1,
                                cpu=lp, threshold_us=args.threshold_us, audio_cpus="", stress_cpus="")
        r = sw.cmd_run(env, ns)
        rows.append(lr.hwlat_summary(json.loads(Path(r["report"]).read_text(encoding="utf-8"))))
    path = raw(env, state) / f"hwlat-{stamp()}.json"
    path.write_text(json.dumps(rows, indent=1), encoding="utf-8")
    print(json.dumps({"hwlat": rows, "file": str(path)}))


def cmd_reboot_prepare(env, args) -> None:
    state = sw.open_state()
    need_free(state)
    running = sw.spike_running(env)
    done = sw.unwind(env, state, running, bring_back_reaper=False)
    st = tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120)
    state["card"] = "rebooting"
    state["reboot"] = {"prepared_at": tps(env, "Get-IemNow", timeout=60)}
    sw.save_state(state)
    items = as_list(st["items"])
    print(json.dumps({"reboot-prepare": done, "pending": [i["key"] for i in items if i["pending"]],
                      "revert_pending": [i["key"] for i in items if i["revert_pending"]]}))


def cmd_reboot(env, args) -> None:
    """Records the owner's quoted approval; without --by-owner it also asks
    Windows for a graceful restart in 60 s (never forced)."""
    state = sw.load_state()
    if state.get("closed") or state["card"] != "rebooting" or "reboot" not in state:
        raise StepError("run reboot-prepare first")
    check_approval(args.approval)
    state["reboot"]["approval"] = args.approval
    state["reboot"]["by"] = "owner" if args.by_owner else "agent"
    sw.save_state(state)
    if args.by_owner:
        print(json.dumps({"reboot": "the owner restarts the PC himself; run post-boot afterwards"}))
        return
    code = sw.ps(env, "& shutdown.exe /r /t 60 /c 'iemmixer S1c: owner-approved restart' ; $LASTEXITCODE", timeout=60, event="ignore")
    if int(code) != 0:
        raise StepError(f"shutdown.exe /r exited {code}: nothing restarts; tell the owner")
    print(json.dumps({"reboot": "requested", "in_s": 60}))


def cmd_post_boot(env, args) -> None:
    state = sw.load_state()
    if "approval" not in state.get("reboot", {}):
        raise StepError("no approved reboot recorded in this window (reboot --approval ...)")
    deadline = time.monotonic() + 900
    while subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"], "exit"], capture_output=True, check=False).returncode != 0:
        if time.monotonic() > deadline:
            sw.alarm("the PC is not reachable 15 min after the approved reboot: tell the owner (power cycle is his)")
            raise StepError("PC unreachable after the reboot")
        time.sleep(15)
    boot = tps(env, "Get-IemBootTime", timeout=60, event="ignore")
    checks = {"booted_after_request": boot > state["reboot"]["prepared_at"], "reaper": False, "handover": None,
              "fingerprint": [], "pending": [], "failed_items": []}
    for _ in range(30):
        if int(sw.ps(env, "@(Get-Process reaper -ErrorAction SilentlyContinue).Count", timeout=60, event="ignore")) > 0:
            checks["reaper"] = True
            break
        time.sleep(10)
    if checks["reaper"]:
        try:
            checks["handover"] = sw.bring_back(env, state)
        except StepError as e:
            checks["handover"] = {"error": str(e)}
    current = tps(env, f"Get-IemReaperFingerprint -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="ignore")
    checks["fingerprint"] = sw.fingerprint_diff(json.loads(baseline_path(env).read_text(encoding="utf-8")), current)
    st = tps(env, f"Get-IemTuningState -ProfilePath {sw.tuning_profile(env)}", timeout=120, event="ignore")
    items = as_list(st["items"])
    checks["pending"] = [i["key"] for i in items if i["pending"] or i["revert_pending"]]
    checks["failed_items"] = [i["key"] for i in items if i["journaled"] and not i["ok"]]
    a = tps(env, "Get-IemCpuSample", timeout=60, event="ignore")
    time.sleep(10)
    b = tps(env, "Get-IemCpuSample", timeout=60, event="ignore")
    checks["interrupts"] = lr.cpu_rates([a, b])
    problems = post_boot_verdict(checks)
    state["card"], state["closed"] = "reaper", True
    state["post_boot"] = {"checks": checks, "problems": problems}
    sw.save_state(state)
    print(json.dumps({"post-boot": checks, "problems": problems}))
    if problems:
        sw.alarm("after the approved reboot: " + "; ".join(problems) + ". Revert: tuning_window undo --tier 3 in a dev window, "
                 "then the pre-approved revert reboot.")
        raise StepError("post-boot checks failed")


STEPS = ("tuning-setup", "enter", "exit", "apply", "undo", "measure", "hwlat", "reboot-prepare")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("tuning-setup", "inventory", "wpt-install", "exit", "state", "reboot-prepare", "post-boot"):
        sub.add_parser(name)
    fp = sub.add_parser("fingerprint")
    g = fp.add_mutually_exclusive_group(required=True)
    g.add_argument("--baseline", action="store_true")
    g.add_argument("--check", action="store_true")
    en = sub.add_parser("enter")
    en.add_argument("--only", default="plan,governor,placement")
    en.add_argument("--idle", choices=("default", "c1", "disable"), default="default")
    for name in ("apply", "undo"):
        p = sub.add_parser(name)
        p.add_argument("--tier", type=int, choices=(2, 3), required=True)
        p.add_argument("--only", default="")
    m = sub.add_parser("measure")
    m.add_argument("--label", required=True)
    m.add_argument("--frames", type=int, default=32)
    m.add_argument("--seconds", type=int, default=600)
    m.add_argument("--burn-us", type=int, default=0)
    m.add_argument("--stress", type=int, default=0)
    m.add_argument("--audio-cpus", default="")
    m.add_argument("--stress-cpus", default="")
    m.add_argument("--trace", choices=("none", "dpc", "diag"), default="dpc")
    m.add_argument("--circular-mb", type=int, default=0)
    h = sub.add_parser("hwlat")
    h.add_argument("--lps", default="0-15")
    h.add_argument("--seconds", type=int, default=30)
    h.add_argument("--threshold-us", type=int, default=10)
    rb = sub.add_parser("reboot")
    rb.add_argument("--approval", required=True)
    rb.add_argument("--by-owner", action="store_true")
    args = ap.parse_args(argv)
    handlers = {"tuning-setup": cmd_tuning_setup, "inventory": cmd_inventory, "fingerprint": cmd_fingerprint, "wpt-install": cmd_wpt_install,
                "enter": cmd_enter, "exit": cmd_exit, "apply": cmd_apply, "undo": cmd_undo, "state": cmd_state, "measure": cmd_measure,
                "hwlat": cmd_hwlat, "reboot-prepare": cmd_reboot_prepare, "reboot": cmd_reboot, "post-boot": cmd_post_boot}
    try:
        env = sw.load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
    except StepError as e:
        print(f"tuning_window: {e}", file=sys.stderr)
        return 1
    try:
        handlers[args.cmd](env, args)
        return 0
    except sw.EventNow:
        print(json.dumps({"event": "ide event (flag file)", "action": "preempt"}), flush=True)
    except StepError as e:
        print(f"tuning_window: {e}", file=sys.stderr, flush=True)
        if args.cmd not in STEPS or not sw.event_now():
            return 1
        print(json.dumps({"event": "ide event (flag file) after a failed step", "action": "preempt"}), flush=True)
    try:
        sw.cmd_preempt(env)
    except StepError as e:
        print(f"tuning_window: preempt: {e}", file=sys.stderr)
        return 1
    return 10


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
```

- [ ] **Step 3: Tests pass; scan; commit.**

```bash
cd "$WORK" && python3 -m unittest discover -s scripts/pc-tuning -p 'test_*.py' -v 2>&1 | tail -3 && python3 scripts/check_integrity.py
git add scripts/pc-tuning/tuning_window.py scripts/pc-tuning/test_tuning_window.py
git commit -m "feat(s1c): tuning window commands (setup, inventory, fingerprint, WPT, mode, tiers, measure, hwlat, reboot)

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Expected: `OK`, and `integrity: clean`.

---

### Task 11: CI — tuning self-test, bundle, script tests

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: `integrity` runs the new Python tests.** In the step `Script self-tests`, after the `scripts/asio-spike` line, add:

```yaml
          python3 -m unittest discover -s scripts/pc-tuning -p 'test_*.py' -v
```

- [ ] **Step 2: The `asio-spike` job.**
  1. After the step `Spike PC module self-test ...`, add:

```yaml
      - name: Tuning module self-test (Windows PowerShell 5.1, real backends on the ephemeral runner)
        shell: powershell
        run: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/pc-tuning/Test-IemTuning.ps1
```

  2. In the step `Bundle (spike, PC scripts, SHA256SUMS)`, extend the `Copy-Item -LiteralPath` list with `scripts/pc-tuning/IemTuning.psm1, scripts/pc-tuning/IemMeasure.psm1`.

- [ ] **Step 3: Check and commit.**

```bash
cd "$WORK" && python3 scripts/check_integrity.py && python3 -m unittest scripts/test_check_integrity.py 2>&1 | tail -1
git add .github/workflows/ci.yml
git commit -m "ci(s1c): tuning self-test and modules in the asio-spike bundle; pc-tuning tests in integrity

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Private profile, runbooks, playbook rule, router

**Files:**
- **Private:**
  - `$PRIV/pc-tuning.json` (chmod 600);
  - `$SPIKE_ENV` (three keys);
  - `$OPS/docs/s1c-pc-runbook.md`;
  - `$PRIV/event-runbook.md` (+ ops `CLAUDE.md` mirror).
- **Public:** `.claude/rules/pc-tuning.md`, `CLAUDE.md`.

- [ ] **Step 1: The env keys.** Append to `$SPIKE_ENV`:

| Key | Value |
|---|---|
| `PC_TUNING_ROOT` | `C:\ProgramData\iemmixer\tuning` |
| `PC_TUNING_ROOT_SCP` | the same folder in the scp notation the file uses for `PC_ROOT_SCP` |
| `PC_XPERF` | `C:\Program Files (x86)\Windows Kits\10\Windows Performance Toolkit\xperf.exe` |

- [ ] **Step 2: The profile.** Write `$PRIV/pc-tuning.json` (chmod 600). The public shape follows; the private values in `<…>` come from W1's inventory (Task 14 Step 4 fills them). Until then the profile holds the placeholders and `devices` stays empty.

```json
{
  "version": 1,
  "journal": "C:\\ProgramData\\iemmixer\\tuning\\journal.json",
  "registry_root": "",
  "layout": { "housekeeping": [0, 1, 6, 7, 8, 9, 10, 11, 12, 13], "card": [2], "nic": [4, 5], "audio": [14] },
  "plan": { "guid": "6c1b0d6e-0a39-4f55-9c2a-1e5a3b7d9c01", "source": "<the REAPER-mode plan GUID, W1 inventory: power.active while REAPER runs>" },
  "governor": "<the Process Lasso governor service name, W1 inventory: services>",
  "placement": ["<tunnel>", "<remote desktop>", "<remote-control agent>", "<clock sync>", "<Audinate services>", "<predecessor app>", "<predecessor runner>", "<Process Lasso GUI>"],
  "services_disable": ["SysMain", "DPS", "WSearch", "DiagTrack"],
  "services_mode": [],
  "updates": { "services": ["wuauserv"], "tasks": ["\\Microsoft\\Windows\\UpdateOrchestrator\\Schedule Scan"] },
  "maintenance": { "off": true, "tasks": [
    "\\Microsoft\\Windows\\Defrag\\ScheduledDefrag",
    "\\Microsoft\\Windows\\DiskDiagnostic\\Microsoft-Windows-DiskDiagnosticDataCollector",
    "\\Microsoft\\Windows\\Application Experience\\Microsoft Compatibility Appraiser",
    "\\Microsoft\\Windows\\Customer Experience Improvement Program\\Consolidator",
    "\\Microsoft\\Windows\\Maintenance\\WinSAT",
    "\\Microsoft\\Windows\\Power Efficiency Diagnostics\\AnalyzeSystem",
    "\\Microsoft\\Windows\\Diagnosis\\Scheduled",
    "<vendor updater tasks, W1 inventory: tasks>"] },
  "defender": { "paths": ["<the spike root>", "C:\\ProgramData\\iemmixer\\tuning"], "processes": ["asio_spike.exe"] },
  "devices": [ { "id": "card", "instance": "<the card's PCI instance path>", "hwid": "<its hardware id prefix>", "lps": [2], "enabled": true } ],
  "nic": { "adapter": "<adapter name>", "properties": { "<power-saving keyword>": "0" }, "rss": { "base": 4, "max": 5 }, "pnp_capabilities": 24 },
  "fingerprint": { "files": ["<the Process Lasso config file>"], "keys": [] }
}
```

  Check it:

```bash
chmod 600 "$TUNING_PROFILE" && python3 -c "
import sys, pathlib; sys.path.insert(0, '$WORK/scripts/pc-tuning'); import tuning_window as t
print(sorted(t.load_profile(pathlib.Path('$TUNING_PROFILE'))))"
```

- [ ] **Step 3: The ops runbook.** Write `$OPS/docs/s1c-pc-runbook.md` (PRIVATE) with:
  - the Global Constraints;
  - the env keys and the profile with their real values and where each was read (inventory file name);
  - the window command sequences of Tasks 14–18 with real names;
  - the reboot question's text;
  - the post-boot and revert procedure;
  - the alarm texts: tuning exit failed, fingerprint differs, trace did not stop, PC unreachable after the reboot.

  Commit and push the ops repo per its `CLAUDE.md`.
- [ ] **Step 4: The event runbook.** In `$PRIV/event-runbook.md`, "ide event": after the flag and `$S preempt`, add a line: `preempt` also stops an S1c trace and reverts the mode levers (it reads the window state). If `$T state` shows `entered: true` after it, run `$T exit` and alarm on failure. Mirror the line into `$OPS/CLAUDE.md` and commit it there.
- [ ] **Step 5: Playbook rule.** Create `.claude/rules/pc-tuning.md`:

```markdown
---
paths:
  - "scripts/pc-tuning/**"
  - "crates/iem-audio-io/src/os.rs"
  - "crates/iem-audio-io/src/cpuset.rs"
---

# PC tuning (S1c, #15)

- Design note `docs/superpowers/specs/2026-09-27-s1c-windows-tuning-design.md`; plan `docs/superpowers/plans/2026-09-27-s1c-windows-tuning.md`.
- Every change is an item (kind + args + desired) read by `Get-IemValue` and written by `Set-IemValue`: apply writes only what differs, journals the before-value first (journal saved before the write), reads back; undo/exit restore from the journal alone. No `GetNewClosure` (it loses the module's private functions).
- Mode levers (plan, governor pause, placement, mode services) are entered only with the card free and reverted before REAPER starts; `spike_window.undo_plan` orders `stop-spike`, `trace-stop`, `tuning-exit`, `restore-buffer`, `bring-back`, `fingerprint`, with the flags `trace` / `tuning_mode` recorded before their actions. A failed exit or fingerprint alarms and never holds REAPER back.
- Global items: Tier 2 without reboot, Tier 3 only through the owner-approved reboot (`reboot-prepare`, `reboot --approval "<owner quote with time>"`, `post-boot`); Tier 4 only after an owner decision on #15.
- REAPER mode = the fingerprint (`Get-IemReaperFingerprint`) equal to the W1 baseline plus the declared Tier 2–3 items; the iemmixer plan is a duplicate, the REAPER-mode plan is never edited; Process Lasso's config is never edited.
- The inventory never reads process command lines, service image paths or task actions. System-log events are counted by provider and id, without messages.
- Power-plan values go through powrprof (`IemPower`), counters through WMI raw classes: both language-neutral. xperf text is English; its parsers in `latency_report.py` were written from the documented format; fix them with a RED test from a scrubbed real excerpt when the PC's output differs.
- `Test-IemTuning.ps1` runs on real backends on the CI runner (HKCU test root, Spooler/W32Time, a test task, a duplicated plan, Defender, child-process CPU Sets); never add a skip for a missing backend.
- Site values (device instances, adapter, process/service/task names of site software, the REAPER-mode plan, paths) only in `~/.config/iemmixer/pc-tuning.json`, the env file and `iemmixer-ops/docs/s1c-pc-runbook.md`.
```

- [ ] **Step 6: Router.** In `CLAUDE.md` "Playbook router", add:

```markdown
- PC tuning (S1c): levers, journal, mode enter/exit, fingerprint, measurement → `.claude/rules/pc-tuning.md`
```

- [ ] **Step 7: Scan and commit.**

```bash
cd "$WORK" && git add .claude/rules/pc-tuning.md CLAUDE.md
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$(git write-tree)"
git commit -m "docs(s1c): playbook rule and router line

Refs #15

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Expected: the scan prints no finding.

---

### Task 13: First push, CI green, the bundle (main session)

- [ ] **Step 1: Pre-push checks, then push.**

```bash
cd "$WORK" && git fetch origin && git merge --ff-only origin/dev && git status -sb
cargo fmt --all -- --check && python3 scripts/check_integrity.py && python3 scripts/check_engine_deps.py && python3 scripts/check_version.py
for d in scripts scripts/asio-spike scripts/pc-tuning; do python3 -m unittest discover -s "$d" -p 'test_*.py' 2>&1 | tail -1; done
git push origin dev
```

- [ ] **Step 2: Wait for every job** in one foreground bounded loop per Bash call (≤ 9 min). Repeat the call until the run is terminal; never `run_in_background`:

```bash
RUN=$(gh run list -R "$REPO" --branch dev --event push --limit 1 --json databaseId --jq '.[0].databaseId'); echo "$RUN"
for i in $(seq 1 53); do s=$(gh run view "$RUN" -R "$REPO" --json status,conclusion --jq '.status+" "+(.conclusion // "")'); echo "$(date +%T) $s"; case "$s" in completed*) break;; esac; sleep 10; done
gh run view "$RUN" -R "$REPO" --json jobs --jq '.jobs[] | .name+": "+(.conclusion // .status)'
```

Every job must be `success`. On failure, read `gh run view "$RUN" -R "$REPO" --log-failed` and fix every finding in ONE commit (`fix(s1c): first CI cycle — …`).

Likely first-cycle items:
- a `windows-sys` import path or a clippy lint in `os.rs` (read the lint);
- the `Test-IemTuning.ps1` cases on the runner's real services, Defender or plan (fix the module, never the assertion);
- `mutants-list` over the shard budget (resize the `shard:` matrix, `ci-rust-toolchain.md`);
- rustfmt.

- [ ] **Step 3: The bundle.** `SHA=$(git rev-parse HEAD); $S fetch-bundle --sha "$SHA"`. Expected: six files, including `IemMeasure.psm1` and `IemTuning.psm1`. Post the SHA, the run id, the coverage line and the `mutants-list` count on #15 (Slovak, short).

---

### Task 14: W1 — inventory, fingerprint baseline, WPT, baseline measurements (dev time)

**Precondition:**
- the owner's "event skončil" is in this conversation after the last "ide event";
- `EVENT-NOW` does not exist;
- if S1a Task 12 has not run, run its Steps 1–6 first in this window (`$S new`, `setup`, `preflight`, `to-dev`, the S1a runs), then `$S set-buffer --frames 32` and continue here from Step 5 with the card free. Steps 2–4 then run in the *next* window before `to-dev`: they need REAPER running.

Runtime: about 2 h. Every step's JSON goes to #15 as numbers only.

- [ ] **Step 1: Open, set up, preflight.**

```bash
$S new --signal "<owner, HH:MM: event skončil …>"
$S setup --sha "$SHA"
$S preflight
$T tuning-setup
```

- [ ] **Step 2: Inventory** (REAPER running, read-only).

```bash
$T inventory
```

  Read it; record on #15 generic facts only:
  - MSI in use or not for the card;
  - where its interrupt lands (from `irqs`);
  - the plan while REAPER runs;
  - the timer resolution;
  - VBS state;
  - fTPM present;
  - NIC link speed.

- [ ] **Step 3: Fill the profile from the inventory.** Fill in `$TUNING_PROFILE`:
  - `plan.source` = `power.active` while REAPER runs;
  - `governor` = the Process Lasso governor service name;
  - `placement` process names;
  - vendor updater tasks;
  - the card's `instance` and `hwid`;
  - the adapter name and its power-saving keywords (`advanced` rows);
  - the Process Lasso config file for `fingerprint.files`.

  Then run `$T tuning-setup` again.

- [ ] **Step 4: Fingerprint baseline and WPT** (REAPER still running).

```bash
$T fingerprint --baseline
$T wpt-install
$T fingerprint --check
```

  Expected:
  - `wpt-install` gives `installed: now` with a version; the ADK bootstrapper's signature is checked on the PC.
  - The second fingerprint read (after the install) shows whether the Process Lasso config is stable. If it differs, set `fingerprint.keys` to its rule-line patterns (design note §5.1), run `$T tuning-setup` and `$T fingerprint --baseline` again, and record it on #15.

- [ ] **Step 5: Switch and set 32.** `$S to-dev`, then `$S set-buffer --frames 32` (skip both if S1a's part of this window already did).

- [ ] **Step 6: Baseline measurement set** (no tuning; design note §4.3).

```bash
$T hwlat --lps 0-15 --seconds 30
$T measure --label base-idle --seconds 600 --trace dpc
$T measure --label base-load --seconds 600 --burn-us 40 --stress 4 --trace dpc
$T measure --label base-headroom --seconds 600 --burn-us 200 --stress 4 --trace dpc
```

  Glitches whose summary names no module get a 2-min diagnostic: `$T measure --label base-diag --seconds 120 --burn-us 40 --stress 4 --trace diag`.

  Validate the parsers against the real texts. If `dpcisr.txt` or `near.txt` differs from the documented format (the summary shows no modules while the text has them), write a RED test from a scrubbed excerpt (module names kept, no host or path), fix `latency_report.py` in a GREEN commit, push (one cycle) and re-run the summary from the raw files.

- [ ] **Step 7: Back to event.** `$S to-event`. Expected:
  - `trace-stop` is absent: nothing is running;
  - `restore-buffer` reads back the original;
  - `bring-back` passes;
  - `fingerprint: []`.

  Any alarm goes to the owner (❓).

- [ ] **Step 8: Baseline on #15** (Slovak): hwlat maxima per CPU; per run the verdict, glitches, top DPC/ISR modules, the per-CPU interrupt rates of the card's CPU and the callback's CPU, the callback thread's priority, and the System-log events.

---

### Task 15: W2 — Tier 1 A/B, Tier 2, Tier 3 written, reboot-prepare (dev time)

**Precondition:** as Task 14, a new window. Runtime: about 2.5 h.

- [ ] **Step 1: Open and switch.** `$S new --signal "…"`, `$S setup --sha "$SHA"` (only when the bundle changed), `$S preflight`, `$T tuning-setup` (records the fingerprint baseline in the window), `$S to-dev`, `$S set-buffer --frames 32`.
- [ ] **Step 2: Tier 1 A/B.** Each step is `$T exit` (harmless when nothing is entered), then `$T enter …`, then the same pair of runs with the label shown:

| Step | Enter | Runs (`--seconds 600 --trace dpc`, load = `--burn-us 40 --stress 4 --stress-cpus 6-13`) |
|---|---|---|
| a | `--only plan --idle default` | `t1a-idle`, `t1a-load` |
| b | `--only plan --idle c1` | `t1b-idle`, `t1b-load` |
| c | `--only plan --idle disable` | `t1c-idle`, `t1c-load` |
| d | best of a–c + `governor` | `t1d-load` |
| e | d + `placement` | `t1e-load` |
| f | e; engine CPU Set A/B | `t1f-none-load` (no `--audio-cpus`), `t1f-14-load` (`--audio-cpus 14`), `t1f-3-load` (`--audio-cpus 3`) |

  Decide with the design note §4.4 budgets:
  - the idle variant: the C-state counters and `busy_pct`/frequency in the summaries show the thermal cost of `disable`;
  - the engine placement.

  Put both decisions into the profile (a comment on #15 with the numbers).
- [ ] **Step 3: Tier 2.**

```bash
$T apply --tier 2
$T measure --label t2-load --seconds 600 --burn-us 40 --stress 4 --stress-cpus 6-13 --audio-cpus <chosen> --trace dpc
```

  A refused update-orchestrator task (`failed`, access denied) is expected on some builds: remove it from the profile, record it on #15, re-run `apply --tier 2` until no row fails.
- [ ] **Step 4: Tier 3 written** (pending until the reboot). `$T apply --tier 3`. Then `$T state`: every Tier 3 item `ok` and `pending`.
- [ ] **Step 5: Prepare the reboot.** `$T reboot-prepare`. The spike stops, mode levers exit, the buffer returns to the original, and the card stays free (REAPER is not started: it starts at boot). The output lists `pending`.
- [ ] **Step 6: The owner question** (one ❓, Slovak, self-contained; the design note §6.4 argument in plain words):
  - which settings (the card's and the NIC's interrupt placement, NIC power saving);
  - why they need the reboot;
  - that REAPER comes back by itself;
  - that the agent checks REAPER, the card and the fingerprint after it;
  - that one revert reboot is pre-approved if a check fails.

  Options: the agent restarts gracefully now (recommended), or the owner restarts it himself. Then continue with Task 16. Until the answer the PC stays with the card free and REAPER off: dev time, no event.

---

### Task 16: The approved reboot and post-boot checks

- [ ] **Step 1: Reboot** (only after the owner's explicit "áno"):

```bash
$T reboot --approval "<owner, HH:MM: …>"
```

  If the owner restarts the PC himself, record his message with `$T reboot --approval "<owner, HH:MM: …>" --by-owner` (no restart is requested). `post-boot` accepts any boot after `reboot-prepare`: the recorded time is `reboot-prepare`'s.

- [ ] **Step 2: Post-boot** (about 2–5 min later):

```bash
$T post-boot
```

  Expected:
  - `booted_after_request` true; REAPER started by itself;
  - handover passes;
  - `fingerprint: []`, `pending: []`, `failed_items: []`;
  - the card's CPU (LP 2) shows its interrupt rate in `interrupts` (≈ 3000/s when one interrupt per 32-sample transfer).

  On any problem, the PC is in event mode and REAPER may be degraded: alarm the owner at once. At the next dev window: `$T undo --tier 3`, then `reboot-prepare` → the pre-approved revert reboot → `post-boot`.
- [ ] **Step 3:** Record on #15 (Slovak): the post-boot result and the interrupt placement. The PC stays in event mode; development continues at the next "event skončil".

---

### Task 17: W3 — after Tier 3, gate runs, Tier 4 only with a cause (dev time)

- [ ] **Step 1: Open, switch, enter with the chosen levers.** `$S new --signal "…"`, `$S preflight`, `$T tuning-setup`, `$S to-dev`, `$S set-buffer --frames 32`, `$T enter --only plan,governor,placement --idle <chosen>`.
- [ ] **Step 2: After Tier 3.** Run `$T hwlat --lps 0-15 --seconds 30` and `$T measure --label t3-load --seconds 600 --burn-us 40 --stress 4 --stress-cpus 6-13 --audio-cpus <chosen> --trace dpc`. Confirm the card's ISR/DPC run on LP 2 (usage lines) and the NIC's on LP 4–5.
- [ ] **Step 3: Gate runs.** Three consecutive `--seconds 1800` runs with the same load:
  - `gate-1` with `--trace dpc`;
  - `gate-2` and `gate-3` with `--trace none`.

  All three must be stable; the traced one must have no finding.
- [ ] **Step 4: If the gate fails.**
  1. Run `--trace diag` for 2 min and read `near_glitch`.
  2. Name the cause on #15 with numbers.
  3. When the cause maps to a Tier 4 item (design note §6.5), ask the owner one ❓ per item, with its trade-off and the numbers. Nothing in Tier 4 runs without his answer.
  4. Otherwise fix the lever and repeat the gate.
- [ ] **Step 5: Leave.** `$S to-event` (fingerprint `[]`).

---

### Task 18: W4 — the 8 h proxy soak (dev time)

- [ ] **Step 1: Open, switch, enter** as Task 17 Step 1.
- [ ] **Step 2: Soak.**

```bash
$T measure --label soak-proxy --seconds 28800 --burn-us 40 --stress 4 --stress-cpus 6-13 --audio-cpus <chosen> --trace dpc --circular-mb 1024
```

  Run it in the foreground in bounded pieces. The command itself runs 8 h, so run it through the session's normal long-command path: a foreground Bash call is limited to 10 min. Start it with `nohup … > "$RAW/pc-tuning/soak-<stamp>.log" 2>&1 &` inside one Bash call. Then poll that log with foreground bounded loops (≤ 9 min per call) until the `summary` line appears or the process exits. Before each poll, check that the process is alive. If it died, `$S status`, then `$S preempt` if the window is open.

  "ide event" stops it (pre-emption) and the soak starts from zero in the next window.
- [ ] **Step 3: Verdict.** Stable over 8 h with 0 missed periods → the S1c gate holds. Any glitch: the cut traces name the cause, then back to Task 17 Step 4.
- [ ] **Step 4:** `$S to-event`; numbers on #15.

---

### Task 19: Report, results, hand-offs (main session)

- [ ] **Step 1: The report on #15** (Slovak, plain, numbers):
  - before/after per tier: verdicts, glitches, DPC/ISR maxima of the top modules, the per-CPU interrupt rates of the card's and the callback's CPU, hwlat maxima;
  - the chosen idle variant and engine placement;
  - the Tier 3 post-boot result;
  - the soak verdict;
  - the fingerprint results;
  - the Tier 4 decisions (if any).

  Tick #15's second box ("DPC/ISR measured before/after") on evidence.
- [ ] **Step 2: Results in the design note.** Add `## 11. Results` (numbers, decisions, parser corrections, the timeline finding on "what degraded 32" if one was found). Commit `docs(s1c): results` (`Refs #15`) and push (one CI cycle, bounded wait).
- [ ] **Step 3: Hand-offs** (comments, English):
  - **#9 (S6):**
    - the guard owns `IemTuning.psm1`: `Enter` after REAPER quits, `Exit` in `iemmode event` and at logon, `Get-IemTuningState` on every `iemmode` call with a drift alarm, Tier 2 re-applied on a new profile version, Tier 3 never on its own;
    - the engine applies L5 through `iem_audio_io::os` (power throttling, the chosen CPU Set, helpers below the measured callback priority, locked RT memory);
    - the spike's `hwlat` and markers stay available for HIL.
  - **#10 (S7):** the real-load soak method (Task 20) and the summary format.

  Tick #15's third box when S6 has wired the module into install and the guard: it stays open until then, with a link to the S6 item.

---

### Task 20: W6 — the real-load 8 h soak (acceptance; needs S6's ASIO backend)

- [ ] **Step 1: Precondition.** S6 ships the engine on the card, the server and the guard's mode switch. The tuning profile version equals the one of Task 18, and `$T state` shows no drift.
- [ ] **Step 2: Soak** in `dev` through the guard:
  - the engine at 32 on the card; the server;
  - one engineer listen stream and one mixer client connected for 8 h (the S7 client harness);
  - the engine's own telemetry supplies the counts (the same missed/overrun/position-gap definitions);
  - the S1c sentinels and a circular DPC trace run alongside (`tuning_window.py` against the engine's pid, or the S6 equivalent).
- [ ] **Step 3:** 0 missed periods over ≥ 8 h ticks #15's first box. Numbers on #15.
- [ ] **Step 4: PR and merge** (only when the run's orchestrator asks):
  - update the required checks if a job was added;
  - open `dev` → `main` with the summary and `Refs #15`;
  - wait for every check including the mutation shards, kill survivors;
  - `gh pr merge --merge` per `pr-merge-policy`;
  - bump `dev` first thing after the merge.

## Hand-off to later sub-projects

- **S6 (#9):**
  - the module and its journal are the guard's to call;
  - `os.rs` is the engine's placement/priority layer;
  - the unwind order (trace, tuning, buffer, REAPER, fingerprint) carries over to `iemmode event`.
- **S7 (#10):** the soak method and per-run summaries; hwlat per CPU as a HIL health probe.
- **S8 (#11):** after cutover REAPER mode is rollback-only; the Tier 4 list may be re-weighed with the owner.
