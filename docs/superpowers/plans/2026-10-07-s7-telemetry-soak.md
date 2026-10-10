# iemmixer S7, parts 1–2: Telemetry, Switch Record and Soak (Implementation Plan)

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to run this plan task by task. Steps use checkbox (`- [ ]`) syntax for tracking.
> - Tasks 10–12 run in the main session, never in a subagent: the ops repo, pushes, CI waits, the PC.
> - A subagent never touches the PC.
> - Lanes run one at a time (design §10). Each task below is one lane unless it says otherwise.

**Goal:** parts 1 and 2 of design §10, plus the guard-side half of §5:
- The engine exports RT-safe histograms of the callback interval and of the callback's own time. The guard passes them through `Reply.engine`.
- Every switch leaves a timed record (`last_switch`) with its in-ear silence window.
- A soak harness (`iem-soakclient`), its CI run, `iempc dispatch-soak`, the ops `soak.yml` and a pure verdict let the PC prove 8 h at 32 samples. After that comes the first 8 h soak.

**Architecture:**
- **`iem-audio-io`:**
  - new `hist.rs`: `PeriodHist`, `StreamHists`, `HistSnapshot`, `quantile_us`;
  - `Telemetry::on_callback` returns the interval it judged;
  - NullRt and `AsioStream` each own an `Arc<StreamHists>` and record twice per callback (two relaxed increments).
- **`iem-engine-proto` / `iem-engine`:**
  - `Status` gains `interval_hist`, `process_hist` and `hist_top_us` (additive);
  - `control::Driver` gains `histograms()`.
- **`iem-guard`:**
  - `pc::Status` and `EngineStatus` carry `late`, `overruns`, `process_max_us`, `hist_top_us`, both histograms and the engine `pid`;
  - the guard pipe's `MAX_FRAME` becomes 256 KiB;
  - new pure `switch_log.rs` (`StepTime`, `LastSwitch`, `SwitchOutcome`, `silence_ms`, `Laps`);
  - `GuardState.last_switch`, `Reply.last_switch`.
- **`iem-soakclient`** (new workspace crate, sync, no async runtime):
  - ureq for login and `/api/site`, tungstenite for `/ws/<member>` and `/ws/audio`, opus to decode;
  - a pure core plus thin socket threads;
  - a JSON summary rewritten every minute.
- **`scripts/iem-pc/`:**
  - `soak_verdict.py` (pure, stdlib; the ops report job runs it from this repo at the soaked SHA);
  - `iempc_soak.py` (`dispatch-soak`; iempc passes itself in, so the module never imports iempc).
- **CI:** the bundle carries `iem-soakclient.exe`; the `e2e` job runs the harness against NullRt and the server; Windows clippy; coverage; mutation scope.
- **Ops repo (private):** `soak.yml` (`verify` → `soak` on `iem-pc`, 600 min → `report` posting `soak/iem-pc`).

**Tech stack:**
- Rust 1.98.1 (edition 2024).
- Crates, all already locked:
  - `tungstenite` 0.28.0 (`default-features = false, features = ["handshake"]`; its handshake deps are already in the lock);
  - `ureq` 3.4.2 (`default-features = false`, as the guard uses it);
  - `opus` 0.4.0 (with `opusic-sys`, already built by `iem-server/audio`);
  - `serde` and `serde_json`.
- Python 3.12 stdlib; Windows PowerShell 5.1 (ops job only); GitHub Actions.

**Spec:**
- `docs/superpowers/specs/2026-10-07-s7-hil-live-soak-design.md` §3, §4, §5 (guard side), §9, §10 items 1–2.
- The S1a baseline p99.9 at B = 32 is 347 µs (`docs/superpowers/specs/2026-09-27-s1a-asio-spike-design.md`, results table, column B = 32).

**Detail sources (private, never committed):**
- `~/devel/iemmixer-ops/.github/workflows/hil.yml` (the model for `soak.yml`);
- `~/devel/iemmixer-ops/docs/s6-pc-runbook.md`;
- `~/.config/iemmixer/iem-pc.env`;
- the ops `site/site.toml` (`lan_url`, the server's port, the S1c profile's housekeeping CPUs).

## Global Constraints

- **Version:** the workspace is already `2.0.0-dev.17` on `dev` (above `main`). Task 1 checks it with `python3 scripts/check_version.py` and does **not** bump again.
- **Tier 0:**
  - Nothing compiles locally. Allowed locally: `cargo fmt --all --check`, `cargo metadata`, `cargo tree`, `cargo update -p`, `python3 -m unittest`, `git`.
  - Every Rust, PowerShell and workflow change is proven in hosted CI: one push per cycle, one fix commit per failing cycle, bounded foreground waits (at most 9 min per Bash call).
- **RED → GREEN per behaviour:**
  - Every behaviour lands as a `test(<scope>): [red] … (#10)` commit, then `feat|fix(<scope>): [green] … (#10)`.
  - A RED test may fail by not compiling (new fields or functions), as in S6.
  - No `#[ignore]`, skips, `.only` or `continue-on-error`. The coverage floor never drops.
  - The diff-scoped mutation gate runs on the PR; resize the shard matrix from `mutants-list` before the PR.
- **Commits:**
  - `dev` only, noreply identity.
  - Subjects carry `(#10)`. Every message ends with `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`.
  - The PR body ends with the session's Claude Code attribution line.
- **RT contract (I7):**
  - The only new per-callback work is two `fetch_add(1, Relaxed)` calls into arrays allocated before the stream starts.
  - `PeriodHist::record` carries `#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]`.
  - The work is proven by `crates/iem-audio-io/tests/rt.rs` and by `crates/iem-engine/tests/{rt,rtsan}.rs` through `tests/common/mod.rs::drive`.
- **Additive protocols:**
  - `iem-engine-proto`: no `deny_unknown_fields`; struct-level `#[serde(default)]`.
  - Guard `Reply`, `EngineStatus` and `GuardState` stay readable by and from older peers. A record a guard cannot read becomes `None`, never an unreadable state or reply (`switch_log::lenient`).
- **Integrity (I8):**
  - No kill or terminate method names, no `Stop-Process`, no restart or shutdown command, anywhere in `crates/`, `scripts/`, `.github/`, `e2e/`, comments included. Write "force-end" in prose.
  - The soak client closes sockets by dropping them. Its PC job waits with bounds and never ends a process.
- **P6:**
  - No site value in any file, test or example. Use the placeholders `member1`…`member9`, `engineer`, `mixer.example.org`, `http://10.0.0.10`, port 8080, synthetic CPU Set ids 256/257 and synthetic SHAs.
  - The soak summary and the posted `soak/iem-pc` text hold numbers and fixed reason codes only: never a member id, host, URL or PIN.
- **P5:** only the installed, attested bundle's `iem-soakclient.exe` runs on the PC, from `bundles\<sha>\`.
- **P10:** the client is light (two socket threads, ~50 Opus decodes a second) and confined to the housekeeping CPU Set when one is given. No browser and no Node on the PC.
- **Dev time:**
  - The PC steps (Task 12) run only when the owner's latest signal is "event skončil" and `~/.config/iemmixer/EVENT-NOW` does not exist.
  - "Ide event" → `python3 scripts/iem-pc/iempc.py event`. A soak cut short is cancelled, never red, and restarts from zero in the next dev window.
