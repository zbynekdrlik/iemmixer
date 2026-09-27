# iemmixer S1a — ASIO Spike Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) or superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Tasks 11–14 (pushes, CI waits, the PC window, results, PR) run in the main session, never in a subagent.

**Goal:** Prove the azo 0.2.1 ASIO host on the IEM PC's Dante card at 96 kHz. Measure driver facts, callback timing, missed and late periods, callback CPU, drift and reopen times at 32, 48 and 64 samples. Decide azo or a fallback. Verify the interim switch REAPER → iemmixer → REAPER both ways (ticket #3, program #1).

**Architecture:** Three layers.

- **`iem-audio-io`:**
  - `format.rs` and `telemetry.rs`: portable and fully tested.
  - `asio.rs`: Windows only, the crate's only unsafe code.
  - `examples/asio_spike.rs`: the program with the `probe`, `duplex` and `reopen` modes.
- **CI:** the job `asio-spike` (hosted `windows-2025`) builds, tests and uploads the bundle `asio-spike-<sha>`: the exe, the PC module and task script, `GoldenPc.psm1` and `SHA256SUMS`.
- **The window:**
  - On the dev box, `scripts/asio-spike/spike_window.py` runs the window over ssh.
  - On the PC, `scripts/asio-spike/SpikePc.psm1` and `spike-task.ps1` run under one Interactive task, `\iemmixer\iemmixer-asio-spike`.
  - The driver checks the "ide event" flag every 2 s. It restores the driver's preferred buffer with read-back before REAPER comes back.

**Tech Stack:** Rust 1.98.1 (edition 2024). New crates:
- `azo` =0.2.1 and `azo-sys` 0.2.1 (MIT), Windows only;
- `windows-sys` 0.61 features `Win32_Foundation` and `Win32_UI_WindowsAndMessaging`;
- `serde_json` as a dev-dependency (the example's report).

Also Windows PowerShell 5.1 (the PC and the CI job), Python 3.12 (stdlib) and GitHub Actions (hosted only).

**Spec:** program spec §2.2, §4.3, I2, I3, I8, P5, P6, P10, R1, R6. Design note: `docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md`. Owner scope on #3: 2026-09-26, measure for iemmixer only; the target is 32 (#15). Prepared design: #3, comment of 2026-09-27 04:34.

**Detail sources (private, never committed):**

- `~/devel/iemmixer-ops/docs/s1b-pc-runbook.md` (PC access, REAPER save and quit, the ASIO module name);
- `~/devel/iemmixer-ops/docs/archive/10-site-appendix-private-draft.md` (the driver's preference key: the `PrefBuffSize` line);
- `~/.config/iemmixer/event-runbook.md` (own start task, handover lesson);
- `~/devel/reaperiem` at its pinned SHA, read only (meter bridge state and heartbeat keys, `scripts/reascripts/meter_bridge.lua`).

## Global Constraints

- **The PC only in dev time:**
  - A window starts only after the owner's "event skončil" arrived in this conversation after the last "ide event", and while `~/.config/iemmixer/EVENT-NOW` does not exist.
  - The session creates that flag (with the time) the moment "ide event" arrives, before anything else, and removes it on "event skončil".
  - Never infer an event, never ask whether one runs.
- **"ide event" during a window:**
  - The flag makes any running `spike_window.py` pre-empt within 2 s.
  - If none runs, run `python3 scripts/asio-spike/spike_window.py preempt` at once.
  - Then run the event runbook's checks and confirm to the owner.
- **The card:**
  - The host never calls `set_sample_rate`, `set_clock_source` or `open_control_panel`; `scripts/check_integrity.py` fails on them.
  - Only 96 kHz is accepted, and only the driver's preferred buffer, which `set-buffer` wrote.
  - Every output is zeroed (A1). Dante is never touched.
  - REAPER keeps its value: the preferred buffer is restored and read back before REAPER starts (owner, #3).
- **I3:** the PC task refuses while `reaper.exe` runs or anything holds the ASIO module. REAPER never starts while `asio_spike.exe` exists.
- **I8, never force:**
  - The spike stops only through its stop file; REAPER quits by action 40004 after saving (40026); the predecessor app keeps running.
  - The words `taskkill`, `Stop-Process`, `TerminateProcess` and `shutdown /f` never appear (the `integrity` scan).
  - A spike that does not stop within 60 s is an owner alarm; the last resort is the owner's reboot, which comes back in event mode.
- **Owner approval first (not in this plan's windows):** OS restart with the engine running, reboot with the engine parked, the hard kill, SEH injection (`seh_ctl`), round-trip latency (D5 loopback).
- **P5:** only the `asio-spike-<sha>` artifact of a green `push` run on `dev` whose head is the reviewed commit reaches the PC. `SHA256SUMS` is checked on the dev box (`fetch-bundle`, `setup`) and on the PC before every run.
- **P6:** no site value in this repository — no host, user, path, registry key, driver or task name, track count or EXTSTATE key. They live in `$PRIV/asio-spike.env` and the ops runbook. Raw reports stay in `$RAW/asio-spike/` (chmod 700).
- **Tier 0:** no local cargo compilation. Locally only `cargo fmt`, `cargo metadata`, `cargo tree` and Python. Rust and PowerShell are proven in hosted CI (Task 11): one push per cycle, one fix commit per failing cycle, foreground bounded waits (≤ 9 min per Bash call), never `run_in_background`.
- **Tests:**
  - Every change ships tests that can fail; no `#[ignore]`, no skips, no `continue-on-error`.
  - The coverage floor never drops.
  - The mutation gate is diff-scoped: resize the shard matrix if `mutants-list` says so, never raise a timeout.
- **Branches and identity:**
  - `dev` only until Task 14.
  - Noreply identity. Every commit carries `Refs #3` and ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - The PR body (Task 14) ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- **Durable state:** decisions and findings go on #3 the moment they land. Raw reports, bundles and the window state live outside the repo and outside `/tmp`.

## Review Focus

1. **Use-after-free by a late callback.**
   - Expected: `Running::finish` stops the driver, clears `STREAM`, spins until `IN_FLIGHT == 0`, disposes the buffers, and only then frees the `Box`.
   - `with_stream` increments `IN_FLIGHT` before it reads the slot (both SeqCst).
   - `Drop` for `Running` runs the same path.
   - Proof: code review. The PC window exercises it 5× in `reopen` and on every run.
2. **Sound on the band's channels.**
   - Expected: every output channel has a buffer zeroed before `start()` and at the top of every callback. After a caught panic the outputs are zeroed again and the work never runs again.
   - Tests: `format` round trips; on the PC, the `--panic-at` run reports `callbacks_after_fault > 0`.
3. **A driver call off the creating thread.**
   - Expected: `Host` is `!Send` (it holds `InitGuard`); `Running` borrows it.
   - The only calls from the callback thread are `sample_position` and `output_ready`, as the ASIO SDK host does.
4. **The card changed behind REAPER's back.**
   - Expected: the integrity scan refuses the three setting calls, and `format::admit` refuses 48 kHz or a foreign buffer.
   - `Set-SpikeBufferPref` accepts only 32/48/64 or the recorded original, keeps the registry kind and reads the value back.
   - `undo_plan` always restores before `bring-back`, and `unwind` records the restore only after the read-back succeeded.
   - Tests: `test_check_integrity.py::test_asio_setting_calls_are_refused`, `admit_accepts_only_96k_the_expected_buffer_and_one_known_format`, `Test-SpikePc.ps1` cases `buffer-pref-*`, `test_spike_window.py::UndoPlanTests`.
5. **"ide event" at any moment.**
   - Expected: `guarded()` sees the flag within one poll: a read-only call is abandoned, a changing call finishes. Then `preempt` runs stop-spike → restore-buffer → bring-back.
   - A half-done switch (`card = switching`) still brings REAPER back.
   - Tests: `GuardTests`, `UndoPlanTests`.
6. **Timing statistics that lie.**
   - Expected boundaries: missed at exactly 2 periods, late above 1.5, overrun above 1 period; warm-up callbacks are counted but not judged; the histogram overflow bucket reports the maximum; drift is `None` below 1 s.
   - Tests: the `telemetry` unit tests, kept free of equivalent mutants.
7. **P6.**
   - Expected: no site value in the diff; the design note and plan name no key, path, task or driver string.
   - Tests: the pre-push denylist scan and the CI `secrets` job.

## Shell variables (paste at the start of every task)

```bash
export WORK="$HOME/devel/iemmixer"
export PRED="$HOME/devel/reaperiem"
export PRIV="$HOME/.config/iemmixer"
export OPS="$HOME/devel/iemmixer-ops"
export RAW="$HOME/.local/share/iemmixer/golden-raw"
export WP="$HOME/.claude/work-products/iemmixer-gen2"
export SPIKE_ENV="$PRIV/asio-spike.env"
export REPO=zbynekdrlik/iemmixer
export S="python3 $WORK/scripts/asio-spike/spike_window.py"
```

## File Structure

```
crates/iem-audio-io/Cargo.toml             + azo, windows-sys (Windows only); serde_json (dev); [[example]] asio_spike test = true
crates/iem-audio-io/src/lib.rs             deny(unsafe_code); mod format, telemetry, asio (Windows, allow unsafe)
crates/iem-audio-io/src/format.rs          ASIO sample types ↔ f64, peak, admit (I2 refusals)
crates/iem-audio-io/src/telemetry.rs       histograms, classify, drift, message replies, counters, activity guard
crates/iem-audio-io/src/asio.rs            Windows host: Host (!Send), Running, callbacks, message pump
crates/iem-audio-io/examples/asio_spike.rs probe / duplex / reopen, JSON report, exit codes
Cargo.lock                                 azo, azo-sys, windows-* 0.100, bitflags 2.13.2
scripts/engine-deps-allow.txt              + 12 crate names
.cargo/mutants.toml                        exclude asio.rs and the example
scripts/check_integrity.py (+test)         refuse set_sample_rate / set_clock_source / open_control_panel
scripts/asio-spike/SpikePc.psm1            PC module (buffer preference, sums, blockers, run, task, bring-back)
scripts/asio-spike/spike-task.ps1          Interactive task entry point
scripts/asio-spike/Test-SpikePc.ps1        PC module self-test (CI asio-spike)
scripts/asio-spike/spike_window.py (+test) dev-box window driver and interim switch
.github/workflows/ci.yml                   job asio-spike; integrity runs the spike tests
.claude/rules/asio-spike.md                playbook rule
CLAUDE.md                                  router line; interim switch = spike_window.py
private: $PRIV/asio-spike.env, $OPS/docs/s1a-pc-runbook.md, $PRIV/event-runbook.md (+ ops CLAUDE.md)
```

---

### Task 1: Start — sync, design on the ticket

**Files:** none new (the design note and this plan are already committed on `dev`).

- [x] **Step 1: Sync and check the version.**

```bash
cd "$WORK" && git fetch origin && git status -sb && git log --oneline origin/main..origin/dev
python3 scripts/check_version.py
```

Expected:
- `## dev...origin/dev` and a clean tree;
- only `chore: bump version to 2.0.0-dev.8` plus the docs commit ahead of `main`;
- `check_version.py` passes. `2.0.0-dev.8` > main's `2.0.0-dev.7`; no new bump is needed.

- [x] **Step 2: Design summary on #3 (Slovak, plain).** Write `$WP/s1a-design-comment.md` (work-product dir, not `/tmp`) with:
  - what gets measured (§1 of the note);
  - that REAPER's buffer is restored with read-back;
  - the EVENT-NOW flag;
  - the verdict rules (§4);
  - the tests that wait for the owner's approval (§7);
  - that the predecessor app stays running and its graceful exit stays open.

  Then:

```bash
gh issue comment 3 -R "$REPO" --body-file "$WP/s1a-design-comment.md"
```

- [x] **Step 3: S0 hand-offs.**
  - `gh issue view 1 -R zbynekdrlik/iemmixer-ops --json state` must still be `OPEN`; it is done in Task 12 Step 3.
  - Hand-off (1), the interim runbook, is replaced in Task 10.

---

### Task 2: `format.rs` — sample formats and the I2 refusals

**Files:**
- Create: `crates/iem-audio-io/src/format.rs`
- Modify: `crates/iem-audio-io/src/lib.rs` (module list)

- [x] **Step 1: Write the module with its tests.** Create `crates/iem-audio-io/src/format.rs`:

```rust
//! Sample formats of an ASIO card and their conversion to and from f64 (S1a
//! design note §3). Portable: the ASIO host (Windows) uses it, the tests run
//! everywhere. Integers are two's complement little-endian; a value outside
//! ±1.0 is clipped and a non-finite value becomes silence, in both directions.

use core::fmt;

/// The only sample rate iemmixer runs at (program spec I2).
pub const RATE: f64 = 96_000.0;

/// The little-endian ASIO sample types (ASIOSampleType 16–20 and 24–27; the
/// big-endian ones exist only on old Macs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    I16,
    I24,
    I32,
    F32,
    F64,
    /// A 32-bit container holding a sign-extended 16/18/20/24-bit value.
    I32In16,
    I32In18,
    I32In20,
    I32In24,
}

impl SampleFormat {
    pub fn from_asio(code: i32) -> Option<Self> {
        Some(match code {
            16 => Self::I16,
            17 => Self::I24,
            18 => Self::I32,
            19 => Self::F32,
            20 => Self::F64,
            24 => Self::I32In16,
            25 => Self::I32In18,
            26 => Self::I32In20,
            27 => Self::I32In24,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::I16 => "Int16LSB",
            Self::I24 => "Int24LSB",
            Self::I32 => "Int32LSB",
            Self::F32 => "Float32LSB",
            Self::F64 => "Float64LSB",
            Self::I32In16 => "Int32LSB16",
            Self::I32In18 => "Int32LSB18",
            Self::I32In20 => "Int32LSB20",
            Self::I32In24 => "Int32LSB24",
        }
    }

    /// Bytes per sample.
    pub fn bytes(self) -> usize {
        match self {
            Self::I16 => 2,
            Self::I24 => 3,
            Self::F64 => 8,
            Self::I32
            | Self::F32
            | Self::I32In16
            | Self::I32In18
            | Self::I32In20
            | Self::I32In24 => 4,
        }
    }

    /// 2^(bits − 1) of an integer format; `None` for floats.
    fn full_scale(self) -> Option<f64> {
        match self {
            Self::I16 | Self::I32In16 => Some(32_768.0),
            Self::I32In18 => Some(131_072.0),
            Self::I32In20 => Some(524_288.0),
            Self::I24 | Self::I32In24 => Some(8_388_608.0),
            Self::I32 => Some(2_147_483_648.0),
            Self::F32 | Self::F64 => None,
        }
    }

    /// Writes `src` into `dst` (whole samples only); returns the samples written.
    pub fn encode(self, src: &[f64], dst: &mut [u8]) -> usize {
        let n = src.len().min(dst.len() / self.bytes());
        for (out, &x) in dst.chunks_exact_mut(self.bytes()).zip(src) {
            let x = clean(x);
            match (self, self.full_scale()) {
                (Self::F32, _) => out.copy_from_slice(&(x as f32).to_le_bytes()),
                (Self::F64, _) => out.copy_from_slice(&x.to_le_bytes()),
                (Self::I16, Some(full)) => {
                    out.copy_from_slice(&(quantize(x, full) as i16).to_le_bytes())
                }
                (Self::I24, Some(full)) => {
                    let [b0, b1, b2, _] = (quantize(x, full) as i32).to_le_bytes();
                    out.copy_from_slice(&[b0, b1, b2]);
                }
                (_, Some(full)) => out.copy_from_slice(&(quantize(x, full) as i32).to_le_bytes()),
                (_, None) => out.fill(0),
            }
        }
        n
    }

    /// Reads whole samples of `src` into `dst`; returns the samples read.
    pub fn decode(self, src: &[u8], dst: &mut [f64]) -> usize {
        for (inp, out) in src.chunks_exact(self.bytes()).zip(dst.iter_mut()) {
            *out = self.read(inp);
        }
        dst.len().min(src.len() / self.bytes())
    }

    /// The largest |sample| of `src` (0 for no samples).
    pub fn peak(self, src: &[u8]) -> f64 {
        src.chunks_exact(self.bytes())
            .map(|s| self.read(s).abs())
            .fold(0.0, f64::max)
    }

    fn read(self, s: &[u8]) -> f64 {
        let x = match (self, self.full_scale()) {
            (Self::F32, _) => f64::from(f32::from_le_bytes(array(s))),
            (Self::F64, _) => f64::from_le_bytes(array(s)),
            (Self::I16, Some(full)) => f64::from(i16::from_le_bytes(array(s))) / full,
            (Self::I24, Some(full)) => {
                let [b0, b1, b2]: [u8; 3] = array(s);
                f64::from(i32::from_le_bytes([0, b0, b1, b2]) >> 8) / full
            }
            (_, Some(full)) => f64::from(i32::from_le_bytes(array(s))) / full,
            (_, None) => 0.0,
        };
        clean(x)
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn array<const N: usize>(s: &[u8]) -> [u8; N] {
    s.try_into().unwrap_or([0; N])
}

fn clean(x: f64) -> f64 {
    if x.is_finite() {
        x.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// −1.0 maps to −full, +1.0 to the largest code (full − 1).
fn quantize(x: f64, full: f64) -> i64 {
    let top = full as i64;
    ((x * full).round() as i64).clamp(-top, top - 1)
}

/// Why the host refuses to stream (I2, A1).
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// The card runs at another rate; the host never sets it.
    Rate(f64),
    /// The driver's preferred buffer is not the one the owner set.
    Buffer {
        preferred: i32,
        expected: i32,
    },
    NoChannels,
    /// A sample type this host does not convert.
    Format(i32),
    /// Channels of different sample types.
    MixedFormats,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rate(r) => write!(
                f,
                "the card runs at {r} Hz; only {RATE} Hz is accepted (I2)"
            ),
            Self::Buffer {
                preferred,
                expected,
            } => write!(
                f,
                "the driver's preferred buffer is {preferred} samples, expected {expected}"
            ),
            Self::NoChannels => f.write_str("the driver reports no channels"),
            Self::Format(code) => write!(f, "unsupported ASIO sample type {code}"),
            Self::MixedFormats => f.write_str("channels of different sample types"),
        }
    }
}

impl std::error::Error for Refusal {}

/// Accepts a driver only at 96 kHz, at the expected preferred buffer, with one
/// supported sample type on every channel; returns that format.
pub fn admit(
    rate: f64,
    preferred: i32,
    expected: i32,
    types: &[i32],
) -> Result<SampleFormat, Refusal> {
    if rate != RATE {
        return Err(Refusal::Rate(rate));
    }
    if expected <= 0 || preferred != expected {
        return Err(Refusal::Buffer {
            preferred,
            expected,
        });
    }
    let first = *types.first().ok_or(Refusal::NoChannels)?;
    let format = SampleFormat::from_asio(first).ok_or(Refusal::Format(first))?;
    if types.iter().any(|&t| t != first) {
        return Err(Refusal::MixedFormats);
    }
    Ok(format)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [SampleFormat; 9] = [
        SampleFormat::I16,
        SampleFormat::I24,
        SampleFormat::I32,
        SampleFormat::F32,
        SampleFormat::F64,
        SampleFormat::I32In16,
        SampleFormat::I32In18,
        SampleFormat::I32In20,
        SampleFormat::I32In24,
    ];

    #[test]
    fn asio_codes_map_both_ways_and_unknown_codes_are_none() {
        let codes = [16, 17, 18, 19, 20, 24, 25, 26, 27];
        for (code, f) in codes.into_iter().zip(ALL) {
            assert_eq!(SampleFormat::from_asio(code), Some(f), "{code}");
        }
        for code in [-1, 0, 2, 15, 21, 23, 28, 32] {
            assert_eq!(SampleFormat::from_asio(code), None, "{code}");
        }
    }

    #[test]
    fn names_and_sizes() {
        let names: Vec<_> = ALL.iter().map(|f| f.to_string()).collect();
        assert_eq!(
            names,
            [
                "Int16LSB",
                "Int24LSB",
                "Int32LSB",
                "Float32LSB",
                "Float64LSB",
                "Int32LSB16",
                "Int32LSB18",
                "Int32LSB20",
                "Int32LSB24"
            ]
        );
        let sizes: Vec<_> = ALL.iter().map(|f| f.bytes()).collect();
        assert_eq!(sizes, [2, 3, 4, 4, 8, 4, 4, 4, 4]);
    }

    #[test]
    fn integer_codes_are_exact() {
        let mut b = [0u8; 4];
        SampleFormat::I32.encode(&[0.5], &mut b);
        assert_eq!(i32::from_le_bytes(b), 1 << 30);
        SampleFormat::I32.encode(&[1.0], &mut b);
        assert_eq!(i32::from_le_bytes(b), i32::MAX);
        SampleFormat::I32.encode(&[-1.0], &mut b);
        assert_eq!(i32::from_le_bytes(b), i32::MIN);
        SampleFormat::I32In24.encode(&[-0.5], &mut b);
        assert_eq!(i32::from_le_bytes(b), -(1 << 22));
        SampleFormat::I32In20.encode(&[1.0], &mut b);
        assert_eq!(i32::from_le_bytes(b), (1 << 19) - 1);
        SampleFormat::I32In18.encode(&[-1.0], &mut b);
        assert_eq!(i32::from_le_bytes(b), -(1 << 17));
        SampleFormat::I32In16.encode(&[0.25], &mut b);
        assert_eq!(i32::from_le_bytes(b), 1 << 13);
        let mut h = [0u8; 2];
        SampleFormat::I16.encode(&[-0.25], &mut h);
        assert_eq!(i16::from_le_bytes(h), -(1 << 13));
        let mut t = [0u8; 3];
        SampleFormat::I24.encode(&[-1.0 / 8_388_608.0], &mut t);
        assert_eq!(t, [0xff, 0xff, 0xff]);
        SampleFormat::I24.encode(&[0.5], &mut t);
        assert_eq!(t, [0x00, 0x00, 0x40]);
    }

    #[test]
    fn round_trips_are_within_one_code() {
        let xs = [0.0, 0.5, -0.5, 0.123_456_789, -0.987_654_321, 0.999_9, -1.0];
        for f in ALL {
            let mut bytes = vec![0u8; xs.len() * f.bytes()];
            assert_eq!(f.encode(&xs, &mut bytes), xs.len());
            let mut back = [9.0; 7];
            assert_eq!(f.decode(&bytes, &mut back), xs.len());
            let step = f.full_scale().map_or(1e-7, |full| 1.0 / full);
            for (a, b) in xs.iter().zip(back) {
                assert!((a - b).abs() <= step, "{f}: {a} -> {b}");
            }
        }
    }

    #[test]
    fn out_of_range_is_clipped_and_non_finite_is_silence() {
        for f in ALL {
            let xs = [2.0, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
            let mut bytes = vec![0xAAu8; xs.len() * f.bytes()];
            f.encode(&xs, &mut bytes);
            let mut back = [9.0; 5];
            f.decode(&bytes, &mut back);
            let step = f.full_scale().map_or(0.0, |full| 1.0 / full);
            assert!((back[0] - 1.0).abs() <= step, "{f}: {}", back[0]);
            assert_eq!(back[1], -1.0, "{f}");
            assert_eq!(&back[2..], &[0.0; 3], "{f}");
        }
        let mut back = [9.0; 2];
        let nan = f32::NAN
            .to_le_bytes()
            .into_iter()
            .chain(2.5f32.to_le_bytes())
            .collect::<Vec<_>>();
        SampleFormat::F32.decode(&nan, &mut back);
        assert_eq!(back, [0.0, 1.0]);
    }

    #[test]
    fn only_whole_samples_are_converted() {
        let mut b = [0u8; 7];
        assert_eq!(SampleFormat::I32.encode(&[0.5, 0.5], &mut b), 1);
        assert_eq!(&b[4..], &[0, 0, 0]);
        let mut out = [9.0; 3];
        assert_eq!(
            SampleFormat::I16.decode(&[0, 0x40, 0, 0xC0, 7], &mut out),
            2
        );
        assert_eq!(out, [0.5, -0.5, 9.0]);
        assert_eq!(
            SampleFormat::I16.decode(&[0, 0x40, 0, 0xC0], &mut out[..1]),
            1
        );
    }

    #[test]
    fn peak_is_the_largest_magnitude() {
        let mut b = [0u8; 12];
        SampleFormat::I32.encode(&[0.25, -0.75, 0.5], &mut b);
        assert_eq!(SampleFormat::I32.peak(&b), 0.75);
        assert_eq!(SampleFormat::I32.peak(&[]), 0.0);
        assert_eq!(SampleFormat::I32.peak(&b[..3]), 0.0);
    }

    #[test]
    fn admit_accepts_only_96k_the_expected_buffer_and_one_known_format() {
        assert_eq!(
            admit(96_000.0, 32, 32, &[18, 18, 18]),
            Ok(SampleFormat::I32)
        );
        assert_eq!(admit(48_000.0, 32, 32, &[18]), Err(Refusal::Rate(48_000.0)));
        assert_eq!(
            admit(96_000.000_1, 32, 32, &[18]),
            Err(Refusal::Rate(96_000.000_1))
        );
        assert_eq!(
            admit(96_000.0, 64, 32, &[18]),
            Err(Refusal::Buffer {
                preferred: 64,
                expected: 32
            })
        );
        assert_eq!(
            admit(96_000.0, 0, 0, &[18]),
            Err(Refusal::Buffer {
                preferred: 0,
                expected: 0
            })
        );
        assert_eq!(
            admit(96_000.0, -1, -1, &[18]),
            Err(Refusal::Buffer {
                preferred: -1,
                expected: -1
            })
        );
        assert_eq!(admit(96_000.0, 32, 32, &[]), Err(Refusal::NoChannels));
        assert_eq!(admit(96_000.0, 32, 32, &[2]), Err(Refusal::Format(2)));
        assert_eq!(
            admit(96_000.0, 32, 32, &[18, 19]),
            Err(Refusal::MixedFormats)
        );
        assert_eq!(admit(96_000.0, 1, 1, &[19]), Ok(SampleFormat::F32));
    }

    #[test]
    fn refusals_explain_themselves() {
        assert!(Refusal::Rate(48_000.0).to_string().contains("48000 Hz"));
        assert!(
            Refusal::Buffer {
                preferred: 64,
                expected: 32
            }
            .to_string()
            .contains("64 samples, expected 32")
        );
        assert!(Refusal::Format(2).to_string().contains("type 2"));
        assert!(Refusal::NoChannels.to_string().contains("no channels"));
        assert!(Refusal::MixedFormats.to_string().contains("different"));
    }
}
```

- [x] **Step 2: Declare it.** In `crates/iem-audio-io/src/lib.rs`, replace

```rust
pub mod nullrt;
pub mod offline;
pub mod wav;
```

with

```rust
pub mod format;
pub mod nullrt;
pub mod offline;
pub mod wav;
```

- [x] **Step 3: Format and commit** (the tests run in CI at Task 11).

```bash
cd "$WORK" && cargo fmt --all -- --check
git add crates/iem-audio-io/src/format.rs crates/iem-audio-io/src/lib.rs
git commit -m "feat(audio-io): ASIO sample formats and the I2 refusals

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `telemetry.rs` — lock-free callback statistics

**Files:**
- Create: `crates/iem-audio-io/src/telemetry.rs`
- Modify: `crates/iem-audio-io/src/lib.rs`

- [x] **Step 1: Write the module with its tests.** Create `crates/iem-audio-io/src/telemetry.rs`:

```rust
//! Callback telemetry of a real-time audio stream (S1a design note §4). The
//! driver's callback thread is the only writer and records without locks or
//! allocation; any other thread reads snapshots. Portable: the ASIO host
//! (Windows) feeds it, the tests run everywhere.

use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};

/// Histogram resolution: 1 µs buckets up to 5 ms, then one overflow bucket.
pub const BUCKETS: usize = 5_001;
const BUCKET_NS: u64 = 1_000;
/// The first callbacks of a stream prime the driver's buffers (possibly inside
/// `start()`); they are counted but never judged late, missed or gapped.
pub const WARMUP: u64 = 8;
/// −50 dBFS, the band-activity threshold (program spec §4.2).
pub const ACTIVITY_THRESHOLD: f64 = 0.003_162_277_660_168_379;
const NO_POSITION: i64 = i64::MIN;

/// ASIO driver-to-host message selectors (asio.h `kAsio…`).
pub mod selector {
    pub const SELECTOR_SUPPORTED: i32 = 1;
    pub const ENGINE_VERSION: i32 = 2;
    pub const RESET_REQUEST: i32 = 3;
    pub const BUFFER_SIZE_CHANGE: i32 = 4;
    pub const RESYNC_REQUEST: i32 = 5;
    pub const LATENCIES_CHANGED: i32 = 6;
    pub const SUPPORTS_TIME_INFO: i32 = 7;
    pub const SUPPORTS_TIME_CODE: i32 = 8;
    pub const OVERLOAD: i32 = 15;
}

/// How one callback interval compares with the period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    OnTime,
    /// More than 1.5 periods.
    Late,
    /// At least 2 periods: a whole period passed without a callback.
    Missed,
}

pub fn classify(interval_ns: u64, period_ns: u64) -> Gap {
    if interval_ns >= period_ns.saturating_mul(2) {
        Gap::Missed
    } else if interval_ns.saturating_mul(2) > period_ns.saturating_mul(3) {
        Gap::Late
    } else {
        Gap::OnTime
    }
}

/// One buffer period in nanoseconds.
pub fn period_ns(frames: u32, rate: f64) -> u64 {
    (f64::from(frames) * 1e9 / rate).round() as u64
}

/// The card clock against the host clock over (host ns, sample position)
/// pairs, in ppm: positive when the card runs fast. `None` below 1 s of data.
pub fn drift_ppm(first: (u64, i64), last: (u64, i64), rate: f64) -> Option<f64> {
    let elapsed = last.0.checked_sub(first.0)? as f64 / 1e9;
    if elapsed < 1.0 {
        return None;
    }
    let card = last.1.wrapping_sub(first.1) as f64 / rate;
    Some((card - elapsed) / elapsed * 1e6)
}

/// Level of a linear peak in dBFS (−150 for silence).
pub fn dbfs(peak: f64) -> f64 {
    (20.0 * peak.max(1e-300).log10()).max(-150.0)
}

/// The host's answer to an `asioMessage`. It supports resets (by reopening
/// the driver), resyncs and time info; it never resizes buffers live, so a
/// size change is answered 0 (the driver then asks for a reset).
pub fn reply(sel: i32, value: i32) -> i32 {
    use selector::*;
    match sel {
        SELECTOR_SUPPORTED => i32::from(matches!(
            value,
            ENGINE_VERSION
                | RESET_REQUEST
                | BUFFER_SIZE_CHANGE
                | RESYNC_REQUEST
                | LATENCIES_CHANGED
                | SUPPORTS_TIME_INFO
                | OVERLOAD
        )),
        ENGINE_VERSION => 2,
        RESET_REQUEST | RESYNC_REQUEST | LATENCIES_CHANGED | SUPPORTS_TIME_INFO => 1,
        _ => 0,
    }
}

pub struct Histogram {
    counts: Box<[AtomicU64]>,
    max_ns: AtomicU64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: (0..BUCKETS).map(|_| AtomicU64::new(0)).collect(),
            max_ns: AtomicU64::new(0),
        }
    }
}

