# iemmixer #20 — Engine Model Rework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) or superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Pushes and CI waits (Task 11) run in the main session, never in a subagent.

**Goal:** Replace the REAPER-shaped routing graph (buses with kinds, 268 sends with tap modes, bus→bus sends, a master, a topological sort) with the purpose-built monitor-mixer model derived from the members' GUI — inputs, groups, mixes with levels, heard mixes, taps, talkback — in the protocol, the engine, the site file, the importer/exporter and the band data, keeping every parity proof; and make `iem-migrate band` output transactional (ticket #20, program #1).

**Architecture:** `iem-engine-proto` gets the new ids (`InputId`, `GroupId`, `MixId`, `Source`, `EqTarget`), state (`MixState{inputs, mixes}`, `Mix{out, inputs, groups, mixes}`), commands, changes and topology. `iem-engine` parses the new `[engine]` table (`site.rs`), compiles it (`topology.rs`, replacing `graph.rs`), and renders the fixed pipeline (`rt.rs`) driven by the pure `core.rs`. `iem-rpp` maps REAPER tracks to inputs, mixes and group instances and back; `iem-core::band` moves to schema 3; `iem-migrate` follows and stages its band output. Design note: `docs/superpowers/specs/2026-09-26-engine-model-rework-design.md`.

**Tech Stack:** Rust 1.98.1 (edition 2024); no new crates; GitHub Actions (hosted only).

**Spec:** program spec §2.4, I4–I7, §3.1–3.5, F5–F18, F29; S3 and S4 design notes (mechanics unchanged); owner rulings on #20.

## Global Constraints

