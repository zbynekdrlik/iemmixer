---
paths:
  - "crates/iem-engine/**"
  - "crates/iem-engine-proto/**"
  - "crates/iem-audio-io/**"
  - "fuzz/**"
  - "config/test-site.toml"
---

# Engine core (S3)

- **RT contract (I7) at B = 32 / 96 kHz (333 µs):** `Processor::process` allocates, locks, logs and makes syscalls never. Proofs: `tests/rt.rs` (`assert_no_alloc`, own binary), `tests/rtsan.rs` (the `engine` CI job: `nightly-2026-09-20`, `-Zbuild-std`, `RUSTFLAGS="-Zsanitizer=realtime --cfg iem_rtsan"`; `process` carries `#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]`), `examples/bench.rs` (typical p50 ≤ 25 %, worst p50 ≤ the period). New per-block work goes into `tests/common/mod.rs`'s worst-case scenario too.
- **Sample-accurate commands:** `RtCmd { at, group, op }`; blocks are cut at command times and the test signal's end, every ramp steps per sample. `parity.rs::outputs_do_not_depend_on_the_block_size` requires bit-identical output for 32/64/97/256 — a mix `x + m·(y − x)` must keep `y` exactly at `m = 1` (and `x` at 0). ≤ 512 commands per block; a group (one `write_chunk`) is applied whole or waits.
- **Linear mix parity:** `parity.rs` has an independent reference model written from A2–A11 over the declarative site; change the engine and the model only from the spec, never one to fit the other.
- **Site (`[engine]` in `site.toml`):** inputs (`rx` 1–2), buses (`output` 2 TX + EQ + limiter, `stems` no TX, `translator` 1 TX, `master` 2 TX), send families `from × to` with `tap = pre` (inputs) / `post` (buses). `config/test-site.toml` keeps the §3.1 shape with synthetic channels (RX 101–132, TX 71–93, 160-channel map); real values only in the ops repo.
- **Control core:** `Core::apply` is pure and caps every field (§2.3: no pipe tokens); one revision per changing request; batches ≤ 256 ops and ≤ 512 RT commands, atomic. Solo masks live in the core (effective send mute), listen slots and the test signal are transient, never persisted.
- **Protocol:** N/N−1 via `negotiate`; additive schemas — no `deny_unknown_fields` in `iem-engine-proto`, every struct `#[serde(default)]`. Commands `op`-tagged, messages `type`-tagged; add an op to `OPS` too.
- **Persistence:** `current.json` + 20 `gen-<seq>.json` + `baseline.json`, SHA-256 over the payload's raw bytes (`RawValue`); load chain current → generations → baseline → muted defaults with `StateLost`.
- **Pipes:** Unix socket files on Linux, named pipes on Windows. Readers poll with a receive timeout so a dropped connection closes its socket; interprocess has **no timeouts on Windows named pipes** — the pipe integration tests run on Linux only until S6 reworks the Windows reader (and adds reject-remote and the DACL).
- **Tests that wait** use bounded loops (5 s) and never hang a mutant: run `run()` in a thread with a deadline when a mutation could make it start. A mutation that could make `process`, `Control::run` or a boundary search loop forever needs one test that runs it on a thread with a 5 s `recv_timeout` (`rt_tests::run_bounded`, `control::tests::run_bounded`); the mutation workflows' nextest profile (`.config/nextest.toml`, `NEXTEST_PROFILE=mutants`) ends the other, hanging tests at that first failure.
- **Mutation-testable timing:** keep decisions pure and feed them `Instant`s (`nullrt::step`, `Control::tick(now)`); a unit harness builds `Control` from `Parts` with a fake `Driver` and a socket pair (Unix only).
- **Talkback / taps:** 96 ↔ 48 kHz half-band (79 taps, β 10.06) on the media thread; non-finite talkback enters as silence (a NaN in the FIR history poisons everything after it).
- **EQ cost:** a moving band is redesigned every 16 samples of its own ramp (`iem_dsp::eq::DESIGN_EVERY`); all 220 bands moving once cost 640 µs per 32-sample block when every sample redesigned.
- **Dependencies:** the engine's closure must equal `scripts/engine-deps-allow.txt` (`supply-chain` runs `scripts/check_engine_deps.py`); `iem-engine` is GPL-3.0-or-later (cargo-deny exception) because it links the limiter.
- **Fuzzing:** `tests/props.rs` (stable, seeded per CI run) and `fuzz/` (cargo-fuzz 0.13.2 on the pinned nightly, 60 s per push, 30 min nightly in `fuzz-nightly.yml`; its own workspace and `Cargo.lock`, checked with `cargo metadata --locked` because cargo-fuzz has no `--locked`).