impl Histogram {
    pub fn record(&self, ns: u64) {
        let i = usize::try_from(ns / BUCKET_NS)
            .unwrap_or(usize::MAX)
            .min(BUCKETS - 1);
        if let Some(c) = self.counts.get(i) {
            c.fetch_add(1, Relaxed);
        }
        self.max_ns.fetch_max(ns, Relaxed);
    }

    pub fn snapshot(&self) -> HistogramSnapshot {
        HistogramSnapshot {
            counts: self.counts.iter().map(|c| c.load(Relaxed)).collect(),
            max_ns: self.max_ns.load(Relaxed),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistogramSnapshot {
    pub counts: Vec<u64>,
    pub max_ns: u64,
}

impl HistogramSnapshot {
    pub fn total(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// The upper edge of the bucket holding quantile `q` (0 < q ≤ 1), never
    /// above the maximum (the overflow bucket reports the maximum); 0 when
    /// empty.
    pub fn quantile_ns(&self, q: f64) -> u64 {
        let total = self.total();
        if total == 0 {
            return 0;
        }
        let rank = ((q * total as f64).ceil() as u64).clamp(1, total);
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                if i + 1 >= BUCKETS {
                    return self.max_ns;
                }
                return (i as u64 + 1).saturating_mul(BUCKET_NS).min(self.max_ns);
            }
        }
        self.max_ns
    }

    /// p50, p99, p99.9 and the maximum, in µs.
    pub fn summary_us(&self) -> [f64; 4] {
        let us = |ns: u64| ns as f64 / 1e3;
        [
            us(self.quantile_ns(0.5)),
            us(self.quantile_ns(0.99)),
            us(self.quantile_ns(0.999)),
            us(self.max_ns),
        ]
    }
}

/// Everything one stream recorded, read at one moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub period_ns: u64,
    pub callbacks: u64,
    pub late: u64,
    pub missed: u64,
    /// Callbacks that took longer than one period.
    pub overruns: u64,
    /// Sample positions that did not advance by exactly one buffer.
    pub position_gaps: u64,
    /// Host ns from the stream's base to its first callback (0 = none yet).
    pub first_callback_ns: u64,
    pub resets: u64,
    pub resyncs: u64,
    pub latency_changes: u64,
    pub buffer_size_changes: u64,
    pub overloads: u64,
    pub rate_changes: u64,
    pub interval: HistogramSnapshot,
    pub duration: HistogramSnapshot,
    pub drift_ppm: Option<f64>,
}

pub struct Telemetry {
    period_ns: u64,
    frames: i64,
    rate: f64,
    interval: Histogram,
    duration: Histogram,
    callbacks: AtomicU64,
    late: AtomicU64,
    missed: AtomicU64,
    overruns: AtomicU64,
    position_gaps: AtomicU64,
    first_ns: AtomicU64,
    last_ns: AtomicU64,
    first_pos: AtomicI64,
    first_pos_ns: AtomicU64,
    last_pos: AtomicI64,
    last_pos_ns: AtomicU64,
    resets: AtomicU64,
    resyncs: AtomicU64,
    latency_changes: AtomicU64,
    buffer_size_changes: AtomicU64,
    overloads: AtomicU64,
    rate_changes: AtomicU64,
    reopen: AtomicBool,
    input_peak: AtomicU64,
}

impl Telemetry {
    pub fn new(frames: u32, rate: f64) -> Self {
        Self {
            period_ns: period_ns(frames, rate),
            frames: i64::from(frames),
            rate,
            interval: Histogram::default(),
            duration: Histogram::default(),
            callbacks: AtomicU64::new(0),
            late: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            position_gaps: AtomicU64::new(0),
            first_ns: AtomicU64::new(0),
            last_ns: AtomicU64::new(0),
            first_pos: AtomicI64::new(NO_POSITION),
            first_pos_ns: AtomicU64::new(0),
            last_pos: AtomicI64::new(NO_POSITION),
            last_pos_ns: AtomicU64::new(0),
            resets: AtomicU64::new(0),
            resyncs: AtomicU64::new(0),
            latency_changes: AtomicU64::new(0),
            buffer_size_changes: AtomicU64::new(0),
            overloads: AtomicU64::new(0),
            rate_changes: AtomicU64::new(0),
            reopen: AtomicBool::new(false),
            input_peak: AtomicU64::new(0),
        }
    }

    pub fn period_ns(&self) -> u64 {
        self.period_ns
    }

    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Relaxed)
    }

    /// At the entry of callback: `entry_ns` on the host clock (> 0), the
    /// driver's sample position when it reported one.
    pub fn on_callback(&self, entry_ns: u64, position: Option<i64>) {
        let n = self.callbacks.fetch_add(1, Relaxed);
        let prev = self.last_ns.swap(entry_ns, Relaxed);
        if n == 0 {
            self.first_ns.store(entry_ns, Relaxed);
        } else if n >= WARMUP {
            let dt = entry_ns.saturating_sub(prev);
            self.interval.record(dt);
            match classify(dt, self.period_ns) {
                Gap::Missed => self.missed.fetch_add(1, Relaxed),
                Gap::Late => self.late.fetch_add(1, Relaxed),
                Gap::OnTime => 0,
            };
        }
        if let Some(pos) = position {
            let before = self.last_pos.swap(pos, Relaxed);
            self.last_pos_ns.store(entry_ns, Relaxed);
            if before == NO_POSITION {
                self.first_pos.store(pos, Relaxed);
                self.first_pos_ns.store(entry_ns, Relaxed);
            } else if n >= WARMUP && pos.wrapping_sub(before) != self.frames {
                self.position_gaps.fetch_add(1, Relaxed);
            }
        }
    }

    /// At the exit of a callback: how long it took.
    pub fn on_done(&self, duration_ns: u64) {
        self.duration.record(duration_ns);
        if duration_ns > self.period_ns {
            self.overruns.fetch_add(1, Relaxed);
        }
    }

    /// The largest input |sample| of one callback (linear, 0..=1).
    pub fn on_input_peak(&self, peak: f64) {
        if peak.is_finite() {
            // Non-negative finite f64 bit patterns order like their values.
            self.input_peak.fetch_max(peak.abs().to_bits(), Relaxed);
        }
    }

    /// The largest input peak since the last call.
    pub fn take_input_peak(&self) -> f64 {
        f64::from_bits(self.input_peak.swap(0, Relaxed))
    }

    /// Counts one `asioMessage` and answers it ([`reply`]).
    pub fn driver_message(&self, sel: i32, value: i32) -> i32 {
        let counter = match sel {
            selector::RESET_REQUEST => Some(&self.resets),
            selector::BUFFER_SIZE_CHANGE => Some(&self.buffer_size_changes),
            selector::RESYNC_REQUEST => Some(&self.resyncs),
            selector::LATENCIES_CHANGED => Some(&self.latency_changes),
            selector::OVERLOAD => Some(&self.overloads),
            _ => None,
        };
        if let Some(c) = counter {
            c.fetch_add(1, Relaxed);
        }
        if matches!(sel, selector::RESET_REQUEST | selector::BUFFER_SIZE_CHANGE) {
            self.reopen.store(true, Relaxed);
        }
        reply(sel, value)
    }

    /// The driver reported a sample-rate change (the host never sets one).
    pub fn on_rate_change(&self) {
        self.rate_changes.fetch_add(1, Relaxed);
    }

    pub fn rate_changes(&self) -> u64 {
        self.rate_changes.load(Relaxed)
    }

    /// True once after the driver asked for a reset (or a buffer resize).
    pub fn take_reopen(&self) -> bool {
        self.reopen.swap(false, Relaxed)
    }

    pub fn snapshot(&self) -> Snapshot {
        let first_pos = self.first_pos.load(Relaxed);
        let drift = if first_pos == NO_POSITION {
            None
        } else {
            drift_ppm(
                (self.first_pos_ns.load(Relaxed), first_pos),
                (self.last_pos_ns.load(Relaxed), self.last_pos.load(Relaxed)),
                self.rate,
            )
        };
        Snapshot {
            period_ns: self.period_ns,
            callbacks: self.callbacks.load(Relaxed),
            late: self.late.load(Relaxed),
            missed: self.missed.load(Relaxed),
            overruns: self.overruns.load(Relaxed),
            position_gaps: self.position_gaps.load(Relaxed),
            first_callback_ns: self.first_ns.load(Relaxed),
            resets: self.resets.load(Relaxed),
            resyncs: self.resyncs.load(Relaxed),
            latency_changes: self.latency_changes.load(Relaxed),
            buffer_size_changes: self.buffer_size_changes.load(Relaxed),
            overloads: self.overloads.load(Relaxed),
            rate_changes: self.rate_changes.load(Relaxed),
            interval: self.interval.snapshot(),
            duration: self.duration.snapshot(),
            drift_ppm: drift,
        }
    }
}

/// Band activity on the inputs: `needed` consecutive one-second peaks above
/// −50 dBFS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityGuard {
    run: u32,
    needed: u32,
}

impl ActivityGuard {
    pub fn new(needed: u32) -> Self {
        Self { run: 0, needed }
    }

