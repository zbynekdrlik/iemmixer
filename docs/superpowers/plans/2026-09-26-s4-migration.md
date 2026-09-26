# iemmixer S4 — Importer, Exporter and Data Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) or superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Pushes and CI waits (Task 10) run in the main session, never in a subagent.

**Goal:** Move the predecessor's mixes and band data into iemmixer and back without loss (ticket #7, program #1): an RPP + ReaEQ parser and importer (project + newest backup JSON → `MixState` and a topology diff against `site.toml`), an exporter (state → a **new** RPP, byte-identical when unchanged, self-checked), the legacy data migration (presets, snapshots and customizations re-keyed per era, photos, PINs hashed with the same values, JWT secret, VAPID key, push subscriptions, LAN certificate, renamed member archived), all as the `iem-migrate` CLI that reads files only, fails loudly and prints a dry-run report.

**Architecture:** Library work lands in permissive crates — `iem-rpp` (reader, ReaEQ decode/patch, predecessor project model, topology, aliases and eras, import, export, synthetic site projects, legacy band data re-keying, backup cross-check), `iem-core::band` (the schema-2 band data files S5 adopts) and `iem-server::band_import` (PINs, secrets, certificate, push, photos; `PinStore` import marks). The new `iem-migrate` crate (GPL-3.0-or-later, it links `iem-engine` for `site`, `graph` and `persist`) is the CLI. Design note: `docs/superpowers/specs/2026-09-26-s4-migration-design.md`.

**Tech Stack:** Rust 1.98.1 (edition 2024); existing crates only (`serde`, `serde_json`, `toml` 0.9, `base64` 0.22, `sha2`, `thiserror`, `argon2`, `p256`, `tempfile` for tests); GitHub Actions (hosted only).

**Spec:** program spec §2.4, §3.4 "Migration", §3.5 "Data", §4.3 rollback, P6, P9, D8; the S3 hand-off on #7.

Detail sources (private, never committed): `05-fact-engine-api-migration.md` §2 and §4 (index leaks, per-era re-keying, RPP field table), `02-reaper-control-surface.md`, `04-parity-inventory-and-hazards.md`; the predecessor tree `~/devel/reaperiem` at `03be5b97deafdc5d765516014c3f59370edd534b` (formats: `iem-core/src/{backup,preset,snapshot,types,ws,config}.rs`, `iem-server/src/{pin_store,push_store,photo_store,customization_store}.rs`) and its `backup` branch (history of the saved project, presets, snapshots, `pins.json`).

## Global Constraints

