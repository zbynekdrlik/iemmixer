# Gen 3 phase C: the engine on Linux with our Dante device — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** iemmixer's engine runs on the Gen 3 box with our Dante device as its audio backend; the server and the UI run beside it, and a member's mix changed in the browser is heard on our TX by a real Dante receiver, measured, at a relaxed latency.

**Architecture:** One process: `iem-engine run --backend dante` hosts the device (phase B's `iem-dante` daemon, now a library) and a pump thread on the media clock that reads the RX rings at the playout point, calls the engine's `Process` every 32 frames and writes per-TX-channel engine rings that the TX thread sends as a new `Source::Engine`, stamped RX time plus a fixed offset. `iem-server` runs as its own unit and talks to the engine over the existing Unix-socket pipes. systemd supervises both; "ide event" stops them.

**Tech Stack:** Rust (workspace toolchain 1.98.1); `iem-dante` (GPL-3.0-or-later) now linked by the GPL `iem-engine` on Linux; `iem_audio_io` (`Process`, `Block`, `StreamStats`, `hist`, `channels::ChannelMap`); tokio only on the device's own thread; systemd units; Python for deploy, guard and drivers; Playwright for the browser step.

**Spec:** `docs/superpowers/specs/2026-10-24-gen3-linux-dante-design.md` (§3 licence, §5.1 shape, §5.2 timing, §5.4 phase C, §5.5 events and clock, §5.6 testing). Ticket #72. Phase B: `docs/superpowers/plans/2026-10-26-gen3-phase-b-audio-through.md`, `.claude/rules/gen3.md`. Engine: `.claude/rules/engine.md`, `.claude/rules/server-engine.md`; browser: `.claude/rules/e2e.md`, `.claude/rules/live.md`.

## Choices made in this plan

1. **One process, the device inside the engine** *(my call, refining design §5.1)*. Phase B's `daemon::media::Media` shares `Arc<[Ring]>` (`Box<[AtomicU64]>` slots), `Arc<TxShared>` with seqlocked `Cell`s and `Arc<RxShared>` between threads; a two-process split would rebuild all of it as a pointer-free shared-memory layout, share or duplicate the clock poller, and tie two units whose restarts tear the rings. The ring's own doc already names "phase C's engine" as its reader, and design §3 allows the GPL engine to link the GPL crate. **Refinement:** the pump lives in `iem-dante` (`stream`, depending on the permissive `iem-audio-io` for the callback contract) and the engine's glue in `iem-engine/src/dante.rs` (Linux, as `asio.rs` is Windows), not in a `dante` module of `iem-audio-io`: that crate is MIT OR Apache-2.0 with no dependencies and `iem-server` dev-depends on it, so a module there would pull the GPL device and its tokio/mdns-sd closure into every user of the audio contract. `iem-dante` still knows nothing about mixing. Cost, counted (`cargo metadata --locked`, 2026-10-26): the engine's Linux closure gains `iem-dante` and 15 crates — bytes, errno, fastrand, flume, if-addrs, lock_api, mdns-sd, mio, scopeguard, signal-hook-registry, socket-pktinfo, socket2, spin, tokio, tokio-macros — allowlisted deliberately in `scripts/engine-deps-allow.txt`; a device fault restarts the engine (its state is saved first). `iem-dante-dev` stays (the standalone device for phase B's tooling, the `ctl` client, the clock probe), built on the same library, never running beside the engine (`Conflicts=`).
2. **What drives `Process`** *(my call)*. A `dante-engine` thread on the media clock (`PtpClock::sleep_until`, `SCHED_FIFO` 40, below the TX/RX threads' 45 and dantesync's 50) calls `Process` with B = 32 frames, unchanged. Block k covers input stamps [kB, kB + B) and runs once its last sample's playout time has passed (media now ≥ kB + B + P, P = the device's `latency_ns` in samples, the playout the checks already use); its outputs are stamped kB + D with **D = B + P + W**, W = 96 samples (1 ms of work margin): design §5.2's "RX media time plus a fixed processing offset". The fpp of either side does not enter: the rings decouple packet size from the block (the card sends us 16-frame packets and asks 32 from us, `gen3.md`); the only bound is that block k's outputs are written before the TX packet stamped kB + D leaves (kB + D + fpp + 250 µs), which holds whenever the block ends within W of its due time; a pump later than W re-bootstraps (silence for the skipped stamps, `Process::discontinuity`, counted). "Relaxed latency" = P + W on top of today's B (D = 320 samples, 3.3 ms, at P = 2 ms); phase F shrinks P and W, B stays 32. While the clock is not ready the pump paces itself (NullRt's rule) on silent inputs and drops its outputs, so commands, meters and `Status` keep flowing while TX sends nothing.
3. **Channel mapping: the identity of channel numbers** *(my call)*. Site RX n = our RX channel n, site TX n = our TX channel n, HIL's spare outputs and returns after them, through the portable `iem_audio_io::channels::ChannelMap` the ASIO path already uses; a channel past the device's count refuses the start (exit 3, as a card that lacks one). Evidence: the topology's `rx`/`tx` are card channel numbers (`topology.rs`), `ChannelMap` is not Windows-bound, and G2 needs our TX channel n to take the card's TX channel n's name at the cutover. Names stay in the ops device config (P6). **Inputs in phase C:** our own TX test sources through self-subscriptions (Live 2) and the IEM card's spare HIL TX (Live 3, as phase B). Not the stage rack: the design lets us subscribe our RX there (§5.6), and a subscription adds a flow at the rack while changing nothing the band hears, but it takes one of the rack's TX flow slots, which the band's receivers share, and phase C's "done when" needs no band input; reading the rack's free flow slots and subscribing real inputs is phase E's. **Outputs:** our TX only, heard by the card's spare HIL returns (the owner's #72 exception), as in phase B.
4. **Server and UI on the box** *(my call)*. `iem-server` (features `standalone,audio`, as CI's e2e job builds it) as `iem-server.service`, plain HTTP on the LAN at port 8080 (no capability, no TLS, no tunnel: members see the tunnel banner, as in CI), `IEMMIXER_CONFIG` in its own writable state folder, `IEMMIXER_ENGINE_PIPE` the engine's socket. **Data:** the public synthetic test band (`config/test-site.toml`, `config/test-roster.toml`), never the PC's live data nor a copy of the band's people: on Linux the server keeps its PIN pepper in a plain owner-only file (`pepper.rs`: "other platforms are test-only"), so no band member's PIN hash belongs on the box before phase D picks its secret store. The test site's numbering sets the device's channel counts (at least 132 RX and 95 TX, plus spare test channels: an ops config change). **PINs:** the engineer's and one test member PIN from the private env, provisioned on stdin at deploy (`pin set-engineer`, `pin set-member`, as the e2e job), never in the repo or on a command line; the test site keeps `pin_changes = false`. **Reaching it:** `http://<box>:8080/`, kept as `GEN3_UI_URL` in the private env; the browser steps use tokens minted on the box from its own JWT secret (`iem-soakclient token`, `live.md`'s one-off check), never a PIN. The page's version line names the build; deploy and the spec check it against the bundle's SHA.
5. **Supervisor: systemd** *(design §5.1's call, made concrete)*. `iem-engine.service` (engine + device, `Conflicts=iem-dante.service`) and `iem-server.service` with no ordering between them (the server reconnects to the engine, as on the PC, so the two stop in parallel). Both run as the box's admin user (phase B's drop-in), `Restart=on-failure`, `TimeoutStopSec=10`, `SendSIGKILL=no` (I8), never enabled at boot (deploy disables all three). The engine adds `RestartPreventExitStatus=2 3` (site or usage, a channel the device lacks), `StartLimitIntervalSec=600` with `StartLimitBurst=3` (the guard's crash loop: three in ten minutes stays down) and `LimitRTPRIO=80`. SIGTERM is the engine's shutdown (save, fade, our RX flows stopped, goodbyes, exit 0) through a new `Ending::Stop`, the path the ASIO session end takes. "ide event" and the live guard stop the stack with one `systemctl stop iem-server iem-engine iem-dante`, inside the event path's 5 s collect bound and never changing its exit code. No alarms on the box yet (no push audience there): remote logs and dead-man timers are phase D's.
6. **Tests.** CI: the pump's decisions and its RT path (unit, `assert_no_alloc`, rtsan), the engine glue with a fake device, and a loopback through a real mix on `ManualClock` compared bit for bit with the `Offline` render. Live 1: the stack up, restarted, stopped. Live 2: a self-loop sine through a member's mix for 10 minutes, closed and opened in the browser. Live 3: `iempc gen3-mix` through the IEM card: the echo's and the engine's round trips each equal over bursts and equal to each other (the test mix adds no delay: zero-lookahead limiter, centre pan gain √2·sin π/4 = 1, pinned in Task 4), every burst with the input closed in the browser not heard, no hole, late packet, pump re-bootstrap or re-anchor in the window, the live guard running throughout.

## Global Constraints

- **P6** (program spec): "Site data never enters the public repo: names, hosts, Dante channel numbers and real track names, PINs, keys." Here: the box runs the public synthetic test band; the device's names live in `iemmixer-ops/site/gen3-device.toml`; the box's address, user, `GEN3_UI_URL` and test PINs only in the private env; the card's spare slot is "spare X" and is never printed; tokens are never printed or put on a command line.
- **Dante rule** (CLAUDE.md, #72 2026-10-24): "Never change Dante subscriptions or any Dante device other than the IEM PC's own card. One exception: for a Gen 3 test, the card's spare HIL return inputs may subscribe to our own 3.0 device's test TX, never on a band member's channel, and the subscription is removed after the test." Our RX subscriptions are ours (design §5.6); no subscription to the stage rack in phase C.
- **Clock** (design §5.5): dantesync is the box's one PTP owner; our device never opens UDP 319/320, never sends PTP, can never be a grandmaster.
- **I7** (program spec): "RT contract: lock-free rings, atomics and fixed messages only; ≤ 512 commands per block; no allocation, lock, syscall or log; CI checks with `assert_no_alloc` and rtsan." It covers `Pump::block` beside `Processor::process` and phase B's TX/RX hot paths.
- **I8** (program spec): "Nothing is force-killed." systemd's SIGTERM is a request the engine answers with its shutdown; every unit has `SendSIGKILL=no`.
- **Owner's event signals** (CLAUDE.md): the only gate; never ask whether an event runs. "ide event" stops the Gen 3 stack; every script here refuses while `EVENT-NOW` exists; the PC side (HIL jobs) runs only in dev time.
- **Tier 0:** no local cargo compilation. Each task ends with a push to `dev` and a green CI run (test, clippy, coverage floor, `mutants-list`) before the next task builds on it. Deploys use the `gen3-bundle-<sha>` artifact with its `SHA256SUMS`.
- **Licence** (design §3): "the engine binary is already GPL through `iem-limiter-mga` (D1a). Dante protocol code may live in a GPL crate the engine links … Server, UI and guard stay separate permissive processes." `iem-server` never links `iem-engine` or `iem-dante` outside its tests; the engine's allowlist and `deny.toml` say so in words.
- **Self-contained runtime** (owner): nothing on the box depends on the dev box at runtime; the dev box only deploys and tests.
- **Logs** name counts, ports, opcodes, channel numbers and peer ids, never a name, an address or a MAC; the control socket's status is numbers only.
- **Versions:** `[workspace.package].version` on `dev` stays above `main` (dev is `2.0.0-dev.44`, main `2.0.0-dev.43` at `8eeaf0b`); after any merge, bump first.

## Review Focus

1. **A late pump or a moving clock never bursts or sends stale audio.** A pump behind more than W re-bootstraps once (the skipped stamps go out as silence, counted as TX holes, `discontinuity()` once); a re-anchor or a clock that went back re-aligns; while the clock is not ready nothing is sent. Tests: Task 1 (`Blocks`, `Pump`), Task 4 (re-anchor on loopback).
2. **The stop is bounded and saves.** SIGTERM saves, fades, stops our RX flows at their transmitters within 1 s, sends the goodbyes and exits 0 inside the unit's 10 s; a device that stops serving saves and exits 1 (restarted); a site channel the device lacks exits 3 (not restarted). Tests: Task 3 (driver, control), Task 6 (unit contents), Live 1.
3. **Sample conversion at the edges.** Full scale, above full scale (saturates, never wraps), NaN (0), 16/24/32-bit flows, and bit for bit against the offline render. Tests: Task 1 (`quantize`), Task 4.
4. **Channel ownership.** TX channels outside the map keep their test sources; `source <tx> engine` only on mapped channels; an engine channel switched to a test source and back; an unsubscribed engine input is silence and counts no hole. Tests: Task 1, Task 2.
5. **The event path stays first.** "ide event" stops server, engine and device in one call within its existing 5 s collect bound and never waits on them; nothing of the stack starts at boot; a live-guard trip stops the stack. Tests: Task 6, Task 7.

---

### Task 1: The pump and the engine's TX rings (`iem-dante::stream`)

**Files:**
- Create: `crates/iem-dante/src/stream.rs`, `crates/iem-dante/src/stream/tests.rs`
- Modify: `crates/iem-dante/Cargo.toml` (`iem-audio-io` dependency; `unexpected_cfgs` check-cfg `iem_rtsan`; the header's "never a dependency of iem-engine" becomes "linked by iem-engine on Linux from phase C"), `src/lib.rs` (module, `#![cfg_attr(iem_rtsan, feature(sanitize))]`), `src/media/ring.rs` (+ tests), `src/media/source.rs` (+ tests), `src/media/rx.rs` (`RxShared::routed`), `src/media/tx.rs` (+ tests), `src/daemon/media.rs` (passes no engine channels until Task 2), `tests/hot_path.rs`, `.cargo/mutants.toml`

**Interfaces:**
- Produces:
  - `Ring::put_at(&self, pos: u64, samples: &[i32])`: stores whatever the reader did (no late or early check, never moves `read_end`), counts `written` only; for the engine's output rings, whose late writes show as TX holes.
  - `Source::Engine`: the TX channel's engine ring at the stamp, a hole sent as silence and counted in the lane's `holes`. `Source::fill(&mut self, stamp: u64, rate: u32, out: &mut [i32], rings: &[Ring], own: Option<&Ring>) -> Fill` (`own` = this TX channel's engine ring); its words round-trip through `source_from`.
  - `TxShared::new(cfg, ip, rings, seed, engine: &[usize])`: one engine ring per TX channel index in `engine` (the standalone device passes none); `TxShared::engine_ring(tx: usize) -> Option<&Ring>`; `set_source(tx, Source::Engine)` is `false` for a channel without one.
  - `RxShared::routed(rx: usize) -> bool` (its route atomic, `Relaxed`).
  - `stream::BLOCK = 32`, `stream::WORK_MARGIN = 96`, `stream::ENGINE_PRIORITY = 40`.
  - `stream::to_f64(s: i32) -> f64` (s · 2⁻³¹, exact); `stream::quantize(x: f64, bits: u8) -> i32` (rounded at `bits`, left-justified, saturating at full scale, NaN → 0).
  - `stream::Map { rx: Vec<usize>, tx: Vec<usize> }` (engine input i ← RX channel index `rx[i]`; output j → TX channel index `tx[j]`).
  - `stream::Timing { block: u64, playout: u64, margin: u64 }` with `offset(&self) -> u64` = block + playout + margin (D).
  - `stream::Blocks` (pure): `bootstrap(now: u64, t: Timing) -> Blocks` (the first start is the smallest multiple of `block` whose input ends at or after `now − playout`: no catch-up at start); `next(&mut self, now: u64) -> Due` with `Due::{Run { start: u64, late: bool }, Wait { until: u64 }, Rebootstrap { skipped: u64 }}`: due when now ≥ start + block + playout, `late` from one block behind, `Rebootstrap` when more than `margin` behind or more than `media::tx::DISCONTINUITY` ahead.
  - `stream::PumpStats` (atomics and an `iem_audio_io::hist::StreamHists` sized for B at the rate): `stream_stats() -> StreamStats` (frames 32, callbacks = blocks, late, missed = skipped blocks, overruns = blocks longer than B, resets = re-bootstraps, running, faulted, fault, max_process_ns, fault_callback_ns stored before `faulted`), `histograms() -> HistSnapshot`, `view() -> PumpView { media: bool, offset: u64, playout: u64, blocks, late, missed, overruns, rebootstraps, max_process_us }`.
  - `stream::Pump<P: Process>`: `new(processor: P, map: Map, timing: Timing, bits: u8, rx: Arc<RxShared>, tx: Arc<TxShared>, stats: Arc<PumpStats>) -> Self` (every buffer allocated here); `media(&mut self, now: u64, epoch: u32) -> u64` (runs every due block at media sample time `now`; the first media step and a new epoch re-bootstrap and call `discontinuity()` once before the next block; returns the media sample time to wake at); `free(&mut self)` (one block on silent inputs, outputs dropped); `block(&mut self, start: u64)` (the RT part: routed RX rings read at `start`, an unrouted one is zeros and reads nothing; `process` in `catch_unwind`: a panic zeroes the block, the processor is never called again, the fault is stored; `quantize`, `put_at(start + offset)`; both histograms; carries `#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]`).
  - `stream::spawn<P: Process + 'static>(clock: Arc<dyn PtpClock>, pump: Pump<P>, stop: Arc<AtomicBool>) -> io::Result<JoinHandle<()>>`: thread `dante-engine`, `ask_realtime(ENGINE_PRIORITY)` (said once either way); clock ready → `media` then `sleep_until(ns_of(wake))`; not ready → `free` paced on `Instant` deadlines a block apart (NullRt's `step`: eight blocks behind resynchronises). Glue, excluded from mutation per function with its reason.

- [ ] **Step 1: Tests**
  - `quantize`: 0.5 at 24 bits is `0x4000_0000`; −1.0 exact; 1.0 and 4.0 saturate at the top 24-bit value; NaN and ±∞ give 0 and the saturation; 16 and 32 bits; `quantize(to_f64(s), 24) == s` for every s on the 24-bit grid in a seeded sweep.
  - `Blocks`: `bootstrapping_never_starts_behind` (the first block is due at or after the bootstrap); due exactly at start + block + playout and not one sample before; `late_is_one_block_behind`; `a_pump_behind_more_than_its_margin_rebootstraps_once_and_counts_the_skipped_blocks`; `a_schedule_far_ahead_rebootstraps`.
  - `Pump` with a test processor (copies input i to output i, counts `discontinuity` calls, panics on demand): `the_output_stamped_t_is_the_input_stamped_t_minus_the_offset` (RX ring filled with a counter); `an_unrouted_rx_channel_is_silence_and_counts_no_hole`; `a_new_epoch_rebootstraps_and_calls_discontinuity_once`; `the_first_media_block_after_free_pacing_calls_discontinuity`; `free_blocks_write_nothing_to_the_engine_rings`; `a_panic_zeroes_its_block_and_the_processor_is_never_called_again` (fault time stored before `faulted`); `stream_stats_map_every_count`.
  - TX: `an_engine_source_sends_its_engine_ring_at_the_stamp`; `an_engine_hole_is_silence_counted_in_the_lane`; `engine_is_refused_for_a_channel_without_an_engine_ring`; `every_source_survives_its_words` (Engine included); `put_at_stores_behind_and_far_ahead_of_the_reader`.
  - `tests/hot_path.rs`: 300 pump blocks (32 inputs, 24 outputs) and 400 TX passes with engine sources under `assert_no_alloc`.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push; CI green. Commit: `feat(dante): the engine pump on the media clock and the engine's TX rings (#72)`.

### Task 2: The device as a library (`iem-dante::host`), engine sources on the control socket

**Files:**
- Create: `crates/iem-dante/src/host.rs`, `crates/iem-dante/src/host/tests.rs`
- Modify: `src/bin/iem-dante-dev.rs` (a thin caller), `src/daemon/media.rs` (`Media::new(.., engine_tx: &[usize])`, `Media::attach_engine(stats: Arc<PumpStats>)`), `src/ctl.rs` and `src/ctl/tests.rs`, `src/lib.rs`, `.cargo/mutants.toml`

**Interfaces:**
- Consumes: Task 1's `TxShared::new(.., engine)`, `PumpStats::view`.
- Produces:
  - `host::HostError::{Setup(String), Run(String)}` (exit 2 and 1, today's `Failure`), `host::exit_code(&HostError) -> u8`.
  - `host::prepare(opts: &daemon::Options, engine_tx: &[usize]) -> Result<Prepared, HostError>`: the binary's present setup (config, state, `check_writable`, interface, `NetInfo`, `Media`, the control socket bound), nothing served yet; `Prepared::media(&self) -> Arc<Media>`, `Prepared::config(&self) -> &DeviceConfig`.
  - `host::Signals::{Stop, Report(Arc<AtomicBool>)}`: SIGTERM/SIGINT end serving (the standalone device), or only set the flag (the engine decides when to stop).
  - `Prepared::start(self, signals: Signals) -> Result<Host, HostError>`: thread `dante-device`, a current-thread tokio runtime running the clock poller, `daemon::serve(..)` until stopped, then the mDNS goodbyes (3 s).
  - `Host::ended(&self) -> Option<String>` (serving ended by itself: the error), `Host::stop(self, within: Duration) -> Stopped` with `Stopped::{Done, Failed(String), StillRunning}` (asks serving to end, waits for the thread; one still running is a warning, never killed).
  - `iem-dante-dev`: `prepare(opts, &[])`, `start(Signals::Stop)`, wait; behaviour and exit codes as in phase B.
  - Control socket: `source <tx> engine` (`ok` on a channel with an engine ring, else `error: no engine on that channel`); `status` gains `"engine": {media, offset, playout, blocks, late, missed, overruns, rebootstraps, max_process_us}` only once an engine is attached (numbers only).

- [ ] **Step 1: Tests:** `exit_codes_are_phase_b_s` (setup 2, run 1); `a_serving_error_is_ended_with_its_reason` (`Host::ended` from the serve result, a pure mapping); ctl: `source_engine_parses`, `source_engine_on_a_channel_without_an_engine_ring_is_refused`, `the_status_has_no_engine_until_one_is_attached`, `the_engine_status_is_numbers_only`; `Media::new` with engine channels builds one ring each and none for the rest.
- [ ] **Step 2:** Implement: move the binary's `run`/`serve`/`goodbye` glue into `host`, unchanged in behaviour; the thread, the runtime and the signals stay glue (per-function `exclude_re` with reasons, as the binary was).
- [ ] **Step 3:** Push; CI green (the `dante-bin` job still builds the standalone device). Commit: `feat(dante): the device as a library the engine can host (#72)`.

### Task 3: The engine's Dante backend (`iem-engine::dante`)

**Files:**
- Create: `crates/iem-engine/src/dante.rs` (`#[cfg(target_os = "linux")]`), `crates/iem-engine/src/dante/tests.rs`
- Modify: `crates/iem-engine/Cargo.toml` (`[target.'cfg(target_os = "linux")'.dependencies] iem-dante`), `src/engine.rs` and `src/engine/tests.rs` (`Backend::Dante`, `RunConfig.dante`, the parser, `USAGE`, `run`'s start), `src/control.rs`, `src/control/stop.rs`, `src/control/tests.rs` (`Ending::Stop`, `Ending::Backend`, `Exit::Backend`), `src/bin/iem-engine.rs` (`--version`; `Exit::Backend` → 1), `src/lib.rs`, `tests/rt.rs`, `tests/rtsan.rs`, `scripts/engine-deps-allow.txt`, `deny.toml` (the `iem-dante` exception's comment: the engine links it on Linux), `fuzz/Cargo.lock`, `.cargo/mutants.toml`, `.config/nextest.toml`

**Interfaces:**
- Consumes: Tasks 1–2 (`Pump`, `Map`, `Timing`, `PumpStats`, `stream::spawn`, `host::prepare`, `Prepared::start`, `Host`).
- Produces:
  - CLI: `iem-engine run --backend dante --site <site.toml> --state-dir <dir> --pipe <socket> --dante-config <device.toml> (--dante-iface <name> | --dante-ip <IPv4>) [--dante-control <socket>]` (default `iem_dante::ctl::SOCKET`). `--block` and `--sine` are refused with it (as with asio); a `--dante-*` flag without `--backend dante` is refused; off Linux `--backend dante` is a usage error before any state. `RunConfig.dante: Option<DanteArgs { config: PathBuf, place: DantePlace, control: PathBuf }>` (portable; on Linux it becomes `iem_dante::daemon::Options`, the same three fields).
  - `iem-engine --version`: `iem-engine <engine_build()>` (`<version>+<GITHUB_SHA or local>`).
  - `dante::check(cfg: &DeviceConfig, topo: &Topology, hil: &[u16]) -> Result<Map, EngineError>` (pure): the rate must be 96 000 (else `Card`); `ChannelMap::new(&topo.rx, &outputs, cfg.rx.len(), cfg.tx.len())`, `with_hil_return` under `--test-signal`; a channel the device lacks is `EngineError::Card` (exit 3); the result as card indices = our channel indices.
  - `dante::start(args: &DanteArgs, topo: &Topology, hil: &[u16], processor: Processor) -> Result<(Box<dyn Driver>, usize), EngineError>` (glue): `host::prepare(.., &map.tx)` (Setup → `Usage`, exit 2; Run → `Io`, exit 1), `check`, every mapped TX channel's source set to `Source::Engine`, `Media::attach_engine`, the pump spawned with `Timing { block: 32, playout: to_samples(cfg.latency_ns), margin: WORK_MARGIN }`, the host started with `Signals::Report`; one start line: topology, inputs, outputs, offset, playout.
  - `dante::Device` (trait the engine's driver stops through; the real `Host` behind it, a fake in tests): `ended(&self) -> Option<String>`, `stop(self: Box<Self>, within: Duration) -> Stopped`.
  - `dante::DanteDriver` (implements `control::Driver`): `stats()` = `PumpStats::stream_stats`; `histograms()`; `ending()` = `Ending::Stop` once the reported flag is set, `Ending::Backend(why)` once the device ended by itself; `stop()` ends the pump (its flag, joined within 1 s, a warning if not) and then the device (`Device::stop`, `STOP_WITHIN` 7 s), and returns `Released`.
  - `control::Ending::Stop` → the shutdown path (save, fade, release, exit 0; info line `a stop was asked (SIGTERM or SIGINT)`); `Ending::Backend(String)` → save, release, `Exit::Backend` (exit 1).
  - `scripts/engine-deps-allow.txt` gains `iem-dante` and the 15 crates of Choice 1, each under one reviewed comment line.

- [ ] **Step 1: Tests**
  - `engine/tests.rs`: `dante_needs_its_config_and_one_place`, `dante_refuses_block_and_sine`, `dante_flags_need_the_dante_backend`, `off_linux_dante_is_a_usage_error_before_any_state` (`#[cfg(not(target_os = "linux"))]`, into the nextest priority list beside its asio twin), `version_names_the_build`.
  - `dante/tests.rs`: `channel_numbers_map_to_the_same_device_channels` (site RX 101 → our RX index 100); `hil_returns_follow_the_topology_rx`; `a_site_channel_the_device_lacks_is_a_card_refusal`; `a_device_at_another_rate_is_refused`; driver with a fake device and a stopped pump: `a_reported_signal_is_a_stop_ending`, `a_device_that_ended_is_a_backend_ending`, `stop_ends_the_pump_before_the_device`, `stats_are_the_pumps`.
  - `control/tests.rs`: `a_stop_ending_saves_fades_and_exits_shutdown`, `a_backend_ending_saves_and_exits_backend`.
  - `tests/rt.rs`: `the_dante_pump_block_does_not_allocate` (the real `Processor` on the worst-case scenario, rings for its channels, 300 blocks); `tests/rtsan.rs`: `the_dante_pump_block_is_realtime_safe` (Linux, the `engine` job's rtsan step).
  - `scripts/check_engine_deps.py` passes (the `supply-chain` job); `cargo metadata --locked --manifest-path fuzz/Cargo.toml` resolves.
- [ ] **Step 2:** Implement. `dante::start` is glue (excluded with its reason, as `asio.rs`); every decision is in `check`, `DanteDriver` and Task 1's `Blocks`/`Pump`.
- [ ] **Step 3:** Push; CI green (also `windows`, which compiles the engine without the module, and `engine`'s rtsan and bench). Commit: `feat(engine): the Dante backend on Linux, the device in the engine's process (#72)`.

### Task 4: A mix through our device on loopback, bit for bit

**Files:**
- Create: `crates/iem-engine/tests/dante_loopback.rs` (Linux)
- Modify: `.config/nextest.toml` (its bounded test into the `mutants` priority list)

**Interfaces:**
- Consumes: Tasks 1–3; phase B's `tests/loopback.rs` pattern (`ManualClock`, 127.0.0.1, the TX and RX passes stepped as their threads step them, our `Own` resolver).

- [ ] **Step 1: Tests** (an inline site, the smallest `[engine]` table `compile` accepts: inputs on RX 1 and RX 2, the engineer's stereo mix on TX 3/TX 4 hearing both, centre pan; a device config built in code: 6 RX, 4 TX, 96 kHz, 24 bits, latency 1 ms; both processors with `fade_in_ms: 0`):
  - `the_mix_through_our_device_is_the_offline_render_bit_for_bit`: TX 1 the counter and TX 2 a sine feed RX 1/RX 2 (self-subscriptions), the engine's TX 3/TX 4 feed RX 5/RX 6; over one simulated second RX 5 and RX 6 at stamp T equal `quantize(offline(T − D), 24)` for every T, where `offline` is `Offline { block: 32 }` on a second `Processor` of the same state over the samples RX 1/RX 2 held.
  - `the_test_mix_adds_no_delay`: an impulse on RX 1 at 0 dB level, centre pan, flat EQ, unlimited: the first non-zero output sample is the impulse's own stamp + D, of equal value (pins Live 3's `engine == echo`).
  - `a_re_anchor_rebootstraps_the_pump_and_restarts_the_fade`: an epoch change → `rebootstraps` 1, `discontinuity` once, TX holes only for the skipped stamps.
  - `nothing_is_sent_before_the_clock_is_ready_yet_commands_apply`: a not-ready clock: TX packets 0, a level command reaches the processor (the free pace drained it).
  - Every wait bounded (5 s), the first in the nextest priority list.
- [ ] **Step 2:** Push; CI green. Commit: `test(engine): a mix through our device on loopback, bit for bit (#72)`.

### Task 5: The Gen 3 bundle in CI

**Files:**
- Modify: `.github/workflows/ci.yml` (`dante-bin` becomes `gen3-bin`), `.claude/rules/ci-rust-toolchain.md` only if the required-checks note names the job

**Interfaces:**
- Produces: artifact `gen3-bundle-<sha>` (pushes to `dev` and `main`, 14 days): `iem-engine`, `iem-server`, `iem-dante-dev`, `iem-soakclient`, `BUILD` (the SHA), `SHA256SUMS` over the four binaries and `BUILD`.

- [ ] **Step 1:** `gen3-bin` (`needs: [test, wasm]`): downloads `wasm-dist` into `crates/iem-ui/dist`; `cargo build --locked --release -p iem-engine -p iem-soakclient`, `-p iem-dante --bin iem-dante-dev`, `-p iem-server --features standalone,audio`; each binary an x86-64 ELF; `iem-dante-dev --version` = `iem-dante-dev <version> <GITHUB_SHA>`, `iem-engine --version` = `iem-engine <version>+<GITHUB_SHA>`; the server started on a scratch copy of the test site and port answers `/api/version` with a `git_hash` that is a prefix of `GITHUB_SHA` (then stopped with SIGTERM, its exit 0 checked); sums written and printed; upload on pushes only.
- [ ] **Step 2:** Regenerate `main`'s required checks from the job list (`gen3-bin` replaces `dante-bin`).
- [ ] **Step 3:** Push; CI green. Commit: `ci: the Gen 3 bundle for the box (#72)`.

### Task 6: The units and the stack's deploy

**Files:**
- Create: `scripts/gen3/iem-engine.service`, `scripts/gen3/iem-server.service`
- Modify: `scripts/gen3/iem-dante.service` (`Conflicts=iem-engine.service`), `scripts/gen3/deploy.sh`, `scripts/gen3/test_deploy.py`
- Private: `iemmixer-ops/site/gen3-device.toml` (channel counts covering the test site plus spare test channels: at least 136 RX and 96 TX), `GEN3_UI_URL`, `GEN3_ENGINEER_PIN`, `GEN3_MEMBER_PIN` in the private env; the box's sudoers lines (below), added the way phase B's were

**Interfaces:**
- Produces:
  - `iem-engine.service`: `ExecStart=/opt/iemmixer/bin/iem-engine run --backend dante --site /var/lib/iemmixer/site/iemmixer.toml --state-dir /var/lib/iemmixer/engine --pipe /run/iemmixer/engine.sock --dante-config /etc/iemmixer/dante-device.toml --dante-iface ${IEM_DANTE_IFACE}` (`EnvironmentFile=/etc/iemmixer/iem-dante.env`); `User=` from `iem-engine.service.d/10-user.conf`; `StateDirectory=iemmixer iem-dante` (the device's state file and its subscriptions stay where phase B kept them); `RuntimeDirectory=iemmixer iem-dante`, mode 0700 (the engine's pipes, the device's control socket at its phase B path); `LimitRTPRIO=80`; `Conflicts=iem-dante.service`; `Restart=on-failure`, `RestartSec=2`, `RestartPreventExitStatus=2 3`, `StartLimitIntervalSec=600`, `StartLimitBurst=3`; `TimeoutStopSec=10`, `SendSIGKILL=no`.
  - `iem-server.service`: `ExecStart=/opt/iemmixer/bin/iem-server`, `Environment=IEMMIXER_CONFIG=/var/lib/iemmixer/site/iemmixer.toml IEMMIXER_ENGINE_PIPE=/run/iemmixer/engine.sock PORT=8080 RUST_LOG=info`; the same user drop-in; `StateDirectory=iemmixer`; no `After=`/`Requires=` on the engine; `Restart=on-failure`, `TimeoutStopSec=10`, `SendSIGKILL=no`.
  - `deploy.sh --host <user@host> --bundle <dir> --sha <40 hex> --config <ops device config> --iface <name> [--start engine|device] [--pins]` (replaces `--artifact`/`--sha256`):
    - refusals as phase B (the flag at the start and right before the install; a `--host` that is not `GEN3_HOST` warns; never root; `sudo -n -l systemctl stop iem-server iem-engine iem-dante` must pass before anything is copied); the bundle's `BUILD` must equal `--sha` and its `SHA256SUMS` must hold, on the dev box and again on the box;
    - `systemctl stop iem-server iem-engine iem-dante`; scp into `~/.cache/iem-gen3-deploy` (overwritten, never deleted); `sudo -n install -D` the four binaries to `/opt/iemmixer/bin/`, the drop-ins before their units, the three units, the device config and the env; `sudo -n install -d -o <user> -m 0700 /var/lib/iemmixer/site` and the test site and roster copied there by the user itself (the server writes beside them);
    - `--pins` (server stopped): `iem-server pin set-engineer` and `pin set-member <id>` for every roster person, each PIN on stdin through ssh from the private env, never argv, never printed;
    - `daemon-reload`, `disable iem-dante iem-engine iem-server` (none at boot), `start iem-engine iem-server` (or `start iem-dante` with `--start device`); a flag that came meanwhile stops all three and exits 1;
    - read back: the installed sums, both `--version` lines, `is-active` after `GEN3_SETTLE_S`, and on the box `curl -sf http://127.0.0.1:8080/api/version` naming the SHA.
  - Sudoers additions (exact command lines, `NOPASSWD`): the new `install -D` lines, `install -d -o <user> -m 0700 /var/lib/iemmixer/site`, `systemctl stop iem-server iem-engine iem-dante`, `systemctl disable iem-dante iem-engine iem-server`, `systemctl start iem-engine iem-server`, `systemctl start iem-dante`, `systemctl restart iem-engine`.

- [ ] **Step 1: Tests** (`test_deploy.py`, stand-in ssh/scp, `PC_ENV` a temporary file): the order (stop, copy, sums again, install, site, disable, start); refusals with nothing changed after them (the flag, a sum mismatch on either side, `BUILD` not `--sha`, `sudo -l` refused); `--pins` sends each PIN only on stdin and never prints it; `--start device` starts only `iem-dante`; a late flag stops all three; the units' contents (`Conflicts` both ways, `LimitRTPRIO`, runtime folders, the stop bound with `SendSIGKILL=no`, `RestartPreventExitStatus=2 3`, `StartLimitBurst=3`, no ordering between engine and server, `--site`/`--pipe`/`IEMMIXER_ENGINE_PIPE` agreeing).
- [ ] **Step 2:** Implement (`set -euo pipefail`).
- [ ] **Step 3:** Push; CI green (`integrity` runs the Python tests). Commit: `feat(gen3): the engine and server units and the stack's deploy (#72)`.

### Task 7: "ide event" and the live guard stop the whole stack

**Files:**
- Modify: `scripts/iem-pc/iempc_event.py`, `scripts/iem-pc/test_iempc_event.py`, `scripts/gen3/live_guard.py`, `scripts/gen3/test_live_guard.py`

**Interfaces:**
- Produces: one shared command `sudo -n systemctl stop iem-server iem-engine iem-dante`: the event's Gen 3 stop (started right after the flag, collected within `GEN3_STOP_S`, 5 s, never the event's exit code, every exception caught, its log in `gen3-stop.log` as before) and the live guard's trip stop (within `STOP_S`). The guard's 10-second line adds the pump's `late`, `missed` and `rebootstraps` from `status.engine` when present (reported, never a trip: the guard watches our traffic).

- [ ] **Step 1: Tests:** `Gen3StopTests` stop all three in one call and keep every phase B property (a stop still running at 5 s leaves the event path at once; no `GEN3_HOST` is one info line); the guard's trip runs the same command; an absent `engine` object changes nothing; engine counts never trip.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push; CI green. Commit: `feat(gen3): "ide event" and the live guard stop the whole stack (#72)`.

### Task 8: The mix loop driver and the browser step

**Files:**
- Create: `e2e/playwright.gen3.config.ts`, `e2e/tests/gen3/member-mix.spec.ts`
- Modify: `e2e/playwright.config.ts` (`testIgnore` adds `**/gen3/**` in both projects), `.github/workflows/ci.yml` (the e2e job's `--list` step also lists the gen3 config), `scripts/iem-pc/iempc_gen3.py`, `scripts/iem-pc/test_iempc_gen3.py`, `scripts/iem-pc/iempc.py` (`gen3-mix`: dev time, locked, a PC command, `refuse_while_running(ip, "gen3-mix")` first)

**Interfaces:**
- Consumes: `e2e/tests/live/support/live.ts` fixtures (`openLive`, `expectBuild`, the relay, the console guard; env read in `support/env.ts`), passed the box's values in the same `LIVE_*` variables (`LIVE_BASE_URL` = `GEN3_UI_URL`, `LIVE_SHA`, `LIVE_TOKENS`, `LIVE_MEMBER`, `LIVE_TEST_INPUT`); not the burst desk (the box's engine feeds no band member).
- Produces:
  - Spec titles: `the box serves the deployed build` (the page's version line names `LIVE_SHA`'s short hash); `the member opens the test input in their mix` (the input's level to 0 dB and unmuted, IEM VOL 0 dB and unmuted, through the page; a runner-side `PageSocket`'s `applied()` then its state shows both); `the member closes the test input in their mix` (level off); each a clean console.
  - `iempc gen3-mix --slot X --member <id> --box-input <id> --bursts N [--input mic1] [--dbfs -30] [--ttl 2]` (`--input`, `--dbfs`, `--ttl` are `gen3-loop`'s, the PC's burst; X never printed): R = the box input's RX channel and T = the member's mix's first TX, both read from `config/test-site.toml`'s `[engine]` table (the file deploy put on the box), and the same ids go to the spec as `LIVE_MEMBER`/`LIVE_TEST_INPUT`; preflight as `gen3-loop` plus `status.engine.media` 1 and a live TX flow carrying T; an engineer and a member token minted on the box (`iem-soakclient token --jwt-secret-file /var/lib/iemmixer/site/secrets/jwt_secret --sub <id> [--engineer] --seconds 900 --out <new file>`), copied to an owner-only file, removed on the box at once and on the dev box at the end; then in one HIL job (as `gen3-loop`): `ctl source T echo R D` (D = `status.engine.offset`) and N bursts → echo round trips; `ctl source T engine`; spec "closes" and N bursts; spec "opens" and N bursts → engine round trips; spec "closes" again; `job-end` (after any failure too, never once the flag exists).
  - Verdict line `{"gen3_mix": {pass, bursts, echo, engine, closed_heard, difference, offset, holes, late, rebootstraps, re_anchors, failed, job_end}}`, numbers and fixed words; pass: every echo and engine burst heard and each set equal, engine minus echo exactly 0, no closed burst heard, and between the counts no hole (RX R, the TX flow), late packet, pump re-bootstrap or re-anchor.

- [ ] **Step 1: Tests** (`test_iempc_gen3.py`, a fake card and a stand-in ssh and `npx`): R and T come from the test site for a given member and input (an id the site lacks refuses before any call); the verdict passes only the exact shape (an engine round trip one sample off fails `difference`, a closed burst heard fails, a re-bootstrap fails); a spec step that fails ends the job with `job_end` set and no further burst; the token file is never printed and is removed on both sides on every exit; the command is refused while a live run or soak may run. The spec: `--list` of the gen3 config in CI.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** Push; CI green; `mutants-list` within the shard matrix (resize before the PR if not). Open the dev→main PR (auto-merge when every gate is green). Commit: `feat(gen3): the mix loop driver and the browser step (#72)`.

### Task 9: Live 1 — the stack on the box

- [ ] **Step 1:** Dev time only (after "event skončil"). Ops first: the device config's channel counts (Task 6), the private env keys, the sudoers lines. Start `live_guard.py` for the session.
- [ ] **Step 2:** `deploy.sh --bundle <gen3-bundle of Task 8's merged push> --sha <sha> --config <ops device config> --iface <Dante interface> --pins`. Expected: `iem-engine` and `iem-server` active, `iem-dante` inactive, all three disabled; the engine's start line names the topology hash, D and P; the server log says the roster reached the states.
- [ ] **Step 3:** `ctl status` for 10 minutes once dantesync is locked: `engine.media` 1, blocks rising 3 000 a second, `late`, `missed`, `rebootstraps` 0, `max_process_us` below 333. Dante Controller lists our device with the new channel counts and no new gap in the journal's minute lines.
- [ ] **Step 4:** From the dev box: Playwright `the box serves the deployed build` (`-c e2e/playwright.gen3.config.ts`, tokens minted and removed as `gen3-mix` does): the version line names the SHA, clean console.
- [ ] **Step 5:** `sudo systemctl restart iem-engine`: the journal shows the save, the fade, the flow stops, the goodbyes, exit 0 within 10 s; the server reconnects by itself. `systemctl stop iem-server iem-engine iem-dante` ends all within 10 s. Record the numbers (no names) on #72.

### Task 10: Live 2 — the self-loop through a member's mix (our device only)

- [ ] **Step 1:** R and T as `gen3-mix` reads them (the box input's RX, the member's first TX). `ctl source S sine 1000 -30` on a spare TX channel S outside the site; our RX R subscribed to our TX S and a spare RX Q outside the site subscribed to our TX T, both with a raw 0x3010 on our own device (netaudio refuses self-subscriptions, `gen3.md`); `ctl check Q sine 1000`.
- [ ] **Step 2:** Spec "opens": Q's check hears the sine; 10 minutes with no discontinuity, Q's holes 0, the pump's `late`, `missed`, `rebootstraps` 0.
- [ ] **Step 3:** Spec "closes": Q goes blind within one second; "opens" again: heard. Remove the three subscriptions, `ctl source S silence`. Record on #72.

### Task 11: Live 3 — through the IEM card, the browser in the loop (phase C done)

- [ ] **Step 1:** Read only, before: the card's two spare HIL returns' subscriptions (to restore) and its RX latency.
- [ ] **Step 2:** Our RX R (the box input's channel, as `gen3-mix` reads it) subscribed to the card's spare HIL TX X (resolved through the card's ARC, phase B Task 9b); R reads Receiving.
- [ ] **Step 3:** Subscribe both of the IEM card's spare HIL returns to our test device's TX T for the test, with a raw ARC 0x3010 to the card in dev time (phase B Task 11's way, the owner's #72 exception and the ROZHODNUTÉ of 2026-10-26: no owner click); nothing else of the card changes.
- [ ] **Step 4:** `iempc gen3-mix --slot X --member <id> --box-input <id> --bursts 5` (a member who plays in the box's active band, mix without peers). Pass as Task 8 states; the live guard did not trip. Record D, the echo and engine round trips, and the round trip less D against phase B's base, as phase F's starting numbers.
- [ ] **Step 5:** Set the returns back to what Step 1 read (the same raw 0x3010, each to its own spare TX); read back equal to Step 1. Unsubscribe R; spec "closes"; stop the stack.
- [ ] **Step 6:** Record on #72 (numbers only) and tick phase C. Update `.claude/rules/gen3.md` (phase C: `stream`, `host`, the units, deploy, `gen3-mix`; its `paths:` gain `crates/iem-engine/src/dante.rs`, `crates/iem-engine/src/dante/**`, `crates/iem-engine/tests/dante_loopback.rs`, `e2e/tests/gen3/**`, `e2e/playwright.gen3.config.ts`), `.claude/rules/engine.md` (the Dante backend, `Ending::Stop`/`Backend`, the allowlist), and the CLAUDE.md router's Gen 3 line. Commit: `docs(rules): gen3 phase C (#72)`.

## Risks

- **A stock kernel at a 333 µs wake:** the pump may wake late under load. W (1 ms) absorbs it; lateness shows as `late`/`rebootstraps` and TX holes, never as a burst. Phase F brings PREEMPT_RT, `mlockall` and the core plan.
- **rtsan over a bigger closure:** the `engine` job's rtsan build now compiles tokio and mdns-sd with the sanitizer flags. If a crate refuses them, gate the dependency's use out of the rtsan test, never drop the test.
- **More channels on the device:** 136 RX and 96 TX mean more ARC pages and `_netaudio-chan` records than phase B ran with (16 TX). Pages already end early by size (`gen3.md`); watch the gap lines and Dante Controller's listing in Live 1.
- **The clock under load:** phase B saw dantesync miss our 200 ms reply bound under 3 000 packets a second each way (holdovers, no re-anchor). Phase C adds the engine's flows; a re-anchor during Live 3 fails the verdict and is re-run, and stays phase D's input.
- **Server on Linux:** its PIN pepper is a plain owner-only file there ("test-only"), acceptable only for the synthetic test band. Phase D decides the box's secret store and a service user of its own before any band data reaches the box.