    /// Feeds one second's peak; true while the band is playing.
    pub fn observe(&mut self, peak: f64) -> bool {
        self.run = if peak > ACTIVITY_THRESHOLD {
            self.run.saturating_add(1)
        } else {
            0
        };
        self.run >= self.needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: u64 = 333_333; // 32 samples at 96 kHz

    #[test]
    fn periods_and_levels() {
        assert_eq!(period_ns(32, 96_000.0), P);
        assert_eq!(period_ns(64, 96_000.0), 666_667);
        assert_eq!(period_ns(48, 96_000.0), 500_000);
        assert_eq!(dbfs(1.0), 0.0);
        assert!((dbfs(ACTIVITY_THRESHOLD) + 50.0).abs() < 1e-9);
        assert_eq!(dbfs(0.0), -150.0);
        assert_eq!(dbfs(-1.0), -150.0);
        assert_eq!(dbfs(1e-12), -150.0);
    }

    #[test]
    fn classify_boundaries() {
        assert_eq!(classify(P, P), Gap::OnTime);
        assert_eq!(classify(300, 200), Gap::OnTime);
        assert_eq!(classify(301, 200), Gap::Late);
        assert_eq!(classify(399, 200), Gap::Late);
        assert_eq!(classify(400, 200), Gap::Missed);
        assert_eq!(classify(0, 200), Gap::OnTime);
        assert_eq!(classify(2 * P - 1, P), Gap::Late);
        assert_eq!(classify(2 * P, P), Gap::Missed);
    }

    #[test]
    fn drift_needs_a_second_and_has_the_card_sign() {
        assert_eq!(drift_ppm((0, 0), (999_999_999, 96_000), 96_000.0), None);
        assert_eq!(drift_ppm((5, 0), (4, 0), 96_000.0), None);
        assert_eq!(
            drift_ppm((0, 0), (1_000_000_000, 96_000), 96_000.0),
            Some(0.0)
        );
        let fast = drift_ppm((0, 0), (10_000_000_000, 960_096), 96_000.0).unwrap();
        assert!((fast - 100.0).abs() < 1e-6, "{fast}");
        let slow = drift_ppm((1_000_000_000, 10), (3_000_000_000, 10 + 191_904), 96_000.0).unwrap();
        assert!((slow + 500.0).abs() < 1e-6, "{slow}");
    }

    #[test]
    fn histogram_quantiles() {
        let h = Histogram::default();
        assert_eq!(h.snapshot().quantile_ns(0.5), 0);
        for _ in 0..98 {
            h.record(333_400);
        }
        h.record(1_200_000);
        h.record(9_000_000);
        let s = h.snapshot();
        assert_eq!(s.total(), 100);
        assert_eq!(s.max_ns, 9_000_000);
        assert_eq!(s.quantile_ns(0.5), 334_000);
        assert_eq!(s.quantile_ns(0.98), 334_000);
        assert_eq!(s.quantile_ns(0.99), 1_201_000);
        assert_eq!(s.quantile_ns(1.0), 9_000_000);
        assert_eq!(s.quantile_ns(0.0), 334_000);
        assert_eq!(s.summary_us(), [334.0, 1201.0, 9000.0, 9000.0]);
        let one = Histogram::default();
        one.record(700);
        assert_eq!(one.snapshot().quantile_ns(0.5), 700);
        assert_eq!(one.snapshot().counts.len(), BUCKETS);
        one.record(u64::MAX);
        assert_eq!(one.snapshot().counts[BUCKETS - 1], 1);
    }

    #[test]
    fn warmup_callbacks_are_counted_but_not_judged() {
        let t = Telemetry::new(32, 96_000.0);
        let mut at = 1_000;
        for i in 0..WARMUP {
            t.on_callback(at, Some(i as i64 * 7));
            at += 10 * P;
        }
        let s = t.snapshot();
        assert_eq!(
            (
                s.callbacks,
                s.late,
                s.missed,
                s.position_gaps,
                s.interval.total()
            ),
            (WARMUP, 0, 0, 0, 0)
        );
        assert_eq!(s.first_callback_ns, 1_000);
    }

    #[test]
    fn late_missed_and_position_gaps_are_counted() {
        let t = Telemetry::new(32, 96_000.0);
        let mut at = 1_000;
        let mut pos = 0;
        for _ in 0..WARMUP {
            t.on_callback(at, Some(pos));
            at += P;
            pos += 32;
        }
        t.on_callback(at, Some(pos)); // on time
        at += P * 3 / 2 + 1;
        pos += 32;
        t.on_callback(at, Some(pos)); // late
        at += 2 * P;
        pos += 64;
        t.on_callback(at, Some(pos)); // missed, one gap
        at += P;
        t.on_callback(at, None); // no position: no gap judged
        let s = t.snapshot();
        assert_eq!(
            (s.callbacks, s.late, s.missed, s.position_gaps),
            (WARMUP + 4, 1, 1, 1)
        );
        assert_eq!(s.interval.total(), 4);
        assert_eq!(t.callbacks(), WARMUP + 4);
    }

    #[test]
    fn overruns_are_longer_than_a_period() {
        let t = Telemetry::new(32, 96_000.0);
        t.on_done(P);
        t.on_done(P + 1);
        t.on_done(10);
        let s = t.snapshot();
        assert_eq!(s.overruns, 1);
        assert_eq!(s.duration.total(), 3);
        assert_eq!(s.period_ns, P);
        assert_eq!(t.period_ns(), P);
    }

    #[test]
    fn drift_comes_from_the_first_and_last_positions() {
        let t = Telemetry::new(32, 96_000.0);
        assert_eq!(t.snapshot().drift_ppm, None);
        t.on_callback(1_000, Some(0));
        t.on_callback(2_000_001_000, Some(192_000));
        assert_eq!(t.snapshot().drift_ppm, Some(0.0));
    }

    #[test]
    fn input_peaks_keep_the_maximum_until_taken() {
        let t = Telemetry::new(32, 96_000.0);
        assert_eq!(t.take_input_peak(), 0.0);
        t.on_input_peak(0.25);
        t.on_input_peak(0.5);
        t.on_input_peak(0.125);
        t.on_input_peak(f64::NAN);
        assert_eq!(t.take_input_peak(), 0.5);
        assert_eq!(t.take_input_peak(), 0.0);
        t.on_input_peak(-0.75);
        t.on_input_peak(f64::INFINITY);
        assert_eq!(t.take_input_peak(), 0.75);
    }

    #[test]
    fn driver_messages_are_answered() {
        use selector::*;
        for s in [
            ENGINE_VERSION,
            RESET_REQUEST,
            BUFFER_SIZE_CHANGE,
            RESYNC_REQUEST,
            LATENCIES_CHANGED,
            SUPPORTS_TIME_INFO,
            OVERLOAD,
        ] {
            assert_eq!(reply(SELECTOR_SUPPORTED, s), 1, "{s}");
        }
        for s in [SELECTOR_SUPPORTED, SUPPORTS_TIME_CODE, 9, 0] {
            assert_eq!(reply(SELECTOR_SUPPORTED, s), 0, "{s}");
        }
        let answers: Vec<i32> = [
            ENGINE_VERSION,
            RESET_REQUEST,
            BUFFER_SIZE_CHANGE,
            RESYNC_REQUEST,
            LATENCIES_CHANGED,
            SUPPORTS_TIME_INFO,
            SUPPORTS_TIME_CODE,
            OVERLOAD,
            99,
        ]
        .into_iter()
        .map(|s| reply(s, 0))
        .collect();
        assert_eq!(answers, [2, 1, 0, 1, 1, 1, 0, 0, 0]);
    }

    #[test]
    fn driver_messages_are_counted_and_resets_request_a_reopen() {
        use selector::*;
        let t = Telemetry::new(32, 96_000.0);
        assert_eq!(t.driver_message(ENGINE_VERSION, 0), 2);
        assert_eq!(t.driver_message(SELECTOR_SUPPORTED, OVERLOAD), 1);
        assert!(!t.take_reopen());
        assert_eq!(t.driver_message(RESET_REQUEST, 0), 1);
        assert!(t.take_reopen());
        assert!(!t.take_reopen());
        assert_eq!(t.driver_message(BUFFER_SIZE_CHANGE, 64), 0);
        assert!(t.take_reopen());
        assert_eq!(t.driver_message(RESYNC_REQUEST, 0), 1);
        assert_eq!(t.driver_message(LATENCIES_CHANGED, 0), 1);
        assert!(!t.take_reopen());
        assert_eq!(t.driver_message(OVERLOAD, 0), 0);
        assert_eq!(t.driver_message(OVERLOAD, 0), 0);
        assert_eq!(t.rate_changes(), 0);
        t.on_rate_change();
        assert_eq!(t.rate_changes(), 1);
        let s = t.snapshot();
        assert_eq!(
            (
                s.resets,
                s.buffer_size_changes,
                s.resyncs,
                s.latency_changes,
                s.overloads,
                s.rate_changes
            ),
            (1, 1, 1, 1, 2, 1)
        );
    }

    #[test]
    fn activity_needs_consecutive_seconds_above_minus_50_dbfs() {
        let mut g = ActivityGuard::new(3);
        assert!(!g.observe(0.01));
        assert!(!g.observe(0.01));
        assert!(!g.observe(ACTIVITY_THRESHOLD));
        assert!(!g.observe(0.01));
        assert!(!g.observe(0.01));
        assert!(g.observe(0.01));
        assert!(g.observe(1.0));
        assert!(!g.observe(0.0));
        let mut now = ActivityGuard::new(1);
        assert!(now.observe(ACTIVITY_THRESHOLD * 1.001));
    }
}
```

Notes for the reviewer:
- `dbfs` and `on_input_peak` avoid a `>`/`>=` on 0, which would be an equivalent mutant.
- The callback thread is the only writer, so `Relaxed` is enough. The snapshot is a statistic, not a consistent cut.

- [x] **Step 2: Declare it.** In `lib.rs` add `pub mod telemetry;` after `pub mod offline;`.

- [x] **Step 3: Format and commit.**

```bash
cd "$WORK" && cargo fmt --all -- --check
git add crates/iem-audio-io/src/telemetry.rs crates/iem-audio-io/src/lib.rs
git commit -m "feat(audio-io): lock-free callback telemetry and the activity guard

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Dependencies, allowlists, mutation scope, the I2 integrity guard

**Files:**
- Modify: `crates/iem-audio-io/Cargo.toml`, `Cargo.lock`, `scripts/engine-deps-allow.txt`, `.cargo/mutants.toml`
- Modify: `scripts/check_integrity.py`, `scripts/test_check_integrity.py`

- [x] **Step 1: Manifest.** Replace `crates/iem-audio-io/Cargo.toml` with:

```toml
[package]
name = "iem-audio-io"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
description = "iemmixer audio backends: the Process trait, deterministic Offline, paced NullRt and the Windows ASIO host (S1a spike; S6 backend)"

[dependencies]

[target.'cfg(windows)'.dependencies]
# ASIO host (S1a design note §3). Pinned exactly; fallbacks in program spec §2.2.
azo = "=0.2.1"
# The driver thread's message pump (PeekMessageW / DispatchMessageW).
windows-sys = { version = "0.61", features = ["Win32_Foundation", "Win32_UI_WindowsAndMessaging"] }

[dev-dependencies]
# The asio_spike example's JSON report.
serde_json = "1"

[[example]]
name = "asio_spike"
# Runs the argument-parser tests on every OS.
test = true
```

- [x] **Step 2: Lockfile (resolution only, no compilation).**

```bash
cd "$WORK" && cargo metadata --format-version 1 > /dev/null && git diff Cargo.lock | grep -E '^[+-](name|version) ' | sort | uniq -c
```

Expected additions:
- `azo` 0.2.1 and `azo-sys` 0.2.1;
- `windows-bindgen`, `windows-core`, `windows-default`, `windows-implement`, `windows-interface`, `windows-link`, `windows-metadata`, `windows-registry`, `windows-result` and `windows-strings`, all 0.100.0;
- `bitflags` 2.11.0 → 2.13.2 (azo-sys needs it).

No other package may change. If one does, stop and investigate.

- [x] **Step 3: Engine dependency allowlist.** `iem-audio-io` is in the engine's closure, so the Windows target adds twelve names. Insert them into `scripts/engine-deps-allow.txt` in alphabetical order:

```
azo
azo-sys
bitflags
windows-bindgen
windows-core
windows-default
windows-implement
windows-interface
windows-metadata
windows-registry
windows-result
windows-strings
```

Then:

```bash
python3 scripts/check_engine_deps.py
```

Expected: `engine dependencies: <n> crates, all allowlisted`.

Licences are all MIT or MIT OR Apache-2.0, which `deny.toml` already allows. `windows-bindgen` and `windows-metadata` are build-time only (azo's `build.rs`).

- [x] **Step 4: Mutation scope.** In `.cargo/mutants.toml`, append to `exclude_globs` (after the `iem-tray` entry):

```toml
  # Windows-only ASIO host (unsafe FFI, S1a): not compiled on the Linux
  # runners; the PC window exercises it. Its decisions live in the mutated
  # format.rs and telemetry.rs.
  "crates/iem-audio-io/src/asio.rs",
  # The S1a spike program (a Windows-only main; its parser is tested).
  "crates/iem-audio-io/examples/**",
```

- [x] **Step 5: The I2 integrity guard (test first).** Append to `IntegrityTests` in `scripts/test_check_integrity.py`:

```python
    def test_asio_setting_calls_are_refused(self) -> None:
        for call in ("driver.set_sample_rate(48000.0)", "d.set_clock_source(1)", "self.driver.open_control_panel()"):
            self.put("crates/a/src/lib.rs", f"fn f() {{ {call}; }}\n")
            self.assertEqual(len(ci.violations(self.root)), 1, call)
        self.put("crates/a/src/lib.rs", "// the host never calls set_sample_rate\nfn f() {}\n")
        self.assertEqual(ci.violations(self.root), [])
```

Run `python3 -m unittest scripts/test_check_integrity.py`. Expected: the new test fails, with 0 violations where 1 is expected.

Then in `scripts/check_integrity.py`:
- add after `FORCE_KILL`:

```python
ASIO_SETTINGS = re.compile(r"\.(?:set_sample_rate|set_clock_source|open_control_panel)\s*\(")
```

- inside the first loop over `crates/**/*.rs` (after the `#[ignore]` line), add:

```python
        found += [f"{rel}:{n}: the host never changes the card's rate, clock or panel (program spec I2)" for n, line in lines(path) if ASIO_SETTINGS.search(line)]
```

- update the module docstring's list with `no ASIO rate, clock or control-panel call (I2)`.

Run:

```bash
python3 -m unittest scripts/test_check_integrity.py && python3 scripts/check_integrity.py
```

Expected: all tests pass and `integrity: clean`.

- [x] **Step 6: Commit.**

```bash
git add crates/iem-audio-io/Cargo.toml Cargo.lock scripts/engine-deps-allow.txt .cargo/mutants.toml scripts/check_integrity.py scripts/test_check_integrity.py
git commit -m "build(audio-io): azo 0.2.1 for Windows, allowlists, mutation scope, I2 integrity guard

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `asio.rs` — the Windows host

**Files:**
- Create: `crates/iem-audio-io/src/asio.rs`
- Modify: `crates/iem-audio-io/src/lib.rs` (lint, module, crate doc)

- [x] **Step 1: Write the host.** Create `crates/iem-audio-io/src/asio.rs`:

```rust
//! ASIO host on azo 0.2.1 (S1a spike; the S6 backend grows from it; design
//! note `docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md` §3).
//!
//! - Every driver call happens on the thread that created the driver (COM
//!   STA); that thread also pumps its window messages. [`Host`] is `!Send`.
//! - ASIO callbacks carry no user pointer: one global slot holds the running
//!   stream, and a counter of callbacks in flight lets the owner free the
//!   stream only after the last callback left it.
//! - Every output channel of the card gets a buffer, zeroed before `start()`
//!   and on every callback, also after a caught panic (A1).
//! - The host never sets the sample rate, never selects a clock source and
//!   never opens the control panel; it refuses any rate but 96 kHz and any
//!   buffer but the driver's preferred one (I2).

use core::ffi::{CStr, c_long, c_void};
use core::fmt;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use azo::dto::{ChannelId, Granularity};
use azo::sys::{Bool, Callbacks, MessageSelector, SampleRate, Time, TimeInfoFlags};
use azo::utils::com::InitGuard;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

use crate::format::{self, Refusal, SampleFormat};
use crate::telemetry::{self, Snapshot, Telemetry};

#[derive(Debug)]
pub enum AsioError {
    /// No ASIO driver is registered (no `HKLM\SOFTWARE\ASIO`).
    NoDrivers(String),
    /// No driver has this description; the registered ones are listed.
    NotFound {
        wanted: String,
        present: Vec<String>,
    },
    /// COM could not create the driver object.
    Create(String),
    /// `init()` returned false; the driver's error text.
    Init(String),
    /// An ASIO call failed: which call, and the driver's error text.
    Call(&'static str, String),
    Refused(Refusal),
    /// A stream already runs in this process.
    Busy,
}

impl fmt::Display for AsioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDrivers(e) => write!(f, "no ASIO drivers registered: {e}"),
            Self::NotFound { wanted, present } => {
                write!(f, "no ASIO driver named {wanted:?}; present: {present:?}")
            }
            Self::Create(e) => write!(f, "creating the driver failed: {e}"),
            Self::Init(e) => write!(f, "the driver refused init(): {e}"),
            Self::Call(what, e) => write!(f, "{what} failed: {e}"),
            Self::Refused(r) => write!(f, "refused: {r}"),
            Self::Busy => f.write_str("a stream already runs in this process"),
        }
    }
}

impl std::error::Error for AsioError {}

#[derive(Debug, Clone, PartialEq)]
pub struct ClockInfo {
    pub index: i32,
    pub name: String,
    pub current: bool,
}

/// What the driver reports without any buffer (read-only calls only).
#[derive(Debug, Clone, PartialEq)]
pub struct DriverInfo {
    pub name: String,
    pub version: i32,
    pub inputs: i32,
    pub outputs: i32,
    pub buffer_min: i32,
    pub buffer_max: i32,
    pub buffer_preferred: i32,
    /// `fixed`, `power-of-two` or `linear:<step>`.
    pub buffer_granularity: String,
    pub rate: f64,
    pub can_96k: bool,
    /// At the preferred size, before buffers exist (samples).
    pub latency_in: i32,
    pub latency_out: i32,
    pub clocks: Vec<ClockInfo>,
    /// ASIOSampleType per channel: inputs, then outputs.
    pub sample_types: Vec<i32>,
}

/// Options of one stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamConfig {
    /// The buffer the owner set in the driver; must equal its preferred size.
    pub frames: i32,
    /// Busy-work per callback standing in for the engine's DSP (µs).
    pub burn_us: u32,
    /// Fault injection: panic inside callback number `panic_at` (0 = never).
    pub panic_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StartTimings {
    pub create_buffers: Duration,
    pub start: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StopTimings {
    pub stop: Duration,
    pub dispose: Duration,
    pub stop_ok: bool,
    pub dispose_ok: bool,
}

/// A driver instance, created, used and released on this thread.
pub struct Host {
    driver: InitGuard<azo::Driver>,
}

impl Host {
    /// Creates and initialises the driver whose registry description is `description`.
    pub fn open(description: &str) -> Result<Self, AsioError> {
        let drivers = azo::get_drivers().map_err(|e| AsioError::NoDrivers(e.to_string()))?;
        let Some(meta) = drivers
            .iter()
            .find(|d| d.description.to_string_lossy() == description)
        else {
            return Err(AsioError::NotFound {
                wanted: description.to_owned(),
                present: drivers
                    .iter()
                    .map(|d| d.description.to_string_lossy())
                    .collect(),
            });
        };
        let driver = meta
            .create_instance()
            .map_err(|e| AsioError::Create(e.to_string()))?;
        if !driver.init(None) {
            return Err(AsioError::Init(text(&driver.last_error())));
        }
        Ok(Self { driver })
    }

    fn call(&self, what: &'static str) -> impl Fn(azo::Error) -> AsioError + '_ {
        move |e| AsioError::Call(what, format!("{e} ({})", text(&self.driver.last_error())))
    }

    pub fn info(&self) -> Result<DriverInfo, AsioError> {
        let d = &*self.driver;
        let counts = d.channel_counts().map_err(self.call("getChannels"))?;
        let size = d.buffer_size().map_err(self.call("getBufferSize"))?;
        let rate = d.get_sample_rate().map_err(self.call("getSampleRate"))?;
        let latency = d.latencies().map_err(self.call("getLatencies"))?;
        let clocks = d.clock_sources().map_err(self.call("getClockSources"))?;
        let mut sample_types = Vec::new();
        for (input, n) in [(true, counts.in_), (false, counts.out)] {
            for index in 0..n {
                let info = d
                    .channel_info(ChannelId { input, index })
                    .map_err(self.call("getChannelInfo"))?;
                sample_types.push(info.sample_type.0);
            }
        }
        Ok(DriverInfo {
            name: text(&d.name()),
            version: d.version(),
            inputs: counts.in_,
            outputs: counts.out,
            buffer_min: size.min,
            buffer_max: size.max,
            buffer_preferred: size.preferred,
            buffer_granularity: match size.granularity {
                None => "fixed".to_owned(),
                Some(Granularity::Exponential) => "power-of-two".to_owned(),
                Some(Granularity::Linear { step }) => format!("linear:{step}"),
            },
            rate,
            can_96k: d.can_sample_rate(format::RATE).is_ok(),
            latency_in: latency.in_,
            latency_out: latency.out,
            clocks: clocks
                .iter()
                .map(|c| ClockInfo {
                    index: c.index,
                    name: CStr::from_bytes_until_nul(&c.name)
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    current: c.is_current_source != Bool::FALSE,
                })
                .collect(),
            sample_types,
        })
    }

    /// Creates a buffer on every input and output of the card, zeroes the
    /// outputs and starts streaming.
    pub fn start(&self, info: &DriverInfo, cfg: StreamConfig) -> Result<Running<'_>, AsioError> {
        let d = &*self.driver;
        let format = format::admit(
            info.rate,
            info.buffer_preferred,
            cfg.frames,
            &info.sample_types,
        )
        .map_err(AsioError::Refused)?;
        let frames = u32::try_from(cfg.frames).map_err(|_| {
            AsioError::Refused(Refusal::Buffer {
                preferred: info.buffer_preferred,
                expected: cfg.frames,
            })
        })?;
        if !STREAM.load(Ordering::SeqCst).is_null() {
            return Err(AsioError::Busy);
        }
        let channels: Vec<ChannelId> = (0..info.inputs)
            .map(|index| ChannelId { input: true, index })
            .chain((0..info.outputs).map(|index| ChannelId {
                input: false,
                index,
            }))
            .collect();
        let t = Instant::now();
        // SAFETY: CALLBACKS is a static, so it outlives the buffers. The
        // pointers are dereferenced only by callbacks of this stream, which
        // end before `Running::finish` disposes the buffers.
        let buffers: Vec<[*mut c_void; 2]> =
            unsafe { d.create_buffers(channels.iter().copied(), cfg.frames, &raw const CALLBACKS) }
                .map_err(self.call("createBuffers"))?
                .collect();
        let create_buffers = t.elapsed();
        let split = usize::try_from(info.inputs).unwrap_or(0).min(buffers.len());
        let (inputs, outputs) = buffers.split_at(split);
        let stream = Box::new(Stream {
            format,
            bytes: (frames as usize).saturating_mul(format.bytes()),
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            telemetry: Telemetry::new(frames, info.rate),
            base: Instant::now(),
            burn: Duration::from_micros(u64::from(cfg.burn_us)),
            panic_at: cfg.panic_at,
            faulted: AtomicBool::new(false),
            output_ready: AtomicBool::new(true),
            driver: ptr::from_ref(d),
        });
        for ch in &stream.outputs {
            let [a, b] = *ch;
            zero(a, stream.bytes);
            zero(b, stream.bytes);
        }
        let raw = Box::into_raw(stream);
        STREAM.store(raw, Ordering::SeqCst);
        let t = Instant::now();
        let started = d.start();
        let start = t.elapsed();
        let running = Running {
            host: self,
            stream: raw,
            timings: StartTimings {
                create_buffers,
                start,
            },
        };
        match started {
            Ok(()) => Ok(running),
            Err(e) => {
                let err = self.call("start")(e);
                running.finish();
                Err(err)
            }
        }
    }

    /// Reads the latencies the driver reports now (samples).
    pub fn latencies(&self) -> Result<(i32, i32), AsioError> {
        let l = self.driver.latencies().map_err(self.call("getLatencies"))?;
        Ok((l.in_, l.out))
    }
}

/// A started stream; [`Running::finish`] (or drop) stops it.
pub struct Running<'h> {
    host: &'h Host,
    stream: *mut Stream,
    pub timings: StartTimings,
}