- **Tier 0:** no local cargo compilation. Locally only `cargo fmt`, `cargo metadata`, `cargo tree`, Python. Every Rust claim is proven in hosted CI (Task 11): one push per cycle, one fix commit per failing cycle, foreground bounded waits (≤ 9 min per call), never `run_in_background`.
- **Owner rulings:** nothing kept only because REAPER had it; the GUI (spec §3.2) decides what the engine holds. REAPER exists only in `iem-rpp`/`iem-migrate` and the goldens.
- **Parity:** the goldens and their tolerances never change; the oracle's reference model is written from the design note §3, independently of the engine code; block-size invariance stays bit-exact.
- **RT (I7):** `process()` allocates, locks, logs and makes syscalls never; the worst-case scenario covers every new op.
- **P6:** synthetic ids and channels only; the real site table and aliases change in the private ops repo (its `dev` branch), never here.
- **Tests:** every change ships tests that can fail; no `#[ignore]`, no skips; the coverage floor never drops; the mutation matrix is sized from `mutants-list`.
- **Branches and identity:** `dev` only; no PR or merge. Noreply identity; every commit carries `Refs #20` and `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **The IEM PC is not touched.**
- **Durable state:** decisions and findings go on #20 the moment they land.

## Review Focus

1. **Parity:** `parity.rs::impulse_oracle_matches_random_states` (a reference model written from the design note), `the_reference_model_encodes_a3_a6_a9_a10`, `outputs_do_not_depend_on_the_block_size` (bit-exact).
2. **Nothing REAPER-shaped left in the engine:** no bus kinds, sends, taps, master or sort in `iem-engine-proto`/`iem-engine` (`grep`), define-before-use enforced (`topology::tests::a_heard_mix_must_be_declared_before`).
3. **Import fidelity:** the master must be muted, pans 0, the stems→bus sends unity — each a listed error; `export(p, import(p)) == p` byte for byte.
4. **Transactional band output:** `band_output_is_unchanged_after_a_failure_at_every_step`, `a_crash_between_the_renames_is_recovered`.
5. **Hostile clients:** `props.rs::random_requests_never_panic_and_keep_state_in_caps` over the new commands.

## File Structure

```
config/test-site.toml                         new [engine] table (inputs, groups, mixes)
crates/iem-engine-proto/src/{lib,ids,state,msg}.rs
crates/iem-engine/src/{lib,site,topology(was graph),params,cmd,core,rt,rt_tests,persist,control,engine}.rs
crates/iem-engine/tests/{common/mod,parity,props,pipes,rt,rtsan}.rs, examples/bench.rs
fuzz/fuzz_targets/engine_request.rs
crates/iem-core/src/band.rs                   schema 3 (Source input|mix, groups)
crates/iem-rpp/src/{topology,aliases,import,export,sitegen,band,backup}.rs
crates/iem-migrate/src/{site,import_cmd,export_cmd,band_cmd,stage}.rs, tests/cli.rs
.claude/rules/{engine,migration}.md, docs (program spec amendment, this plan, the design note)
(ops repo, dev) site/aliases.toml, site/site.toml [engine]
```

---

### Task 1: Design on #20 and the docs

- [ ] **Step 1:** design note and this plan; denylist-scan the staged tree; commit `docs(#20): engine model rework design note and plan`.
- [ ] **Step 2:** Slovak design summary on #20 (model, what disappears, parity, transactional band output).

### Task 2: Protocol (`iem-engine-proto`)

**Interfaces:** `InputId`, `GroupId`, `MixId`, `Source::{Input, Mix}`, `EqTarget::{Input, Mix, Group{mix, group}}`; `InputState{trim_db, processing, muted, eq}`, `Level{gain_db = DB_OFF, pan, muted}`, `MixGroup{gain_db, muted, eq}`, `MixOut{volume_db, muted, eq, limiter}`, `Mix{out, inputs, groups, mixes}`, `MixState{inputs, mixes}` (`SCHEMA = 2`), `Solo{mix, sources}`, `Transient{solo, listen: [Option<MixId>; 2], test_signal}`; `Cmd::{SetInput, SetMix, SetLevel, SetGroup, SetEq, SetLimiter, ResetLimiterStats, SetSolo, StartListen, StopListen, …}` (`OPS` 20); `Change::{Input, MixOut, Level, Group, Solo, Listen, TestSignal, LimiterStatsReset}`; `TopologyInfo{hash, sample_rate, engineer, inputs[InputInfo{id, channels, talkback, group}], groups[GroupInfo{id, inputs}], mixes[MixInfo{id, channels, mixes}]}`; `Meters{seq, inputs, mixes, groups (mix-major), gr_db, limiter_active_s, trips}`.

- [ ] **Step 1: Tests** — ids validate and serialise as plain strings / `{"input": …}`, `{"mix": …}`, `{"group": {"mix", "group"}}`; defaults (level off, strips 0 dB, limiter on −6 dB); unknown fields ignored, missing defaulted; every command round-trips and its op is listed; JSON shapes of `set_level` and a `level` change; N/N−1 negotiation unchanged.
- [ ] **Step 2: Implement.**

### Task 3: Site and topology (`iem-engine`)

**Interfaces:** `site::{Site{channels, engineer, inputs, groups, mixes}, SiteInput{id, rx, talkback}, SiteGroup{id, inputs}, SiteMix{id, tx, mixes}, parse, load, SiteError}`; `topology::{compile(&Site) -> Result<Topology, SiteError>, Topology{inputs, groups, mixes, direct, rx, tx, engineer, hash}, InputNode{id, rx, stereo, talkback, group}, GroupNode{id, inputs}, MixNode{id, tx, mono, mixes}}` with `input_index`, `group_index`, `mix_index`, `slot(mix, &Source)`, `levels(mix)`, `info()`.

- [ ] **Step 1: Tests** — the test site's shape (24 inputs / 32 RX, 1 group of 7, 11 mixes / 21 TX, member1 hears 8, the engineer 9, the translator mono); every `SiteError` (ids, channels, groups, heard mixes: unknown, self, repeated, declared later; engineer not a stereo mix; second talkback); hash stable and topology-sensitive; slots.
- [ ] **Step 2: Implement; rewrite `config/test-site.toml`** (the §3.1 shape, synthetic channels, member1 after the mixes it hears).

### Task 4: Caps, RT commands and the control core

**Interfaces:** `params::{cap_input, cap_level, cap_group, cap_out, …, InputParams{trim, muted, processing}}`; `RtOp::{Input, InputEq, MixOut, MixEq, Limiter, ResetLimiter, Level{m, k, gain, pan, muted}, Group{m, g, gain, muted}, GroupEq, Listen{slot, mix}, TestSignal, StopTestSignal, FadeOut, Panic}`; `core::{reconcile, to_state, defaults_muted, Core}`.

- [ ] **Step 1: Tests** — one set → one revision and one RT op per changed stage; unchanged sets keep the revision; caps and non-finite rejection for every field; unknown ids; batches atomic and bounded; solo silences every other level of the mix only (groups and output untouched) and clears; foreign solo sources refused; listen slot 0 engineer, slot 1 one other mix (`NoSource` for a second); test signal and fault flags; import fits one block; full sync covers every stage; defaults mute every mix.
- [ ] **Step 2: Implement.**

### Task 5: The RT processor

- [ ] **Step 1: Tests (`rt_tests.rs`)** — mono input at unity on both sides; stereo keeps channels; trim/EQ only with processing; talkback before the mute gate; a level reads P (mute kills it, talkback too); the group strip is Σ → EQ → fader → mute; a heard mix is read post-mute and unclipped; output chain EQ → limiter → volume → mute → safety → clamp; a mono mix is the half-sum on its one channel; sanitiser; commands at their sample; 512 budget; limiter ramps; test signal caps every TX; taps side-effect-free; fades; meters every 3 200 samples (inputs, mixes, groups); the bounded-run tests.
- [ ] **Step 2: Implement.**

### Task 6: Persistence, control loop, engine binary

- [ ] **Step 1:** counters per mix; schema-1 files refused with a reason; meters message layout; the control tests and `engine.rs` start-up on the new topology.

### Task 7: Integration tests, benchmark, fuzz target

- [ ] **Step 1:** `tests/common/mod.rs` worst case with the new commands; `parity.rs` new reference model (design note §3) and the A3/A6/A9/A10 cases; block-size invariance; `props.rs` generators; `pipes.rs` commands; `examples/bench.rs` typical and worst; `fuzz/fuzz_targets/engine_request.rs`.

### Task 8: Band data schema 3 (`iem-core`)

- [ ] **Step 1:** `MixSend{src: Source{input|mix}, …}`, `groups: {id → gain_db}` replaces `stems_fader_db`, `SCHEMA = 3`; round-trip tests.

### Task 9: Importer and exporter (`iem-rpp`)

**Interfaces:** `topology::{Topology{inputs, groups, mixes, engineer}, TopoInput, TopoGroup, TopoMix, Counts, diff, engine_toml}`; `aliases::{Aliases{tracks, members}, MemberAlias{id, mix, archived}}`; `import::{import, Imported{topology, state, counts, notes, tracks: Vec<Place>}, Place::{Input, Mix, Group{group, mix}}, project, compare}`; `export::{export, export_checked}` (with the not-carried-back report); `sitegen::{synthetic_site, sample_state, project, aliases_toml}`; `band` and `backup` on the new sources.

- [ ] **Step 1: Tests** — every mapping rule and error of design note §7 (master muted, pans 0, unity stems sends, group membership, heard mixes, mono mix, missing receives off, input faders ignored and reported); counts 45/268/44/10/24 of the synthetic predecessor project; diff and `engine_toml` (dependency order) round-trip; unchanged export byte-identical; edited state within 1e-9 dB; values the project cannot hold are reported and not carried back; era re-keying on inputs and heard mixes, stems level → the group; backup cross-check.
- [ ] **Step 2: Implement.**

### Task 10: `iem-migrate`, transactional band output

**Interfaces:** `stage::{Stage, Step, commit, recover}` (staging dir next to the target, `.complete` marker, two renames, roll forward/back); `band_cmd::run_with(args, fail: &dyn Fn(Step) -> io::Result<()>)`.

- [ ] **Step 1: Tests** — the CLI integration tests on the new site; a failure injected at every step leaves the target byte-identical and no staging directory; a crash after the first rename is rolled forward by `recover`, one before it rolled back; a second run after a crash succeeds.
- [ ] **Step 2: Implement.**

### Task 11: CI cycles

- [ ] **Step 1:** `cargo fmt --all`, denylist scan, push `dev`; watch lint/test/engine/fuzz/migrate/windows to terminal; one fix commit per failing cycle.
- [ ] **Step 2:** read `mutants-list`; resize the `shard:` matrix in `ci.yml` when needed (Task 11 repeats).
- [ ] **Step 3:** record the parity maxima, the B = 32 benchmark and the mutant count on #20.

### Task 12: Private site (ops repo, `dev`)

- [ ] **Step 1:** aliases in the new form (stems tracks → the group id, members `{id, mix, archived}`, no `master`); the `[engine]` table emitted by the CI-built `iem-migrate import --emit-topology` from the saved project; `tools/check_import.sh` passes; commit on the ops `dev` branch.

### Task 13: Playbook, spec amendment, hand-offs

- [ ] **Step 1:** `.claude/rules/engine.md` and `migration.md`; program spec amendment line (A11, F29 master, I4, §3.1 counts); hand-offs on #8 (S5) and #9 (S6); results on #20.
