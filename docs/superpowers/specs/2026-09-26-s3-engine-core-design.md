# S3 — Engine core on simulated audio: design note

**Ticket:** #6 (program #1). **Spec:** `2026-09-24-iemmixer-gen2-program.md` §2.2–2.5 (I1–I10), §3.1, §3.3 (A1–A13), §3.4 (X1–X4, X13–X15, Q1, Q4), §3.5, §4.4. **Plan:** `docs/superpowers/plans/2026-09-26-s3-engine-core.md`. **Inputs:** the S2 kernels (`iem-dsp`, `iem-limiter-mga`) and the S0/S2 hand-offs on #6.

> **Model superseded (#20):** §3.1–3.4 (graph, sends, buses) describe the REAPER-shaped model that `2026-09-26-engine-model-rework-design.md` replaced; the rest stands.

## 1. Goal

The whole engine except the ASIO driver: a process that compiles the §3.1 graph from `site.toml`, mixes it in one real-time callback at **B = 32, 96 kHz (333 µs, I2)**, owns and persists the mix state, talks to the server over local pipes, and survives faults the way §2.4 prescribes. It runs on two backends: `Offline` (deterministic, any block size; the parity harness) and `NullRt` (paced real time; E2E and soak). S6 adds ASIO behind the same trait.

Done means: the impulse oracle and block-size invariance hold in hosted CI, the process path is proven allocation-free (`assert_no_alloc`) and clean under rtsan, and `process()` at B = 32 is measured against the 333 µs period.

## 2. Crates

| Crate | Licence | Role | Dependencies |
|---|---|---|---|
| `iem-engine-proto` | MIT OR Apache-2.0 | ids, `MixState`, `Cmd`/`Reply`/`Event`/`Hello`, N/N−1 negotiation, frame codec | serde, serde_json |
| `iem-audio-io` | MIT OR Apache-2.0 | `Process` trait, `Offline`, `NullRt`, WAV codec | none |
| `iem-engine` (lib + bin) | GPL-3.0-or-later (links the limiter, D1) | site, graph, RT processor, control core, persistence, pipes, media | proto, audio-io, iem-dsp, iem-limiter-mga, rtrb, triple_buffer, interprocess, serde, serde_json, toml, sha2, tracing |

The engine dependency closure is pinned by an allowlist (`scripts/engine-deps-allow.txt`, checked in `supply-chain`, §5.2): a new crate in the engine needs a reviewed line.

## 3. Decisions

### 3.1 Topology in `site.toml` (I4)

The engine reads only the `[engine]` table; the REAPER-era server keys in the same file stay untouched until S5 (`iem-core`'s config ignores `engine`).

- `[[engine.inputs]]` `id`, `rx` (1 = mono, 2 = stereo card channels), `talkback = true` on the one input that carries talkback (A4).
- `[[engine.buses]]` `id`, `kind` = `output` (stereo TX, EQ, limiter) | `stems` (group, no TX) | `translator` (mono TX, A10) | `master` (A11), `tx`.
- `[[engine.sends]]` `from` × `to` (a cross product, one table per send family) and `tap` = `pre` (mode 3, inputs only) | `post` (mode 0, buses only).
- `[engine] channels` (the card's channel map) and `engineer` (the bus with the fixed listen tap, X3).

**Validation** refuses: duplicate ids (inputs and buses share one namespace), unknown references, channel counts per kind, channels outside the map, a card channel used twice (RX or TX), a second send for one (from, to) pair, a pre tap from a bus or a post tap from an input, sends into `master`, a cycle, more than one master or talkback input. `config/test-site.toml` gains the §3.1 shape with synthetic channels (RX 101–132, TX 71–92 as today plus translator 93): 24 inputs on 32 RX, 23 TX, 268 sends, 44 EQs, 10 limiters.

### 3.2 Graph and one RT thread (I5, I7)

- Compiled once per run: inputs in site order, buses in a topological order (Kahn), incoming sends grouped per bus, and for X13 the set of TX buses reachable from each input.
- `Processor` (the `Process` implementation) owns every buffer and node, preallocated for segments of ≤ 256 samples; `process()` splits the block at command timestamps and at 256, so output depends only on sample indices (§3.5 block sizes, bit-exact by construction).
- **Per input:** RX (mono → L = R, A2) → sanitiser (X1) → test signal replaces the input (X13) → processing crossfade 20 ms (trim ramp → `Equalizer<2>`, A4, Q3) → `+= 0.379934·talkback` on the talkback input → mute gate 5 ms (A3: every tap) → sanitiser → **P** (pre-fader tap); master reads `P·v·gains(p)` (A11).
- **Per bus, topological order:** Σ incoming sends (`StereoGain`: `v·(1−m)·gains(p)`, A5; pre reads P, post reads the source bus's **O**, A6/A9) → by kind:
  - stems: EQ → fader stage → **O** (A7);
  - output: EQ → limiter (A13, the engineer listen tap here, X3) → fader stage → **O** (bus-to-bus reads it, A9; the member listen tap reads it through a listen-path limiter) → TX stage (A8);
  - translator: fader stage → O → `(O_L + O_R)/2` on its one TX channel (A10) → TX stage;
  - master: Σ input and stems-bus post-fader outputs → fader stage → TX stage (A11).
- **TX stage:** Q1 safety stage = the MGA core at 0 dB with 100 % link → clamp ±1.0 (±0.1 while a test signal reaches this bus, X13) → engine fade (500 ms fade-in at start, fade-out on `Shutdown`) → the TX channel. Only topology TX channels exist in the backend's output (A1; S6 zeroes every other device channel).
- **Fader stage** = `StereoGain(v, m, p)` for every node, so the track law is the send law (A5); input nodes never mute there (their mute is the gate).
- **Ramps (X15):** gain/pan 10 ms, mute 5 ms, EQ 20 ms, processing 20 ms, fade-in 500 ms. Bus limiter: enabling and lowering are instant, raising ramps over 10 ms in dB, disabling crossfades 10 ms (the S2 policy, §4.4). The 50 ms preset ramp is not built (§6).
- **Budget:** ≤ 512 commands per block; a batch is one ring chunk and is applied whole or waits for the next block. Steady-state fast paths (`StereoGain::steady`, `Equalizer::is_identity`, two small `iem-dsp` additions) keep the per-sample work to the moving parameters, and a moving EQ band is redesigned every 16 samples of its own ramp (exactly at the target on the last step): redesigning 220 moving bands on every sample cost 640 µs per 32-sample block.
- **X1:** every input and node block is sanitised; a trip zeroes the block, resets the node's EQ and limiter, counts, and raises an alarm.

### 3.3 Commands, state, solo (I6, X2, X3, X13)

- The **control core** (`Core`) is pure: `apply(Request) → Result<Outcome>`, where `Outcome` holds the new revision, the `Change`s and one group of RT commands (indices and linear values, `Copy`). It caps every field (non-finite → error; ranges clamped; ids ≤ 64 bytes and known; batches ≤ 256 ops; solo ≤ 64 sources) — the pipe carries no token (§2.3).
- **State:** `MixState` (inputs, buses, sends by stable id) plus transient state (solo, listen, test signal); every change bumps `rev`; `Delta{rev, origin, changes}` carries whole entities; `GetState` resyncs.
- **Solo (X2):** per scope bus B, sends into B and into the stems buses feeding B are muted (5 ms) unless their source is soloed or is itself in B's tree. Solos clear 10 s after the controlling connection drops (reconnect within 10 s keeps them); the 10 s-after-the-member's-last-connection rule is the server's (S5 hand-off).
- **Listen (X3):** slot 0 = the engineer bus after its limiter, before fader and mute; slot 1 = one member bus after mute through a listen-path limiter (MGA at 0 dB). A second distinct member gets `no_source`.
- **Test signal (X13):** only with the `--test-signal` launch flag; a sine replaces one input at ≤ −20 dBFS for ≤ 120 s (both capped), 50 ms fades, never persisted; while it sounds every TX bus reachable from that input is clamped at 0.1.

### 3.4 Meters and taps

- **Meters:** the RT thread accumulates peaks (inputs at P, buses at O), limiter GR and X14 active samples, trips and overruns, and every 3 200 samples (30 Hz at 96 kHz) fills the input side of a `triple_buffer` in place and publishes. The control loop forwards changed frames as `Meters`.
- **Taps (X3, X4):** the RT thread pushes 96 kHz stereo into an `rtrb` ring per slot (overruns counted, never blocking); the media thread decimates 2:1 with a 79-tap Kaiser half-band FIR (β = 10.06, each polyphase branch at unity: ±1e-4 dB to 20 kHz, ≥ 99.6 dB from 28 kHz, measured in numpy) and sends 20 ms 48 kHz f32 frames on the media pipe. Talkback frames from the server are interpolated with the same filter (non-finite samples enter as silence) into a 120 ms ring the RT thread reads with a 5 ms gate.

### 3.5 Persistence (§2.4, I10)

- Files in the state dir: `current.json`, `gen-<seq>.json` (20 kept), `baseline.json`. Each is `{format, schema, sha256, payload}`, where `sha256` covers the payload's raw bytes (`serde_json` `RawValue`), so re-serialisation never matters.
- Save: temp file → fsync → the previous `current.json` becomes the next generation → rename → prune → fsync the directory. Debounce: 1 s after the last change, ≤ 5 s after the first, at once on `Shutdown` and `SaveNow`; the X14 counters ride along (Q4: persist until reset).
- **Load chain:** current → generations newest first → baseline → defaults with every TX bus muted and an alarm. A file that fails its checksum, its format or its schema is skipped and reported. Entries for ids the topology no longer has are dropped; missing ones get defaults; every value is capped as if it came from the pipe.
- **Additive schemas:** readers ignore unknown fields and default missing ones (no `deny_unknown_fields` in the proto crate); `schema` only goes up.

### 3.6 Pipes and protocol (§2.3, I1)

- `interprocess` local sockets, sync threads: a Unix socket file on Linux (CI), a named pipe on Windows (S6 adds reject-remote and the DACL). Control: 4-byte LE length + JSON (≤ 1 MiB, else the connection is closed); media: fixed 20-byte binary header + f32 LE.
- `Hello{proto}` from the client; the engine speaks `min(ours, theirs)` when `theirs ≥ ours − 1`, else refuses (N/N−1). Unknown fields are ignored; an unknown command is a typed error reply, not a disconnect.
- One controlling connection (a new one supersedes the old: a restarted server never waits for a stale socket) plus up to 7 observers that may only read.

### 3.7 Crash model without the driver (§2.4)

- Panics unwind. The backend calls `process()` inside `catch_unwind`; a panic zeroes that block's outputs, the stream stops calling the processor and keeps outputs at zero, and marks itself faulted. The control loop sees it within 10 ms, saves, emits `Alarm{fault}` and `DriverReleased`, and the process exits non-zero for the guard to respawn (S6).
- `Shutdown`: save → 50 ms fade-out → stop the backend → `DriverReleased` → exit 0.
- Fault injection: the `--fault-injection` launch flag enables `InjectFault` (a panic on the RT thread); automation injects panics only.

### 3.8 Real-time proof

- `tests/rt.rs` (`assert_no_alloc`): `process()` with commands, taps, meters, talkback, test signal and fault-free sanitiser trips.
- **rtsan** (`-Zsanitizer=realtime`, merged in rustc Nov 2025, runtime shipped for x86_64 Linux): `process()` carries `#[sanitize(realtime = "nonblocking")]` under `cfg(iem_rtsan)`; the `rt-safety` job builds with a pinned nightly and `-Zbuild-std` and runs `tests/rtsan.rs`, including a self-test that a violation is detected.
- **CPU:** `examples/bench.rs` times `process()` at B = 32 on the hosted runner for a typical and a worst case (every EQ band and send moving, limiters in GR, both taps, talkback, test signal, 512 commands per block) and prints p50/p99/p99.9/max against the 333 µs period. Gates on the hosted runner: typical p50 ≤ 25 %, worst p50 ≤ the period (shared runners preempt, so tails are reported, not gated); the §3.5 p99.9 gate belongs to the PC (S7). Measured (run 36246425178): typical p50 20.8 µs (6.3 %), p99.9 37.7 µs (11.3 %); worst p50 200.6 µs (60.2 %), p99.9 272 µs (81.6 %), max 354 µs.

## 4. Proof

- **Impulse oracle (§3.5):** an independent reference model written from A2–A11 computes the gain from every RX channel to every TX channel for random states over `test-site.toml`; the engine's `Offline` output of staggered impulses equals it within 1e-12, and is zero everywhere else. Dedicated cases pin A3 (mute kills pre-fader sends and talkback), A6 (pre ignores the input fader, post follows the stems fader), A9 (bus-to-bus is post-mute and unclipped), A10.
- **Block-size invariance:** hot material, EQs and limiters active, commands at identical sample indices inside blocks; B = 32/64/97/256 agree within 1e-12.
- **Fuzzing:** a stable randomised harness over the request parser and `Core::apply` in every test run, and a `cargo-fuzz` target (per-push 60 s in `fuzz`, a nightly shard).

## 5. Deviations from earlier sketches

- Panics unwind (spec §2.4) — the synthesis's `panic = "abort"` is not used.
- Sends are declared as `from × to` families in `site.toml`; stable ids are strings (`InputId`, `BusId`, `SendId{src, dst}`), not `SmolStr`.
- Track pan (A5) exists on every node; `master` and `translator` have no EQ or limiter (§3.1 counts 44 EQs and 10 limiters).
- A moving EQ band is redesigned every 16 samples (S2 redesigned per sample); steady state and block-size invariance are unchanged.
- Block-size invariance is bit-exact (§3.5 asks ≤ 1e-12): a crossfade keeps the wet sample exactly at its end.
- Windows named pipes in `interprocess` have no read timeouts, so the reader that closes a dropped connection works on Unix only; the pipe integration tests run on Linux, the Windows job runs the engine's library tests (S6 reworks the Windows reader).

## 6. Not in S3

ASIO, the STA thread, the stall watchdog, driver resets, DACL and reject-remote (S6); the server's engine client, Opus, the 10 s member solo rule, the 50 ms preset ramp and the UI (S5); the importer, the site's real `site.toml` and `baseline.json` (S4); the PC soak and p99.9 gate (S7).

## 7. Risks

- **B = 32 on the PC:** hosted numbers only bound the arithmetic; interrupts and DPCs are S1c/S7.
- **Mutation budget:** the S3 diff has 1 031 mutants (88 shards at 12 per shard); shard durations are measured at the first PR.
- **Worst-case transients:** every EQ band and send moving at once costs about 60 % of the period on the hosted runner (a preset or import, 20 ms); the PC is faster, S1c/S7 measure it.
- **rtsan toolchain:** a nightly feature; the job pins the nightly and documents a failure rather than hiding it.