impl Running<'_> {
    fn stream(&self) -> Option<&Stream> {
        // SAFETY: `stream` is either null or the live Box this Running owns.
        unsafe { self.stream.as_ref() }
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        self.stream().map(|s| s.telemetry.snapshot())
    }

    pub fn callbacks(&self) -> u64 {
        self.stream().map_or(0, |s| s.telemetry.callbacks())
    }

    pub fn rate_changed(&self) -> bool {
        self.stream()
            .is_some_and(|s| s.telemetry.rate_changes() > 0)
    }

    pub fn take_input_peak(&self) -> f64 {
        self.stream().map_or(0.0, |s| s.telemetry.take_input_peak())
    }

    pub fn take_reopen(&self) -> bool {
        self.stream().is_some_and(|s| s.telemetry.take_reopen())
    }

    pub fn faulted(&self) -> bool {
        self.stream()
            .is_some_and(|s| s.faulted.load(Ordering::SeqCst))
    }

    /// Stops the driver, waits until no callback is inside the stream,
    /// disposes the buffers and frees the stream. Returns the last snapshot.
    pub fn finish(mut self) -> (Option<Snapshot>, StopTimings) {
        self.stop_now()
    }

    fn stop_now(&mut self) -> (Option<Snapshot>, StopTimings) {
        let raw = core::mem::replace(&mut self.stream, ptr::null_mut());
        if raw.is_null() {
            return (None, StopTimings::default());
        }
        let d = &*self.host.driver;
        let t = Instant::now();
        let stopped = d.stop();
        let stop = t.elapsed();
        STREAM.store(ptr::null_mut(), Ordering::SeqCst);
        while IN_FLIGHT.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
        // SAFETY: the slot no longer points to the stream and no callback is
        // inside it, so this is the only reference; it came from Box::into_raw.
        let stream = unsafe { Box::from_raw(raw) };
        let snapshot = stream.telemetry.snapshot();
        let t = Instant::now();
        let disposed = d.dispose_all_buffers();
        let dispose = t.elapsed();
        drop(stream);
        (
            Some(snapshot),
            StopTimings {
                stop,
                dispose,
                stop_ok: stopped.is_ok(),
                dispose_ok: disposed.is_ok(),
            },
        )
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self.stop_now();
    }
}

/// Dispatches this thread's pending window messages (drivers may post to a
/// hidden window of the thread that created them).
pub fn pump_messages() {
    let mut msg = MSG::default();
    // SAFETY: plain Win32 calls on this thread's own queue with a valid MSG.
    unsafe {
        while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn text(s: &core::ffi::CStr) -> String {
    s.to_string_lossy().into_owned()
}

struct Stream {
    format: SampleFormat,
    /// One half-buffer of one channel.
    bytes: usize,
    inputs: Vec<[*mut c_void; 2]>,
    outputs: Vec<[*mut c_void; 2]>,
    telemetry: Telemetry,
    base: Instant,
    burn: Duration,
    panic_at: u64,
    faulted: AtomicBool,
    output_ready: AtomicBool,
    driver: *const azo::Driver,
}

static STREAM: AtomicPtr<Stream> = AtomicPtr::new(ptr::null_mut());
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static CALLBACKS: Callbacks = Callbacks {
    buffer_switch: on_buffer_switch,
    sample_rate_did_change: on_rate_change,
    asio_message: on_message,
    buffer_switch_time_info: on_buffer_switch_time_info,
};

/// Runs `f` on the live stream, if any, keeping it alive meanwhile.
fn with_stream<R>(f: impl FnOnce(&Stream) -> R) -> Option<R> {
    IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
    let raw = STREAM.load(Ordering::SeqCst);
    // SAFETY: a non-null slot points to a live stream: its owner clears the
    // slot and waits for IN_FLIGHT == 0 before it frees the stream, and this
    // callback incremented IN_FLIGHT before it read the slot.
    let out = unsafe { raw.as_ref() }.map(f);
    IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    out
}

unsafe extern "system" fn on_buffer_switch(index: c_long, _direct: Bool) {
    with_stream(|s| s.on_buffer(index != 0, None));
}

unsafe extern "system" fn on_buffer_switch_time_info(
    params: *mut Time,
    index: c_long,
    _direct: Bool,
) -> *mut Time {
    // SAFETY: the driver passes a valid Time for this call, or null.
    let position = unsafe { params.as_ref() }
        .filter(|t| {
            t.time_info
                .flags
                .contains(TimeInfoFlags::SAMPLE_POSITION_VALID)
        })
        .map(|t| i64::from(t.time_info.sample_position));
    with_stream(|s| s.on_buffer(index != 0, position));
    params
}

unsafe extern "system" fn on_message(
    sel: MessageSelector,
    value: c_long,
    _message: *const c_void,
    _opt: *const f64,
) -> c_long {
    // Drivers ask (supported selectors, engine version, time info) while
    // createBuffers runs, before the stream exists: answer without counting.
    with_stream(|s| s.telemetry.driver_message(sel.0, value))
        .unwrap_or_else(|| telemetry::reply(sel.0, value))
}

unsafe extern "system" fn on_rate_change(_rate: SampleRate) {
    with_stream(|s| s.telemetry.on_rate_change());
}

impl Stream {
    fn on_buffer(&self, second: bool, position: Option<i64>) {
        let entry = self.base.elapsed();
        let position = position.or_else(|| {
            // SAFETY: the driver outlives the stream (Running borrows Host).
            unsafe { self.driver.as_ref() }
                .and_then(|d| d.sample_position().ok())
                .map(|p| p.position)
        });
        self.telemetry.on_callback(nanos(entry).max(1), position);
        for ch in &self.outputs {
            zero(half(ch, second), self.bytes);
        }
        if !self.faulted.load(Ordering::Relaxed) {
            let caught = catch_unwind(AssertUnwindSafe(|| self.work(second)));
            if caught.is_err() {
                self.faulted.store(true, Ordering::SeqCst);
                for ch in &self.outputs {
                    zero(half(ch, second), self.bytes);
                }
            }
        }
        if self.output_ready.load(Ordering::Relaxed) {
            // SAFETY: as above.
            let ok = unsafe { self.driver.as_ref() }.is_some_and(|d| d.output_ready().is_ok());
            if !ok {
                // ASE_NotPresent: the driver does not need the signal.
                self.output_ready.store(false, Ordering::Relaxed);
            }
        }
        self.telemetry
            .on_done(nanos(self.base.elapsed().saturating_sub(entry)));
    }

    fn work(&self, second: bool) {
        let n = self.telemetry.callbacks();
        if self.panic_at != 0 && n == self.panic_at {
            inject_fault(n);
        }
        let mut peak = 0.0_f64;
        for ch in &self.inputs {
            peak = peak.max(self.format.peak(read(half(ch, second), self.bytes)));
        }
        self.telemetry.on_input_peak(peak);
        if !self.burn.is_zero() {
            let t = Instant::now();
            while t.elapsed() < self.burn {
                core::hint::spin_loop();
            }
        }
    }
}

#[allow(
    clippy::panic,
    reason = "fault injection (program spec §2.4), a dev-only flag"
)]
fn inject_fault(n: u64) -> ! {
    panic!("injected fault at callback {n}")
}

fn half(ch: &[*mut c_void; 2], second: bool) -> *mut c_void {
    let [a, b] = *ch;
    if second { b } else { a }
}

fn zero(p: *mut c_void, bytes: usize) {
    if !p.is_null() {
        // SAFETY: the driver allocated `bytes` bytes behind each half-buffer
        // pointer (buffer size × sample size) for the life of the buffers.
        unsafe { ptr::write_bytes(p.cast::<u8>(), 0, bytes) };
    }
}

fn read<'a>(p: *mut c_void, bytes: usize) -> &'a [u8] {
    if p.is_null() {
        return &[];
    }
    // SAFETY: as in `zero`; the driver does not write this half while the host
    // is inside the callback for it.
    unsafe { core::slice::from_raw_parts(p.cast::<u8>().cast_const(), bytes) }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}
```

Facts this relies on (azo 0.2.1 and windows-* 0.100 sources, read 2026-09-27):
- `DriverMetadata::description` is an `HSTRING` with `to_string_lossy()` and no `Display`.
- `create_instance()` returns `InitGuard<Driver>`, which initialises COM as STA on the calling thread and is `!Send`.
- `create_buffers` is `unsafe` and returns one `[*mut c_void; 2]` per requested channel, in order.
- `Callbacks` holds four `unsafe extern "system"` fn pointers, with no user pointer.
- `output_ready()` returns `NOT_PRESENT` on drivers that do not need it.

- [x] **Step 2: Lint and module.** In `crates/iem-audio-io/src/lib.rs`:
  - replace `#![forbid(unsafe_code)]` with `#![deny(unsafe_code)]`;
  - put the module list in this order:

```rust
#[cfg(windows)]
#[allow(unsafe_code, reason = "ASIO FFI: azo buffers and callbacks (S1a design note §3)")]
pub mod asio;
pub mod format;
pub mod nullrt;
pub mod offline;
pub mod telemetry;
pub mod wav;
```

  - after the crate doc's `NullRt` bullet, add the paragraph:

```rust
//! For the card (S1a design note §3): [`format`] (ASIO sample types ↔ f64,
//! the I2 refusals), [`telemetry`] (lock-free callback statistics) and, on
//! Windows only, `asio` — the crate's only unsafe code.
//!
```

  - change the doc's last sentence to "The ASIO host (`asio`, Windows) follows the same contract in S6."

- [x] **Step 3: Format and commit.**