- **Size budgets (#36):** `iempc.py`, `test_iempc.py` and `iem-guard/src/daemon.rs` (+ `daemon/tests.rs`, 4451 lines) are over budget. New logic goes into new modules (`iempc_soak.py`, `switch_log.rs`) and new test files (`test_iempc_soak.py`, `daemon/record_tests.rs`); the old files get call sites only.

## Review Focus

1. **I7 on the callback.**
   - Expected: `on_buffer` and NullRt's `pace` only call `PeriodHist::record` (an index computation plus one relaxed increment). The `Arc` is cloned on the owner thread at open.
   - Tests: `tests/rt.rs::recording_the_stream_histograms_does_not_allocate`, engine `rt.rs::process_does_not_allocate` and `rtsan.rs::process_is_realtime_safe` with the histogram totals asserted.
2. **Bucketing.**
   - Expected: 1 µs buckets `[b, b+1)` below two periods; the overflow is exactly the intervals telemetry counts as missed (≥ 2 periods); the range is capped at 1 ms.
   - Tests: `hist::tests::the_overflow_bucket_holds_exactly_what_telemetry_counts_missed`, `two_periods_at_32_samples_end_at_bucket_667`.
3. **Additive fields both ways.**
   - Tests: `msg::tests::the_histograms_in_status_are_additive_and_sparse`, `proto::tests::the_engine_carries_the_fields_hil_v1_reads`, `state::tests::a_last_switch_this_guard_cannot_read_is_dropped_not_the_state`.
4. **The guard reply always fits a frame.**
   - Test: `proto::tests::the_largest_reply_fits_a_frame` (50 alarms of 600 two-byte characters, an 8000-character detail, both histograms full, a full switch record).
5. **Silence window.**
   - Expected: it starts at the first of `EngineStop` or `ReaperSaveQuit` and ends at `ReaperHandover` (to event) or `EngineArm` (to dev or live), inclusive; there is no window when either end is missing.
   - Tests: `switch_log::tests::*`.
6. **Verdict order and conservatism.**
   - Expected: late counts buckets ≥ 347; p99.9 is the bucket's upper edge at rank ⌈0.999·n⌉ in integer math; counting is over the soak window (last − first); red names the first failure.
   - Tests: `test_soak_verdict.py`.
7. **Dispatch guards.**
   - Expected: EVENT-NOW (at start, and again right before the dispatch); guard in dev, not switching, no HIL job; active bundle = SHA = running engine build; green push run; once per SHA per dev entry.
   - Tests: `test_iempc_soak.py`.
8. **"Ide event" never makes red.**
   - Expected: a cancelled PC job, a `left-dev` record or a missing record maps to `cancelled`.
   - Test: `test_report_maps_a_cancelled_or_unrecorded_pc_job_to_cancelled`.

## Shell variables (paste at the start of every task)

```bash
export WORK="$HOME/devel/iemmixer"
export PRIV="$HOME/.config/iemmixer"
export OPS="$HOME/devel/iemmixer-ops"
export STATE="$HOME/.local/share/iemmixer/iem-pc"
export REPO=zbynekdrlik/iemmixer
export OPS_REPO=zbynekdrlik/iemmixer-ops
export P="python3 $WORK/scripts/iem-pc/iempc.py"
```

## File Structure

```
crates/iem-audio-io/src/hist.rs                 NEW PeriodHist, StreamHists, HistSnapshot, quantile_us (portable, mutated)
crates/iem-audio-io/src/lib.rs                  pub mod hist; #![cfg_attr(iem_rtsan, feature(sanitize))]
crates/iem-audio-io/Cargo.toml                  [lints.rust] unexpected_cfgs check-cfg cfg(iem_rtsan)
crates/iem-audio-io/src/telemetry.rs            on_callback -> Option<u64> (the judged interval)
crates/iem-audio-io/src/telemetry_tests.rs      the_judged_interval_is_returned_after_the_warm_up
crates/iem-audio-io/src/nullrt.rs               Arc<StreamHists>, records both, histograms()
crates/iem-audio-io/src/asio.rs                 Start/Owner/Backend carry Arc<StreamHists>; AsioStream::histograms() (Windows)
crates/iem-audio-io/tests/rt.rs                 recording_the_stream_histograms_does_not_allocate
crates/iem-engine/tests/common/mod.rs           Scenario.hists; drive records as the backends do
crates/iem-engine/tests/{rt,rtsan}.rs           histogram totals asserted
crates/iem-engine/examples/bench.rs             p99.9 from the histogram printed beside the sorted one
crates/iem-engine-proto/src/msg.rs              Status.{interval_hist, process_hist, hist_top_us}
crates/iem-engine/src/control.rs                Driver::histograms; status_msg fills them
crates/iem-engine/src/{engine,asio}.rs          NullRtDriver / AsioDriver::histograms
crates/iem-guard/src/pc.rs                      pc::Status += late, overruns, process_max_us, hist_top_us, interval_hist, process_hist
crates/iem-guard/src/effects/engine.rs          parse (+ hist()), engine_status(.., pid)
crates/iem-guard/src/proto.rs                   EngineStatus += pid and the soak figures; Reply.last_switch; MAX_FRAME 256 KiB
crates/iem-guard/src/switch_log.rs              NEW StepTime, SwitchOutcome, LastSwitch, silence_ms, Laps, lenient (pure, mutated)
crates/iem-guard/src/state.rs                   GuardState.last_switch
crates/iem-guard/src/daemon.rs                  Laps in Guard; lap per step; record in finish; View/Reply carry it; engine pid
crates/iem-guard/src/daemon/record_tests.rs     NEW daemon tests (engine pid, switch record)
crates/iem-guard/src/{view,cli}.rs              test Reply literals += last_switch: None
crates/iem-guard/src/bundle.rs                  test only: a_bundle_may_carry_the_soak_client
crates/iem-soakclient/{Cargo.toml,src/lib.rs,src/net.rs,src/main.rs,tests/fake_server.rs}   NEW crate
Cargo.toml                                      members += "crates/iem-soakclient"; Cargo.lock (new package entry only)
scripts/iem-pc/soak_verdict.py (+test_soak_verdict.py)   NEW pure verdict, report mapping, CI harness check
scripts/iem-pc/iempc_soak.py (+test_iempc_soak.py)       NEW dispatch-soak
scripts/iem-pc/iempc.py                         import iempc_soak; cmd_dispatch_soak wrapper; COMMANDS; parser (≈ 12 lines)
.github/workflows/ci.yml                        test coverage, windows clippy, bundle exe, e2e soak step (timeout 45)
.cargo/mutants.toml, .config/nextest.toml       soakclient main.rs excluded; the bounded test first
.claude/rules/{engine,guard,e2e}.md, .claude/rules/soak.md (NEW), CLAUDE.md (router line)
docs/superpowers/plans/2026-10-07-s7-telemetry-soak.md   this plan
private: $OPS/.github/workflows/soak.yml, ops repo variables/secrets, $OPS/docs/s6-pc-runbook.md (soak section)
```

---

### Task 1: Start: sync, version check, plan on the ticket, library facts

**Files:** `docs/superpowers/plans/2026-10-07-s7-telemetry-soak.md`.

- [ ] **Step 1: Sync and check the version (no bump).**

```bash
cd "$WORK" && git fetch origin && git checkout dev && git merge --ff-only origin/dev && git status -sb
python3 scripts/check_version.py && sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1   # 2.0.0-dev.17, above main: no bump
```

- [ ] **Step 2: Commit this plan** (`docs(s7): implementation plan, telemetry, switch record and soak (#10)`). Run the denylist scan over it first:

```bash
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --identities scripts/allowed-identities.txt --tree HEAD
```

- [ ] **Step 3: Plan summary on #10** (Slovak, plain). Cover:
  - what the histograms prove;
  - the switch record;
  - what the soak client does on the PC: it reads only, listens to one member's mix, logs in once;
  - the known effect: while the soak runs, the soak's page socket keeps that member's solo past the 10 s grace (`solo.rs`), and the engineer's own Listen to another member's mix answers `no_source`;
  - that "ide event" cancels a soak.

  Post it with `gh issue comment 10 -R "$REPO" --body-file …`.
- [ ] **Step 4: Library facts** (read the locked sources, record on #10):

```bash
R=$(ls -d ~/.cargo/registry/src/*/ | head -1)
sed -n '/\[features\]/,/^\[lib/p' $R/tungstenite-0.28.0/Cargo.toml   # default = ["handshake"]; handshake = data-encoding, http, httparse, sha1
grep -n 'pub fn connect<\|pub fn accept_hdr<' $R/tungstenite-0.28.0/src/*.rs
grep -n 'pub fn decode_float' $R/opus-0.4.0/src/lib.rs
grep -n 'timeout_global\|http_status_as_error' $R/ureq-3.4.2/src/config.rs | head
```

  Record two things:
  - the client API in use: `tungstenite::connect(&str)`, `tungstenite::accept_hdr` (tests), `opus::Decoder::decode_float`, `ureq::Agent::config_builder()` as `crates/iem-guard/src/win/mod.rs` uses it;
  - UNVERIFIED until Task 5 Step 1: whether adding `tungstenite` and `ureq` as direct dependencies of a new crate changes `Cargo.lock` beyond the new package entry.

---

### Task 2: `iem-audio-io`: the stream histograms (RT) [lane A, ~500 LoC]

**Files:**
- Create: `crates/iem-audio-io/src/hist.rs`.
- Modify: `crates/iem-audio-io/src/{lib.rs,telemetry.rs,telemetry_tests.rs,nullrt.rs,asio.rs}`, `crates/iem-audio-io/Cargo.toml`, `crates/iem-audio-io/tests/rt.rs`, `crates/iem-engine/tests/{common/mod.rs,rt.rs,rtsan.rs}`, `crates/iem-engine/examples/bench.rs`, `.claude/rules/engine.md`.

- [ ] **Step 1 (RED): tests first.**
  - `hist.rs` `#[cfg(test)] mod tests`:
    - `two_periods_at_32_samples_end_at_bucket_667`: with `PeriodHist::new(period_ns(32, 96_000.0))`, `top() == 667`. Indexes: `index(0) == 0`, `index(999) == 0`, `index(1_000) == 1`, `index(346_999) == 346`, `index(347_000) == 347`, `index(666_665) == 666`, `index(666_666) == 667`, `index(u64::MAX) == 667`.
    - `the_overflow_bucket_holds_exactly_what_telemetry_counts_missed`: for every `dt` in `[499_999, 500_000, 666_665, 666_666, 666_667, 1_000_000, u64::MAX]`, `(h.index(dt) == 667) == (telemetry::classify(dt, 333_333) == Gap::Missed)`.
    - `a_larger_period_is_capped_at_one_millisecond`: period 1 000 000 gives `top() == 1000`, `index(999_999) == 999`, `index(1_000_000) == 1000`.
    - `a_zero_period_has_one_bucket_and_the_overflow`: period 0 gives top 1, `index(1) == 0`, `index(2) == 1`.
    - `record_and_sparse_keep_ascending_non_empty_buckets`: recording 0, 0, 346 999, 347 000 and `u64::MAX` gives `[(0, 2), (346, 1), (347, 1), (667, 1)]`, and `StreamHists::snapshot().top_us == 667`.
    - `the_quantile_is_the_upper_edge_at_rank_ceil_per_mille_n`:
      - `[(10, 998), (82, 1), (83, 1)]` at 999 gives `Some(83)`;
      - `[(10, 997), (83, 3)]` gives `Some(84)`;
      - `[(5, 1)]` at 1 gives `Some(6)`;
      - `[(5, 1), (9, 1)]` at 1000 gives `Some(10)`;
      - `[]` gives `None`.
  - `telemetry_tests.rs`: `the_judged_interval_is_returned_after_the_warm_up`. The first callback and the warm-up return `None`; callback 9 returns `Some(entry_9 - entry_8)`; a missed interval returns its value too.
  - `nullrt.rs` tests:
    - `nullrt_records_every_interval_and_every_callback_time`: wait until `callbacks >= 200`, read `stats()` then `histograms()`. Expect `top_us == 667`, `process total >= stats.callbacks`, `interval total + 1 >= stats.callbacks`.
    - `a_slow_callback_lands_in_both_overflow_buckets`: the existing `Slow` processor (20 ms on call 3) puts at least 1 count in bucket 667 of both histograms.
  - `crates/iem-audio-io/tests/rt.rs`:

```rust
/// I7 (S7 design note §3): the backends record every callback's interval and
/// time into the stream histograms; neither allocates nor frees, overflow
/// included. Built outside the detector: the arrays are the stream's.
#[test]
fn recording_the_stream_histograms_does_not_allocate() {
    let (violations, snap) = std::thread::spawn(|| {
        let h = StreamHists::new(period_ns(32, 96_000.0));
        reset_violation_count();
        assert_no_alloc(|| {
            for i in 0..100_000_u64 {
                h.interval.record(333_000 + (i % 400) * 1_000); // spans the overflow bucket
                h.process.record(i % 90_000);
            }
            h.interval.record(u64::MAX);
        });
        (violation_count(), h.snapshot())
    })
    .join()
    .unwrap();
    assert_eq!(violations, 0, "recording a histogram allocated");
    assert_eq!(snap.interval.iter().map(|e| e.1).sum::<u64>(), 100_001);
}
```

  - `crates/iem-engine/tests/rt.rs::process_does_not_allocate` and `rtsan.rs::process_is_realtime_safe` gain:

```rust
let h = s.hists.snapshot();
let total = |v: &[(u32, u64)]| v.iter().map(|e| e.1).sum::<u64>();
assert_eq!((total(&h.interval), total(&h.process)), (BLOCKS, BLOCKS), "the histograms recorded every block");
assert!(h.interval.iter().any(|&(b, _)| b == h.top_us), "the overflow path ran");
```

  (`BLOCKS` = the test's block count: 6 000 in `rt.rs`, 3 000 in `rtsan.rs`.)

  Commit: `test(audio-io): [red] stream histograms, 1 µs to two periods, RT-safe; the judged interval (#10)`.
- [ ] **Step 2 (GREEN): `hist.rs`.**

```rust
//! The stream's distributions (S7 design note §3): the callback interval and
//! the callback's own time, in 1 µs buckets below two periods and one overflow
//! bucket at two periods or more, counted since the stream opened. The RT
//! thread records with one relaxed increment into an array allocated before
//! the stream starts (I7); the control thread reads a sparse snapshot once a
//! second for `Status`.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One bucket: 1 µs.
pub const BUCKET_NS: u64 = 1_000;
/// The longest range below the overflow bucket: 1 ms. Two periods are 667 µs
/// at 32 samples, 96 kHz; a larger NullRt block is capped here, so `Status`
/// and the guard's reply stay bounded (at most 1001 buckets each).
pub const MAX_RANGE_NS: u64 = 1_000_000;

pub struct PeriodHist {
    limit_ns: u64,
    top: usize,
    counts: Box<[AtomicU64]>,
}

impl PeriodHist {
    pub fn new(period_ns: u64) -> Self {
        let limit_ns = period_ns.max(1).saturating_mul(2).min(MAX_RANGE_NS);
        let top = usize::try_from(limit_ns.div_ceil(BUCKET_NS)).unwrap_or(0);
        Self { limit_ns, top, counts: (0..=top).map(|_| AtomicU64::new(0)).collect() }
    }

    /// The overflow bucket's index: two periods in µs, rounded up.
    pub fn top(&self) -> u32 {
        u32::try_from(self.top).unwrap_or(u32::MAX)
    }

    /// The bucket of `ns`: whole µs below two periods, else the overflow.
    pub fn index(&self, ns: u64) -> usize {
        if ns >= self.limit_ns {
            self.top
        } else {
            usize::try_from(ns / BUCKET_NS).unwrap_or(self.top)
        }
    }

    /// RT thread: one relaxed increment (I7).
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    pub fn record(&self, ns: u64) {
        if let Some(c) = self.counts.get(self.index(ns)) {
            c.fetch_add(1, Relaxed);
        }
    }

    /// The non-empty buckets, ascending (control thread; allocates).
    pub fn sparse(&self) -> Vec<(u32, u64)> {
        self.counts
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let n = c.load(Relaxed);
                (n > 0).then(|| (u32::try_from(i).unwrap_or(u32::MAX), n))
            })
            .collect()
    }
}

/// Both histograms of one stream, shared with its RT thread.
pub struct StreamHists {
    pub interval: PeriodHist,
    pub process: PeriodHist,
}

impl StreamHists {
    pub fn new(period_ns: u64) -> Self {
        Self { interval: PeriodHist::new(period_ns), process: PeriodHist::new(period_ns) }
    }

    pub fn snapshot(&self) -> HistSnapshot {
        HistSnapshot { top_us: self.interval.top(), interval: self.interval.sparse(), process: self.process.sparse() }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistSnapshot {
    pub top_us: u32,
    pub interval: Vec<(u32, u64)>,
    pub process: Vec<(u32, u64)>,
}

/// The upper edge (µs) of the bucket holding the `per_mille` quantile (rank
/// ⌈per_mille·n/1000⌉, at least 1): the soak verdict's rule
/// (scripts/iem-pc/soak_verdict.py `quantile_us`). `None` when empty.
pub fn quantile_us(sparse: &[(u32, u64)], per_mille: u64) -> Option<u32> {
    let total: u64 = sparse.iter().map(|e| e.1).sum();
    let rank = per_mille.saturating_mul(total).div_ceil(1000).max(1);
    let mut seen = 0u64;
    sparse.iter().find_map(|&(b, n)| {
        seen = seen.saturating_add(n);
        (total > 0 && seen >= rank).then(|| b.saturating_add(1))
    })
}
```

  - `lib.rs`: add `pub mod hist;`, add `#![cfg_attr(iem_rtsan, feature(sanitize))]` next to `#![deny(unsafe_code)]`, and list `hist` in the module docs.
  - `Cargo.toml` (copied from `crates/iem-engine/Cargo.toml`):

```toml
[lints.rust]
# `iem_rtsan` is set by the rtsan CI job (nightly, -Zsanitizer=realtime): hist::PeriodHist::record.
unexpected_cfgs = { level = "warn", check-cfg = ["cfg(iem_rtsan)"] }
```

- [ ] **Step 3 (GREEN): the judged interval.**
  - `Telemetry::on_callback(&self, entry_ns: u64, position: Option<i64>) -> Option<u64>` returns `Some(dt)` exactly where it records `self.interval.record(dt)` today, and `None` otherwise.
  - Its doc adds: "the backend records the same interval in the stream's histogram (S7)".
  - Callers that ignore the value stay as they are: the spike `Stream::on_buffer` and the tests.
- [ ] **Step 4 (GREEN): NullRt.**
  - `NullRt` gains `hists: Arc<StreamHists>`, built in `start` from `period(block, rate)` in ns. It is cloned into the thread closure, and `pace(cfg, p, s, h)` records:

```rust
let started = Instant::now();
if let Some(before) = prev.replace(started) {
    h.interval.record(u64::try_from(started.saturating_duration_since(before).as_nanos()).unwrap_or(u64::MAX));
}
// … process, as today …
s.max_ns.fetch_max(ns, Ordering::AcqRel);
h.process.record(ns);
```

  - `pub fn histograms(&self) -> HistSnapshot { self.hists.snapshot() }`.
  - `Shared` keeps `#[derive(Default)]`.
- [ ] **Step 5 (GREEN): ASIO (Windows; `asio.rs` is excluded from mutation).**
  - `AsioStream::start` builds `let hists = Arc::new(StreamHists::new(telemetry::period_ns(frames, format::RATE)));`. It keeps the `Arc` in `AsioStream { hists }` and passes a clone in `Start { hists }`, which goes to `Owner { hists }`. UNVERIFIED: the line in `owner_main` where `Owner` is built from `Start`; thread the field through there.
  - `start_stream` sets `Backend { hists: Arc::clone(&self.hists), .. }` (owner thread, at open). The histograms are counted since `AsioStream::start` and survive reopens. Each open's telemetry still skips its warm-up, so the reopen gap is never recorded.
  - `Backend::on_buffer`:

```rust
if let Some(dt) = self.telemetry.on_callback(nanos(entry).max(1), position) {
    self.hists.interval.record(dt);
}
// … as today …
let took = nanos(self.base.elapsed().saturating_sub(entry));
self.telemetry.on_done(took);
self.hists.process.record(took);
```

  - `pub fn histograms(&self) -> HistSnapshot { self.hists.snapshot() }`.
- [ ] **Step 6: the engine's RT workload.**
  - `tests/common/mod.rs`: `Scenario` gains `/// The backends' per-block histogram records (S7), as drive makes them. pub hists: Arc<StreamHists>`. `scenario()` builds it with `StreamHists::new(period_ns(BLOCK as u32, 96_000.0))`.
  - `drive` records around `process`, as a backend does:

```rust
/// On time, late (1.5 periods), the S1a p99.9 edge and a missed period (overflow).
const INTERVALS: [u64; 4] = [333_333, 347_000, 500_000, 700_000];
// in the loop, before `process`:
s.hists.interval.record(INTERVALS[k % INTERVALS.len()]);
// after it:
s.hists.process.record(20_000 + (k % 64) as u64 * 1_000);
```

  - Update the module doc's list of the worst case ("the backends' two histogram records per block").
- [ ] **Step 7: bench.** `examples/bench.rs` records each timed `process` call into a `PeriodHist::new(period_ns(32, 96_000.0))` (`(dt * 1000.0) as u64` ns) and prints `hist p99.9 {quantile_us(..., 999)} µs` on the existing line, beside the sorted p99.9. It prints only, never gates.
- [ ] **Step 8: rule.** In `.claude/rules/engine.md`, after the RT contract bullet, add one bullet, "Stream histograms (S7)":
  - `iem_audio_io::hist`: buckets `[b, b+1)` µs below two periods, overflow at 2 periods (= `missed`), range capped at 1 ms;
  - NullRt and the ASIO backend record the interval (after the warm-up) and the callback's own time (the span of `max_process_ns`);
  - counted since the stream opened (the ASIO card's reopens included);
  - proofs as above.

  Local checks, then commit:

```bash
cd "$WORK" && cargo fmt --all --check
```

  Commit: `feat(audio-io): [green] stream histograms of the callback interval and time, RT-safe (#10)`.

---

### Task 3: `Status` fields, the guard's `Reply.engine`, the frame cap [lane B, ~450 LoC; after Task 2]

**Files:** `crates/iem-engine-proto/src/msg.rs`, `crates/iem-engine/src/{control.rs,engine.rs,asio.rs}`, `crates/iem-guard/src/{pc.rs,effects/engine.rs,proto.rs,daemon.rs}`, `crates/iem-guard/src/daemon/record_tests.rs` (new), `.claude/rules/{engine,guard}.md`.

- [ ] **Step 1 (RED): engine side.**
  - `msg.rs` `the_histograms_in_status_are_additive_and_sparse`:
    - a `Status { hist_top_us: 667, interval_hist: vec![(333, 2990), (667, 1)], process_hist: vec![(40, 2991)], .. }` serialises `json["interval_hist"] == json!([[333, 2990], [667, 1]])` and round-trips;
    - `OldStatus` (the existing test struct) reads it;
    - `Status::default()` serialises no `interval_hist` or `process_hist` key, and `hist_top_us` is 0;
    - an old engine's `{"callbacks":4}` reads with empty histograms and top 0.
  - `control.rs` `status_carries_both_histograms_and_their_top`:
    - a test driver `Measured(HistSnapshot)` (`stats` running, `stop` Released, `histograms` → `Some(clone)`) replaces `r.c.driver`;
    - `status_msg(&StreamStats::default())` then carries all three fields;
    - with `Idle` they are empty and 0.

  Commit: `test(engine): [red] Status carries both stream histograms, sparse and additive (#10)`.
- [ ] **Step 2 (GREEN): engine side.** `msg.rs` `Status`:

```rust
    /// S7, additive (design note §3): the callback interval since the stream
    /// opened, 1 µs buckets `[b, b + 1)` below two periods and the overflow
    /// bucket `hist_top_us` (two periods or more: the card's `missed`), as
    /// `[[bucket, count], …]`, non-empty buckets ascending. Absent without a
    /// stream and from an older engine.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interval_hist: Vec<(u32, u64)>,
    /// S7, additive: the callback's own time, the span `process_max_us`
    /// measures (decode, `process()` and encode on the card; `process()` on
    /// NullRt), in the buckets of `interval_hist`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub process_hist: Vec<(u32, u64)>,
    /// S7, additive: the overflow bucket's index, two periods in µs rounded
    /// up (667 at 32 samples, 96 kHz; at most 1000); 0 without histograms.
    pub hist_top_us: u32,
```

  - `control.rs`:
    - `Driver` gains a default method:

```rust
    /// The stream's histograms since it opened (S7 design note §3), read once
    /// a second for `Status`; none from a backend without them.
    fn histograms(&self) -> Option<HistSnapshot> {
        None
    }
```

    - `status_msg` reads `let h = self.driver.as_ref().and_then(|d| d.histograms()).unwrap_or_default();` and sets `hist_top_us: h.top_us, interval_hist: h.interval, process_hist: h.process`.
  - `engine.rs` `NullRtDriver::histograms` → `Some(self.0.histograms())`; `asio.rs` `AsioDriver::histograms` → `Some(self.stream.histograms())`.

  Commit: `feat(engine): [green] Status carries the stream histograms (#10)`.
- [ ] **Step 3 (RED): guard side.**
  - `effects/engine.rs`:
    - `engine_messages_are_read_field_by_field` gains a status with `"late": 5, "overruns": 1, "process_max_us": 61.5, "hist_top_us": 667, "interval_hist": [[333, 359990], [400, 9]], "process_hist": [[60, 360000]]`, read into the new `pc::Status` fields;
    - new `a_histogram_with_a_bad_entry_reads_as_none`: each of `[[1]]`, `[[1,2,3]]`, `[[-1,2]]`, `[[4294967296,1]]`, `[["a",1]]`, `{"a":1}`, and one bad entry among good ones, reads as an empty `Vec`;
    - `the_reply_names_the_engine_by_its_commit` passes the new fields and `pid` through `engine_status(&seen, 3, Some(70), Some(4242))`.
  - `proto.rs`:
    - `the_engine_carries_the_fields_hil_v1_reads` expects the exact JSON with `"pid": 4242, "late": 5, "overruns": 1, "process_max_us": 61.5, "hist_top_us": 667, "interval_hist": [[333, 359990], [400, 9]], "process_hist": [[60, 360000]]`;
    - a partial engine `{"frames":32}` still reads with the rest at defaults, and `EngineStatus::default()` has no histogram keys;
    - `the_frame_cap_is_64_kib` becomes `the_frame_cap_is_256_kib`: `assert_eq!(MAX_FRAME, 262_144)`, and a 70 000-byte body is accepted (above 64 KiB);
    - new `the_largest_reply_fits_a_frame`: `Alarms::KEEP` alarms of `"ž".repeat(ALARM_CHARS)`, a `"ž".repeat(DETAIL_CHARS)` detail, a switching of all `Step::ALL`, and an engine with both histograms at 1001 entries of `(1000, u64::MAX)`; `write_frame` must succeed.
  - `daemon/record_tests.rs` (new; `daemon.rs` gets `#[cfg(test)] mod record_tests;`) `the_engine_reply_names_its_pid`: with `g.state.pids.engine = Some(Child { pid: 4242, .. })` and an engine seen, `handle(.., Request::Status, ..)` gives `engine.pid == Some(4242)`; with no engine child, `None`.

  Commit: `test(guard): [red] the engine's soak figures, histograms and pid in the reply; a 256 KiB frame (#10)`.
- [ ] **Step 4 (GREEN): guard side.**
  - `pc.rs` `Status` gains `late: u64, overruns: u64, process_max_us: f64, hist_top_us: u32, interval_hist: Vec<(u32, u64)>, process_hist: Vec<(u32, u64)>`, documented as "S7, from the engine's `Status`; an older engine's are 0 and empty". Update every literal in tests (`effects/engine.rs` ×6, `pc.rs` ×2, `daemon/tests.rs` ×1).
  - `effects/engine.rs`:

```rust
/// `Status.interval_hist` / `process_hist` (S7): `[[bucket, count], …]`. A
/// histogram with any entry that is not a pair of integers in range reads as
/// none (the soak verdict then names it), never as part of one.
fn hist(v: &Value, key: &str) -> Vec<(u32, u64)> {
    let Some(list) = v.get(key).and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .map(|e| {
            let pair = e.as_array().filter(|p| p.len() == 2)?;
            let bucket = u32::try_from(pair.first()?.as_u64()?).ok()?;
            Some((bucket, pair.get(1)?.as_u64()?))
        })
        .collect::<Option<Vec<_>>>()
        .unwrap_or_default()
}
```

    `parse` fills `late`, `overruns` (`number`), `process_max_us` (`as_f64`, else 0.0), `hist_top_us` (`u32::try_from(number(..)).unwrap_or(0)`) and both histograms. `engine_status(seen, spawns, last_exit, pid: Option<u32>)` copies them.
  - `proto.rs`:
    - `pub const MAX_FRAME: usize = 256 * 1024;` with the doc "a reply carries every kept alarm and the engine's two histograms (S7): `the_largest_reply_fits_a_frame`";
    - `EngineStatus` gains:

```rust
    /// The engine process the guard started or adopted (`GuardState.pids`):
    /// the soak's "one pid" (S7 design note §4); null when unknown.
    pub pid: Option<u32>,
    /// S7, passed through from the engine's `Status` (design note §3): the
    /// 1.5-period late counter (information), overruns, the longest callback,
    /// and both histograms; an older engine's are 0 and absent.
    pub late: u64,
    pub overruns: u64,
    pub process_max_us: f64,
    pub hist_top_us: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub interval_hist: Vec<(u32, u64)>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub process_hist: Vec<(u32, u64)>,
```

  - `daemon.rs` `Guard::engine_status` passes `self.state.pids.engine.as_ref().map(|c| c.pid)`.
  - Rules:
    - `.claude/rules/guard.md`: the HIL v1 bullet's `EngineStatus` field list gains `pid`, `late`, `overruns`, `process_max_us`, `hist_top_us`, `interval_hist`, `process_hist`; add the 256 KiB frame and its reason.
    - `.claude/rules/engine.md`: `Status` lists the three fields.
  - Version skew: an iemmode older than this frame cap refuses replies over 64 KiB. `activate` copies the bins before the hand-over, so the pair never mixes on the PC.

  Commit: `feat(guard): [green] Reply.engine carries the soak figures, histograms and pid; 256 KiB frames (#10)`.

  **Decision recorded on #10 (agent's own):**
  - With 50 kept alarms (~36 KB) plus the 8 KB detail, two histograms (up to 54 KB at the 1 ms cap) could pass the old 64 KiB cap.
  - A reply over the cap is never written, so `iemmode status`, and the reply of `iemmode event`, would fail. Hence 256 KiB.
  - Alternative not taken: a separate request for the histograms. It adds a request type and leaves the tray's updates as they are.

---

### Task 4: Guard: the switch record and the silence window [lane B again, ~450 LoC; after Task 3: same files]

**Files:**
- Create: `crates/iem-guard/src/switch_log.rs`.
- Modify: `crates/iem-guard/src/{lib.rs,state.rs,proto.rs,daemon.rs,view.rs,cli.rs}`, `crates/iem-guard/src/daemon/record_tests.rs`, `.claude/rules/guard.md`.

- [ ] **Step 1 (RED): pure tests** (`switch_log.rs` `#[cfg(test)] mod tests`; `st(step, ms)` builds a `StepTime`).
  - `the_silence_of_a_switch_to_event_runs_from_the_engine_stop_through_reapers_handover`: steps `[JobsCancel 5, RunnerStop 900, EngineStop 600, ServerStop 300, TrayStop 100, TuningExit 2000, PrefCheck 50, ReaperStart 15000, ReaperHandover 7000, AppStart 9000, AppHandover 3000, Fingerprint 200]` give `Some(25_050)`.
  - `the_silence_of_a_switch_to_dev_runs_from_reapers_save_and_quit_through_the_engine_arm`: steps `[Precheck 400, AppStop 3000, ReaperSaveQuit 8000, TuningEnter 2000, Data 1500, PrefCheck 50, EngineStart 700, EngineArm 10500, ServerStart 900, TrayStart 300, IdentityCheck 2000, RunnerStart 800]` give `Some(22_750)` for `Mode::Dev` and the same for `Mode::Live`.
  - `the_first_silencing_step_opens_the_window`: an event plan with `EngineStop` before a stale REAPER's `ReaperSaveQuit` starts at `EngineStop`.
  - `a_failed_engine_stop_and_its_health_read_count_toward_the_silence`: `EngineStop, EngineHealth, …, ReaperHandover` sums every step between.
  - `a_switch_that_silenced_nothing_or_never_played_again_has_no_window`: no start step gives `None`; a start without the end gives `None`; `[ReaperHandover, EngineStop]` (the end before the start) gives `None`.
  - `laps_time_each_step_from_the_end_of_the_one_before`: with `t0 + Duration`s, `start(t0); lap(EngineStop, t0+600ms); lap(ServerStop, t0+900ms)` gives `[600, 300]`; `take` empties and the next `lap` without `start` is 0 ms.
  - `outcomes_read_and_an_unknown_one_reads_as_unknown`: `"kept_serving"` gives `KeptServing`; `"later"` gives `Unknown`.
  - `a_record_a_guard_cannot_read_is_none`: `lenient` turns `{"from": 7}` into `None` and a valid record into `Some`.
- [ ] **Step 2 (RED): state, proto, daemon tests.**
  - `state.rs`:
    - `the_last_switch_round_trips_and_a_reset_keeps_it`: `sample()` gains a record, save/load is equal, and `reset()` keeps it;
    - `an_older_guards_state_without_a_last_switch_loads`;
    - `a_last_switch_this_guard_cannot_read_is_dropped_not_the_state`: `{"mode":"dev","last_switch":{"from":"dev"}}` loads with mode dev, `last_switch: None`, no error.
  - `proto.rs`:
    - `replies_round_trip_with_alarms_and_a_switch` gains `last_switch`;
    - an old reply without it decodes to `None`;
    - a malformed `last_switch` decodes the reply with `None`;
    - `the_largest_reply_fits_a_frame` adds a record of 30 steps.
  - `daemon/record_tests.rs`:
    - `a_dev_entry_keeps_its_record_with_every_step_timed`: `FakePc::new(band_up())` and `pc.delay(Call::EngineArm, 150 ms)`, then `run_switch(.., Event, Dev)`. Check `g.state.last_switch`: `from` event, `to` dev, `ended_in` dev, `outcome` `Done`; `steps.iter().map(|s| s.step)` equals `plan(Mode::Dev, &band_up())`; the `EngineArm` step has `ms >= 150`; `ended >= started`; `silence_ms == Some(sum over ReaperSaveQuit..=EngineArm) >= 150`.
    - `a_failed_step_is_timed_and_kept_in_the_record`: an event plan whose `AppHandover` fails (policy `Continue`) still lists `AppHandover`.
    - `a_failed_engine_stop_records_its_health_read`: `pc.fail(Call::EngineStop, …)` gives `[…, EngineStop, EngineHealth, …]`.
    - `every_reply_and_the_pipes_view_carry_the_last_switch`: `g.reply(..)`, `handle(.., Request::Status, ..)` and `Shared::route(&Request::Status)` all carry `g.state.last_switch`.
    - `the_record_survives_a_guard_restart`: a guard on a temp root (`Guard::open`) runs a switch; `GuardState::load` reads the same record.
    - `an_unwound_dev_entry_records_the_unwind`: a failed `EngineArm` gives a record with `to: Event` and `ended_in: Event` (the unwind is a switch of its own, `back_to_event`).
    - Mark the needed helpers in `daemon/tests.rs` (`band_up`, `iemmixer_up`) `pub(super)`.

  Commit: `test(guard): [red] the last switch with its steps timed and its in-ear silence, kept and replied (#10)`.
- [ ] **Step 3 (GREEN): `switch_log.rs`** (`pub mod switch_log;` in `lib.rs`):

```rust
//! The guard's record of the last switch (S7 design note §5): each step with
//! its time and the in-ear silence it caused. Pure: the daemon times the
//! steps (`Laps`) and keeps the record in its state and its replies; `iempc
//! switch-test` (S7 part 3) reads it.

use std::time::Instant;

use serde::{Deserialize, Deserializer, Serialize};

use crate::plan::{Mode, Step};
use crate::state::Switching;

/// A step and its time: from the end of the step before it (the switch's
/// start for the first) to its own end, so a record's steps add up to the
/// switch (the state save between two steps counts toward the second).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepTime {
    pub step: Step,
    pub ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchOutcome {
    Done,
    KeptServing,
    NeedsOwner,
    /// An outcome a newer guard saved.
    #[serde(other)]
    Unknown,
}

/// The last switch that ended: in `GuardState` and in every `Reply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSwitch {
    pub from: Mode,
    pub to: Mode,
    /// The mode the switch left the guard in (an unwound or stopped switch
    /// ends elsewhere than `to`).
    pub ended_in: Mode,
    pub outcome: SwitchOutcome,
    /// Seconds since the epoch.
    pub started: u64,
    pub ended: u64,
    /// Every step that ran, failed ones included, in order.
    pub steps: Vec<StepTime>,
    /// [`silence_ms`] of the steps.
    pub silence_ms: Option<u64>,
}

impl LastSwitch {
    pub fn new(sw: &Switching, ended_in: Mode, outcome: SwitchOutcome, ended: u64, steps: Vec<StepTime>) -> Self {
        let silence_ms = silence_ms(sw.to, &steps);
        Self { from: sw.from, to: sw.to, ended_in, outcome, started: sw.started, ended, steps, silence_ms }
    }
}

/// The in-ear silence of a switch to `to` (ms): from the first step that
/// silences the in-ears (the engine's stop and fade-out, or REAPER's save
/// and quit, whichever ran first) through the step after which the other
/// side plays (REAPER's handover for `event`, the engine's arm for `dev` and
/// `live`), both included. `None` when either end is missing.
pub fn silence_ms(to: Mode, steps: &[StepTime]) -> Option<u64> {
    let start = steps
        .iter()
        .position(|s| matches!(s.step, Step::EngineStop | Step::ReaperSaveQuit))?;
    let last = if to == Mode::Event { Step::ReaperHandover } else { Step::EngineArm };
    let window = steps.get(start..)?;
    let end = window.iter().position(|s| s.step == last)?;
    Some(window.get(..=end)?.iter().map(|s| s.ms).sum())
}

/// The step clock of the switch running now (not persisted: a restarted
/// guard re-plans to event and times that switch).
#[derive(Debug, Default)]
pub struct Laps {
    last: Option<Instant>,
    steps: Vec<StepTime>,
}

impl Laps {
    pub fn start(&mut self, now: Instant) {
        self.last = Some(now);
        self.steps.clear();
    }

    pub fn lap(&mut self, step: Step, now: Instant) {
        let ms = self.last.map_or(0, |t| {
            u64::try_from(now.saturating_duration_since(t).as_millis()).unwrap_or(u64::MAX)
        });
        self.steps.push(StepTime { step, ms });
        self.last = Some(now);
    }

    pub fn take(&mut self) -> Vec<StepTime> {
        self.last = None;
        std::mem::take(&mut self.steps)
    }
}

/// `GuardState.last_switch` and `Reply.last_switch` as read: a record this
/// guard cannot read is none, never an unreadable state or reply.
pub fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Option<LastSwitch>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}
```

- [ ] **Step 4 (GREEN): wiring.**
  - `state.rs` `GuardState`:

```rust
    /// The last switch that ended (S7 design note §5); a reset keeps it.
    #[serde(deserialize_with = "crate::switch_log::lenient")]
    pub last_switch: Option<LastSwitch>,
```

    The struct-level `#[serde(default)]` covers a missing field.
  - `proto.rs` `Reply`:

```rust
    /// The last switch that ended, with its steps timed and its silence
    /// (S7); absent from an older guard.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::switch_log::lenient")]
    pub last_switch: Option<LastSwitch>,
```

    Every `Reply { .. }` literal gains `last_switch: …`: `View::reply`, `Guard::reply`, and the tests in `proto.rs` ×5, `view.rs` ×1, `cli.rs` ×1.
  - `daemon.rs` (call sites only; #36):
    - `Guard` gains `laps: Laps` (default);
    - `View` gains `pub last_switch: Option<LastSwitch>`, published in `Guard::publish` with `v.last_switch.clone_from(&self.state.last_switch)`;
    - `begin`: `self.laps.start(Instant::now());`
    - `switch()`: `let r = run_step(pc, g, step, to); g.laps.lap(step, Instant::now()); match r { … }`;
    - `failure()`: right after `pc.engine_health()`, `g.laps.lap(Step::EngineHealth, Instant::now())`;
    - `finish()`:

```rust
let sw = self.state.switching.take();
let from = sw.as_ref().map(|s| s.from);
// … mode, job, trial as today …
if let Some(s) = &sw {
    self.state.last_switch = Some(LastSwitch::new(s, mode, outcome.into(), self.now(), self.laps.take()));
}
```

    - `impl From<Outcome> for SwitchOutcome` (`Done → Done`, `KeptServing → KeptServing`, `NeedsOwner → NeedsOwner`).
  - `.claude/rules/guard.md`: one bullet, "Switch record (S7, `switch_log.rs`)", covering the step time definition, the window ends, the unwind being its own record, `lenient`, and `Reply.last_switch` being what `iemmode status` prints.

  Commit: `feat(guard): [green] the last switch with its steps timed and its in-ear silence (#10)`.

---

### Task 5: `iem-soakclient`: the crate and its pure core [lane C, ~500 LoC]

**Files:**
- Create: `crates/iem-soakclient/{Cargo.toml,src/lib.rs}`.
- Modify: `Cargo.toml` (members), `Cargo.lock`.

- [ ] **Step 1: crate skeleton and lock.**

```toml
[package]
name = "iem-soakclient"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
description = "iemmixer soak harness (S7): one mixer socket and one listen socket on a member's mix, counted into a JSON summary"

[dependencies]
# The listen frames are the server's Opus (X4); the server's own codec crate.
opus = "0.4"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
# Sync WebSocket client (already locked through axum's ws); plain ws:// on the LAN.
tungstenite = { version = "0.28", default-features = false, features = ["handshake"] }
# Login and /api/site over plain HTTP, as the guard uses it.
ureq = { version = "3.4", default-features = false }

# The housekeeping CPU Set (S1c profile), as the engine sets its own.
[target.'cfg(windows)'.dependencies]
iem-win = { path = "../iem-win" }
```

  Add `"crates/iem-soakclient"` to `[workspace].members`. Then update and check the lock:

```bash
cd "$WORK" && cargo metadata --format-version 1 >/dev/null && cargo metadata --locked --format-version 1 >/dev/null
git diff --stat Cargo.lock    # expected: the iem-soakclient entry only; anything else is reported on #10 before going on
python3 scripts/check_engine_deps.py   # the engine's closure is unchanged
```

- [ ] **Step 2 (RED): pure tests** (`lib.rs` `#[cfg(test)] mod tests`).
  - `parse_args`:
    - `the_arguments_and_their_defaults`: `--member member9 --seconds 600 --out s.json` gives base `http://127.0.0.1`, `direct` false, no CPU Sets;
    - `every_bad_argument_is_a_usage_error`: missing `--member`, `--out` or `--seconds`; `--seconds 0`, `36001` or `x`; a member outside `[A-Za-z0-9_-]{1,64}`; an unknown flag, and `--pin` in particular (`the_pin_never_comes_from_argv`); `--cpu-sets 256,x`; `--cpu-sets` off Windows (`cfg!(windows)` decides the expectation).
  - `pin_from`:
    - `IEM_SOAK_PIN` digits (4–12) accepted;
    - empty, missing or non-digit refused, with a message that never echoes the value.
  - `origin`:
    - `--direct` uses the base;
    - otherwise `lan_url` `http://10.0.0.10/` gives `http://10.0.0.10`;
    - `None` gives `Reason::SiteUnreadable`;
    - `https://mixer.example.org` gives `Reason::NotHttp`.
  - URL builders:
    - `ws_url("http://10.0.0.10:8080", "/ws/audio?token=t")` gives `ws://10.0.0.10:8080/ws/audio?token=t`;
    - `mixer_path("member9", "t")` gives `/ws/member9?token=t&proto=2`;
    - `listen_start("member9")` gives `{"cmd":"ListenStart","member_id":"member9"}`.
  - `Gaps`:
    - `a_wait_of_exactly_60_ms_is_no_gap_and_61_is`;
    - `the_end_counts_the_wait_since_the_last_frame`;
    - `a_run_without_a_frame_is_one_gap_as_long_as_the_run`;
    - `a_reconnect_does_not_restart_the_gap_clock`;
    - `expected_frames_count_one_per_20_ms_from_the_first_frame`.
  - `classify`: `Meters`, `AudioStatus` with its status, `Hello`/`State` as `Other`, and garbage as `Other`.
  - `backoff(n)`: 1, 2, 4, 8, 10, 10 s.
  - `Summary`:
    - `the_summary_has_its_schema_and_no_site_value`: the default serialises `schema` 1 and every field below; it has no `member`, `url` or `pin` key;
    - `write_summary_replaces_the_file_atomically`: a temp dir holds no `.tmp` left behind and a second write replaces the first.

  Commit: `test(soak): [red] the soak client's arguments, gaps, events and summary (#10)`.
- [ ] **Step 3 (GREEN): `lib.rs`.** Key types:

```rust
//! The soak harness (S7 design note §4): logs in as the engineer through the
//! server's login, opens one mixer socket and one listen socket on one
//! member's mix at the LAN address the server names (`/api/site`), decodes
//! the Opus frames and counts frames, gaps, reconnects and meter frames into
//! a JSON summary. It reads only: it sends no mixer command. The PIN comes
//! from `IEM_SOAK_PIN`, never from the command line. Nothing here ends a
//! process: sockets close by being dropped.

pub mod net;

/// More than this without a listen frame is a gap.
pub const GAP: Duration = Duration::from_millis(60);
/// One Opus frame: 20 ms, 960 samples per channel at 48 kHz.
pub const FRAME: Duration = Duration::from_millis(20);
pub const SCHEMA: u32 = 1;
pub const PIN_ENV: &str = "IEM_SOAK_PIN";
/// The build, as the guard names its own (`GITHUB_SHA` in CI).
pub const BUILD: &str = match option_env!("GITHUB_SHA") { Some(s) => s, None => "local" };

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub base: String,       // --base (default http://127.0.0.1): where /api/site is read
    pub direct: bool,       // --direct: everything through --base (CI; the test site's lan_url is a placeholder)
    pub member: String,     // --member: the mix both sockets are on
    pub seconds: u64,       // --seconds 1..=36000
    pub out: PathBuf,       // --out: the summary file
    pub cpu_sets: Vec<u32>, // --cpu-sets 256,257 (Windows): CPU Set ids, as the engine's [card] cpu_sets
}

/// Why a run ended early: a fixed code, never a site value (P6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason { SiteUnreadable, NotHttp, LoginRefused, NotEngineer, ServerGone, CpuSets }
impl Reason {
    pub fn code(self) -> &'static str { /* "site-unreadable", "not-http", "login-refused", "not-engineer", "server-gone", "cpu-sets" */ }
}

/// The harness's summary (S7 design note §4), rewritten every minute and at
/// the end: numbers and reason codes only (P6).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Summary {
    pub schema: u32,
    pub build: String,
    /// The whole `--seconds` ran.
    pub complete: bool,
    /// Since the sockets were first opened.
    pub seconds: f64,
    pub frames: u64,
    /// One per 20 ms from the first frame to the end.
    pub expected_frames: u64,
    /// Frames Opus refused, or that held other than 960 samples.
    pub decode_errors: u64,
    /// More than 60 ms without a frame (first frame to the end; a run without one is one gap).
    pub gaps: u64,
    pub max_gap_ms: u64,
    /// ListenStart to the first frame.
    pub first_frame_ms: Option<u64>,
    /// `Meters` events on the mixer socket.
    pub meter_frames: u64,
    /// Sockets opened again after a close, both sockets.
    pub reconnects: u64,
    /// `AudioStatus` `no_source` answers.
    pub no_source: u64,
    /// Why the run ended early (`Reason::code`).
    pub error: Option<String>,
}
```

  Also define:
  - `Gaps` (`frame(now)`, `end(started, now)`, `expected(now)`; a gap is a wait `> GAP`);
  - `classify(text) -> Event { Meters, AudioStatus(String), Other }`;
  - `origin`, `ws_url`, `mixer_path`, `listen_start`, `backoff`, `parse_args`, `pin_from(Option<String>) -> Result<String, String>`;
  - `write_summary(path, &Summary) -> io::Result<()>` (`<out>.tmp`, then rename).

  Commit: `feat(soak): [green] the soak client's pure core and summary (#10)`.

---

### Task 6: `iem-soakclient`: the wire, the binary, the fake-server tests [lane C, ~550 LoC; after Task 5]

**Files:**
- Create: `crates/iem-soakclient/src/{net.rs,main.rs}`, `crates/iem-soakclient/tests/fake_server.rs`.
- Modify: `.config/nextest.toml`.

- [ ] **Step 1 (RED): `tests/fake_server.rs`.**
  - The fake runs on `std::net::TcpListener` on `127.0.0.1:0`, with one thread per connection.
    - It peeks the head (`TcpStream::peek` until `\r\n\r\n`).
    - HTTP routes: `POST /api/auth` (checks the body `{"member":"engineer","pin":"1234"}`, answers `{"token":"T","member":"engineer","engineer":true,"expires_in":604800}` or 401) and `GET /api/site` (`{"lan_url": <script's>, "public_host": "mixer.example.org"}`).
    - Upgrades go through `tungstenite::accept_hdr`, recording the URI.
    - `/ws/member9?token=T&proto=2` sends `Hello`, `State`, then `{"event":"Meters","data":{"meters":{"mic1":[0.1,0.1]}}}` every 20 ms.
    - `/ws/audio?token=T` waits for `ListenStart{member_id:"member9"}`, answers `AudioStatus listening`, then sends one Opus packet (`opus::Encoder::new(48_000, Stereo, LowDelay)` over 1920 zeros) every 20 ms.
    - Every connection and URI goes to an `mpsc` channel the test reads.
  - Tests (each with `--seconds 2` or 3 and `Limits { give_up: 1 s, write_every: 500 ms, read_timeout: 100 ms }`):
    - `a_run_counts_frames_meters_one_reconnect_and_one_bad_frame`: the first listen connection is dropped after 10 frames and one garbage binary frame (`[0xff; 3]`) is sent. Expect `complete`, `reconnects == 1`, `decode_errors == 1`, `gaps >= 1`, `frames >= 60`, `meter_frames >= 60`, `first_frame_ms.is_some()`.
    - `the_sockets_go_to_the_lan_url_the_server_names`: fake A answers `/api/site` with fake B's `http://127.0.0.1:<port>`. A sees only `/api/site`; B sees the login and both sockets.
    - `a_refused_login_ends_the_run_with_its_reason`: a 401 gives `complete == false`, `error == Some("login-refused")`, and a return within 1 s.
    - `an_https_lan_url_is_refused`: gives `error == Some("not-http")`.
    - `a_server_that_stays_gone_ends_the_run_after_the_give_up_bound`: the fake drops both sockets and refuses later upgrades; `--seconds 30` must return within 5 s with `error == Some("server-gone")`. It runs `net::run` on a thread with `recv_timeout(5 s)`, the bounded test.
    - `the_summary_written_names_no_member_and_no_host`: the written file contains neither `member9` nor `127.0.0.1`.

  Commit: `test(soak): [red] the soak client against a fake server: login, LAN URL, sockets, reconnects, give-up (#10)`.
- [ ] **Step 2 (GREEN): `net.rs`.**

```rust
pub struct Limits {
    /// A socket that cannot be opened again for this long ends the run (server-gone).
    pub give_up: Duration,
    pub write_every: Duration,
    /// Socket reads wait this long, so the threads see the end within it.
    pub read_timeout: Duration,
}
impl Default for Limits { /* 120 s, 60 s, 500 ms */ }

/// One run: login (once), then the mixer and listen sockets on their own
/// threads until `seconds` have passed or a socket stays gone past `give_up`;
/// `write` gets the summary every `write_every` and at the end.
pub fn run(args: &Args, pin: &str, limits: &Limits, write: &mut dyn FnMut(&Summary)) -> Summary
```

  - HTTP:
    - an agent built as `crates/iem-guard/src/win/mod.rs` builds it (`timeout_global(Some(10 s))`, `http_status_as_error(false)`, `max_redirects(0)`);
    - `GET {base}/api/site` unless `--direct`, then `origin(..)`;
    - `POST {origin}/api/auth` with `{"member":"engineer","pin":…}`; 200 with `engineer: true` is required.
  - Sockets:
    - `tungstenite::connect(ws_url(..))`, then `set_read_timeout(limits.read_timeout)` on the plain stream (`MaybeTlsStream::Plain`);
    - the listen thread sends `listen_start(member)` after every open, decodes binary frames with `opus::Decoder::new(48_000, Stereo)` and `decode_float(.., &mut [0f32; 1920], false)` (anything but `Ok(960)` is a decode error), and feeds `Gaps`;
    - text frames go through `classify`;
    - a close or error reopens after `backoff(n)` and counts `reconnects`;
    - at the end the listen thread sends `ListenStop`, and both sockets are dropped.
  - Shared counts sit in `Arc<Mutex<Tally>>`; the stop is an `Arc<AtomicBool>`.
  - `seconds` runs from the first open.
- [ ] **Step 3 (GREEN): `main.rs`.**
  - It reads `argv` and `IEM_SOAK_PIN`: a usage error exits 2 with `USAGE`.
  - On Windows with `--cpu-sets`, it calls `iem_win::power::set_cpu_sets(&ids)` first; a failure writes `error: "cpu-sets"` and exits 1.
  - Then `run(.., &mut |s| write_summary(&args.out, s))`, a final write, the summary on stdout, and the exit code: 0 when `complete`, else 1.
  - Its stderr carries reason codes only.
- [ ] **Step 4:** in `.config/nextest.toml`, add `| test(=a_server_that_stays_gone_ends_the_run_after_the_give_up_bound)` to the `priority = 100` filter. `scripts/test_mutants_recheck.py` checks that the name exists.

  Commit: `feat(soak): [green] the soak client's sockets, login and binary (#10)`.

---

### Task 7: `soak_verdict.py`: the verdict, the report mapping, the CI harness check [lane D, ~500 LoC]

**Files:** create `scripts/iem-pc/soak_verdict.py`, `scripts/iem-pc/test_soak_verdict.py` (stdlib only; the integrity job's `unittest discover -s scripts/iem-pc` runs them).

- [ ] **Step 1 (RED): tests on synthetic poll records.**
  - The helper `polls(n=481, every=60, **change)` builds `{"t", "exit": 0, "status": {"mode": "dev", "switching": null, "detail": "mode dev; bundle <SHA>", "alarms": [], "engine": {"pid": 4242, "build": SHA, "callbacks", "missed", "resets", "late", "overruns", "process_max_us", "hist_top_us": 667, "interval_hist", "process_hist"}}}`.
  - In it, `callbacks` grows by 180 000 a minute, `interval_hist` bucket 333 by 179 990 and bucket 340 by 10, and `process_hist` bucket 40 by 180 000.
  - Tests:
    - `test_eight_hours_on_one_engine_inside_every_bound_is_green`;
    - `test_less_than_eight_hours_of_polls_is_red_with_the_hours`: 7.5 h gives a summary starting `red: polled 7.50 h of 8 h`;
    - `test_a_hole_in_the_polls_is_red`: 301 s without a poll;
    - `test_another_engine_pid_or_bundle_is_red`;
    - `test_a_poll_without_an_engine_or_out_of_dev_is_red`;
    - `test_one_missed_period_or_one_reset_is_red`: missed +1, resets +1;
    - `test_late_counts_intervals_of_347_us_or_more_from_the_histogram`: Δ bucket 346 = 10 % stays green; Δ bucket 347 at 0.21 % is red;
    - `test_late_at_exactly_two_per_mille_is_green_and_one_more_is_red`: 2 of 1000 green, 3 of 1000 red (integer math);
    - `test_counts_before_the_first_poll_do_not_count`: the first poll already holds late buckets and missed 5;
    - `test_a_histogram_that_went_back_or_is_missing_is_red`;
    - `test_process_p999_at_the_83_us_edge_is_green_and_one_bucket_more_is_red`: Δ `{10: 998, 82: 2}` is green (edge 83); `{10: 998, 83: 2}` is red (84);
    - `test_the_quantile_is_the_upper_edge_at_rank_ceil_999_n_per_mille`: the same cases as Rust `hist::quantile_us`;
    - `test_harness_gaps_reconnects_or_thin_frames_are_red`;
    - `test_an_incomplete_or_short_harness_is_red`;
    - `test_red_names_the_first_failing_number_in_order`: missed +1 and gaps 2 together, the summary names missed;
    - `test_the_summary_holds_numbers_only`: the summary matches `^[a-z0-9 .,:;%+()/≥µ-]*$` and contains no member id or SHA;
    - `test_drift_alarms_during_the_soak_are_counted_as_information`;
    - `test_report_maps_a_cancelled_or_unrecorded_pc_job_to_cancelled`;
    - `test_report_maps_a_left_dev_record_to_cancelled_and_other_pc_failures_to_failure`;
    - `test_harness_problems_for_the_ci_step`;
    - `test_main_report_reads_the_record_directory_and_prints_one_json`;
    - `test_main_harness_exits_1_on_a_problem`.

  Commit: `test(iem-pc): [red] the soak verdict on synthetic polls, the report mapping and the CI harness check (#10)`.
- [ ] **Step 2 (GREEN): `soak_verdict.py`.** Constants and the core:

```python
LATE_US = 347             # S1a p99.9 interval at B = 32 (design §3): intervals of 347 µs or more are late
LATE_PER_MILLE = 2        # ≤ 0.2 % of the soak's intervals
PROCESS_P999_US = 83      # 25 % of the 333 µs period
P999 = 999                # per mille
HOURS = 8.0
POLL_HOLE_S = 300         # a longer time without a poll is a hole in the record
FRAMES_PERCENT = 99       # the harness got at least this share of its expected frames
DRIFT = "tuning drift:"

def hist(pairs, where: str) -> dict[int, int]:            # [[b, c], …]; any bad entry raises Bad(where …)
def delta(first: dict, last: dict, where: str) -> dict:  # bucketwise last − first; a negative bucket raises Bad("… went back")
def at_or_above(h: dict, us: int) -> int:
def quantile_us(h: dict, per_mille: int) -> int | None:  # rank = (per_mille·n + 999) // 1000, at least 1; upper edge b + 1
def verdict(polls: list[dict], harness: dict | None, sha: str, hours: float = HOURS) -> dict:
    """{"conclusion": "success"|"failure", "summary": str, "first_failure": str|None, "numbers": {…}}"""
def report(pc_result: str, record: dict | None, polls: list[dict] | None, harness: dict | None, sha: str, hours: float) -> dict:
    """The ops report job's conclusion: cancelled for a cancelled PC job, a missing record or reason
    left-dev ("ide event" never makes red); failure for any other PC failure, naming its reason code;
    else verdict()."""
def harness_problems(summary: dict, min_seconds: float, max_gaps: int) -> list[str]:
```

  - The verdict's order, where the first failure leads the summary as `red: <it>; <numbers>`:
    1. polls exist; every poll has `exit` 0, mode `dev`, no switching and an engine;
    2. `engine.build == sha` throughout, and one non-null `engine.pid`;
    3. `last.t - first.t >= hours·3600` (reported as "polled N h") and no hole over `POLL_HOLE_S`;
    4. missed Δ == 0;
    5. resets Δ == 0;
    6. both histograms present, `hist_top_us > LATE_US`, Δ never negative;
    7. late: `at_or_above(Δinterval, 347) * 1000 <= total * 2`, with `total > 0`;
    8. `quantile_us(Δprocess, 999) <= 83`;
    9. the harness is `complete` and its `seconds >= hours·3600`;
    10. harness gaps == 0;
    11. harness reconnects == 0;
    12. `frames * 100 >= expected_frames * 99`.
  - Green: `green: 8.01 h, 481 polls, missed +0, resets +0, late 0.006 % (≥ 347 µs), process p99.9 41 µs, gaps 0, reconnects 0, frames 1441120`.
  - `numbers` also carries information: Δ`late` (the 1.5-period counter), Δ`overruns`, the last `process_max_us`, `decode_errors`, `meter_frames`, `drift_alarms` (alarms whose text starts with `DRIFT` and whose `at >= first.t`).
  - **Addition to design §4, flagged for review:** check 12 exists because with gaps measured between frames, gaps 0 alone cannot tell a thinned stream (a frame every 50 ms) from a full one.
  - CLI:
    - `soak_verdict.py report --pc <result> --dir <record dir> --sha <sha> --hours <h>` reads `result.json`, `polls.jsonl` (UTF-8, a BOM tolerated) and `soakclient.json` when present, prints one JSON object, and exits 0;
    - `soak_verdict.py harness <summary.json> --min-seconds N --max-gaps K` prints the problems and exits 1 when there are any.

  Commit: `feat(iem-pc): [green] the soak verdict, the report mapping and the CI harness check (#10)`.

---

### Task 8: `iempc dispatch-soak` [lane E, ~350 LoC; independent; may follow Task 7 in the same lane if the lane's diff stays at or below ~600]

**Files:**
- Create: `scripts/iem-pc/iempc_soak.py`, `scripts/iem-pc/test_iempc_soak.py`.
- Modify: `scripts/iem-pc/iempc.py` (call site only), `.claude/rules/guard.md`.

- [ ] **Step 1 (RED): `test_iempc_soak.py`.**
  - It reuses `test_iempc.Base` (`from test_iempc import Base, SHA, SHA2, RUN`; `Base` has no tests of its own) and `self.pc.replies[("status",)] = (0, json.dumps(READY))`, where `READY = {"ok": True, "mode": "dev", "switching": None, "detail": f"mode dev; bundle {SHA}", "alarms": [], "engine": {"build": SHA, "pid": 4242, "parked": False, "faulted": False}}`.
  - Tests:
    - `test_a_soak_is_dispatched_with_the_active_bundle_in_dev`:
      - `self.gh.named("workflow", "run")` equals `["workflow", "run", "soak.yml", "-R", ip.OPS_REPO, "-f", f"sha={SHA}", "-f", "branch=dev", "-f", f"run={RUN}", "-f", "hours=8"]`;
      - `self.pc.calls == [("iemmode.exe", ["status"], "abandon")]`;
      - `soak.json` holds `{sha, branch, run, hours, entry, at}`;
    - `test_the_flag_refuses_the_dispatch_before_any_call`: no PC call and no gh call;
    - `test_a_flag_that_appears_during_the_status_read_runs_the_event_path`: exit `ip.PREEMPTED`, and the calls end with `("iemmode.exe", ["event"], "ignore")`;
    - `test_a_flag_that_appears_during_the_gh_waits_stops_the_dispatch`: `FakeGh.on_list` writes the flag; refused with "no soak dispatch during an event (nothing was dispatched)"; nothing recorded;
    - `test_not_dev_switching_or_a_hil_job_is_refused`;
    - `test_another_active_bundle_or_engine_build_or_no_engine_is_refused`;
    - `test_a_parked_or_faulted_engine_is_refused`;
    - `test_a_sha_without_a_green_push_run_is_refused`;
    - `test_one_soak_per_sha_per_dev_entry`: refused a second time in the same dev entry, allowed after `next_entry`;
    - `test_hours_outside_1_to_9_are_refused`;
    - `test_active_bundle_and_soak_refusal_are_pure`: synthetic replies, including details `mode dev; no bundle` and `mode dev; bundle <SHA>; HIL job 42`.

  Commit: `test(iem-pc): [red] dispatch-soak: dev only, the active bundle, a green push run, once per dev entry (#10)`.
- [ ] **Step 2 (GREEN): `iempc_soak.py`.**

```python
"""`iempc dispatch-soak` (S7 design note §4): dispatches the private ops repo's
soak.yml with this box's gh authentication (no token in the public repo). The
soak changes no guard state, so the PC must already run the bundle: the guard
in dev, no switch, no HIL job, the active bundle and the running engine both
the SHA, which has a green push run of ci.yml (bundle and attest). Once per SHA
per dev entry (soak.json). A new "ide event" flag is checked right before the
dispatch. iempc.py passes itself in (`ip`), so this module never imports it
(#36: iempc.py is over its size budget)."""

SOAK_WORKFLOW = "soak.yml"
HOURS_DEFAULT = 8
HOURS_MAX = 9  # the ops job's 600 min hold 9 h of polls, the start and the end
BUNDLE = re.compile(r"(?:^|; )bundle ([0-9a-f]{40})(?:;|$)")

def active_bundle(reply: dict) -> str | None: ...
def soak_refusal(reply: dict | None, sha: str) -> str | None: ...   # pure
def dispatch(ctx, ip) -> int:
    sha = ip.check_sha(ctx.args.sha)
    hours = check_hours(ctx.args.hours)
    entry = ip.current_entry()
    done = list(ip.read_json(ip.state_dir() / "soak.json", {}).get("soaks", []))
    if any(d.get("sha") == sha and d.get("entry") == entry for d in done):
        raise ip.Refused(f"a soak of {sha} was already dispatched in dev entry {entry}")
    code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    why = soak_refusal(reply if code == 0 else None, sha)
    if why:
        raise ip.Refused(f"no soak: {why} (nothing was dispatched)")
    run, branch = ip.green_run(sha, ip.BRANCHES)
    if ip.event_now():
        raise ip.Refused(f"{ip.EVENT_NOW} appeared: no soak dispatch during an event (nothing was dispatched)")
    ip.gh(["workflow", "run", SOAK_WORKFLOW, "-R", ip.OPS_REPO, "-f", f"sha={sha}", "-f", f"branch={branch}",
           "-f", f"run={run}", "-f", f"hours={hours}"])
    record = {"sha": sha, "branch": branch, "run": run, "hours": hours, "entry": entry, "at": ip.now_iso()}
    ip.write_json(ip.state_dir() / "soak.json", {"soaks": (done + [record])[-200:]})
    ip.emit({"dispatched_soak": record})
    return 0
```

  `iempc.py` changes:
  - `import iempc_soak` after the stdlib imports (the script's folder is first on `sys.path` when it runs; the tests insert it);
  - the wrapper:

```python
def cmd_dispatch_soak(ctx: Ctx) -> int:
    """S7 soak dispatch; the code lives in iempc_soak.py (#36)."""
    return iempc_soak.dispatch(ctx, sys.modules[__name__])
```

  - `"dispatch-soak": Spec(cmd_dispatch_soak, pc=True, dev_time=True, locked=True)`;
  - a parser with `--sha` (required) and `--hours` (int, default `iempc_soak.HOURS_DEFAULT`);
  - the docstring's first paragraph names the soak dispatch.

  In `.claude/rules/guard.md`, the dev-box bullet adds `dispatch-soak` (its guards, `soak.json`, `gh run rerun` for a failed ops run of the same entry).

  Commit: `feat(iem-pc): [green] iempc dispatch-soak in iempc_soak.py (#10)`.

---

### Task 9: CI, bundle and rules [lane F, ~150 LoC; after Tasks 6 and 7]

**Files:** `.github/workflows/ci.yml`, `.cargo/mutants.toml`, `crates/iem-guard/src/bundle.rs` (test only), `.claude/rules/{e2e,soak}.md`, `CLAUDE.md`.

- [ ] **Step 1: bundle.**
  - The `bundle` job's build line adds `-p iem-soakclient`.
  - `$exes` becomes `'iem-engine', 'iem-server', 'iemmixer-guard', 'iemmode', 'iem-tray', 'iem-migrate', 'iem-soakclient'`.
  - `bundle::REQUIRED`, `iempc.BUNDLE_REQUIRED` and `Test-IemBundleSums` stay as they are: `valid_name` already admits any plain root name, and requiring the client would refuse the install of every older bundle.
  - Test: `bundle::tests::a_bundle_may_carry_the_soak_client` (`verify` accepts `iem-soakclient.exe` among the sums), committed as `test(guard): a bundle may carry the soak client (#10)`. It is a characterization test and passes at once.
- [ ] **Step 2: other jobs.**
  - `test`: the coverage command adds `--package iem-soakclient`, and the step name lists it.
  - `windows`: a new step `Soak client clippy (its Windows CPU Sets)`, `run: cargo clippy --locked -p iem-soakclient --all-targets -- -D warnings` (one cargo command per step).
  - `e2e`:
    - `timeout-minutes: 45`;
    - the build step adds `cargo build --locked --release -p iem-soakclient`;
    - after Playwright:

```yaml
      - name: Soak client against the NullRt engine and the server (10 min on a push, 2 min on a pull request; S7 design note section 9)
        # After Playwright: listen.spec.ts listens to member8 and the engineer's mix, and a second mix heard
        # at once answers no_source. The client reads only: it owns no mix (member9's page and listen tap).
        env:
          SOAK_SECONDS: ${{ github.event_name == 'push' && '600' || '120' }}
        run: |
          set -euo pipefail
          IEM_SOAK_PIN="$E2E_ENGINEER_PIN" ./target/release/iem-soakclient --base http://127.0.0.1:8080 --direct \
            --member member9 --seconds "$SOAK_SECONDS" --out "$RUNNER_TEMP/soakclient.json"
          cat "$RUNNER_TEMP/soakclient.json"
          python3 scripts/iem-pc/soak_verdict.py harness "$RUNNER_TEMP/soakclient.json" \
            --min-seconds "$((SOAK_SECONDS - 1))" --max-gaps 3
```

    - "Upload failure evidence" adds `${{ runner.temp }}/soakclient.json`.
  - `--max-gaps 3` is the hosted runner's tolerance for scheduling stalls; the PC verdict wants 0. Record the measured gaps of the first ten push runs on #10 and lower the tolerance if they stay at 0.
- [ ] **Step 3: mutation scope.** In `.cargo/mutants.toml` `exclude_globs`, add `"crates/iem-soakclient/src/main.rs"` with the reason: "argv, env, CPU Set and file glue of the soak binary; its decisions are lib.rs (parse_args, pin_from, origin, Gaps, write_summary) and net.rs (run), mutated; the e2e job runs the binary". `lib.rs` and `net.rs` stay mutated.
- [ ] **Step 4: rules.**
  - New `.claude/rules/soak.md`, with `paths: crates/iem-soakclient/**, scripts/iem-pc/soak_verdict.py, scripts/iem-pc/iempc_soak.py`:
    - the client (reads only, PIN from env, reason codes, give-up bound, CPU Sets);
    - the summary schema;
    - the verdict's checks and their order;
    - the report mapping;
    - `dispatch-soak`;
    - the known effects (the member's solo grace, Listen `no_source`);
    - the 10-min drift poll not existing.
  - `CLAUDE.md` gets a router line: "Soak harness, verdict, dispatch-soak → `.claude/rules/soak.md`".
  - `.claude/rules/e2e.md` gets one bullet on the soak step: after Playwright, member9 read-only, 600/120 s, the harness check.

  Commit: `ci(s7): soak client in the bundle and the e2e job; Windows clippy; coverage; rules (#10)`.

---

### Task 10: Ops repo: `soak.yml`, variables, runbook (private; main session)

You cannot see the ops repo from here. Model `soak.yml` on its `hil.yml` (S6 plan Task 13 Step 4): validated inputs, the App token from `vars.OPS_APP_ID` and `secrets.OPS_APP_KEY`, and a report job that posts the check run.

**Files (ops repo, `dev` branch, owner-merged PR):**
- `.github/workflows/soak.yml`;
- `docs/s6-pc-runbook.md` (a "Soak" section);
- repository variables `SOAK_MEMBER`, `SOAK_BASE`, `SOAK_CPU_SETS` (optional);
- secret `IEM_ENGINEER_PIN`.

- [ ] **Step 1: variables and secret** (`gh variable set … -R "$OPS_REPO"`, `gh secret set IEM_ENGINEER_PIN -R "$OPS_REPO"` with the value from `$PRIV`, never typed into a file of either repo):
  - `SOAK_MEMBER`: a member id whose mix the harness listens to. Prefer a member not active in dev: the solo grace and the Listen effects of Task 1 Step 3.
  - `SOAK_BASE`: the server's loopback URL on the PC. UNVERIFIED: the HTTP port the PC's server binds (P9: the band's usual address); read the ops `site.toml`.
  - `SOAK_CPU_SETS`: the S1c profile's housekeeping CPUs as CPU Set ids (comma list), or unset. UNVERIFIED: how an LP maps to a CPU Set id on the PC (commonly 256 + LP). Read it through the tuning module's state.
  - UNVERIFIED: the ops site's `lan_url` scheme must be `http://`. The client refuses `https` (`not-http`) because LAN 443 serves an expired certificate.
- [ ] **Step 2: `soak.yml`.** Every input is read from `env:` and validated before use; pin every action to a full SHA with its tag comment:

```yaml
name: soak
on:
  workflow_dispatch:
    inputs:
      sha: { required: true, type: string }
      branch: { required: true, type: string }
      run: { required: true, type: string }
      hours: { required: true, type: string, default: "8" }
permissions:
  contents: read
concurrency:
  group: soak-iem-pc
  cancel-in-progress: false
jobs:
  verify:
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    outputs:
      ok: ${{ steps.v.outputs.ok }}
    steps:
      - name: Validate inputs
        env: { SHA: "${{ inputs.sha }}", BRANCH: "${{ inputs.branch }}", RUN: "${{ inputs.run }}", HOURS: "${{ inputs.hours }}" }
        run: |
          set -euo pipefail
          [[ "$SHA" =~ ^[0-9a-f]{40}$ ]] && [[ "$BRANCH" =~ ^(dev|main)$ ]] && [[ "$RUN" =~ ^[0-9]+$ ]] && [[ "$HOURS" =~ ^[1-9]$ ]]
      - id: app
        uses: actions/create-github-app-token@<sha> # <tag>
        with: { app-id: "${{ vars.OPS_APP_ID }}", private-key: "${{ secrets.OPS_APP_KEY }}", owner: zbynekdrlik, repositories: iemmixer }
      - id: v
        name: The run is a green push run of the SHA on the branch (P5)
        env: { GH_TOKEN: "${{ steps.app.outputs.token }}", SHA: "${{ inputs.sha }}", BRANCH: "${{ inputs.branch }}", RUN: "${{ inputs.run }}" }
        run: |
          set -euo pipefail
          got=$(gh api "repos/zbynekdrlik/iemmixer/actions/runs/$RUN" --jq '[.head_sha, .head_branch, .event, .conclusion] | join(" ")')
          [ "$got" = "$SHA $BRANCH push success" ] || { echo "::error::run $RUN is not a green push run of $SHA on $BRANCH"; exit 1; }
          echo "ok=yes" >> "$GITHUB_OUTPUT"
  soak:
    needs: verify
    if: needs.verify.outputs.ok == 'yes'
    runs-on: [self-hosted, iem-pc]
    timeout-minutes: 600
    permissions: {}
    steps:
      - name: Soak (iemmode status every 60 s, the client harness alongside)
        shell: powershell
        env:
          SHA: "${{ inputs.sha }}"
          HOURS: "${{ inputs.hours }}"
          SOAK_MEMBER: "${{ vars.SOAK_MEMBER }}"
          SOAK_BASE: "${{ vars.SOAK_BASE }}"
          SOAK_CPU_SETS: "${{ vars.SOAK_CPU_SETS }}"
          IEM_SOAK_PIN: "${{ secrets.IEM_ENGINEER_PIN }}"
        run: |
          $ErrorActionPreference = 'Stop'
          $result = Join-Path $PWD 'result.json'
          '{"conclusion":"failure","reason":"not-finished"}' | Set-Content -Encoding ascii -Path $result
          if ($env:SHA -notmatch '^[0-9a-f]{40}$' -or $env:HOURS -notmatch '^[1-9]$' -or $env:SOAK_MEMBER -notmatch '^[A-Za-z0-9_-]{1,64}$') { throw 'bad input' }
          $iemmode = Join-Path $env:LOCALAPPDATA 'iemmixer\bin\iemmode.exe'
          $client = Join-Path $env:LOCALAPPDATA "iemmixer\bundles\$env:SHA\iem-soakclient.exe"
          function Read-Status { $raw = & $iemmode status; [pscustomobject]@{ code = $LASTEXITCODE; line = (($raw | ForEach-Object { "$_" }) -join ' ') } }
          $s = Read-Status
          $st = $s.line | ConvertFrom-Json
          if ($st.mode -ne 'dev') { '{"conclusion":"cancelled","reason":"left-dev"}' | Set-Content -Encoding ascii -Path $result; exit 0 }
          if (-not $st.engine -or $st.engine.build -ne $env:SHA) { '{"conclusion":"failure","reason":"bundle-not-active"}' | Set-Content -Encoding ascii -Path $result; exit 1 }
          if (-not (Test-Path -LiteralPath $client)) { '{"conclusion":"failure","reason":"no-client"}' | Set-Content -Encoding ascii -Path $result; exit 1 }
          $base = if ($env:SOAK_BASE) { $env:SOAK_BASE } else { 'http://127.0.0.1' }
          $seconds = [int]$env:HOURS * 3600 + 180
          $cargs = @('--base', $base, '--member', $env:SOAK_MEMBER, '--expect-build', $env:SHA, '--seconds', "$seconds", '--out', (Join-Path $PWD 'soakclient.json'))
          if ($env:SOAK_CPU_SETS) { $cargs += @('--cpu-sets', $env:SOAK_CPU_SETS) }
          $p = Start-Process -FilePath $client -ArgumentList $cargs -NoNewWindow -PassThru
          $polls = Join-Path $PWD 'polls.jsonl'
          $end = (Get-Date).AddSeconds([int]$env:HOURS * 3600 + 120)
          while ((Get-Date) -lt $end -and -not $p.HasExited) {
            $s = Read-Status
            $t = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
            [IO.File]::AppendAllText($polls, ('{"t":' + $t + ',"exit":' + $s.code + ',"status":' + $s.line + '}') + "`n")
            $mode = ($s.line | ConvertFrom-Json).mode
            if ($mode -ne 'dev') { '{"conclusion":"cancelled","reason":"left-dev"}' | Set-Content -Encoding ascii -Path $result; exit 0 }
            Start-Sleep -Seconds 60
          }
          if (-not $p.WaitForExit(600000)) { '{"conclusion":"failure","reason":"harness-did-not-end"}' | Set-Content -Encoding ascii -Path $result; exit 1 }
          '{"conclusion":"success","reason":"finished"}' | Set-Content -Encoding ascii -Path $result
      - if: always()
        uses: actions/upload-artifact@<sha> # <tag>
        with:
          name: soak-record
          path: |
            result.json
            polls.jsonl
            soakclient.json
          if-no-files-found: warn
          retention-days: 30
  report:
    needs: [verify, soak]
    if: always() && needs.verify.outputs.ok == 'yes'
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    permissions: { contents: read, actions: read }
    steps:
      - uses: actions/checkout@<sha> # <tag>
        with: { repository: zbynekdrlik/iemmixer, ref: "${{ inputs.sha }}", path: iemmixer, persist-credentials: false }
      - name: The record (none when the runner stopped)
        env: { GH_TOKEN: "${{ github.token }}" }
        run: |
          set -euo pipefail
          mkdir -p rec
          gh run download "$GITHUB_RUN_ID" -R "$GITHUB_REPOSITORY" -n soak-record -D rec || echo "no record"
      - name: Verdict (no token in this step)
        env: { SHA: "${{ inputs.sha }}", HOURS: "${{ inputs.hours }}", PC: "${{ needs.soak.result }}" }
        run: |
          set -euo pipefail
          [[ "$SHA" =~ ^[0-9a-f]{40}$ ]] && [[ "$HOURS" =~ ^[1-9]$ ]]
          python3 iemmixer/scripts/iem-pc/soak_verdict.py report --pc "$PC" --dir rec --sha "$SHA" --hours "$HOURS" > verdict.json
          cat verdict.json
      - id: app
        uses: actions/create-github-app-token@<sha> # <tag>
        with: { app-id: "${{ vars.OPS_APP_ID }}", private-key: "${{ secrets.OPS_APP_KEY }}", owner: zbynekdrlik, repositories: iemmixer }
      - name: Post soak/iem-pc
        env: { GH_TOKEN: "${{ steps.app.outputs.token }}", SHA: "${{ inputs.sha }}" }
        run: |
          set -euo pipefail
          c=$(jq -r '.conclusion' verdict.json)
          case "$c" in success|failure|cancelled) ;; *) c=failure;; esac
          gh api "repos/zbynekdrlik/iemmixer/check-runs" -f name=soak/iem-pc -f head_sha="$SHA" -f status=completed \
            -f conclusion="$c" -f "output[title]=Soak" -f "output[summary]=$(jq -r '.summary' verdict.json | head -c 60000)"
```

  Notes:
  - The `soak` job holds one secret, the engineer PIN. This deviates from `hil.yml`'s secret-free `pc` job, as the design (§4) requires.
  - It changes no guard state: no `job-begin`, no install, no activate.
  - Its PowerShell holds no process-ending call; the client ends itself after `--seconds`, or `connection-lost` at the first close of either socket ("ide event"), and it starts only at a server whose `/api/version` names `--expect-build` (`wrong-server` when the predecessor app already answers). Both are the #10 decision of 2026-10-07 (`.claude/rules/soak.md`).
  - UNVERIFIED:
    - how GitHub concludes a job whose self-hosted runner gets Ctrl-Break from the guard mid-job, and whether the `if: always()` upload still runs. The report maps cancelled, a missing record and `left-dev` to `cancelled`, so neither outcome makes red. Confirm at the first "ide event" during a soak and record it on #10.
    - the PowerShell 5.1 console decoding of `iemmode status` output (`[Console]::OutputEncoding`). Only ASCII numbers and fields are read; non-ASCII alarm text may arrive mangled, which the verdict never reads.
- [ ] **Step 3: runbook.**
  - Add a "Soak" section to `docs/s6-pc-runbook.md`: how to dispatch, how to read the run's state, the record artifact, the cancelled cases, and re-running in the next dev window.
  - Drift: the guard's own check runs hourly (`DRIFT_EVERY`) and raises `tuning drift: …` alarms that every poll carries; the verdict counts them as information. The design's 10-minute drift poll needs a guard request that does not exist (`iemmode` has no tuning-state command); flag it on #10 for S7 part 3 or #15.
  - The DPC trace (`iempc trace`, #15) runs alongside when #15 has it. This plan adds nothing for it.

---

### Task 11: Push, CI green (main session)

- [ ] **Step 1: pre-push checks, then push.**

```bash
cd "$WORK" && git fetch origin && git merge --ff-only origin/dev && git status -sb
cargo fmt --all --check && python3 scripts/check_integrity.py && python3 scripts/check_engine_deps.py && python3 scripts/check_version.py
python3 -m unittest discover -s scripts -p 'test_*.py' 2>&1 | tail -1
python3 -m unittest discover -s scripts/iem-pc -p 'test_*.py' 2>&1 | tail -1
git push origin dev
```

- [ ] **Step 2: bounded foreground wait** (S6 Task 15 Step 2's loop: at most 53 × 10 s per Bash call).
  - Every job must be green, including `engine` (rtsan), `windows`, `bundle`, `e2e` (the soak step's summary is printed), `test` (coverage floor), `integrity`, `supply-chain` and `mutants-list`.
  - On failure: `gh run view "$RUN" -R "$REPO" --log-failed`, one fix commit, push, wait again.
- [ ] **Step 3:** resize the mutation `shard:` matrix if `mutants-list` asks. Post on #10:
  - the e2e soak summary numbers (frames, gaps, max gap, reconnects, meters);
  - the bench's histogram p99.9 beside the sorted one.

---

### Task 12: The first 8 h soak (dev time; main session)

**Precondition:** the owner's latest signal is "event skončil" and `EVENT-NOW` does not exist. On "ide event" at any moment, run `$P event` (it writes the flag first).

- [ ] **Step 1:** fetch, install and activate the green bundle, then enter dev (S6 flow):

```bash
$P fetch-bundle --sha <sha>
$P install --sha <sha>
$P activate --sha <sha>
$P dev
$P status
```

  `status` must show `engine.build == <sha>`, `interval_hist` present and `hist_top_us == 667`.
- [ ] **Step 2:** `$P dispatch-soak --sha <sha>`. Record the ops run id on #10.
- [ ] **Step 3:** read the ops run's state with bounded foreground polls from the main session (`gh run view <id> -R "$OPS_REPO" --json status,conclusion`, at most 9 min per Bash call, repeated until it is terminal). Post the posted `soak/iem-pc` summary (numbers only) on #10:
  - green gives #15 acceptance 1 (after the #15 tuning lane);
  - red: the first failing number goes to #10 with the next step;
  - cancelled: dispatch again in the next dev window (a new dev entry).

---

## UNVERIFIED (check before or while implementing)

1. `asio.rs` `owner_main`: the exact line that builds `Owner` from `Start` (thread `hists` through it). Windows-only; compiled in the `windows` and `bundle` jobs.
2. The `Cargo.lock` diff after adding `iem-soakclient`: only the new package entry is expected (Task 5 Step 1).
3. The ops site: `lan_url` scheme (`http`), the PC server's loopback port (`SOAK_BASE`), the housekeeping CPU Set ids.
4. GitHub's conclusion for a job whose self-hosted runner is stopped mid-job, and whether `always()` steps run (Task 10 Step 2).
5. The design's 10-minute drift poll and the DPC trace: not implementable with today's guard and iempc; flagged on #10.
6. `solo.rs`: the soak's page socket on the member's mix holds that mix's solo past the grace. Confirmed in the code (`SoloJanitor` counts any page connection on the mix); listed as a known effect, not changed.

### Critical Files for Implementation
- crates/iem-audio-io/src/hist.rs (new; with nullrt.rs and asio.rs, which record into it)
- crates/iem-guard/src/proto.rs
- crates/iem-guard/src/daemon.rs (with the new switch_log.rs)
- crates/iem-soakclient/src/net.rs (new; with lib.rs)
- scripts/iem-pc/soak_verdict.py (new; with iempc_soak.py)
---

## Part 3: switch timing (design §5, §10 item 3; #10 decision of 2026-10-07)

Tasks 13 and 14 are one lane each, in order. Task 15 is the main session's run. The Global Constraints above apply.

### Task 13: Guard: an unwound entry's record spans the entry and its unwind [lane, ~250 LoC]

**Why.** When a dev or live entry fails and unwinds, `back_to_event` runs a new switch. That switch's `begin` restarts the `Laps`, and the entry never reaches `finish`. So the entry's steps are dropped, and the in-ear silence (REAPER quit by the entry, REAPER back only at the unwind's handover) is in neither record. The decision is that the unwind's record carries the failed entry's steps as its prefix.

**Files:** `crates/iem-guard/src/switch_log.rs`, `crates/iem-guard/src/daemon.rs` (call sites only, #36), `crates/iem-guard/src/daemon/record_tests.rs`, `.claude/rules/guard.md`.

- [ ] **Step 1 (RED).**
  - `switch_log::tests::a_resumed_clock_keeps_the_steps_and_times_from_the_last_lap`: `Laps::resume` keeps the steps and the last instant; the next lap is timed from the failed step's lap.
  - `record_tests::an_unwound_dev_entry_records_its_steps_then_the_unwind`:
    - `FakePc` fails `EngineArm` (policy `Unwind`) on a dev entry from event;
    - `g.state.last_switch` has `from: Event`, `to: Event`, `ended_in: Event` and `unwound: Some(Dev)`;
    - `started` is the entry's start;
    - the steps are the entry's up to `EngineArm`, then the unwind's;
    - `silence_ms` runs from the entry's `ReaperSaveQuit` through the unwind's `ReaperHandover`.
  - `record_tests::a_crash_fallback_record_has_no_unwound_entry`: the watch's dev → event fallback is a switch of its own, so `unwound` is `None`.
  - `state` and `proto` round trips carry `unwound`, and an older record without it reads as `None`.
- [ ] **Step 2 (GREEN).**
  - `LastSwitch` gains `#[serde(default)] pub unwound: Option<Mode>`, the target of the entry this record unwinds. `Laps::resume(&mut self)` keeps `steps` and `last`.
  - `Guard` gains `unwinding: Option<(Mode, u64)>`, the failed entry's target and its `Switching.started`. `back_to_event` sets it from `g.state.switching` before it calls `run_switch`. `begin` calls `laps.resume()` instead of `laps.start(now)` when `unwinding` is set.
  - `finish` builds the record with `unwound` and the entry's `started`, then clears `unwinding`.
  - A pre-empted entry ("ide event" during dev) unwinds through the same path, so its record spans it too.
- [ ] **Step 3.** Add a "Switch record" sentence to `guard.md`. Commits `test(guard): [red] …` / `feat(guard): [green] … (#10)`.

### Task 14: `iempc switch-test` [lane, ~350 LoC; after Task 13 is green in CI]

**Files:** `scripts/iem-pc/iempc_switch.py` (new; `ip` passed in, never imported), `scripts/iem-pc/test_iempc_switch.py`, `scripts/iem-pc/iempc.py` (call sites only), `.claude/rules/guard.md`.

- **What it does (dev time only):**
  - Refuses unless:
    - the guard is in dev, not switching, with no HIL job;
    - EVENT-NOW does not exist;
    - the running engine is the active bundle.
  - Runs `iemmode event` directly through the guard (the pipe's own switch). It never writes EVENT-NOW: that flag is the owner's signal, and the test must not leave it behind.
  - Reads `last_switch` (dev → event) from the reply or from one `iemmode status`.
  - Then, unless EVENT-NOW appeared meanwhile (an "ide event" during the test wins: no dev leg, the PC stays in event), runs `iemmode dev` and reads `last_switch` (event → dev).
  - Emits one JSON object with both records' `silence_ms`, the step times and the handover time (from the first of `ReaperStart` / `ReaperHandover` through `AppHandover`).
  - Prints a verdict:
    - dev → event silence ≤ 60 000 ms;
    - handover ≤ 90 000 ms;
    - the dev leg `outcome` `done`.
  - A red verdict names the first failing number. It still ends in dev when it can; nothing is ever force-ended.
- **Tests:** on synthetic replies (`test_iempc.Base` fakes):
  - every refusal;
  - no EVENT-NOW written;
  - a flag before the dev leg means no `iemmode dev` and the run exits with the event path's code;
  - the verdict's three bounds, each on both sides;
  - the handover window's ends.

### Task 15: The switch-timing run (main session, dev time)

After Tasks 13 and 14 are on the PC:

1. `iempc switch-test`, three runs a few minutes apart. Post the numbers on #10.
2. Tighten the guard's 120 s handover bound (S6 §11) to the measured worst case plus margin. Make it its own small lane, with the numbers in the commit.

---

## Part 4: Listen probe and live E2E (design §6, §10 item 4; #10 decisions of 2026-10-07)

Tasks 16 to 24 are lanes and run one at a time, in order, unless a task says it is independent. Tasks 25 to 27 run in the main session, because they touch the ops repo, the pushes, CI waits and the PC. A subagent never touches the PC. The Global Constraints above apply, together with the additions below.

**Goal:**
- **Listen probe:** a HIL signal can carry its sine to the engineer's listen sockets that ask for it with `&hil=1`, and to nothing else.
- **Live specs:** `e2e/tests/live/` measures on the real PC, through the public host:
  - Listen at ± 1 Hz and ± 0.5 dB;
  - talkback at −8.4 ± 0.3 dB;
  - the 7 PENDING parity rows;
  - the #25 repeats.
- **Running it:** `iempc dispatch-live` runs the ops `live.yml`, and a pure verdict posts `live/iem-pc`.
- **The manifest:** after the first green run it loses its last PENDING row (`--cutover` passes).

**Architecture:**
- **Engine (lane 4a):**
  - `Cmd::HilTestSignal` gains `listen: bool`, which travels to `RtOp::HilTestSignal`.
  - The RT processor gets two more rings, the **probe taps**. They are allocated in `Processor::with_hil` and pushed only while a HIL signal with `listen` runs, at the cadence of each listened slot. They carry the spare outputs' own sample expression.
  - The normal taps keep their silent frames exactly as today.
  - The media pump frames the probe rings as new media streams **2** (`ENGINEER_PROBE`) and **3** (`MEMBER_PROBE`). The IEMF v1 header is unchanged, and the stream byte is the mark.
- **Guard (lane 4b):** `iemmode test-signal <input> <dbfs> <ttl> --listen` sets `Request::TestSignal.listen`. As today, it works only inside a begun HIL job.
- **Server (lane 4c):**
  - `MediaLink` routes streams 2 and 3 to a `probe` broadcast per slot, with Opus encoders of their own. Only a `/ws/audio` session opened with `&hil=1` subscribes to it.
  - A pure `ProbeGate` sends that session the probe frames in place of the silent ones while probe frames come.
  - The session is told the burst's edges with `AudioStatus` `probe` and `listening`.
- **Live specs (lanes 4d-1 to 4d-3):**
  - They run with their own Playwright config.
  - Every page socket goes through a runner-side relay. The relay opens one real socket per path and test, only after `/api/version` names the run's build, and never opens a second one.
  - The specs measure only inside a burst.
  - `live_verdict.py` is the ops report job's pure verdict.
- **PC side (lane 4f):**
  - `iem-soakclient token` mints the run's short-lived tokens on the PC.
  - `iempc dispatch-live` dispatches `live.yml`.
  - `dispatch-soak` and `switch-test` refuse while a live run of the dev entry may still run.
- **Ops (main session):** `live.yml` has the jobs `verify`, then `pc-begin`, then `browser` and `pc` in parallel, then `pc-end`, then `report`.

**Decisions this part takes. Record them on #10 with Task 16, before code. Item 1 is the main session's technical decision (no band PIN in GitHub; #10, 2026-10-08).**
1. **No band PIN in GitHub for the live run either.**
   - The design (§2 P6, §6) has PINs "from the ops run's environment". By the reasoning of the #10 decision of 2026-10-07, that would copy a band credential into GitHub, and a PIN change would turn every later run red.
   - Instead, `pc-begin` mints an engineer token and one member token on the PC with `iem-soakclient token`. The expiry is the run's length (3600 s). They are handed to `browser` in a private artifact of one-day retention, which `report` deletes.
   - Since nothing then logs in, the design's login-budget risk is gone.
   - The predecessor shares the secret, so the relay's build check and its no-reconnect rule apply as for the soak client.
2. **The probe mark is the media stream id.**
   - The tap rings carry bare interleaved f32 (`rt.rs:40`, `rt.rs:211-240`). The 79-tap decimator spreads samples across frame edges (`media.rs` `TapFramer`). So a mark inside the same ring could not keep the edge frames exact.
   - Separate probe rings and streams 2 and 3 keep the normal taps byte-for-byte what they are today.
3. **The PC job is split into `pc-begin`, `pc` and `pc-end`.**
   - The engine gets `--test-signal` only from a restart inside the HIL job (`activate` then `restart_in_job`), and that restarts the server too. So the browser may start only after `pc-begin` has finished.
   - `pc-end` (`if: always()`) ends the guard's job and reads the PC evidence even after a failed `pc`.
4. **Bursts run at −20 dBFS (the HIL ceiling, `daemon.rs:52`), not −30.**
   - The limiter's range is −6…0 dB (`params.rs:23`).
   - Per-mix levers alone reach −30 + 12 (level, `params.rs:15`) + 12 (mix EQ, `params.rs:20`) = −6 dBFS, which never limits; −20 reaches +4 dBFS.
   - The limiter row is driven by the burst's sine, not by a talkback step: Chromium's AGC makes the talkback level unknown (`talkback.js:47-55`), and holding Talk longer widens the window in which a switch could meet an open talkback.
5. **The burst signal is explicit.** The server sends a `&hil=1` session `AudioStatus{status:"probe"}` when it starts forwarding probe frames and `"listening"` when the normal frames resume. The UI ignores an unknown status (`audio_player.rs:316-323`).
6. **"Listen within 3 s" (row 570) is measured on the probe.**
   - In dev time the band does not play, and no input level may stand for a signal (#38).
   - The only deterministic source is the burst's sine, played through the real Listen button and player.

### Additions to the Global Constraints (Parts 4 and 5)

- **Size budgets (#36):**
  - Over budget, so they get call sites only: `daemon.rs`, `daemon/tests.rs`, `iempc.py`, `test_iempc.py`, `IemPc.psm1`, `msg.rs`, `control.rs`.
  - Also above 1000 lines and treated the same way: `rt.rs` (1244), `core.rs` (1098), `core_tests.rs` (1237), `engine.rs` (1226), `pc.rs` (2128), `routes.rs` (1577), `login_guard.rs` (1036), `Test-IemPc.ps1` (1263), `proto.rs` (980, close).
  - New logic goes into new modules: `probe.rs`, `probe_gate.rs`, `peer_route.rs`, `mint.rs`, `iempc_live.py`, `live_verdict.py`, `IemHil.psm1`.
  - New tests go into new files: `rt_tests_probe.rs`, `msg/s7_tests.rs`, `control/tests/s7.rs`, `daemon/probe_tests.rs`, `test_iempc_live.py`, `test_live_verdict.py`, `Test-IemHil.ps1`.
- **The owner's rule (#9 2026-09-28):**
  - The sine reaches the spare outputs and the probe streams only.
  - Every mix TX stays zero while any HIL signal runs (unchanged; proven again with `listen: true` in `parity.rs`).
  - The normal taps stay silent during it (unchanged).
  - Probe streams reach only `&hil=1` sockets, and `/ws/audio` is engineer-only (`listen_ws.rs:40-49`).
  - A live spec changes a mix or an input only inside a burst, and restores it before the burst ends, checked against the server's `listening` status.
- **No reconnect in live specs (the #10 decision of 2026-10-07, applied to the browser):**
  - Every page socket goes through `relaySockets`, which opens at most one real socket per path and test, after `/api/version` at the public host names `LIVE_SHA`.
  - A server-side close fails the test.
  - Runner-side sockets (`BurstWatch`, `PageSocket`) follow the same rule.
- **P6 for live:**
  - Specs read every site value from `LIVE_*` variables, lazily, so `--list` works without them.
  - The live config switches off trace, screenshot and video, because a trace would hold tokens and the host.
  - No thrown error carries a URL with its query.
  - The verdict prints fixed codes, numbers from known annotation keys and spec titles (public, from this repo), never a test's error text.
- **Additive both ways:**

| Change | An older peer |
|---|---|
| `HilTestSignal.listen` | An older engine ignores it (no `deny_unknown_fields`): no probe, so the spec fails "no probe within 3 min", which is safe. |
| Media streams 2 and 3 | An older server drops them (`listen_frame`'s `_ => return`, server `engine/media.rs:223-227`). |
| `Request::TestSignal.listen` | An older guard ignores it (a plain burst). |
| `WsQuery.hil` | An older server ignores it (silence). |
| `AudioStatus "probe"` | The UI ignores it. |
| Part 5's `Status` and `EngineStatus` fields | Default to 0 or null. |

### Review Focus (Part 4)

1. **TX zero and silent taps with `listen`.** Tests:
   - `rt_tests_probe::{every_mix_tx_is_zero_during_a_probe, the_listen_taps_stay_exactly_silent_during_a_probe}`;
   - `parity.rs::a_listen_probe_keeps_every_mix_tx_zero_and_its_taps_do_not_depend_on_the_block_size`.
2. **I7.**
   - Expected: `probe::push_probe` is an index loop and one `push_partial_slice` into a ring allocated in `with_hil`.
   - Tests: `tests/rt.rs::process_does_not_allocate` and `tests/rtsan.rs::process_is_realtime_safe` run the probe through `common::scenario` (`listen: true`, both slots listened) and assert the probe rings got samples.
3. **Routing.**
   - Expected: a session without `&hil=1` never holds a probe receiver.
   - Tests:
     - `listen_ws::tests::a_socket_without_hil_holds_no_probe_feed`;
     - `engine::media::tests::probe_frames_reach_only_the_probe_channel_of_their_slot`;
     - the NullRt test `a_listener_without_hil_gets_only_silence_while_a_probe_runs`.
4. **Relay.**
   - Expected: one real socket per path and test; a build check before each open; a second attempt is refused and never reaches the server.
   - Tests: `relay.ts` is exercised by every live spec, and `tone.spec.ts` (mock CI) tests the pure estimators.
5. **Burst-only actions.** Every changing step in `talkback`, `limiter` and `meters` is guarded by `watch.inBurst()` and restores in `finally`.
6. **Verdict.**
   - Expected: cancelled for `left-dev` and `not-free`; red names the first failing check; the summary holds only codes, numbers and titles.
   - Tests: `test_live_verdict.py`.
7. **Dispatch guards.**
   - Expected: the soak guards plus "no soak of this entry may still run", and the reverse for `dispatch-soak` and `switch-test`.
   - Tests: `test_iempc_live.py`.

### File Structure (Part 4)

```
crates/iem-engine-proto/src/msg.rs              HilTestSignal.listen (field + doc; `mod s7_tests;`)
crates/iem-engine-proto/src/msg/s7_tests.rs     NEW listen additive; Part 5 adds the Status fields
crates/iem-engine-proto/src/media.rs            stream::ENGINEER_PROBE = 2, MEMBER_PROBE = 3 (+ test)
crates/iem-engine/src/cmd.rs                    RtOp::HilTestSignal.listen
crates/iem-engine/src/core.rs                   passes listen to start_test (call sites)
crates/iem-engine/src/probe.rs                  NEW push_probe (RT, mutated)
crates/iem-engine/src/rt.rs                     probe rings, TestRt.listen, push in render_mixes (call sites)
crates/iem-engine/src/rt_tests_probe.rs         NEW (included from rt_tests.rs beside `mod hil`)
crates/iem-engine/src/engine.rs                 media_pump drains the probe rings (call site)
crates/iem-engine/src/{control.rs,core_tests.rs,rt_tests_hil.rs}, examples/bench.rs, tests/{common/mod.rs,parity.rs,pipes.rs,rt.rs,rtsan.rs}   literals += listen; scenario listen: true
crates/iem-guard/src/{proto.rs,cli.rs,pc.rs,effects/engine.rs,win/engine.rs,win/mod.rs,daemon.rs}   listen pass-through
crates/iem-guard/src/effects/engine/tests.rs, crates/iem-guard/src/daemon/probe_tests.rs (NEW)
crates/iem-server/src/probe_gate.rs             NEW ProbeGate, PROBE_HOLD (pure, mutated)
crates/iem-server/src/engine/media.rs           Route, route(), probe channels and encoders, subscribe_probe
crates/iem-server/src/listen_ws.rs              hil sessions, feeds(), probe/listening statuses
crates/iem-server/src/mixer_ws.rs               WsQuery.hil
crates/iem-server/src/engine/testkit.rs         EngineHarness::supervise
e2e/playwright.live.config.ts                   NEW
e2e/tests/live/support/{env.ts,relay.ts,burst.ts,tone.ts,audio.ts}   NEW
e2e/tests/live/{listen-probe,talkback,limiter,meters,tunnel,push,client-error}.spec.ts   NEW
e2e/tests/tone.spec.ts                          NEW (mock CI: the pure estimators)
e2e/tests/support/wav.ts                        NEW (toneWav moved out of talkback.spec.ts)
crates/iem-soakclient/src/{mint.rs (NEW), lib.rs, main.rs}
scripts/iem-pc/{live_verdict.py,test_live_verdict.py,iempc_live.py,test_iempc_live.py}   NEW
scripts/iem-pc/iempc.py                         cmd_dispatch_live; refuse_while_live in two wrappers (call sites)
.github/workflows/ci.yml                        live --list step; token mint check in the e2e soak step
docs/parity/gen1-tests.tsv                      Task 27 only, after the first green live/iem-pc
.claude/rules/{engine,server-engine,guard,e2e,soak}.md, .claude/rules/live.md (NEW), CLAUDE.md (router)
private: $OPS/.github/workflows/live.yml, ops variables, $OPS/docs/s6-pc-runbook.md (Live section)
```

---

### Task 16: Engine: the listen probe [lane 4a, ~570 LoC]

**Files:** as listed under File Structure (engine and proto lines), plus `.claude/rules/engine.md`.

- [ ] **Step 1 (RED): tests first.**
  - `msg/s7_tests.rs`, `the_listen_flag_of_the_hil_signal_is_additive`:
    - `{"op":"hil_test_signal","input":"mic1","hz":1000.0,"dbfs":-20.0,"ttl_s":30.0,"card_tx":[94,95]}` reads `listen: false`;
    - with `"listen":true` it reads `true`;
    - `listen: false` serialises without the key;
    - a local copy of today's variant fields (an older engine) reads `"listen":true` and ignores it.
  - `media.rs`, `the_probe_streams_are_2_and_3_beside_the_listen_taps`: the ids differ from 0, 1 and 16, and a stream-2 frame round-trips through `write_media`/`read_media` with the v1 header.
  - `rt_tests_probe.rs`, on `rig_hil(test site, [StartListen engineer, StartListen member1], TEST_FLAG, AT_ONCE, SPARE)` with `HilTestSignal{input mic1, hz 1000, dbfs -20, ttl_s 0.2, card_tx SPARE, listen: true}` at 0, run 0.3 s at 32:
    - `a_listen_probe_carries_the_spare_outputs_sine_on_both_probe_taps`:
      - every pair of `h.probes[0]` and `h.probes[1]` has `l == r`;
      - the left samples equal spare output 94's samples over the same span (as `f32`);
      - the peak after the 50 ms fade-in is within 1e-6 of 0.1.
    - `the_listen_taps_stay_exactly_silent_during_a_probe`: `h.taps[k]` drained during the signal are all `0.0`, and there are as many values as the probe tap's (lockstep cadence).
    - `every_mix_tx_is_zero_during_a_probe`: every topology TX channel is `0.0` from the signal's sample to its end (TTL plus the 50 ms fade).
    - `no_probe_without_listen_or_without_a_listened_slot`:
      - `listen: false` leaves both probe rings empty;
      - `listen: true` with only slot 0 listened leaves `probes[1]` empty.
    - `a_plain_test_signal_never_feeds_the_probe`: `StartTestSignal` leaves the probe rings empty, and the taps carry the capped mix (not 0).
    - `the_probe_taps_go_quiet_when_the_signal_ends`.
    - `the_listen_flag_travels_from_the_command_to_the_rt_op`:
      - `Core::apply` gives `RtOp::HilTestSignal{listen: true, ..}`;
      - without the test flag the answer is `Forbidden`, as today.
  - `tests/common/mod.rs`: the scenario's `HilTestSignal` gets `listen: true`, and both slots are listened while it runs.
    - `tests/rt.rs::process_does_not_allocate` and `tests/rtsan.rs::process_is_realtime_safe` gain `assert!(s.handles.probes.iter().any(|c| c.slots() > 0), "the probe path ran")`.
    - `examples/bench.rs`'s worst case gets `listen: true` and its listen slots (engine.md: new per-block work goes into the scenario and the bench).
  - `tests/parity.rs`, `a_listen_probe_keeps_every_mix_tx_zero_and_its_taps_do_not_depend_on_the_block_size`:
    - the schedule of `hil_outputs_do_not_depend_on_the_block_size` with `listen: true` and both slots listened;
    - at 32/64/97/256: every TX exactly 0.0 inside the signal, the normal taps exactly 0.0, and the probe taps bit-identical across block sizes.
  - `tests/pipes.rs`, `a_listen_probe_goes_out_on_the_probe_streams_and_the_taps_stay_silent`:
    - NullRt, `--test-signal`, the test site (hil_tx 94/95);
    - a supervisor client sends the signal with `listen: true` after the controller's two `StartListen`;
    - the media client reads stream 0 and 1 frames that are all zeros, and stream 2 and 3 frames whose peak is −20 ± 0.1 dBFS.

  Commit: `test(engine): [red] the listen probe: the HIL sine on probe taps of its own, the taps silent, every TX zero (#10)`.
- [ ] **Step 2 (GREEN): proto.** `msg.rs`, in `HilTestSignal`:

```rust
        /// S7, additive (design note §6): the listen probe. While the signal
        /// runs, the listen taps keep their silent frames and the probe streams
        /// (`media::stream::ENGINEER_PROBE`, `MEMBER_PROBE`) carry the spare
        /// outputs' sine, for the server's `&hil=1` listeners only. Every mix's
        /// TX stays zero. An older engine ignores it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        listen: bool,
```

  `media.rs` `stream`:

```rust
    /// The listen probe (S7): the HIL sine of the engineer slot / the member
    /// slot, for `&hil=1` listeners only; an older server drops both.
    pub const ENGINEER_PROBE: u8 = 2;
    pub const MEMBER_PROBE: u8 = 3;
```

- [ ] **Step 3 (GREEN): RT.**
  - `cmd.rs`: `RtOp::HilTestSignal { …, listen: bool }`.
  - `core.rs`: `Cmd::HilTestSignal { listen, .. }` leads to `start_test(.., Some(mask), *listen)`, and `StartTestSignal` passes `false`.
  - `rt.rs` changes (call sites):
    - `TestRt.listen`;
    - `Processor.probes: [Producer<f32>; 2]` and `RtHandles.probes: [Consumer<f32>; 2]`, both `RingBuffer::new(TAP_RING)` in `with_hil`, always allocated (2 × 154 KB);
    - in `render_mixes`, after each `push_tap(&mut taps[k], …, hil, …)`:

```rust
let probe = test.as_ref().is_some_and(|t| t.mask.is_some() && t.listen);
// … inside `if listen[k] == Some(m) { push_tap(…); … }`:
if probe {
    probe::push_probe(&mut probes[k], test_buf.get(..n).unwrap_or_default(),
                      fade_buf.get(..n).unwrap_or_default(), tap_buf, &status.tap_overruns);
}
```

  - `probe.rs` (new, mutated):

```rust
//! The listen probe (S7 design note §6): while a HIL signal with `listen`
//! runs, each listened slot's probe tap carries the spare outputs' own
//! samples (`render_hil`: the sine clamped to `TEST_CAP`, times the output
//! fade), stereo, at the slot's cadence. RT: no allocation (I7).

pub fn push_probe(p: &mut Producer<f32>, sine: &[f64], fade: &[f64], scratch: &mut [f32], overruns: &AtomicU64) {
    let mut used = 0;
    for (pair, (s, f)) in scratch.as_chunks_mut::<2>().0.iter_mut().zip(sine.iter().zip(fade)) {
        let y = (s.clamp(-TEST_CAP, TEST_CAP) * f) as f32;
        *pair = [y, y];
        used += 2;
    }
    let (_, rest) = p.push_partial_slice(scratch.get(..used).unwrap_or_default());
    if !rest.is_empty() {
        overruns.fetch_add(1, Ordering::Relaxed);
    }
}
```

  - `engine.rs` `media_pump(taps, probes, conns, stop)`:
    - framers `[ENGINEER_LISTEN, MEMBER_LISTEN, ENGINEER_PROBE, MEMBER_PROBE]`;
    - drained as `taps.iter_mut().chain(probes.iter_mut())`, the normal taps first.
  - Known effect: a partial probe frame left at a burst's end opens the next burst's first probe frame. It is the same sine at the same level, and the spec measures well inside a burst.
  - Update every `Cmd::HilTestSignal` and `RtOp::HilTestSignal` literal (`listen: false` unless a test says otherwise): `control.rs`, `core_tests.rs`, `rt_tests_hil.rs`, `msg.rs` tests, `tests/{common,parity}`, `bench.rs`.

  Commit: `feat(engine): [green] the listen probe on probe streams 2 and 3; the taps and every TX as before (#10)`.
- [ ] **Step 4: rule.** In `engine.md` "Supervisor, hold, HIL mask", add a sentence: `listen` enables the probe rings and streams; every TX zero and the taps silent stay as they are; name the proofs.

  Local check: `cargo fmt --all --check`.

---

### Task 17: Guard and iemmode: `test-signal --listen` [lane 4b, ~200 LoC; after Task 16]

**Files:** `crates/iem-guard/src/{proto.rs,cli.rs,pc.rs,effects/engine.rs,effects/engine/tests.rs,win/engine.rs,win/mod.rs,daemon.rs}`, `crates/iem-guard/src/daemon/probe_tests.rs` (new), `.claude/rules/guard.md`.

- [ ] **Step 1 (RED).**
  - `cli.rs`:
    - `ask(&["test-signal","mic1","-20","30","--listen"])` gives `TestSignal{listen: true}`;
    - without `--listen` it gives `listen: false`;
    - `["test-signal","--listen","mic1","-20","30"]` and `["test-signal","mic1","-20","30","--loud"]` are errors.
  - `proto.rs`:
    - `every_request()` gains a `TestSignal{listen: true}`;
    - `requests_are_tagged_by_cmd_in_snake_case` pins `{"cmd":"test_signal","input":"mic1","dbfs":-20.0,"ttl_s":30.0,"listen":true}`, and the `listen: false` JSON stays as it is today.
  - `effects/engine/tests.rs`: `the_listen_probe_adds_listen_to_the_hil_signal` (`"listen": true`). `the_hil_test_signal_names_its_card_outputs` stays byte-equal for `false`.
  - `daemon/probe_tests.rs` (`#[cfg(test)] mod probe_tests;` in `daemon.rs`):
    - `a_listen_probe_needs_a_begun_hil_job`: refused outside a job, in event and in live, as the plain signal;
    - `a_listen_probe_reaches_the_engine_with_listen`: `FakePc` records `(input, dbfs, ttl, tx, listen)`;
    - `a_plain_test_signal_sends_no_listen`.

  Commit: `test(guard): [red] iemmode test-signal --listen asks the engine for the listen probe, inside a HIL job only (#10)`.
- [ ] **Step 2 (GREEN).**
  - `Request::TestSignal { input, dbfs, ttl_s, #[serde(default, skip_serializing_if = "std::ops::Not::not")] listen: bool }`.
  - `cli.rs`: a trailing `--listen` is stripped before `exactly::<3>`, so the negative dB value never meets the flag parser. Update `IEMMODE_USAGE`.
  - `Pc::engine_hil_signal(.., listen: bool)` (`pc.rs` trait and `FakePc`, `win/mod.rs`, `win/engine.rs::hil_signal`).
  - `effects::engine::hil_test_signal(.., listen)` adds `"listen": true` only when true.
  - `daemon.rs` (call sites): `test_signal(pc, g, &input, dbfs, ttl_s, listen)`; its reply adds `"; listen probe"`.
  - `guard.md` HIL bullet: `--listen`, the job gate, and the probe going only to `&hil=1` sockets.

  Commit: `feat(guard): [green] test-signal --listen (#10)`.

---

### Task 18: Server: probe frames only to `&hil=1` sockets [lane 4c, ~480 LoC; after Task 16]

**Files:** `crates/iem-server/src/{probe_gate.rs (new), lib.rs, engine/media.rs, listen_ws.rs, mixer_ws.rs, engine/testkit.rs}`, `.claude/rules/server-engine.md`.

**The routing code points:**
- `engine/media.rs::listen_frame` (today `:222-249`): `route(h.stream)`.
- `Inner.probe: [broadcast::Sender<Bytes>; 2]` beside `listen` (`:128`).
- `run()`'s encoders become `[Option<ListenEncoder>; 4]`, indexed `slot + 2·probe`, so the probe never shares the normal stream's Opus state.
- `MediaLink::subscribe_probe(slot)`.
- `listen_ws.rs::ws_audio` (`:31-51`) reads `query.hil == Some(1)`.
- `session` (`:133-223`): `feeds()` on `ListenStart`, a probe arm, and `ProbeGate` on the listen arm.

- [ ] **Step 1 (RED).**
  - `probe_gate.rs` tests:
    - `a_listen_frame_passes_while_no_probe_came`;
    - `after_a_probe_frame_listen_frames_wait_probe_hold` (99 ms dropped, 100 ms passes);
    - `the_first_probe_frame_enters_a_burst_and_the_first_listen_frame_after_it_leaves_it`.
  - `engine/media.rs`:
    - `route_maps_0_1_to_the_listen_slots_2_3_to_the_probe_and_nothing_else` (16 and 4 give `None`);
    - `probe_frames_reach_only_the_probe_channel_of_their_slot`: `subscribe(0)` and `subscribe(1)` receivers get nothing for a stream 2 or 3 frame, and the diagnostics' counts and sequence gaps are unchanged by probe frames.
  - `listen_ws.rs`:
    - `a_socket_without_hil_holds_no_probe_feed`: `feeds(&link, 1, false).probe.is_none()`, and with `true` it is `Some`.
    - `mod live` (`EngineHarness::start_with(|c| c.flags.test_signal = true)`), `a_listener_without_hil_gets_only_silence_while_a_probe_runs_and_a_hil_listener_gets_the_tone`:
      - `start(&s, member1)`;
      - non-hil and hil feeds of slot 1;
      - `h.supervise(HilTestSignal{.., dbfs -20, ttl 1.0, listen: true})`;
      - collect for 1.5 s and decode with `opus::Decoder`;
      - every non-hil frame peaks below 1e-4, and the hil probe frames reach −20 ± 1 dBFS.
    - (`/ws/audio` is engineer-only, `listen_ws.rs:40-49`; "a member socket" is every socket without `&hil=1`, whichever mix it hears.)
  - `mixer_ws.rs`: `the_query_reads_hil` (`?token=t&hil=1` gives `Some(1)`, an absent one gives `None`).

  Commit: `test(server): [red] probe frames reach only a hil=1 listen socket; every other listener gets silence (#10)`.
- [ ] **Step 2 (GREEN).** `probe_gate.rs`:

```rust
//! The listen probe's gate for one `&hil=1` session (S7 design note §6):
//! probe frames go out as they come; the slot's own (silent) frames wait
//! until no probe frame came for `PROBE_HOLD`, so the player gets one stream.
pub const PROBE_HOLD: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
pub struct ProbeGate { last_probe: Option<Instant>, in_burst: bool }

pub enum Pass { Drop, Send, SendLeaving }

impl ProbeGate {
    /// A probe frame: send it; `true` when it opens a burst (tell the socket `probe`).
    pub fn probe(&mut self, now: Instant) -> bool { … }
    /// A listen frame: drop it inside the hold; the first one after a burst
    /// leaves it (tell the socket `listening`).
    pub fn listen(&mut self, now: Instant) -> Pass { … }
}
```

  - `engine/media.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route { pub slot: usize, pub probe: bool }
pub fn route(stream: u8) -> Option<Route> {
    match stream {
        stream::ENGINEER_LISTEN => Some(Route { slot: 0, probe: false }),
        stream::MEMBER_LISTEN => Some(Route { slot: 1, probe: false }),
        stream::ENGINEER_PROBE => Some(Route { slot: 0, probe: true }),
        stream::MEMBER_PROBE => Some(Route { slot: 1, probe: true }),
        _ => None,
    }
}
```

    `listen_frame` counts `Stats::frame` only for `!probe`, and sends to `probe[slot]` or `listen[slot]`.
  - `listen_ws.rs`:
    - `pub fn feeds(media: &MediaLink, slot: usize, hil: bool) -> Feeds { listen: media.subscribe(slot), probe: hil.then(|| media.subscribe_probe(slot)) }`;
    - `session(socket, state, hil)` keeps `probe_rx` and a `ProbeGate`;
    - a probe arm (`if probing`) sends `status("probe", target)` when `gate.probe(now)` is true, then the frame;
    - the listen arm follows `gate.listen(now)` and sends `status("listening", target)` on `SendLeaving`;
    - probe frames also reset `last_audio`;
    - the info log names `hil`.
  - `mixer_ws::WsQuery` gains `#[serde(default)] pub hil: Option<u8>`.
  - `testkit.rs`: `EngineHarness::supervise(&self, cmd: Cmd) -> Result<(), ErrorBody>`, a role-`Supervisor` connection in the pattern of `shutdown`, read until its reply.
  - `server-engine.md`: one bullet, "Listen probe (S7)".

  Commit: `feat(server): [green] the listen probe's frames only to hil=1 listen sockets, with probe and listening statuses (#10)`.

---

### Task 19: Live specs I: support, the tone estimate, the live config, the listen probe [lane 4d-1, ~560 LoC; after Task 18]

**Files:** `e2e/playwright.live.config.ts`, `e2e/tests/live/support/{env.ts,relay.ts,burst.ts,tone.ts,audio.ts}`, `e2e/tests/live/listen-probe.spec.ts`, `e2e/tests/tone.spec.ts`, `.github/workflows/ci.yml` (one step).

- [ ] **Step 1 (RED): `e2e/tests/tone.spec.ts`** (mock CI, no page use; it imports `./support/fixtures` as every spec does):
  - `the tone estimate reads 1 kHz at -20 dBFS within 0.01 Hz and 0.01 dB`;
  - `the tone estimate tells 1001 Hz from 1000 Hz`;
  - `a dropout inside the window is a gap`;
  - `silence reads -150 dBFS`;
  - `a CELT 20 ms stereo packet passes the TOC check and SILK or 10 ms or mono does not` (synthetic TOC bytes per RFC 6716 §3.1).

  Commit: `test(e2e): [red] the live specs' tone estimate and Opus TOC check (#10)`.
- [ ] **Step 2 (GREEN): support.**
  - `tone.ts`: `toneOf(x, rate) -> { hz, dbfs, gap }`:
    - frequency from rising zero crossings, linearly interpolated;
    - level as `20·log10(√2·rms)`;
    - a gap is ≥ 48 samples under 1e-5.
  - `burst.ts`: `celt20msStereo(packet)` checks config ≥ 16, `config % 4 == 3`, the s bit set, c = 0, and 2 to 1276 bytes.
  - `env.ts`: `live()` reads lazily and validates:
    - `LIVE_BASE_URL` (`^https://`);
    - `LIVE_SHA` (40 hex);
    - `LIVE_TOKENS` (a file of `{engineer, member}`);
    - `LIVE_MEMBER`, `LIVE_TEST_INPUT`, `LIVE_TALKBACK_INPUT` (`[A-Za-z0-9_-]{1,64}`);
    - `LIVE_BURST_DBFS` (a number ≤ −20).

    A refusal names the variable, never its value.

    `expectBuild(request)` requires `/api/version`'s `git_hash` to be ≥ 7 hex and a prefix of `LIVE_SHA` (the `Test-IemHilVersion` rule). Otherwise it fails with "the public host does not answer with the run's build (tunnel or server)", the failing hop.

    `openLive(page, who, path)` puts the token into `localStorage.iem_token` and goes to the page, as `openMixer` does without a login.
  - `relay.ts`:

```ts
/** Every page socket through the runner (#10 decision 2026-10-07, applied to the
 *  browser): one real socket per path and test, opened only after /api/version
 *  names LIVE_SHA; a second attempt is refused and never reaches the server (after
 *  "ide event" the predecessor answers at the same address with the same secret).
 *  `/ws/audio` gains `&hil=1` with `hil`. Errors name the path, never the URL. */
export async function relaySockets(page: Page, opts: { hil?: boolean } = {}): Promise<Relay> {
  const relay = new Relay();
  await page.routeWebSocket(/\/ws\//, async (route) => {
    const path = new URL(route.url()).pathname;
    if (relay.opened.has(path)) { relay.refused.push(path); route.close({ code: 1000 }); return; }
    relay.opened.add(path);
    await expectBuild();
    const real = new NodeWebSocket(path === "/ws/audio" && opts.hil ? `${route.url()}&hil=1` : route.url());
    const queue: (string | Buffer)[] = [];
    route.onMessage((m) => (real.readyState === NodeWebSocket.OPEN ? real.send(m) : queue.push(m)));
    real.on("open", () => queue.splice(0).forEach((m) => real.send(m)));
    real.on("message", (d, binary) => { relay.observe(path, d, binary); route.send(binary ? (d as Buffer) : d.toString()); });
    real.on("close", () => { relay.serverClosed.push(path); route.close({ code: 1000 }); });
    route.onClose(() => real.close());
  });
  return relay;
}
```

    (Playwright 1.58's `WebSocketRoute.connectToServer()` takes no URL, so the relay holds the real socket itself.)
  - `burst.ts`: `BurstWatch.open(token)` is a runner-side `/ws/audio?…&hil=1` socket. It opens after `expectBuild` and sends `ListenStart` for `engineer`. It records:
    - the `probe` and `listening` statuses with their times;
    - the binary frames inside bursts and how many fail `celt20msStereo`.

    `burst({ minLeftMs = 22_000, within = 180_000 })` resolves at a `probe` status. `inBurst()` is true from a `probe` status until `listening` or 28 s after it. A server close makes every later call throw: no reconnect.
  - `audio.ts`:
    - `ANALYSER_INIT` (`page.addInitScript`) wraps `AudioNode.prototype.connect`: the first node connected to an `AudioDestinationNode` (the player's limiter, `audio_player.js:63-64`) is also connected to an `AnalyserNode` with `fftSize` 32768, kept in `window.__live_analyser`.
    - `readTone(page)` returns `toneOf(getFloatTimeDomainData)` at the context's rate (48 kHz).
    - `TALK_INIT` wraps `AudioEncoder.prototype.encode` to push each frame's peak (`AudioData.copyTo` of plane 0) and `performance.now()` into `window.__live_talk_in`.
  - `playwright.live.config.ts`:
    - `testDir: "./tests/live"`, `workers: 1`, `retries: 0`;
    - `timeout: 240_000`, `globalTimeout: 40 * 60_000`;
    - `use: { baseURL: process.env.LIVE_BASE_URL, trace: "off", screenshot: "off", video: "off" }`;
    - reporters `list` and `["json", { outputFile: process.env.LIVE_RESULTS ?? "live-results.json" }]`;
    - one `chromium` project.

    `e2e/playwright.config.ts` keeps ignoring `**/live/**`.
- [ ] **Step 3: `listen-probe.spec.ts`.** Engineer-page allowances: the incognito Push pair, as in `listen.spec.ts`. Titles, exactly:
  - `Listen on the engineer page plays audio within 3 s on the real PC` (row 570):
    - `expectBuild`, then `BurstWatch.open`, then `relaySockets(page, { hil: true })`, then `openLive(page, "engineer")`;
    - `await watch.burst()`, click Listen, then poll `__iem_audio_level() > -100` every 100 ms for at most 3 s;
    - annotation `first_audio_ms`.
  - `the listen probe plays 1 kHz at the burst level through the player within 1 Hz and 0.5 dB`:
    - `addInitScript(ANALYSER_INIT)`;
    - inside a burst, after 1 s of settling, three `readTone` windows 300 ms apart, each gap-free;
    - the median `|hz − 1000| ≤ 1` and `|dbfs − LIVE_BURST_DBFS| ≤ 0.5`;
    - annotations `listen_hz` and `listen_dbfs`.
  - `#25 repeat: every listen frame in a burst is CELT Opus, 20 ms, stereo, and decodes without error`:
    - the watch's burst frames ≥ 50 with 0 failing the TOC check;
    - `__iem_audio_error()` is null;
    - `GET /api/audio/diagnostics` shows `receiving_oiem` and `packets_per_second` ≥ 45;
    - annotation `opus_frames`.

    Numbers go out only as `test.info().annotations.push({ type: "live_number", description: "<key>=<number>" })`.
- [ ] **Step 4: CI.** In the `e2e` job, after "Playwright (mock E2E, clean console)":

```yaml
      - name: Live specs listed (they run only from the ops live run; no LIVE_* here)
        working-directory: e2e
        run: npx playwright test --config playwright.live.config.ts --list
```

  Commit: `feat(e2e): [green] live support (relay without reconnect, burst watch, tone estimate) and the listen probe specs (#10)`.

---

### Task 20: Live specs II: talkback, limiter, meters [lane 4d-2, ~330 LoC; after Task 19]

**Files:** `e2e/tests/support/wav.ts` (new; `toneWav` moved from `talkback.spec.ts` in its own commit `test(e2e): toneWav moves to support/wav.ts (#10)`), `e2e/tests/live/{talkback,limiter,meters}.spec.ts`.

Every changing step below runs only while `watch.inBurst()`, starts with at least 22 s of the burst left, and restores what `state()` / `console()` showed in a `finally`.

- [ ] **`talkback.spec.ts`.**
  - Top-level `test.use({ launchOptions: { args: [fake ui, fake device, --use-file-for-fake-audio-capture=<1 kHz, 0.5 WAV>] }, permissions: ["microphone"] })`.
  - The test input is `eng_mic` (from `LIVE_TALKBACK_INPUT`).
  - Titles:
    - `talkback reaches the ENG_MIC meter at -8.4 dB of its input within 0.3 dB during a burst`:
      - `addInitScript(TALK_INIT)`;
      - a runner-side `PageSocket` on the engineer page, opened after `expectBuild`;
      - `SetInput{trim_db: -150, processing: true, muted: false}` takes the card away, as in the mock spec;
      - the meter reads silence, then Talk is held (hover, `mouse.down`, the `live` class);
      - after 2.5 s of settling, 3 s of both series;
      - `median(meter dB) − median(input dB) = −8.405 ± 0.3`, with the input series steady within 0.2 dB, else red "talkback input not steady";
      - release at the latest 8 s after the press;
      - annotation `talkback_db`.
    - `held Talk reaches the ENG_MIC input continuously on the real PC` (row 731):
      - 50 meter frames while held, ≥ 40 above −60 dB;
      - below −60 dB within 600 ms of the release.

      The card output is not reached by design: every mix TX is zero in a burst.
- [ ] **`limiter.spec.ts`.**
  - The page is `LIVE_MEMBER`'s, with the engineer token, through the relay.
  - Title `the limiter counter counts while a burst drives the mix over its limit, and Reset zeros it` (row 611):
    - in that mix: `LIVE_TEST_INPUT`'s level +12 dB, the mix EQ band 1 at 1 kHz +12.04 dB, and the limiter on at −6 dB (−20 + 24 = +4 dBFS);
    - `limiterActiveSeconds` grows ≥ 1.5 s within 5 s;
    - restore the level and the EQ;
    - wait until two reads 500 ms apart agree (X14 lag, `e2e.md`);
    - click Reset; the counter reads 0;
    - restore the limiter;
    - annotation `limiter_active_s`.
- [ ] **`meters.spec.ts`.**
  - Title `#25 repeat: meters arrive about 10 times a second and the burst input reads the burst level on the real card`:
    - `SetInput{LIVE_TEST_INPUT, processing: false, muted: false}`, so the sine reaches the meter dry;
    - over 3 s, ≥ 27 frames, and the input's median peak within 0.5 dB of `LIVE_BURST_DBFS`;
    - annotations `meter_fps` and `burst_input_dbfs`.

  Commit: `feat(e2e): live talkback, limiter and meter specs, inside a burst only (#10)`.

---

### Task 21: Live specs III: tunnel ×2, push unsubscribe, client error marker; the live rule [lane 4d-3, ~350 LoC]

**Files:** `e2e/tests/live/{tunnel,push,client-error}.spec.ts`, `.claude/rules/live.md` (new), `.claude/rules/e2e.md`, `CLAUDE.md`.

- [ ] **`tunnel.spec.ts`** (rows 735 and 736):
  - `the engineer's header shows Vonkajší prístup: OK matching /api/tunnel through the real tunnel`;
  - `a member's page shows no tunnel banner while the real tunnel is Ok` (`LIVE_MEMBER` with its own token).

  Both use the mock spec's selectors.
- [ ] **`push.spec.ts`** (row 717), `logout revokes a real Web Push subscription in the browser and on the server`:
  - `chromium.launchPersistentContext(tmp)` (not incognito: the Push API exists) with `notifications` granted;
  - its own console guard with no allowance;
  - wait for `POST /api/push/subscribe` 200 and keep the endpoint from its body;
  - log out through the UI;
  - `POST /api/push/unsubscribe` answers 200 with that endpoint, and `pushManager.getSubscription()` is null;
  - in `finally`, when it is still subscribed, `POST /api/push/unsubscribe` with the endpoint and the token.

  `pc-end` compares the subscription counts as the backstop.
- [ ] **`client-error.spec.ts`** (row 555), `the marker panic report is accepted on the real PC`:
  - `POST /api/client-error` with `panic_message: iemmixer-live-marker-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}` answers 204;
  - `pc-end` finds it in the server's log, `<root>\logs\server.log` (`win/procs.rs:241-252`, `site.rs:425`), not in the tray's rolling log.
- [ ] **Rules.**
  - `live.md` (`paths: e2e/tests/live/**, e2e/playwright.live.config.ts, scripts/iem-pc/{live_verdict,iempc_live}.py`), covering:
    - where the specs run;
    - the `LIVE_*` variables;
    - the tokens (decision 1);
    - the relay and no-reconnect rule;
    - the burst protocol (−20 dBFS, 30 s every 60 s, `probe`/`listening`) and the burst-only rule;
    - each measurement's method;
    - the verdict's checks;
    - the manifest rows.
  - `e2e.md`: its last-but-one bullet ("Live specs … never in public CI") now says they live here and run only from ops `live.yml`, with public CI only listing them.
  - `CLAUDE.md`: the router line "Live E2E on the PC, live verdict, dispatch-live → `.claude/rules/live.md`".

  Commit: `feat(e2e): live tunnel, push and client-error specs; the live rule (#10)`.

---

### Task 22: `live_verdict.py` [lane, ~450 LoC; independent]

**Files:** `scripts/iem-pc/live_verdict.py`, `scripts/iem-pc/test_live_verdict.py` (stdlib).

- [ ] **Step 1 (RED).** Tests on a synthetic record directory:
  - `test_a_full_run_with_every_spec_passed_is_green`;
  - `test_a_failed_skipped_or_missing_spec_is_red_naming_the_first_title`;
  - `test_no_burst_or_a_refused_burst_is_red`;
  - `test_a_missing_or_unreadable_client_log_marker_is_red`;
  - `test_a_push_subscription_left_behind_is_red`;
  - `test_a_failed_job_end_is_red`;
  - `test_a_cancelled_pc_job_left_dev_or_not_free_is_cancelled`;
  - `test_another_pc_failure_names_its_reason_code`;
  - `test_numbers_come_only_from_known_keys_and_finite_values`;
  - `test_the_summary_never_holds_a_tests_error_text` (an error naming `https://mixer.example.org/ws/audio?token=x` never appears);
  - `test_expected_titles_are_read_from_the_spec_files` (the parity checker's `test(` regex);
  - `test_main_report_reads_the_record_and_prints_one_json`.

  Commit: `test(iem-pc): [red] the live verdict on synthetic records (#10)`.
- [ ] **Step 2 (GREEN).**

```python
BEGIN_REASONS = {"ready", "not-free", "left-dev", "bundle-not-active", "engine-not-up", "no-client", "token-failed", "push-count-failed"}
PC_REASONS = {"browser-done", "left-dev", "browser-never-started", "burst-refused", "jobs-unreadable", "not-finished"}
CANCELLED = {"left-dev", "not-free"}
NUMBER_KEYS = ("listen_hz", "listen_dbfs", "first_audio_ms", "talkback_db", "limiter_active_s", "meter_fps", "burst_input_dbfs", "opus_frames")
def report(results: dict | None, begin, pc, evidence, bursts: list, titles: list[str], pc_results: dict) -> dict:
    """{"conclusion","summary","first_failure","numbers"}; checks in order: begin ready; ≥ 1 burst and every
    burst exit 0; every expected title passed (status "expected"); client_log found; push_after ≤ push_before;
    job_end ok."""
```

  CLI: `live_verdict.py report --dir <record> --specs e2e/tests/live --begin <result> --pc <result> --browser <result> --end <result>`.

  The summary looks like:
  `green: 10 live specs, 7 bursts, listen 1000.02 Hz -20.08 dBFS, first audio 640 ms, talkback -8.39 dB, limiter 2.4 s, client log found, push 2 -> 2`.

  Commit: `feat(iem-pc): [green] the live verdict and its report mapping (#10)`.

---

### Task 23: `iem-soakclient token` [lane, ~220 LoC; independent]

**Files:** `crates/iem-soakclient/src/{mint.rs (new), lib.rs, main.rs}`, `.github/workflows/ci.yml` (the e2e soak step), `.claude/rules/soak.md`.

- [ ] **Step 1 (RED).** In `mint.rs` tests:
  - `a_member_token_reads_back_as_that_member_and_not_engineer` and `an_engineer_token_reads_back_as_the_engineer`, read back through the body of `extract_claims`, as `token.rs` does;
  - `the_arguments_refuse_a_member_with_engineer_and_engineer_without_it`;
  - `seconds_are_60_to_7200`;
  - `a_bad_sub_is_a_usage_error_naming_no_value`;
  - `the_token_alone_goes_to_stdout`.

  Commit: `test(soak): [red] iem-soakclient token mints a short-lived token on the PC (#10)`.
- [ ] **Step 2 (GREEN).**
  - `mint::token(secret, sub, engineer, now, seconds)`; `engineer_token` calls it with `seconds + TOKEN_MARGIN`.
  - `main.rs`: `token --jwt-secret-file <path> --sub <id> [--engineer] --seconds <n>`. It exits 2 on a usage error and 1 with `secret-unreadable`.
  - CI: in the e2e soak step, mint an engineer token and a `member9` token from the job's own secret. Then `GET /api/audio/diagnostics` answers 200 for the engineer token and is refused for the member token. Check the exact refusal status in `routes.rs`.
  - `soak.md`: the subcommand and its trust (the same as `--jwt-secret-file`).

  Commit: `feat(soak): [green] iem-soakclient token (#10)`.

---

### Task 24: `iempc dispatch-live` [lane, ~450 LoC; independent]

**Files:** `scripts/iem-pc/iempc_live.py`, `scripts/iem-pc/test_iempc_live.py`, `scripts/iem-pc/iempc.py` (call sites), `.claude/rules/guard.md`.

- [ ] **Step 1 (RED).** Tests on `test_iempc.Base`:
  - `test_a_live_run_is_dispatched_with_the_active_bundle_in_dev`: `gh workflow run live.yml -R <ops> -f sha -f branch -f run`, with `live.json` recorded;
  - `test_the_flag_refuses_before_any_call`;
  - `test_a_flag_during_the_gh_waits_stops_the_dispatch`;
  - `test_not_settled_another_bundle_or_engine_or_parked_is_refused` (reusing `iempc_soak.settled_refusal`, `active_bundle` and `playing_refusal`);
  - `test_a_sha_without_a_green_push_run_is_refused`;
  - `test_one_live_run_per_sha_per_dev_entry`;
  - `test_a_soak_of_this_entry_that_may_still_run_refuses` (`iempc_soak.running_soak`);
  - `test_dispatch_soak_and_switch_test_refuse_while_a_live_run_may_still_run`, which also passes after `WINDOW_S`;
  - `test_an_unreadable_live_record_counts_as_running`.

  Commit: `test(iem-pc): [red] dispatch-live, and soak/switch-test refuse while a live run may run (#10)`.
- [ ] **Step 2 (GREEN).**
  - `iempc_live.py`:
    - `LIVE_WORKFLOW = "live.yml"`, `RECORD = "live.json"`, and `WINDOW_S = 5400` (the jobs' bounds 15 + 60 + 10 + 5 minutes);
    - `live_refusal(reply, sha)`, `running_live(ip, entry, now)`, `refuse_while_live(ip)` and `dispatch(ctx, ip)`, in `iempc_soak.dispatch`'s shape.
  - `iempc.py`:
    - `cmd_dispatch_live` with `Spec(pc=True, dev_time=True, locked=True)` and a parser with `--sha`;
    - `cmd_dispatch_soak` and `cmd_switch_test` first call `iempc_live.refuse_while_live(sys.modules[__name__])`, a call site each.
  - `guard.md`, the dev-box bullet: `dispatch-live`.

  Commit: `feat(iem-pc): [green] iempc dispatch-live (#10)`.

---

### Task 25: Ops repo: `live.yml`, variables, runbook (private; main session)

This needs decision 1 recorded on #10 first. Model it on `hil.yml` and Task 10's `soak.yml`:
- validated inputs;
- every action pinned to a full SHA with its tag comment;
- App tokens from `vars.OPS_APP_ID` and `secrets.OPS_APP_KEY`;
- PowerShell 5.1 on the PC with no process-ending call;
- bounded waits.

The ops variables:
- `LIVE_BASE_URL` (`https://` plus the public host);
- `LIVE_MEMBER` (a member not active in dev);
- `LIVE_TEST_INPUT` (an input the burst replaces);
- `LIVE_TALKBACK_INPUT`;
- `LIVE_SERVER_CONFIG` (`IEMMIXER_CONFIG` for `notify --count`);
- `LIVE_SECRET_FILE` (the server's `secrets/jwt_secret`).

The bursts are fixed at −20 dBFS, 30 s, every 60 s. No secret is new.

```yaml
name: live
on: { workflow_dispatch: { inputs: { sha: …, branch: …, run: … } } }
permissions: { contents: read }
concurrency: { group: live-iem-pc, cancel-in-progress: false }
jobs:
  verify:   # ubuntu: inputs, App token, "a green push run of the SHA on the branch" (as soak.yml)
  pc-begin: # [self-hosted, iem-pc], timeout 15, permissions {}
    # status: dev, no switch, no HIL job, active bundle = engine build = SHA (else begin.json reason)
    # iemmode job-begin $GITHUB_RUN_ID (refused → not-free); iemmode activate $SHA (restart in job, HIL flags);
    # wait ≤ 60 s for engine build = SHA, frames 32, callbacks advancing (else engine-not-up)
    # push_before = iem-server notify --count alarm (IEMMIXER_CONFIG = vars.LIVE_SERVER_CONFIG)
    # tokens.json = { engineer: iem-soakclient token --sub engineer --engineer --seconds 3600,
    #                 member: … --sub $LIVE_MEMBER --seconds 3600 }   (never echoed)
    # upload live-begin (begin.json) and live-tokens (tokens.json, retention-days: 1)
  browser:  # ubuntu-24.04, needs pc-begin, if begin ready, timeout 45, permissions { contents: read }
    # checkout iemmixer at SHA (persist-credentials false); setup-node; npm ci; playwright install chromium
    # download live-tokens; ::add-mask:: each token; LIVE_* env; LIVE_RESULTS=results.json
    # npx playwright test -c playwright.live.config.ts; upload live-browser (results.json) always()
  pc:       # [self-hosted, iem-pc], needs pc-begin, if begin ready, timeout 60, permissions { actions: read }
    # every 10 s: GET /repos/$REPO/actions/runs/$RUN_ID/attempts/$ATTEMPT/jobs (Bearer github.token)
    #   browser in_progress and ≥ 60 s since the last burst → iemmode test-signal $LIVE_TEST_INPUT -20 30 --listen
    #   (one line per burst in bursts.jsonl); a status outside dev → left-dev (cancelled); browser completed → done;
    #   never started within 15 min → browser-never-started
    # upload live-pc (pc.json, bursts.jsonl) always()
  pc-end:   # [self-hosted, iem-pc], needs [pc-begin, browser, pc], if always() && pc-begin succeeded, timeout 10
    # iemmode job-end $GITHUB_RUN_ID; Select-String -SimpleMatch the marker in the tail (20000 lines) of
    # <root>\logs\server.log → found|missing|unreadable; push_after; upload live-end (evidence.json)
  report:   # ubuntu, needs all, if always() && verify ok, permissions { contents: read, actions: write }
    # checkout at SHA; download live-begin/live-pc/live-end/live-browser (never live-tokens);
    # python3 scripts/iem-pc/live_verdict.py report …; App token → check run live/iem-pc on the SHA;
    # delete the live-tokens artifact (gh api -X DELETE …/actions/artifacts/<id>)
```

Runbook: a "Live" section with dispatching, the cancelled cases, the tokens' life and `gh run rerun`.

---

### Task 26: Push, CI green (main session)

Use Task 11's steps. On top of the usual jobs, CI must show:
- the `e2e` job's new steps green (live `--list`, `tone.spec.ts`, the token mint);
- the `windows` job green.

Post on #10: the e2e job's soak numbers and the listed live titles.

---

### Task 27: The live run on the PC, then the manifest (main session, dev time)

**Precondition:** as Task 12. On "ide event", run `$P event`. A cut run is cancelled, never red.

1. Fetch, install and activate the green bundle, then `$P dev`. Then `$P dispatch-live --sha <sha>`, poll the ops run with bounded calls, and post `live/iem-pc`'s summary (numbers only) on #10.
2. On the first green run, flip the manifest in its own commit, `docs(parity): the seven PC halves pass on the IEM PC (#10)`. The message names no other ticket with `#`; the rows' reasons may.

| Line | gen1 test | Added to `gen2` (the CI half stays) | New status / reason |
|---|---|---|---|
| 555 | marker panic report reaches the rolling log file | `e2e/tests/live/client-error.spec.ts::the marker panic report is accepted on the real PC; scripts/iem-pc/test_live_verdict.py::test_a_missing_or_unreadable_client_log_marker_is_red` | TRANSFORMED: "CI half in #25; PC half: the live run's pc-end finds the marker in the server's log on the PC (S7, live/iem-pc green on <short sha>)" |
| 570 | Listen produces audio output within 3 seconds on engineer page | `e2e/tests/live/listen-probe.spec.ts::Listen on the engineer page plays audio within 3 s on the real PC` | TRANSFORMED: "… on the burst's sine (dev time has no band signal, #38)" |
| 611 | counter accumulates while limiter is reducing gain, Reset zeros it | `e2e/tests/live/limiter.spec.ts::the limiter counter counts while a burst drives the mix over its limit, and Reset zeros it` | TRANSFORMED |
| 717 | logout fires unsubscribe browser-side and server-side | `e2e/tests/live/push.spec.ts::logout revokes a real Web Push subscription in the browser and on the server` | TRANSFORMED |
| 731 | engineer talkback delivers continuous signal to ENGINEER mic track | `e2e/tests/live/talkback.spec.ts::held Talk reaches the ENG_MIC input continuously on the real PC` | TRANSFORMED: "… to the ENG_MIC input; no card output, every mix TX is zero in a burst (owner's rule)" |
| 735 | engineer header shows Vonkajší prístup: OK matching /api/tunnel | `e2e/tests/live/tunnel.spec.ts::the engineer's header shows Vonkajší prístup: OK matching /api/tunnel through the real tunnel` | TRANSFORMED |
| 736 | member page (member1) shows no tunnel banner while Ok | `e2e/tests/live/tunnel.spec.ts::a member's page shows no tunnel banner while the real tunnel is Ok` | TRANSFORMED |

   Rows 498 and 499 (OBSOLETE, "real-PC meter checks belong to S7 #10") also cite `e2e/tests/live/meters.spec.ts::#25 repeat: meters arrive about 10 times a second and the burst input reads the burst level on the real card`, and stay OBSOLETE.

   The checker must print:
   `parity manifest: 908 rows (PORTED 331, TRANSFORMED 354, OBSOLETE 223, PENDING 0, FEATURE-GAP 0); allowed until cutover: 0 (none)`.
   `--cutover` must exit 0. Run the denylist tree scan over the edit before committing.

---

## Part 5: HIL v2 (design §7; #10 decisions of 2026-10-07)

Tasks 28 to 31 are lanes, one at a time; 30 may share a lane with 29 if the diff stays at or below ~600 lines. Tasks 32 and 33 run in the main session.

`hil-v1.ps1` keeps its name, because it is `bundle::REQUIRED` and what the ops `hil.yml` calls. It grows six checks:
- `pipe-owner`;
- `tunnel-peer` and `lan-peer`;
- `reopen-time`;
- `fault-time`;
- `alarm-ack`;
- F30's byte restore.

**Where each number comes from today, and what is new:**

| Check | Today | New |
|---|---|---|
| First-instance flag | No Win32 call reads `FILE_FLAG_FIRST_PIPE_INSTANCE` back. The engine sets it through interprocess (`pipe/win.rs:18-31`), and HIL v1 reads only the DACL, `pipe_private` (`win/engine.rs:488-530`). | `EngineStatus.pipe_server_pid`: `GetNamedPipeServerProcessId` on the guard's supervisor connection, read once per engine like the DACL. HIL compares it with `EngineStatus.pid` (Task 3). That proves the flag's effect: the engine created the pipe and serves the guard's instance. |
| Tunnel peer | `login_guard::ClientKey::from_request` (`login_guard.rs:113-133`) classifies a request (`Tunnel` only from a host peer carrying `CF-Connecting-IP`), but no route reports it. HIL v1 checks only `/api/version` at the public host. | `GET /api/peer` answers `{"origin":"tunnel"\|"lan","peer":"loopback"\|"host"\|"other"}` (fixed codes, no address). |
| Forced reopen's duration (≈ 100 ms) | The owner logs only the open part ("open N ready after X ms", `asio.rs:1281-1287`), in the log. `resets` counts reopens (`asio.rs:1798`). S1a measured ≈ 104 ms per reopen at 64 samples. | `StreamStats.last_reopen_us`, from `Owner::reopen`'s entry (before `finish` stops the old stream) to the new stream's measured period. It goes into the engine's `Status.last_reopen_us`, the guard's `pc::Status`, and `EngineStatus.last_reopen_us`. |
| Fault callback's time (< 1 ms) | The faulting callback's time enters only `max_process_ns` and `process_hist` (`asio.rs:935-937`, `nullrt.rs:196-201`) of an engine that exits 70 at once. `tick` (`control.rs:727-729`) goes to `fault()` (`:650-656`) with no `Status` between. | `StreamStats.fault_callback_ns` (ASIO `Backend::on_buffer`, NullRt `pace`). The engine sends a last `Status` with `fault_callback_us` before `fault()`. The guard keeps `EngineStatus.last_fault_us` across the respawn. |
| Alarm test | `alarm_test` (`daemon.rs:1884-1894`) raises `"alarm test (iemmode alarm-test)"` and never acknowledges it. | Nothing in Rust. HIL finds its own alarm in the reply's `alarms` and acknowledges it with `iemmode alarm-ack <id>`. |

### Review Focus (Part 5)

1. **Every new field is additive.** An older engine's or guard's field reads as 0 or null, and HIL v2 then fails that check with "lacks '<field>'".
2. **`last_fault_us` survives the respawn.** Tests: `effects::engine::tests::the_last_fault_is_a_faulted_status_callback_time`, the NullRt `tests/pipes.rs` fault test, and the PowerShell fake with `fault_us`.
3. **`/api/peer` answers fixed codes.** A foreign peer with a forged header is `lan`/`other`.
4. **`alarm-ack`.** It never acknowledges an alarm other than the exact test text with no step and no owner question, and it acknowledges only after `alarm-push` passed.
5. **F30.** The installed site's sha256 after the revert equals the one before the change.

---

### Task 28: Engine: the reopen's time and the faulting callback's time [lane 5a, ~350 LoC]

**Files:** `crates/iem-audio-io/src/{lib.rs,owner.rs,nullrt.rs,asio.rs}`, `crates/iem-engine-proto/src/{msg.rs, msg/s7_tests.rs}`, `crates/iem-engine/src/control.rs` (call sites), `crates/iem-engine/src/control/tests/s7.rs` (new, `mod s7;` inside `control.rs`'s `mod tests`), `crates/iem-engine/tests/pipes.rs`, `.claude/rules/engine.md`.

- [ ] **Step 1 (RED).**
  - `owner.rs`: `a_reopen_time_is_whole_microseconds_never_zero_and_saturates`.
  - `nullrt.rs`: `a_panic_records_the_faulting_callbacks_own_time` (the `nullrt_panic_marks_the_stream_faulted_and_stops_calling` processor; `0 < fault_callback_ns < 1 s`; 0 before the panic).
  - `msg/s7_tests.rs`: `the_reopen_and_fault_times_are_additive` (an old `{"callbacks":4}` reads 0 and 0.0).
  - `control/tests/s7.rs`:
    - `a_fault_sends_a_last_status_with_the_faulting_callbacks_time`: a `scripted` driver whose stats are faulted with `fault_callback_ns` 412_000; the broadcasts end `Status{faulted: true, fault_callback_us: 412.0}`, then the fault's `Saved`, the `Fault` alarm, then `DriverReleased`.
    - `status_carries_the_last_reopen`.
  - `tests/pipes.rs`, `an_injected_fault_ends_with_a_status_that_times_the_faulting_callback`: NullRt with `--fault-injection`, a supervisor `InjectFault`; the last `Status` before `DriverReleased` has `faulted` and `0 < fault_callback_us < 1e6`.

  Commit: `test(engine): [red] the forced reopen's time and the faulting callback's time in Status (#10)`.
- [ ] **Step 2 (GREEN).**
  - `lib.rs` `StreamStats`: `last_reopen_us: u64` and `fault_callback_ns: u64`, each with a doc.
  - `owner::reopen_us(took: Duration) -> u64`: `u64::try_from(took.as_micros()).unwrap_or(u64::MAX).max(1)`.
  - `asio.rs` (Windows, excluded from mutation): `Owner::reopen` takes `let began = Instant::now();` before `finish`; after `open(carry, false)` succeeds, `self.shared.reopen_us.store(owner::reopen_us(began.elapsed()), SeqCst)`.
  - `Backend::on_buffer`: `let was = self.faulted.load(Acquire);`, then after `took`:

```rust
if !was && self.faulted.load(Ordering::Acquire) {
    self.fault_ns.store(took.max(1), Ordering::Relaxed); // one store on the fault path (I7)
}
```

    `Owner::watch` copies `b.fault_ns` into `shared.fault_ns` when it sees `b.faulted`, and `stats()` reads both.
  - `nullrt.rs`: `s.fault_ns.store(ns.max(1), Release)` before `faulted.store(true)`.
  - `msg.rs` `Status` (fields only):

```rust
    /// S7 HIL v2, additive: the last driver reopen, from the old stream's
    /// stop to the new one's measured period, µs; 0 before any (NullRt: 0).
    pub last_reopen_us: u64,
    /// S7 HIL v2, additive: the faulting callback's own time, µs (its entry
    /// to its return, the caught panic included); 0 while not faulted.
    pub fault_callback_us: f64,
```

  - `control.rs` (call sites):
    - `status_msg` fills both;
    - `tick` becomes `if stats.faulted { let last = self.next_status(&stats); self.broadcast(&EngineMsg::Status(last)); return Some(self.fault(…)); }`.
  - `engine.md`: both fields; the last `Status` before a fault's `DriverReleased`.

  Commit: `feat(engine): [green] Status carries the last reopen's time and the faulting callback's time (#10)`.

---

### Task 29: Guard: the pipe's server, the reopen time and the last fault in `Reply.engine` [lane 5b, ~350 LoC; after Task 28]

**Files:** `crates/iem-win/src/pipe.rs`, `crates/iem-guard/src/{pc.rs,proto.rs,effects/engine.rs,effects/engine/tests.rs,win/engine.rs,win/mod.rs}`, `.claude/rules/guard.md`. `daemon.rs` is unchanged: `engine_status` copies from `EngineSeen`.

- [ ] **Step 1 (RED).**
  - `iem-win` (Windows job), `server_pid_names_the_listening_process`: with the existing `pair()`, `server_pid(client.as_handle()) == std::process::id()`. Off Windows it is `Unsupported`.
  - `effects/engine/tests.rs`:
    - `engine_messages_are_read_field_by_field` reads `last_reopen_us` 104000 and `fault_callback_us` 412.5;
    - `the_last_fault_is_a_faulted_status_callback_time` (`fault_time`: `Some` only when faulted, finite and > 0);
    - `the_reply_names_the_engine_by_its_commit` passes `pipe_server_pid`, `last_reopen_us` and `last_fault_us`.
  - `proto.rs`: extend `the_engine_carries_the_fields_hil_v1_reads`'s exact JSON with `"pipe_server_pid": 4242, "last_reopen_us": 104000, "last_fault_us": 412.5`. Add the three at their largest in `the_largest_reply_fits_a_frame`. Keep `proto.rs` under 1000 lines, else move these tests into `proto/tests_s7.rs`.

  Commit: `test(guard): [red] HIL v2's figures in Reply.engine: the pipe's server, the reopen time, the last fault (#10)`.
- [ ] **Step 2 (GREEN).**
  - `iem_win::pipe::server_pid(pipe: BorrowedHandle<'_>) -> io::Result<u32>` (`GetNamedPipeServerProcessId`; the `Win32_System_Pipes` feature is already on).
  - `pc::Status` gains `last_reopen_us` and `fault_callback_us`. `EngineSeen` gains `pipe_server_pid: Option<u32>` and `last_fault_us: Option<f64>`.
  - `proto::EngineStatus`: `pipe_server_pid: Option<u32>`, `last_reopen_us: u64`, `last_fault_us: Option<f64>`, each documented with its source.
  - `effects::engine`: `parse` fills them, `pub fn fault_time(s: &pc::Status) -> Option<f64>`, and `engine_status` copies them.
  - `win/engine.rs::seen` (Windows glue):
    - the per-pid cache `pc.dacl` becomes `(pid, private, server_pid)`, read with `Supervisor::server_pid()`;
    - `WinPc.last_fault: Option<f64>` is taken with `fault_time` from the inbox's newest status at every look;
    - before a closed supervisor is replaced, it is taken from the old one's inbox, so the respawn keeps it.
  - `FakePc::engine_seen` sets both new `EngineSeen` fields.
  - `guard.md`, the HIL bullet: the three fields and their sources. Delete "Not in HIL v1: …".

  Commit: `feat(guard): [green] Reply.engine carries the pipe's server pid, the last reopen time and the last fault's callback time (#10)`.

---

### Task 30: Server: `GET /api/peer` [lane 5c, ~200 LoC; independent; may share a lane with Task 29]

**Files:** `crates/iem-server/src/peer_route.rs` (new), `crates/iem-server/src/lib.rs` (`mod`), `login_guard.rs` (`LoginGuard::is_host`, one fn), `routes.rs` (one `.route` line), `.claude/rules/security-baseline.md`.

- [ ] **Step 1 (RED).** With `MockConnectInfo`, as in `auth.rs`'s tests:
  - `a_loopback_peer_with_cf_connecting_ip_is_the_tunnel`;
  - `the_hosts_own_address_with_the_header_is_the_tunnel` (`HostAddrs::new([10.0.0.10])`);
  - `a_loopback_peer_without_the_header_is_lan`;
  - `a_foreign_peer_with_a_forged_header_is_lan_and_other`;
  - `the_answer_has_exactly_origin_and_peer` (no address).

  Commit: `test(server): [red] /api/peer names how a request was classified, without an address (#10)`.
- [ ] **Step 2 (GREEN).**

```rust
//! `GET /api/peer` (S7 HIL v2, design §7 "the tunnel peer"): how this server
//! classified the request it answers, by `login_guard`'s rule: `origin`
//! "tunnel" when the socket peer is this host and the request carries
//! CF-Connecting-IP, else "lan"; `peer` "loopback" | "host" | "other". Fixed
//! codes only, no address (P6); public like /api/version.
pub async fn get_peer(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>,
                      headers: HeaderMap) -> Json<PeerInfo>
```

  `security-baseline.md` gets one sentence on it.

  Commit: `feat(server): [green] GET /api/peer (#10)`.

---

### Task 31: HIL v2 in `hil-v1.ps1` [lane 5d, ~550 LoC; after Tasks 29 and 30]

**Files:**
- `scripts/iem-pc/IemHil.psm1` (new);
- `scripts/iem-pc/hil-v1.ps1`;
- `scripts/iem-pc/Test-IemHil.ps1` (new);
- `scripts/iem-pc/Test-IemPc.ps1` (the end-to-end block moves out, plus a call site);
- `.github/workflows/ci.yml` (`bundle`: the `Copy-Item` adds `IemHil.psm1`; `bundle::REQUIRED`, `iempc.BUNDLE_REQUIRED` and `Test-IemBundleSums` are unchanged, so older bundles still install);
- `.claude/rules/guard.md`.

- [ ] **Step 0: move without changes.** The `hil-v1.ps1` end-to-end block of `Test-IemPc.ps1` (the fake `iemmode` through `$h10`) moves verbatim into `Test-IemHil.ps1`. `Test-IemPc.ps1` starts it in a process of its own, like `Test-IemTuningInstall.ps1`. CI green.

  Commit: `test(iem-pc): the hil-v1.ps1 end-to-end self-test moves to Test-IemHil.ps1 (#10)`.
- [ ] **Step 1 (RED).** In `Test-IemHil.ps1`, against IemHil.psm1's pure functions:
  - `hil-pipe-owner-the-engine-serves-its-pipe` (`pipe_server_pid == pid`, `pipe_private`); refused when they differ, either is null, or the DACL is not private.
  - `hil-peer-tunnel-from-loopback-or-host` and `hil-peer-refuses-lan-or-other-through-the-public-host`; `hil-lan-peer-is-lan`.
  - `hil-reopen-time-within-the-bound` (104 ms at a 200 ms bound passes; 0, missing, or 201 ms fails).
  - `hil-fault-time-under-1-ms` (412.5 µs passes; 0, null, or 1000 µs fails).
  - `hil-own-test-alarm-is-the-new-one` and `hil-stale-test-alarms-are-only-the-exact-text-unacked-without-step-or-question`.
  - `hil-test-alarm-text-is-the-guards`: `crates/iem-guard/src/daemon.rs` holds `"alarm test (iemmode alarm-test)"`.
  - `hil-site-restored-by-bytes`.

  The fake `iemmode` gains:
  - engine fields `pid`, `pipe_server_pid`, `last_reopen_us` and `last_fault_us`, the last after `inject-fault`;
  - `alarms` from the scenario;
  - `alarm-test` appends an alarm with the next id;
  - `alarm-ack <id>` is logged.

  End-to-end tests:
  - `hil-run-acks-its-own-test-alarm-and-the-stale-ones-never-another`: calls hold `alarm-ack 5` and `alarm-ack 3`, never `alarm-ack 4`;
  - `hil-run-acks-nothing-when-the-push-failed` (`refuse: ['alarm-test']`);
  - `hil-run-v2-checks-pass-on-good-figures`;
  - `hil-run-a-respawned-engine-without-last-fault-fails-fault-time`;
  - `hil-run-f30-without-the-installed-site-fails`.

  The failing set of `$h1` grows by `lan-peer` and `tunnel-peer` (the server is down).

  Commit: `test(iem-pc): [red] HIL v2: pipe owner, tunnel peer, reopen and fault times, the test alarm acknowledged, F30 restored (#10)`.
- [ ] **Step 2 (GREEN).**
  - `IemHil.psm1` (PowerShell 5.1, ASCII, `Set-StrictMode -Version Latest`, reads through `Get-IemProp`):
    - `$script:HilTestAlarmText = 'alarm test (iemmode alarm-test)'`;
    - `Test-IemHilPipeOwner`, `Test-IemHilPeer -Want tunnel|lan`, `Test-IemHilReopenTime -MaxMs`, `Test-IemHilFaultTime -MaxUs`;
    - `Get-IemHilMaxAlarmId`, `Get-IemHilTestAlarms -Reply -Above` (returns `own` and `stale`), `Test-IemHilSiteRestored`, `Test-IemHilV2Inputs`.
  - `hil-v1.ps1` (it imports `IemHil.psm1` beside `IemPc.psm1`):
    - parameters `-ReopenMaxMs 200` (S1a's 104 ms at 64, doubled; tighten after Task 33), `-FaultMaxUs 1000` and `-SiteInstalled ''`;
    - after `pipes`, `pipe-owner`;
    - after `public-host`, `tunnel-peer` (`<public>/api/peer`) and `lan-peer` (`<lan_url>/api/peer`);
    - after `reopen`, `reopen-time` (the waited `$after`);
    - after `panic`, `fault-time` (`$later`);
    - `alarm-test`: read the max id first; once `alarm-push` passed, `alarm-ack` its own id, then each stale one, as one check `alarm-ack` with numbers `{own, stale}`;
    - F30 needs `-SiteChange`, `-SiteRevert` and `-SiteInstalled` together; `Get-FileHash` of the installed file before the change and after the revert must be equal.
  - The doc header's "Not in HIL v1" paragraph goes. The summary text is left as it is ("HIL v1 …", `IemPc.psm1`: call sites only).
  - `guard.md`, the HIL bullet: v2's checks.

  Commit: `feat(iem-pc): [green] HIL v2 checks and the test alarm acknowledged (#10)`.

---

### Task 32: Ops `hil.yml`: F30 and the v2 inputs (private; main session)

The `pc` job, before `hil-v1.ps1`:
1. Read the installed site's path from the ops variable `HIL_SITE`.
2. Write `site-revert.toml` as its exact bytes.
3. Write `site-change.toml` as those bytes plus `"\n# hil f30 synthetic change, run <id>\n"`. A comment changes no topology, so no state entry can be dropped.

Then pass `-SiteChange`, `-SiteRevert` and `-SiteInstalled`. Run it in dev time from the ops `dev` branch, in an owner-merged PR, and add a runbook line.

### Task 33: Push, CI, the HIL v2 run (main session)

After CI is green, run `iempc dispatch-hil` on the SHA in dev time, then read `hil/iem-pc`. Post the numbers on #10:
- reopen ms;
- fault µs;
- pipe server = engine;
- the peer codes;
- the acknowledged test alarms (own and stale);
- F30 restored.

Then set `-ReopenMaxMs` to the measured worst plus margin in a small lane of its own, with the number in the commit.

### UNVERIFIED (Parts 4 and 5)

1. Chromium's Web Push subscribe on a hosted runner: a headless persistent context reaching FCM. If not, row 717 needs the owner's call; it is never skipped.
2. An `AnalyserNode` attached by wrapping `AudioNode.prototype.connect` in an init script, and the `AudioEncoder.prototype.encode` wrap, without a console warning.
3. The self-hosted runner's `github.token` (`actions: read`) reading the `browser` job of its own run attempt.
4. Login budgets on the public host: with decision 1 there is no login at all. If the owner keeps PINs, a successful login counts nothing (`login_guard` counts failures only). Still open either way: a Cloudflare bot or WAF challenge for GitHub-hosted IPs; `expectBuild` names that hop.
5. Chromium's audio processing (AGC, noise suppression) on the fake capture staying steady within 0.2 dB over 3 s, which the talkback reference needs.
6. Playwright's `WebSocketRoute.send(Buffer)` reaching the page's `arraybuffer` socket as binary.
7. Opus 128 kb/s CELT plus WebCodecs keeping a −20 dBFS sine within 0.5 dB. The server test asserts 1 dB for a 0.5 amplitude.
8. The stereo bit on every CELT frame of a dual-mono sine.
9. The reopen time at 32 samples. S1a measured ≈ 104 ms at 64.
10. PC paths:
    - `<root>\logs\server.log` under `%LOCALAPPDATA%\iemmixer`;
    - the installed site's path (`HIL_SITE`);
    - `IEMMIXER_CONFIG` for `notify --count`.
11. Whether `dev` entries re-import mix state, which would undo a spec's leftover change if a restore failed (`finally` restores either way).