- **Tier 0:** no local cargo compilation. Locally only `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p`, Python. Every Rust claim is proven in hosted CI (Task 10): one push per cycle, one fix commit per failing cycle, foreground bounded waits (≤ 9 min per call), never `run_in_background`. The private dry run (Task 11) uses the `iem-migrate` binary CI builds on `dev` pushes.
- **Reads files only:** the importer never talks to REAPER, the predecessor app or the PC. The original project is never written; exports are created exclusively.
- **Fail loudly:** unknown track names, unknown member ids, unmappable keys, ambiguous eras, unsupported routing or plug-ins, values outside the engine's caps are errors that list every offender. `--partial` only downgrades *missing* categories to report lines.
- **P6:** synthetic names, ids and channels only in the public repo (`config/test-site.toml`, `sitegen`); aliases, eras and the real-site check live in the ops repo. Never commit a predecessor member name, track name, channel number, PIN or secret. Scan every commit's tree with the denylist.
- **P9:** PIN values, the JWT secret and the VAPID key are carried unchanged; defaults are never compiled in (a private file supplies them).
- **Security baseline:** `pin_hashes.json` stays argon2id-only (the new `imported` list holds member ids); backups written or archived never contain PINs; secrets are created owner-only and never overwritten.
- **Licences (D1):** `iem-migrate` is GPL-3.0-or-later (cargo-deny exception, licence text copied from `iem-engine`); `iem-rpp`, `iem-core`, `iem-server` stay MIT OR Apache-2.0 and never depend on `iem-engine`.
- **Tests:** every feature ships tests that can fail; no `#[ignore]`, no skips; `iem-migrate` joins coverage and the mutation package lists; every test finishes well under the nextest 10 s cap (argon2 at production cost in few tests only).
- **Branches and identity:** `dev` only; no PR or merge. Noreply identity; every commit carries `Refs #7` and `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **The IEM PC is not touched in S4.**
- **Durable state:** decisions and findings go on #7 the moment they land.

## Review Focus

1. **Mis-keyed legacy data (R8):** a key that maps to the wrong source in some era must fail, not import silently — `band::tests::*era*`, `band::tests::unmappable_key_fails_with_the_item`.
2. **Lossless round trip:** `export::tests::unchanged_state_exports_byte_identical`, `export::tests::edited_state_reimports_within_1e_9_db`, `iem-migrate` `export_never_overwrites_and_self_checks`.
3. **P9:** `band_import::tests::imported_pin_verifies_with_the_same_value`, `pin_store::tests::a_later_import_never_overwrites_an_iemmixer_set_pin`, `band_import::tests::jwt_signed_with_the_legacy_secret_verifies`, push marker written.
4. **Topology authority:** `iem-migrate` `import_refuses_a_topology_that_differs_from_the_site` (exit 3, diff printed, nothing written).
5. **Secrets hygiene:** reports never contain PIN or secret values (`report_never_prints_secrets`); archived backups lose `pins`.

## Shell variables (paste at the start of every task)

```bash
export WORK="$HOME/devel/iemmixer"
export OPS="$HOME/devel/iemmixer-ops"
export PRED="$HOME/devel/reaperiem"
export PRIV="$HOME/.config/iemmixer"
export REPO=zbynekdrlik/iemmixer
```

## File Structure

```
Cargo.toml, Cargo.lock                       + member iem-migrate
deny.toml                                    GPL exception for iem-migrate
fuzz/Cargo.lock                              workspace versions (stale since the dev.5 bump)
scripts/check_version.py (+test)             CRATES += iem-migrate; fuzz/Cargo.lock versions checked
.cargo/mutants.toml                          exclusions with reasons (bin entry)
.github/workflows/ci.yml                     coverage + mutation package lists, windows (clippy/test/build), migrate-bin artifact, shard matrix
.github/workflows/mutation-full.yml          package lists
crates/iem-rpp/Cargo.toml                    + iem-engine-proto, iem-core, toml
crates/iem-rpp/src/read.rs                   lossless RPP reader, tokens, token patching
crates/iem-rpp/src/reaeq.rs                  + decode, EqBlob (in-place patch)
crates/iem-rpp/src/topology.rs               Topology, Counts, diff, [engine] TOML emission, --expect
crates/iem-rpp/src/aliases.rs                aliases.toml, eras.toml, era candidates
crates/iem-rpp/src/legacy.rs                 predecessor project model (tracks, receives, FX, master)
crates/iem-rpp/src/import.rs                 project + aliases → Imported {topology, state, counts, notes}; state comparison
crates/iem-rpp/src/export.rs                 patch the template; self-check
crates/iem-rpp/src/sitegen.rs                synthetic predecessor-shaped project from a topology + state
crates/iem-rpp/src/band.rs                   legacy presets/snapshots/customizations → iem_core::band
crates/iem-rpp/src/backup.rs                 backup JSON v1 cross-check
crates/iem-core/{Cargo.toml,src/lib.rs,src/band.rs}   schema-2 band data types
crates/iem-server/src/pin_store.rs           imported marks, import_*_hash
crates/iem-server/src/band_import.rs         legacy config, default PINs, PINs, secrets, push, TLS, photos
crates/iem-migrate/{Cargo.toml,LICENSE,src/lib.rs,src/main.rs,src/site.rs,src/import_cmd.rs,src/export_cmd.rs,src/band_cmd.rs,tests/cli.rs}
.claude/rules/migration.md, CLAUDE.md        playbook rule + router line
(ops repo) site/aliases.toml, site/eras.toml, tools/eras_from_history.py (+test), tools/check_import.sh
```

---

### Task 1: Start — sync, fuzz lockfile, docs, design on #7

**Files:** `scripts/check_version.py`, `scripts/test_check_version.py`, `fuzz/Cargo.lock`, this plan and the design note.

- [x] **Step 1: Sync and check the version** — `git fetch origin && git merge --ff-only origin/dev`; `python3 scripts/check_version.py --base-ref origin/main` → `version bump OK: 2.0.0-dev.4 -> 2.0.0-dev.5` (bumped in `aeb6f44`).
- [x] **Step 2: [red] the version gate checks `fuzz/Cargo.lock`** — CI's `fuzz` job fails at `cargo metadata --locked --manifest-path fuzz/Cargo.toml` because the bump left `fuzz/Cargo.lock` at `2.0.0-dev.4`. Test `test_stale_fuzz_lockfile_fails` (`write_tree` also writes `fuzz/Cargo.lock`); run `python3 scripts/test_check_version.py` → the new test fails.
- [x] **Step 3: [green]** — `consistency_errors` compares every workspace crate recorded in `fuzz/Cargo.lock` with the workspace version; update `fuzz/Cargo.lock` (`cargo metadata --manifest-path fuzz/Cargo.toml --format-version 1 >/dev/null`, non-compiling); tests pass; `python3 scripts/check_version.py` → consistent.
- [x] **Step 4: Design on #7** — a Slovak comment summarising design note §3 (crates and licences, private aliases/eras, import rules, export and round trip, legacy data, PIN rule, secrets, CLI) and §5.
- [x] **Step 5: Denylist-scan the docs (temporary index, `--tree`), commit** `docs(s4): migration design note and implementation plan`.

### Task 2: `iem-rpp` reader and ReaEQ decoder

**Files:** `crates/iem-rpp/src/read.rs`, `crates/iem-rpp/src/reaeq.rs`, `crates/iem-rpp/src/lib.rs`, `crates/iem-rpp/Cargo.toml`.

**Interfaces:** `read::parse(&str) -> Result<(Doc, Block), RppError>`; `Doc { lines, crlf, final_eol }` with `render()`, `content(i)`, `tokens(i)`, `set_token(i, k, text)`; `Block { head, end, children }` with `direct()`; `read::tokens(&str) -> Vec<String>` (quotes removed), `read::raw_tokens(&str) -> Vec<&str>`. `reaeq::EqBlob::decode(&[&str]) -> Result<EqBlob, RppError>` (`eq: ReaEq`), `EqBlob::lines_for(&ReaEq) -> Result<Vec<String>, RppError>`.

- [x] **Step 1: Tests** — CRLF and LF render byte-identical; nested chunks; unbalanced `<`/`>` errors; mixed line endings error; quoted tokens with each quote kind; `set_token` keeps indentation and other tokens; the S1b REAPER 7.65 chunk decodes to its `ReaEq`; `lines_for` of the decoded EQ equals the input lines; a patched band re-decodes to the new values; bad magic, short state, band count ≠ bands, unknown band type are errors.
- [x] **Step 2: Implement** (stack parser over lines; per-line base64 lengths kept for re-splitting).

### Task 3: Topology, counts, aliases and eras

**Files:** `crates/iem-rpp/src/topology.rs`, `crates/iem-rpp/src/aliases.rs`.

**Interfaces:** `Topology { inputs: Vec<TopoInput{id, rx, talkback}>, buses: Vec<TopoBus{id, kind, tx}>, sends: Vec<(SendId, Tap)>, engineer: Option<BusId> }`, `counts() -> Counts {tracks, sends, eqs, limiters, trims}`, `diff(&self, site: &Topology) -> Vec<String>`, `engine_toml(channels) -> String`, `has_send`, `input`, `bus`; `Counts::check(&str) -> Result<(), String>` for `--expect`. `Aliases { master, tracks, members: BTreeMap<String, MemberAlias{id, bus, stems, archived}> }`, `Eras { era: Vec<Era{first_seen, last_seen, tracks}> }` with `candidates(t) -> Vec<usize>`; `parse_aliases`, `parse_eras` (TOML, unknown keys refused, eras ordered and non-overlapping).

- [x] **Step 1: Tests** — counts of the `test-site.toml` shape (45/268/44/10/24 via a hand-built topology in tests); diff lists missing/extra ids, rx/tx/kind/talkback/engineer and send/tap differences and nothing for equal topologies in another order; emitted TOML re-parses (toml) into the same families; `--expect` parsing and mismatch messages; era candidates inside, in a gap, before the first, after the last; overlapping or unordered eras refused; bad ids refused.
- [x] **Step 2: Implement.**

### Task 4: Predecessor project model and import

**Files:** `crates/iem-rpp/src/legacy.rs`, `crates/iem-rpp/src/import.rs`.

**Interfaces:** `legacy::LegacyProject::parse(&str) -> Result<LegacyProject, RppError>` (tracks with line indices of `VOLPAN`, `MUTESOLO`, `FX`, `REC`, `HWOUT`, `AUXRECV`, plug-ins with kind/bypass/block; master lines). `import::import(&LegacyProject, &Aliases) -> Result<Imported, Problems>` → `Imported { topology, state, counts, notes, tracks: Vec<TrackRef> }`; `import::lin_to_db`, `import::project(&Topology, &MixState) -> MixState` (fields the project can hold), `import::compare(&Topology, &MixState, &MixState, tol_db) -> Vec<String>`; `Problems(Vec<String>)`.

- [x] **Step 1: Tests (on `sitegen` output, Task 5 Step 1 lands first in the same commit)** — every §3.2 rule of the design note: classification (armed input, stereo/mono RX, output/translator/stems, master TX), talkback and engineer detection (none / two → error), modes 3/0 with the wrong source kind, routing fields ≠ 0, REAPER solo, `FX 0` on input vs bus, TRIM/ReaEQ/limiter values and every chain error (missing, bypassed, wrong order, active unknown plug-in; bypassed unknown plug-in reported), limiter release/link, unknown names listed together, linear 0 → `DB_OFF`, all counts.
- [x] **Step 2: Implement.**

### Task 5: Synthetic site projects and export

**Files:** `crates/iem-rpp/src/sitegen.rs`, `crates/iem-rpp/src/export.rs`.

**Interfaces:** `sitegen::project(&Topology, &MixState, &dyn Fn(&str) -> String) -> Result<String, RppError>` (predecessor-shaped: inputs, buses, master header; TRIM/ReaEQ/limiter, talkback and listen stand-ins, a bypassed extra plug-in); `sitegen::aliases(&Topology, &dyn Fn(&str) -> String, master) -> Aliases`. `export::export(&LegacyProject, &Aliases, &MixState) -> Result<String, Problems>`; `export::export_checked(template: &str, &Aliases, &MixState) -> Result<String, Problems>` (patch, re-import, compare within 1e-9 dB, same topology).

- [x] **Step 1: Tests** — `import(sitegen(t, s)) == project(t, s)`; unchanged state exports byte-identical (also with CRLF); edited state (every field kind: fader, pan, mute, processing, trim, every EQ field and kind, global EQ gain, limiter on/off and ceiling, send gain/pan/mute incl. off, master) re-imports within 1e-9 dB; ReaEQ lines of untouched EQs identical; missing state entry is an error; unrepresentable fields ignored.
- [x] **Step 2: Implement.**

### Task 6: Band data types, legacy re-keying and the backup cross-check

**Files:** `crates/iem-core/src/band.rs` (+ `lib.rs`, `Cargo.toml` dependency on `iem-engine-proto`), `crates/iem-rpp/src/band.rs`, `crates/iem-rpp/src/backup.rs`.

**Interfaces:** `iem_core::band::{SCHEMA, MixSend, Preset, Snapshot, PresetFile, SnapshotFile, CustomizationFile}`. `band::View { bus, stems }` built from `MemberAlias` + `Topology`; `band::rekey_presets(&HashMap<String, PresetEntry>, &Ctx) -> Result<(Vec<Preset>, Stats), Problems>`, `rekey_snapshots(&[MixSnapshot], &Ctx)`, `rekey_customization(&Customization, &Ctx)`, `Ctx { topology, aliases, eras, legacy_member, member: &MemberAlias }`. `backup::cross_check(&MixerBackup, &Imported, &Aliases) -> Result<Check {compared, differing}, Problems>`.

- [x] **Step 1: Tests** — era by time (inside, gap resolved by validity, ambiguous gap fails, agreeing candidates pass); unmappable key fails with the item's label and the keys; units (preset dB, snapshot linear incl. 0, pan `p·2−1`, stems level); input EQ kept, bus EQ dropped and counted; mix viewer sources map to buses; archived member flags; customizations use the newest era; band file JSON round trip; backup: unknown names fail, equal values compare clean, a changed send/mute/fader/EQ/limiter is reported.
- [x] **Step 2: Implement.**

### Task 7: PIN rule and server-side band data

**Files:** `crates/iem-server/src/pin_store.rs`, `crates/iem-server/src/band_import.rs`, `crates/iem-server/src/lib.rs`, `.claude/rules/security-baseline.md` (the `imported` list).

**Interfaces:** `PinStore::{is_imported, import_engineer_hash, import_member_hash}` (return `false` when an iemmixer-set hash is kept; `set_*` clear the mark). `band_import::{LegacyConfig, parse_legacy_config, DefaultPins, parse_default_pins, PinPlan, import_pins, SecretOutcome, import_secret, validate_vapid, import_push, import_tls, import_photo}`, each with a `dry_run` flag that writes nothing.

- [x] **Step 1: Tests** — `pin_hashes.json` without `imported` still loads (additive); import sets a missing hash; re-import of an unchanged PIN keeps the hash; an imported hash is replaced when the legacy PIN changed; `set_member_hash` / `set_engineer_hash` clear the mark and a later import keeps them; the imported PIN verifies with the same value (production hasher); default PINs only from the file, argv never; config parsing (quoted, unquoted, missing keys, placeholder refused, invalid VAPID refused); secrets created owner-only, equal no-op, different refused; JWT signed with the legacy secret verifies via `auth::extract_claims`; push copied with the marker, a predecessor without the marker imports none; TLS PEM shape and never-overwrite; photos (JPEG, ≤ 256 KB).
- [x] **Step 2: Implement.**

### Task 8: `iem-migrate` CLI

**Files:** `crates/iem-migrate/*`, `Cargo.toml` (member), `Cargo.lock`, `deny.toml`, `scripts/check_version.py` (`CRATES`), `.cargo/mutants.toml`.

**Interfaces:** `iem_migrate::run(&[String]) -> Result<String, Failure { code, msg }>`; `site::topology(&Site) -> Topology`; subcommands per design note §3.5.

- [x] **Step 1: Tests (`tests/cli.rs`, on `config/test-site.toml` + `sitegen`)** — import: counts 45/268/44/10/24 and `--expect` pass/fail; baseline and current written and loadable by `persist::Store::load` (source `Current`); `--dry-run` writes nothing; a site differing by one send exits 3 with the diff and writes nothing; `--emit-topology` output compiles with `graph::compile` and equals the site's topology; unknown track name exits 2. Export: byte-identical for the imported state, edited state round trip, `--out` existing or equal to `--rpp` refused, self-check failure writes nothing. Band: end-to-end on a synthetic legacy directory (two eras, a renamed member, PINs, config, push, photos, certificate, backups) — files written, PINs verify, iemmixer-set PIN kept on re-run, dry run writes nothing, `--partial`, report without secrets. The binary's exit codes (`CARGO_BIN_EXE_iem-migrate`).
- [x] **Step 2: Implement** (thin `main.rs`; all logic in the library).

### Task 9: CI wiring

- [x] **Step 1:** add `iem-migrate` to the coverage packages, `mutants-list`, `mutation-warmup`, `mutation` and `mutation-full.yml` package lists; windows job: clippy + `--lib` tests + release build of `iem-migrate` (added to `windows-binaries`); new dev-push job `migrate-bin` uploading `iem-migrate-linux-<sha>` (used by Task 11).
- [x] **Step 2:** after the first green push, read `mutants-list`'s count and resize the `shard:` matrix to `ceil(count / 12)` if needed (report the change; required checks are regenerated when the next PR is prepared).

### Task 10: Push and CI cycles

- [x] **Step 1:** denylist-scan the staged tree, `cargo fmt --all --check`, Python tests; push `dev`; wait in the foreground (≤ 9 min per call) until every job is terminal.
- [x] **Step 2:** on failure read `gh run view --log-failed`, fix everything in one commit, push again. Repeat until all jobs are green.

### Task 11: Private fixtures and the real dry run (ops repo, never the public repo)

- [x] **Step 1:** `tools/eras_from_history.py` (+ test): walks the predecessor's saved-project history (backup and dev branches, read-only `git show`), writes `site/eras.toml` (one era per layout, `first_seen`/`last_seen`).
- [x] **Step 2:** `site/aliases.toml`: every track name of every era → id (the §3.1 ids), members (renamed member archived), `master`.
- [x] **Step 3:** `tools/check_import.sh`: downloads the `iem-migrate-linux-<sha>` artifact of the `dev` head, runs `import --dry-run --expect tracks=45,sends=268,eqs=44,limiters=10,trims=24 --emit-topology` on the newest saved project, and `band --dry-run --partial` on the backup-branch data; prints counts only.
- [x] **Step 4:** run it; record the counts (not values) on #7; commit the ops files on the ops `dev` branch.

### Task 12: Playbook, plan progress, hand-offs

- [x] **Step 1:** `.claude/rules/migration.md` (paths `crates/iem-rpp/src/{read,legacy,import,export,band,backup,aliases,topology,sitegen}.rs`, `crates/iem-migrate/**`, `crates/iem-server/src/band_import.rs`) + router line in `CLAUDE.md`.
- [x] **Step 2:** tick this plan, add execution notes (CI runs, mutant count, shards, deviations).
- [x] **Step 3:** hand-offs on #7 for S5 (schema-2 band files, `imported` PIN marks, archived entries read-only), S6 (run on the PC as the server's user, band directory, DPAPI pepper, `windows-binaries` carries `iem-migrate.exe`), S8 (shadow imports with `--dry-run`, cutover import, rollback export).

---

## Execution notes (2026-09-26)

- **Commits on `dev`:** aac45c3 / 3b76704 / b989bdc (stale `fuzz/Cargo.lock` after the dev.5 bump: [red] test, [green] gate + lockfile, test index fix), e588790 (docs), 289a370 (core band types), 30e558e (rpp), d02a7c0 (server), 17f5f0e (migrate CLI), f0a0a3a (CI), c5191dc (E0106 in two test helpers; the no-`[engine]`-table import path).
- **CI:** run 36255675738 (e588790) green — the fuzz job fixed. Run 36257942982 (f0a0a3a) failed on E0106 (two test helpers returning `&mut` needed a named lifetime); cancelled. Run 36258242059 (c5191dc): lint, test (line coverage 81.9 %, floor 62 %), engine, fuzz, wasm, e2e, golden-bundle, supply-chain, secrets, integrity, `migrate-bin` green.
- **Mutation budget:** `mutants-list` counts 947 diff-scoped mutants → 79 shards at 12 per shard; the matrix has 88: no resize.
- **Real site (private, counts only):** the CI-built binary imports the newest saved project with tracks 45, sends 268, EQs 44, limiters 10, trims 24 (`--expect` passes); the ops `site.toml` had no `[engine]` table, so the import exited 3 and `--emit-topology` proposed it; that table is now on the ops `dev` branch (owner merge to `main`), and the import passes against it (topology equal, no value outside the caps). The band dry run on the backup-branch data (presets, snapshots, `pins.json`; no config, certificate, photos or backups there) maps 8 presets (208 sends) and 420 of 424 snapshots (10 669 sends, 41 between eras), 6 members with their own PIN and 3 on the predecessor's default (from the private defaults file); **4 auto snapshots of one morning (5 − 6 months old) are unmappable** (key 23 in a layout never saved) and fail the run loudly — resolution belongs to the S8 shadow imports (hand-off on #7).
- **Deviations:** the band-data files use the id-keyed schema 2 in `iem_core::band` (the REAPER-era server types stay until S5); `import` accepts a `site.toml` without `[engine]` (exit 3 + proposal) because the ops site has none yet; the backup JSON is cross-checked, never applied (the project saved at switch time is newer, §4.3); the importer derives talkback/engineer from the predecessor's own plug-ins (`OIEM Receive`, `VBAN IEM`).