```bash
cd "$WORK" && cargo fmt --all -- --check
git add crates/iem-audio-io/src/asio.rs crates/iem-audio-io/src/lib.rs
git commit -m "feat(audio-io): Windows ASIO host on azo (S1a spike)

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The `asio_spike` example

**Files:**
- Create: `crates/iem-audio-io/examples/asio_spike.rs`

- [x] **Step 1: Write the program.** Create `crates/iem-audio-io/examples/asio_spike.rs`:

```rust
//! S1a ASIO spike on the real card (design note
//! `docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md`). It runs
//! only on the IEM PC in dev time, started by the Interactive task from a
//! request file (`scripts/asio-spike/`). Outputs stay silent; the card's
//! rate, clock and buffer are never changed from here.
//!
//! Exit codes: 0 done or stopped, 1 other error, 2 usage, 3 driver missing,
//! 4 refused (rate, buffer, format), 5 band activity, 6 fault caught
//! (`--panic-at`), 7 the driver changed the sample rate.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: asio_spike probe|duplex|reopen --driver <name> --report <file> --stop-file <file> \
[--progress <file>] [--frames 32|48|64] [--seconds S] [--burn-us U] [--stress T] [--panic-at K] [--cycles C]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Mode {
    /// Read-only driver facts; no buffers.
    Probe,
    /// Duplex with silent outputs for `seconds`, optionally under load.
    Duplex,
    /// `cycles` × (start, 5 s, stop, release, open), timing every phase.
    Reopen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Args {
    mode: Mode,
    driver: String,
    report: PathBuf,
    progress: Option<PathBuf>,
    stop_file: PathBuf,
    frames: i32,
    seconds: u64,
    burn_us: u32,
    stress: u32,
    panic_at: u64,
    cycles: u32,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut it = argv.iter();
    let mode = match it.next().map(String::as_str) {
        Some("probe") => Mode::Probe,
        Some("duplex") => Mode::Duplex,
        Some("reopen") => Mode::Reopen,
        other => return Err(format!("unknown mode {other:?}")),
    };
    let mut a = Args {
        mode,
        driver: String::new(),
        report: PathBuf::new(),
        progress: None,
        stop_file: PathBuf::new(),
        frames: 0,
        seconds: 600,
        burn_us: 0,
        stress: 0,
        panic_at: 0,
        cycles: 5,
    };
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let num = |max: u64| match value.parse::<u64>() {
            Ok(n) if n <= max => Ok(n),
            _ => Err(format!(
                "{flag}: expected a number up to {max}, got {value:?}"
            )),
        };
        match flag.as_str() {
            "--driver" => a.driver.clone_from(value),
            "--report" => a.report = value.into(),
            "--progress" => a.progress = Some(value.into()),
            "--stop-file" => a.stop_file = value.into(),
            "--frames" => a.frames = i32::try_from(num(4096)?).unwrap_or(0),
            "--seconds" => a.seconds = num(3600)?,
            "--burn-us" => a.burn_us = u32::try_from(num(300)?).unwrap_or(0),
            "--stress" => a.stress = u32::try_from(num(8)?).unwrap_or(0),
            "--panic-at" => a.panic_at = num(u64::MAX)?,
            "--cycles" => a.cycles = u32::try_from(num(20)?).unwrap_or(0),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if a.driver.is_empty() || a.report.as_os_str().is_empty() || a.stop_file.as_os_str().is_empty()
    {
        return Err("--driver, --report and --stop-file are required".to_owned());
    }
    if a.mode != Mode::Probe && ![32, 48, 64].contains(&a.frames) {
        return Err("--frames must be 32, 48 or 64".to_owned());
    }
    if a.seconds == 0 || a.cycles == 0 {
        return Err("--seconds and --cycles must be positive".to_owned());
    }
    Ok(a)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse(&argv) {
        Ok(args) => platform(&args),
        Err(e) => {
            eprintln!("asio_spike: {e}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(not(windows))]
fn platform(_: &Args) -> ExitCode {
    eprintln!("asio_spike: Windows only (the card is on the IEM PC)");
    ExitCode::from(2)
}

#[cfg(windows)]
fn platform(args: &Args) -> ExitCode {
    spike::main(args)
}

#[cfg(windows)]
mod spike {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use iem_audio_io::asio::{
        self, AsioError, DriverInfo, Host, Running, StopTimings, StreamConfig,
    };
    use iem_audio_io::format::SampleFormat;
    use iem_audio_io::telemetry::{ActivityGuard, Snapshot, dbfs};
    use serde_json::{Value, json};

    use super::{Args, ExitCode, Mode};

    const FIRST_CALLBACK_WAIT: Duration = Duration::from_secs(2);
    const AFTER_FAULT: Duration = Duration::from_secs(2);
    const REOPEN_RUN: Duration = Duration::from_secs(5);
    const ACTIVITY_SECONDS: u32 = 3;

    pub fn main(a: &Args) -> ExitCode {
        let mut report = json!({
            "tool": "asio_spike",
            "version": env!("CARGO_PKG_VERSION"),
            "build_sha": option_env!("GITHUB_SHA"),
            "mode": format!("{:?}", a.mode).to_lowercase(),
            "frames": a.frames, "seconds": a.seconds, "burn_us": a.burn_us,
            "stress": a.stress, "panic_at": a.panic_at, "cycles": a.cycles,
        });
        let code = match run(a, &mut report) {
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
        };
        match write_json(&a.report, &report) {
            Ok(()) => ExitCode::from(code),
            Err(e) => {
                eprintln!("asio_spike: writing {}: {e}", a.report.display());
                ExitCode::from(1)
            }
        }
    }

    fn run(a: &Args, report: &mut Value) -> Result<u8, AsioError> {
        let host = Host::open(&a.driver)?;
        let info = host.info()?;
        report["driver"] = info_json(&info);
        match a.mode {
            Mode::Probe => {
                report["outcome"] = json!("done");
                Ok(0)
            }
            Mode::Duplex => duplex(a, host, info, report),
            Mode::Reopen => reopen(a, host, info, report),
        }
    }

    fn duplex(
        a: &Args,
        mut host: Host,
        mut info: DriverInfo,
        report: &mut Value,
    ) -> Result<u8, AsioError> {
        let cfg = StreamConfig {
            frames: a.frames,
            burn_us: a.burn_us,
            panic_at: a.panic_at,
        };
        let stress = Stress::start(a.stress);
        let deadline = Instant::now() + Duration::from_secs(a.seconds);
        let mut guard = ActivityGuard::new(ACTIVITY_SECONDS);
        let mut segments = Vec::new();
        let mut resets = Vec::new();
        let mut loudest = 0.0_f64;
        let mut fault: Option<(Instant, u64)> = None;
        let mut outcome = "done";
        loop {
            let running = host.start(&info, cfg)?;
            let first = wait_first_callback(&running)?;
            let latency = host.latencies()?;
            let t0 = Instant::now();
            let mut next_second = t0 + Duration::from_secs(1);
            let mut next_progress = t0 + Duration::from_secs(5);
            let reopen = loop {
                asio::pump_messages();
                std::thread::sleep(Duration::from_millis(10));
                let now = Instant::now();
                if a.stop_file.exists() {
                    outcome = "stopped";
                    break false;
                }
                if now >= deadline {
                    break false;
                }
                if running.take_reopen() {
                    break true;
                }
                if running.rate_changed() {
                    outcome = "rate-changed";
                    break false;
                }
                if running.faulted() {
                    match fault {
                        None => fault = Some((now, running.callbacks())),
                        Some((at, _)) if now.duration_since(at) >= AFTER_FAULT => {
                            outcome = "fault-caught";
                            break false;
                        }
                        Some(_) => {}
                    }
                }
                if now >= next_second {
                    next_second += Duration::from_secs(1);
                    let peak = running.take_input_peak();
                    loudest = loudest.max(peak);
                    if guard.observe(peak) {
                        outcome = "band-activity";
                        break false;
                    }
                }
                if now >= next_progress {
                    next_progress += Duration::from_secs(5);
                    if let (Some(path), Some(s)) = (&a.progress, running.snapshot()) {
                        let _ = write_json(path, &progress_json(t0.elapsed(), &s, loudest));
                    }
                }
            };
            let seconds = t0.elapsed().as_secs_f64();
            let (snap, stop) = running.finish();
            if let (Some((_, at)), Some(s)) = (fault, &snap) {
                report["callbacks_after_fault"] = json!(s.callbacks.saturating_sub(at));
            }
            segments.push(json!({
                "latency_in": latency.0, "latency_out": latency.1,
                "create_buffers_us": us(first.0.create_buffers), "start_us": us(first.0.start),
                "first_callback_us": us(first.1), "seconds": seconds,
                "telemetry": snap.as_ref().map(telemetry_json), "stop": stop_json(stop),
            }));
            if !reopen {
                break;
            }
            // The driver asked for a reset: release it and create it again on this thread.
            let t = Instant::now();
            drop(host);
            let release = t.elapsed();
            let t = Instant::now();
            host = Host::open(&a.driver)?;
            info = host.info()?;
            resets.push(json!({ "release_us": us(release), "open_us": us(t.elapsed()) }));
        }
        stress.stop();
        report["segments"] = json!(segments);
        report["resets_handled"] = json!(resets);
        report["loudest_input_dbfs"] = json!(dbfs(loudest));
        report["outcome"] = json!(outcome);
        Ok(match outcome {
            "band-activity" => 5,
            "fault-caught" => 6,
            "rate-changed" => 7,
            _ => 0,
        })
    }

    fn reopen(
        a: &Args,
        mut host: Host,
        mut info: DriverInfo,
        report: &mut Value,
    ) -> Result<u8, AsioError> {
        let cfg = StreamConfig {
            frames: a.frames,
            burn_us: 0,
            panic_at: 0,
        };
        let mut cycles = Vec::new();
        let mut outcome = "done";
        for cycle in 0..a.cycles {
            if a.stop_file.exists() {
                outcome = "stopped";
                break;
            }
            let running = host.start(&info, cfg)?;
            let (timings, first) = wait_first_callback(&running)?;
            let t = Instant::now();
            while t.elapsed() < REOPEN_RUN && !a.stop_file.exists() {
                asio::pump_messages();
                std::thread::sleep(Duration::from_millis(10));
            }
            let (snap, stop) = running.finish();
            let t = Instant::now();
            drop(host);
            let release = t.elapsed();
            let t = Instant::now();
            host = Host::open(&a.driver)?;
            let open = t.elapsed();
            info = host.info()?;
            cycles.push(json!({
                "cycle": cycle,
                "create_buffers_us": us(timings.create_buffers), "start_us": us(timings.start),
                "first_callback_us": us(first), "stop": stop_json(stop),
                "release_us": us(release), "open_us": us(open),
                "callbacks": snap.as_ref().map_or(0, |s| s.callbacks),
                "missed": snap.as_ref().map_or(0, |s| s.missed),
            }));
        }
        report["cycles"] = json!(cycles);
        report["outcome"] = json!(outcome);
        Ok(0)
    }

    /// Pumps messages until the first callback; the start timings and the wait.
    fn wait_first_callback(
        running: &Running<'_>,
    ) -> Result<(asio::StartTimings, Duration), AsioError> {
        let t = Instant::now();
        while t.elapsed() < FIRST_CALLBACK_WAIT {
            asio::pump_messages();
            if running.callbacks() > 0 {
                return Ok((running.timings, t.elapsed()));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(AsioError::Call(
            "start",
            format!("no callback within {FIRST_CALLBACK_WAIT:?}"),
        ))
    }

    /// Busy threads at normal priority standing in for the server and the stream.
    struct Stress {
        stop: Arc<AtomicBool>,
        threads: Vec<JoinHandle<()>>,
    }

    impl Stress {
        fn start(n: u32) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let threads = (0..n)
                .map(|_| {
                    let stop = Arc::clone(&stop);
                    std::thread::spawn(move || {
                        while !stop.load(Ordering::Relaxed) {
                            std::hint::spin_loop();
                        }
                    })
                })
                .collect();
            Self { stop, threads }
        }

        fn stop(self) {
            self.stop.store(true, Ordering::Relaxed);
            for t in self.threads {
                let _ = t.join();
            }
        }
    }

    fn us(d: Duration) -> f64 {
        d.as_secs_f64() * 1e6
    }

    fn info_json(i: &DriverInfo) -> Value {
        let mut types: BTreeMap<String, usize> = BTreeMap::new();
        for &t in &i.sample_types {
            let name = SampleFormat::from_asio(t)
                .map_or_else(|| format!("unsupported:{t}"), |f| f.name().to_owned());
            *types.entry(name).or_default() += 1;
        }
        json!({
            "name": i.name, "version": i.version, "inputs": i.inputs, "outputs": i.outputs,
            "buffer": { "min": i.buffer_min, "max": i.buffer_max, "preferred": i.buffer_preferred, "granularity": i.buffer_granularity },
            "rate": i.rate, "can_96k": i.can_96k,
            "latency_at_preferred": { "in": i.latency_in, "out": i.latency_out },
            "clocks": i.clocks.iter().map(|c| json!({ "index": c.index, "name": c.name, "current": c.current })).collect::<Vec<_>>(),
            "sample_types": types,
        })
    }

    fn telemetry_json(s: &Snapshot) -> Value {
        let q = |v: [f64; 4]| json!({ "p50": v[0], "p99": v[1], "p999": v[2], "max": v[3] });
        json!({
            "period_us": s.period_ns as f64 / 1e3,
            "callbacks": s.callbacks, "late": s.late, "missed": s.missed,
            "overruns": s.overruns, "position_gaps": s.position_gaps,
            "first_callback_us": s.first_callback_ns as f64 / 1e3,
            "messages": {
                "resets": s.resets, "resyncs": s.resyncs, "latency_changes": s.latency_changes,
                "buffer_size_changes": s.buffer_size_changes, "overloads": s.overloads, "rate_changes": s.rate_changes,
            },
            "interval_us": q(s.interval.summary_us()),
            "duration_us": q(s.duration.summary_us()),
            "drift_ppm": s.drift_ppm,
        })
    }

    fn progress_json(elapsed: Duration, s: &Snapshot, loudest: f64) -> Value {
        json!({
            "elapsed_s": elapsed.as_secs(), "callbacks": s.callbacks, "late": s.late, "missed": s.missed,
            "overruns": s.overruns, "position_gaps": s.position_gaps, "resets": s.resets,
            "loudest_input_dbfs": dbfs(loudest),
        })
    }

    fn stop_json(t: StopTimings) -> Value {
        json!({ "stop_us": us(t.stop), "dispose_us": us(t.dispose), "stop_ok": t.stop_ok, "dispose_ok": t.dispose_ok })
    }

    /// Writes `value` next to `path` and renames it into place.
    fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_a_duplex_run_under_load() {
        let a = parse(&argv(
            "duplex --driver D1 --report r.json --stop-file stop --progress p.json --frames 32 --seconds 600 --burn-us 100 --stress 4 --panic-at 7 --cycles 3",
        ))
        .unwrap();
        assert_eq!(
            a,
            Args {
                mode: Mode::Duplex,
                driver: "D1".into(),
                report: "r.json".into(),
                progress: Some("p.json".into()),
                stop_file: "stop".into(),
                frames: 32,
                seconds: 600,
                burn_us: 100,
                stress: 4,
                panic_at: 7,
                cycles: 3,
            }
        );
    }

    #[test]
    fn probe_needs_no_frames_and_has_defaults() {
        let a = parse(&argv("probe --driver D1 --report r --stop-file s")).unwrap();
        assert_eq!(
            (a.mode, a.frames, a.seconds, a.cycles, a.progress),
            (Mode::Probe, 0, 600, 5, None)
        );
        assert_eq!(
            parse(&argv(
                "reopen --driver D1 --report r --stop-file s --frames 48"
            ))
            .unwrap()
            .mode,
            Mode::Reopen
        );
    }

    #[test]
    fn bad_input_is_refused() {
        for bad in [
            "",
            "record --driver D1 --report r --stop-file s",
            "probe --driver",
            "probe --driver D1 --report r --stop-file s --colour red",
            "probe --report r --stop-file s",
            "probe --driver D1 --stop-file s",
            "probe --driver D1 --report r",
            "duplex --driver D1 --report r --stop-file s",
            "duplex --driver D1 --report r --stop-file s --frames 16",
            "duplex --driver D1 --report r --stop-file s --frames 32x",
            "duplex --driver D1 --report r --stop-file s --frames 32 --seconds 0",
            "duplex --driver D1 --report r --stop-file s --frames 32 --seconds 3601",
            "duplex --driver D1 --report r --stop-file s --frames 32 --burn-us 301",
            "duplex --driver D1 --report r --stop-file s --frames 32 --stress 9",
            "reopen --driver D1 --report r --stop-file s --frames 32 --cycles 0",
            "reopen --driver D1 --report r --stop-file s --frames 32 --cycles 21",
        ] {
            assert!(parse(&argv(bad)).is_err(), "{bad:?}");
        }
        assert!(parse(&argv("duplex --driver D1 --report r --stop-file s --frames 64 --seconds 3600 --burn-us 300 --stress 8")).is_ok());
    }
}
```

Behaviour to check in review:
- **Duplex:**
  - checks the stop file, the deadline, reset requests, a rate change, a fault and band activity;
  - pumps messages every 10 ms;
  - takes a snapshot only every 5 s, for progress, so the main thread does not allocate at 100 Hz.
- **A driver reset request:** finish, release, open, `info()` again. The next `start()` re-runs `admit` on the fresh facts.
- **`--panic-at K`:** the fault is caught in callback K; the run continues 2 s and reports `callbacks_after_fault`.

- [x] **Step 2: Format and commit.**

```bash
cd "$WORK" && cargo fmt --all -- --check
git add crates/iem-audio-io/examples/asio_spike.rs
git commit -m "feat(audio-io): asio_spike program — probe, duplex, reopen

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: PC module, task entry point, self-test

**Files:**
- Create: `scripts/asio-spike/SpikePc.psm1`, `scripts/asio-spike/spike-task.ps1`, `scripts/asio-spike/Test-SpikePc.ps1`

- [x] **Step 1: The module.** Create `scripts/asio-spike/SpikePc.psm1`:

```powershell
#Requires -Version 5.1
# S1a ASIO spike: PC-side work (design note §5). Never ends a process by
# force; never starts REAPER while the spike runs or before the driver's
# preferred buffer is back at its recorded value.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
# Get-GoldenAsioHolders, Write-GoldenStatus, Write-GoldenRequest, Test-GoldenHttp,
# Wait-GoldenProcessGone, Invoke-GoldenSaveQuit, Get-GoldenMeterSamples (S1b, reviewed).
Import-Module (Join-Path $PSScriptRoot 'GoldenPc.psm1') -Force -Global

$script:SpikeFrames = @(32, 48, 64)
$script:SpikeTaskPath = '\iemmixer\'
$script:SpikeTaskName = 'iemmixer-asio-spike'

function Get-SpikeBufferPref {
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Name)
    $item = Get-Item -LiteralPath $Key
    $kind = $item.GetValueKind($Name)
    if (@([Microsoft.Win32.RegistryValueKind]::DWord, [Microsoft.Win32.RegistryValueKind]::String) -notcontains $kind) {
        throw "$Name has registry kind $kind (expected DWord or String)"
    }
    [pscustomobject]@{ value = [int]$item.GetValue($Name); kind = "$kind" }
}

function Set-SpikeBufferPref {
    param([Parameter(Mandatory)][string]$Key, [Parameter(Mandatory)][string]$Name,
          [Parameter(Mandatory)][int]$Value, [Parameter(Mandatory)][int]$Original)
    if (($script:SpikeFrames -notcontains $Value) -and ($Value -ne $Original)) {
        throw "buffer $Value refused: only 32, 48, 64 or the recorded original $Original"
    }
    $before = Get-SpikeBufferPref -Key $Key -Name $Name
    $data = if ($before.kind -eq 'String') { "$Value" } else { $Value }
    Set-ItemProperty -LiteralPath $Key -Name $Name -Value $data -Type $before.kind
    $after = Get-SpikeBufferPref -Key $Key -Name $Name
    if ($after.value -ne $Value -or $after.kind -ne $before.kind) {
        throw "read-back $($after.value) ($($after.kind)) after writing $Value ($($before.kind))"
    }
    [pscustomobject]@{ before = $before.value; after = $after.value; kind = $after.kind }
}

function Test-SpikeSums {
    # SHA256SUMS lines: "<64 hex>  <file name>", names without any path part.
    param([Parameter(Mandatory)][string]$Bin)
    $lines = @(Get-Content -LiteralPath (Join-Path $Bin 'SHA256SUMS') | Where-Object { $_.Trim() })
    if ($lines.Count -eq 0) { throw 'SHA256SUMS is empty' }
    $names = @()
    foreach ($line in $lines) {
        if ($line -notmatch '^([0-9a-f]{64})  ([A-Za-z0-9_.-]+)$') { throw "malformed SHA256SUMS line: $line" }
        $sha = $Matches[1]; $file = $Matches[2]
        $actual = (Get-FileHash -LiteralPath (Join-Path $Bin $file) -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $sha) { throw "hash mismatch: $file" }
        $names += $file
    }
    return ,$names
}

function Get-SpikeBlockers {
    # I3: one ASIO host. Empty = the spike may open the card.
    param([Parameter(Mandatory)][string]$AsioModule)
    $problems = @()
    if (@(Get-Process -Name reaper -ErrorAction SilentlyContinue).Count -gt 0) { $problems += 'reaper.exe runs' }
    $holders = Get-GoldenAsioHolders -Module $AsioModule
    if ($holders.Count -gt 0) { $problems += "the ASIO module is held by $($holders -join ', ')" }
    if (@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count -gt 0) { $problems += 'a spike already runs' }
    return ,$problems
}

function New-SpikeArguments {
    param([Parameter(Mandatory)]$Request, [Parameter(Mandatory)][string]$Root)
    $status = Join-Path $Root 'status'
    $q = { param($s) '"' + $s + '"' }
    $a = @($Request.mode,
           '--driver', (& $q $Request.driver),
           '--report', (& $q (Join-Path $status ($Request.id + '.report.json'))),
           '--progress', (& $q (Join-Path $status ($Request.id + '.progress.json'))),
           '--stop-file', (& $q (Join-Path $Root 'queue\stop')))
    switch ($Request.mode) {
        'probe' { }
        'duplex' {
            if ($script:SpikeFrames -notcontains [int]$Request.frames) { throw "frames $($Request.frames) refused" }
            $a += @('--frames', [int]$Request.frames, '--seconds', [int]$Request.seconds, '--burn-us', [int]$Request.burn_us,
                    '--stress', [int]$Request.stress, '--panic-at', [long]$Request.panic_at)
        }
        'reopen' {
            if ($script:SpikeFrames -notcontains [int]$Request.frames) { throw "frames $($Request.frames) refused" }
            $a += @('--frames', [int]$Request.frames, '--cycles', [int]$Request.cycles)
        }
        default { throw "unknown mode $($Request.mode)" }
    }
    return ,([string[]]$a)
}

function Invoke-SpikeRun {
    # Runs in the console session (the Interactive task): one request, bounded.
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)]$Request)
    $bin = Join-Path $Root 'bin'
    $status = Join-Path $Root ("status\" + $Request.id + '.json')
    $stop = Join-Path $Root 'queue\stop'
    [void](Test-SpikeSums -Bin $bin)
    $blockers = Get-SpikeBlockers -AsioModule $Request.module
    if ($blockers.Count -gt 0) { Write-GoldenStatus -Path $status -State 'refused' -Results $blockers; return }
    $spikeArgs = New-SpikeArguments -Request $Request -Root $Root
    $p = Start-Process -FilePath (Join-Path $bin 'asio_spike.exe') -ArgumentList $spikeArgs -PassThru -NoNewWindow `
        -RedirectStandardError (Join-Path $Root ("status\" + $Request.id + '.stderr.txt'))
    $null = $p.Handle   # keeps ExitCode readable after the exit (Windows PowerShell 5.1)
    $p.PriorityClass = [Diagnostics.ProcessPriorityClass]::High
    Write-GoldenStatus -Path $status -State 'running' -Results @([pscustomobject]@{ pid = $p.Id })
    $deadline = (Get-Date).AddSeconds([int]$Request.timeout)
    while (-not $p.WaitForExit(500)) {
        if ((Get-Date) -gt $deadline -and -not (Test-Path -LiteralPath $stop)) {
            New-Item -ItemType File -Force -Path $stop | Out-Null   # graceful: the spike polls the stop file
        }
    }
    Write-GoldenStatus -Path $status -State 'exited' -Results @([pscustomobject]@{ exit = $p.ExitCode })
}

function Register-SpikeTask {
    param([Parameter(Mandatory)][string]$Root)
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + (Join-Path $Root 'bin\spike-task.ps1') + '"')
    # Over ssh USERDOMAIN is the workgroup, not the machine: take the token's own name.
    $principal = New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskPath $script:SpikeTaskPath -TaskName $script:SpikeTaskName -Action $action -Principal $principal -Settings $settings -Force | Out-Null
}

function Start-SpikeTask {
    Start-ScheduledTask -TaskPath $script:SpikeTaskPath -TaskName $script:SpikeTaskName
}

function Stop-SpikeGracefully {
    # Writes the stop file and waits for the spike to leave by itself; never kills.
    param([Parameter(Mandatory)][string]$Root, [int]$Seconds = 60)
    New-Item -ItemType File -Force -Path (Join-Path $Root 'queue\stop') | Out-Null
    $deadline = (Get-Date).AddSeconds($Seconds)
    while (@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count -gt 0) {
        if ((Get-Date) -gt $deadline) { return [pscustomobject]@{ gone = $false } }
        Start-Sleep -Milliseconds 500
    }
    [pscustomobject]@{ gone = $true }
}

function ConvertFrom-SpikeReaperLine {
    # One tab-separated line of REAPER's web interface: the fields after the verb.
    param([Parameter(Mandatory)][AllowEmptyString()][string]$Text, [Parameter(Mandatory)][string]$Verb)
    foreach ($line in ($Text -split "`n")) {
        $f = $line.TrimEnd("`r") -split "`t"
        if ($f[0] -eq $Verb) { return ,@($f | Select-Object -Skip 1) }
    }
    return ,@()
}

function Get-SpikeExtState {
    param([Parameter(Mandatory)][string]$Http, [Parameter(Mandatory)][string]$SectionKey)
    $text = (Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/GET/EXTSTATE/$SectionKey" -TimeoutSec 5).Content
    $f = ConvertFrom-SpikeReaperLine -Text $text -Verb 'EXTSTATE'
    if ($f.Count -ge 3) { return [string]$f[2] }
    return ''
}

function Invoke-SpikeBringBack {
    # "ide event" / end of window: REAPER back through our own start task, then
    # the handover checks (design note §5.4). The predecessor app kept running.
    param([Parameter(Mandatory)][string]$Http, [Parameter(Mandatory)][string]$StartTaskPath, [Parameter(Mandatory)][string]$StartTask,
          [Parameter(Mandatory)][int]$NTrack, [Parameter(Mandatory)][string]$BridgeState, [Parameter(Mandatory)][string]$BridgeAction,
          [Parameter(Mandatory)][string]$Heartbeat, [Parameter(Mandatory)][string]$AsioModule, [Parameter(Mandatory)][string]$AppHttp)
    if (@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count -gt 0) { throw 'the spike still runs: REAPER may not start (I3)' }
    $holders = Get-GoldenAsioHolders -Module $AsioModule
    if (@($holders | Where-Object { $_ -notlike 'reaper.exe:*' }).Count -gt 0) { throw "the ASIO module is held by $($holders -join ', ')" }
    if (@(Get-Process reaper -ErrorAction SilentlyContinue).Count -eq 0) { Start-ScheduledTask -TaskPath $StartTaskPath -TaskName $StartTask }
    $deadline = (Get-Date).AddSeconds(120); $tracks = -1
    while ($tracks -ne $NTrack -and (Get-Date) -lt $deadline) {
        try { $f = ConvertFrom-SpikeReaperLine -Text (Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/NTRACK" -TimeoutSec 5).Content -Verb 'NTRACK'; if ($f.Count -ge 1) { $tracks = [int]$f[0] } } catch { }
        if ($tracks -ne $NTrack) { Start-Sleep -Seconds 1 }
    }
    if ($tracks -ne $NTrack) { throw "REAPER did not load the project within 120 s (tracks $tracks, expected $NTrack)" }
    # The meter bridge: trigger it at most once, and only while its state is empty (a second trigger blocks REAPER with a dialog).
    $bridge = Get-SpikeExtState -Http $Http -SectionKey $BridgeState
    $triggered = $false
    if ($bridge -ne '1') {
        if ($bridge -ne '') { throw "meter bridge state is '$bridge': not triggered (only an empty state may be triggered)" }
        Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/$BridgeAction" -TimeoutSec 5 | Out-Null
        $triggered = $true
    }
    $h1 = Get-SpikeExtState -Http $Http -SectionKey $Heartbeat
    Start-Sleep -Seconds 3
    $h2 = Get-SpikeExtState -Http $Http -SectionKey $Heartbeat
    if ($h1 -eq $h2) { throw "the meter heartbeat does not advance ('$h1')" }
    $holders = Get-GoldenAsioHolders -Module $AsioModule
    if (@($holders | Where-Object { $_ -like 'reaper.exe:*' }).Count -ne 1) { throw "REAPER does not hold the ASIO module: $($holders -join ', ')" }
    $app = Test-GoldenHttp -Uri $AppHttp
    if (-not ($app -gt 0 -and $app -lt 500)) { throw "the predecessor app does not answer (HTTP $app)" }
    [pscustomobject]@{ tracks = $tracks; bridge_triggered = $triggered; heartbeat = 'advancing'; asio = 'reaper'; app = $app }
}

Export-ModuleMember -Function *-Spike*
```

- [x] **Step 2: The task entry point.** Create `scripts/asio-spike/spike-task.ps1`:

```powershell
#Requires -Version 5.1
# Entry point of the Interactive task \iemmixer\iemmixer-asio-spike: runs one
# request from queue\request.json in the console session (S1a design note §5).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'SpikePc.psm1') -Force
$root = Split-Path -Parent $PSScriptRoot
$req = Get-Content -LiteralPath (Join-Path $root 'queue\request.json') -Raw | ConvertFrom-Json
$status = Join-Path $root ("status\" + $req.id + '.json')
try {
    if ($req.kind -ne 'spike') { throw "unknown request kind: $($req.kind)" }
    Invoke-SpikeRun -Root $root -Request $req
} catch {
    Write-GoldenStatus -Path $status -State 'failed' -Results @([pscustomobject]@{ error = "$_" })
    exit 1
}
```

- [x] **Step 3: The self-test** (Windows PowerShell 5.1 in the CI job `asio-spike`). Create `scripts/asio-spike/Test-SpikePc.ps1`:

```powershell
#Requires -Version 5.1
# Self-test of the S1a PC module on Windows PowerShell 5.1 (CI job asio-spike).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
foreach ($f in (Get-ChildItem -LiteralPath $here -File | Where-Object { @('.ps1', '.psm1') -contains $_.Extension })) {
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "parse errors in $($f.Name): $($errors[0].Message)" }
}
# The bundle layout: SpikePc.psm1 next to GoldenPc.psm1.
$base = Join-Path ([IO.Path]::GetTempPath()) ('spike-test-' + [guid]::NewGuid())
$bin = Join-Path $base 'bin'
New-Item -ItemType Directory -Force -Path $bin, (Join-Path $base 'queue'), (Join-Path $base 'status') | Out-Null
Copy-Item -LiteralPath (Join-Path $here 'SpikePc.psm1'), (Join-Path $here '..\golden\GoldenPc.psm1') -Destination $bin
Import-Module (Join-Path $bin 'SpikePc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }

# Driver preferred buffer: kind kept, read back, only 32/48/64 or the original.
$key = 'HKCU:\Software\iemmixer-spike-test-' + [guid]::NewGuid()
New-Item -Path $key -Force | Out-Null
try {
    New-ItemProperty -LiteralPath $key -Name 'Pref' -Value 64 -PropertyType DWord | Out-Null
    $p = Get-SpikeBufferPref -Key $key -Name 'Pref'
    Assert ($p.value -eq 64 -and $p.kind -eq 'DWord') 'buffer-pref-reads-value-and-kind'
    $r = Set-SpikeBufferPref -Key $key -Name 'Pref' -Value 32 -Original 64
    Assert ($r.before -eq 64 -and $r.after -eq 32 -and (Get-SpikeBufferPref -Key $key -Name 'Pref').value -eq 32) 'buffer-pref-set-reads-back'
    Throws { Set-SpikeBufferPref -Key $key -Name 'Pref' -Value 16 -Original 64 } 'buffer-pref-refuses-16'
    Throws { Set-SpikeBufferPref -Key $key -Name 'Pref' -Value 128 -Original 64 } 'buffer-pref-refuses-other-than-original'
    Assert ((Get-SpikeBufferPref -Key $key -Name 'Pref').value -eq 32) 'buffer-pref-refusal-changes-nothing'
    [void](Set-SpikeBufferPref -Key $key -Name 'Pref' -Value 128 -Original 128)
    Assert ((Get-SpikeBufferPref -Key $key -Name 'Pref').value -eq 128) 'buffer-pref-restores-any-recorded-original'
    New-ItemProperty -LiteralPath $key -Name 'Text' -Value '64' -PropertyType String | Out-Null
    [void](Set-SpikeBufferPref -Key $key -Name 'Text' -Value 48 -Original 64)
    $t = Get-SpikeBufferPref -Key $key -Name 'Text'
    Assert ($t.value -eq 48 -and $t.kind -eq 'String') 'buffer-pref-keeps-string-kind'
    New-ItemProperty -LiteralPath $key -Name 'Blob' -Value ([byte[]](1, 2)) -PropertyType Binary | Out-Null
    Throws { Get-SpikeBufferPref -Key $key -Name 'Blob' } 'buffer-pref-refuses-binary'
} finally {
    Remove-Item -LiteralPath $key -Recurse -Force
}

# Bundle hashes.
$sums = Join-Path $base 'sums'
New-Item -ItemType Directory -Force -Path $sums | Out-Null
Set-Content -LiteralPath (Join-Path $sums 'a.exe') -Value 'binary'
$h = (Get-FileHash -LiteralPath (Join-Path $sums 'a.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath (Join-Path $sums 'SHA256SUMS') -Value "$h  a.exe"
$n = Test-SpikeSums -Bin $sums
Assert ($n.Count -eq 1 -and $n[0] -eq 'a.exe') 'sums-accept-a-matching-file'
Set-Content -LiteralPath (Join-Path $sums 'a.exe') -Value 'tampered'
Throws { Test-SpikeSums -Bin $sums } 'sums-detect-a-changed-file'
Set-Content -LiteralPath (Join-Path $sums 'SHA256SUMS') -Value "$h  ..\a.exe"
Throws { Test-SpikeSums -Bin $sums } 'sums-refuse-a-path'
Set-Content -LiteralPath (Join-Path $sums 'SHA256SUMS') -Value ''
Throws { Test-SpikeSums -Bin $sums } 'sums-refuse-an-empty-list'

# Spike arguments from a request.
$req = [pscustomobject]@{ id = 'spike-1'; mode = 'duplex'; driver = 'Some Card'; frames = 32; seconds = 600; burn_us = 100; stress = 4; panic_at = 0; cycles = 5 }
$a = New-SpikeArguments -Request $req -Root 'C:\x y'
Assert ($a[0] -eq 'duplex' -and $a[2] -eq '"Some Card"' -and $a[4] -eq '"C:\x y\status\spike-1.report.json"' -and $a[8] -eq '"C:\x y\queue\stop"') 'arguments-quote-paths-and-driver'
Assert (($a -join ' ') -like '*--frames 32 --seconds 600 --burn-us 100 --stress 4 --panic-at 0') 'arguments-duplex-options'
$req.mode = 'probe'
Assert ((New-SpikeArguments -Request $req -Root 'C:\r').Count -eq 9) 'arguments-probe-has-no-options'
$req.mode = 'reopen'
Assert (((New-SpikeArguments -Request $req -Root 'C:\r') -join ' ') -like '*--frames 32 --cycles 5') 'arguments-reopen-options'
$req.frames = 16
Throws { New-SpikeArguments -Request $req -Root 'C:\r' } 'arguments-refuse-frames-16'
$req.mode = 'record'
Throws { New-SpikeArguments -Request $req -Root 'C:\r' } 'arguments-refuse-an-unknown-mode'

# REAPER web-interface lines.
$f = ConvertFrom-SpikeReaperLine -Text "NTRACK`t45`n" -Verb 'NTRACK'
Assert ($f.Count -eq 1 -and $f[0] -eq '45') 'reaper-line-ntrack'
$f = ConvertFrom-SpikeReaperLine -Text "TRACK`t1`n`r`nEXTSTATE`tsec`tkey`t1`r`n" -Verb 'EXTSTATE'
Assert ($f.Count -eq 3 -and $f[2] -eq '1') 'reaper-line-extstate-crlf'
Assert ((ConvertFrom-SpikeReaperLine -Text '' -Verb 'NTRACK').Count -eq 0) 'reaper-line-absent'

# Nothing blocks on a runner without REAPER or the card; a stop with no spike returns at once.
$b = Get-SpikeBlockers -AsioModule 'iemmixer-no-such-module.dll'
Assert ($b.Count -eq 0) 'blockers-none-without-reaper-or-card'
Assert ((Stop-SpikeGracefully -Root $base -Seconds 5).gone -and (Test-Path -LiteralPath (Join-Path $base 'queue\stop'))) 'stop-writes-the-stop-file'

Remove-Item -LiteralPath $base -Recurse -Force
Write-Host 'Test-SpikePc: all passed'
```

- [x] **Step 4: Integrity scan and commit.**

```bash
cd "$WORK" && python3 scripts/check_integrity.py
git add scripts/asio-spike/SpikePc.psm1 scripts/asio-spike/spike-task.ps1 scripts/asio-spike/Test-SpikePc.ps1
git commit -m "feat(asio-spike): PC module, Interactive task entry, self-test

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: The dev-box window driver

**Files:**
- Create: `scripts/asio-spike/spike_window.py`, `scripts/asio-spike/test_spike_window.py`

- [x] **Step 1: Tests first.** Create `scripts/asio-spike/test_spike_window.py`:

```python
"""Tests for scripts/asio-spike/spike_window.py (pure parts and the event
guard; ssh is the PC)."""
from __future__ import annotations

import hashlib
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import spike_window as sw  # noqa: E402

FULL = "\n".join(f"{k}=v" for k in sw.REQUIRED if k not in ("PC_BUFFER_ORIGINAL", "PC_NTRACK")) + "\nPC_BUFFER_ORIGINAL=64\nPC_NTRACK=9\n"


def write(text: str, name: str = "asio-spike.env") -> Path:
    p = Path(tempfile.mkdtemp()) / name
    p.write_text(text, encoding="utf-8")
    return p


class EnvTests(unittest.TestCase):
    def test_complete_env_loads(self) -> None:
        env = sw.load_env(write(FULL.replace("PC_SSH=v", 'PC_SSH="u@h"') + "# comment\n"))
        self.assertEqual((env["PC_SSH"], env["PC_BUFFER_ORIGINAL"]), ("u@h", "64"))

    def test_missing_keys_are_named(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "missing PC_BUFFER_KEY"):
            sw.load_env(write(FULL.replace("PC_BUFFER_KEY=v\n", "")))

    def test_numbers_must_be_numbers(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "PC_BUFFER_ORIGINAL must be a whole number"):
            sw.load_env(write(FULL.replace("PC_BUFFER_ORIGINAL=64", "PC_BUFFER_ORIGINAL=sixty")))

    def test_missing_file(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "missing"):
            sw.load_env(Path(tempfile.mkdtemp()) / "absent.env")


class RequestTests(unittest.TestCase):
    def test_limits(self) -> None:
        sw.check_request("probe", None, 600, 0, 0, 5)
        sw.check_request("duplex", 32, 3600, 300, 8, 20)
        for bad in (("record", 32, 600, 0, 0, 5), ("duplex", 16, 600, 0, 0, 5), ("reopen", None, 600, 0, 0, 5),
                    ("duplex", 32, 0, 0, 0, 5), ("duplex", 32, 3601, 0, 0, 5), ("duplex", 32, 600, 301, 0, 5),
                    ("duplex", 32, 600, 0, 9, 5), ("reopen", 48, 600, 0, 0, 0), ("reopen", 48, 600, 0, 0, 21)):
            with self.assertRaises(sw.StepError, msg=str(bad)):
                sw.check_request(*bad)

    def test_timeouts(self) -> None:
        self.assertEqual([sw.run_timeout("probe", 600, 5), sw.run_timeout("duplex", 600, 5), sw.run_timeout("reopen", 600, 5)], [60, 660, 210])

    def test_request_hashtable_quotes_text_and_keeps_numbers(self) -> None:
        self.assertEqual(sw.ps_hashtable({"mode": "duplex", "driver": "It's a card", "frames": 32}),
                         "@{ mode = 'duplex'; driver = 'It''s a card'; frames = 32 }")
        self.assertEqual(sw.ps_hashtable({"flag": True}), "@{ flag = 'True' }")
        with self.assertRaises(sw.StepError):
            sw.ps_hashtable({"a; b": 1})


class UndoPlanTests(unittest.TestCase):
    def state(self, **kw) -> dict:
        s = {"card": "reaper", "pref_original": 64, "pref_current": None, "pref_restored": False}
        s.update(kw)
        return s

    def test_nothing_to_undo_before_the_switch(self) -> None:
        self.assertEqual(sw.undo_plan(self.state(), spike_running=False), [])

    def test_a_running_spike_is_stopped_restored_and_reaper_comes_back(self) -> None:
        self.assertEqual(sw.undo_plan(self.state(card="free", pref_current=32), spike_running=True),
                         ["stop-spike", "restore-buffer", "bring-back"])

    def test_a_half_done_switch_still_brings_reaper_back(self) -> None:
        self.assertEqual(sw.undo_plan(self.state(card="switching"), spike_running=False), ["bring-back"])

    def test_a_restored_or_unchanged_buffer_is_not_written_again(self) -> None:
        self.assertEqual(sw.undo_plan(self.state(card="free", pref_current=32, pref_restored=True), False), ["bring-back"])
        self.assertEqual(sw.undo_plan(self.state(card="free", pref_current=64), False), ["bring-back"])
        self.assertTrue(sw.buffer_changed(self.state(pref_current=48)))
        self.assertFalse(sw.buffer_changed(self.state()))


class PreflightTests(unittest.TestCase):
    GOOD = {"pref": 64, "reaper": 1, "app": 1, "spike": 0, "holders": ["reaper.exe:6496"], "task": True, "files": 4}

    def test_good_state_passes(self) -> None:
        self.assertEqual(sw.preflight_problems(dict(self.GOOD), 64), [])
        self.assertEqual(sw.preflight_problems(dict(self.GOOD, holders=None), 64), [])

    def test_each_problem_is_named(self) -> None:
        for change, words in (({"pref": 32}, "preferred buffer is 32"), ({"reaper": 0}, "REAPER is not running"),
                              ({"app": 0}, "predecessor app"), ({"spike": 1}, "spike already runs"),
                              ({"holders": ["asio_spike.exe:1"]}, "holders"), ({"task": False}, "not registered"),
                              ({"files": 3}, "3 verified")):
            problems = sw.preflight_problems(dict(self.GOOD, **change), 64)
            self.assertEqual(len(problems), 1, change)
            self.assertIn(words, problems[0])


class BundleTests(unittest.TestCase):
    def bundle(self) -> Path:
        d = Path(tempfile.mkdtemp())
        lines = []
        for name in sw.BUNDLE_FILES:
            (d / name).write_bytes(name.encode())
            lines.append(f"{hashlib.sha256(name.encode()).hexdigest()}  {name}")
        (d / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")
        return d

    def test_a_complete_bundle_verifies(self) -> None:
        self.assertEqual(sw.verify_bundle(self.bundle()), sorted(sw.BUNDLE_FILES))

    def test_a_changed_file_fails(self) -> None:
        d = self.bundle()
        (d / "asio_spike.exe").write_bytes(b"other")
        with self.assertRaisesRegex(sw.StepError, "asio_spike.exe"):
            sw.verify_bundle(d)

    def test_a_missing_or_extra_entry_fails(self) -> None:
        d = self.bundle()
        text = (d / "SHA256SUMS").read_text(encoding="utf-8")
        (d / "SHA256SUMS").write_text("\n".join(text.splitlines()[1:]), encoding="utf-8")
        with self.assertRaisesRegex(sw.StepError, "expected"):
            sw.verify_bundle(d)

    def test_paths_and_bad_lines_are_refused(self) -> None:
        for bad in ("0" * 64 + "  ../x.exe", "0" * 64 + " one-space.exe", "xyz  a.exe"):
            with self.assertRaises(sw.StepError, msg=bad):
                sw.parse_sums(bad)
        self.assertEqual(sw.parse_sums("\n" + "a" * 64 + "  a.exe\n"), {"a.exe": "a" * 64})

    def test_only_a_green_dev_push_run_of_that_sha(self) -> None:
        runs = [
            {"databaseId": 1, "headSha": "s", "event": "pull_request", "headBranch": "dev", "conclusion": "success"},
            {"databaseId": 2, "headSha": "s", "event": "push", "headBranch": "dev", "conclusion": "failure"},
            {"databaseId": 3, "headSha": "t", "event": "push", "headBranch": "dev", "conclusion": "success"},
            {"databaseId": 4, "headSha": "s", "event": "push", "headBranch": "main", "conclusion": "success"},
            {"databaseId": 5, "headSha": "s", "event": "push", "headBranch": "dev", "conclusion": "success"},
        ]
        self.assertEqual(sw.pick_run(runs, "s"), 5)
        with self.assertRaises(sw.StepError):
            sw.pick_run(runs[:4], "s")


class VerdictTests(unittest.TestCase):
    def report(self, outcome: str = "done", **tel) -> dict:
        t = {"callbacks": 1000, "late": 2, "missed": 0, "overruns": 0, "position_gaps": 0,
             "messages": {"resets": 0}, "interval_us": {"p999": 410.0}}
        t.update(tel)
        return {"outcome": outcome, "segments": [{"telemetry": t}]}

    def test_a_clean_run_is_stable(self) -> None:
        v = sw.verdict(self.report())
        self.assertEqual((v["stable"], v["callbacks"], v["late"], v["interval_p999_us"]), (True, 1000, 2, 410.0))

    def test_any_missed_overrun_gap_reset_or_early_end_is_unstable(self) -> None:
        for r in (self.report(missed=1), self.report(overruns=1), self.report(position_gaps=1),
                  self.report(messages={"resets": 1}), self.report("stopped"), {"outcome": "done", "segments": []}):
            self.assertFalse(sw.verdict(r)["stable"], r)

    def test_segments_add_up(self) -> None:
        r = self.report()
        r["segments"].append({"telemetry": {"callbacks": 5, "missed": 1, "interval_us": {"p999": 900.0}}})
        r["segments"].append({"telemetry": None})
        v = sw.verdict(r)
        self.assertEqual((v["callbacks"], v["missed"], v["interval_p999_us"], v["stable"]), (1005, 1, 900.0, False))


class GuardTests(unittest.TestCase):
    """guarded() with local commands standing in for ssh."""

    def setUp(self) -> None:
        self.flag = Path(tempfile.mkdtemp()) / "EVENT-NOW"
        self.saved = (sw.EVENT_NOW, sw.POLL_S)
        sw.EVENT_NOW, sw.POLL_S = self.flag, 0.1

    def tearDown(self) -> None:
        sw.EVENT_NOW, sw.POLL_S = self.saved

    def py(self, code: str) -> list[str]:
        return [sys.executable, "-c", code]

    def test_output_and_stdin_pass_through(self) -> None:
        out = sw.guarded(self.py("import sys, time; time.sleep(0.3); print(sys.stdin.read().upper())"), "hello\n", 10, "finish")
        self.assertEqual(out.strip(), "HELLO")

    def test_a_failing_command_raises(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "exit 3"):
            sw.guarded(self.py("import sys; sys.exit(3)"), "", 10, "finish")

    def test_abandon_returns_within_a_poll(self) -> None:
        self.flag.touch()
        t = time.monotonic()
        with self.assertRaises(sw.EventNow):
            sw.guarded(self.py("import time; time.sleep(5)"), "", 10, "abandon")
        self.assertLess(time.monotonic() - t, 2.0)

    def test_finish_completes_the_call_first(self) -> None:
        self.flag.touch()
        t = time.monotonic()
        with self.assertRaises(sw.EventNow):
            sw.guarded(self.py("import time; time.sleep(0.5)"), "", 10, "finish")
        self.assertGreaterEqual(time.monotonic() - t, 0.5)

    def test_ignore_is_for_the_preemption_itself(self) -> None:
        self.flag.touch()
        self.assertEqual(sw.guarded(self.py("print('ok')"), "", 10, "ignore").strip(), "ok")

    def test_a_call_past_its_bound_is_reported_not_killed(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "never kill"):
            sw.guarded(self.py("import time; time.sleep(3)"), "", 0.3, "finish")

    def test_the_flag_file_is_the_event_signal(self) -> None:
        self.assertFalse(sw.event_now())
        self.flag.touch()
        self.assertTrue(sw.event_now())


if __name__ == "__main__":
    unittest.main()
```

Run `python3 -m unittest discover -s scripts/asio-spike -p 'test_*.py'`. Expected: an import error (the module does not exist yet).

- [x] **Step 2: The driver.** Create `scripts/asio-spike/spike_window.py`:

```python
#!/usr/bin/env python3
"""S1a ASIO-spike window driver on the dev box (design note §5); also the
interim switch between REAPER and iemmixer until S6's `iemmode`.

A window opens only with the owner's quoted "event skončil" and never while
the "ide event" flag file exists. Every wait checks that flag every 2 s; when
it appears the driver stops the spike through its stop file, restores the
driver's preferred buffer (read back) and brings REAPER back with the handover
checks (`preempt`). PC work runs in SpikePc.psm1 over ssh; site values come
only from the private env file ($SPIKE_ENV). Nothing is ever ended by force."""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "golden"))
from golden_window import StepError, check_signal, interlock_hits, parse_meter_peaks, ps_quote  # noqa: E402

REQUIRED = (
    "PC_SSH", "PC_ROOT", "PC_ROOT_SCP", "PC_ASIO_MODULE", "PC_ASIO_DRIVER",
    "PC_BUFFER_KEY", "PC_BUFFER_NAME", "PC_BUFFER_ORIGINAL",
    "PC_REAPER_HTTP", "PC_MAIN_PROJECT", "PC_REAPER_START_TASK_PATH", "PC_REAPER_START_TASK",
    "PC_NTRACK", "PC_METER_BRIDGE", "PC_METER_HEARTBEAT", "PC_METER_ACTION",
    "PC_APP_PROCESS", "PC_APP_HTTP", "RAW_DIR",
)
FRAMES = (32, 48, 64)
POLL_S = 2.0
REPO = "zbynekdrlik/iemmixer"
BUNDLE_FILES = ("GoldenPc.psm1", "SpikePc.psm1", "asio_spike.exe", "spike-task.ps1")
TASK = "-TaskPath '\\iemmixer\\' -TaskName 'iemmixer-asio-spike'"
STATE = Path(os.environ.get("SPIKE_STATE", str(Path.home() / ".local/state/iemmixer/spike-window.json")))
EVENT_NOW = Path(os.environ.get("IEMMIXER_EVENT_NOW", str(Path.home() / ".config/iemmixer/EVENT-NOW")))


class EventNow(Exception):
    """The owner said "ide event" (the flag file exists): pre-empt."""


def load_env(path: Path) -> dict[str, str]:
    if not path.is_file():
        raise StepError(f"{path}: missing (private env, plan Task 10)")
    env: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep:
            raise StepError(f"{path}: not KEY=VALUE: {key}")
        env[key.strip()] = value.strip().strip('"')
    missing = [k for k in REQUIRED if not env.get(k)]
    if missing:
        raise StepError(f"{path}: missing {', '.join(missing)}")
    for k in ("PC_BUFFER_ORIGINAL", "PC_NTRACK"):
        if not env[k].isdigit():
            raise StepError(f"{path}: {k} must be a whole number")
    return env


def event_now() -> bool:
    return EVENT_NOW.exists()


def check_request(mode: str, frames: int | None, seconds: int, burn_us: int, stress: int, cycles: int) -> None:
    if mode not in ("probe", "duplex", "reopen"):
        raise StepError(f"unknown mode {mode}")
    if mode != "probe" and frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if not (1 <= seconds <= 3600 and 0 <= burn_us <= 300 and 0 <= stress <= 8 and 1 <= cycles <= 20):
        raise StepError("limits: seconds 1..3600, burn-us 0..300, stress 0..8, cycles 1..20")


def run_timeout(mode: str, seconds: int, cycles: int) -> int:
    """Seconds after which the PC task writes the stop file itself."""
    return {"probe": 60, "duplex": seconds + 60, "reopen": 30 * cycles + 60}[mode]


def buffer_changed(state: dict) -> bool:
    return state.get("pref_current") not in (None, state.get("pref_original")) and not state.get("pref_restored")


def undo_plan(state: dict, spike_running: bool) -> list[str]:
    """What leaving the window (or "ide event") must do, in order."""
    plan: list[str] = []
    if spike_running:
        plan.append("stop-spike")
    if buffer_changed(state):
        plan.append("restore-buffer")
    if state.get("card") in ("switching", "free"):
        plan.append("bring-back")
    return plan


def preflight_problems(r: dict, original: int) -> list[str]:
    problems = []
    if r["pref"] != original:
        problems.append(f"the driver's preferred buffer is {r['pref']}, the recorded original is {original}: stop and tell the owner")
    if not r["reaper"]:
        problems.append("REAPER is not running (a window starts from the event state)")
    if not r["app"]:
        problems.append("the predecessor app is not running")
    if r["spike"]:
        problems.append("a spike already runs")
    if any(not h.lower().startswith("reaper.exe:") for h in r["holders"] or []):
        problems.append(f"unexpected ASIO module holders {r['holders']}")
    if not r["task"]:
        problems.append("the spike task is not registered (run setup)")
    if r["files"] != len(BUNDLE_FILES):
        problems.append(f"{r['files']} verified bundle files on the PC, expected {len(BUNDLE_FILES)}")
    return problems


def ps_hashtable(fields: dict) -> str:
    parts = []
    for k, v in fields.items():
        if not re.fullmatch(r"[a-z_]+", k):
            raise StepError(f"bad request field {k!r}")
        parts.append(f"{k} = {v}" if isinstance(v, int) and not isinstance(v, bool) else f"{k} = {ps_quote(str(v))}")
    return "@{ " + "; ".join(parts) + " }"


def parse_sums(text: str) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        m = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not m:
            raise StepError(f"malformed SHA256SUMS line: {line!r}")
        sums[m.group(2)] = m.group(1)
    return sums


def verify_bundle(bundle: Path) -> list[str]:
    sums = parse_sums((bundle / "SHA256SUMS").read_text(encoding="utf-8"))
    if sorted(sums) != sorted(BUNDLE_FILES):
        raise StepError(f"bundle lists {sorted(sums)}, expected {sorted(BUNDLE_FILES)}")
    for name, sha in sums.items():
        if hashlib.sha256((bundle / name).read_bytes()).hexdigest() != sha:
            raise StepError(f"bundle file {name} does not match SHA256SUMS")
    return sorted(sums)


def pick_run(runs: list[dict], sha: str) -> int:
    """The successful push run on dev for exactly `sha` (P5)."""
    for r in runs:
        if (r.get("headSha"), r.get("event"), r.get("headBranch"), r.get("conclusion")) == (sha, "push", "dev", "success"):
            return int(r["databaseId"])
    raise StepError(f"no successful push run on dev for {sha}")


def verdict(report: dict) -> dict:
    """Stable = ended as planned with no missed period, no overrun, no
    position gap and no driver reset."""
    tel = [s.get("telemetry") or {} for s in report.get("segments", [])]
    total = {k: sum(t.get(k, 0) for t in tel) for k in ("callbacks", "late", "missed", "overruns", "position_gaps")}
    resets = sum((t.get("messages") or {}).get("resets", 0) for t in tel)
    worst = max(((t.get("interval_us") or {}).get("p999", 0.0) for t in tel), default=0.0)
    stable = report.get("outcome") == "done" and bool(tel) and resets == 0 and all(
        total[k] == 0 for k in ("missed", "overruns", "position_gaps"))
    return {"outcome": report.get("outcome"), "stable": stable, "resets": resets, "interval_p999_us": worst, **total}


def guarded(cmd: list[str], stdin: str, timeout: float, event: str) -> str:
    """Runs `cmd` and checks the "ide event" flag every POLL_S seconds.
    event="abandon": a read-only call is left to end by itself and EventNow
    is raised at once; "finish": a changing call completes, then EventNow;
    "ignore": the pre-emption itself. Never kills anything."""
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    deadline = time.monotonic() + timeout
    seen = False
    data: str | None = stdin
    while True:
        try:
            out, err = proc.communicate(data, timeout=POLL_S)
            break
        except subprocess.TimeoutExpired:
            data = None  # already handed over; a retry must not send it again
            if event != "ignore" and event_now():
                seen = True
                if event == "abandon":
                    raise EventNow() from None
            if time.monotonic() > deadline:
                raise StepError(f"PC call still running after {timeout} s (bounded on the PC; check it, never kill)") from None
    if proc.returncode != 0:
        raise StepError(f"PC command failed (exit {proc.returncode}): {err.strip()[-1500:]}")
    if event != "ignore" and (seen or event_now()):
        raise EventNow()
    return out


# ---- PC access (the PC is the external dependency; no unit tests below) ----

def ssh_cmd(env: dict[str, str]) -> list[str]:
    return ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"],
            "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -"]


def ps(env: dict[str, str], body: str, timeout: float = 300, event: str = "finish"):
    """Runs `body` after importing SpikePc (single-line statements: `-Command -`
    reads stdin line by line); PC errors come back as {ok: false}."""
    script = "\n".join([
        "$ErrorActionPreference = 'Stop'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"try {{ Import-Module (Join-Path {ps_quote(env['PC_ROOT'])} 'bin\\SpikePc.psm1') -Force ; $r = & {{ {body} }} ; $o = [pscustomobject]@{{ ok = $true; r = $r }} }} "
        f"catch {{ $o = [pscustomobject]@{{ ok = $false; error = \"$_\" }} }} ; ConvertTo-Json -InputObject $o -Depth 8 -Compress",
    ])
    out = [line for line in guarded(ssh_cmd(env), script + "\n", timeout, event).splitlines() if line.strip()]
    doc = json.loads(out[-1]) if out else {"ok": False, "error": "no output from the PC"}
    if not doc["ok"]:
        raise StepError(f"PC step failed: {doc['error']}")
    return doc["r"]


def scp(src: str, dst: str) -> None:
    proc = subprocess.run(["scp", "-q", "-o", "BatchMode=yes", src, dst], capture_output=True, text=True, check=False, timeout=600)
    if proc.returncode != 0:
        raise StepError(f"scp failed: {proc.stderr.strip()[-800:]}")


def remote(env: dict[str, str], rel: str) -> str:
    return f"{env['PC_SSH']}:{env['PC_ROOT_SCP']}/{rel}"


def pc(env: dict[str, str], rel: str) -> str:
    return ps_quote(env["PC_ROOT"] + "\\" + rel.replace("/", "\\"))


def raw_dir(env: dict[str, str], state: dict) -> Path:
    d = Path(env["RAW_DIR"]).expanduser() / "asio-spike" / state["id"]
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    return d


def bundle_dir(env: dict[str, str], sha: str) -> Path:
    return Path(env["RAW_DIR"]).expanduser() / "asio-spike" / "bundles" / sha


# ---- state ----

def load_state() -> dict:
    if not STATE.is_file():
        raise StepError("no window: run 'new --signal ...' first")
    return json.loads(STATE.read_text(encoding="utf-8"))


def save_state(state: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=1), encoding="utf-8")
    tmp.replace(STATE)


def open_state() -> dict:
    state = load_state()
    if state.get("closed"):
        raise StepError(f"window {state['id']} is closed: open a new one")
    if event_now():
        raise EventNow()
    return state


def spike_running(env: dict[str, str], event: str = "abandon") -> bool:
    return bool(ps(env, "@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count", timeout=60, event=event))


# ---- commands ----

def cmd_new(env, args) -> None:
    check_signal(args.signal)
    if event_now():
        raise StepError(f"{EVENT_NOW} exists: an event is on, no window")
    if STATE.is_file() and not json.loads(STATE.read_text(encoding="utf-8")).get("closed"):
        raise StepError("the last window is still open: finish it (to-event) or run preempt")
    wid = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    save_state({"id": wid, "signal": args.signal, "card": "reaper", "pref_original": int(env["PC_BUFFER_ORIGINAL"]),
                "pref_current": None, "pref_restored": False, "runs": [], "closed": False})
    print(wid)


def cmd_fetch_bundle(env, args) -> None:
    sha = args.sha
    subprocess.run(["git", "fetch", "-q", "origin", "dev"], check=True)
    if subprocess.run(["git", "merge-base", "--is-ancestor", sha, "origin/dev"], check=False).returncode != 0:
        raise StepError(f"{sha} is not on origin/dev")
    listing = subprocess.run(
        ["gh", "run", "list", "-R", REPO, "--workflow", "ci.yml", "--branch", "dev", "--event", "push", "--limit", "50",
         "--json", "databaseId,headSha,event,headBranch,conclusion"], check=True, capture_output=True, text=True)
    run_id = pick_run(json.loads(listing.stdout), sha)
    dest = bundle_dir(env, sha)
    if dest.exists():
        raise StepError(f"{dest} exists")
    subprocess.run(["gh", "run", "download", str(run_id), "-R", REPO, "-n", f"asio-spike-{sha}", "-D", str(dest)], check=True)
    files = verify_bundle(dest)
    (dest.parent / f"{sha}.source-sha").write_text(sha + "\n", encoding="utf-8")
    print(json.dumps({"bundle": str(dest), "run": run_id, "files": files}))


def cmd_setup(env, args) -> None:
    state = open_state()
    bundle = bundle_dir(env, args.sha)
    if (bundle.parent / f"{args.sha}.source-sha").read_text(encoding="utf-8").strip() != args.sha:
        raise StepError("bundle .source-sha differs from --sha (P5: only the reviewed dev commit's bundle)")
    verify_bundle(bundle)
    dirs = ", ".join(pc(env, d) for d in ("bin", "queue", "status"))
    guarded(ssh_cmd(env), f"New-Item -ItemType Directory -Force -Path {dirs} | Out-Null\n", 60, "finish")
    for name in (*BUNDLE_FILES, "SHA256SUMS"):
        scp(str(bundle / name), remote(env, f"bin/{name}"))
    names = ps(env, f"$n = Test-SpikeSums -Bin {pc(env, 'bin')} ; Register-SpikeTask -Root {ps_quote(env['PC_ROOT'])} ; $n")
    state["bundle_sha"] = args.sha
    save_state(state)
    print(json.dumps({"setup": args.sha, "verified": names, "task": "registered"}))


def cmd_preflight(env, args) -> None:
    state = open_state()
    if state["card"] != "reaper":
        raise StepError("preflight belongs before to-dev")
    r = ps(env, " ; ".join([
        f"$p = Get-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])}",
        f"$h = Get-GoldenAsioHolders -Module {ps_quote(env['PC_ASIO_MODULE'])}",
        f"$s = Test-SpikeSums -Bin {pc(env, 'bin')}",
        "[pscustomobject]@{ pref = $p.value; kind = $p.kind; holders = @($h); "
        "reaper = @(Get-Process reaper -ErrorAction SilentlyContinue).Count; "
        f"app = @(Get-Process -Name {ps_quote(env['PC_APP_PROCESS'])} -ErrorAction SilentlyContinue).Count; "
        "spike = @(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count; "
        f"task = [bool](Get-ScheduledTask {TASK} -ErrorAction SilentlyContinue); files = @($s).Count }}",
    ]), timeout=120, event="abandon")
    problems = preflight_problems(r, state["pref_original"])
    if problems:
        raise StepError("; ".join(problems))
    state["preflight"] = r
    save_state(state)
    print(json.dumps({"preflight": r}))


def cmd_to_dev(env, args) -> None:
    state = open_state()
    if state["card"] != "reaper" or "preflight" not in state:
        raise StepError("run preflight first (REAPER must hold the card)")
    texts = ps(env, f"Get-GoldenMeterSamples -Http {ps_quote(env['PC_REAPER_HTTP'])} -Seconds 60", timeout=180, event="abandon")
    hits = interlock_hits([parse_meter_peaks(t) for t in texts])
    if hits:
        raise StepError(f"band activity: peaks above -50 dBFS on tracks {sorted(hits)}; no switch, alarm the owner")
    state["card"] = "switching"
    save_state(state)
    r = ps(env, f"Invoke-GoldenSaveQuit -Http {ps_quote(env['PC_REAPER_HTTP'])} -Project {ps_quote(env['PC_MAIN_PROJECT'])} -AsioModule {ps_quote(env['PC_ASIO_MODULE'])}", timeout=120)
    state["card"] = "free"
    save_state(state)
    print(json.dumps({"to-dev": r, "app": "kept running"}))


def cmd_set_buffer(env, args) -> None:
    state = open_state()
    if state["card"] != "free":
        raise StepError("the card is not free (run to-dev)")
    if args.frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if spike_running(env):
        raise StepError("a spike runs")
    state["pref_current"], state["pref_restored"] = args.frames, False   # recorded before the write: preempt restores
    save_state(state)
    r = ps(env, f"Set-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])} -Value {args.frames} -Original {state['pref_original']}")
    print(json.dumps({"set-buffer": r}))


def cmd_run(env, args) -> None:
    state = open_state()
    if state["card"] != "free":
        raise StepError("the card is not free (run to-dev)")
    check_request(args.mode, args.frames, args.seconds, args.burn_us, args.stress, args.cycles)
    current = state["pref_current"] if state["pref_current"] is not None else state["pref_original"]
    if args.mode != "probe" and args.frames != current:
        raise StepError(f"the driver's preferred buffer is {current}: run set-buffer --frames {args.frames} first")
    fields = {"mode": args.mode, "driver": env["PC_ASIO_DRIVER"], "module": env["PC_ASIO_MODULE"], "frames": args.frames or 0,
              "seconds": args.seconds, "burn_us": args.burn_us, "stress": args.stress, "panic_at": args.panic_at,
              "cycles": args.cycles, "timeout": run_timeout(args.mode, args.seconds, args.cycles)}
    rid = ps(env, f"Remove-Item -LiteralPath {pc(env, 'queue/stop')} -ErrorAction SilentlyContinue ; "
                  f"$id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'spike' -Fields {ps_hashtable(fields)} ; Start-SpikeTask ; $id")
    state["runs"].append({"request": rid, **fields})
    save_state(state)
    status_path, progress_path = pc(env, f"status/{rid}.json"), pc(env, f"status/{rid}.progress.json")
    watch = (f"$s = {status_path} ; $p = {progress_path} ; [pscustomobject]@{{ "
             "status = $(if (Test-Path -LiteralPath $s) { Get-Content -LiteralPath $s -Raw | ConvertFrom-Json } else { $null }); "
             "progress = $(if (Test-Path -LiteralPath $p) { Get-Content -LiteralPath $p -Raw | ConvertFrom-Json } else { $null }) }")
    deadline = time.monotonic() + fields["timeout"] + 120
    next_pc = 0.0
    while True:
        if event_now():
            raise EventNow()
        if time.monotonic() >= next_pc:
            next_pc = time.monotonic() + 10
            st = ps(env, watch, timeout=60, event="abandon")
            if st.get("progress"):
                print(json.dumps({"progress": st["progress"]}), flush=True)
            phase = (st.get("status") or {}).get("state")
            if phase in ("exited", "failed", "refused"):
                break
            if time.monotonic() > deadline:
                ps(env, f"New-Item -ItemType File -Force -Path {pc(env, 'queue/stop')} | Out-Null ; 'stop'", timeout=60)
                raise StepError("the spike has not exited: stop file written; watch it, never kill; alarm the owner if it stays")
        time.sleep(POLL_S)
    if phase != "exited":
        raise StepError(f"spike request {phase}: {json.dumps(st['status'].get('results'))}")
    out = raw_dir(env, state)
    scp(remote(env, f"status/{rid}.report.json"), str(out / f"{rid}.report.json"))
    scp(remote(env, f"status/{rid}.stderr.txt"), str(out / f"{rid}.stderr.txt"))
    report = json.loads((out / f"{rid}.report.json").read_text(encoding="utf-8"))
    v = verdict(report)
    state["runs"][-1]["verdict"] = v
    save_state(state)
    print(json.dumps({"run": rid, "exit": st["status"]["results"][0]["exit"], "verdict": v}))


def unwind(env: dict[str, str], state: dict, running: bool) -> list:
    """Stop the spike, restore the buffer (read back), bring REAPER back."""
    done = []
    gone = True
    for step in undo_plan(state, running):
        if step == "stop-spike":
            gone = bool(ps(env, f"(Stop-SpikeGracefully -Root {ps_quote(env['PC_ROOT'])} -Seconds 60).gone", timeout=120, event="ignore"))
            done.append({"stop-spike": gone})
        elif step == "restore-buffer":
            r = ps(env, f"Set-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])} "
                        f"-Value {state['pref_original']} -Original {state['pref_original']}", event="ignore")
            state["pref_current"], state["pref_restored"] = state["pref_original"], True
            save_state(state)
            done.append({"restore-buffer": r})
        elif step == "bring-back":
            if not gone:
                raise StepError("the spike did not stop within 60 s, so REAPER cannot start (I3): alarm the owner now; "
                                "the last resort is the owner's reboot, which comes back in event mode")
            r = ps(env, "Invoke-SpikeBringBack " + " ".join([
                f"-Http {ps_quote(env['PC_REAPER_HTTP'])}",
                f"-StartTaskPath {ps_quote(env['PC_REAPER_START_TASK_PATH'])} -StartTask {ps_quote(env['PC_REAPER_START_TASK'])}",
                f"-NTrack {int(env['PC_NTRACK'])} -BridgeState {ps_quote(env['PC_METER_BRIDGE'])}",
                f"-BridgeAction {ps_quote(env['PC_METER_ACTION'])} -Heartbeat {ps_quote(env['PC_METER_HEARTBEAT'])}",
                f"-AsioModule {ps_quote(env['PC_ASIO_MODULE'])} -AppHttp {ps_quote(env['PC_APP_HTTP'])}",
            ]), timeout=240, event="ignore")
            state["card"] = "reaper"
            save_state(state)
            done.append({"bring-back": r})
    state["closed"] = True
    save_state(state)
    return done


def cmd_to_event(env, args) -> None:
    state = open_state()
    if spike_running(env):
        raise StepError("a spike runs: wait for it, or preempt")
    print(json.dumps({"to-event": unwind(env, state, running=False)}))


def cmd_preempt(env, args=None) -> None:
    state = load_state()
    if state.get("closed"):
        print(json.dumps({"preempt": state["id"], "plan": [], "note": "window already closed"}))
        return
    running = spike_running(env, event="ignore")
    print(json.dumps({"preempt": state["id"], "plan": undo_plan(state, running)}), flush=True)
    state["preempted"] = True
    print(json.dumps({"done": unwind(env, state, running)}))


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("new").add_argument("--signal", required=True)
    for name in ("fetch-bundle", "setup"):
        sub.add_parser(name).add_argument("--sha", required=True)
    for name in ("preflight", "to-dev", "to-event", "preempt", "status"):
        sub.add_parser(name)
    sub.add_parser("set-buffer").add_argument("--frames", type=int, required=True)
    run = sub.add_parser("run")
    run.add_argument("--mode", required=True, choices=("probe", "duplex", "reopen"))
    run.add_argument("--frames", type=int)
    run.add_argument("--seconds", type=int, default=600)
    run.add_argument("--burn-us", type=int, default=0)
    run.add_argument("--stress", type=int, default=0)
    run.add_argument("--panic-at", type=int, default=0)
    run.add_argument("--cycles", type=int, default=5)
    args = ap.parse_args(argv)
    if args.cmd == "status":
        print(json.dumps({"event_now": event_now(), "state": load_state() if STATE.is_file() else None}, indent=1))
        return 0
    handlers = {"new": cmd_new, "fetch-bundle": cmd_fetch_bundle, "setup": cmd_setup, "preflight": cmd_preflight,
                "to-dev": cmd_to_dev, "set-buffer": cmd_set_buffer, "run": cmd_run, "to-event": cmd_to_event, "preempt": cmd_preempt}
    try:
        env = load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
        try:
            handlers[args.cmd](env, args)
        except EventNow:
            print(json.dumps({"event": "ide event (flag file)", "action": "preempt"}), flush=True)
            cmd_preempt(env)
            return 10
        return 0
    except StepError as e:
        print(f"spike_window: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
```

- [x] **Step 3: Run the tests.**

```bash
cd "$WORK" && python3 -m unittest discover -s scripts/asio-spike -p 'test_*.py' -v 2>&1 | tail -3
python3 scripts/check_integrity.py
```

Expected: `Ran 28 tests … OK` and `integrity: clean`. The `ResourceWarning`s for the abandoned child processes are expected: the guard never kills.

- [x] **Step 4: Commit.**

```bash
chmod +x scripts/asio-spike/spike_window.py
git add scripts/asio-spike/spike_window.py scripts/asio-spike/test_spike_window.py
git commit -m "feat(asio-spike): dev-box window driver and interim switch

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: CI — job `asio-spike`, spike tests in `integrity`

**Files:**
- Modify: `.github/workflows/ci.yml`

- [x] **Step 1: The integrity job runs the driver tests.** In the step `Script self-tests`, after the first `unittest discover` line, add:

```yaml
          python3 -m unittest discover -s scripts/asio-spike -p 'test_*.py' -v
```

- [x] **Step 2: The job.** Insert after the `windows` job (before `supply-chain:`):

```yaml
  asio-spike:
    name: asio-spike
    runs-on: windows-2025
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Spike PC module self-test (Windows PowerShell 5.1, as on the PC)
        shell: powershell
        run: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/asio-spike/Test-SpikePc.ps1
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - name: Audio I/O clippy and tests on Windows (format, telemetry, the ASIO host builds, the spike's parser)
        run: |
          cargo clippy --locked -p iem-audio-io --all-targets -- -D warnings
          cargo test --locked -p iem-audio-io
      - name: Build the spike (release)
        run: cargo build --locked --release -p iem-audio-io --example asio_spike
      - name: Without an ASIO driver the spike reports no-driver and exits 3
        shell: pwsh
        run: |
          $ErrorActionPreference = 'Stop'
          $report = Join-Path $env:RUNNER_TEMP 'probe.json'
          & target/release/examples/asio_spike.exe probe --driver 'No Such Card' --report $report --stop-file (Join-Path $env:RUNNER_TEMP 'stop')
          if ($LASTEXITCODE -ne 3) { throw "exit $LASTEXITCODE, expected 3" }
          $r = Get-Content -LiteralPath $report -Raw | ConvertFrom-Json
          if ($r.outcome -ne 'no-driver') { throw "outcome $($r.outcome), expected no-driver" }
          $global:LASTEXITCODE = 0
      - name: Bundle (spike, PC scripts, SHA256SUMS)
        shell: pwsh
        run: |
          $ErrorActionPreference = 'Stop'
          $b = Join-Path $env:RUNNER_TEMP 'asio-spike'
          New-Item -ItemType Directory -Force -Path $b | Out-Null
          Copy-Item -LiteralPath target/release/examples/asio_spike.exe, scripts/asio-spike/SpikePc.psm1, scripts/asio-spike/spike-task.ps1, scripts/golden/GoldenPc.psm1 -Destination $b
          $lines = Get-ChildItem -LiteralPath $b -File | Sort-Object Name | ForEach-Object { '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name }
          [IO.File]::WriteAllText((Join-Path $b 'SHA256SUMS'), (($lines -join "`n") + "`n"))
      - name: Upload (dev pushes only — the only spike that may reach the PC, P5)
        if: github.event_name == 'push' && github.ref == 'refs/heads/dev'
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: asio-spike-${{ github.sha }}
          path: ${{ runner.temp }}/asio-spike
          retention-days: 14
          if-no-files-found: error
```

The `windows` job keeps its engine clippy and tests, which include `iem-audio-io --lib`. The new job adds the example, the release build, the PC self-test and the bundle. It does not wait for `wasm`.

- [x] **Step 3: Check and commit.**

```bash
cd "$WORK" && python3 scripts/check_integrity.py && python3 -m unittest scripts/test_check_integrity.py
git add .github/workflows/ci.yml
git commit -m "ci: asio-spike job (Windows build, PC self-test, bundle); spike tests in integrity

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Private runbook, event runbook, playbook rule, router

**Files:**
- Private: `$PRIV/asio-spike.env` (chmod 600), `$OPS/docs/s1a-pc-runbook.md`, `$PRIV/event-runbook.md`, `$OPS/CLAUDE.md`
- Public: `.claude/rules/asio-spike.md`, `CLAUDE.md`

- [x] **Step 1: The private env.** Write `$PRIV/asio-spike.env` (never committed anywhere public), with values from the private sources in the plan header:

| Key | Value source |
|---|---|
| `PC_SSH` | as in `golden.env` |
| `PC_ROOT` / `PC_ROOT_SCP` | a new folder `asio-spike` next to the golden root, in both notations as in `golden.env` |
| `PC_ASIO_MODULE` | as in `golden.env` |
| `PC_ASIO_DRIVER` | the driver's registry description (REAPER's `asio_driver_name`, S1b runbook; `probe` confirms it: a wrong name exits 3 and lists the present ones) |
| `PC_BUFFER_KEY` | the driver-preference key of the site appendix's `PrefBuffSize` line, in PowerShell notation (`HKCU:\…`) |
| `PC_BUFFER_NAME` | the value name on that line |
| `PC_BUFFER_ORIGINAL` | the value REAPER runs with (64 on 2026-09-27; preflight refuses any other reading) |
| `PC_REAPER_HTTP`, `PC_MAIN_PROJECT` | as in `golden.env` |
| `PC_REAPER_START_TASK_PATH`, `PC_REAPER_START_TASK` | our own no-time-limit start task from the event runbook (never the predecessor's 72 h task) |
| `PC_NTRACK` | the project's track count (event runbook) |
| `PC_METER_BRIDGE`, `PC_METER_HEARTBEAT` | `<section>/<key>` of the meter bridge's running flag and heartbeat (`meter_bridge.lua` at the pinned SHA) |
| `PC_METER_ACTION` | the bridge's named action (event runbook) |
| `PC_APP_PROCESS`, `PC_APP_HTTP` | as in `golden.env` |
| `RAW_DIR` | `$RAW` |

```bash
chmod 600 "$PRIV/asio-spike.env" && python3 -c "
import sys; sys.path.insert(0, '$WORK/scripts/asio-spike'); import spike_window as s, pathlib
print(sorted(s.load_env(pathlib.Path('$PRIV/asio-spike.env'))))"
```

- [x] **Step 2: The ops runbook.** Write `$OPS/docs/s1a-pc-runbook.md` (PRIVATE) with:
  - the rules of the Global Constraints;
  - the env keys with their real values and where each was read;
  - the window command sequence of Task 12 with the real task path, key and module;
  - "ide event" handling (flag, `preempt`, checks);
  - the alarm texts: spike does not stop; buffer read-back differs; handover check fails.

  Commit it in the ops repo (`docs(s1a): PC runbook for the ASIO spike`) and push the ops repo's default branch per its own CLAUDE.md.

- [x] **Step 3: The event runbook.**
  - In `$PRIV/event-runbook.md`, "ide event" section, first step: `date -Iseconds > ~/.config/iemmixer/EVENT-NOW`. Then, if `$S status` shows a window that is not closed, run `$S preempt`; then the existing checks.
  - "event skončil": `rm -f ~/.config/iemmixer/EVENT-NOW`; development continues. A spike window starts only when Task 12 runs.
  - Mirror the same text into `$OPS/CLAUDE.md` (the S0 Task 13 copy) and commit it in the ops repo.

- [x] **Step 4: Playbook rule.** Create `.claude/rules/asio-spike.md`:

```markdown
---
paths:
  - "crates/iem-audio-io/src/asio.rs"
  - "crates/iem-audio-io/src/format.rs"
  - "crates/iem-audio-io/src/telemetry.rs"
  - "crates/iem-audio-io/examples/asio_spike.rs"
  - "scripts/asio-spike/**"
---

# ASIO host and the S1a spike (#3)

- `asio.rs` is the crate's only unsafe code (`deny(unsafe_code)` at the root, `allow` on the module). Every driver call stays on the thread that created `Host` (`!Send`, COM STA; that thread pumps messages). A stream is freed only after `STREAM` is cleared and `IN_FLIGHT` is 0.
- The host never calls `set_sample_rate`, `set_clock_source` or `open_control_panel` (integrity scan, I2) and streams only at 96 kHz and the driver's preferred buffer (`format::admit`). Outputs are zeroed before `start()` and on every callback (A1).
- Decisions live in `format.rs` and `telemetry.rs` (portable, tested, mutated); `asio.rs` and the example are excluded from mutation and build only on the Windows jobs (`windows`, `asio-spike`). Avoid `>`/`>=` on values where both branches agree (equivalent mutants).
- The driver's preferred buffer changes only through `spike_window.py set-buffer` and is restored with read-back before REAPER starts; REAPER keeps its own value (owner, #3).
- PC windows only in dev time. `~/.config/iemmixer/EVENT-NOW` = an event is on (created on "ide event", removed on "event skončil"); every wait of `spike_window.py` sees it within 2 s and pre-empts. Nothing is killed; a spike that does not stop is an owner alarm.
- Only the `asio-spike-<sha>` artifact of a green `dev` push reaches the PC (`fetch-bundle`; `SHA256SUMS` checked on the dev box and on the PC).
- `SpikePc.psm1` imports `GoldenPc.psm1` from its own folder (the bundle layout); run `Test-SpikePc.ps1` on Windows PowerShell 5.1 (CI `asio-spike`) after any PowerShell change.
- Site values (driver name, registry key, task names, handover values) only in `~/.config/iemmixer/asio-spike.env` and the ops runbook `docs/s1a-pc-runbook.md`.
```

- [x] **Step 5: Router and interim switch.** In `CLAUDE.md`:
  - add to "Playbook router": `- ASIO host, the S1a spike, the PC window driver → .claude/rules/asio-spike.md`;
  - replace the paragraph starting `**Until S1a's interim switch script (S6: `iemmode`)**` with:

```markdown
**Until S6's `iemmode`** the interim switch is `scripts/asio-spike/spike_window.py` with the private `~/.config/iemmixer/asio-spike.env`: "ide event" → create `~/.config/iemmixer/EVENT-NOW`, then `preempt` if a window is open, then the event runbook's checks (`~/.config/iemmixer/event-runbook.md`); "event skončil" → remove the flag; `to-dev` / `to-event` switch only inside a window of a running task.
```

- [x] **Step 6: Scan and commit.**

```bash
cd "$WORK" && git add .claude/rules/asio-spike.md CLAUDE.md
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$(git write-tree)"
git commit -m "docs(s1a): playbook rule and the interim switch in CLAUDE.md

Refs #3

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

Expected: the scan prints no finding.

---

### Task 11: First push, CI green, the bundle (main session)

- [x] **Step 1: Pre-push checks, then push.**

```bash
cd "$WORK" && git fetch origin && git merge --ff-only origin/dev && git status -sb
cargo fmt --all -- --check && python3 scripts/check_integrity.py && python3 scripts/check_engine_deps.py && python3 scripts/check_version.py
python3 -m unittest discover -s scripts -p 'test_*.py' 2>&1 | tail -1 && python3 -m unittest discover -s scripts/asio-spike -p 'test_*.py' 2>&1 | tail -1
git push origin dev
```

The pre-push hook runs gitleaks and the denylist.

- [x] **Step 2: Wait for every job** in one foreground bounded loop per Bash call (≤ 9 min). Repeat the call until the run is terminal, never with `run_in_background`:

```bash
RUN=$(gh run list -R "$REPO" --branch dev --event push --limit 1 --json databaseId --jq '.[0].databaseId'); echo "$RUN"
for i in $(seq 1 53); do s=$(gh run view "$RUN" -R "$REPO" --json status,conclusion --jq '.status+" "+(.conclusion // "")'); echo "$(date +%T) $s"; case "$s" in completed*) break;; esac; sleep 10; done
gh run view "$RUN" -R "$REPO" --json jobs --jq '.jobs[] | .name+": "+(.conclusion // .status)'
```

Every job must be `success`, including `asio-spike`, `windows`, `supply-chain`, `mutants-list` and `integrity`. On failure:
- read `gh run view "$RUN" -R "$REPO" --log-failed`;
- fix every finding in ONE commit (`fix(s1a): first CI cycle — …`), push, wait again.

Likely first-cycle items:
- a clippy lint on Windows (read the lint name);
- a `windows-sys` import path;
- rustfmt;
- `mutants-list` over the shard budget, which means resizing the `shard:` matrix. This is `ci-rust-toolchain.md`; never raise a timeout.

- [x] **Step 3: Coverage and mutation outlook.**
  - Read the `test` job's `line coverage` line. It must stay ≥ the floor.
  - Record the `mutants-list` count on #3.
  - Survivors appear only in the PR's mutation gate (Task 14). Kill each with a test in `format.rs`/`telemetry.rs`, or exclude a proven equivalent mutant with a reason.

- [x] **Step 4: Fetch the bundle of the green head** (dev box only; the PC is not touched):

```bash
SHA=$(git rev-parse HEAD); $S fetch-bundle --sha "$SHA"
```

Expected: `{"bundle": ".../asio-spike/bundles/<sha>", "run": <id>, "files": ["GoldenPc.psm1", "SpikePc.psm1", "asio_spike.exe", "spike-task.ps1"]}`.

`fetch-bundle` needs `$SPIKE_ENV` (Task 10). Post the SHA and run id on #3.

---

### Task 12: The PC window (dev time only; main session)

> **Status 2026-09-27:** not started — PC window, after the event ("event skončil"). Tasks 1–11 are done: CI run 36297231771 on `c5a40c9` green, bundle fetched and verified on the dev box.

**Precondition:** the owner's "event skončil" is in this conversation after the last "ide event", and `~/.config/iemmixer/EVENT-NOW` does not exist. Quote the owner's message with its time in `new --signal`. Every step's JSON output goes to #3 (numbers only, no site values). Runtime: about 90 min.

- [ ] **Step 1: Open, set up, preflight** (the PC is still in event state, REAPER running).

```bash
$S new --signal "<owner, HH:MM: event skončil …>"
$S setup --sha "$SHA"
$S preflight
```

Expected:
- `setup` verifies four files and registers the task;
- `preflight` shows `pref` = the recorded original, holders only `reaper.exe`, `files` = 4.

Any problem stops the window. `preflight` changes nothing, so `$S to-event` just closes it.

- [ ] **Step 2: Switch to dev.**

```bash
$S to-dev
```

Expected:
- 60 s quiet interlock, then REAPER saved (project file changed) and quit, and the module released;
- the predecessor app keeps running.

On band activity nothing changes: tell the owner.

- [ ] **Step 3: Ops issue 1 (S0 hand-off).**
  - Over ssh (read-only), read the PC-only credential named in the private appendix §5 H1 (old and rotated values).
  - Append it to `$PRIV/denylist-credentials.txt`, rebuild the concatenated `$PRIV/denylist.txt` as the S0 runbook says, and update the `DENYLIST` secret (`gh secret set DENYLIST -R "$REPO" < "$PRIV/denylist.txt"`).
  - Close ops issue 1 with a comment that names no value.

- [ ] **Step 4: At the current buffer.**

```bash
$S run --mode probe
$S run --mode duplex --frames 64 --seconds 600
$S run --mode reopen --frames 64 --cycles 5
$S run --mode duplex --frames 64 --seconds 60 --panic-at 50000
```

Expected:
- `probe` exits 0 and lists channels, sample types (one supported type), buffer caps, latencies and clocks;
- `duplex` gives a verdict with numbers;
- `reopen` gives 5 cycles with phase times;
- the fault run exits 6, with `callbacks_after_fault` > 0.

A `probe` exit 4 means `refused`: read the reason. Rate ≠ 96 kHz is an owner question, never a change.

- [ ] **Step 5: 32 samples (the target).**

```bash
$S set-buffer --frames 32
$S run --mode probe
$S run --mode duplex --frames 32 --seconds 600
$S run --mode duplex --frames 32 --seconds 600 --burn-us 100 --stress 4
```

If `probe` shows `buffer.preferred` still 64 (the driver reads its preference only at load), record it as a finding. Stop that size; do not work around it.

- [ ] **Step 6: 48 samples** — only if `probe` showed 48 inside min/max/granularity:

```bash
$S set-buffer --frames 48 && $S run --mode duplex --frames 48 --seconds 600
```

- [ ] **Step 7: Back to event state.**

```bash
$S to-event
```

Expected:
- `restore-buffer` reads back the original;
- `bring-back` reports the tracks equal to the project's, `bridge_triggered` true or false, heartbeat advancing, `asio: reaper` and the app answering.

Any failure is an owner alarm (❓). REAPER and the app are the band's system.

- [ ] **Step 8: Graceful exit of the predecessor app** (#3 acceptance, open). Try it only when the PC's remote-desktop MCP is configured in this session: the tray Exit, verify gone, start it from its executable (S1b §8), verify it answers. Otherwise record on #3 that it stays open and hand it to S6 (#9).

- [ ] **Step 9: Raw data stays private.** `ls -l "$RAW/asio-spike/<window id>"` shows the reports (chmod 700). Nothing from them is committed.

---

### Task 13: Report, decision, hand-offs (main session)

- [ ] **Step 1: The report on #3 (Slovak, plain, numbers).** Include:
  - a table per buffer (32/48/64): p50/p99/p99.9/max interval, late, missed, overruns, position gaps, callback CPU p50/p99.9 idle and under load, reported latencies (samples and ms), drift ppm;
  - reopen phase times (median/max);
  - fault run outcome;
  - clock role (current source, read only);
  - the verdict "azo accepted" or the fallback (design note §4);
  - the lowest stable buffer, and that 32 stays the target (#15);
  - the skipped owner-approval tests;
  - the switch verified there and back.

  Mark the two acceptance boxes only on evidence.

- [ ] **Step 2: Results in the design note.** Add a `## 9. Results` section: numbers, the decision, the findings (e.g. the driver's preference-at-load behaviour, 48 accepted or not). Commit: `docs(s1a): results`.

- [ ] **Step 3: Hand-offs.**
  - **#15 (S1c):** the idle and loaded baseline at 32, and what degraded it.
  - **#9 (S6):**
    - the backend API (`Host`, `Running`, the slot and in-flight rule, the reset path);
    - the preferred-buffer switch (write, read back, restore before REAPER);
    - the handover checks;
    - that the spike's exit codes are not the engine's;
    - the open graceful-exit item.
  - **Owner question** (one `❓`, Slovak): the next approval-gated test to schedule. Nothing runs without it.

---

### Task 14: PR and merge (after Task 13; only when the run's orchestrator asks for it)

- [ ] **Step 1:** Update the required checks with `asio-spike` (S0 plan Task 15 Step 6). A skipped required job counts as passing, and `asio-spike` needs nothing.
- [ ] **Step 2:** Open the `dev` → `main` PR. Its body covers the summary, the verdict, the checks and #3. Wait for every check including the mutation shards, kill survivors, merge with `gh pr merge --merge` per `pr-merge-policy`, then bump `dev` to `2.0.0-dev.9` first thing.

## Hand-off to later sub-projects

- **S1c (#15):** the 32-sample baseline, idle and under load, and the `--burn-us`/`--stress` tool for the before/after measurements.
- **S6 (#9):**
  - `asio.rs` becomes the backend behind `iem_audio_io::Process`: channel-major f64 via `format`, `catch_unwind` already at the boundary;
  - the guard reuses the buffer switch, the handover checks and the EVENT-NOW discipline;
  - the Windows pipes and the DACL stay S6 items.
