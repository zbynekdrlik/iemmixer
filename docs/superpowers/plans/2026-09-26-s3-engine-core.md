# iemmixer S3 — Engine Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Pushes and CI waits (Task 14) run in the main session, never in a subagent.

**Goal:** The iemmixer engine without the ASIO driver: the §3.1 graph compiled from `site.toml`, mixed on one RT thread at B = 32 / 96 kHz with the S2 kernels, revisioned state with checksummed persistence, control and media pipes, the driver-free crash model, and the proofs (impulse oracle, block-size invariance, `assert_no_alloc`, rtsan, CPU benchmark) in hosted CI (ticket #6, program #1).

**Architecture:** Three new crates. `iem-engine-proto` (MIT OR Apache-2.0) holds the typed protocol and stable ids; `iem-audio-io` (MIT OR Apache-2.0) the `Process` trait with the `Offline` and paced `NullRt` backends; `iem-engine` (GPL-3.0-or-later, links `iem-limiter-mga`) the site parser and graph compiler, the RT `Processor`, the pure control `Core`, persistence, the media path (taps, 96↔48 kHz half-band), pipes and the binary. The RT thread only sees `Copy` commands with sample timestamps (rtrb), writes meters into a triple buffer and taps into rings; everything else lives on control threads.

**Tech Stack:** Rust 1.98.1 (edition 2024); new runtime crates `rtrb` 0.4.0, `triple_buffer` 9.0.0 (MPL-2.0), `interprocess` 2.4.4; existing `serde`, `serde_json` (+ `raw_value`, `float_roundtrip`), `toml` 0.9, `sha2` 0.10, `tracing`, `tracing-subscriber`; dev: `assert_no_alloc` 1.1.2, `tempfile` 3; rtsan via rustc `-Zsanitizer=realtime` on a pinned nightly; `cargo-fuzz` for the parser target; GitHub Actions (hosted only).

**Spec:** `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md` (§2.2–2.5, §3.1, §3.3, §3.4, §3.5, §4.4); design note `docs/superpowers/specs/2026-09-26-s3-engine-core-design.md`; hand-offs on #6 (S0: rt-safety job, fuzz job + nightly shard, engine dependency allowlist; S2: rtsan, parser fuzzing, Q1/X3/X4 nodes).

Detail sources (private, never committed): `05-fact-engine-api-migration.md` (API sketch, domain model), `07-synthesis.md` §2.3–2.5, `12-program-spec.md`.

## Global Constraints

- **Tier 0:** no local cargo compilation. Locally only `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p`, Python. Every Rust claim is proven in hosted CI (Task 14): one push per cycle, one fix commit per failing cycle, foreground bounded waits (≤ 9 min per call), never `run_in_background`.
- **I2:** 96 kHz only (the engine refuses other rates); the RT budget is the 333 µs period at B = 32.
- **I5:** f64, plain summing, no bus pan law, zero added latency.
- **I7:** `process()` does no allocation, lock, syscall or log; ≤ 512 commands per block; library code in the three crates has `forbid(unsafe_code)` and denies `clippy::indexing_slicing`, `unwrap_used`, `expect_used`, `panic` (the one deliberate fault-injection panic carries a scoped `allow` with its reason).
- **I4:** the graph is compiled once per run from `site.toml` and validated (acyclic, unique TX, channels in map, one send per pair).
- **I6:** the engine is the single writer; `State`/`Delta` carry `rev`; a gap means `GetState`.
- **I8:** no force-kill anywhere; the engine stops only on `Shutdown` or a fault.
- **Licences (D1):** `iem-engine` is GPL-3.0-or-later (cargo-deny exception); proto and audio-io stay MIT OR Apache-2.0 and never depend on the limiter.
- **P6:** synthetic ids and channels only (`test-site.toml`: RX 101–132, member TX 71–88, engineer 91/92, master 89/90, translator 93). No site value in code, docs, tests or commits.
- **Tests:** every feature ships tests that can fail; no `#[ignore]`, no skips; the three crates join coverage (floor never drops); blocking waits in tests are bounded (socket read timeouts ≤ 5 s); diff-scoped mutation ≤ 20 min per shard, the matrix sized from `mutants-list`.
- **Branches and identity:** `dev` only; no PR or merge. Noreply identity; every commit carries `Refs #6` and `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **The IEM PC is not touched in S3.**
- **Durable state:** decisions and findings go on #6 the moment they land.

## Review Focus

1. **Hostile or buggy clients** (NaN, 1e308, 10 kB ids, 10 000-op batches, unknown commands, truncated frames): the engine replies with a typed error or clamps, never panics, never allocates on the RT side — `props.rs::random_requests_never_panic_and_keep_state_in_caps`, `proto` frame tests, `pipes.rs::oversized_frame_closes_only_that_connection`.
2. **Corrupted or foreign state files** (bad checksum, truncated JSON, newer schema with extra fields, ids the topology no longer has): the load chain falls through to the next file and reports why; unknown fields are ignored — `persist::tests::*`.
3. **A panic on the RT thread** mid-block: that block's outputs are zero, the processor is never called again, the engine saves and exits non-zero with `DriverReleased` — `audio-io` `offline_panic_zeroes_the_block_and_stops`, `nullrt_panic_*`, `pipes.rs::fault_injection_releases_the_driver_and_exits`.
4. **Server restart while soloed or listening:** a new controller supersedes the old one, solos survive a reconnect within 10 s and clear after it — `control::tests::solo_*`, `pipes.rs::a_new_controller_supersedes_the_old`.
5. **Commands landing inside a block:** results are independent of the block size (32/64/97/256) — `parity.rs::outputs_do_not_depend_on_the_block_size`.

## Shell variables (paste at the start of every task)

```bash
export WORK="$HOME/devel/iemmixer"
export WP="$HOME/.claude/work-products/iemmixer-gen2"
export PRIV="$HOME/.config/iemmixer"
export REPO=zbynekdrlik/iemmixer
```

## File Structure

```
Cargo.toml                                   + 3 members
Cargo.lock                                   + rtrb, triple_buffer, interprocess (+ their deps)
deny.toml                                    GPL exception also for iem-engine
config/test-site.toml                        + [engine] topology (§3.1 shape, synthetic channels)
crates/iem-core/src/config.rs                Config ignores the `engine` table
scripts/check_version.py (+test)             CRATES += 3
scripts/check_engine_deps.py (+test)         engine dependency allowlist (supply-chain)
scripts/engine-deps-allow.txt                the allowlist
.cargo/mutants.toml                          exclusions with reasons (bin entry, fuzz)
.github/workflows/ci.yml                     coverage, clippy, mutation lists, fuzz (props + cargo-fuzz), engine job (rtsan, bench, parity report), supply-chain, windows
.github/workflows/mutation-full.yml          package lists
.github/workflows/fuzz-nightly.yml           nightly cargo-fuzz shard
crates/iem-dsp/src/{pan,eq}.rs               StereoGain::steady, Equalizer::is_identity (+ tests)
crates/iem-engine-proto/{Cargo.toml,src/lib.rs,src/ids.rs,src/state.rs,src/msg.rs,src/frame.rs,src/media.rs}
crates/iem-audio-io/{Cargo.toml,src/lib.rs,src/offline.rs,src/nullrt.rs,src/wav.rs}
crates/iem-engine/Cargo.toml, LICENSE (GPL text copy)
crates/iem-engine/src/lib.rs                 constants, module list
crates/iem-engine/src/site.rs                [engine] table → Site, validation errors
crates/iem-engine/src/graph.rs               Site → Graph (order, send groups, reach), TopologyInfo
crates/iem-engine/src/params.rs              proto values → linear DSP parameters (shared by Core and Processor)
crates/iem-engine/src/core.rs                pure control core: caps, apply, solo, listen, test signal, import
crates/iem-engine/src/rt.rs                  RtCmd/RtOp, Processor (Process impl), meters, taps, talkback
crates/iem-engine/src/rt/nodes.rs            input, bus, send, TX-stage node state
crates/iem-engine/src/resample.rs            Kaiser half-band, Decimator2, Interpolator2
crates/iem-engine/src/media.rs               tap → 48 kHz frames, talkback frames → ring
crates/iem-engine/src/persist.rs             store, load chain, SaveSchedule
crates/iem-engine/src/control.rs             control loop (connections, broadcast, timers, shutdown/fault)
crates/iem-engine/src/pipe.rs                interprocess listeners, names, reader threads
crates/iem-engine/src/engine.rs              run(RunConfig), render()
crates/iem-engine/src/bin/iem-engine.rs      CLI
crates/iem-engine/tests/{rt,rtsan,parity,pipes,props}.rs
crates/iem-engine/examples/bench.rs          CPU benchmark at B = 32
fuzz/{Cargo.toml,Cargo.lock,fuzz_targets/engine_request.rs}
.claude/rules/engine.md, CLAUDE.md           playbook rule + router line
```

---

### Task 1: Start — sync, version, design on #6, docs commit

**Files:** Add `docs/superpowers/specs/2026-09-26-s3-engine-core-design.md`, `docs/superpowers/plans/2026-09-26-s3-engine-core.md`.

- [x] **Step 1: Sync and check the version**

```bash
set -euo pipefail
cd "$WORK"; git switch dev; git fetch origin; git merge --ff-only origin/dev
python3 scripts/check_version.py --base-ref origin/main
```
Expected: `version bump OK: 2.0.0-dev.3 -> 2.0.0-dev.4` (bumped in `e3f32cb`).

- [x] **Step 2: Design on #6 (durable decision)** — a Slovak comment summarising design note §3 (crates and licences, site format, graph and the RT contract at B = 32, sample-accurate commands, solo/listen/test-signal semantics, persistence, pipes, crash model, rtsan via rustc) and §6 (what moves to S4/S5/S6).

- [x] **Step 3: Denylist-scan the docs without touching the index, then commit**

```bash
tmpidx="$(mktemp)"; cp .git/index "$tmpidx"
GIT_INDEX_FILE="$tmpidx" git add docs/superpowers
tree="$(GIT_INDEX_FILE="$tmpidx" git write-tree)"; rm -f "$tmpidx"
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$tree"
git add docs/superpowers
git commit -m "docs(s3): engine core design note and implementation plan" -m "Refs #6" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `iem-engine-proto` — ids, state, messages, frames

**Files:** Create `crates/iem-engine-proto/{Cargo.toml,src/lib.rs,src/ids.rs,src/state.rs,src/msg.rs,src/frame.rs,src/media.rs}`; modify `Cargo.toml` (member), `Cargo.lock`, `scripts/check_version.py` (`CRATES`).

**Interfaces (produces):**

```rust
// ids.rs — stable ids; JSON strings; validity: 1..=64 bytes of [a-z0-9_.-], first [a-z0-9]
pub struct InputId(pub String);  pub struct BusId(pub String);   // Clone, Eq, Ord, Hash, serde(transparent)
pub fn valid_id(s: &str) -> bool;
#[serde(rename_all = "snake_case")] pub enum Source { Input(InputId), Bus(BusId) }   // {"input":"mic1"}
pub struct SendId { pub src: Source, pub dst: BusId }                               // Ord (sorted lists)
#[serde(rename_all = "snake_case")] pub enum EqOwner { Input(InputId), Bus(BusId) }

// state.rs — every struct has Default and #[serde(default)] (additive schemas: unknown fields ignored)
pub const SCHEMA: u32 = 1;
pub const DB_OFF: f64 = -150.0;               // ≤ DB_OFF is silence (linear 0)
pub enum BandKind { HighPass, LowShelf, Peak, HighShelf }             // snake_case
pub struct EqBand { pub kind: BandKind, pub enabled: bool, pub freq_hz: f64, pub gain_db: f64, pub bw_oct: f64 }
pub struct Eq { pub gain_db: f64, pub bands: [EqBand; 5] }             // Default = ReaEQ standard flat (iem-dsp values)
pub struct Limiter { pub enabled: bool, pub limit_db: f64 }             // Default {true, -6}
pub struct InputState { pub trim_db: f64, pub muted: bool, pub processing: bool, pub fader_db: f64, pub pan: f64, pub eq: Eq }
pub struct BusState { pub fader_db: f64, pub pan: f64, pub muted: bool, pub eq: Eq, pub limiter: Limiter }
pub struct SendState { pub gain_db: f64, pub pan: f64, pub muted: bool }  // Default gain_db = DB_OFF
pub struct SendEntry { pub id: SendId, pub state: SendState }
pub struct MixState { pub inputs: BTreeMap<InputId, InputState>, pub buses: BTreeMap<BusId, BusState>, pub sends: Vec<SendEntry> }
pub struct Solo { pub scope: BusId, pub sources: Vec<Source> }
pub struct TestSignal { pub input: InputId, pub hz: f64, pub dbfs: f64, pub ttl_s: f64 }
pub struct Transient { pub solo: Vec<Solo>, pub listen: [Option<BusId>; 2], pub test_signal: Option<TestSignal> }
pub fn db_to_lin(db: f64) -> f64;             // ≤ DB_OFF → 0.0, else 10^(db/20)

// msg.rs
pub const PROTO: u16 = 1;
pub fn negotiate(ours: u16, theirs: u16) -> Option<u16>;   // theirs ≥ ours → ours; theirs = ours−1 → theirs; else None
#[serde(tag = "op", rename_all = "snake_case")] pub enum Cmd {
  SetInput { input: InputId, trim_db: Option<f64>, muted: Option<bool>, processing: Option<bool>, fader_db: Option<f64>, pan: Option<f64> },
  SetBus { bus: BusId, fader_db: Option<f64>, pan: Option<f64>, muted: Option<bool> },
  SetSend { id: SendId, gain_db: Option<f64>, pan: Option<f64>, muted: Option<bool> },
  SetEq { owner: EqOwner, eq: Eq },
  SetLimiter { bus: BusId, enabled: Option<bool>, limit_db: Option<f64> },
  ResetLimiterStats { bus: BusId },
  SetSolo { scope: BusId, sources: Vec<Source> },
  StartListen { bus: BusId }, StopListen { bus: BusId },
  StartTestSignal { input: InputId, hz: f64, dbfs: f64, ttl_s: f64 }, StopTestSignal,
  Batch { ops: Vec<Cmd> },
  ImportState { state: MixState, baseline: bool },
  GetState, GetTopology, SaveNow, Shutdown, InjectFault, Ping,
}
impl Cmd { pub fn is_read_only(&self) -> bool; }         // GetState, GetTopology, Ping
#[serde(rename_all = "snake_case")] pub enum Role { Control, Observe }
#[serde(tag = "type", rename_all = "snake_case")] pub enum ClientMsg { Hello { proto: u16, role: Role, client: String }, Request { id: u64, origin: Option<u64>, cmd: Cmd } }
#[serde(rename_all = "snake_case")] pub enum ErrCode { BadRequest, Unsupported, UnknownId, BadValue, Forbidden, NoSource, NotController, TooLarge }
pub struct ErrorBody { pub code: ErrCode, pub msg: String }
pub struct Reply { pub id: u64, pub rev: u64, pub error: Option<ErrorBody> }
pub struct Hello { pub proto: u16, pub engine_build: String, pub topology_hash: String, pub state_rev: u64, pub sample_rate: u32, pub block: u32, pub role: Role }
#[serde(rename_all = "snake_case")] pub enum BusKind { Output, Stems, Translator, Master }
#[serde(rename_all = "snake_case")] pub enum Tap { Pre, Post }
pub struct InputInfo { pub id: InputId, pub channels: u8, pub talkback: bool }
pub struct BusInfo { pub id: BusId, pub kind: BusKind, pub tx_channels: u8, pub eq: bool, pub limiter: bool }
pub struct SendInfo { pub id: SendId, pub tap: Tap }
pub struct TopologyInfo { pub hash: String, pub sample_rate: u32, pub engineer: BusId, pub inputs: Vec<InputInfo>, pub buses: Vec<BusInfo>, pub sends: Vec<SendInfo> }  // meter order = inputs, buses
#[serde(tag = "kind", rename_all = "snake_case")] pub enum Change {
  Input { id: InputId, state: InputState }, Bus { id: BusId, state: BusState }, Send { id: SendId, state: SendState },
  Solo { scope: BusId, sources: Vec<Source> }, Listen { listen: [Option<BusId>; 2] }, TestSignal { signal: Option<TestSignal> }, LimiterStatsReset { bus: BusId } }
pub struct Meters { pub seq: u64, pub inputs: Vec<[f32; 2]>, pub buses: Vec<[f32; 2]>, pub gr_db: Vec<f32>, pub limiter_active_s: Vec<f64>, pub trips: u64 }
pub struct Status { pub callbacks: u64, pub late: u64, pub faulted: bool, pub process_max_us: f64, pub trips: u64, pub tap_overruns: u64, pub talkback_dropped: u64, pub cmd_backlog: u64 }
#[serde(rename_all = "snake_case")] pub enum AlarmCode { StateFallback, StateLost, Sanitizer, Fault, SaveFailed }
pub struct Alarm { pub code: AlarmCode, pub detail: String }
#[serde(tag = "type", rename_all = "snake_case")] pub enum EngineMsg {
  Hello(Hello), Reply(Reply), Topology(TopologyInfo), State { rev: u64, state: MixState, transient: Transient },
  Delta { rev: u64, origin: Option<u64>, changes: Vec<Change> }, Meters(Meters), Status(Status),
  Saved { rev: u64, generation: u64 }, Alarm(Alarm), DriverReleased { reason: String }, Superseded }
pub fn parse_client(bytes: &[u8]) -> Result<ClientMsg, (Option<u64>, ErrorBody)>;  // unknown op → Unsupported with the id when readable

// frame.rs — control pipe: u32 LE length + JSON
pub const MAX_FRAME: usize = 1 << 20;
pub enum FrameError { Io(io::Error), TooLarge(usize), Closed }
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> Result<(), FrameError>;
pub fn read_frame<R: Read>(r: &mut R, buf: &mut Vec<u8>) -> Result<(), FrameError>;   // EOF before a header → Closed

// media.rs — media pipe: 20-byte header "IEMF", ver 1, stream, channels, 0, seq u64, frames u16, 0u16, then f32 LE
pub const MEDIA_HEADER: usize = 20; pub const FRAME_48K: usize = 960;
pub mod stream { pub const ENGINEER_LISTEN: u8 = 0; pub const MEMBER_LISTEN: u8 = 1; pub const TALKBACK: u8 = 16; }
pub struct MediaHeader { pub stream: u8, pub channels: u8, pub seq: u64, pub frames: u16 }
pub fn write_media<W: Write>(w: &mut W, h: &MediaHeader, samples: &[f32]) -> io::Result<()>;
pub fn read_media<R: Read>(r: &mut R, samples: &mut Vec<f32>) -> Result<MediaHeader, FrameError>;  // frames·channels ≤ 4·FRAME_48K
```

- [x] **Step 1: Tests first** (in-crate `#[cfg(test)]`), each able to fail:
  - `ids_are_validated`: `valid_id("member1.stems")`, not `""`, `"A"`, `"-x"`, 65 bytes, `"a b"`.
  - `json_shapes_are_stable`: `serde_json::to_string(&Cmd::SetBus{…})` equals `{"op":"set_bus","bus":"member1","fader_db":-3.0,"pan":null,"muted":null}`; `Source::Input` is `{"input":"mic1"}`; `ClientMsg::Request` nests as `{"type":"request","id":7,"origin":null,"cmd":{"op":"ping"}}`.
  - `unknown_fields_are_ignored_and_missing_ones_default`: `{"inputs":{"mic1":{"trim_db":-3,"future":1}}}` parses to `MixState` with `trim_db = -3`, `processing = true`, flat EQ.
  - `unknown_op_is_unsupported_with_its_id` / `garbage_is_bad_request_without_id`: `parse_client(br#"{"type":"request","id":9,"cmd":{"op":"warp"}}"#)` → `Err((Some(9), Unsupported))`; `parse_client(b"\xff")` → `Err((None, BadRequest))`.
  - `negotiate_speaks_n_and_n_minus_1`: with ours 5: 5→5, 6→5, 9→5, 4→4, 3→None, 0→None.
  - `frames_round_trip_and_oversize_is_refused`: write/read; a header claiming `MAX_FRAME + 1` → `TooLarge` without reading the body; empty reader → `Closed`; a truncated body → `Io(UnexpectedEof)`.
  - `media_frames_round_trip_and_bad_magic_fails`.
  - `db_to_lin_floors_at_db_off`: `db_to_lin(-150.0) == 0.0`, `db_to_lin(0.0) == 1.0`, `db_to_lin(-6.0) == 10f64.powf(-0.3)`.
  - `eq_default_is_the_reaeq_standard_flat`: the five kinds, frequencies and bandwidths of `iem_dsp::eq::EqParams::standard_flat()` (dev-dependency), gains 0 dB, disabled.

- [x] **Step 2: Implementation** per the interfaces; `lib.rs` re-exports every public item; crate attributes as in `iem-dsp`; `Cargo.toml` depends on `serde` (derive) and `serde_json`.

- [x] **Step 3: Commit** `feat(proto): typed engine protocol, stable ids, state, frames (S3)`.

---

### Task 3: `iem-audio-io` — `Process`, `Offline`, `NullRt`, WAV

**Files:** Create `crates/iem-audio-io/{Cargo.toml,src/lib.rs,src/offline.rs,src/nullrt.rs,src/wav.rs}`; members, lockfile, `CRATES`.

**Interfaces (produces):**

```rust
pub struct Block<'a> { /* frames, n_in, n_out, input: &'a [f64], output: &'a mut [f64] — channel-major */ }
impl<'a> Block<'a> {
  pub fn new(frames: usize, n_in: usize, input: &'a [f64], n_out: usize, output: &'a mut [f64]) -> Self;
  pub fn frames(&self) -> usize;
  pub fn input(&self, ch: usize) -> &[f64];          // empty when out of range
  pub fn output(&mut self, ch: usize) -> &mut [f64];  // empty when out of range
  pub fn zero_outputs(&mut self);
}
pub trait Process: Send { fn process(&mut self, block: &mut Block<'_>); }
pub struct Planar { /* channels, frames, data */ }
impl Planar { pub fn new(channels: usize, frames: usize) -> Self; pub fn channels(&self) -> usize; pub fn frames(&self) -> usize;
              pub fn channel(&self, ch: usize) -> &[f64]; pub fn channel_mut(&mut self, ch: usize) -> &mut [f64]; }
pub struct Fault { pub frame: u64, pub message: String }
pub fn panic_message(p: &(dyn Any + Send)) -> String;
// offline.rs
pub struct Offline { pub block: usize }
pub struct OfflineRun { pub output: Planar, pub fault: Option<Fault> }
impl Offline { pub fn run<P: Process>(&self, p: &mut P, input: &Planar, outputs: usize) -> OfflineRun; }  // block 0 → 1; the last block may be short; after a fault every later frame is zero and p is not called again
// nullrt.rs
pub enum InputSignal { Silence, Sine { hz: f64, amp: f64 } }
pub struct NullRtConfig { pub sample_rate: u32, pub block: usize, pub inputs: usize, pub outputs: usize, pub signal: InputSignal }
pub struct StreamStats { pub callbacks: u64, pub late: u64, pub faulted: bool, pub running: bool, pub max_process_ns: u64, pub fault: Option<String> }
pub struct NullRt<P: Process + 'static> { /* JoinHandle<P>, Arc<Shared> */ }
impl<P: Process + 'static> NullRt<P> {
  pub fn start(cfg: NullRtConfig, p: P) -> io::Result<Self>;   // thread "iem-nullrt"; absolute deadlines; > 8 periods behind → resync (late += 1)
  pub fn stats(&self) -> StreamStats;
  pub fn stop(self) -> P;                                      // sets stop, joins, returns the processor
}
// wav.rs
pub fn read(bytes: &[u8]) -> io::Result<(u32, Planar)>;        // PCM 16/24/32, float 32/64, WAVE_FORMAT_EXTENSIBLE
pub fn write(rate: u32, audio: &Planar) -> Vec<u8>;           // float64 EXTENSIBLE
pub fn read_file(path: &Path) -> io::Result<(u32, Planar)>; pub fn write_file(path: &Path, rate: u32, audio: &Planar) -> io::Result<()>;
```

- [x] **Step 1: Tests first:**
  - `offline_passes_blocks_of_the_requested_size`: a recording processor sees frames `[32, 32, 32, 4]` for 100 frames at block 32 and gets channel-major input equal to the source; output equals what it wrote.
  - `offline_panic_zeroes_the_block_and_stops`: a processor that writes 1.0 and panics at its third call: output frames 0..64 are 1.0, the rest 0.0, `fault.frame == 64`, message contains the panic text, the processor saw 3 calls.
  - `block_accessors_never_panic`: `input(99)` and `output(99)` are empty.
  - `nullrt_paces_close_to_real_time`: 32-frame blocks at 96 kHz for 300 ms → callbacks within [0.5×, 1.5×] of 900; `stop()` returns the processor with the same count.
  - `nullrt_feeds_the_sine`: `Sine{hz: 1000, amp: 0.5}`: the processor's input peak is 0.5 ± 1e-3 on every channel and the zero crossings are 96 samples apart.
  - `nullrt_panic_marks_the_stream_faulted_and_stops_calling`: after the panic, `stats().faulted`, `fault` carries the message, the callback count no longer grows over 50 ms, `running == false`.
  - `wav_round_trips_float64_and_reads_pcm`: `write` → `read` bit-exact for 3 channels; hand-built PCM16 and PCM24 files read to `x/32768` and `x/8388608`; a non-RIFF buffer is `InvalidData`.

- [x] **Step 2: Implementation** — `catch_unwind(AssertUnwindSafe(|| p.process(&mut block)))` in both drivers; NullRt measures each call with `Instant` outside `process()` and keeps the max; its buffers are allocated once before the loop.

- [x] **Step 3: Commit** `feat(audio-io): Process trait, Offline and paced NullRt backends, WAV codec (S3)`.

---

### Task 4: `iem-dsp` steady-state fast paths

**Files:** Modify `crates/iem-dsp/src/pan.rs`, `crates/iem-dsp/src/eq.rs`, `crates/iem-dsp/tests/rt.rs`.

**Interfaces (produces):** `StereoGain::steady(&self) -> Option<(f64, f64)>` (the exact values `tick()` would return while nothing moves); `Equalizer::<CH>::is_identity(&self) -> bool` (every band bypassed, global gain resting at exactly 1.0).

- [x] **Step 1: Tests first:** `steady_is_none_while_moving_and_equals_tick_at_rest` (after `set`, `None` for 960 ticks, then `Some(tick())` bit-exact; muted gives `(0, 0)`); `identity_only_when_every_band_is_bypassed_at_unity` (flat → true; one enabled band → false until disabled and faded; global gain 0.5 → false; process output equals input exactly when true). `rt.rs` calls both under `assert_no_alloc`.
- [x] **Step 2: Implementation** (5 lines each) and **commit** `feat(dsp): steady-state accessors for the engine's fast paths`.

---

### Task 5: Site and graph

**Files:** Create `crates/iem-engine/{Cargo.toml,LICENSE,src/lib.rs,src/site.rs,src/graph.rs}`; modify `config/test-site.toml` (`[engine]`), `crates/iem-core/src/config.rs` (ignore `engine`), `deny.toml`, members, lockfile, `CRATES`.

**Interfaces (produces):**

```rust
// lib.rs
pub const SAMPLE_RATE: u32 = 96_000; pub const SEG: usize = 256; pub const MAX_CMDS_PER_BLOCK: usize = 512;
pub const TALKBACK_GAIN: f64 = 0.379934; pub const TEST_CAP: f64 = 0.1; pub const MAX_BATCH: usize = 256; pub const MAX_SOLO: usize = 64;
// site.rs
pub struct SiteInput { pub id: String, pub rx: Vec<u16>, pub talkback: bool }
pub struct SiteBus { pub id: String, pub kind: BusKind, pub tx: Vec<u16> }
pub struct SiteSends { pub from: Vec<String>, pub to: Vec<String>, pub tap: Tap }
pub struct Site { pub channels: u16, pub engineer: String, pub inputs: Vec<SiteInput>, pub buses: Vec<SiteBus>, pub sends: Vec<SiteSends> }
pub enum SiteError { Toml(String), NoEngineTable, BadId(String), DuplicateId(String), UnknownId(String),
  ChannelCount { id: String, expected: &'static str, got: usize }, ChannelRange { id: String, ch: u16 }, ChannelReused { ch: u16 },
  DuplicateSend { from: String, to: String }, TapMismatch { from: String }, BadDestination { to: String }, BadSource { from: String },
  Cycle(String), SecondMaster, SecondTalkback, Engineer(String), Io(String) }
pub fn parse(text: &str) -> Result<Site, SiteError>;       // only the [engine] table; deny_unknown_fields inside it
pub fn load(path: &Path) -> Result<Site, SiteError>;
// graph.rs
pub enum Src { Pre(usize), Post(usize) }
pub struct InputNode { pub id: InputId, pub rx: [usize; 2], pub stereo: bool, pub talkback: bool }
pub struct BusNode { pub id: BusId, pub kind: BusKind, pub tx: [Option<usize>; 2], pub sends: Range<usize> }
pub struct SendEdge { pub id: SendId, pub src: Src, pub dst: usize }
pub struct Graph { pub inputs: Vec<InputNode>, pub buses: Vec<BusNode> /* processing order */, pub sends: Vec<SendEdge> /* grouped by dst */,
                   pub rx: Vec<u16>, pub tx: Vec<u16>, pub engineer: usize, pub master: Option<usize>, pub reach: Vec<Vec<bool>> /* [input][bus] */, pub hash: String }
pub fn compile(site: &Site) -> Result<Graph, SiteError>;
impl Graph { pub fn input_index(&self, id: &InputId) -> Option<usize>; pub fn bus_index(&self, id: &BusId) -> Option<usize>;
             pub fn send_index(&self, id: &SendId) -> Option<usize>; pub fn info(&self) -> TopologyInfo;
             pub fn has_eq(&self, bus: usize) -> bool; pub fn has_limiter(&self, bus: usize) -> bool; }
```

Rules: a post source bus is always earlier in `buses` than its destination (Kahn over send edges plus master's implicit edges from every stems bus); `rx`/`tx` list card channels in first-use order and nodes hold indices into them; `reach[i][b]` is true when a path of sends (and the implicit master edges) leads from input `i` to TX-bearing bus `b`; `hash` is the SHA-256 hex of the canonical `TopologyInfo` JSON without the hash.

`config/test-site.toml` `[engine]`: `channels = 128`, `engineer = "engineer"`; inputs `mic1`…`mic10`, `hand1`…`hand3`, `eng_mic` (talkback), `keys`, `iemonly`, `content` (stereo) and the stems group `click`, `guide`, `drums`, `bass`, `inst`, `other`, `bgvs` on RX 101–132; buses `member1`…`member9`, `engineer` (output, TX 71–88, 91/92), `member1.stems`…`engineer.stems`, `translator` (TX 93), `master` (TX 89/90); send families: 17 direct × 10 output buses `pre`; 7 stems-group × 10 stems buses `pre`; `hand1` → `translator` `pre`; each stems bus → its output bus `post`; `member2`…`member9` → `member1` `post`; `member1`…`member9` → `engineer` `post`.

- [x] **Step 1: Tests first** (`site.rs`/`graph.rs` unit tests; a `tests/`-free crate stage):
  - `test_site_has_the_program_shape`: `compile(load("config/test-site.toml"))`: 24 inputs, `rx.len() == 32`, `tx.len() == 23`, 268 sends (241 `Pre`, 27 `Post`), 20 buses with EQ, 10 with a limiter, master last, `member1` after `member2`…`member9`, `engineer` after `member1`.
  - one test per `SiteError` variant on a minimal site string (duplicate id across inputs and buses, unknown reference, mono output bus, channel 0 and channel 129, RX reused, TX reused, two sends for one pair across families, `post` from an input, `pre` from a bus, send into master, send from master, a two-bus cycle `a → b → a`, two masters, two talkback inputs, engineer not an output bus, id `"Mic 1"`, unknown key inside `[engine]`, no `[engine]` table).
  - `reach_follows_sends_and_master`: `mic1` reaches all ten output buses and master, not the translator; `hand1` also reaches the translator; `drums` reaches output buses only through stems and master.
  - `hash_is_stable_and_topology_sensitive`: same site → same hash; one extra send → different.
  - `iem-core`: `config_accepts_the_engine_table` loads `config/test-site.toml` with `Config::load`.

- [x] **Step 2: Implementation.** `iem-core` `Config` gains `#[serde(default, skip_serializing)] pub engine: Option<serde::de::IgnoredAny>` (the table belongs to the engine) and `engine: None` in `Default`. `deny.toml` adds the `iem-engine` GPL exception with the D1 reason.

- [x] **Step 3: Commit** `feat(engine): site.toml topology, validation and graph compiler (S3)`.

---

### Task 6: Parameters and the control core

**Files:** Create `crates/iem-engine/src/params.rs`, `crates/iem-engine/src/core.rs`; RT command types in `crates/iem-engine/src/rt.rs` (types only in this task).

**Interfaces (produces):**

```rust
// params.rs — proto values → DSP parameters, every field capped
pub struct Caps; // consts: TRIM_DB (-150, 24), FADER_DB (-150, 12), PAN (-1, 1), EQ_FREQ (20, 24000), EQ_GAIN_DB (-150, 12.041199826559248), EQ_BW (0.01, 4), LIMIT_DB (-6, 0), TEST_HZ (20, 20000), TEST_DBFS (-120, -20), TEST_TTL_S (0.001, 120)
pub fn cap(v: f64, lo: f64, hi: f64) -> Option<f64>;          // non-finite → None
pub fn cap_input(s: &InputState) -> InputState; pub fn cap_bus(s: &BusState) -> BusState; pub fn cap_send(s: &SendState) -> SendState; pub fn cap_eq(e: &Eq) -> Eq;  // non-finite → default
pub fn eq_params(e: &Eq) -> iem_dsp::eq::EqParams;
pub struct InputParams { pub trim: f64, pub muted: bool, pub processing: bool, pub fader: f64, pub pan: f64 }
pub fn input_params(s: &InputState) -> InputParams; pub fn fader(db: f64) -> f64;   // = db_to_lin
// rt.rs (types)
#[derive(Clone, Copy, Debug, Default, PartialEq)] pub struct RtCmd { pub at: u64, pub group: u16, pub op: RtOp }
#[derive(Clone, Copy, Debug, Default, PartialEq)] pub enum RtOp { #[default] Nop,
  Input { i: u16, p: InputParams }, InputEq { i: u16, eq: EqParams }, Bus { b: u16, fader: f64, pan: f64, muted: bool }, BusEq { b: u16, eq: EqParams },
  Limiter { b: u16, enabled: bool, limit_db: f64 }, ResetLimiter { b: u16 }, Send { s: u16, gain: f64, pan: f64, muted: bool },
  Listen { slot: u8, bus: Option<u16> }, TestSignal { i: u16, hz: f64, amp: f64, ttl: u64 }, StopTestSignal, FadeOut, Panic }
pub fn group(at: u64, ops: &[RtOp]) -> Vec<RtCmd>;   // group = ops.len() on the first command, 0 on the rest
// core.rs
pub struct Flags { pub test_signal: bool, pub fault_injection: bool }
pub enum Effect { None, SendState, SendTopology, Save, Shutdown, Imported { baseline: bool } }
pub struct Outcome { pub rev: u64, pub changes: Vec<Change>, pub rt: Vec<RtOp>, pub effect: Effect }
pub struct CmdError { pub code: ErrCode, pub msg: String }
pub struct Core { /* graph: Arc<Graph>, state, transient, rev, flags */ }
impl Core {
  pub fn new(graph: Arc<Graph>, state: MixState, flags: Flags) -> Self;   // state reconciled by `reconcile`
  pub fn apply(&mut self, cmd: &Cmd) -> Result<Outcome, CmdError>;
  pub fn clear_solos(&mut self) -> Outcome; pub fn end_test_signal(&mut self) -> Outcome;
  pub fn state(&self) -> &MixState; pub fn transient(&self) -> &Transient; pub fn rev(&self) -> u64; pub fn graph(&self) -> &Arc<Graph>;
}
pub fn reconcile(graph: &Graph, state: &MixState) -> (MixState, Vec<String> /* dropped ids */);   // topology order, defaults for missing, caps
pub fn defaults_muted(graph: &Graph) -> MixState;                                                 // load-chain end: every TX bus muted
```

Semantics: a command that changes nothing returns the current `rev` and no changes; a changing request bumps `rev` by exactly one; `Batch` applies all ops on a copy and commits only if every op succeeds (no nested batch, import, shutdown, save or fault inside; ≤ `MAX_BATCH`); every outcome's `rt.len() ≤ MAX_CMDS_PER_BLOCK`. Solo: sends into the scope's tree (the scope bus plus the stems buses with a post send into it) whose source is neither soloed nor a bus of the tree get an effective mute; `SetSolo` validates each source against the tree's senders. Listen per design §3.3 (`NoSource` for a second distinct member). Test signal needs `flags.test_signal` (`Forbidden` otherwise), amplitude `db_to_lin(min(dbfs, −20))`, `ttl = round(ttl_s·96000)`. `InjectFault` needs `flags.fault_injection`. `SetEq` on a bus without EQ and `SetLimiter` on a bus without limiter are `BadValue`.

- [x] **Step 1: Tests first** (`core.rs` unit tests over `test-site.toml`): `a_set_changes_state_bumps_rev_and_emits_one_rt_op`; `an_unchanged_set_keeps_rev`; `values_are_capped_and_non_finite_rejected` (fader +40 → +12, pan 3 → 1, NaN → `BadValue`, 1e308 trim → 24); `unknown_ids_are_unknown_id`; `batches_are_atomic_and_bounded` (a batch whose third op fails changes nothing; 257 ops → `BadValue`; nested batch → `BadRequest`); `solo_mutes_the_tree_and_clears` (soloing `mic2` on `member3`: every send into `member3` and `member3.stems` except from `mic2` and `member3.stems` is effectively muted in its `RtOp::Send`; the stored `SendState.muted` is unchanged; `clear_solos` restores); `solo_rejects_foreign_sources`; `listen_allows_engineer_and_one_member`; `test_signal_needs_the_flag_and_is_capped` (dbfs −3 → amp 0.1, ttl 500 s → 120 s); `fault_injection_needs_the_flag`; `import_replaces_state_and_fits_one_block` (rt ops ≤ 512, dropped unknown ids reported by `reconcile`); `eq_and_limiter_only_where_the_bus_has_them`; `read_only_commands_do_not_change_rev`.

- [x] **Step 2: Implementation**; **commit** `feat(engine): capped control core with revisioned changes, solo, listen and test signal (S3)`.

---

### Task 7: The RT processor

**Files:** `crates/iem-engine/src/rt.rs`, `crates/iem-engine/src/rt/nodes.rs`; `crates/iem-engine/tests/rt.rs`.

**Interfaces (produces):**

```rust
pub struct Options { pub fade_in_ms: f64 }                       // Default 500
pub struct MeterFrame { pub seq: u64, pub inputs: Vec<[f64; 2]>, pub buses: Vec<[f64; 2]>, pub gr_db: Vec<f64>, pub active: Vec<u64>, pub trips: u64 }
pub struct RtStatus { pub faded_out: AtomicBool, pub trips: AtomicU64, pub tap_overruns: AtomicU64, pub talkback_underruns: AtomicU64, pub deferred: AtomicU64 }
pub struct RtHandles { pub cmds: rtrb::Producer<RtCmd>, pub meters: triple_buffer::Output<MeterFrame>, pub taps: [rtrb::Consumer<f32>; 2], pub talkback: rtrb::Producer<f32>, pub status: Arc<RtStatus> }
pub const METER_PERIOD: u64 = 3_200; pub const CMD_RING: usize = 4_096; pub const TAP_RING: usize = 2 * 19_200; pub const TALK_RING: usize = 11_520;
pub struct Processor { .. }
impl Processor { pub fn new(graph: Arc<Graph>, state: &MixState, counters: &[u64] /* per bus, X14 base */, opts: Options) -> (Self, RtHandles);
                 pub fn time(&self) -> u64; }
impl Process for Processor { fn process(&mut self, block: &mut Block<'_>); }  // #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
pub fn push_group(p: &mut rtrb::Producer<RtCmd>, at: u64, ops: &[RtOp]) -> bool;   // one write_chunk; false when the ring lacks room
```

Algorithm (design §3.2): `process()` loops: apply due commands (head `at ≤ now`, a group only when the remaining budget of 512 covers it, all of it at once), cut the segment at `min(SEG, next.at − now, frames − done)`, render it. Rendering order: inputs → buses in graph order (sends accumulate with `StereoGain::steady` fast path) → TX stage per TX bus → taps. After the block: X14 counters, meter publication every `METER_PERIOD` samples, status atomics.

- [x] **Step 1: Tests first** (unit tests in `rt.rs` on small synthetic sites built with `site::parse`, plus `tests/rt.rs`):
  - `a_mono_input_feeds_both_channels_at_unity` (A2) and `stereo_keeps_its_channels`.
  - `trim_and_eq_apply_only_with_processing` (Q3/A4: trim −6 dB halves the send output after 20 ms with processing on; processing off passes the input unchanged).
  - `talkback_is_added_before_the_mute_gate` (A3/A4: 0.5 talkback samples add `0.5·0.379934` to the talkback input's pre tap; muting that input silences both).
  - `pre_sends_ignore_the_input_fader_post_sends_follow_the_bus_fader` (A6).
  - `output_chain_is_eq_limiter_fader_mute_safety_clamp` (A8: limit −6 dB on a 1.0 input caps the bus at 0.5012 before a +12 dB fader; the TX output never exceeds 1.0; a +12 dB fader on 0.9 is caught by the safety stage below 1.0 without a clamp hit).
  - `bus_to_bus_reads_post_mute_unclipped` (A9: member2 at 2.0 post fader feeds member1 2.0·g, while member2's own TX is ≤ 1.0).
  - `translator_is_the_half_sum_on_one_channel` (A10) and `master_sums_post_fader_inputs_and_stems` (A11).
  - `sanitizer_silences_and_resets_a_tripping_node` (X1: a NaN on one RX zeroes that input's block, counts a trip in the meters and `RtStatus`, the next finite block passes).
  - `commands_apply_at_their_sample` (a fader change at `at = 1000` inside a 256 block starts ramping exactly at sample 1000).
  - `at_most_512_commands_per_block_and_groups_stay_whole` (600 single commands: 512 applied in the first block; a 400-op group behind 200 singles waits for the next block and applies whole).
  - `limiter_raise_ramps_lower_is_instant` (§4.4).
  - `test_signal_caps_reachable_tx_and_ends_after_its_ttl` (X13: a −3 dBFS request arrives as amp 0.1; reachable TX ≤ 0.1 while active; unreachable TX untouched; silence and caps lifted after TTL + fade).
  - `listen_taps_are_side_effect_free` (X3: TX output identical with and without both taps; slot 0 carries the engineer pre-fader signal, slot 1 the member post-mute signal through the 0 dB listen limiter).
  - `fade_in_and_fade_out` (500 ms from zero; `FadeOut` reaches zero in 50 ms and sets `faded_out`).
  - `meters_publish_every_3200_samples` (peaks at P and O, GR, X14 active base + count).
  - `tests/rt.rs`: `the_detector_sees_an_allocation`; `process_does_not_allocate` (20 000 blocks at B = 32 with a command group every block, taps, talkback, test signal, meter publication, a sanitiser trip, all under `assert_no_alloc`, violation count 0).
- [x] **Step 2: Implementation**; **commit** `feat(engine): RT processor — A1–A13, ramps, sanitiser, Q1 safety, taps, test cap, meters (S3)`.

---

### Task 8: Impulse oracle and block-size invariance

**Files:** `crates/iem-engine/tests/parity.rs`.

- [x] **Step 1: The reference model** — written from A2–A11 only (no engine graph code): for each node a 2×32 gain matrix from RX channels, computed recursively over `site::parse` of `config/test-site.toml` and a `MixState`: input `P = (1−mute)·(processing ? trim : 1)·e_rx` (mono → both rows), `O = P ⊙ v·gains(p)`; a send adds `src ⊙ v(1−m)·gains(p)` with `src = P` (pre) or the source bus's `O` (post); stems and output `O = Σ ⊙ v(1−m)·gains(p)`; translator `TX = (O_L + O_R)/2`; master `O = (Σ input O + Σ stems O) ⊙ v(1−m)·gains(p)`.
- [x] **Step 2: `impulse_oracle_matches_random_states`** — 8 seeds; random trims (±12 dB), faders (−30…+6 dB), pans, 20 % mutes, processing on/off, EQ all disabled, limiters enabled (the impulse 1e-3 stays far below any limit); `Offline` at B = 32 with `fade_in_ms = 0`; impulses on RX channel k at sample 64 + 16k; each TX sample `64 + 16k` equals `1e-3·M[tx][k]` within 1e-12 (relative to 1e-3), every other sample is exactly 0. Prints `oracle max error …`.
- [x] **Step 3: Dedicated cases:** `input_mute_kills_pre_sends_and_talkback` (A3), `pre_ignores_the_input_fader_post_follows_the_stems_fader` (A6), `bus_to_bus_is_post_mute` (A9), `translator_carries_only_hand1` (A10).
- [x] **Step 4: `outputs_do_not_depend_on_the_block_size`** — 0.25 s of hot material (noise and sines up to 2.0), random enabled EQ bands, limiters in GR, a schedule of 40 commands at fixed sample indices (many inside blocks: fader/pan/mute ramps, EQ moves, limiter lower/raise/disable, solo, listen, processing toggles) pushed with their `at`; B = 32, 64, 97, 256; max abs difference ≤ 1e-12 (prints `invariance max difference …`).
- [x] **Step 5: Commit** `test(engine): impulse oracle over test-site.toml and block-size invariance (S3)`.

---

### Task 9: Media path — half-band resampler, taps and talkback

**Files:** `crates/iem-engine/src/resample.rs`, `crates/iem-engine/src/media.rs`.

**Interfaces (produces):**

```rust
pub const TAPS: usize = 79; pub const BETA: f64 = 10.06;
pub fn half_band() -> [f64; TAPS];                      // Kaiser-windowed, even offsets exactly 0, Σ = 1
pub struct Decimator2 { .. } impl Decimator2 { pub fn new() -> Self; pub fn push(&mut self, l: f64, r: f64) -> Option<(f64, f64)>; }  // every second input yields an output
pub struct Interpolator2 { .. } impl Interpolator2 { pub fn new() -> Self; pub fn push(&mut self, x: f64) -> [f64; 2]; }
pub struct TapFramer { .. } impl TapFramer { pub fn new(stream: u8) -> Self; pub fn feed(&mut self, interleaved_96k: &[f32], out: &mut Vec<(MediaHeader, Vec<f32>)>); }  // 960-frame 48 kHz stereo frames
pub struct TalkbackFeed { .. } impl TalkbackFeed { pub fn new() -> Self; pub fn feed(&mut self, samples_48k: &[f32], ring: &mut rtrb::Producer<f32>) -> usize /* dropped */; }
```

- [x] **Step 1: Tests first:** `half_band_is_symmetric_normalised_and_half_zero`; `decimator_passes_1_khz_and_rejects_30_khz` (1 kHz: amplitude within ±0.001 dB after settling; 30 kHz: ≤ −95 dB); `decimator_dc_gain_is_one`; `interpolator_has_unity_gain_and_rejects_images` (a 48 kHz 1 kHz sine → 96 kHz 1 kHz at ±0.001 dB; 47 kHz image ≤ −95 dB); `tap_framer_emits_960_frame_stereo_frames_with_sequence`; `talkback_feed_counts_drops_when_the_ring_is_full`.
- [x] **Step 2: Implementation** (I0 by its power series); **commit** `feat(engine): 96↔48 kHz half-band media path for listen taps and talkback (X3, X4)`.

---

### Task 10: Persistence

**Files:** `crates/iem-engine/src/persist.rs`.

**Interfaces (produces):**

```rust
pub const FORMAT: &str = "iemmixer-state"; pub const GENERATIONS: usize = 20;
pub struct Persisted { pub rev: u64, pub topology_hash: String, pub saved_unix_ms: u64, pub state: MixState, pub counters: BTreeMap<BusId, u64> }
pub enum Source { Current, Generation(u64), Baseline, Defaults }
pub struct Loaded { pub persisted: Persisted, pub source: Source, pub rejected: Vec<(PathBuf, String)> }
pub struct Store { dir: PathBuf }
impl Store { pub fn open(dir: &Path) -> io::Result<Self>; pub fn save(&self, p: &Persisted) -> io::Result<u64 /* generation seq of the previous current, 0 if none */>;
             pub fn save_baseline(&self, p: &Persisted) -> io::Result<()>; pub fn load(&self, graph: &Graph) -> Loaded; }
pub fn encode(p: &Persisted) -> Vec<u8>; pub fn decode(bytes: &[u8]) -> Result<Persisted, String>;
pub struct SaveSchedule { .. } impl SaveSchedule { pub fn changed(&mut self, now: Instant); pub fn due(&self, now: Instant) -> bool; pub fn saved(&mut self); }  // 1 s quiet or 5 s since the first change
```

- [x] **Step 1: Tests first** (`tempfile` dirs): `save_then_load_is_lossless` (bit-exact f64s); `a_save_rotates_current_into_a_generation_and_keeps_20`; `a_bad_checksum_falls_back_to_the_newest_good_generation` (reported in `rejected`); `truncated_or_foreign_files_are_skipped`; `baseline_is_the_last_resort_before_defaults`; `nothing_loadable_gives_muted_defaults` (`Source::Defaults`, every output/translator/master bus muted); `newer_fields_are_ignored_and_missing_default` (a hand-written payload with `"future": 1` and no `sends`); `unknown_ids_are_dropped_on_load`; `schedule_waits_one_quiet_second_but_at_most_five`.
- [x] **Step 2: Implementation** (temp file + `sync_all`, rename the old current to `gen-<seq:010>.json`, rename temp → current, prune, `File::open(dir)?.sync_all()` on Unix); **commit** `feat(engine): atomic checksummed saves, generations, baseline and load chain (§2.4)`.

---

### Task 11: Control loop, pipes, engine run and the binary

**Files:** `crates/iem-engine/src/{control.rs,pipe.rs,engine.rs,bin/iem-engine.rs}`; `crates/iem-engine/tests/pipes.rs`.

**Interfaces (produces):**

```rust
// pipe.rs
pub fn control_name(pipe: &str) -> io::Result<Name<'static>>; pub fn media_name(pipe: &str) -> io::Result<Name<'static>>;  // Unix: path / path + ".media"; Windows: namespaced
// engine.rs
pub struct RunConfig { pub site: PathBuf, pub state_dir: PathBuf, pub pipe: String, pub block: usize, pub flags: Flags, pub signal: InputSignal }
pub enum Exit { Shutdown, Fault(String) }
pub fn run(cfg: RunConfig) -> Result<Exit, EngineError>;                // blocking; logs via tracing on control threads only
pub fn render(site: &Path, state: Option<&Path>, input: &Path, output: &Path, block: usize) -> Result<(), EngineError>;
// bin: iem-engine run --site S --state-dir D --pipe P [--block 32] [--test-signal] [--fault-injection] [--sine HZ]
//      iem-engine render --site S [--state F] --in IN.wav --out OUT.wav [--block N]
// exit codes: 0 shutdown, 2 usage/config, 70 fault
pub fn parse_args(args: &[String]) -> Result<Command, String>;          // in lib (engine.rs) so it is unit-tested
```

Control loop (main thread of `run`): 10 ms ticks on an `mpsc` of `Connected{id, role-less SendHalf}`, `Frame{id, bytes}`, `Closed{id}`; first frame must be `Hello` (else error reply and close); `Hello` → `Hello` reply, `Topology`, `State`; a new `Control` hello supersedes the previous controller (`Superseded`, closed); observers get only read-only commands (`NotController`); every applied change → `Delta` to all; `Effect`s drive state/topology replies, saves, shutdown; meters forwarded when `updated()`; `Status` each second; solos cleared 10 s after the controller drops unless another controls; test signal cleared at its deadline; saves by `SaveSchedule`; backend fault → save, `Alarm{fault}`, `DriverReleased`, `Exit::Fault`; `Shutdown` → reply, save, `FadeOut`, wait for `faded_out` (≤ 200 ms), stop the backend, `DriverReleased`, `Exit::Shutdown`. Writes use a 1 s send timeout; a failed write drops that connection. The media thread (5 ms ticks) owns the tap consumers and the framers and writes to the current media connection; media reader threads feed `TalkbackFeed` into the talkback producer (behind a `Mutex`, never touched by the RT thread).

- [x] **Step 1: Tests first** (`tests/pipes.rs`, the engine in-process on NullRt at B = 32 with a temp state dir and a temp socket path; every read has a 5 s timeout):
  - `hello_topology_state_then_reply_and_delta` (proto 1; `SetBus` → `Reply{rev: 1}` and `Delta{rev: 1, origin}` on both the controller and an observer).
  - `observers_cannot_write`, `a_new_controller_supersedes_the_old`, `garbage_gets_a_typed_error_and_the_connection_survives`, `oversized_frame_closes_only_that_connection`.
  - `meters_arrive_at_about_30_hz` (≥ 10 `Meters` in 1 s with the sine input showing on every input meter).
  - `listen_frames_arrive_on_the_media_pipe` (engineer and one member; 960-frame stereo frames with increasing `seq`; a second member → `NoSource`).
  - `talkback_frames_reach_the_talkback_input` (the `eng_mic` meter rises with talkback frames and falls without).
  - `shutdown_saves_fades_and_releases` (reply, `DriverReleased`, `Exit::Shutdown`, `current.json` with the new rev; a second run loads it and reports the same rev in `Hello`).
  - `fault_injection_releases_the_driver_and_exits` (`Alarm{fault}`, `DriverReleased`, `Exit::Fault`, state saved).
  - `the_binary_runs_and_shuts_down` (spawns `CARGO_BIN_EXE_iem-engine run …`, `Shutdown`, exit code 0 within 5 s) and `the_binary_renders_offline` (`render` refuses a WAV that is not 96 kHz or does not have 32 channels with exit 2 (I2); a 32-channel 96 kHz WAV renders 23 channels).
  - unit tests of `parse_args`.
- [x] **Step 2: Implementation**; **commit** `feat(engine): control loop, local-socket pipes, crash model and the iem-engine binary (S3)`.

---

### Task 12: Fuzzing, rtsan and the CPU benchmark

**Files:** `crates/iem-engine/tests/{props.rs,rtsan.rs}`, `crates/iem-engine/examples/bench.rs`, `fuzz/{Cargo.toml,Cargo.lock,fuzz_targets/engine_request.rs}`.

- [x] **Step 1: `props.rs`** — `random_requests_never_panic_and_keep_state_in_caps`: `IEM_FUZZ_ITERS` (default 2 000) random byte strings, mutated valid JSON and random valid commands through `parse_client` + `Core::apply`; after each: every state value finite and within its cap, `rev` non-decreasing, `rt.len() ≤ 512`. `random_command_groups_render_finite_bounded_output`: random groups into a `Processor`, 200 blocks, every TX sample finite and |x| ≤ 1.
- [x] **Step 2: `rtsan.rs`** — `process_is_realtime_safe` (same scenario as `rt.rs`, 5 000 blocks; under `cfg(iem_rtsan)` any violation aborts the binary) and `rtsan_detects_a_violation` (re-runs the test binary with `IEM_RTSAN_CHILD=1 --exact child_allocates_in_a_nonblocking_function`; under `cfg(iem_rtsan)` expects failure and `RealtimeSanitizer` in stderr, otherwise success). The crate root has `#![cfg_attr(iem_rtsan, feature(sanitize))]`; `Cargo.toml` declares `check-cfg = ["cfg(iem_rtsan)"]`.
- [x] **Step 3: `examples/bench.rs`** — `typical` (the site at rest, sine inputs) and `worst` (all 220 EQ bands enabled and moving, every send ramping, limiters in GR, both taps, talkback, test signal, a 512-command group every block): 30 000 timed calls each at B = 32 after 3 000 warm-up calls; prints `bench <case> p50 … p99 … p99.9 … max … µs (period 333.3 µs)`; exits 1 when p50 > 25 % of the period.
- [x] **Step 4: `fuzz/`** — a standalone workspace (`[workspace] members = ["."]`) with `libfuzzer-sys` and `iem-engine` by path; `engine_request` feeds the bytes to `parse_client` and `Core::apply` on a `test-site.toml` core. `fuzz/Cargo.lock` committed (`cargo generate-lockfile` in `fuzz/`).
- [x] **Step 5: Commit** `test(engine): randomised request fuzzing, rtsan, CPU benchmark and cargo-fuzz target (S3)`.

---

### Task 13: CI wiring and the dependency allowlist

**Files:** `.github/workflows/{ci.yml,mutation-full.yml,fuzz-nightly.yml}`, `.cargo/mutants.toml`, `scripts/check_engine_deps.py` (+ `scripts/test_check_engine_deps.py`), `scripts/engine-deps-allow.txt`.

- [x] **Step 1: Package lists** — `test` coverage adds `--package iem-engine-proto --package iem-audio-io --package iem-engine`; `lint` wasm clippy adds `-p iem-engine-proto`; `mutants-list`, `mutation-warmup`, `mutation`, `mutation-full.yml` add the three packages.
- [x] **Step 2: `fuzz` job** — adds `cargo test --locked --release -p iem-engine --test props -- --nocapture` and a nightly `cargo fuzz run engine_request -- -max_total_time=60` (toolchain `nightly-2026-09-20`, `cargo-fuzz` via `taiki-e/install-action` pinned).
- [x] **Step 3: `engine` job** (the S0 hand-off's rt-safety job): parity report (`cargo test --locked --release -p iem-engine --test parity -- --nocapture --test-threads 1`, grep `oracle `/`invariance `), rtsan (`rustup toolchain install nightly-2026-09-20 --component rust-src`; `RUSTFLAGS="-Zsanitizer=realtime --cfg iem_rtsan" cargo +nightly-2026-09-20 test -Zbuild-std --target x86_64-unknown-linux-gnu -p iem-engine --test rtsan -- --nocapture`), benchmark (`cargo run --locked --release -p iem-engine --example bench`).
- [x] **Step 4: `supply-chain`** — `python3 scripts/check_engine_deps.py` (normal-dependency closure of `iem-engine` from `cargo metadata --locked` for `x86_64-pc-windows-msvc` and `x86_64-unknown-linux-gnu` equals `scripts/engine-deps-allow.txt`; extra or missing crates fail with their names). Unit tests on a synthetic metadata document.
- [x] **Step 5: `windows` job** — clippy and tests for the three crates (named pipes on the target OS).
- [x] **Step 6: `fuzz-nightly.yml`** — `schedule` 03:17 UTC and `workflow_dispatch`, 30 min of `engine_request`, crash artifacts uploaded; read-only token.
- [x] **Step 7: Local gates** (`check_integrity.py`, script tests) and **commit** `ci: engine crates in coverage, clippy and mutation; engine job (parity, rtsan, bench); fuzz; dependency allowlist`.

---

### Task 14: Push, CI to green, mutation budget

- [x] **Step 1: Push per task group and wait in the foreground** (as S2 Task 9 Step 1): `git push origin dev`, then a bounded poll of the newest `dev` run (≤ 54 × 10 s per call) until every job is terminal; on a failure `gh run view <id> --log-failed`, one fix commit for every failure of the cycle, push again.
- [x] **Step 2: Read the numbers** — `engine` job: `oracle max error`, `invariance max difference`, rtsan result, `bench typical|worst p50/p99/p99.9/max`; `mutants-list`: count. If it fails, extend `shard:` to `ceil(count / 12)` entries in its own commit and push (the coordinator regenerates the required checks).
- [x] **Step 3: Results on #6** — a Slovak comment: runs, numbers, shard matrix, deviations, hand-offs to S4 (#7), S5 (#8), S6 (#9).

---

### Task 15: Playbook

**Files:** `.claude/rules/engine.md`, `CLAUDE.md` (router line), this plan's execution notes.

- [x] **Step 1: The rule** (paths `crates/iem-engine/**`, `crates/iem-engine-proto/**`, `crates/iem-audio-io/**`, `fuzz/**`): RT contract and where each proof lives, sample-accurate commands and the 512 budget, the site format and validation, protocol N/N−1 and additive schemas, persistence files, the rtsan job and its nightly pin, mutation-budget notes, pipe naming per OS.
- [x] **Step 2: Commit and push with the last CI cycle** `docs(s3): playbook rule, plan progress`.

## Execution notes

- **Layout deviations:** the RT command types live in `src/cmd.rs` (not `rt.rs`); the processor's unit tests in `src/rt_tests.rs`; the RT-safety workload shared by `rt.rs` and `rtsan.rs` in `tests/common/mod.rs`; the benchmark is an example (`examples/bench.rs`), because nextest cannot list a `harness = false` bench. `RunConfig.solo_grace` makes the 10 s X2 grace testable. `Exit::Shutdown { faded }` reports whether the fade-out reached silence.
- **Cargo.lock** gained only the new crates: after `cargo metadata` re-resolved unrelated Windows-only `windows-sys` edges, the old package entries were restored and the lock checked with `cargo metadata --locked --offline` (S2 note). `fuzz/Cargo.lock` from `cargo generate-lockfile`.
- **CI cycles** (hosted, `dev`): proto and audio-io green at the first push; site/graph: the synthetic RX 101–132 exceeded a 128-channel map (map now 160); clippy `field_reassign_with_default`, `manual_contains`; rustfmt ran before the `rt` module was declared (fix: `cargo fmt --all -- --check` before every commit); a Python edit that silently matched nothing left a wrong framer assertion (edits now assert their anchor).
- **Findings fixed on the way:** (1) the benchmark's worst case took 640 µs per 32-sample block because every moving EQ band was redesigned on every sample → `iem_dsp::eq::DESIGN_EVERY` = 16 (200 µs after); (2) block sizes differed by 2.4e-16 because a finished crossfade computed `x + 1·(y − x)` → RED/GREEN, bit-identical now; (3) one NaN talkback frame poisoned the interpolator's history for good → RED/GREEN, non-finite samples enter as silence; (4) `interprocess` has no read timeouts on Windows named pipes → pipe integration tests Linux-only, S6 hand-off.
- **Numbers** (run 36246425178): impulse oracle max error 1.8e-15 over 8 random states (2 290 non-zero paths); block-size invariance 0 (bit-identical, 40 commands, 24 000 samples × 23 TX); rtsan clean on the worst-case workload and its self-test violation reported; bench typical p50 20.8 µs (6.3 %), p99 30.3 µs, p99.9 37.7 µs (11.3 %), max 65 µs; worst p50 200.6 µs (60.2 %), p99 218 µs, p99.9 272 µs (81.6 %), max 354 µs; props 20 000 requests; cargo-fuzz 60 s clean; line coverage 78 %.
- **Mutation budget:** `mutants-list` counts 1 031 mutants in the dev→main diff; the matrix grew from 50 to 88 shards (12 per shard). Shard durations are unmeasured until the PR runs them; the required checks on `main` must list `engine` and the 88 shards (S0 plan, Task 15 Step 6).
