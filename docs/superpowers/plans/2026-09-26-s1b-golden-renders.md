# iemmixer S1b — Golden Renders Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Tasks 13–14 (PC windows) and 12/16 (CI waits, merge) run in the main session, never in a subagent.

**Goal:** Measure REAPER's undocumented mix behaviour (pan taper, shelf bandwidth, HPF gain, mono downmix, mono level, bypass, trim, summing) by offline renders on the IEM PC's own REAPER in dev time, with a verified bit-identical restore of every file REAPER and the predecessor own, and commit the measured laws plus compact golden vectors (≤ 20 MB) that S2 needs (ticket #4, program #1).

**Architecture:** Three parts.

- **Generator.** The new crate `iem-rpp` (generator half; S4 adds the importer and exporter) writes deterministic synthetic REAPER projects and 64-bit float stimuli into a hash-manifested bundle.
  - It is built and run only in hosted CI (Tier 0). A push-only job uploads the bundle.
- **Render window.** A dev-box driver (`scripts/golden/golden_window.py`) walks the D7 procedure step by step over ssh.
  - The steps: interlock, save and quit REAPER, graceful app stop, backup and manifest, staging, offline renders in the console session through one Interactive task, fetch, verify and restore, bring-back.
  - A PowerShell module on the PC (`scripts/golden/GoldenPc.psm1`) does the file-level work.
  - GUI-only steps go through the PC's remote-desktop MCP server.
- **Analysis.** `scripts/golden/analyze.py` (numpy) turns the fetched renders into `goldens/s1b/`.

**Tech Stack:** Rust 1.98.1 (edition 2024) with crates already in `Cargo.lock` (base64 0.22, sha2 0.10, serde/serde_json, thiserror 2, tempfile 3 for tests); Windows PowerShell 5.1 (the PC's shell, also used on the CI `windows` job); Python 3.12 (stdlib for tooling, numpy 2.4.6 for analysis); GitHub Actions (hosted only); REAPER 7.65 on the IEM PC.

**Spec:** `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md` (§3.3–3.5, §4, D7 decided 2026-09-26), design note `docs/superpowers/specs/2026-09-26-s1b-golden-renders-design.md`.

Detail sources (private, never committed):
- `05-fact-reaper-mix-semantics.md` (golden recipe, open questions);
- `05-fact-eq-limiter-spec.md` (ReaEQ mapping and chunk layout, shelf candidates, limiter);
- `05-fact-iem-pc.md`, `10-site-appendix-private-draft.md`;
- the private runbook `14-s1b-pc-runbook-private.md` (PC paths, names, commands).

## Global Constraints

- **D7 (binding):**
  - REAPER is never installed or run on a dev box. Renders run only on the IEM PC's REAPER, only in dev time.
  - Dev time means the owner's "event skončil" arrived in this conversation after the last "ide event". The agent never infers it and never asks whether an event runs.
  - Every window: a full backup plus sha256 manifest first; offline renders of generated copies with a render instance that cannot open the ASIO card; a verified bit-identical restore; the PC back in its pre-window state.
  - The original project is never touched by render work, and Dante is never touched.
- **Owner event signals (D2):**
  - "ide event" pre-empts any window immediately (`golden_window.py preempt`), then the event runbook's checks run, then the owner gets a confirmation.
  - "event skončil" alone never starts a window. The window starts only when a task of this plan is being executed.
- **Never force-kill (P4, I8):**
  - Allowed: REAPER quits by action; the predecessor app leaves only by its tray Exit; the render instance ends by itself or by `CloseMainWindow()`.
  - The words `taskkill`, `Stop-Process`, `TerminateProcess` and `shutdown /f` never appear in any file (the `integrity` scan also covers `.psm1` after Task 9).
  - The predecessor app's launcher script is not used, because it force-kills.
- **The card:**
  - The render instance's `reaper.ini` never has `mode=3` (ASIO) and carries no ASIO keys. The PC module refuses mode 3.
  - Every render is watched with `tasklist /m <ASIO module>`. Any holder stops the queue and alarms the owner.
- **P6 — site data never enters this repository:**
  - host names, user names, user paths, the predecessor's data-folder and task names, the ASIO module name, ports, member names, the project name.
  - They live in `$PRIV/golden.env`, `$PRIV/golden-trees.json` and the private runbook.
  - Site EQ values are committed only anonymised and de-duplicated. P6 allows EQ values, not names.
- **P5 — bundle trust:**
  - Only a `golden-bundle-<sha>` artifact from a `push` run on `dev` whose head SHA is the reviewed commit reaches the PC.
  - `check_bundle.py` (dev box) and `Test-GoldenRpp` (PC) re-check hashes, the plug-in allowlist, and job-relative paths.
- **Tier 0:**
  - No local cargo compilation. Allowed locally: `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p`, Python and its tests.
  - Rust tests are proven in CI at Task 12. The bundle is generated only by CI.
- **Tests:**
  - Every feature ships tests that can fail. No `#[ignore]`, no skips, no `continue-on-error`.
  - Coverage never decreases (`iem-rpp` joins the coverage run).
  - Diff-scoped mutation testing, every shard ≤ 20 min. Resize the shard matrix if `mutants-list` says so, never raise a timeout.
- **Branches and identity:** as in S0 (`dev` → PR → `main`, merge commits, noreply identity).
  - Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
  - The PR body ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- **Durable state:**
  - Decisions and findings go on #4 the moment they land.
  - Raw renders and backup manifests go to `$RAW` (private, outside the repo, never `/tmp`).
  - PC backups stay on the PC (they contain the predecessor's secrets). Deleting old backups on the PC needs the owner's explicit approval.

## Review Focus

1. **A render instance that opens the ASIO card.**
   - Expected: `New-GoldenResourceDir -DummyMode 3` throws, and `golden_window.py` refuses `PC_DUMMY_MODE=3`.
   - `Watch-GoldenRender` writes the stop file and returns `asio-alarm` on any module holder.
   - Tests: `test_dummy_mode_three_is_refused` (Python), `Test-GoldenPc.ps1` case `resource-dir-refuses-asio`.
2. **A file REAPER or the app changed that the verification misses** (extra file, changed file, touched mtime, registry value).
   - Expected: `Compare-GoldenManifest` reports changed, missing and extra files. Mtime-only changes are reported as `touched`. Registry exports are compared.
   - Restore copies back and quarantines, never deletes.
   - Test: `Test-GoldenPc.ps1` cases `verify-detects-*` and `restore-makes-identical`.
3. **A tampered or foreign bundle.**
   - Expected: a hash mismatch, an unlisted file, a plug-in outside the three allowed heads, or a media or render path outside `@@JOB@@`/`@@OUT@@` all fail. This holds in CI (`check_bundle.py`), on the dev box before upload, and on the PC (`Invoke-GoldenStage`).
   - Tests: `test_check_bundle.py`, `Test-GoldenPc.ps1` cases `stage-*`, Rust `rejects_a_foreign_plugin`.
4. **"ide event" during a window, or a hung render.**
   - Expected: `preempt` stops the queue, restores, and brings REAPER and the app back.
   - REAPER is never started while any render instance exists, and nothing is killed.
   - Tests: `test_undo_plan_*` (Python). Window 1 exercises `close-render` once.
5. **Over-claimed laws.**
   - Expected: every law in `laws.json` names its cases, its residual and a verdict. The render format is asserted to be IEEE float 64-bit before any 1e-9 comparison. The offline-only residuals are listed and never marked confirmed.
   - Tests: `test_analyze.py` (`test_read_wav_refuses_pcm`, `test_shelf_alpha_classifies_candidates`, `test_linear_oracle_detects_a_wrong_gain`).
6. **Determinism.**
   - Expected: two generator runs give byte-identical bundles. The hot material regenerates bit-identically, since it uses no libm and its checksum was cross-checked by an independent Python implementation.
   - Tests: `bundles_are_byte_identical_across_runs`, `hot_material_matches_the_reference_checksum`.
7. **P6.**
   - Expected: the public diff contains no site value. Site EQs are anonymised; PC facts are only in the private env and runbook.
   - Test: the pre-push denylist scan and the CI `secrets` job.

## Scope decisions (recorded on #4 in Task 1)

- **Synthetic projects only.** No transformed copy of the site project is rendered. The synthetic cases cover every send, volume, pan and FX field the site uses, and site-level fidelity is S4's round trip plus S8's shadow imports.
- **All REAPER goldens S2 needs are rendered in S1b:** the ReaEQ matrix, the site EQs and the limiter. S2 then needs no PC time.
- **The limiter is not ported in S1b** (GPL crate, S2). S1b stores the outputs only, with a ceiling sanity check.
- **Each window restores the pre-window state.** In S0/S1a-era dev time that means REAPER and the predecessor app are running again. A late "ide event" then only needs its checks.
- **Offline cannot measure these, and the report lists them as residuals** (Method B/C, S7/S8):
  - live input duplication, the hardware-output mono downmix, and `norunmute`;
  - live delay compensation, and REAPER's ramps.

## Shell variables (paste at the start of every task)

```bash
export WORK="$HOME/devel/iemmixer"
export PRED="$HOME/devel/reaperiem"
export WP="$HOME/.claude/work-products/iemmixer-gen2"
export PRIV="$HOME/.config/iemmixer"
export OPS="$HOME/devel/iemmixer-ops"
export RAW="$HOME/.local/share/iemmixer/golden-raw"
export GOLDEN_ENV="$PRIV/golden.env"
export REPO=zbynekdrlik/iemmixer
```

## File Structure

```
Cargo.toml                                   + "crates/iem-rpp" in [workspace].members
crates/iem-rpp/Cargo.toml                    new crate (MIT OR Apache-2.0), no new third-party crates
crates/iem-rpp/src/lib.rs                    module list + cli()
crates/iem-rpp/src/rpp.rs                    RPP text writer (quoting, numbers, chunks)
crates/iem-rpp/src/wav.rs                    IEEE-float WAV writer
crates/iem-rpp/src/stimulus.rs               impulse, log sweep, hot material (exact arithmetic)
crates/iem-rpp/src/reaeq.rs                  ReaEQ band model + VST2 state-chunk encoder
crates/iem-rpp/src/fx.rs                     FX chain, JSFX blocks, plug-in allowlist, deterministic GUIDs
crates/iem-rpp/src/project.rs                project/track/send/item model → RPP text
crates/iem-rpp/src/oracle.rs                 impulse-tap oracle under the S1b hypotheses
crates/iem-rpp/src/cases.rs                  case catalogue (design note §6)
crates/iem-rpp/src/bundle.rs                 bundle writer + allowlist check
crates/iem-rpp/src/bin/iem-rpp-gen.rs        thin CLI
crates/iem-rpp/cases/site-eq.json            anonymised, de-duplicated site EQs (Task 7)
crates/iem-rpp/tests/gen_cli.rs              CLI integration test
scripts/golden/check_bundle.py (+test)       bundle gate (CI and dev box)
scripts/golden/GoldenPc.psm1                 PC module (manifests, backup, verify/restore, stage, render queue)
scripts/golden/golden-task.ps1               entry point of the Interactive task (console session)
scripts/golden/Test-GoldenPc.ps1             PC module self-test (CI windows job)
scripts/golden/golden_window.py (+test)      dev-box window driver
scripts/golden/analyze.py (+test)            renders → goldens/s1b
scripts/check_version.py (+test)             CRATES += iem-rpp
scripts/check_integrity.py (+test)           .psm1 scanned for force-kill; goldens/ ≤ 20 MB
.github/workflows/ci.yml                     golden-bundle job; iem-rpp in coverage and mutation; golden self-tests
.gitattributes                               goldens binary vectors
goldens/s1b/{laws.json,README.md,*.f64,index.json}   Task 15
.claude/rules/golden-renders.md              playbook rule (paths: crates/iem-rpp/**, scripts/golden/**, goldens/**)
CLAUDE.md                                    router line
docs/superpowers/specs/2026-09-26-s1b-golden-renders-design.md
docs/superpowers/plans/2026-09-26-s1b-golden-renders.md
```

The following are private and never go in the public repo:
- `$PRIV/golden.env` and `$PRIV/golden-trees.json`;
- `$OPS/tools/extract_site_eq.py`;
- `$WP/14-s1b-pc-runbook-private.md` (moved to `$OPS/docs/` in Task 10);
- `$RAW/<window>/`;
- on the PC, `%LOCALAPPDATA%\iemmixer-golden\`.

---

### Task 1: Start — sync, version, design on the ticket, docs commit

**Files:**
- Add: `docs/superpowers/specs/2026-09-26-s1b-golden-renders-design.md`, `docs/superpowers/plans/2026-09-26-s1b-golden-renders.md` (both already written)

- [ ] **Step 1: Sync and check the version**

```bash
set -euo pipefail
cd "$WORK"
git switch dev
git fetch origin
git merge --ff-only origin/dev
git merge origin/main          # no-op unless main moved
python3 scripts/check_version.py --base-ref origin/main
```
Expected: `dev` is greater than `main`. On 2026-09-26, `dev` was `2.0.0-dev.2` and `main` was `2.0.0-dev.1`.

If the check fails (for example, a PR merged since), bump it first:
```bash
cur=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml","rb"))["workspace"]["package"]["version"])')
next="2.0.0-dev.$(( ${cur##*.} + 1 ))"
sed -i "s/^version = \"$cur\"$/version = \"$next\"/" Cargo.toml
cargo metadata --format-version 1 > /dev/null
python3 scripts/check_version.py --base-ref origin/main
git commit -am "chore: bump version to $next" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 2: Put the design on #4 (durable decision)**

Write `$RAW/../s1b-design-comment.md` (create `$RAW` with `mkdir -p "$RAW" && chmod 700 "$RAW"`). It holds a Slovak summary of design note §3–§4 and §8: the window procedure, the synthetic projects, the pre-window state restore, the licence copy, and the residuals.

The comment carries no site values, and it points to the two docs paths.

```bash
gh issue comment 4 -R "$REPO" --body-file "$RAW/../s1b-design-comment.md"
```

- [ ] **Step 3: Denylist-scan the two docs without touching the index**

```bash
tmpidx="$(mktemp)"; cp .git/index "$tmpidx"
GIT_INDEX_FILE="$tmpidx" git add docs/superpowers/specs/2026-09-26-s1b-golden-renders-design.md docs/superpowers/plans/2026-09-26-s1b-golden-renders.md
tree="$(GIT_INDEX_FILE="$tmpidx" git write-tree)"; rm -f "$tmpidx"
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$tree"
```
Expected: exit 0 with no `denylist entry` lines. On a hit, reword the line (never allowlist a site value) and repeat.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-09-26-s1b-golden-renders-design.md docs/superpowers/plans/2026-09-26-s1b-golden-renders.md
git commit -m "docs(s1b): design note and implementation plan for the golden renders" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `iem-rpp` crate skeleton, version check, CI package lists

**Files:**
- Create: `crates/iem-rpp/Cargo.toml`, `crates/iem-rpp/src/lib.rs`
- Modify: `Cargo.toml`, `Cargo.lock`, `scripts/check_version.py`, `scripts/test_check_version.py`, `.github/workflows/ci.yml`

- [ ] **Step 1: RED — the version check must know every workspace member**

Append to `scripts/test_check_version.py` (class `ConsistencyTests` or a new class):

```python
class WorkspaceMembersTests(unittest.TestCase):
    def test_crates_list_matches_the_workspace_members(self) -> None:
        import tomllib
        root = Path(__file__).resolve().parent.parent
        members = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))["workspace"]["members"]
        self.assertEqual(sorted(m.split("/")[-1] for m in members), sorted(cv.CRATES))
```
Run `python3 -m unittest scripts/test_check_version.py -v`. It passes now. Add the crate (Step 2) and it fails until Step 3.

- [ ] **Step 2: Create the crate**

`crates/iem-rpp/Cargo.toml`:
```toml
[package]
name = "iem-rpp"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
description = "REAPER project (RPP) generator for iemmixer golden renders (importer and exporter follow in S4)"

[dependencies]
base64 = "0.22"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
sha2 = "0.10"
thiserror = "2.0"

[dev-dependencies]
tempfile = "3"

[[bin]]
name = "iem-rpp-gen"
path = "src/bin/iem-rpp-gen.rs"
```

`crates/iem-rpp/src/lib.rs` (modules are added task by task; the full file at the end of Task 8):
```rust
//! REAPER project (RPP) generator for the S1b golden renders. S4 adds the
//! importer and exporter to this crate.

pub mod rpp;
```
Create `crates/iem-rpp/src/rpp.rs` as an empty module for now: `//! RPP text writer (Task 3).`. Create `crates/iem-rpp/src/bin/iem-rpp-gen.rs` with `fn main() {}` (replaced in Task 8).

In the root `Cargo.toml`, set `members = ["crates/iem-core", "crates/iem-server", "crates/iem-ui", "crates/iem-tray", "crates/iem-rpp"]`, then:
```bash
cargo metadata --format-version 1 > /dev/null      # adds iem-rpp to Cargo.lock (no new third-party crates)
git diff --stat Cargo.lock                          # expect only the iem-rpp package entry
python3 -m unittest scripts/test_check_version.py -v   # RED: CRATES lacks iem-rpp
```

- [ ] **Step 3: GREEN — `CRATES` gets the new crate**

In `scripts/check_version.py`: `CRATES = ["iem-core", "iem-server", "iem-ui", "iem-tray", "iem-rpp"]`.
```bash
python3 -m unittest discover -s scripts -p 'test_*.py' -v
python3 scripts/check_version.py
```
Expected: all pass, `version: consistent …`.

- [ ] **Step 4: CI package lists**

In `.github/workflows/ci.yml`:
- `test` job: `cargo llvm-cov --locked --package iem-core --package iem-server --package iem-rpp --all-features …`, and rename the step to "Tests with line coverage (iem-core, iem-server, iem-rpp) against the floor".
- `mutants-list`, `mutation-warmup` and `mutation`: add `--package iem-rpp` or `-p iem-rpp` wherever `iem-ui` is listed (4 places: the `cargo mutants --list` call, both `cargo nextest run … --no-run`, and the `cargo mutants --in-diff` call).

Clippy already runs `--workspace`.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/iem-rpp scripts/check_version.py scripts/test_check_version.py .github/workflows/ci.yml
git commit -m "feat(iem-rpp): crate skeleton for the golden generator" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: RPP text writer

**Files:**
- Create: `crates/iem-rpp/src/rpp.rs` (replace the placeholder)

**Interfaces:**
- Produces:
  - `q(&str) -> Result<String, RppError>` does REAPER quoting: bare, then `"…"`, `'…'` or `` `…` ``.
  - `num(f64) -> Result<String, RppError>` writes the shortest round-trip decimal, never with an exponent; `-0` becomes `0`.
  - `Chunk { head, body }` with `line()`, `child()` and `render()`, using two-space indent and `\n`.

- [ ] **Step 1: Write the module with its tests**

```rust
//! RPP text writer. A REAPER project is line based: `<HEAD …` opens a
//! chunk, `>` closes it, every other line is space-separated tokens.

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum RppError {
    #[error("value is not finite: {0}")]
    NotFinite(f64),
    #[error("string cannot be quoted for RPP: {0:?}")]
    Unquotable(String),
    #[error("invalid project: {0}")]
    Invalid(String),
}

/// Quotes one token the way REAPER does: bare when non-empty without space
/// or quote characters, else the first of `"`, `'`, `` ` `` it does not contain.
pub fn q(s: &str) -> Result<String, RppError> {
    if s.contains(['\n', '\r']) {
        return Err(RppError::Unquotable(s.to_owned()));
    }
    if !s.is_empty() && !s.contains([' ', '"', '\'', '`']) {
        return Ok(s.to_owned());
    }
    ['"', '\'', '`']
        .into_iter()
        .find(|quote| !s.contains(*quote))
        .map(|quote| format!("{quote}{s}{quote}"))
        .ok_or_else(|| RppError::Unquotable(s.to_owned()))
}

/// Shortest decimal that parses back to the same f64 (Rust's `Display`
/// never uses exponent notation). `-0` is written as `0`.
pub fn num(x: f64) -> Result<String, RppError> {
    if !x.is_finite() {
        return Err(RppError::NotFinite(x));
    }
    if x == 0.0 {
        return Ok("0".to_owned());
    }
    Ok(format!("{x}"))
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Line(String),
    Chunk(Chunk),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub head: String,
    pub body: Vec<Node>,
}

impl Chunk {
    pub fn new(head: impl Into<String>) -> Self {
        Self { head: head.into(), body: Vec::new() }
    }

    pub fn line(&mut self, text: impl Into<String>) -> &mut Self {
        self.body.push(Node::Line(text.into()));
        self
    }

    pub fn child(&mut self, chunk: Chunk) -> &mut Self {
        self.body.push(Node::Chunk(chunk));
        self
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out, 0);
        out
    }

    fn render_into(&self, out: &mut String, depth: usize) {
        let pad = "  ".repeat(depth);
        out.push_str(&pad);
        out.push('<');
        out.push_str(&self.head);
        out.push('\n');
        for node in &self.body {
            match node {
                Node::Line(text) => {
                    out.push_str(&pad);
                    out.push_str("  ");
                    out.push_str(text);
                    out.push('\n');
                }
                Node::Chunk(chunk) => chunk.render_into(out, depth + 1),
            }
        }
        out.push_str(&pad);
        out.push_str(">\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_tokens_stay_bare_and_others_are_quoted() {
        assert_eq!(q("reaeq.dll").unwrap(), "reaeq.dll");
        assert_eq!(q("").unwrap(), "\"\"");
        assert_eq!(q("a b").unwrap(), "\"a b\"");
        assert_eq!(q("say \"hi\"").unwrap(), "'say \"hi\"'");
        assert_eq!(q("it's \"x\"").unwrap(), "`it's \"x\"`");
        assert!(q("a\"b'c`d").is_err());
        assert!(q("two\nlines").is_err());
    }

    #[test]
    fn numbers_round_trip_without_exponents() {
        assert_eq!(num(1.0).unwrap(), "1");
        assert_eq!(num(-0.0).unwrap(), "0");
        assert_eq!(num(0.1).unwrap(), "0.1");
        assert_eq!(num(1e-7).unwrap(), "0.0000001");
        assert_eq!(num(0.000803).unwrap(), "0.000803");
        assert_eq!(num(-0.4).unwrap(), "-0.4");
        let x = 2996.2342070275295_f64;
        assert_eq!(num(x).unwrap().parse::<f64>().unwrap().to_bits(), x.to_bits());
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(num(bad).is_err());
        }
    }

    #[test]
    fn chunks_render_nested_with_two_space_indent() {
        let mut inner = Chunk::new("SOURCE WAVE");
        inner.line("FILE \"x.wav\"");
        let mut outer = Chunk::new("ITEM");
        outer.line("POSITION 0").child(inner);
        assert_eq!(outer.render(), "<ITEM\n  POSITION 0\n  <SOURCE WAVE\n    FILE \"x.wav\"\n  >\n>\n");
    }
}
```

- [ ] **Step 2: Format and commit** (the tests run in CI at Task 12)

```bash
cargo fmt -p iem-rpp
git add crates/iem-rpp/src/rpp.rs
git commit -m "feat(iem-rpp): RPP text writer with REAPER quoting" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Float WAV writer and stimuli

**Files:**
- Create: `crates/iem-rpp/src/wav.rs`, `crates/iem-rpp/src/stimulus.rs`
- Modify: `crates/iem-rpp/src/lib.rs` (`pub mod stimulus; pub mod wav;`)

**Interfaces:**
- Produces: `wav::float_wav(rate, bits ∈ {32,64}, &[Vec<f64>]) -> Result<Vec<u8>, WavError>` writes format tag 3, an 18-byte `fmt `, a `fact` and a `data` chunk. The data starts at byte 58.
- Produces:
  - `stimulus::impulse(len, at, amp)`;
  - `stimulus::log_sweep(rate, f1, f2, secs, amp)`, the only stimulus using libm; it is stored in the bundle and never regenerated;
  - `stimulus::hot_material(rate, seed) -> [Vec<f64>; 2]`, which uses exact arithmetic only and is regenerated by S2.

- [ ] **Step 1: `wav.rs`**

```rust
//! Minimal IEEE-float WAV writer (format tag 3) for the stimuli.

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum WavError {
    #[error("no channels")]
    NoChannels,
    #[error("channels differ in length")]
    Ragged,
    #[error("unsupported bit depth {0} (32 or 64)")]
    Bits(u16),
    #[error("sample is not finite")]
    NotFinite,
    #[error("too large for a RIFF file")]
    TooLarge,
}

/// Byte offset of the first sample.
pub const DATA_OFFSET: usize = 58;

pub fn float_wav(rate: u32, bits: u16, channels: &[Vec<f64>]) -> Result<Vec<u8>, WavError> {
    let first = channels.first().ok_or(WavError::NoChannels)?;
    let frames = first.len();
    if channels.iter().any(|c| c.len() != frames) {
        return Err(WavError::Ragged);
    }
    if bits != 32 && bits != 64 {
        return Err(WavError::Bits(bits));
    }
    let too_large = |_| WavError::TooLarge;
    let n_ch = u16::try_from(channels.len()).map_err(too_large)?;
    let block_align = n_ch.checked_mul(bits / 8).ok_or(WavError::TooLarge)?;
    let byte_rate = rate.checked_mul(u32::from(block_align)).ok_or(WavError::TooLarge)?;
    let data_len = frames.checked_mul(usize::from(block_align)).ok_or(WavError::TooLarge)?;
    let data_len32 = u32::try_from(data_len).map_err(too_large)?;
    let riff_len = u32::try_from(DATA_OFFSET - 8 + data_len).map_err(too_large)?;
    let frames32 = u32::try_from(frames).map_err(too_large)?;

    let mut out = Vec::with_capacity(DATA_OFFSET + data_len);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&18u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes());
    out.extend_from_slice(&n_ch.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&frames32.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len32.to_le_bytes());
    for i in 0..frames {
        for ch in channels {
            let x = ch[i];
            if !x.is_finite() {
                return Err(WavError::NotFinite);
            }
            if bits == 64 {
                out.extend_from_slice(&x.to_le_bytes());
            } else {
                #[allow(clippy::cast_possible_truncation)]
                out.extend_from_slice(&(x as f32).to_le_bytes());
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], i: usize) -> u16 {
        u16::from_le_bytes([b[i], b[i + 1]])
    }
    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
    }

    #[test]
    fn writes_a_64_bit_float_header_and_interleaved_frames() {
        let b = float_wav(96_000, 64, &[vec![0.5, -1.0], vec![0.25, 0.0]]).unwrap();
        assert_eq!(&b[0..4], b"RIFF");
        assert_eq!(u32_at(&b, 4) as usize, b.len() - 8);
        assert_eq!(&b[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(&b, 16), 18);
        assert_eq!(u16_at(&b, 20), 3);
        assert_eq!(u16_at(&b, 22), 2);
        assert_eq!(u32_at(&b, 24), 96_000);
        assert_eq!(u32_at(&b, 28), 96_000 * 16);
        assert_eq!(u16_at(&b, 32), 16);
        assert_eq!(u16_at(&b, 34), 64);
        assert_eq!(&b[38..42], b"fact");
        assert_eq!(u32_at(&b, 46), 2);
        assert_eq!(&b[50..54], b"data");
        assert_eq!(u32_at(&b, 54), 32);
        let frame0: Vec<f64> = b[DATA_OFFSET..DATA_OFFSET + 16]
            .chunks(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(frame0, vec![0.5, 0.25]);
        assert_eq!(b.len(), DATA_OFFSET + 32);
    }

    #[test]
    fn writes_32_bit_samples_as_f32() {
        let b = float_wav(48_000, 32, &[vec![0.5]]).unwrap();
        assert_eq!(u16_at(&b, 34), 32);
        assert_eq!(f32::from_le_bytes(b[DATA_OFFSET..DATA_OFFSET + 4].try_into().unwrap()), 0.5);
    }

    #[test]
    fn refuses_bad_input() {
        assert_eq!(float_wav(48_000, 64, &[]), Err(WavError::NoChannels));
        assert_eq!(float_wav(48_000, 64, &[vec![0.0], vec![]]), Err(WavError::Ragged));
        assert_eq!(float_wav(48_000, 24, &[vec![0.0]]), Err(WavError::Bits(24)));
        assert_eq!(float_wav(48_000, 64, &[vec![f64::NAN]]), Err(WavError::NotFinite));
    }
}
```

- [ ] **Step 2: `stimulus.rs`**

The reference checksums below were computed on 2026-09-26 by an independent Python implementation of the same algorithm (IEEE basic operations only, so both must agree bit for bit).

```rust
//! Deterministic stimuli. Everything except the log sweep uses IEEE basic
//! operations only (no libm), so S2 regenerates it bit-identically.

use std::f64::consts::PI;

pub fn impulse(len: usize, at: usize, amp: f64) -> Vec<f64> {
    let mut v = vec![0.0; len];
    if let Some(x) = v.get_mut(at) {
        *x = amp;
    }
    v
}

/// Exponential sweep f1 → f2 over `secs` (stored in the bundle; not regenerated).
pub fn log_sweep(rate: u32, f1: f64, f2: f64, secs: f64, amp: f64) -> Vec<f64> {
    let fs = f64::from(rate);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (fs * secs).round() as usize;
    let k = (f2 / f1).ln();
    (0..n)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f64 / fs;
            amp * (2.0 * PI * f1 * secs / k * ((t / secs * k).exp() - 1.0)).sin()
        })
        .collect()
}

struct XorShift(u64);

impl XorShift {
    /// Uniform in [-1, 1): xorshift64* with the top 53 bits.
    #[allow(clippy::cast_precision_loss)]
    fn next_unit(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
        r as f64 / (1u64 << 52) as f64 - 1.0
    }
}

#[allow(clippy::cast_precision_loss)]
fn triangle(i: usize, period: usize) -> f64 {
    let phase = (i % period) as f64 / period as f64;
    4.0 * (phase - 0.5).abs() - 1.0
}

/// One second of limiter material in ten 100 ms segments, peaks up to +12 dBFS:
/// silence; L-only noise +6 dB (link); both +9.5 dB triangle (2 segments);
/// silence (release, 2 segments); a 0.3/0.6/1.2/2.4 staircase (2 segments);
/// R noise +12 dB with L at -12 dB.
pub fn hot_material(rate: u32, seed: u64) -> [Vec<f64>; 2] {
    let n = rate as usize;
    let seg = n / 10;
    let mut rng = XorShift(seed | 1);
    let mut left = vec![0.0; n];
    let mut right = vec![0.0; n];
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let noise = rng.next_unit();
        let (a, b) = match i / seg {
            0 | 4 | 5 => (0.0, 0.0),
            1 => (2.0 * noise, 0.0),
            2 | 3 => (3.0 * triangle(i, 48), 3.0 * triangle(i, 48)),
            6 | 7 => {
                let step = [0.3, 0.6, 1.2, 2.4][((i - 6 * seg) * 4 / (2 * seg)).min(3)];
                (step * triangle(i, 96), step * triangle(i, 96))
            }
            _ => (0.25 * noise, 4.0 * noise),
        };
        *l = a;
        *r = b;
    }
    [left, right]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn digest(ch: &[Vec<f64>; 2]) -> String {
        let mut h = Sha256::new();
        for c in ch {
            for x in c {
                h.update(x.to_le_bytes());
            }
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn impulse_places_one_sample() {
        assert_eq!(impulse(4, 2, 0.5), vec![0.0, 0.0, 0.5, 0.0]);
        assert_eq!(impulse(2, 5, 0.5), vec![0.0, 0.0]);
    }

    #[test]
    fn hot_material_matches_the_reference_checksum() {
        // Independent Python implementation, 2026-09-26 (seed 27).
        assert_eq!(digest(&hot_material(96_000, 27)), "c920f0bd71bbd2ec121fc287d4fd5be137b1868eabe3f5eb52719a0b1928770e");
        assert_eq!(digest(&hot_material(48_000, 27)), "2bdb3ee5df9d1fc5429ba8e4ee8d82a62d79d0a7a9227b975f9e308b35d12d67");
        assert_eq!(digest(&hot_material(44_100, 27)), "3bcdb799908cf53fbb1fb1b2521481f0feb31ce159e7791a521d3444dc0916f0");
    }

    #[test]
    fn hot_material_segments_have_their_shape() {
        let [l, r] = hot_material(96_000, 27);
        assert!(l[..9_600].iter().chain(&r[..9_600]).all(|x| *x == 0.0));
        assert!(r[9_600..19_200].iter().all(|x| *x == 0.0));
        assert!(l[9_600..19_200].iter().any(|x| x.abs() > 1.9));
        assert!(r[86_400..].iter().any(|x| x.abs() > 3.9));
        assert!(l.iter().chain(&r).all(|x| x.abs() <= 4.0));
    }

    #[test]
    fn log_sweep_has_the_requested_length_and_bound() {
        let s = log_sweep(96_000, 20.0, 20_000.0, 2.0, 0.25);
        assert_eq!(s.len(), 192_000);
        assert!(s.iter().all(|x| x.abs() <= 0.25));
        assert!(s.iter().any(|x| x.abs() > 0.249));
    }
}
```

- [ ] **Step 3: Commit**

```bash
cargo fmt -p iem-rpp
git add crates/iem-rpp/src
git commit -m "feat(iem-rpp): float WAV writer and deterministic stimuli" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: ReaEQ state chunk and FX chain

**Files:**
- Create: `crates/iem-rpp/src/reaeq.rs`, `crates/iem-rpp/src/fx.rs`
- Modify: `crates/iem-rpp/src/lib.rs` (`pub mod fx; pub mod reaeq;`)

**Interfaces:**
- Produces:
  - `reaeq::{BandKind, Band, ReaEq}`, all serde (snake_case kinds: `low_shelf`, `high_shelf`, `high_pass`, `band`);
  - `ReaEq::standard_flat()`, `ReaEq::single(band)` and `ReaEq::chunk()`.
- Produces:
  - `fx::{Fx, FxSlot, fx_chain, guid, ALLOWED_FX_HEADS}`.
  - `ALLOWED_FX_HEADS` holds the only three plug-in heads a generated project may contain. Task 8 and the PC repeat the check.

The chunk layout was verified on 2026-09-26: this encoder reproduced all five base64 lines of a ReaEQ chunk written by REAPER 7.65 byte for byte. The layout:
- the header (60 bytes): `qeer`, `0xfeed5eee`, 2 inputs with masks 1 and 2, 2 outputs with masks 1 and 2, the state length, 1, `0x00100000`;
- the state: `i32 33`, `i32 5`, then five bands of `{i32 type, i32 enabled, f64 Hz, f64 linear gain, f64 octaves, u8 1}`;
- a tail: `i32 1`, `i32 1`, `f64 global gain`, then 16 constant bytes;
- a program line;
- base64 lines of the header, then 96-byte slices of the state, then the program.

- [ ] **Step 1: `reaeq.rs`**

```rust
//! ReaEQ (Cockos) VST2 state chunk, as REAPER 7.65 writes it.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

use crate::rpp::{Chunk, RppError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BandKind {
    LowShelf,
    HighShelf,
    HighPass,
    Band,
}

impl BandKind {
    pub const fn code(self) -> i32 {
        match self {
            Self::LowShelf => 0,
            Self::HighShelf => 1,
            Self::HighPass => 4,
            Self::Band => 8,
        }
    }

    /// Slot of this kind in the standard HP / LS / Band / Band / HS layout.
    const fn slot(self) -> usize {
        match self {
            Self::HighPass => 0,
            Self::LowShelf => 1,
            Self::Band => 2,
            Self::HighShelf => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub kind: BandKind,
    pub enabled: bool,
    pub freq_hz: f64,
    pub gain_lin: f64,
    pub bw_oct: f64,
}

impl Band {
    pub const fn new(kind: BandKind, freq_hz: f64, gain_lin: f64, bw_oct: f64) -> Self {
        Self { kind, enabled: true, freq_hz, gain_lin, bw_oct }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReaEq {
    pub bands: Vec<Band>,
    pub global_gain: f64,
}

/// Default band frequencies as REAPER stores them (bands 2-5 copied from a
/// REAPER-written chunk; the HPF default from its f32 normalised value).
pub const STANDARD_FREQS: [f64; 5] = [80.20834168547682, 200.3077623397404, 801.9398157380639, 2996.2342070275295, 8016.061124722856];
const STANDARD_KINDS: [BandKind; 5] = [BandKind::HighPass, BandKind::LowShelf, BandKind::Band, BandKind::Band, BandKind::HighShelf];
const STANDARD_BWS: [f64; 5] = [2.0, 2.0, 1.0, 1.0, 2.0];
const TAIL: [u8; 16] = [0, 0, 0, 0, 0xfd, 1, 0, 0, 0x5c, 1, 0, 0, 2, 0, 0, 0];
const PROGRAM: &[u8] = b"\0Program 1\0\x10\0\0\0";
pub const HEAD: &str = r#"VST "VST: ReaEQ (Cockos)" reaeq.dll 0 "" 1919247729<56535472656571726561657100000000> """#;

impl ReaEq {
    /// The standard layout, every band off and flat, global gain 1.
    pub fn standard_flat() -> Self {
        let bands = (0..5)
            .map(|i| Band { kind: STANDARD_KINDS[i], enabled: false, freq_hz: STANDARD_FREQS[i], gain_lin: 1.0, bw_oct: STANDARD_BWS[i] })
            .collect();
        Self { bands, global_gain: 1.0 }
    }

    /// The standard layout with one enabled band in its kind's slot.
    pub fn single(band: Band) -> Self {
        let mut eq = Self::standard_flat();
        eq.bands[band.kind.slot()] = band;
        eq
    }

    pub fn state(&self) -> Result<Vec<u8>, RppError> {
        let count = i32::try_from(self.bands.len()).map_err(|_| RppError::Invalid("too many bands".into()))?;
        let mut b = Vec::with_capacity(8 + 33 * self.bands.len() + 32);
        b.extend_from_slice(&33i32.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        for band in &self.bands {
            for x in [band.freq_hz, band.gain_lin, band.bw_oct] {
                if !x.is_finite() || x < 0.0 {
                    return Err(RppError::Invalid(format!("ReaEQ band value {x}")));
                }
            }
            b.extend_from_slice(&band.kind.code().to_le_bytes());
            b.extend_from_slice(&i32::from(band.enabled).to_le_bytes());
            b.extend_from_slice(&band.freq_hz.to_le_bytes());
            b.extend_from_slice(&band.gain_lin.to_le_bytes());
            b.extend_from_slice(&band.bw_oct.to_le_bytes());
            b.push(1);
        }
        if !self.global_gain.is_finite() || self.global_gain < 0.0 {
            return Err(RppError::Invalid(format!("ReaEQ global gain {}", self.global_gain)));
        }
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&1i32.to_le_bytes());
        b.extend_from_slice(&self.global_gain.to_le_bytes());
        b.extend_from_slice(&TAIL);
        Ok(b)
    }

    pub fn chunk(&self) -> Result<Chunk, RppError> {
        let state = self.state()?;
        let len = u32::try_from(state.len()).map_err(|_| RppError::Invalid("state too long".into()))?;
        let mut header = Vec::with_capacity(60);
        header.extend_from_slice(b"qeer");
        header.extend_from_slice(&0xfeed_5eee_u32.to_le_bytes());
        for _ in 0..2 {
            header.extend_from_slice(&2u32.to_le_bytes());
            header.extend_from_slice(&1u64.to_le_bytes());
            header.extend_from_slice(&2u64.to_le_bytes());
        }
        header.extend_from_slice(&len.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&0x0010_0000_u32.to_le_bytes());
        let mut c = Chunk::new(HEAD);
        c.line(STANDARD.encode(&header));
        for part in state.chunks(96) {
            c.line(STANDARD.encode(part));
        }
        c.line(STANDARD.encode(PROGRAM));
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpp::Node;

    fn lines(c: &Chunk) -> Vec<String> {
        c.body.iter().map(|n| match n { Node::Line(l) => l.clone(), Node::Chunk(_) => panic!("nested chunk") }).collect()
    }

    #[test]
    fn matches_a_chunk_written_by_reaper_7_65() {
        let eq = ReaEq::single(Band::new(BandKind::HighPass, 100.41747866600939, 1.0, 2.0));
        assert_eq!(
            lines(&eq.chunk().unwrap()),
            vec![
                "cWVlcu5e7f4CAAAAAQAAAAAAAAACAAAAAAAAAAIAAAABAAAAAAAAAAIAAAAAAAAAzQAAAAEAAAAAABAA",
                "IQAAAAUAAAAEAAAAAQAAAG9ScPi3GllAAAAAAAAA8D8AAAAAAAAAQAEAAAAAAAAAAAQEaDDZCWlAAAAAAAAA8D8AAAAAAAAAQAEIAAAAAAAAAAEaHb6ED4lAAAAAAAAA",
                "8D8AAAAAAADwPwEIAAAAAAAAAHfH++l3aKdAAAAAAAAA8D8AAAAAAADwPwEBAAAAAAAAAKWt3qUPUL9AAAAAAAAA8D8AAAAAAAAAQAEBAAAAAQAAAAAAAAAAAPA/AAAA",
                "AP0BAABcAQAAAgAAAA==",
                "AFByb2dyYW0gMQAQAAAA",
            ]
        );
    }

    #[test]
    fn single_places_each_kind_in_its_slot() {
        for (kind, slot) in [(BandKind::HighPass, 0), (BandKind::LowShelf, 1), (BandKind::Band, 2), (BandKind::HighShelf, 4)] {
            let eq = ReaEq::single(Band::new(kind, 1000.0, 2.0, 1.0));
            assert_eq!(eq.bands[slot].kind, kind);
            assert!(eq.bands[slot].enabled);
            assert_eq!(eq.bands.iter().filter(|b| b.enabled).count(), 1);
        }
    }

    #[test]
    fn state_length_is_in_the_header_and_bad_values_are_refused() {
        let eq = ReaEq::standard_flat();
        assert_eq!(eq.state().unwrap().len(), 205);
        let mut bad = ReaEq::standard_flat();
        bad.bands[2].gain_lin = f64::NAN;
        assert!(bad.chunk().is_err());
        let mut neg = ReaEq::standard_flat();
        neg.global_gain = -1.0;
        assert!(neg.chunk().is_err());
    }

    #[test]
    fn kinds_serialise_in_snake_case() {
        assert_eq!(serde_json::to_string(&BandKind::HighPass).unwrap(), "\"high_pass\"");
        let back: BandKind = serde_json::from_str("\"low_shelf\"").unwrap();
        assert_eq!(back, BandKind::LowShelf);
    }
}
```

- [ ] **Step 2: `fx.rs`**

```rust
//! FX chain blocks: ReaEQ, the trim JSFX and the output limiter JSFX, the
//! plug-in allowlist and deterministic GUIDs.

use sha2::{Digest, Sha256};

use crate::reaeq::{HEAD as REAEQ_HEAD, ReaEq};
use crate::rpp::{Chunk, RppError, num};

pub const TRIM_HEAD: &str = r#"JS utility/volume_pan """#;
pub const LIMITER_HEAD: &str = r#"JS loser/MGA_JSLimiterST """#;
/// Every plug-in head a generated project may contain (P5). The bundle
/// check and the PC stager repeat this list.
pub const ALLOWED_FX_HEADS: [&str; 3] = [REAEQ_HEAD, TRIM_HEAD, LIMITER_HEAD];

#[derive(Debug, Clone, PartialEq)]
pub enum Fx {
    ReaEq(ReaEq),
    /// `utility/volume_pan` slider 1 in dB.
    Trim { db: f64 },
    /// `loser/MGA_JSLimiterST` at threshold = ceiling = `limit_db`, 50 ms, 75 %.
    Limiter { limit_db: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FxSlot {
    pub fx: Fx,
    pub bypassed: bool,
}

impl FxSlot {
    pub const fn active(fx: Fx) -> Self {
        Self { fx, bypassed: false }
    }
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` from the seed's sha256.
pub fn guid(seed: &str) -> String {
    let hex: String = Sha256::digest(seed.as_bytes()).iter().take(16).map(|b| format!("{b:02X}")).collect();
    format!("{{{}-{}-{}-{}-{}}}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn js(head: &str, sliders: &[f64]) -> Result<Chunk, RppError> {
    let mut fields = sliders.iter().map(|v| num(*v)).collect::<Result<Vec<_>, _>>()?;
    fields.resize(64, "-".to_owned());
    let mut c = Chunk::new(head);
    c.line(fields.join(" "));
    Ok(c)
}

pub fn fx_chunk(fx: &Fx) -> Result<Chunk, RppError> {
    match fx {
        Fx::ReaEq(eq) => eq.chunk(),
        Fx::Trim { db } => js(TRIM_HEAD, &[*db, 0.0, 0.0]),
        Fx::Limiter { limit_db } => js(LIMITER_HEAD, &[*limit_db, 50.0, 75.0, *limit_db, 0.0]),
    }
}

pub fn fx_chain(slots: &[FxSlot], seed: &str) -> Result<Chunk, RppError> {
    let mut c = Chunk::new("FXCHAIN");
    c.line("SHOW 0").line("LASTSEL 0").line("DOCKED 0");
    for (i, slot) in slots.iter().enumerate() {
        c.line(format!("BYPASS {} 0 0", u8::from(slot.bypassed)));
        c.child(fx_chunk(&slot.fx)?);
        c.line("FLOATPOS 0 0 0 0");
        c.line(format!("FXID {}", guid(&format!("{seed}/fx{i}"))));
        c.line("WAK 0 0");
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_are_deterministic_and_well_formed() {
        let g = guid("a");
        assert_eq!(g, guid("a"));
        assert_ne!(g, guid("b"));
        assert_eq!(g.len(), 38);
        assert_eq!(g.matches('-').count(), 4);
    }

    #[test]
    fn js_blocks_carry_64_slider_fields() {
        let c = fx_chunk(&Fx::Limiter { limit_db: -6.0 }).unwrap().render();
        assert!(c.starts_with("<JS loser/MGA_JSLimiterST \"\"\n  -6 50 75 -6 0 - "));
        let fields = c.lines().nth(1).unwrap().split_whitespace().count();
        assert_eq!(fields, 64);
        assert!(fx_chunk(&Fx::Trim { db: 6.0 }).unwrap().render().contains("\n  6 0 0 - "));
    }

    #[test]
    fn chain_marks_bypass_per_slot() {
        let slots = [FxSlot::active(Fx::Trim { db: 6.0 }), FxSlot { fx: Fx::Trim { db: 1.0 }, bypassed: true }];
        let text = fx_chain(&slots, "t").unwrap().render();
        let bypass: Vec<&str> = text.lines().filter(|l| l.trim_start().starts_with("BYPASS")).collect();
        assert_eq!(bypass, vec!["  BYPASS 0 0 0", "  BYPASS 1 0 0"]);
        for head in ALLOWED_FX_HEADS {
            assert!(!head.starts_with('<'));
        }
    }
}
```

- [ ] **Step 3: Commit**

```bash
cargo fmt -p iem-rpp
git add crates/iem-rpp/src
git commit -m "feat(iem-rpp): ReaEQ state chunk (byte-exact vs REAPER 7.65) and FX chains" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Project model and the impulse oracle

**Files:**
- Create: `crates/iem-rpp/src/project.rs`, `crates/iem-rpp/src/oracle.rs`
- Modify: `crates/iem-rpp/src/lib.rs` (`pub mod oracle; pub mod project;`)

**Interfaces:**
- Produces: `project::{Project, Track, Send, SendMode, Item, RenderFormat}` and `Project::to_rpp() -> Result<String, RppError>`.
  - Paths are the tokens `"@@JOB@@\stimuli\<file>"` and `"@@OUT@@\<project id>"`, always double-quoted.
- Produces: `oracle::{Tap, balance, post_fader_taps}`.
  - `post_fader_taps` gives every track's expected post-fader impulse taps under the S1b hypotheses, or `None` for a track that uses FX, a mono destination, pre-FX mode, or a source that is `None`.
  - The hypotheses: the +0 dB linear balance law; mono media L = R at unity; mode 3 is pre-fader post-FX; mode 0 is post-fader post-pan; mute zeroes every tap.

Two render keys are UNVERIFIED and are settled by the `cal` family in Task 13:
- `RENDER_STEMS 2` for "selected tracks (stems)";
- the `RENDER_CFG` bytes for 32/64-bit float. REAPER 7.65 writes `ZXZhdxgAAQ==`, which is "evaw" plus 24, 0, 1.

- [ ] **Step 1: `project.rs`**

```rust
//! Project model → RPP text for the golden renders.

use crate::fx::{FxSlot, fx_chain, guid};
use crate::rpp::{Chunk, RppError, num, q};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    PostFader,
    PreFx,
    PreFader,
}

impl SendMode {
    pub const fn code(self) -> u8 {
        match self {
            Self::PostFader => 0,
            Self::PreFx => 1,
            Self::PreFader => 3,
        }
    }
}

/// A receive on the destination track (REAPER's `AUXRECV`).
#[derive(Debug, Clone, PartialEq)]
pub struct Send {
    pub src: usize,
    pub mode: SendMode,
    pub vol: f64,
    pub pan: f64,
    pub mute: bool,
    /// Destination channel field 1024: mix to mono into channel 1.
    pub dst_mono: bool,
}

impl Send {
    pub const fn new(src: usize, mode: SendMode) -> Self {
        Self { src, mode, vol: 1.0, pan: 0.0, mute: false, dst_mono: false }
    }
}

/// A media item; position and length in samples at the project rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub stimulus: String,
    pub position: u64,
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub name: String,
    pub vol: f64,
    pub pan: f64,
    pub mute: bool,
    pub fx_enabled: bool,
    pub fx: Vec<FxSlot>,
    pub item: Option<Item>,
    pub receives: Vec<Send>,
    /// Selected, so it renders as a stem.
    pub render: bool,
}

impl Track {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), vol: 1.0, pan: 0.0, mute: false, fx_enabled: true, fx: Vec::new(), item: None, receives: Vec::new(), render: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    Float64,
    Float32,
}

impl RenderFormat {
    /// `RENDER_CFG` payload "evaw" + bit depth + 0 + 1 (UNVERIFIED until Task 13).
    pub const fn cfg(self) -> &'static str {
        match self {
            Self::Float64 => "ZXZhd0AAAQ==",
            Self::Float32 => "ZXZhdyAAAQ==",
        }
    }

    pub const fn bits(self) -> u16 {
        match self {
            Self::Float64 => 64,
            Self::Float32 => 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub id: String,
    pub rate: u32,
    pub format: RenderFormat,
    pub tracks: Vec<Track>,
}

fn safe_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

impl Project {
    pub fn validate(&self) -> Result<(), RppError> {
        let bad = |m: String| Err(RppError::Invalid(m));
        if !safe_name(&self.id) {
            return bad(format!("project id {:?}", self.id));
        }
        if ![44_100, 48_000, 96_000].contains(&self.rate) {
            return bad(format!("rate {}", self.rate));
        }
        let mut names = std::collections::BTreeSet::new();
        for (i, t) in self.tracks.iter().enumerate() {
            if !safe_name(&t.name) || !names.insert(t.name.as_str()) {
                return bad(format!("track name {:?} (unsafe or duplicate)", t.name));
            }
            if !(t.vol.is_finite() && t.vol >= 0.0 && (-1.0..=1.0).contains(&t.pan)) {
                return bad(format!("track {} vol/pan", t.name));
            }
            if let Some(item) = &t.item {
                if !safe_name(&item.stimulus) || item.length == 0 {
                    return bad(format!("track {} item", t.name));
                }
            }
            for s in &t.receives {
                if s.src >= i || !(s.vol.is_finite() && s.vol >= 0.0 && (-1.0..=1.0).contains(&s.pan)) {
                    return bad(format!("track {} receive from {} (sources precede, gains finite)", t.name, s.src));
                }
            }
        }
        Ok(())
    }

    pub fn to_rpp(&self) -> Result<String, RppError> {
        self.validate()?;
        let mut p = Chunk::new(r#"REAPER_PROJECT 0.1 "7.65/win64" 0 0"#);
        p.line("PANLAW 1").line("PANMODE 3");
        p.line(format!("SAMPLERATE {} 1 0", self.rate));
        p.line("TEMPO 120 4 4 0");
        p.line(format!("RENDER_FILE \"@@OUT@@\\{}\"", self.id));
        p.line("RENDER_PATTERN $track").line("RENDER_FMT 0 2 0").line("RENDER_1X 0");
        p.line("RENDER_RANGE 1 0 0 18 1000").line("RENDER_RESAMPLE 3 0 1").line("RENDER_ADDTOPROJ 0");
        p.line("RENDER_STEMS 2").line("RENDER_DITHER 0").line("RENDER_TRIM 0.000001 0.000001 0 0");
        let mut cfg = Chunk::new("RENDER_CFG");
        cfg.line(self.format.cfg());
        p.child(cfg);
        p.line("MASTER_NCH 2 2").line("MASTER_VOLUME 1 0 -1 -1 1").line("MASTER_PANMODE 3").line("MASTERMUTESOLO 0");
        for t in &self.tracks {
            p.child(self.track_chunk(t)?);
        }
        Ok(p.render())
    }

    fn track_chunk(&self, t: &Track) -> Result<Chunk, RppError> {
        let id = guid(&format!("{}/{}", self.id, t.name));
        let mut c = Chunk::new(format!("TRACK {id}"));
        c.line(format!("NAME {}", q(&t.name)?));
        c.line("PANLAWFLAGS 3");
        c.line(format!("VOLPAN {} {} -1 -1 1", num(t.vol)?, num(t.pan)?));
        c.line(format!("MUTESOLO {} 0 0", u8::from(t.mute)));
        c.line("IPHASE 0");
        c.line(format!("SEL {}", u8::from(t.render)));
        c.line("REC 0 0 0 0 0 0 0 0");
        c.line("NCHAN 2");
        c.line(format!("FX {}", u8::from(t.fx_enabled)));
        c.line(format!("TRACKID {id}"));
        c.line("MAINSEND 0 0");
        for s in &t.receives {
            c.line(format!(
                "AUXRECV {} {} {} {} {} 0 0 0 {} -1:U 0 -1 ''",
                s.src,
                s.mode.code(),
                num(s.vol)?,
                num(s.pan)?,
                u8::from(s.mute),
                if s.dst_mono { 1024 } else { 0 }
            ));
        }
        if !t.fx.is_empty() {
            c.child(fx_chain(&t.fx, &format!("{}/{}", self.id, t.name))?);
        }
        if let Some(item) = &t.item {
            c.child(self.item_chunk(item)?);
        }
        Ok(c)
    }

    #[allow(clippy::cast_precision_loss)]
    fn item_chunk(&self, item: &Item) -> Result<Chunk, RppError> {
        let fs = f64::from(self.rate);
        let mut c = Chunk::new("ITEM");
        c.line(format!("POSITION {}", num(item.position as f64 / fs)?));
        c.line(format!("LENGTH {}", num(item.length as f64 / fs)?));
        c.line("LOOP 0").line("FADEIN 1 0 0 1 0 0 0").line("FADEOUT 1 0 0 1 0 0 0").line("MUTE 0 0");
        c.line(format!("NAME {}", q(&item.stimulus)?));
        c.line("VOLPAN 1 0 1 -1").line("SOFFS 0").line("PLAYRATE 1 1 0 -1 0 0.0025").line("CHANMODE 0");
        let mut src = Chunk::new("SOURCE WAVE");
        src.line(format!("FILE \"@@JOB@@\\stimuli\\{}\"", item.stimulus));
        c.child(src);
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_track() -> Project {
        let mut src = Track::new("src");
        src.render = false;
        src.item = Some(Item { stimulus: "imp-dm-96000.wav".into(), position: 0, length: 48_000 });
        let mut bus = Track::new("bus");
        let mut s = Send::new(0, SendMode::PreFader);
        s.pan = 0.5;
        s.dst_mono = true;
        bus.receives.push(s);
        Project { id: "p".into(), rate: 96_000, format: RenderFormat::Float64, tracks: vec![src, bus] }
    }

    #[test]
    fn writes_render_settings_tokens_and_track_fields() {
        let text = two_track().to_rpp().unwrap();
        for needle in [
            "<REAPER_PROJECT 0.1 \"7.65/win64\" 0 0\n",
            "\n  SAMPLERATE 96000 1 0\n",
            "\n  RENDER_FILE \"@@OUT@@\\p\"\n",
            "\n  RENDER_STEMS 2\n",
            "\n  <RENDER_CFG\n    ZXZhd0AAAQ==\n  >\n",
            "\n    SEL 0\n",
            "\n    AUXRECV 0 3 1 0.5 0 0 0 0 1024 -1:U 0 -1 ''\n",
            "\n        FILE \"@@JOB@@\\stimuli\\imp-dm-96000.wav\"\n",
            "\n      POSITION 0\n      LENGTH 0.5\n",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
    }

    #[test]
    fn refuses_cycles_duplicates_and_unsafe_names() {
        let mut p = two_track();
        p.tracks[1].receives[0].src = 1;
        assert!(p.to_rpp().is_err());
        let mut p = two_track();
        p.tracks[1].name = "src".into();
        assert!(p.to_rpp().is_err());
        let mut p = two_track();
        p.tracks[0].name = "a b".into();
        assert!(p.to_rpp().is_err());
        let mut p = two_track();
        p.tracks[1].receives[0].pan = 1.5;
        assert!(p.to_rpp().is_err());
        let mut p = two_track();
        p.rate = 22_050;
        assert!(p.to_rpp().is_err());
    }

    #[test]
    fn float32_format_changes_only_the_render_cfg() {
        let mut p = two_track();
        p.format = RenderFormat::Float32;
        assert!(p.to_rpp().unwrap().contains("\n    ZXZhdyAAAQ==\n"));
        assert_eq!(RenderFormat::Float32.bits(), 32);
    }
}
```

- [ ] **Step 2: `oracle.rs`**

```rust
//! Impulse-tap oracle for the linear cases, under the hypotheses the
//! goldens test (design note §6): +0 dB linear balance law, mono media
//! duplicated at unity, mode 3 = pre-fader post-FX, mode 0 = post-fader
//! post-pan, mute zeroes every tap. analyze.py compares renders with it.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::project::{Project, SendMode, Track};

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Tap {
    pub at: u64,
    pub l: f64,
    pub r: f64,
}

/// Stereo balance gains for pan p ∈ [-1, 1] at a +0 dB law.
pub fn balance(p: f64) -> (f64, f64) {
    (1.0 - p.max(0.0), 1.0 + p.min(0.0))
}

type Taps = BTreeMap<u64, (f64, f64)>;

fn add(into: &mut Taps, from: &Taps, gl: f64, gr: f64) {
    for (&at, &(l, r)) in from {
        let e = into.entry(at).or_insert((0.0, 0.0));
        e.0 += gl * l;
        e.1 += gr * r;
    }
}

fn track_taps(t: &Track, pre: &[Option<Taps>], post: &[Option<Taps>], stim: &dyn Fn(&str) -> Option<Vec<Tap>>) -> Option<(Taps, Taps)> {
    if !t.fx.is_empty() {
        return None;
    }
    let mut p = Taps::new();
    if let Some(item) = &t.item {
        for tap in stim(&item.stimulus)? {
            let e = p.entry(item.position + tap.at).or_insert((0.0, 0.0));
            e.0 += tap.l;
            e.1 += tap.r;
        }
    }
    for s in &t.receives {
        if s.dst_mono || s.mode == SendMode::PreFx {
            return None;
        }
        let from = (if s.mode == SendMode::PreFader { pre.get(s.src)? } else { post.get(s.src)? }).as_ref()?;
        if s.mute {
            continue;
        }
        let (gl, gr) = balance(s.pan);
        add(&mut p, from, s.vol * gl, s.vol * gr);
    }
    if t.mute {
        p.clear();
    }
    let (gl, gr) = balance(t.pan);
    let mut o = Taps::new();
    add(&mut o, &p, t.vol * gl, t.vol * gr);
    Some((p, o))
}

/// Expected post-fader taps per track (None where the oracle does not apply).
pub fn post_fader_taps(project: &Project, stim: &dyn Fn(&str) -> Option<Vec<Tap>>) -> Vec<Option<Vec<Tap>>> {
    let mut pre: Vec<Option<Taps>> = Vec::with_capacity(project.tracks.len());
    let mut post: Vec<Option<Taps>> = Vec::with_capacity(project.tracks.len());
    for t in &project.tracks {
        match track_taps(t, &pre, &post, stim) {
            Some((p, o)) => {
                pre.push(Some(p));
                post.push(Some(o));
            }
            None => {
                pre.push(None);
                post.push(None);
            }
        }
    }
    post.into_iter()
        .map(|m| m.map(|m| m.into_iter().map(|(at, (l, r))| Tap { at, l, r }).collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{Fx, FxSlot};
    use crate::project::{Item, RenderFormat, Send};

    fn stim(file: &str) -> Option<Vec<Tap>> {
        (file == "dm").then(|| vec![Tap { at: 10, l: 0.5, r: 0.5 }])
    }

    fn src(name: &str) -> Track {
        let mut t = Track::new(name);
        t.item = Some(Item { stimulus: "dm".into(), position: 0, length: 100 });
        t
    }

    #[test]
    fn balance_is_the_plus_zero_db_linear_law() {
        assert_eq!(balance(0.0), (1.0, 1.0));
        assert_eq!(balance(0.5), (0.5, 1.0));
        assert_eq!(balance(-1.0), (1.0, 0.0));
    }

    #[test]
    fn pre_fader_ignores_the_source_fader_and_post_fader_follows_it() {
        let mut s = src("s");
        s.vol = 0.25;
        s.pan = 0.5;
        let mut pre_bus = Track::new("pre");
        pre_bus.receives.push(Send::new(0, SendMode::PreFader));
        let mut post_bus = Track::new("post");
        post_bus.receives.push(Send::new(0, SendMode::PostFader));
        let p = Project { id: "o".into(), rate: 96_000, format: RenderFormat::Float64, tracks: vec![s, pre_bus, post_bus] };
        let taps = post_fader_taps(&p, &stim);
        assert_eq!(taps[1], Some(vec![Tap { at: 10, l: 0.5, r: 0.5 }]));
        assert_eq!(taps[2], Some(vec![Tap { at: 10, l: 0.0625, r: 0.125 }]));
    }

    #[test]
    fn mute_zeroes_every_tap_and_fx_or_mono_leave_the_oracle() {
        let mut s = src("s");
        s.mute = true;
        let mut bus = Track::new("b");
        bus.receives.push(Send::new(0, SendMode::PreFader));
        let mut fx = src("f");
        fx.fx.push(FxSlot::active(Fx::Trim { db: 6.0 }));
        let mut mono = Track::new("m");
        let mut send = Send::new(0, SendMode::PreFader);
        send.dst_mono = true;
        mono.receives.push(send);
        let p = Project { id: "o".into(), rate: 96_000, format: RenderFormat::Float64, tracks: vec![s, bus, fx, mono] };
        let taps = post_fader_taps(&p, &stim);
        assert_eq!(taps[1], Some(vec![]));
        assert_eq!(taps[2], None);
        assert_eq!(taps[3], None);
    }
}
```

- [ ] **Step 3: Commit**

```bash
cargo fmt -p iem-rpp
git add crates/iem-rpp/src
git commit -m "feat(iem-rpp): project model to RPP and the impulse-tap oracle" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Site EQs (private extraction) and the case catalogue

**Files:**
- Create (private): `$OPS/tools/extract_site_eq.py`
- Create: `crates/iem-rpp/cases/site-eq.json`, `crates/iem-rpp/src/cases.rs`
- Modify: `crates/iem-rpp/src/lib.rs` (`pub mod cases;`)

**Interfaces:**
- Produces:
  - `cases::catalogue(only: &[String]) -> Result<Catalogue, CatalogueError>` over `FAMILIES = [cal, pan, mute, sum, downmix, mono, bypass, eq, site-eq, lim]`.
  - `Catalogue { projects: Vec<Case>, stimuli: Vec<Stimulus> }`, where `Case { project, meta: Vec<CaseMeta> }`.
  - `CaseMeta { track, family, stimulus, position, params, expect }` is written into `bundle.json` for `analyze.py`.

- [ ] **Step 1: Private extraction tool (ops repo)**

`$OPS/tools/extract_site_eq.py` is private because it knows the site project's path. It reads every ReaEQ chunk from the predecessor's saved project at a pinned SHA and writes the anonymised, de-duplicated list:

```python
#!/usr/bin/env python3
"""Extract the site's ReaEQ settings (PRIVATE: reads the site project).
Output: anonymised, de-duplicated JSON for crates/iem-rpp/cases/site-eq.json
in the public repo — numbers only (P6 allows EQ values), no names, order
by content hash, ids EQ-01…"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import struct
import subprocess
import sys

KINDS = {0: "low_shelf", 1: "high_shelf", 4: "high_pass", 8: "band"}


def chunks(text: str):
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        if lines[i].strip().startswith('<VST "VST: ReaEQ (Cockos)"'):
            body = []
            i += 1
            while lines[i].strip() != ">":
                body.append(lines[i].strip())
                i += 1
            yield body
        i += 1


def decode(body: list[str]) -> dict:
    raw = b"".join(base64.b64decode(line) for line in body)
    size = struct.unpack_from("<I", raw, 48)[0]
    state = raw[60:60 + size]
    ver, n = struct.unpack_from("<ii", state, 0)
    if ver != 33:
        raise SystemExit(f"unexpected ReaEQ state version {ver}")
    bands, off = [], 8
    for _ in range(n):
        kind, enabled, hz, gain, bw, _flag = struct.unpack_from("<iidddB", state, off)
        off += 33
        if kind not in KINDS:
            raise SystemExit(f"unexpected band type {kind}")
        bands.append({"kind": KINDS[kind], "enabled": bool(enabled), "freq_hz": hz, "gain_lin": gain, "bw_oct": bw})
    _a, _b, global_gain = struct.unpack_from("<iid", state, off)
    return {"bands": bands, "global_gain": global_gain}


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--rev", required=True, help="pinned predecessor SHA")
    ap.add_argument("--path", required=True, help="project path inside the predecessor repo (private)")
    ap.add_argument("--expect", type=int, required=True, help="number of ReaEQ chunks (program spec §3.1: 44)")
    ap.add_argument("--out", required=True)
    args = ap.parse_args(argv)
    text = subprocess.run(["git", "-C", args.repo, "show", f"{args.rev}:{args.path}"], check=True, capture_output=True, text=True).stdout
    eqs = [decode(b) for b in chunks(text)]
    if len(eqs) != args.expect:
        raise SystemExit(f"found {len(eqs)} ReaEQ chunks, expected {args.expect}")
    unique = {json.dumps(e, sort_keys=True): e for e in eqs}
    ordered = sorted(unique.items(), key=lambda kv: hashlib.sha256(kv[0].encode()).hexdigest())
    out = [{"id": f"EQ-{i + 1:02d}", "eq": e} for i, (_, e) in enumerate(ordered)]
    with open(args.out, "w", encoding="utf-8") as fh:
        json.dump(out, fh, indent=1)
        fh.write("\n")
    print(f"{len(eqs)} chunks, {len(out)} distinct EQs written", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
```

Run it. The project path and the pinned SHA come from the private runbook §5. Record the SHA used on #4.
```bash
python3 "$OPS/tools/extract_site_eq.py" --repo "$PRED" --rev "<pinned SHA, runbook §5>" --path "<site project path, runbook §5>" \
  --expect 44 --out "$WORK/crates/iem-rpp/cases/site-eq.json"
git -C "$OPS" add tools/extract_site_eq.py && git -C "$OPS" commit -m "tools: extract the site EQs for the S1b goldens (anonymised)"
```
Expected: `44 chunks, N distinct EQs written`. The dry run against the backup branch on 2026-09-26 found 15 distinct EQs. The JSON has only `id`, `eq.bands[*].{kind,enabled,freq_hz,gain_lin,bw_oct}` and `eq.global_gain`.

- [ ] **Step 2: `cases.rs`**

```rust
//! The S1b case catalogue (design note §6). Every rendered track is one
//! case; its metadata tells scripts/golden/analyze.py what it measures.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::fx::{Fx, FxSlot};
use crate::oracle::{Tap, post_fader_taps};
use crate::project::{Item, Project, RenderFormat, Send, SendMode, Track};
use crate::reaeq::{Band, BandKind, ReaEq};
use crate::stimulus::{hot_material, impulse, log_sweep};

pub const FAMILIES: [&str; 10] = ["cal", "pan", "mute", "sum", "downmix", "mono", "bypass", "eq", "site-eq", "lim"];
pub const RATES: [u32; 3] = [44_100, 48_000, 96_000];
/// Impulse amplitude (headroom for +12 dB gains in float renders).
pub const AMP: f64 = 0.5;
/// Seed of the limiter material (S2 regenerates it with the same seed).
pub const HOT_SEED: u64 = 27;

#[derive(Debug, Clone, PartialEq)]
pub struct Stimulus {
    pub file: String,
    pub rate: u32,
    pub channels: Vec<Vec<f64>>,
    /// Impulse taps (None for sweeps and hot material).
    pub taps: Option<Vec<Tap>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseMeta {
    pub track: String,
    pub family: String,
    pub stimulus: Option<String>,
    pub position: u64,
    pub params: Value,
    pub expect: Option<Vec<Tap>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub project: Project,
    pub meta: Vec<CaseMeta>,
}

pub struct Catalogue {
    pub projects: Vec<Case>,
    pub stimuli: Vec<Stimulus>,
}

#[derive(Debug, Error)]
pub enum CatalogueError {
    #[error("unknown family: {0}")]
    Family(String),
    #[error("site EQ list: {0}")]
    SiteEq(#[from] serde_json::Error),
}

pub fn db(x: f64) -> f64 {
    10f64.powf(x / 20.0)
}

fn k0(rate: u32) -> u64 {
    u64::from(rate) / 100
}

fn k2(rate: u32) -> u64 {
    u64::from(rate) / 50
}

fn samples(rate: u32, ms: u64) -> u64 {
    u64::from(rate) * ms / 1000
}

pub fn stimuli() -> Vec<Stimulus> {
    let mut out = Vec::new();
    for rate in RATES {
        #[allow(clippy::cast_possible_truncation)]
        let (n, a, b) = (samples(rate, 500) as usize, k0(rate) as usize, k2(rate) as usize);
        let (ta, tb) = (k0(rate), k2(rate));
        out.push(Stimulus { file: format!("imp-dm-{rate}.wav"), rate, channels: vec![impulse(n, a, AMP), impulse(n, a, AMP)], taps: Some(vec![Tap { at: ta, l: AMP, r: AMP }]) });
        out.push(Stimulus {
            file: format!("imp-st-{rate}.wav"),
            rate,
            channels: vec![impulse(n, a, AMP), impulse(n, b, AMP)],
            taps: Some(vec![Tap { at: ta, l: AMP, r: 0.0 }, Tap { at: tb, l: 0.0, r: AMP }]),
        });
        // Hypothesis under test (A2): mono media plays as L = R = x.
        out.push(Stimulus { file: format!("imp-mono-{rate}.wav"), rate, channels: vec![impulse(n, a, AMP)], taps: Some(vec![Tap { at: ta, l: AMP, r: AMP }]) });
        let [l, r] = hot_material(rate, HOT_SEED);
        out.push(Stimulus { file: format!("hot-{rate}.wav"), rate, channels: vec![l, r], taps: None });
    }
    let s = log_sweep(96_000, 20.0, 20_000.0, 2.0, 0.25);
    out.push(Stimulus { file: "sweep-96000.wav".into(), rate: 96_000, channels: vec![s.clone(), s], taps: None });
    out
}

fn source(name: &str, stimulus: &str, rate: u32, ms: u64) -> Track {
    let mut t = Track::new(name);
    t.render = false;
    t.item = Some(Item { stimulus: stimulus.into(), position: 0, length: samples(rate, ms) });
    t
}

fn bus(name: &str, receives: Vec<Send>) -> Track {
    let mut t = Track::new(name);
    t.receives = receives;
    t
}

fn send(src: usize, mode: SendMode, vol: f64, pan: f64) -> Send {
    let mut s = Send::new(src, mode);
    s.vol = vol;
    s.pan = pan;
    s
}

struct Builder {
    id: String,
    rate: u32,
    format: RenderFormat,
    tracks: Vec<Track>,
    meta: Vec<CaseMeta>,
}

impl Builder {
    fn new(id: impl Into<String>, rate: u32) -> Self {
        Self { id: id.into(), rate, format: RenderFormat::Float64, tracks: Vec::new(), meta: Vec::new() }
    }

    fn push(&mut self, t: Track) -> usize {
        self.tracks.push(t);
        self.tracks.len() - 1
    }

    fn case(&mut self, mut t: Track, family: &str, params: Value) -> usize {
        t.render = true;
        self.meta.push(CaseMeta {
            track: t.name.clone(),
            family: family.into(),
            stimulus: t.item.as_ref().map(|i| i.stimulus.clone()),
            position: t.item.as_ref().map_or(0, |i| i.position),
            params,
            expect: None,
        });
        self.push(t)
    }

    fn finish(self, stimuli: &[Stimulus]) -> Case {
        let project = Project { id: self.id, rate: self.rate, format: self.format, tracks: self.tracks };
        let rate = project.rate;
        let lookup = |file: &str| stimuli.iter().find(|s| s.file == file && s.rate == rate).and_then(|s| s.taps.clone());
        let taps = post_fader_taps(&project, &lookup);
        let mut meta = self.meta;
        for m in &mut meta {
            if let Some(i) = project.tracks.iter().position(|t| t.name == m.track) {
                m.expect.clone_from(&taps[i]);
            }
        }
        Case { project, meta }
    }
}

fn cal(stimuli: &[Stimulus]) -> Vec<Case> {
    let r = 96_000;
    [RenderFormat::Float64, RenderFormat::Float32]
        .into_iter()
        .map(|format| {
            let bits = format.bits();
            let mut b = Builder::new(format!("cal-96000-f{bits}"), r);
            b.format = format;
            for (case, stim) in [("identity", "imp-dm-96000.wav"), ("stereo", "imp-st-96000.wav"), ("mono", "imp-mono-96000.wav")] {
                b.case(source(&format!("cal{bits}-{case}"), stim, r, 500), "cal", json!({"case": case, "bits": bits}));
            }
            let mut trim = source(&format!("cal{bits}-trim6"), "imp-dm-96000.wav", r, 500);
            trim.fx.push(FxSlot::active(Fx::Trim { db: 6.0 }));
            b.case(trim, "cal", json!({"case": "trim6", "bits": bits, "trim_db": 6.0}));
            let band = Band::new(BandKind::Band, 1000.0, db(6.0), 1.0);
            let mut peak = source(&format!("cal{bits}-peak"), "imp-dm-96000.wav", r, 500);
            peak.fx.push(FxSlot::active(Fx::ReaEq(ReaEq::single(band))));
            b.case(peak, "cal", json!({"case": "peak", "bits": bits, "band": band}));
            b.finish(stimuli)
        })
        .collect()
}

pub fn pan_points() -> Vec<f64> {
    let mut p: Vec<f64> = (-20..=20).map(|i| f64::from(i) / 20.0).collect();
    p.extend([0.86, -0.4, 0.04]);
    p
}

fn pan(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("pan-96000", r);
    for (i, p) in pan_points().into_iter().enumerate() {
        for (kind, stim) in [("dm", "imp-dm-96000.wav"), ("st", "imp-st-96000.wav")] {
            let src = b.push(source(&format!("src-send-{kind}-{i:02}"), stim, r, 500));
            b.case(bus(&format!("pan-send-{kind}-{i:02}"), vec![send(src, SendMode::PreFader, 1.0, p)]), "pan", json!({"what": "send_pan", "source": kind, "pan": p}));
        }
        let mut t = source(&format!("pan-track-st-{i:02}"), "imp-st-96000.wav", r, 500);
        t.pan = p;
        b.case(t, "pan", json!({"what": "track_pan", "source": "st", "pan": p}));
    }
    for (i, (tp, sp)) in [(-0.5, -0.5), (-0.5, 0.0), (-0.5, 0.5), (0.5, -0.5), (0.5, 0.0), (0.5, 0.5)].into_iter().enumerate() {
        let mut src = source(&format!("src-post-{i}"), "imp-st-96000.wav", r, 500);
        src.vol = 0.5;
        src.pan = tp;
        let src = b.push(src);
        b.case(bus(&format!("pan-post-{i}"), vec![send(src, SendMode::PostFader, 1.0, sp)]), "pan", json!({"what": "post_fader", "track_vol": 0.5, "track_pan": tp, "pan": sp}));
    }
    for (i, v) in [0.000803, 0.5, 1.0, 2.0, 3.981, 4.0].into_iter().enumerate() {
        let src = b.push(source(&format!("src-vol-{i}"), "imp-dm-96000.wav", r, 500));
        b.case(bus(&format!("pan-vol-{i}"), vec![send(src, SendMode::PreFader, v, 0.0)]), "pan", json!({"what": "send_vol", "vol": v}));
    }
    b.finish(stimuli)
}

fn mute(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("mute-96000", r);
    for (name, track_mute, mode, send_mute) in [
        ("mute-track-pre", true, SendMode::PreFader, false),
        ("mute-track-post", true, SendMode::PostFader, false),
        ("mute-send", false, SendMode::PreFader, true),
        ("mute-control", false, SendMode::PreFader, false),
    ] {
        let mut src = source(&format!("src-{name}"), "imp-dm-96000.wav", r, 500);
        src.mute = track_mute;
        let src = b.push(src);
        let mut s = Send::new(src, mode);
        s.mute = send_mute;
        b.case(bus(name, vec![s]), "mute", json!({"track_mute": track_mute, "mode": mode.code(), "send_mute": send_mute}));
    }
    for (name, bus_mute) in [("mute-bus-tap", true), ("mute-bus-tap-control", false)] {
        let src = b.push(source(&format!("src-{name}"), "imp-dm-96000.wav", r, 500));
        let mut mid = bus(&format!("mid-{name}"), vec![Send::new(src, SendMode::PreFader)]);
        mid.render = false;
        mid.mute = bus_mute;
        let mid = b.push(mid);
        b.case(bus(name, vec![Send::new(mid, SendMode::PostFader)]), "mute", json!({"bus_mute": bus_mute, "mode": 0}));
    }
    b.finish(stimuli)
}

fn sum(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("sum-96000", r);
    let mut in1 = source("in1", "imp-dm-96000.wav", r, 500);
    in1.vol = 0.25;
    let mut in2 = source("in2", "imp-st-96000.wav", r, 500);
    if let Some(item) = in2.item.as_mut() {
        item.position = 3 * k0(r);
    }
    let mut in3 = source("in3", "imp-dm-96000.wav", r, 500);
    in3.pan = -0.3;
    if let Some(item) = in3.item.as_mut() {
        item.position = 6 * k0(r);
    }
    let (i1, i2, i3) = (b.push(in1), b.push(in2), b.push(in3));
    let mut stems = bus("sum-stems", vec![send(i1, SendMode::PreFader, 0.5, 0.0), send(i2, SendMode::PreFader, 0.25, 0.5), send(i3, SendMode::PostFader, 1.0, 0.0)]);
    stems.vol = 0.5;
    let st = b.case(stems, "sum", json!({"node": "stems"}));
    let mut out = bus("sum-out", vec![send(st, SendMode::PostFader, 1.0, 0.0), send(i1, SendMode::PreFader, 2.0, 0.0)]);
    out.vol = 2.0;
    let o = b.case(out, "sum", json!({"node": "output"}));
    b.case(bus("sum-elevated", vec![send(o, SendMode::PostFader, 0.5, 0.0)]), "sum", json!({"node": "elevated"}));
    b.finish(stimuli)
}

fn downmix(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("downmix-96000", r);
    for (name, kind, vol, pan) in [("dmx-st-p0", "st", 1.0, 0.0), ("dmx-st-pl", "st", 1.0, -0.5), ("dmx-st-pr", "st", 1.0, 0.5), ("dmx-st-v05", "st", 0.5, 0.0), ("dmx-dm-p0", "dm", 1.0, 0.0)] {
        let src = b.push(source(&format!("src-{name}"), &format!("imp-{kind}-96000.wav"), r, 500));
        let mut s = send(src, SendMode::PreFader, vol, pan);
        s.dst_mono = true;
        b.case(bus(name, vec![s]), "downmix", json!({"source": kind, "vol": vol, "pan": pan, "k": [k0(r), k2(r)]}));
    }
    b.finish(stimuli)
}

fn mono(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("mono-96000", r);
    for (name, stim, pan) in [("mono-item-p0", "imp-mono-96000.wav", 0.0), ("mono-item-p05", "imp-mono-96000.wav", 0.5), ("mono-dm-p0", "imp-dm-96000.wav", 0.0)] {
        let mut t = source(name, stim, r, 500);
        t.pan = pan;
        b.case(t, "mono", json!({"file": stim, "pan": pan}));
    }
    b.finish(stimuli)
}

fn bypass(stimuli: &[Stimulus]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("bypass-96000", r);
    let band = Band::new(BandKind::Band, 1000.0, db(12.0), 1.0);
    let eq = || FxSlot::active(Fx::ReaEq(ReaEq::single(band)));
    let trim = || FxSlot::active(Fx::Trim { db: 6.0 });
    let mut chain_off = source("byp-chain", "imp-dm-96000.wav", r, 500);
    chain_off.fx = vec![trim(), eq()];
    chain_off.fx_enabled = false;
    b.case(chain_off, "bypass", json!({"what": "chain_off", "expect": "identity"}));
    let mut slot = source("byp-slot", "imp-dm-96000.wav", r, 500);
    let mut off = eq();
    off.bypassed = true;
    slot.fx = vec![trim(), off];
    b.case(slot, "bypass", json!({"what": "eq_slot_bypassed", "expect": "trim_only", "trim_db": 6.0}));
    let mut control = source("byp-control", "imp-dm-96000.wav", r, 500);
    control.fx = vec![trim(), eq()];
    b.case(control, "bypass", json!({"what": "control", "expect": "trim_and_eq", "trim_db": 6.0, "band": band}));
    b.finish(stimuli)
}

const EQ_FREQS: [f64; 5] = [20.0, 80.0, 1000.0, 8000.0, 20_000.0];
const EQ_GAINS_DB: [f64; 4] = [-12.0, -3.0, 3.0, 12.0];
const EQ_BWS: [f64; 6] = [0.01, 0.4, 0.8, 1.5, 2.0, 4.0];
const HP_BWS: [f64; 4] = [0.4, 1.0, 2.0, 4.0];

fn eq_track(name: &str, stimulus: &str, rate: u32, ms: u64, eq: ReaEq) -> Track {
    let mut t = source(name, stimulus, rate, ms);
    t.fx.push(FxSlot::active(Fx::ReaEq(eq)));
    t
}

fn eq(stimuli: &[Stimulus], rate: u32) -> Case {
    let mut b = Builder::new(format!("eq-{rate}"), rate);
    let imp = format!("imp-dm-{rate}.wav");
    for (kind, tag) in [(BandKind::LowShelf, "ls"), (BandKind::HighShelf, "hs"), (BandKind::Band, "pk")] {
        for (fi, f) in EQ_FREQS.into_iter().enumerate() {
            for (gi, g) in EQ_GAINS_DB.into_iter().enumerate() {
                for (wi, w) in EQ_BWS.into_iter().enumerate() {
                    let band = Band::new(kind, f, db(g), w);
                    b.case(eq_track(&format!("eq-{tag}-f{fi}-g{gi}-w{wi}"), &imp, rate, 500, ReaEq::single(band)), "eq", json!({"band": band, "global_gain": 1.0}));
                }
            }
        }
    }
    for (fi, f) in EQ_FREQS.into_iter().enumerate() {
        for (gi, g) in [1.0, 0.5, db(9.15)].into_iter().enumerate() {
            for (wi, w) in HP_BWS.into_iter().enumerate() {
                let band = Band::new(BandKind::HighPass, f, g, w);
                b.case(eq_track(&format!("eq-hp-f{fi}-g{gi}-w{wi}"), &imp, rate, 500, ReaEq::single(band)), "eq", json!({"band": band, "global_gain": 1.0}));
            }
        }
    }
    let mut disabled = ReaEq::single(Band::new(BandKind::Band, 1000.0, db(12.0), 1.0));
    disabled.bands[2].enabled = false;
    let mut global = ReaEq::standard_flat();
    global.global_gain = 0.5;
    let mut cascade = ReaEq::standard_flat();
    cascade.bands[1] = Band::new(BandKind::LowShelf, 200.0, db(6.0), 2.0);
    cascade.bands[4] = Band::new(BandKind::HighShelf, 8000.0, db(-6.0), 2.0);
    for (name, e) in [
        ("eq-edge-gain0", ReaEq::single(Band::new(BandKind::Band, 1000.0, 0.0, 1.0))),
        ("eq-edge-bw0", ReaEq::single(Band::new(BandKind::Band, 1000.0, db(6.0), 0.0))),
        ("eq-edge-top", ReaEq::single(Band::new(BandKind::Band, 24_000.0, db(6.0), 1.0))),
        ("eq-edge-disabled", disabled),
        ("eq-edge-global", global),
        ("eq-edge-cascade", cascade),
    ] {
        let params = json!({"eq": &e});
        b.case(eq_track(name, &imp, rate, 500, e), "eq-edge", params);
    }
    b.finish(stimuli)
}

#[derive(Deserialize)]
struct SiteEq {
    id: String,
    eq: ReaEq,
}

pub fn site_eqs() -> Result<Vec<(String, ReaEq)>, serde_json::Error> {
    let list: Vec<SiteEq> = serde_json::from_str(include_str!("../cases/site-eq.json"))?;
    Ok(list.into_iter().map(|s| (s.id, s.eq)).collect())
}

fn site_eq(stimuli: &[Stimulus], eqs: &[(String, ReaEq)]) -> Case {
    let r = 96_000;
    let mut b = Builder::new("site-eq-96000", r);
    for (id, e) in eqs {
        let lid = id.to_ascii_lowercase();
        b.case(eq_track(&format!("seq-{lid}-imp"), "imp-dm-96000.wav", r, 500, e.clone()), "site-eq", json!({"id": id, "eq": e, "stimulus": "impulse"}));
        b.case(eq_track(&format!("seq-{lid}-sweep"), "sweep-96000.wav", r, 2000, e.clone()), "site-eq", json!({"id": id, "eq": e, "stimulus": "sweep"}));
    }
    b.finish(stimuli)
}

fn lim(stimuli: &[Stimulus]) -> Vec<Case> {
    [(96_000, &[("m6", -6.0), ("m3", -3.0), ("0", 0.0)][..]), (48_000, &[("m6", -6.0)][..]), (44_100, &[("m6", -6.0)][..])]
        .into_iter()
        .map(|(rate, limits)| {
            let mut b = Builder::new(format!("lim-{rate}"), rate);
            for (tag, l) in limits {
                let mut t = source(&format!("lim-{rate}-{tag}"), &format!("hot-{rate}.wav"), rate, 1000);
                t.fx.push(FxSlot::active(Fx::Limiter { limit_db: *l }));
                b.case(t, "lim", json!({"limit_db": l, "release_ms": 50, "link_pct": 75, "seed": HOT_SEED}));
            }
            b.finish(stimuli)
        })
        .collect()
}

pub fn catalogue(only: &[String]) -> Result<Catalogue, CatalogueError> {
    if let Some(bad) = only.iter().find(|f| !FAMILIES.contains(&f.as_str())) {
        return Err(CatalogueError::Family(bad.clone()));
    }
    let want = |f: &str| only.is_empty() || only.iter().any(|o| o == f);
    let stimuli = stimuli();
    let mut projects = Vec::new();
    if want("cal") {
        projects.extend(cal(&stimuli));
    }
    for (name, build) in [("pan", pan as fn(&[Stimulus]) -> Case), ("mute", mute), ("sum", sum), ("downmix", downmix), ("mono", mono), ("bypass", bypass)] {
        if want(name) {
            projects.push(build(&stimuli));
        }
    }
    if want("eq") {
        projects.extend(RATES.into_iter().map(|rate| eq(&stimuli, rate)));
    }
    if want("site-eq") {
        let eqs = site_eqs()?;
        if !eqs.is_empty() {
            projects.push(site_eq(&stimuli, &eqs));
        }
    }
    if want("lim") {
        projects.extend(lim(&stimuli));
    }
    Ok(Catalogue { projects, stimuli })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::ALLOWED_FX_HEADS;

    fn all() -> Catalogue {
        catalogue(&[]).unwrap()
    }

    fn case<'a>(c: &'a Catalogue, track: &str) -> &'a CaseMeta {
        c.projects.iter().flat_map(|p| &p.meta).find(|m| m.track == track).unwrap()
    }

    #[test]
    fn every_project_renders_and_track_names_are_globally_unique() {
        let c = all();
        let mut seen = std::collections::BTreeSet::new();
        for p in &c.projects {
            p.project.to_rpp().unwrap();
            for m in &p.meta {
                assert!(seen.insert(m.track.clone()), "duplicate case {}", m.track);
            }
        }
    }

    #[test]
    fn family_sizes_are_as_designed() {
        let c = all();
        let count = |id: &str| c.projects.iter().find(|p| p.project.id == id).unwrap().meta.len();
        assert_eq!(count("cal-96000-f64"), 5);
        assert_eq!(count("pan-96000"), 44 * 3 + 6 + 6);
        assert_eq!(count("mute-96000"), 6);
        assert_eq!(count("sum-96000"), 3);
        assert_eq!(count("downmix-96000"), 5);
        assert_eq!(count("eq-96000"), 360 + 60 + 6);
        assert_eq!(count("lim-96000"), 3);
    }

    #[test]
    fn oracle_expectations_follow_the_hypotheses() {
        let c = all();
        let p = pan_points().iter().position(|p| *p == 0.5).unwrap();
        assert_eq!(case(&c, &format!("pan-send-dm-{p:02}")).expect, Some(vec![Tap { at: 960, l: 0.25, r: 0.5 }]));
        assert_eq!(case(&c, "mute-track-pre").expect, Some(vec![]));
        assert_eq!(case(&c, "mute-bus-tap").expect, Some(vec![]));
        assert!(case(&c, "mute-control").expect.as_ref().is_some_and(|t| !t.is_empty()));
        assert_eq!(case(&c, "dmx-st-p0").expect, None);
        assert_eq!(case(&c, "cal64-trim6").expect, None);
    }

    #[test]
    fn sum_topology_matches_the_hand_computed_taps() {
        let c = all();
        let got = case(&c, "sum-elevated").expect.clone().unwrap();
        let want = [(960, 1.125, 1.125), (3_840, 0.03125, 0.0), (4_800, 0.0, 0.0625), (6_720, 0.25, 0.175)];
        assert_eq!(got.len(), want.len());
        for (g, (at, l, r)) in got.iter().zip(want) {
            assert_eq!(g.at, at);
            assert!((g.l - l).abs() < 1e-15 && (g.r - r).abs() < 1e-15, "{g:?}");
        }
    }

    #[test]
    fn only_allowlisted_plugins_appear() {
        for p in &all().projects {
            for line in p.project.to_rpp().unwrap().lines() {
                let t = line.trim();
                if t.starts_with("<VST") || t.starts_with("<JS") {
                    assert!(ALLOWED_FX_HEADS.contains(&&t[1..]), "{t}");
                }
            }
        }
    }

    #[test]
    fn unknown_family_is_refused_and_filters_work() {
        assert!(matches!(catalogue(&["nope".into()]), Err(CatalogueError::Family(_))));
        let cal_only = catalogue(&["cal".into()]).unwrap();
        assert_eq!(cal_only.projects.len(), 2);
        assert_eq!(cal_only.stimuli.len(), 13);
    }

    #[test]
    fn site_eq_file_parses() {
        let eqs = site_eqs().unwrap();
        assert!(eqs.iter().all(|(id, e)| id.starts_with("EQ-") && e.bands.len() == 5));
    }
}
```

The sum test values follow from the oracle hypotheses. They were computed by hand in the design review:
- in1: dual-mono, fader 0.25;
- in2: stereo at +30 ms;
- in3: dual-mono at +60 ms with pan −0.3;
- stems: 0.5·in1 pre, 0.25·in2 pre with pan 0.5, in3 post, fader 0.5;
- out: stems post plus 2·in1 pre, fader 2;
- elevated: 0.5·out post.

- [ ] **Step 3: Denylist-check the site EQ file, format, commit**

```bash
cargo fmt -p iem-rpp
tmpidx="$(mktemp)"; cp .git/index "$tmpidx"; GIT_INDEX_FILE="$tmpidx" git add crates/iem-rpp
tree="$(GIT_INDEX_FILE="$tmpidx" git write-tree)"; rm -f "$tmpidx"
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$tree"
git add crates/iem-rpp
git commit -m "feat(iem-rpp): case catalogue and the anonymised site EQs" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Bundle writer, CLI, bundle gate, CI `golden-bundle` job

**Files:**
- Create: `crates/iem-rpp/src/bundle.rs`, `crates/iem-rpp/tests/gen_cli.rs`, `scripts/golden/check_bundle.py`, `scripts/golden/test_check_bundle.py`
- Modify: `crates/iem-rpp/src/lib.rs`, `crates/iem-rpp/src/bin/iem-rpp-gen.rs`, `.github/workflows/ci.yml`

**Interfaces:**
- Produces: `iem-rpp-gen --out DIR [--only fam,…]`. It writes `DIR/{projects/*.rpp, stimuli/*.wav, bundle.json}` and exits 2 on a non-empty `DIR` or a bad argument.
- Produces: `bundle.json` = `{schema: 1, generator, projects: [{file, id, rate, bits, tracks: [CaseMeta]}], stimuli: [{file, rate, channels, frames}], files: [{path, sha256, bytes}]}`.
- Produces: `python3 scripts/golden/check_bundle.py DIR`. It exits 0 when clean and 1 with one line per problem.

- [ ] **Step 1: `bundle.rs`**

```rust
//! Writes a golden bundle: projects, stimuli and a hash manifest.

use std::fs;
use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::cases::{CaseMeta, Catalogue};
use crate::fx::ALLOWED_FX_HEADS;
use crate::wav::float_wav;

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("output folder is not empty: {0}")]
    NotEmpty(String),
    #[error("plug-in not on the allowlist: {0}")]
    Plugin(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Rpp(#[from] crate::rpp::RppError),
    #[error(transparent)]
    Wav(#[from] crate::wav::WavError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Serialize)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct ProjectEntry {
    pub file: String,
    pub id: String,
    pub rate: u32,
    pub bits: u16,
    pub tracks: Vec<CaseMeta>,
}

#[derive(Debug, Serialize)]
pub struct StimulusEntry {
    pub file: String,
    pub rate: u32,
    pub channels: usize,
    pub frames: usize,
}

#[derive(Debug, Serialize)]
pub struct Manifest {
    pub schema: u32,
    pub generator: String,
    pub projects: Vec<ProjectEntry>,
    pub stimuli: Vec<StimulusEntry>,
    pub files: Vec<FileEntry>,
}

const FX_WORDS: [&str; 9] = ["VST", "VST3", "JS", "CLAP", "AU", "AUi", "DX", "LV2", "VIDEO_EFFECT"];

/// Every plug-in chunk must be one of the allowed heads (P5).
pub fn check_allowlist(rpp: &str) -> Result<(), BundleError> {
    for line in rpp.lines() {
        let t = line.trim();
        let Some(head) = t.strip_prefix('<') else { continue };
        let word = head.split_whitespace().next().unwrap_or_default();
        if FX_WORDS.contains(&word) && !ALLOWED_FX_HEADS.contains(&head) {
            return Err(BundleError::Plugin(t.to_owned()));
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write(root: &Path, rel: &str, bytes: &[u8]) -> Result<FileEntry, BundleError> {
    fs::write(root.join(rel), bytes)?;
    Ok(FileEntry { path: rel.to_owned(), sha256: hex(&Sha256::digest(bytes)), bytes: bytes.len() as u64 })
}

pub fn write_bundle(cat: &Catalogue, out: &Path) -> Result<Manifest, BundleError> {
    if out.exists() && fs::read_dir(out)?.next().is_some() {
        return Err(BundleError::NotEmpty(out.display().to_string()));
    }
    fs::create_dir_all(out.join("projects"))?;
    fs::create_dir_all(out.join("stimuli"))?;
    let mut files = Vec::new();
    let mut stimuli = Vec::new();
    for s in &cat.stimuli {
        files.push(write(out, &format!("stimuli/{}", s.file), &float_wav(s.rate, 64, &s.channels)?)?);
        stimuli.push(StimulusEntry { file: s.file.clone(), rate: s.rate, channels: s.channels.len(), frames: s.channels.first().map_or(0, Vec::len) });
    }
    let mut projects = Vec::new();
    for case in &cat.projects {
        let text = case.project.to_rpp()?;
        check_allowlist(&text)?;
        let rel = format!("projects/{}.rpp", case.project.id);
        files.push(write(out, &rel, text.as_bytes())?);
        projects.push(ProjectEntry { file: rel, id: case.project.id.clone(), rate: case.project.rate, bits: case.project.format.bits(), tracks: case.meta.clone() });
    }
    let manifest = Manifest { schema: 1, generator: format!("iem-rpp {}", env!("CARGO_PKG_VERSION")), projects, stimuli, files };
    fs::write(out.join("bundle.json"), serde_json::to_vec_pretty(&manifest)?)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cases::catalogue;

    #[test]
    fn rejects_a_foreign_plugin_and_accepts_the_allowed_ones() {
        assert!(check_allowlist("  <VST \"VST3: Other (Vendor)\" other.vst3 0 \"\" 1{x} \"\"\n").is_err());
        assert!(check_allowlist("<JS utility/tonegenerator \"\"\n").is_err());
        assert!(check_allowlist("  <JS utility/volume_pan \"\"\n  <SOURCE WAVE\n  AUXRECV 0 3 1 0 0 0 0 0 0 -1:U 0 -1 ''\n").is_ok());
    }

    #[test]
    fn bundles_are_byte_identical_across_runs_and_hashes_match_the_files() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let cat = catalogue(&["cal".into(), "sum".into()]).unwrap();
        let ma = write_bundle(&cat, a.path()).unwrap();
        write_bundle(&cat, b.path()).unwrap();
        assert_eq!(fs::read(a.path().join("bundle.json")).unwrap(), fs::read(b.path().join("bundle.json")).unwrap());
        for f in &ma.files {
            let bytes = fs::read(a.path().join(&f.path)).unwrap();
            assert_eq!(hex(&Sha256::digest(&bytes)), f.sha256);
            assert_eq!(bytes, fs::read(b.path().join(&f.path)).unwrap());
        }
    }

    #[test]
    fn refuses_a_used_folder() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("x"), b"x").unwrap();
        assert!(matches!(write_bundle(&catalogue(&["cal".into()]).unwrap(), d.path()), Err(BundleError::NotEmpty(_))));
    }
}
```

- [ ] **Step 2: `lib.rs` (final) and the binary**

```rust
//! REAPER project (RPP) generator for the S1b golden renders. S4 adds the
//! importer and exporter to this crate.

pub mod bundle;
pub mod cases;
pub mod fx;
pub mod oracle;
pub mod project;
pub mod reaeq;
pub mod rpp;
pub mod stimulus;
pub mod wav;

use std::path::PathBuf;

/// `iem-rpp-gen --out DIR [--only FAMILY,…]` → one-line summary.
pub fn cli(args: &[String]) -> Result<String, String> {
    let mut out = None;
    let mut only = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--out" => out = it.next().map(PathBuf::from),
            "--only" => only = it.next().map(|s| s.split(',').map(str::to_owned).collect()).unwrap_or_default(),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    let out = out.ok_or_else(|| "usage: iem-rpp-gen --out DIR [--only FAMILY,...]".to_owned())?;
    let cat = cases::catalogue(&only).map_err(|e| e.to_string())?;
    let m = bundle::write_bundle(&cat, &out).map_err(|e| e.to_string())?;
    let cases: usize = m.projects.iter().map(|p| p.tracks.len()).sum();
    let bytes: u64 = m.files.iter().map(|f| f.bytes).sum();
    Ok(format!("{} projects, {} cases, {} stimuli, {} bytes", m.projects.len(), cases, m.stimuli.len(), bytes))
}
```

`crates/iem-rpp/src/bin/iem-rpp-gen.rs`:
```rust
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match iem_rpp::cli(&args) {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("iem-rpp-gen: {e}");
            ExitCode::from(2)
        }
    }
}
```

`crates/iem-rpp/tests/gen_cli.rs` (`gen` is a reserved keyword in edition 2024, hence `run_gen`):
```rust
use std::process::Command;

fn run_gen(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_iem-rpp-gen")).args(args).output().unwrap()
}

#[test]
fn writes_a_calibration_bundle_and_refuses_a_used_folder_or_bad_family() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("b");
    let first = run_gen(&["--out", out.to_str().unwrap(), "--only", "cal"]);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert!(String::from_utf8_lossy(&first.stdout).starts_with("2 projects, 10 cases, 13 stimuli, "));
    assert!(out.join("bundle.json").is_file());
    assert!(out.join("projects/cal-96000-f64.rpp").is_file());
    let again = run_gen(&["--out", out.to_str().unwrap(), "--only", "cal"]);
    assert_eq!(again.status.code(), Some(2));
    let bad = run_gen(&["--out", dir.path().join("c").to_str().unwrap(), "--only", "nope"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown family: nope"));
    assert_eq!(run_gen(&["--bogus"]).status.code(), Some(2));
}
```

- [ ] **Step 3: RED then GREEN — `scripts/golden/check_bundle.py`**

Write `scripts/golden/test_check_bundle.py` first:

```python
"""Tests for scripts/golden/check_bundle.py."""
from __future__ import annotations

import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_bundle as cb  # noqa: E402

GOOD_RPP = """<REAPER_PROJECT 0.1 "7.65/win64" 0 0
  RENDER_FILE "@@OUT@@\\p"
  <TRACK {X}
    <FXCHAIN
      <JS utility/volume_pan ""
        6 0 0
      >
    >
    <ITEM
      <SOURCE WAVE
        FILE "@@JOB@@\\stimuli\\imp-dm-96000.wav"
      >
    >
  >
>
"""


def make(root: Path, rpp: str = GOOD_RPP, extra: bool = False, tamper: bool = False) -> None:
    (root / "projects").mkdir(parents=True)
    (root / "stimuli").mkdir()
    files = {"projects/p.rpp": rpp.encode(), "stimuli/imp-dm-96000.wav": b"RIFF-test"}
    for rel, data in files.items():
        (root / rel).write_bytes(data)
    manifest = {"schema": 1, "projects": [{"file": "projects/p.rpp", "id": "p"}],
                "files": [{"path": p, "sha256": hashlib.sha256(d).hexdigest(), "bytes": len(d)} for p, d in files.items()]}
    (root / "bundle.json").write_text(json.dumps(manifest), encoding="utf-8")
    if extra:
        (root / "stimuli" / "unlisted.wav").write_bytes(b"x")
    if tamper:
        (root / "stimuli" / "imp-dm-96000.wav").write_bytes(b"RIFF-evil")


class CheckBundleTests(unittest.TestCase):
    def check(self, **kw) -> list[str]:
        with tempfile.TemporaryDirectory() as d:
            make(Path(d), **kw)
            return cb.problems(Path(d))

    def test_a_clean_bundle_passes(self) -> None:
        self.assertEqual(self.check(), [])

    def test_tampered_and_unlisted_files_fail(self) -> None:
        self.assertEqual(self.check(tamper=True), ["stimuli/imp-dm-96000.wav: sha256 mismatch"])
        self.assertEqual(self.check(extra=True), ["stimuli/unlisted.wav: not listed in bundle.json"])

    def test_foreign_plugins_and_outside_paths_fail(self) -> None:
        foreign = GOOD_RPP.replace('<JS utility/volume_pan ""', '<VST "VST3: Other" o.vst3 0 "" 1 ""')
        self.assertIn("projects/p.rpp:5: plug-in not on the allowlist", self.check(rpp=foreign))
        outside = GOOD_RPP.replace("@@JOB@@\\stimuli\\imp-dm-96000.wav", "C:\\Windows\\x.wav")
        self.assertNotEqual(outside, GOOD_RPP)
        self.assertIn("projects/p.rpp:11: media path outside the job", self.check(rpp=outside))
        render = GOOD_RPP.replace('"@@OUT@@\\p"', '"D:\\elsewhere"')
        self.assertNotEqual(render, GOOD_RPP)
        self.assertIn("projects/p.rpp:2: render path outside the job", self.check(rpp=render))

    def test_main_exit_codes(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            make(Path(d))
            self.assertEqual(cb.main([d]), 0)
        with tempfile.TemporaryDirectory() as d:
            make(Path(d), tamper=True)
            self.assertEqual(cb.main([d]), 1)


if __name__ == "__main__":
    unittest.main()
```

Run `python3 -m unittest discover -s scripts/golden -p 'test_check_bundle.py' -v`. It is RED because the module does not exist yet. Then write `scripts/golden/check_bundle.py`:

```python
#!/usr/bin/env python3
"""Golden bundle gate (P5): every file listed with its sha256, nothing
unlisted, every plug-in on the allowlist, every media and render path
inside the job tokens. Runs in CI (golden-bundle) and on the dev box
before a bundle is uploaded to the PC. Mirrors GoldenPc.psm1 Test-GoldenRpp."""
from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

ALLOWED = {
    'VST "VST: ReaEQ (Cockos)" reaeq.dll 0 "" 1919247729<56535472656571726561657100000000> ""',
    'JS utility/volume_pan ""',
    'JS loser/MGA_JSLimiterST ""',
}
FX_HEAD = re.compile(r"^<(VST3?|JS|CLAP|AUi?|DX|LV2|VIDEO_EFFECT)\b")
FILE_LINE = re.compile(r'^FILE "@@JOB@@\\stimuli\\[A-Za-z0-9._-]+\.wav"$')
RENDER_LINE = re.compile(r'^RENDER_FILE "@@OUT@@\\[A-Za-z0-9._-]+"$')


def rpp_problems(rel: str, text: str) -> list[str]:
    out: list[str] = []
    for n, line in enumerate(text.splitlines(), start=1):
        t = line.strip()
        if FX_HEAD.match(t) and t[1:] not in ALLOWED:
            out.append(f"{rel}:{n}: plug-in not on the allowlist")
        if t.startswith("FILE ") and not FILE_LINE.match(t):
            out.append(f"{rel}:{n}: media path outside the job")
        if t.startswith("RENDER_FILE ") and not RENDER_LINE.match(t):
            out.append(f"{rel}:{n}: render path outside the job")
    return out


def problems(root: Path) -> list[str]:
    manifest = json.loads((root / "bundle.json").read_text(encoding="utf-8"))
    listed = {f["path"]: f for f in manifest.get("files", [])}
    on_disk = {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file()} - {"bundle.json"}
    out = [f"{p}: not listed in bundle.json" for p in sorted(on_disk - listed.keys())]
    out += [f"{p}: listed but missing" for p in sorted(listed.keys() - on_disk)]
    for rel, entry in sorted(listed.items()):
        path = root / rel
        if not path.is_file():
            continue
        data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != entry["sha256"]:
            out.append(f"{rel}: sha256 mismatch")
        if rel.endswith(".rpp"):
            out += rpp_problems(rel, data.decode("utf-8"))
    if not manifest.get("projects"):
        out.append("bundle.json: no projects")
    return out


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print("usage: check_bundle.py BUNDLE_DIR", file=sys.stderr)
        return 2
    found = problems(Path(argv[0]))
    for item in found:
        print(item)
    if found:
        return 1
    print("bundle: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
```
Run it again. The result is GREEN.

- [ ] **Step 4: CI job `golden-bundle`**

Add to `.github/workflows/ci.yml` (after `test`):
```yaml
  golden-bundle:
    name: golden-bundle
    if: github.event_name == 'push' && github.ref == 'refs/heads/dev'
    needs: [test]
    runs-on: ubuntu-24.04
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - name: Generate the golden bundle
        run: cargo run --locked --release -p iem-rpp --bin iem-rpp-gen -- --out "$RUNNER_TEMP/golden-bundle"
      - name: Check the bundle (hashes, plug-in allowlist, job-relative paths)
        run: python3 scripts/golden/check_bundle.py "$RUNNER_TEMP/golden-bundle"
      - name: Upload (dev pushes only — the only bundle that may reach the PC, P5)
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: golden-bundle-${{ github.sha }}
          path: ${{ runner.temp }}/golden-bundle
          retention-days: 14
          if-no-files-found: error
```
It is push-only, so it is not a required check. Add the golden self-tests to the `integrity` job (the numpy venv also serves Task 11):
```yaml
      - name: Golden tooling self-tests (numpy venv)
        run: |
          set -euo pipefail
          python3 -m venv "$RUNNER_TEMP/gvenv"
          "$RUNNER_TEMP/gvenv/bin/pip" install --quiet numpy==2.4.6
          "$RUNNER_TEMP/gvenv/bin/python" -m unittest discover -s scripts/golden -p 'test_*.py' -v
```

- [ ] **Step 5: Commit**

```bash
cargo fmt -p iem-rpp
python3 -m unittest discover -s scripts/golden -p 'test_*.py' -v
git add crates/iem-rpp scripts/golden .github/workflows/ci.yml
git commit -m "feat(iem-rpp): bundle writer, iem-rpp-gen, bundle gate and the golden-bundle CI job" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: PC module, task entry point, Windows self-test

**Files:**
- Create: `scripts/golden/GoldenPc.psm1`, `scripts/golden/golden-task.ps1`, `scripts/golden/Test-GoldenPc.ps1`
- Modify: `scripts/check_integrity.py`, `scripts/test_check_integrity.py` (scan `.psm1`), `.github/workflows/ci.yml` (`windows` job)

**Interfaces (all exported by `GoldenPc.psm1`, Windows PowerShell 5.1):**
- **Manifests and backup:**
  - `Get-GoldenManifest -Roots <hashtable name→path>` returns rows `{root, rel, size, sha256, mtime}`.
  - `Compare-GoldenManifest -Before -After` returns `{identical, changed, missing, extra, touched}`.
  - `Get-GoldenTrees -Path <trees.json>` returns a hashtable with environment variables expanded.
  - `Invoke-GoldenBackup -Roots -Dest [-RegistryKeys] [-TaskNames]` writes `Dest\trees\<name>\…`, `Dest\reg-N.reg`, `Dest\task-N.xml` and `Dest\manifest.json`. It throws unless the copy equals the source.
- **Verify and restore:** `Invoke-GoldenVerify -Backup [-Restore]` returns `{identical, files, registry, restored, quarantined}`. With `-Restore` it copies back and quarantines, never deletes.
- **Staging:**
  - `Test-GoldenRpp -Text` throws on a plug-in outside the allowlist, or a FILE or RENDER_FILE path outside the tokens.
  - `Invoke-GoldenStage -Bundle -Job` verifies hashes and unlisted files, runs `Test-GoldenRpp`, substitutes the tokens, and returns the staged project paths.
  - `New-GoldenResourceDir -Path -MainResource -DummyMode [-Rate]` refuses mode 3 and writes a minimal `reaper.ini`, the two JSFX, and the licence files.
- **Render (console session):**
  - `Get-GoldenAsioHolders -Module` returns `image:pid` strings.
  - `Write-GoldenRequest -Root -Kind -Fields` returns a request id.
  - `Write-GoldenStatus`.
  - `Invoke-GoldenRenderQueue` runs in the console session with a bounded wait per project and no kill.
  - `Watch-GoldenRender -Root -RequestId -AsioModule -TimeoutSec` returns an outcome of `done`, `hung`, `failed`, `asio-alarm` or `timeout`.
  - `Request-GoldenCloseRender -IniPath` sends `CloseMainWindow()` to the render instances.
  - `Register-GoldenTask -Root [-Name]` registers an Interactive, Limited task with no time limit and `IgnoreNew`.
- **REAPER and app:**
  - `Invoke-GoldenSaveQuit -Http -Project -AsioModule`;
  - `Wait-GoldenProcessGone -Name -Seconds`;
  - `Test-GoldenHttp -Uri` returns the HTTP status, or 0;
  - `Invoke-GoldenBringBack -Root -StartTask -Http -AppExe -AppProcess -AppHttp -WantReaper -WantApp`;
  - `Get-GoldenMeterSamples -Http -Seconds` returns raw `/_/NTRACK;TRACK` texts;
  - `Get-GoldenFileHashes -Path` returns `{rel, sha256, size}`.

- [ ] **Step 1: RED — the integrity scan must cover `.psm1`**

In `scripts/test_check_integrity.py` add:
```python
    def test_force_kill_in_a_powershell_module_is_found(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "scripts" / "golden").mkdir(parents=True)
            (root / "scripts" / "golden" / "M.psm1").write_text("function X { Stop-Process -Id 1 }\n", encoding="utf-8")
            self.assertEqual(ci.violations(root), ["scripts/golden/M.psm1:1: force-kill command (program spec I8)"])
```
Use the module alias the test file already uses (`ci` in this example; check the file's import). Run it: RED. Then add `".psm1"` to `CODE_SUFFIXES` in `scripts/check_integrity.py` and run it again: GREEN.

- [ ] **Step 2: `scripts/golden/GoldenPc.psm1`**

```powershell
#Requires -Version 5.1
# S1b golden renders: PC-side work (design note §4). Never ends a process by
# force; the render instance never uses audio mode 3 (ASIO).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:AllowedFxHeads = @(
    'VST "VST: ReaEQ (Cockos)" reaeq.dll 0 "" 1919247729<56535472656571726561657100000000> ""',
    'JS utility/volume_pan ""',
    'JS loser/MGA_JSLimiterST ""'
)
$script:Utf8NoBom = New-Object System.Text.UTF8Encoding $false

function Invoke-GoldenExe {
    param([Parameter(Mandatory)][string]$FilePath, [Parameter(Mandatory)][string[]]$ArgumentList)
    $out = [IO.Path]::GetTempFileName(); $err = [IO.Path]::GetTempFileName()
    try {
        $p = Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -NoNewWindow -Wait -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
        return $p.ExitCode
    } finally { Remove-Item -LiteralPath $out, $err -ErrorAction SilentlyContinue }
}

function Get-GoldenManifest {
    param([Parameter(Mandatory)][hashtable]$Roots)
    $rows = New-Object System.Collections.Generic.List[object]
    foreach ($name in ($Roots.Keys | Sort-Object)) {
        $root = [IO.Path]::GetFullPath($Roots[$name]).TrimEnd('\')
        if (-not (Test-Path -LiteralPath $root -PathType Container)) { throw "root '$name' is missing: $root" }
        foreach ($f in (Get-ChildItem -LiteralPath $root -Recurse -File -Force | Sort-Object FullName)) {
            $rows.Add([pscustomobject]@{
                root   = $name
                rel    = $f.FullName.Substring($root.Length + 1)
                size   = $f.Length
                sha256 = (Get-FileHash -LiteralPath $f.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                mtime  = $f.LastWriteTimeUtc.ToString('o')
            })
        }
    }
    return ,$rows.ToArray()
}

function Compare-GoldenManifest {
    param([Parameter(Mandatory)][AllowEmptyCollection()][object[]]$Before, [Parameter(Mandatory)][AllowEmptyCollection()][object[]]$After)
    $b = @{}; foreach ($r in $Before) { $b["$($r.root)|$($r.rel)"] = $r }
    $a = @{}; foreach ($r in $After) { $a["$($r.root)|$($r.rel)"] = $r }
    $changed = @(); $missing = @(); $extra = @(); $touched = @()
    foreach ($k in $b.Keys) {
        if (-not $a.ContainsKey($k)) { $missing += $k }
        elseif ($a[$k].sha256 -ne $b[$k].sha256 -or $a[$k].size -ne $b[$k].size) { $changed += $k }
        elseif ($a[$k].mtime -ne $b[$k].mtime) { $touched += $k }
    }
    foreach ($k in $a.Keys) { if (-not $b.ContainsKey($k)) { $extra += $k } }
    [pscustomobject]@{
        identical = (($changed.Count + $missing.Count + $extra.Count) -eq 0)
        changed = @($changed | Sort-Object); missing = @($missing | Sort-Object)
        extra = @($extra | Sort-Object); touched = @($touched | Sort-Object)
    }
}

function Get-GoldenTrees {
    param([Parameter(Mandatory)][string]$Path)
    $json = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
    $roots = @{}
    foreach ($p in $json.PSObject.Properties) { $roots[$p.Name] = [Environment]::ExpandEnvironmentVariables([string]$p.Value) }
    return $roots
}

function Export-GoldenRegistry {
    param([Parameter(Mandatory)][string[]]$Keys, [Parameter(Mandatory)][string]$Dest)
    $files = @()
    for ($i = 0; $i -lt $Keys.Count; $i++) {
        $file = Join-Path $Dest "reg-$i.reg"
        $code = Invoke-GoldenExe -FilePath 'reg.exe' -ArgumentList @('export', "`"$($Keys[$i])`"", "`"$file`"", '/y')
        if ($code -ne 0) { throw "reg export failed for key $i (exit $code)" }
        $files += [pscustomobject]@{ key = $Keys[$i]; file = "reg-$i.reg"; sha256 = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant() }
    }
    return ,$files
}

function Invoke-GoldenBackup {
    param([Parameter(Mandatory)][hashtable]$Roots, [Parameter(Mandatory)][string]$Dest,
          [string[]]$RegistryKeys = @(), [string[]]$TaskNames = @())
    if (Test-Path -LiteralPath $Dest) { throw "backup destination exists: $Dest" }
    New-Item -ItemType Directory -Path $Dest | Out-Null
    $source = Get-GoldenManifest -Roots $Roots
    $copies = @{}
    foreach ($name in $Roots.Keys) {
        $target = Join-Path $Dest ("trees\" + $name)
        $code = Invoke-GoldenExe -FilePath 'robocopy.exe' -ArgumentList @("`"$($Roots[$name])`"", "`"$target`"", '/E', '/COPY:DAT', '/DCOPY:DAT', '/R:0', '/W:0', '/NP', '/NFL', '/NDL', '/NJH', '/NJS')
        if ($code -ge 8) { throw "robocopy failed for '$name' (exit $code)" }
        $copies[$name] = $target
    }
    $diff = Compare-GoldenManifest -Before $source -After (Get-GoldenManifest -Roots $copies)
    if (-not $diff.identical) { throw "backup copy differs from its source: $($diff.changed.Count) changed, $($diff.missing.Count) missing, $($diff.extra.Count) extra" }
    $registry = @()   # never `$x = if … { @() }`: an empty array assigned that way becomes $null
    if ($RegistryKeys.Count -gt 0) { $registry = @(Export-GoldenRegistry -Keys $RegistryKeys -Dest $Dest) }
    $tasks = @()
    for ($i = 0; $i -lt $TaskNames.Count; $i++) {
        $xml = Export-ScheduledTask -TaskName $TaskNames[$i]
        [IO.File]::WriteAllText((Join-Path $Dest "task-$i.xml"), $xml, $script:Utf8NoBom)
        $tasks += [pscustomobject]@{ name = $TaskNames[$i]; file = "task-$i.xml" }
    }
    $manifest = [pscustomobject]@{ schema = 1; created = (Get-Date).ToUniversalTime().ToString('o'); roots = $Roots; files = $source; registry = $registry; tasks = $tasks }
    [IO.File]::WriteAllText((Join-Path $Dest 'manifest.json'), ($manifest | ConvertTo-Json -Depth 6), $script:Utf8NoBom)
    [pscustomobject]@{ files = $source.Count; bytes = ($source | Measure-Object size -Sum).Sum; registry = $registry.Count; tasks = $tasks.Count }
}

function Compare-GoldenRegistry {
    param([Parameter(Mandatory)][string]$Backup, [Parameter(Mandatory)]$Manifest)
    $diffs = @()
    $now = Join-Path $Backup ('verify-' + (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmss'))
    New-Item -ItemType Directory -Path $now | Out-Null
    $saved = @($Manifest.registry | Where-Object { $_ })
    if ($saved.Count -gt 0) {
        $current = Export-GoldenRegistry -Keys @($saved | ForEach-Object { $_.key }) -Dest $now
        for ($i = 0; $i -lt $saved.Count; $i++) { if ($current[$i].sha256 -ne $saved[$i].sha256) { $diffs += $saved[$i].key } }
    }
    foreach ($t in @($Manifest.tasks | Where-Object { $_ })) {
        $xml = Export-ScheduledTask -TaskName $t.name
        if ($xml -ne [IO.File]::ReadAllText((Join-Path $Backup $t.file))) { $diffs += "task:$($t.name)" }
    }
    [pscustomobject]@{ identical = ($diffs.Count -eq 0); differences = $diffs }
}

function Invoke-GoldenVerify {
    param([Parameter(Mandatory)][string]$Backup, [switch]$Restore)
    $m = Get-Content -LiteralPath (Join-Path $Backup 'manifest.json') -Raw | ConvertFrom-Json
    $roots = @{}; foreach ($p in $m.roots.PSObject.Properties) { $roots[$p.Name] = [string]$p.Value }
    $diff = Compare-GoldenManifest -Before @($m.files) -After (Get-GoldenManifest -Roots $roots)
    $restored = @(); $quarantined = @()
    if ($Restore -and -not $diff.identical) {
        foreach ($k in @($diff.changed + $diff.missing)) {
            $root, $rel = $k.Split([char[]]'|', 2)
            $dst = Join-Path $roots[$root] $rel
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dst) | Out-Null
            Copy-Item -LiteralPath (Join-Path (Join-Path $Backup "trees\$root") $rel) -Destination $dst -Force
            $restored += $k
        }
        foreach ($k in $diff.extra) {
            $root, $rel = $k.Split([char[]]'|', 2)
            $dst = Join-Path (Join-Path $Backup "quarantine\$root") $rel
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dst) | Out-Null
            Move-Item -LiteralPath (Join-Path $roots[$root] $rel) -Destination $dst
            $quarantined += $k
        }
        $diff = Compare-GoldenManifest -Before @($m.files) -After (Get-GoldenManifest -Roots $roots)
    }
    $reg = Compare-GoldenRegistry -Backup $Backup -Manifest $m
    [pscustomobject]@{ identical = ($diff.identical -and $reg.identical); files = $diff; registry = $reg; restored = $restored; quarantined = $quarantined }
}

function Test-GoldenRpp {
    param([Parameter(Mandatory)][string]$Text)
    foreach ($line in ($Text -split "`r?`n")) {
        $t = $line.Trim()
        if ($t -match '^<(VST3?|JS|CLAP|AUi?|DX|LV2|VIDEO_EFFECT)\b' -and $script:AllowedFxHeads -notcontains $t.Substring(1)) { throw "plug-in not on the allowlist: $t" }
        if ($t.StartsWith('FILE ') -and $t -notmatch '^FILE "@@JOB@@\\stimuli\\[A-Za-z0-9._-]+\.wav"$') { throw "media path outside the job: $t" }
        if ($t.StartsWith('RENDER_FILE ') -and $t -notmatch '^RENDER_FILE "@@OUT@@\\[A-Za-z0-9._-]+"$') { throw "render target outside the job: $t" }
    }
}

function Invoke-GoldenStage {
    param([Parameter(Mandatory)][string]$Bundle, [Parameter(Mandatory)][string]$Job)
    $m = Get-Content -LiteralPath (Join-Path $Bundle 'bundle.json') -Raw | ConvertFrom-Json
    $listed = @{}
    foreach ($f in $m.files) {
        $p = Join-Path $Bundle ($f.path -replace '/', '\')
        if (-not (Test-Path -LiteralPath $p -PathType Leaf)) { throw "bundle file missing: $($f.path)" }
        if ((Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant() -ne $f.sha256) { throw "bundle file hash mismatch: $($f.path)" }
        $listed[[IO.Path]::GetFullPath($p)] = $true
    }
    foreach ($f in (Get-ChildItem -LiteralPath $Bundle -Recurse -File)) {
        if ($f.Name -ne 'bundle.json' -and -not $listed.ContainsKey($f.FullName)) { throw "unlisted bundle file: $($f.FullName)" }
    }
    if (Test-Path -LiteralPath $Job) { throw "job folder exists: $Job" }
    foreach ($d in @('projects', 'stimuli', 'out')) { New-Item -ItemType Directory -Force -Path (Join-Path $Job $d) | Out-Null }
    Copy-Item -Path (Join-Path $Bundle 'stimuli\*') -Destination (Join-Path $Job 'stimuli')
    $staged = @()
    foreach ($proj in $m.projects) {
        $text = [IO.File]::ReadAllText((Join-Path $Bundle ($proj.file -replace '/', '\')))
        Test-GoldenRpp -Text $text
        $text = $text.Replace('@@JOB@@', $Job).Replace('@@OUT@@', (Join-Path $Job 'out'))
        if ($text.Contains('@@')) { throw "unresolved token in $($proj.file)" }
        $dst = Join-Path $Job ("projects\" + $proj.id + '.rpp')
        [IO.File]::WriteAllText($dst, $text, $script:Utf8NoBom)
        New-Item -ItemType Directory -Force -Path (Join-Path $Job ("out\" + $proj.id)) | Out-Null
        $staged += $dst
    }
    return ,$staged
}

function New-GoldenResourceDir {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$MainResource,
          [Parameter(Mandatory)][int]$DummyMode, [int]$Rate = 96000)
    if ($DummyMode -eq 3) { throw 'audio mode 3 is ASIO: refused' }
    if (Test-Path -LiteralPath $Path) { throw "resource folder exists: $Path" }
    New-Item -ItemType Directory -Path $Path | Out-Null
    $ini = @('[REAPER]', 'loadlastproj=0', 'autosave=0', 'verchk=0', '', '[audioconfig]', "mode=$DummyMode", "dummy_srate=$Rate", 'dummy_blocksize=64', 'allow_sr_override=1', '')
    [IO.File]::WriteAllText((Join-Path $Path 'reaper.ini'), ($ini -join "`r`n"), [Text.Encoding]::ASCII)
    foreach ($rel in @('Effects\utility\volume_pan', 'Effects\loser\MGA_JSLimiterST')) {
        $dst = Join-Path $Path $rel
        New-Item -ItemType Directory -Force -Path (Split-Path -Parent $dst) | Out-Null
        Copy-Item -LiteralPath (Join-Path $MainResource $rel) -Destination $dst
    }
    foreach ($name in @('reaper-license.rk', 'reaper-reginfo2.ini')) {
        $src = Join-Path $MainResource $name
        if (Test-Path -LiteralPath $src) { Copy-Item -LiteralPath $src -Destination (Join-Path $Path $name) }
    }
    return (Join-Path $Path 'reaper.ini')
}

function Get-GoldenAsioHolders {
    param([Parameter(Mandatory)][string]$Module)
    $out = & tasklist.exe /m $Module /fo csv /nh
    return ,@($out | Where-Object { $_ -like '"*' } | ForEach-Object { $c = $_.Trim('"') -split '","'; "$($c[0]):$($c[1])" })
}

function Write-GoldenStatus {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$State, [AllowEmptyCollection()][object[]]$Results = @())
    $tmp = "$Path.tmp"
    $json = [pscustomobject]@{ state = $State; at = (Get-Date).ToUniversalTime().ToString('o'); results = @($Results) } | ConvertTo-Json -Depth 6
    [IO.File]::WriteAllText($tmp, $json, $script:Utf8NoBom)
    Move-Item -LiteralPath $tmp -Destination $Path -Force
}

function Write-GoldenRequest {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Kind, [hashtable]$Fields = @{})
    $id = '{0}-{1}' -f $Kind, (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssfff')
    $req = @{ id = $id; kind = $Kind }
    foreach ($k in $Fields.Keys) { $req[$k] = $Fields[$k] }
    [IO.File]::WriteAllText((Join-Path $Root 'queue\request.json'), ($req | ConvertTo-Json -Depth 5), $script:Utf8NoBom)
    return $id
}

function Invoke-GoldenRenderQueue {
    param([Parameter(Mandatory)][string]$Reaper, [Parameter(Mandatory)][string]$Ini, [Parameter(Mandatory)][string[]]$Projects,
          [Parameter(Mandatory)][string]$StatusPath, [Parameter(Mandatory)][string]$StopFile, [int]$TimeoutSec = 600)
    $results = @()
    foreach ($p in $Projects) {
        if (Test-Path -LiteralPath $StopFile) { $results += [pscustomobject]@{ project = $p; state = 'skipped-stop' }; continue }
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $proc = Start-Process -FilePath $Reaper -PassThru -ArgumentList @('-newinst', '-nosplash', '-ignoreerrors', '-cfgfile', "`"$Ini`"", '-renderproject', "`"$p`"")
        $done = $proc.WaitForExit($TimeoutSec * 1000)
        $results += [pscustomobject]@{ project = $p; state = $(if ($done) { 'exited' } else { 'hung' }); exit = $(if ($done) { $proc.ExitCode } else { $null }); pid = $proc.Id; seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1) }
        Write-GoldenStatus -Path $StatusPath -State 'running' -Results $results
        if (-not $done) { break }   # never end it by force: the operator closes the dialog
    }
    $final = if (@($results | Where-Object { $_.state -eq 'hung' }).Count -gt 0) { 'hung' } else { 'done' }
    Write-GoldenStatus -Path $StatusPath -State $final -Results $results
}

function Watch-GoldenRender {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$RequestId, [Parameter(Mandatory)][string]$AsioModule, [Parameter(Mandatory)][int]$TimeoutSec)
    $status = Join-Path $Root "status\$RequestId.json"; $stop = Join-Path $Root 'queue\stop'
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ($true) {
        $holders = @(Get-GoldenAsioHolders -Module $AsioModule)
        if ($holders.Count -gt 0) { New-Item -ItemType File -Force -Path $stop | Out-Null; return [pscustomobject]@{ outcome = 'asio-alarm'; holders = $holders } }
        if (Test-Path -LiteralPath $status) {
            try { $s = Get-Content -LiteralPath $status -Raw | ConvertFrom-Json } catch { $s = $null }
            if ($s -and @('done', 'hung', 'failed') -contains $s.state) { return [pscustomobject]@{ outcome = $s.state; status = $s } }
        }
        if ((Get-Date) -gt $deadline) { New-Item -ItemType File -Force -Path $stop | Out-Null; return [pscustomobject]@{ outcome = 'timeout' } }
        Start-Sleep -Milliseconds 500
    }
}

function Request-GoldenCloseRender {
    param([Parameter(Mandatory)][string]$IniPath)
    $procs = @(Get-CimInstance Win32_Process -Filter "Name = 'reaper.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($IniPath) })
    foreach ($p in $procs) { $gp = Get-Process -Id $p.ProcessId -ErrorAction SilentlyContinue; if ($gp) { [void]$gp.CloseMainWindow() } }
    return $procs.Count
}

function Register-GoldenTask {
    param([Parameter(Mandatory)][string]$Root, [string]$Name = 'iemmixer-golden')
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + (Join-Path $Root 'bin\golden-task.ps1') + '"')
    $principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName $Name -Action $action -Principal $principal -Settings $settings -Force | Out-Null
}

function Test-GoldenHttp {
    param([Parameter(Mandatory)][string]$Uri)
    try { return [int](Invoke-WebRequest -UseBasicParsing -Uri $Uri -TimeoutSec 5 -MaximumRedirection 0).StatusCode }
    catch [System.Net.WebException] { if ($_.Exception.Response) { return [int]$_.Exception.Response.StatusCode } else { return 0 } }
}

function Wait-GoldenProcessGone {
    param([Parameter(Mandatory)][string]$Name, [int]$Seconds = 30)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while (@(Get-Process -Name $Name -ErrorAction SilentlyContinue).Count -gt 0) {
        if ((Get-Date) -gt $deadline) { throw "$Name is still running after $Seconds s (nothing is ended by force)" }
        Start-Sleep -Milliseconds 500
    }
}

function Invoke-GoldenSaveQuit {
    param([Parameter(Mandatory)][string]$Http, [Parameter(Mandatory)][string]$Project, [Parameter(Mandatory)][string]$AsioModule)
    $before = (Get-Item -LiteralPath $Project).LastWriteTimeUtc
    Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/40026" -TimeoutSec 10 | Out-Null
    $deadline = (Get-Date).AddSeconds(15)
    while ((Get-Item -LiteralPath $Project).LastWriteTimeUtc -eq $before) {
        if ((Get-Date) -gt $deadline) { throw 'REAPER did not save within 15 s; nothing else was done' }
        Start-Sleep -Milliseconds 250
    }
    try { Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/40004" -TimeoutSec 5 | Out-Null } catch { }   # REAPER may drop the connection while quitting
    Wait-GoldenProcessGone -Name 'reaper' -Seconds 30
    $holders = @(Get-GoldenAsioHolders -Module $AsioModule)
    if ($holders.Count -gt 0) { throw "the ASIO module is still held: $($holders -join ', ')" }
    [pscustomobject]@{ saved = $true; quit = $true }
}

function Invoke-GoldenBringBack {
    param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$StartTask, [Parameter(Mandatory)][string]$Http,
          [Parameter(Mandatory)][string]$AppExe, [Parameter(Mandatory)][string]$AppProcess, [Parameter(Mandatory)][string]$AppHttp,
          [bool]$WantReaper = $true, [bool]$WantApp = $true)
    $render = @(Get-CimInstance Win32_Process -Filter "Name = 'reaper.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($Root) })
    if ($render.Count -gt 0) { throw 'a render instance still runs: close it (MCP or close-render) before REAPER may start' }
    $reaperUp = $false; $appUp = $false
    if ($WantReaper) {
        if (@(Get-Process reaper -ErrorAction SilentlyContinue).Count -eq 0) { Start-ScheduledTask -TaskName $StartTask }
        $deadline = (Get-Date).AddSeconds(90)
        while (-not $reaperUp -and (Get-Date) -lt $deadline) { $reaperUp = ((Test-GoldenHttp -Uri "$Http/_/NTRACK") -eq 200); if (-not $reaperUp) { Start-Sleep -Seconds 1 } }
        if (-not $reaperUp) { throw 'REAPER did not answer within 90 s after its start task' }
    }
    if ($WantApp) {
        if (@(Get-Process -Name $AppProcess -ErrorAction SilentlyContinue).Count -eq 0) {
            [void](Write-GoldenRequest -Root $Root -Kind 'start-app' -Fields @{ exe = $AppExe })
            Start-ScheduledTask -TaskName 'iemmixer-golden'
        }
        $deadline = (Get-Date).AddSeconds(60)
        while (-not $appUp -and (Get-Date) -lt $deadline) { $code = Test-GoldenHttp -Uri $AppHttp; $appUp = ($code -gt 0 -and $code -lt 500); if (-not $appUp) { Start-Sleep -Seconds 1 } }
        if (-not $appUp) { throw 'the predecessor app did not answer within 60 s' }
    }
    [pscustomobject]@{ reaper = $reaperUp; app = $appUp }
}

function Get-GoldenMeterSamples {
    param([Parameter(Mandatory)][string]$Http, [int]$Seconds = 60)
    $samples = @()
    for ($i = 0; $i -lt $Seconds; $i++) {
        $samples += (Invoke-WebRequest -UseBasicParsing -Uri "$Http/_/NTRACK;TRACK" -TimeoutSec 5).Content
        Start-Sleep -Seconds 1
    }
    return ,$samples
}

function Get-GoldenFileHashes {
    param([Parameter(Mandatory)][string]$Path)
    $root = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    return ,@(Get-ChildItem -LiteralPath $root -Recurse -File | Sort-Object FullName | ForEach-Object {
        [pscustomobject]@{ rel = $_.FullName.Substring($root.Length + 1).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); size = $_.Length }
    })
}

Export-ModuleMember -Function *-Golden*
```

- [ ] **Step 3: `scripts/golden/golden-task.ps1`**

This is the Interactive task's entry point. It runs in the console session, where REAPER's dialogs are visible.

```powershell
#Requires -Version 5.1
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
Import-Module (Join-Path $PSScriptRoot 'GoldenPc.psm1') -Force
$root = Split-Path -Parent $PSScriptRoot
$req = Get-Content -LiteralPath (Join-Path $root 'queue\request.json') -Raw | ConvertFrom-Json
$status = Join-Path $root ("status\" + $req.id + '.json')
try {
    switch ($req.kind) {
        'render' {
            Invoke-GoldenRenderQueue -Reaper $req.reaper -Ini $req.ini -Projects @($req.projects) -StatusPath $status -StopFile (Join-Path $root 'queue\stop') -TimeoutSec ([int]$req.timeout)
        }
        'audiocfg' {
            Start-Process -FilePath $req.reaper -ArgumentList @('-newinst', '-nosplash', '-audiocfg', '-cfgfile', "`"$($req.ini)`"") | Out-Null
            Write-GoldenStatus -Path $status -State 'started'
        }
        'close-render' {
            $n = Request-GoldenCloseRender -IniPath $req.ini
            Write-GoldenStatus -Path $status -State 'done' -Results @([pscustomobject]@{ closed = $n })
        }
        'start-app' {
            Start-Process -FilePath $req.exe -WorkingDirectory (Split-Path -Parent $req.exe) | Out-Null
            Write-GoldenStatus -Path $status -State 'started'
        }
        default { throw "unknown request kind: $($req.kind)" }
    }
} catch {
    Write-GoldenStatus -Path $status -State 'failed' -Results @([pscustomobject]@{ error = "$_" })
    exit 1
}
```

- [ ] **Step 4: `scripts/golden/Test-GoldenPc.ps1` (runs on the CI `windows` job)**

```powershell
#Requires -Version 5.1
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
foreach ($f in (Get-ChildItem -LiteralPath $here -Include '*.ps1', '*.psm1' -Recurse)) {
    $tokens = $null; $errors = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    if ($errors.Count -gt 0) { throw "parse errors in $($f.Name): $($errors[0].Message)" }
}
Import-Module (Join-Path $here 'GoldenPc.psm1') -Force
function Assert($cond, $what) { if (-not $cond) { throw "FAILED: $what" } ; Write-Host "ok  $what" }
function Throws([scriptblock]$b, $what) { $t = $false; try { & $b } catch { $t = $true }; Assert $t $what }

$base = Join-Path ([IO.Path]::GetTempPath()) ('golden-test-' + [guid]::NewGuid())
$a = Join-Path $base 'root a'; $b = Join-Path $base 'root-b'
New-Item -ItemType Directory -Force -Path (Join-Path $a 'sub dir'), $b | Out-Null
Set-Content -LiteralPath (Join-Path $a 'x.ini') -Value 'one'
Set-Content -LiteralPath (Join-Path $a 'sub dir\two words.txt') -Value 'two'
Set-Content -LiteralPath (Join-Path $b 'y.json') -Value '{}'
$roots = @{ 'a' = $a; 'b' = $b }
$backup = Join-Path $base 'backup'

$r = Invoke-GoldenBackup -Roots $roots -Dest $backup
Assert ($r.files -eq 3) 'backup-copies-every-file'
Assert ((Invoke-GoldenVerify -Backup $backup).identical) 'verify-identical-after-backup'

Set-Content -LiteralPath (Join-Path $a 'x.ini') -Value 'changed'
Remove-Item -LiteralPath (Join-Path $b 'y.json')
Set-Content -LiteralPath (Join-Path $a 'new.tmp') -Value 'extra'
$v = Invoke-GoldenVerify -Backup $backup
Assert (-not $v.identical) 'verify-detects-a-difference'
Assert ($v.files.changed.Count -eq 1 -and $v.files.missing.Count -eq 1 -and $v.files.extra.Count -eq 1) 'verify-detects-changed-missing-extra'
$v = Invoke-GoldenVerify -Backup $backup -Restore
Assert $v.identical 'restore-makes-identical'
Assert ($v.quarantined.Count -eq 1 -and (Test-Path -LiteralPath (Join-Path $backup 'quarantine\a\new.tmp'))) 'restore-quarantines-never-deletes'
Assert ((Get-Content -LiteralPath (Join-Path $a 'x.ini')) -eq 'one') 'restore-brings-back-content'
Throws { Invoke-GoldenBackup -Roots $roots -Dest $backup } 'backup-refuses-an-existing-destination'

$rpp = "<REAPER_PROJECT 0.1 `"7.65/win64`" 0 0`n  RENDER_FILE `"@@OUT@@\p`"`n  <JS utility/volume_pan `"`"`n  >`n  FILE `"@@JOB@@\stimuli\imp-dm-96000.wav`"`n>`n"
Test-GoldenRpp -Text $rpp; Assert $true 'rpp-allowlisted-passes'
Throws { Test-GoldenRpp -Text ($rpp.Replace('<JS utility/volume_pan ""', '<VST "VST3: Other" o.vst3 0 "" 1 ""')) } 'rpp-foreign-plugin-fails'
Throws { Test-GoldenRpp -Text ($rpp.Replace('@@JOB@@\stimuli\imp-dm-96000.wav', 'C:\Windows\x.wav')) } 'rpp-outside-media-fails'
Throws { Test-GoldenRpp -Text ($rpp.Replace('@@OUT@@\p', 'D:\elsewhere')) } 'rpp-outside-render-fails'

$bundle = Join-Path $base 'bundle'
New-Item -ItemType Directory -Force -Path (Join-Path $bundle 'projects'), (Join-Path $bundle 'stimuli') | Out-Null
[IO.File]::WriteAllText((Join-Path $bundle 'projects\p.rpp'), $rpp)
[IO.File]::WriteAllText((Join-Path $bundle 'stimuli\imp-dm-96000.wav'), 'RIFF-test')
$files = @(foreach ($rel in @('projects/p.rpp', 'stimuli/imp-dm-96000.wav')) { @{ path = $rel; sha256 = (Get-FileHash -LiteralPath (Join-Path $bundle ($rel -replace '/', '\')) -Algorithm SHA256).Hash.ToLowerInvariant() } })
[IO.File]::WriteAllText((Join-Path $bundle 'bundle.json'), (@{ schema = 1; projects = @(@{ file = 'projects/p.rpp'; id = 'p' }); files = $files } | ConvertTo-Json -Depth 5))
$job = Join-Path $base 'job one'
$staged = Invoke-GoldenStage -Bundle $bundle -Job $job
$text = [IO.File]::ReadAllText($staged[0])
Assert (-not $text.Contains('@@') -and $text.Contains("$job\stimuli\imp-dm-96000.wav") -and (Test-Path -LiteralPath (Join-Path $job 'out\p'))) 'stage-substitutes-tokens'
[IO.File]::WriteAllText((Join-Path $bundle 'stimuli\imp-dm-96000.wav'), 'RIFF-evil')
Throws { Invoke-GoldenStage -Bundle $bundle -Job (Join-Path $base 'job2') } 'stage-rejects-a-tampered-file'

$main = Join-Path $base 'main-resource'
New-Item -ItemType Directory -Force -Path (Join-Path $main 'Effects\utility'), (Join-Path $main 'Effects\loser') | Out-Null
Set-Content -LiteralPath (Join-Path $main 'Effects\utility\volume_pan') -Value 'desc:x'
Set-Content -LiteralPath (Join-Path $main 'Effects\loser\MGA_JSLimiterST') -Value 'desc:y'
Throws { New-GoldenResourceDir -Path (Join-Path $base 'res3') -MainResource $main -DummyMode 3 } 'resource-dir-refuses-asio'
$ini = New-GoldenResourceDir -Path (Join-Path $base 'res') -MainResource $main -DummyMode 4
$iniText = Get-Content -LiteralPath $ini -Raw
Assert ($iniText.Contains('mode=4') -and -not ($iniText -match '(?i)asio') -and (Test-Path -LiteralPath (Join-Path $base 'res\Effects\loser\MGA_JSLimiterST'))) 'resource-dir-is-minimal-and-asio-free'

$st = Join-Path $base 's.json'
Write-GoldenStatus -Path $st -State 'done' -Results @([pscustomobject]@{ n = 1 })
Assert ((Get-Content -LiteralPath $st -Raw | ConvertFrom-Json).state -eq 'done') 'status-is-written-atomically'
Assert (@(Get-GoldenAsioHolders -Module 'no-such-module-xyz.dll').Count -eq 0) 'asio-holders-empty-for-an-unloaded-module'

Remove-Item -LiteralPath $base -Recurse -Force
Write-Host 'Test-GoldenPc: all passed'
```

`Register-GoldenTask`, the render queue and the REAPER/app functions are exercised on the PC in Task 13. They need the PC's task scheduler, REAPER and the app, and those never run in public CI.

- [ ] **Step 5: CI `windows` job step**

Add as the first step after checkout:
```yaml
      - name: Golden PC module self-test (Windows PowerShell 5.1, as on the PC)
        shell: powershell
        run: powershell -NoProfile -ExecutionPolicy Bypass -File scripts/golden/Test-GoldenPc.ps1
```

- [ ] **Step 6: Commit**

```bash
python3 scripts/check_integrity.py && python3 -m unittest discover -s scripts -p 'test_*.py' -v
git add scripts .github/workflows/ci.yml
git commit -m "feat(golden): PC module for backup, verify/restore, staging and the render queue" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Window driver, private env, MCP access

**Files:**
- Create: `scripts/golden/golden_window.py`, `scripts/golden/test_golden_window.py`
- Create (private): `$PRIV/golden.env`, `$PRIV/golden-trees.json`; copy the runbook to `$OPS/docs/s1b-pc-runbook.md`

**Interfaces:**
- Produces: `python3 scripts/golden/golden_window.py <subcommand>`, with exit 0 on success and 1 on `StepError`. The subcommands, in order:
  - `new --signal TEXT`
  - `setup`
  - `preflight`
  - `interlock`
  - `save-quit`
  - `app-stopped` (after the MCP tray Exit)
  - `backup`
  - `stage --bundle DIR --sha SHA`
  - `seed-res`
  - `render [--only ids]`
  - `fetch`
  - `verify-restore`
  - `bring-back`
- Plus the out-of-order subcommands: `audiocfg`, `read-mode`, `close-render`, `preempt` and `status`.
- State: `~/.local/state/iemmixer/golden-window.json`, holding `{id, signal, pre, started, done, projects}`.

- [ ] **Step 1: RED — tests for the pure parts**

`scripts/golden/test_golden_window.py`:
```python
"""Tests for scripts/golden/golden_window.py (pure parts; ssh is the PC)."""
from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import golden_window as gw  # noqa: E402

FULL = "\n".join(f"{k}=v" for k in gw.REQUIRED if k != "PC_DUMMY_MODE") + "\nPC_DUMMY_MODE=4\n"


class EnvTests(unittest.TestCase):
    def write(self, text: str) -> Path:
        d = tempfile.mkdtemp()
        p = Path(d) / "golden.env"
        p.write_text(text, encoding="utf-8")
        return p

    def test_complete_env_loads_and_quotes_are_stripped(self) -> None:
        env = gw.load_env(self.write(FULL.replace("PC_SSH=v", 'PC_SSH="u@h"') + "# comment\n"))
        self.assertEqual(env["PC_SSH"], "u@h")

    def test_missing_keys_are_named(self) -> None:
        with self.assertRaisesRegex(gw.StepError, "missing PC_SSH"):
            gw.load_env(self.write(FULL.replace("PC_SSH=v\n", "")))

    def test_dummy_mode_three_is_refused(self) -> None:
        with self.assertRaisesRegex(gw.StepError, "ASIO"):
            gw.load_env(self.write(FULL.replace("PC_DUMMY_MODE=4", "PC_DUMMY_MODE=3")))


class OrderTests(unittest.TestCase):
    def test_steps_run_in_order(self) -> None:
        state = {"done": ["preflight"]}
        gw.check_order(state, "interlock")
        with self.assertRaisesRegex(gw.StepError, "next step is 'interlock'"):
            gw.check_order(state, "backup")

    def test_signal_must_quote_the_owner(self) -> None:
        gw.check_signal("owner 2026-09-27 21:05: event skončil")
        with self.assertRaises(gw.StepError):
            gw.check_signal("I think the event is over")


class UndoPlanTests(unittest.TestCase):
    def test_undo_plan_before_anything_changed_is_empty(self) -> None:
        self.assertEqual(gw.undo_plan(["preflight", "interlock"], ["preflight", "interlock"], render_running=False), [])

    def test_undo_plan_mid_render_stops_restores_and_brings_back(self) -> None:
        done = ["preflight", "interlock", "save-quit", "app-stopped", "backup", "stage", "seed-res"]
        self.assertEqual(gw.undo_plan(done, done + ["render"], render_running=True), ["stop-render", "verify-restore", "bring-back"])

    def test_undo_plan_after_a_failed_quit_still_brings_back(self) -> None:
        self.assertEqual(gw.undo_plan(["preflight", "interlock"], ["preflight", "interlock", "save-quit"], render_running=False), ["bring-back"])

    def test_undo_plan_after_restore_only_brings_back(self) -> None:
        done = list(gw.STEPS[:-1])
        self.assertEqual(gw.undo_plan(done, done, render_running=False), ["bring-back"])


class ParseTests(unittest.TestCase):
    def test_holders_parse_tasklist_csv(self) -> None:
        self.assertEqual(gw.parse_holders('"reaper.exe","6496","x.dll"\r\n'), [("reaper.exe", 6496)])
        self.assertEqual(gw.parse_holders("INFO: No tasks are running which match the specified criteria.\r\n"), [])

    def test_meter_peaks_skip_the_master(self) -> None:
        text = "NTRACK\t2\nTRACK\t0\tMASTER\t0\t1\t0\t-100\t-100\nTRACK\t1\tin\t0\t1\t0\t-620\t-620\nTRACK\t2\tbus\t0\t1\t0\t-1500\t-1500\n"
        self.assertEqual(gw.parse_meter_peaks(text), {1: -620, 2: -1500})

    def test_interlock_verdict(self) -> None:
        self.assertEqual(gw.interlock_hits([{1: -620, 2: -1500}, {1: -480}]), {1: -480})
        self.assertEqual(gw.interlock_hits([{1: -620}]), {})


if __name__ == "__main__":
    unittest.main()
```
Run it: RED (no module).

- [ ] **Step 2: GREEN — `scripts/golden/golden_window.py`**

```python
#!/usr/bin/env python3
"""S1b render-window driver on the dev box (design note §4). One subcommand
per step, in order; PC-side work runs in GoldenPc.psm1 over ssh. Site values
come only from the private env file ($GOLDEN_ENV). Never ends a process by
force; a window starts only after the owner's "event skončil"."""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import subprocess
import sys
from pathlib import Path

REQUIRED = (
    "PC_SSH", "PC_ROOT", "PC_ROOT_SCP", "PC_TREES", "PC_REGISTRY_KEYS", "PC_TASKS",
    "PC_ASIO_MODULE", "PC_REAPER_EXE", "PC_REAPER_HTTP", "PC_REAPER_START_TASK",
    "PC_MAIN_PROJECT", "PC_APP_EXE", "PC_APP_PROCESS", "PC_APP_HTTP", "PC_DUMMY_MODE", "RAW_DIR",
)
STEPS = ("preflight", "interlock", "save-quit", "app-stopped", "backup", "stage", "seed-res", "render", "fetch", "verify-restore", "bring-back")
CHANGING = STEPS[2:]
TASK = "iemmixer-golden"
INTERLOCK_DB10 = -500
STATE = Path(os.environ.get("GOLDEN_STATE", str(Path.home() / ".local/state/iemmixer/golden-window.json")))


class StepError(Exception):
    """A step failed; the message says what to do next."""


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
    if env["PC_DUMMY_MODE"] == "3":
        raise StepError("PC_DUMMY_MODE=3 is ASIO: refused")
    return env


def ps_quote(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def check_signal(text: str) -> None:
    if "event skončil" not in text.lower():
        raise StepError("--signal must quote the owner's 'event skončil' message (with its time)")


def check_order(state: dict, step: str) -> None:
    done = state["done"]
    expected = STEPS[len(done)] if len(done) < len(STEPS) else None
    if step != expected:
        raise StepError(f"step '{step}' is out of order; the next step is '{expected}'")


def undo_plan(done: list[str], started: list[str], render_running: bool) -> list[str]:
    plan: list[str] = []
    if render_running:
        plan.append("stop-render")
    if "backup" in done and "verify-restore" not in done:
        plan.append("verify-restore")
    if any(s in started for s in CHANGING) and "bring-back" not in done:
        plan.append("bring-back")
    return plan


def parse_holders(text: str) -> list[tuple[str, int]]:
    out = []
    for line in text.splitlines():
        line = line.strip()
        if line.startswith('"'):
            cells = [c.strip('"') for c in line.split('","')]
            out.append((cells[0], int(cells[1])))
    return out


def parse_meter_peaks(text: str) -> dict[int, int]:
    peaks: dict[int, int] = {}
    for line in text.splitlines():
        f = line.split("\t")
        if len(f) > 6 and f[0] == "TRACK" and f[1].isdigit() and f[1] != "0":
            peaks[int(f[1])] = int(f[6])
    return peaks


def interlock_hits(samples: list[dict[int, int]]) -> dict[int, int]:
    worst: dict[int, int] = {}
    for s in samples:
        for idx, db10 in s.items():
            worst[idx] = max(worst.get(idx, -10_000), db10)
    return {i: v for i, v in worst.items() if v > INTERLOCK_DB10}


# ---- ssh / scp (the PC is the external dependency; no unit tests below) ----

def ssh_raw(env: dict[str, str], script: str, timeout: int = 900) -> str:
    proc = subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"],
         "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -"],
        input=script + "\n", text=True, capture_output=True, timeout=timeout, check=False)
    if proc.returncode != 0:
        raise StepError(f"PC command failed (exit {proc.returncode}): {proc.stderr.strip()[-1500:]}")
    return proc.stdout


def ps(env: dict[str, str], body: str, timeout: int = 900):
    """Runs `body` (single-line statements, `-Command -` reads stdin line by
    line) after importing the module. Errors are caught on the PC and come
    back as {ok: false}; -InputObject keeps one-element arrays as arrays."""
    script = "\n".join([
        "$ErrorActionPreference = 'Stop'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"try {{ Import-Module (Join-Path {ps_quote(env['PC_ROOT'])} 'bin\\GoldenPc.psm1') -Force ; $r = & {{ {body} }} ; $o = [pscustomobject]@{{ ok = $true; r = $r }} }} "
        f"catch {{ $o = [pscustomobject]@{{ ok = $false; error = \"$_\" }} }} ; ConvertTo-Json -InputObject $o -Depth 8 -Compress",
    ])
    out = [line for line in ssh_raw(env, script, timeout).splitlines() if line.strip()]
    doc = json.loads(out[-1]) if out else {"ok": False, "error": "no output from the PC"}
    if not doc["ok"]:
        raise StepError(f"PC step failed: {doc['error']}")
    return doc["r"]


def scp(env: dict[str, str], src: str, dst: str) -> None:
    proc = subprocess.run(["scp", "-q", "-r", "-o", "BatchMode=yes", src, dst], capture_output=True, text=True, check=False, timeout=3600)
    if proc.returncode != 0:
        raise StepError(f"scp failed: {proc.stderr.strip()[-800:]}")


def remote(env: dict[str, str], rel: str) -> str:
    return f"{env['PC_SSH']}:{env['PC_ROOT_SCP']}/{rel}"


def pc(env: dict[str, str], rel: str) -> str:
    return ps_quote(env["PC_ROOT"] + "\\" + rel.replace("/", "\\"))


# ---- state ----

def load_state() -> dict:
    if not STATE.is_file():
        raise StepError("no open window: run 'new --signal ...' first")
    return json.loads(STATE.read_text(encoding="utf-8"))


def save_state(state: dict) -> None:
    STATE.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=1), encoding="utf-8")
    tmp.replace(STATE)


def raw_dir(env: dict[str, str], state: dict) -> Path:
    d = Path(env["RAW_DIR"]).expanduser() / state["id"]
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    return d


# ---- steps ----

def step_preflight(env, state, args):
    r = ps(env, " ; ".join([
        f"$v = (Get-Item -LiteralPath {ps_quote(env['PC_REAPER_EXE'])}).VersionInfo.FileVersion",
        f"$h = Get-GoldenAsioHolders -Module {ps_quote(env['PC_ASIO_MODULE'])}",
        f"$ri = @(Get-CimInstance Win32_Process -Filter \"Name = 'reaper.exe'\" | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains({ps_quote(env['PC_ROOT'])}) }}).Count",
        f"[pscustomobject]@{{ version = $v; reaper = @(Get-Process reaper -EA SilentlyContinue).Count; app = @(Get-Process {ps_quote(env['PC_APP_PROCESS'])} -EA SilentlyContinue).Count; holders = @($h); render_instances = $ri; free_gb = [math]::Floor((Get-PSDrive C).Free / 1GB); task = [bool](Get-ScheduledTask -TaskName {TASK} -EA SilentlyContinue) }}",
    ]))
    problems = []
    if not str(r["version"]).startswith("7.65"):
        problems.append(f"REAPER version {r['version']} (expected 7.65)")
    if r["render_instances"]:
        problems.append("a render instance runs")
    if any(not h.lower().startswith("reaper.exe:") for h in r["holders"] or []):
        problems.append(f"unexpected ASIO module holders {r['holders']}")
    if r["free_gb"] < 10:
        problems.append(f"only {r['free_gb']} GB free")
    if not r["task"]:
        problems.append(f"task {TASK} not registered (run setup)")
    if problems:
        raise StepError("; ".join(problems))
    state["pre"] = {"reaper": r["reaper"] > 0, "app": r["app"] > 0}
    return r


def step_interlock(env, state, args):
    if not state["pre"]["reaper"]:
        return {"skipped": "REAPER was not running before the window"}
    texts = ps(env, f"Get-GoldenMeterSamples -Http {ps_quote(env['PC_REAPER_HTTP'])} -Seconds 60", timeout=180)
    hits = interlock_hits([parse_meter_peaks(t) for t in texts])
    if hits:
        raise StepError(f"band activity: peaks above -50 dBFS on tracks {sorted(hits)}; window aborted, alarm the owner")
    return {"samples": len(texts), "quiet": True}


def step_save_quit(env, state, args):
    if not state["pre"]["reaper"]:
        return {"skipped": "REAPER was not running"}
    return ps(env, f"Invoke-GoldenSaveQuit -Http {ps_quote(env['PC_REAPER_HTTP'])} -Project {ps_quote(env['PC_MAIN_PROJECT'])} -AsioModule {ps_quote(env['PC_ASIO_MODULE'])}")


def step_app_stopped(env, state, args):
    return ps(env, f"Wait-GoldenProcessGone -Name {ps_quote(env['PC_APP_PROCESS'])} -Seconds 30; [pscustomobject]@{{ app = 'gone' }}")


def step_backup(env, state, args):
    keys = ",".join(ps_quote(k) for k in env["PC_REGISTRY_KEYS"].split(";"))
    tasks = ",".join(ps_quote(t) for t in env["PC_TASKS"].split(";"))
    r = ps(env, f"$roots = Get-GoldenTrees -Path {pc(env, 'bin/trees.json')} ; Invoke-GoldenBackup -Roots $roots -Dest {pc(env, 'backups/' + state['id'])} -RegistryKeys @({keys}) -TaskNames @({tasks})", timeout=1800)
    scp(env, remote(env, f"backups/{state['id']}/manifest.json"), str(raw_dir(env, state) / "backup-manifest.json"))
    return r


def step_stage(env, state, args):
    bundle = Path(args.bundle)
    if (bundle / ".source-sha").read_text(encoding="utf-8").strip() != args.sha:
        raise StepError("bundle .source-sha differs from --sha (P5: only the reviewed dev commit's bundle)")
    check = subprocess.run([sys.executable, str(Path(__file__).with_name("check_bundle.py")), str(bundle)], capture_output=True, text=True, check=False)
    if check.returncode != 0:
        raise StepError(f"bundle check failed:\n{check.stdout}")
    ps(env, f"New-Item -ItemType Directory -Force -Path {pc(env, 'jobs/' + state['id'])} | Out-Null ; 'ok'")
    scp(env, str(bundle), remote(env, f"jobs/{state['id']}/bundle"))
    staged = ps(env, f"Invoke-GoldenStage -Bundle {pc(env, 'jobs/' + state['id'] + '/bundle')} -Job {pc(env, 'jobs/' + state['id'] + '/run')}", timeout=900)
    state["projects"] = staged
    (raw_dir(env, state) / "bundle.json").write_bytes((bundle / "bundle.json").read_bytes())
    return {"projects": len(staged), "sha": args.sha}


def step_seed_res(env, state, args):
    return ps(env, f"New-GoldenResourceDir -Path {pc(env, 'jobs/' + state['id'] + '/res')} -MainResource ([Environment]::ExpandEnvironmentVariables('%APPDATA%\\REAPER')) -DummyMode {int(env['PC_DUMMY_MODE'])} -Rate 96000")


def step_render(env, state, args):
    projects = state["projects"]
    if args.only:
        wanted = set(args.only.split(","))
        projects = [p for p in projects if Path(p.replace("\\", "/")).stem in wanted]
        if not projects:
            raise StepError(f"no staged project matches {sorted(wanted)}")
    ini = env["PC_ROOT"] + f"\\jobs\\{state['id']}\\res\\reaper.ini"
    listed = ",".join(ps_quote(p) for p in projects)
    rid = ps(env, f"Remove-Item -LiteralPath {pc(env, 'queue/stop')} -ErrorAction SilentlyContinue ; $id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'render' -Fields @{{ reaper = {ps_quote(env['PC_REAPER_EXE'])}; ini = {ps_quote(ini)}; projects = @({listed}); timeout = 600 }} ; Start-ScheduledTask -TaskName {TASK} ; $id")
    state.setdefault("requests", []).append(rid)
    save_state(state)
    r = ps(env, f"Watch-GoldenRender -Root {ps_quote(env['PC_ROOT'])} -RequestId {ps_quote(rid)} -AsioModule {ps_quote(env['PC_ASIO_MODULE'])} -TimeoutSec {600 * len(projects) + 60}", timeout=600 * len(projects) + 300)
    if r["outcome"] == "asio-alarm":
        raise StepError(f"ASIO module loaded by {r['holders']}: queue stopped. Next: preempt (restore + bring-back), alarm the owner, post on #4")
    if r["outcome"] != "done":
        raise StepError(f"render outcome {r['outcome']}: inspect the dialog via MCP, then close-render; never kill")
    return r


def step_fetch(env, state, args):
    dest = raw_dir(env, state) / "renders"
    if dest.exists():
        raise StepError(f"{dest} exists")
    scp(env, remote(env, f"jobs/{state['id']}/run/out"), str(dest))
    listing = ps(env, f"Get-GoldenFileHashes -Path {pc(env, 'jobs/' + state['id'] + '/run/out')}", timeout=1800)
    bad = [f["rel"] for f in listing if hashlib.sha256((dest / f["rel"]).read_bytes()).hexdigest() != f["sha256"]]
    if bad:
        raise StepError(f"fetched renders differ from the PC: {bad[:5]}")
    (dest.parent / "renders-sha256.json").write_text(json.dumps(listing, indent=1), encoding="utf-8")
    return {"files": len(listing), "bytes": sum(f["size"] for f in listing)}


def step_verify_restore(env, state, args):
    r = ps(env, f"Invoke-GoldenVerify -Backup {pc(env, 'backups/' + state['id'])} -Restore", timeout=1800)
    (raw_dir(env, state) / "verify.json").write_text(json.dumps(r, indent=1), encoding="utf-8")
    if not r["identical"]:
        raise StepError("NOT identical after restore: post verify.json facts on #4; no further window until explained")
    return {"identical": True, "restored": len(r["restored"]), "quarantined": len(r["quarantined"]), "touched": len(r["files"]["touched"])}


def step_bring_back(env, state, args):
    want_reaper = "$true" if state.get("pre", {}).get("reaper", True) else "$false"
    want_app = "$true" if state.get("pre", {}).get("app", True) else "$false"
    return ps(env, f"Invoke-GoldenBringBack -Root {ps_quote(env['PC_ROOT'])} -StartTask {ps_quote(env['PC_REAPER_START_TASK'])} -Http {ps_quote(env['PC_REAPER_HTTP'])} -AppExe {ps_quote(env['PC_APP_EXE'])} -AppProcess {ps_quote(env['PC_APP_PROCESS'])} -AppHttp {ps_quote(env['PC_APP_HTTP'])} -WantReaper {want_reaper} -WantApp {want_app}", timeout=300)


STEP_FUNCS = {
    "preflight": step_preflight, "interlock": step_interlock, "save-quit": step_save_quit, "app-stopped": step_app_stopped,
    "backup": step_backup, "stage": step_stage, "seed-res": step_seed_res, "render": step_render, "fetch": step_fetch,
    "verify-restore": step_verify_restore, "bring-back": step_bring_back,
}


def run_step(env, name: str, args) -> None:
    state = load_state()
    check_order(state, name)
    state.setdefault("started", []).append(name)
    save_state(state)
    result = STEP_FUNCS[name](env, state, args)
    state["done"].append(name)
    save_state(state)
    print(json.dumps({"step": name, "window": state["id"], "result": result}, ensure_ascii=False))


def cmd_new(env, args) -> None:
    check_signal(args.signal)
    if STATE.is_file():
        old = json.loads(STATE.read_text(encoding="utf-8"))
        if old.get("started") and "bring-back" not in old.get("done", []):
            raise StepError(f"window {old['id']} is still open: finish it or run preempt")
    wid = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    save_state({"id": wid, "signal": args.signal, "pre": {}, "started": [], "done": [], "projects": []})
    print(wid)


def cmd_setup(env, args) -> None:
    here = Path(__file__).resolve().parent
    ssh_raw(env, f"New-Item -ItemType Directory -Force -Path {', '.join(pc(env, d) for d in ('bin', 'queue', 'status', 'backups', 'jobs'))} | Out-Null")
    for f in ("GoldenPc.psm1", "golden-task.ps1"):
        scp(env, str(here / f), remote(env, f"bin/{f}"))
    scp(env, env["PC_TREES"], remote(env, "bin/trees.json"))
    ps(env, f"Register-GoldenTask -Root {ps_quote(env['PC_ROOT'])} -Name {TASK} ; [pscustomobject]@{{ task = 'registered' }}")
    print("setup: module uploaded, task registered")


def cmd_audiocfg(env, args) -> None:
    state = load_state()
    ini = ps(env, f"New-GoldenResourceDir -Path {pc(env, 'jobs/' + state['id'] + '/res-audiocfg')} -MainResource ([Environment]::ExpandEnvironmentVariables('%APPDATA%\\REAPER')) -DummyMode {int(env['PC_DUMMY_MODE'])} -Rate 96000")
    rid = ps(env, f"$id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'audiocfg' -Fields @{{ reaper = {ps_quote(env['PC_REAPER_EXE'])}; ini = {ps_quote(ini)} }} ; Start-ScheduledTask -TaskName {TASK} ; $id")
    print(json.dumps({"audiocfg": rid, "ini": ini, "next": "MCP: read/select 'Dummy Audio' at 96000 Hz, OK, File > Quit; then read-mode"}))


def cmd_read_mode(env, args) -> None:
    state = load_state()
    r = ps(env, f"$t = Get-Content -LiteralPath {pc(env, 'jobs/' + state['id'] + '/res-audiocfg/reaper.ini')} ; [pscustomobject]@{{ mode = (($t | Where-Object {{ $_ -like 'mode=*' }}) -join ';'); asio = @($t | Where-Object {{ $_ -like 'asio*' }}).Count }}")
    print(json.dumps(r))


def cmd_close_render(env, args) -> None:
    state = load_state()
    ini = env["PC_ROOT"] + f"\\jobs\\{state['id']}\\res\\reaper.ini"
    rid = ps(env, f"$id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'close-render' -Fields @{{ ini = {ps_quote(ini)} }} ; Start-ScheduledTask -TaskName {TASK} ; $id")
    print(json.dumps({"close-render": rid}))


def cmd_preempt(env, args) -> None:
    state = load_state()
    running = ps(env, f"@(Get-CimInstance Win32_Process -Filter \"Name = 'reaper.exe'\" | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains({ps_quote(env['PC_ROOT'])}) }}).Count") or 0
    plan = undo_plan(state["done"], state.get("started", []), bool(running))
    print(json.dumps({"preempt": state["id"], "plan": plan}))
    if "stop-render" in plan:
        ps(env, f"New-Item -ItemType File -Force -Path {pc(env, 'queue/stop')} | Out-Null ; $d = (Get-Date).AddSeconds(600) ; while ((Get-Date) -lt $d -and @(Get-CimInstance Win32_Process -Filter \"Name = 'reaper.exe'\" | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains({ps_quote(env['PC_ROOT'])}) }}).Count -gt 0) {{ Start-Sleep -Seconds 1 }} ; 'waited'", timeout=700)
    if "verify-restore" in plan:
        print(json.dumps(step_verify_restore(env, state, args)))
        state["done"].append("verify-restore")
    if "bring-back" in plan:
        print(json.dumps(step_bring_back(env, state, args)))
        state["done"].append("bring-back")
    state["preempted"] = True
    save_state(state)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("new").add_argument("--signal", required=True)
    for name in ("setup", "audiocfg", "read-mode", "close-render", "preempt", "status", *STEPS):
        if name == "stage":
            p = sub.add_parser(name)
            p.add_argument("--bundle", required=True)
            p.add_argument("--sha", required=True)
        elif name == "render":
            sub.add_parser(name).add_argument("--only")
        elif name != "new":
            sub.add_parser(name)
    args = ap.parse_args(argv)
    try:
        if args.cmd == "status":
            print(json.dumps(load_state(), indent=1))
            return 0
        env = load_env(Path(os.environ.get("GOLDEN_ENV", str(Path.home() / ".config/iemmixer/golden.env"))))
        handlers = {"new": cmd_new, "setup": cmd_setup, "audiocfg": cmd_audiocfg, "read-mode": cmd_read_mode, "close-render": cmd_close_render, "preempt": cmd_preempt}
        if args.cmd in handlers:
            handlers[args.cmd](env, args)
        else:
            run_step(env, args.cmd, args)
        return 0
    except StepError as e:
        print(f"golden_window: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
```
Run `python3 -m unittest discover -s scripts/golden -p 'test_golden_window.py' -v`. The result is GREEN.

- [ ] **Step 3: Private env, trees file and MCP access (never committed)**

- Write `$PRIV/golden.env` (`chmod 600`) and `$PRIV/golden-trees.json` exactly as in the private runbook §3.
- Expand `PC_ROOT` and `PC_MAIN_PROJECT` once from the PC's own environment, read-only:
  ```bash
  ssh <PC_SSH> 'powershell -NoProfile -Command "$env:LOCALAPPDATA; $env:USERPROFILE"'
  ```
- Copy the runbook into the ops repo, then commit and push:
  ```bash
  cp "$WP/14-s1b-pc-runbook-private.md" "$OPS/docs/s1b-pc-runbook.md"
  ```
- Register the PC's remote-desktop MCP server at local scope, following runbook §4. The token goes into the credential store and is never printed. Verify it by taking one screenshot.
- If it cannot be registered, stop here. Every window needs it (tray Exit, dialogs).

- [ ] **Step 4: Commit (public parts only)**

```bash
git add scripts/golden/golden_window.py scripts/golden/test_golden_window.py
git commit -m "feat(golden): dev-box window driver with ordered steps and pre-emption" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Analysis, goldens size gate, playbook rule

**Files:**
- Create: `scripts/golden/analyze.py`, `scripts/golden/test_analyze.py`, `.claude/rules/golden-renders.md`, `.gitattributes`
- Modify: `scripts/check_integrity.py`, `scripts/test_check_integrity.py` (goldens ≤ 20 MB), `CLAUDE.md` (router)

**Interfaces:**
- Produces: `python3 scripts/golden/analyze.py --bundle-json RAW/<w>/bundle.json --renders RAW/<w>/renders --stimuli BUNDLE/stimuli --out goldens/s1b [--families cal]`.
  - It writes `laws.json`, `index.json`, `*.f64` (float64 LE) and `README.md`.
  - It exits 1 on any failure: a missing render, a non-float render, a size over budget, or a limiter output above its ceiling.
- Produces: `goldens/s1b/laws.json` = `{schema, generator, renders, laws: {name: {verdict, max_residual, cases, detail}}, residuals_not_covered}`.
  - The verdicts are `confirmed`, `table`, `mismatch` or `measured`.

- [ ] **Step 1: RED — tests (`scripts/golden/test_analyze.py`, numpy)**

```python
"""Tests for scripts/golden/analyze.py (synthetic renders from known filters)."""
from __future__ import annotations

import json
import math
import struct
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import analyze as an  # noqa: E402


def wav(path: Path, rate: int, data: np.ndarray, bits: int = 64, tag: int = 3) -> None:
    data = np.atleast_2d(data.T).T
    frames, ch = data.shape
    raw = data.astype("<f8" if bits == 64 else "<f4").tobytes() if tag == 3 else (data * 32767).astype("<i2").tobytes()
    fmt = struct.pack("<HHIIHH", tag, ch, rate, rate * ch * bits // 8, ch * bits // 8, bits)
    body = b"WAVE" + b"fmt " + struct.pack("<I", len(fmt)) + fmt + b"data" + struct.pack("<I", len(raw)) + raw
    path.write_bytes(b"RIFF" + struct.pack("<I", len(body)) + body)


def direct_form_ir(coef: np.ndarray, n: int) -> np.ndarray:
    """Independent of an.ir: a direct-form-I filter fed a unit impulse."""
    b0, b1, b2, a1, a2 = coef
    x = np.zeros(n)
    x[0] = 1.0
    y = np.zeros(n)
    for i in range(n):
        y[i] = b0 * x[i] + (b1 * x[i - 1] if i > 0 else 0) + (b2 * x[i - 2] if i > 1 else 0) - (a1 * y[i - 1] if i > 0 else 0) - (a2 * y[i - 2] if i > 1 else 0)
    return y


class WavTests(unittest.TestCase):
    def test_read_wav_round_trips_float_and_refuses_pcm(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "a.wav"
            wav(p, 96000, np.array([[0.5, 0.25], [-1.0, 0.0]]))
            rate, y, bits = an.read_wav(p)
            self.assertEqual((rate, bits), (96000, 64))
            self.assertTrue(np.array_equal(y, np.array([[0.5, 0.25], [-1.0, 0.0]])))
            wav(p, 48000, np.array([[0.5], [0.5]]), bits=16, tag=1)
            with self.assertRaises(an.Fail):
                an.read_wav(p)


class EqTests(unittest.TestCase):
    def test_ir_matches_an_independent_direct_form(self) -> None:
        coef = an.rbj("band", 96000, 1000.0, 10 ** (6 / 20), 1.0)
        self.assertLess(np.max(np.abs(an.ir(coef, 256) - direct_form_ir(coef, 256))), 1e-15)

    def test_recover_biquad_is_exact_for_an_rbj_peak(self) -> None:
        coef = an.rbj("band", 96000, 1000.0, 10 ** (6 / 20), 1.0)
        got = an.recover_biquad(direct_form_ir(coef, 256))
        self.assertLess(np.max(np.abs(got - coef)), 1e-12)

    def test_shelf_alpha_classifies_candidates(self) -> None:
        for fs, f0, g, bw in ((96000, 307.0, 10 ** (-9 / 20), 1.5), (96000, 20.0, 10 ** (12 / 20), 0.4), (44100, 8000.0, 10 ** (-3 / 20), 2.0)):
            for cand in ("B", "A"):
                alpha = an.alpha_bw(fs, f0, bw) if cand == "B" else an.alpha_slope(fs, f0, g, bw)
                h = direct_form_ir(an.rbj("low_shelf", fs, f0, g, bw, alpha=alpha), 256)
                self.assertEqual(an.classify_shelf("low_shelf", fs, f0, g, bw, h), cand, (fs, f0, cand))
        h = direct_form_ir(an.rbj("low_shelf", 96000, 307.0, 0.5, 1.5, alpha=0.001), 256)
        self.assertEqual(an.classify_shelf("low_shelf", 96000, 307.0, 0.5, 1.5, h), "neither")

    def test_hp_gain_scale_is_measured(self) -> None:
        g = 2.0
        coef = an.rbj("high_pass", 96000, 100.0, 1.0, 2.0) * np.array([g, g, g, 1, 1])
        self.assertAlmostEqual(an.hp_gain_scale(96000, 100.0, 2.0, direct_form_ir(coef, 256)), g, places=12)


class LinearTests(unittest.TestCase):
    def test_linear_oracle_detects_a_wrong_gain(self) -> None:
        y = np.zeros((2000, 2))
        y[960] = [0.25, 0.5]
        expect = [{"at": 960, "l": 0.25, "r": 0.5}]
        self.assertEqual(an.oracle_error(y, expect), 0.0)
        y[960, 0] = 0.26
        self.assertAlmostEqual(an.oracle_error(y, expect), 0.01, places=12)
        y[961, 1] = 1e-3
        self.assertGreaterEqual(an.oracle_error(y, expect), 1e-3)


class SizeTests(unittest.TestCase):
    def test_size_gate(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "x.f64").write_bytes(b"\0" * 1024)
            an.check_size(Path(d), limit=2048)
            with self.assertRaises(an.Fail):
                an.check_size(Path(d), limit=512)


class EndToEndTests(unittest.TestCase):
    def test_a_pan_case_and_an_eq_case_become_laws_and_vectors(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "renders" / "pan-96000").mkdir(parents=True)
            (root / "renders" / "eq-96000").mkdir(parents=True)
            (root / "stimuli").mkdir()
            y = np.zeros((48000, 2))
            y[960] = [0.25, 0.5]
            wav(root / "renders" / "pan-96000" / "pan-send-dm-30.wav", 96000, y)
            coef = an.rbj("band", 96000, 1000.0, 2.0, 1.0)
            h = direct_form_ir(coef, 48000 - 960) * 0.5
            e = np.zeros((48000, 2))
            e[960:, 0] = h
            e[960:, 1] = h
            wav(root / "renders" / "eq-96000" / "eq-pk-f2-g3-w2.wav", 96000, e)
            bundle = {"generator": "test", "projects": [
                {"id": "pan-96000", "rate": 96000, "bits": 64, "tracks": [{"track": "pan-send-dm-30", "family": "pan", "stimulus": "imp-dm-96000.wav", "position": 0,
                  "params": {"what": "send_pan", "source": "dm", "pan": 0.5}, "expect": [{"at": 960, "l": 0.25, "r": 0.5}]}]},
                {"id": "eq-96000", "rate": 96000, "bits": 64, "tracks": [{"track": "eq-pk-f2-g3-w2", "family": "eq", "stimulus": "imp-dm-96000.wav", "position": 0,
                  "params": {"band": {"kind": "band", "enabled": True, "freq_hz": 1000.0, "gain_lin": 2.0, "bw_oct": 1.0}, "global_gain": 1.0}, "expect": None}]}]}
            (root / "bundle.json").write_text(json.dumps(bundle), encoding="utf-8")
            out = root / "goldens"
            self.assertEqual(an.main(["--bundle-json", str(root / "bundle.json"), "--renders", str(root / "renders"), "--stimuli", str(root / "stimuli"), "--out", str(out)]), 0)
            laws = json.loads((out / "laws.json").read_text(encoding="utf-8"))["laws"]
            self.assertEqual(laws["send_pan"]["verdict"], "confirmed")
            self.assertEqual(laws["peak_bw"]["verdict"], "confirmed")
            self.assertTrue((out / "eq-96000.f64").is_file())
            self.assertTrue(math.isclose(laws["peak_bw"]["max_residual"], 0.0, abs_tol=1e-9))


if __name__ == "__main__":
    unittest.main()
```
Run it: RED.

- [ ] **Step 2: GREEN — `scripts/golden/analyze.py`**

```python
#!/usr/bin/env python3
"""S1b analysis (design note §3): renders fetched from the IEM PC plus the
bundle's case metadata → goldens/s1b/ (laws.json, index.json, *.f64
float64 LE, README.md). Needs numpy. Never claims a live-only behaviour."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import struct
import sys
from pathlib import Path

import numpy as np

LIMIT_BYTES = 20 * 1024 * 1024
TOL_LINEAR = 1e-12
TOL_COEF = 1e-9
EQ_TAPS = 256
SITE_TAPS = 2048
RESIDUALS = [
    "live input duplication of mono inputs (RECMON) — Method B",
    "hardware-output mono downmix (TRANSLATOR) — the send-to-mono law stands in; Method B/C",
    "FX processing on muted tracks (norunmute) — Method C",
    "live plugin delay compensation — Method B/C",
    "REAPER volume/pan/mute ramps — not reproduced (X15)",
]


class Fail(Exception):
    pass


def read_wav(path: Path) -> tuple[int, np.ndarray, int]:
    data = path.read_bytes()
    if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise Fail(f"{path.name}: not RIFF/WAVE")
    pos, fmt, frames = 12, None, None
    while pos + 8 <= len(data):
        cid = data[pos:pos + 4]
        size = struct.unpack_from("<I", data, pos + 4)[0]
        body = data[pos + 8:pos + 8 + size]
        if cid == b"fmt ":
            tag, ch, rate, _, _, bits = struct.unpack_from("<HHIIHH", body)
            if tag == 0xFFFE and len(body) >= 26:
                tag = struct.unpack_from("<H", body, 24)[0]
            fmt = (tag, ch, rate, bits)
        elif cid == b"data":
            frames = body
        pos += 8 + size + (size & 1)
    if fmt is None or frames is None:
        raise Fail(f"{path.name}: no fmt/data chunk")
    tag, ch, rate, bits = fmt
    if tag != 3 or bits not in (32, 64):
        raise Fail(f"{path.name}: not IEEE float 32/64 (tag {tag}, {bits} bit)")
    y = np.frombuffer(frames, dtype="<f8" if bits == 64 else "<f4").astype(np.float64)
    return rate, y.reshape(-1, ch), bits


# ---- linear cases ----

def oracle_error(y: np.ndarray, expect: list[dict]) -> float:
    ref = np.zeros_like(y)
    for tap in expect:
        if tap["at"] < len(y):
            ref[tap["at"], 0] += tap["l"]
            ref[tap["at"], 1 % y.shape[1]] += tap["r"]
    return float(np.max(np.abs(y - ref))) if y.size else 0.0


def taps(y: np.ndarray, at: int) -> tuple[float, float]:
    return float(y[at, 0]), float(y[at, 1 % y.shape[1]])


# ---- EQ ----

def alpha_bw(fs: float, f0: float, bw: float) -> float:
    w0 = 2 * math.pi * f0 / fs
    return math.sin(w0) * math.sinh(math.log(2) / 2 * bw * w0 / math.sin(w0))


def alpha_slope(fs: float, f0: float, g: float, bw: float) -> float:
    w0 = 2 * math.pi * f0 / fs
    a = math.sqrt(g)
    s = min(max(1.0 / max(bw, 0.01), 0.01), 1.0)
    return math.sin(w0) / 2 * math.sqrt(max((a + 1 / a) * (1 / s - 1) + 2, 0.0))


def rbj(kind: str, fs: float, f0: float, g: float, bw: float, alpha: float | None = None) -> np.ndarray:
    w0 = 2 * math.pi * f0 / fs
    c = math.cos(w0)
    a = math.sqrt(g)
    al = alpha_bw(fs, f0, bw) if alpha is None else alpha
    if kind == "band":
        b = [1 + al * a, -2 * c, 1 - al * a]
        d = [1 + al / a, -2 * c, 1 - al / a]
    elif kind == "high_pass":
        b = [(1 + c) / 2, -(1 + c), (1 + c) / 2]
        d = [1 + al, -2 * c, 1 - al]
    elif kind == "low_shelf":
        r = 2 * math.sqrt(a) * al
        b = [a * ((a + 1) - (a - 1) * c + r), 2 * a * ((a - 1) - (a + 1) * c), a * ((a + 1) - (a - 1) * c - r)]
        d = [(a + 1) + (a - 1) * c + r, -2 * ((a - 1) + (a + 1) * c), (a + 1) + (a - 1) * c - r]
    elif kind == "high_shelf":
        r = 2 * math.sqrt(a) * al
        b = [a * ((a + 1) + (a - 1) * c + r), -2 * a * ((a - 1) + (a + 1) * c), a * ((a + 1) + (a - 1) * c - r)]
        d = [(a + 1) - (a - 1) * c + r, 2 * ((a - 1) - (a + 1) * c), (a + 1) - (a - 1) * c - r]
    else:
        raise Fail(f"unknown band kind {kind}")
    return np.array([b[0] / d[0], b[1] / d[0], b[2] / d[0], d[1] / d[0], d[2] / d[0]])


def recover_biquad(h: np.ndarray, n: int = 64) -> np.ndarray:
    m = np.column_stack([-h[2:n - 1], -h[1:n - 2]])
    (a1, a2), *_ = np.linalg.lstsq(m, h[3:n], rcond=None)
    b0 = h[0]
    b1 = h[1] + a1 * h[0]
    b2 = h[2] + a1 * h[1] + a2 * h[0]
    return np.array([b0, b1, b2, a1, a2])


def ir(coef: np.ndarray, n: int) -> np.ndarray:
    """Impulse response of a normalised biquad [b0, b1, b2, a1, a2]."""
    b0, b1, b2, a1, a2 = (float(c) for c in coef)
    y = [0.0] * n
    for i in range(n):
        x = b0 if i == 0 else b1 if i == 1 else b2 if i == 2 else 0.0
        y[i] = x - (a1 * y[i - 1] if i > 0 else 0.0) - (a2 * y[i - 2] if i > 1 else 0.0)
    return np.array(y)


def ir_residual(h: np.ndarray, coef: np.ndarray) -> float:
    """Max deviation of a measured IR from a candidate, relative to its peak.
    Classification compares responses, never inverted coefficients: the
    inversion is ill-conditioned for low f0/fs."""
    return float(np.max(np.abs(ir(coef, len(h)) - h)) / max(float(np.max(np.abs(h))), 1e-300))


def shelf_alpha(kind: str, fs: float, f0: float, g: float, coef: np.ndarray) -> float:
    """Informational: alpha implied by recovered coefficients (RBJ shelf form)."""
    c = math.cos(2 * math.pi * f0 / fs)
    a = math.sqrt(g)
    a2n = coef[4]
    base = (a + 1) + (a - 1) * c if kind == "low_shelf" else (a + 1) - (a - 1) * c
    a0 = 2 * base / (1 + a2n)
    return a0 * (1 - a2n) / (4 * math.sqrt(a))


def classify_shelf(kind: str, fs: float, f0: float, g: float, bw: float, h: np.ndarray) -> str:
    for name, alpha in (("B", alpha_bw(fs, f0, bw)), ("A", alpha_slope(fs, f0, g, bw))):
        if ir_residual(h[:EQ_TAPS], rbj(kind, fs, f0, g, bw, alpha=alpha)) <= TOL_COEF:
            return name
    return "neither"


def hp_gain_scale(fs: float, f0: float, bw: float, h: np.ndarray) -> float:
    """h[0] = b0 exactly, so the gain applied to an HPF band is h[0] / b0(G=1)."""
    return float(h[0] / rbj("high_pass", fs, f0, 1.0, bw)[0])


def fftconv(a: np.ndarray, b: np.ndarray) -> np.ndarray:
    n = len(a) + len(b) - 1
    size = 1 << (n - 1).bit_length()
    return np.fft.irfft(np.fft.rfft(a, size) * np.fft.rfft(b, size), size)[:n]


# ---- output ----

def check_size(out: Path, limit: int = LIMIT_BYTES) -> int:
    total = sum(p.stat().st_size for p in out.rglob("*") if p.is_file())
    if total > limit:
        raise Fail(f"goldens are {total} bytes, over the {limit}-byte budget")
    return total


class Vectors:
    """Appends float64 vectors to <name>.f64 and records offsets in index.json."""

    def __init__(self, out: Path) -> None:
        self.out, self.index, self.handles = out, {}, {}

    def add(self, name: str, case: str, data: np.ndarray, meta: dict) -> None:
        fh = self.handles.setdefault(name, (self.out / f"{name}.f64").open("wb"))
        offset = fh.tell() // 8
        arr = np.ascontiguousarray(data, dtype="<f8")
        fh.write(arr.tobytes())
        self.index.setdefault(name, {})[case] = {"offset": offset, "shape": list(arr.shape), **meta}

    def close(self) -> None:
        for fh in self.handles.values():
            fh.close()
        (self.out / "index.json").write_text(json.dumps(self.index, indent=1, sort_keys=True), encoding="utf-8")


def law(laws: dict, name: str, case: str, residual: float, tol: float = TOL_LINEAR, detail: dict | None = None) -> None:
    entry = laws.setdefault(name, {"verdict": "confirmed", "max_residual": 0.0, "cases": [], "detail": []})
    entry["cases"].append(case)
    entry["max_residual"] = max(entry["max_residual"], residual)
    if residual > tol:
        entry["verdict"] = "mismatch"
    if detail:
        entry["detail"].append(detail)


def analyse(bundle: dict, renders: Path, stimuli: Path, out: Path, families: set[str] | None) -> dict:
    laws: dict = {}
    vec = Vectors(out)
    for proj in bundle["projects"]:
        for case in proj["tracks"]:
            fam = case["family"]
            if families and fam not in families:
                continue
            path = renders / proj["id"] / f"{case['track']}.wav"
            if not path.is_file():
                raise Fail(f"missing render {proj['id']}/{case['track']}.wav")
            rate, y, bits = read_wav(path)
            if rate != proj["rate"]:
                raise Fail(f"{path.name}: rate {rate}, expected {proj['rate']}")
            p, name = case["params"], case["track"]
            law(laws, f"render_bits_{proj['bits']}", name, 0.0 if bits == proj["bits"] else 1.0, tol=0.5, detail={"bits": bits})
            if bits != 64:
                continue   # 1e-9 comparisons need 64-bit renders (Task 13 fixes the config)
            k0 = rate // 100
            if case["expect"] is not None:
                fam_law = {"pan": p.get("what", "pan"), "mute": "mute", "sum": "summing", "mono": "mono_media", "cal": "cal_" + str(p.get("case"))}.get(fam, fam)
                law(laws, fam_law, name, oracle_error(y, case["expect"]), detail={"params": p, "measured": [taps(y, t["at"]) for t in case["expect"]]})
            elif fam == "cal" and p["case"] == "trim6":
                g = y[k0, 0] / 0.5
                law(laws, "trim_db", name, abs(g - 10 ** (6 / 20)), detail={"gain": g})
            elif fam in ("cal", "eq", "eq-edge", "bypass") and ("band" in p or "eq" in p or p.get("case") == "peak" or fam == "bypass"):
                h = y[k0:, 0] / 0.5
                if fam == "bypass":
                    want = {"identity": 1.0, "trim_only": 10 ** (p.get("trim_db", 0) / 20)}.get(p["expect"])
                    if want is not None:
                        err = float(np.max(np.abs(h[1:EQ_TAPS]))) + abs(h[0] - want)
                        law(laws, f"bypass_{p['what']}", name, err, detail={"h0": float(h[0])})
                    continue
                vec.add(f"eq-{rate}", name, h[:EQ_TAPS], {"params": p})
                band = p.get("band")
                if band is None:
                    law(laws, "eq_edge", name, 0.0, tol=math.inf, detail={"params": p, "h0": float(h[0]), "sum_abs": float(np.sum(np.abs(h[:EQ_TAPS])))})
                    laws["eq_edge"]["verdict"] = "measured"
                    continue
                coef = recover_biquad(h)
                hn = h[:EQ_TAPS]
                kind, f0, g, bw = band["kind"], band["freq_hz"], band["gain_lin"], band["bw_oct"]
                if kind == "band":
                    law(laws, "peak_bw", name, ir_residual(hn, rbj("band", rate, f0, g, bw)), tol=TOL_COEF, detail={"coef": coef.tolist()})
                elif kind == "high_pass":
                    scale = hp_gain_scale(rate, f0, bw, hn)
                    law(laws, "hp_bw", name, ir_residual(hn / scale, rbj("high_pass", rate, f0, 1.0, bw)), tol=TOL_COEF)
                    laws.setdefault("hp_gain", {"verdict": "measured", "max_residual": 0.0, "cases": [], "detail": []})
                    laws["hp_gain"]["cases"].append(name)
                    laws["hp_gain"]["detail"].append({"gain_lin": g, "scale": scale})
                else:
                    verdict = classify_shelf(kind, rate, f0, g, bw, hn)
                    laws.setdefault("shelf_bw", {"verdict": "measured", "max_residual": 0.0, "cases": [], "detail": []})
                    laws["shelf_bw"]["cases"].append(name)
                    laws["shelf_bw"]["detail"].append({"kind": kind, "fs": rate, "f0": f0, "gain_lin": g, "bw": bw, "alpha_implied": shelf_alpha(kind, rate, f0, g, coef), "candidate": verdict, "coef": coef.tolist()})
            elif fam == "downmix":
                k2 = rate // 50
                law(laws, "mono_downmix", name, 0.0, tol=math.inf, detail={"params": p, "L_at_k0": taps(y, k0), "R_at_k2": taps(y, k2)})
                laws["mono_downmix"]["verdict"] = "measured"
            elif fam == "site-eq":
                if p["stimulus"] == "impulse":
                    vec.add("site-eq-96000", p["id"], y[k0:k0 + SITE_TAPS, 0] / 0.5, {"eq": p["eq"]})
                else:
                    imp = renders / proj["id"] / f"{name.replace('-sweep', '-imp')}.wav"
                    _, yi, _ = read_wav(imp)
                    _, stim, _ = read_wav(stimuli / "sweep-96000.wav")
                    h = yi[k0:, 0] / 0.5
                    ref = fftconv(stim[:, 0], h)[: len(y)]
                    err = float(np.max(np.abs(y[: len(ref), 0] - ref)) / max(np.max(np.abs(y[:, 0])), 1e-300))
                    law(laws, "site_eq_linearity", name, err, tol=1e-6, detail={"id": p["id"], "residual_db": 20 * math.log10(max(err, 1e-300))})
            elif fam == "lim":
                ceiling = 10 ** (p["limit_db"] / 20)
                n = rate
                peak = float(np.max(np.abs(y[:n])))
                if peak > ceiling * (1 + 1e-12):
                    raise Fail(f"{name}: limiter output {peak} above ceiling {ceiling}")
                vec.add(f"lim-{rate}", name, y[:n], {"params": p})
                law(laws, "limiter_ceiling", name, 0.0, detail={"peak": peak, "ceiling": ceiling})
    vec.close()
    for key in ("shelf_bw",):
        entry = laws.get(key)
        if entry:
            cands = {d["candidate"] for d in entry["detail"]}
            entry["verdict"] = f"candidate {cands.pop()}" if len(cands) == 1 and "neither" not in cands else "table"
    for key in ("hp_gain",):
        entry = laws.get(key)
        if entry:
            scales = entry["detail"]
            if all(abs(d["scale"] - 1) <= TOL_COEF for d in scales):
                entry["verdict"] = "ignored"
            elif all(abs(d["scale"] - d["gain_lin"]) <= TOL_COEF * d["gain_lin"] for d in scales):
                entry["verdict"] = "linear gain"
            else:
                entry["verdict"] = "table"
    return laws


def readme(laws: dict) -> str:
    rows = ["| Law | Verdict | Max residual | Cases |", "|---|---|---|---|"]
    for name in sorted(laws):
        e = laws[name]
        rows.append(f"| `{name}` | {e['verdict']} | {e['max_residual']:.3g} | {len(e['cases'])} |")
    return "\n".join(["# S1b goldens", "", "Measured on the IEM PC's REAPER 7.65 (offline renders, D7). Generated by `scripts/golden/analyze.py`; vectors are float64 little-endian, offsets in `index.json`.", "", *rows, "", "## Not covered by offline renders", "", *[f"- {r}" for r in RESIDUALS], ""])


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--bundle-json", required=True)
    ap.add_argument("--renders", required=True)
    ap.add_argument("--stimuli", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--families")
    args = ap.parse_args(argv)
    bundle = json.loads(Path(args.bundle_json).read_text(encoding="utf-8"))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    try:
        laws = analyse(bundle, Path(args.renders), Path(args.stimuli), out, set(args.families.split(",")) if args.families else None)
        renders = sorted(p for p in Path(args.renders).rglob("*.wav"))
        digest = hashlib.sha256("".join(f"{p.relative_to(args.renders).as_posix()}:{hashlib.sha256(p.read_bytes()).hexdigest()}\n" for p in renders).encode()).hexdigest()
        doc = {"schema": 1, "generator": bundle.get("generator"), "renders": {"count": len(renders), "sha256_of_list": digest}, "laws": laws, "residuals_not_covered": RESIDUALS}
        (out / "laws.json").write_text(json.dumps(doc, indent=1, sort_keys=True), encoding="utf-8")
        (out / "README.md").write_text(readme(laws), encoding="utf-8")
        total = check_size(out)
    except Fail as e:
        print(f"analyze: {e}", file=sys.stderr)
        return 1
    print(f"analyze: {len(laws)} laws, {total} bytes in {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
```
Run `python3 -m unittest discover -s scripts/golden -p 'test_analyze.py' -v`. The result is GREEN; numpy 2.4.6 is on the dev box. The `law(...)` bookkeeping needs `detail` defaulting to a list, which the helper creates.

- [ ] **Step 3: Goldens size gate in `check_integrity.py` (RED → GREEN)**

Test first, in `scripts/test_check_integrity.py`:
```python
    def test_goldens_over_twenty_megabytes_fail(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "goldens" / "s1b").mkdir(parents=True)
            (root / "goldens" / "s1b" / "x.f64").write_bytes(b"\0" * (20 * 1024 * 1024 + 1))
            self.assertIn("goldens/: 20971521 bytes, over the 20 MB budget (spec §3.5)", ci.violations(root))
```
Then add to `violations()` in `scripts/check_integrity.py`:
```python
    goldens = root / "goldens"
    if goldens.is_dir():
        total = sum(p.stat().st_size for p in goldens.rglob("*") if p.is_file())
        if total > 20 * 1024 * 1024:
            found.append(f"goldens/: {total} bytes, over the 20 MB budget (spec §3.5)")
```

- [ ] **Step 4: `.gitattributes`, playbook rule, router**

`.gitattributes`:
```
goldens/**/*.f64 binary
```

`.claude/rules/golden-renders.md`:
```markdown
---
paths:
  - "crates/iem-rpp/**"
  - "scripts/golden/**"
  - "goldens/**"
---

# Golden renders (S1b, D7)

- REAPER runs only on the IEM PC, only in dev time (the owner's "event skončil" quoted in `golden_window.py new --signal`), one step at a time in the order of the S1b design note §4. "ide event" → `golden_window.py preempt` immediately, then the event runbook.
- The render instance never uses audio mode 3 (ASIO); every render is watched with `tasklist /m`; any holder stops the queue. Never end any process by force: dialogs are closed via MCP or `close-render`.
- Only the `golden-bundle-<sha>` artifact of a reviewed `dev` push may reach the PC; `check_bundle.py` and the PC stager repeat the hash and plug-in allowlist checks. A new plug-in needs all three allowlists changed together (`fx.rs`, `check_bundle.py`, `GoldenPc.psm1`).
- A window succeeds only with `verify-restore` identical. PC backups stay on the PC (they hold the predecessor's secrets); raw renders live in `~/.local/share/iemmixer/golden-raw/`, never in the repo or `/tmp`.
- `RENDER_CFG` / `RENDER_STEMS` values are only trusted after the `cal` family confirmed them; analysis refuses non-float renders and compares at 1e-9 only on 64-bit.
- Site values (paths, names, host, module) are only in `~/.config/iemmixer/golden.env`, `golden-trees.json` and the ops runbook.
```

In `CLAUDE.md`, under "Playbook router", add:
```
- Golden renders on the IEM PC (generator, window driver, analysis) → `.claude/rules/golden-renders.md`
```

- [ ] **Step 5: Commit**

```bash
python3 scripts/check_integrity.py && python3 -m unittest discover -s scripts -p 'test_*.py' -v
python3 -m unittest discover -s scripts/golden -p 'test_*.py' -v
git add scripts .gitattributes .claude/rules/golden-renders.md CLAUDE.md
git commit -m "feat(golden): render analysis, goldens size gate and playbook rule" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 12: First push, CI green, bundle download

- [ ] **Step 1: Push `dev` and wait for every job** (load the `ci-push-discipline` skill first)

```bash
git fetch origin && git merge origin/dev && git push origin dev
run=$(gh run list -R "$REPO" --branch dev --workflow CI --limit 1 --json databaseId -q '.[0].databaseId')
# one foreground bounded poll per the ci-monitoring skill; then:
gh run view "$run" -R "$REPO" --json conclusion,jobs -q '.conclusion, (.jobs[] | "\(.name): \(.conclusion)")'
```
Expected: every job succeeds, including `test` (with `iem-rpp` at or above the coverage floor), `integrity` (golden self-tests), `windows` (`Test-GoldenPc: all passed`) and `golden-bundle`.

The likely first-push fixes are clippy lints in the new crate, and `mutants-list` shard capacity; resize the matrix, never the timeout.

Fix everything in ONE commit per round, with the cause of each fix in the message.

- [ ] **Step 2: Download the bundle of that exact commit**

```bash
sha=$(git rev-parse HEAD)
[ "$(gh run view "$run" -R "$REPO" --json headSha -q .headSha)" = "$sha" ]
B="$RAW/bundles/$sha"; mkdir -p "$B"
gh run download "$run" -R "$REPO" -n "golden-bundle-$sha" -D "$B"
echo "$sha" > "$B/.source-sha"
python3 scripts/golden/check_bundle.py "$B"
jq '[.projects[] | .tracks | length] | add' "$B/bundle.json"
```
Expected: `bundle: clean`, with about 1,900 cases (1,278 EQ, 144 pan, plus the site EQs and the rest).

Record the SHA and the case count on #4.

---

### Task 13: Window 1 — setup, Dummy Audio calibration, `cal` renders (IEM PC, dev time)

**Precondition:** the owner's "event skončil" was received in this conversation after the last "ide event". Never ask for it; if it has not come, work on anything else and run this task when it does. From here on, an "ide event" means `preempt` first, then the event runbook.

All commands use `G="python3 scripts/golden/golden_window.py"`. The MCP steps use the PC's remote-desktop MCP server, per private runbook §4.

- [ ] **Step 1: Open the window and set up**

```bash
$G new --signal "<owner's message, verbatim, with its time>"
$G setup          # first window only: uploads the module, registers the task
$G preflight
```
Expected output from `preflight`:
- `version 7.65`;
- `holders` only `reaper.exe:<pid>`;
- `render_instances 0`;
- `free_gb ≥ 10`;
- `task true`.

Also check the `pre` state.

- [ ] **Step 2: Quiet check, save, quit, app stop**

```bash
$G interlock      # 60 s; any peak > -50 dBFS aborts and alarms the owner
$G save-quit      # save (40026), quit (40004), REAPER gone, ASIO module free
```
MCP: open the notification-area menu of the predecessor app and click its **Exit** item. Take a screenshot before and after. The private runbook §4 names the icon.

```bash
$G app-stopped
$G backup         # ~0.7 GB; prints file count and bytes; manifest copied to $RAW/<id>/
```

- [ ] **Step 3: Confirm the Dummy Audio mode value**

```bash
$G stage --bundle "$B" --sha "$sha"
$G audiocfg       # starts a throwaway render instance with -audiocfg (resource folder res-audiocfg)
```
MCP: take a screenshot of the audio device dialog of the new REAPER window.
- If the audio system shows **Dummy Audio**, set the rate to 96000 and click OK.
- If it shows anything else, select **Dummy Audio**, set 96000, and click OK.

Then use **File > Quit** in that instance. Meanwhile, `ssh … tasklist /m <ASIO module>` must stay empty, and the watcher is not running yet, so check it by hand every few seconds.

```bash
$G read-mode      # prints mode=<value>, asio=<0>
```
- Set `PC_DUMMY_MODE=<value>` in `$GOLDEN_ENV`. The value must not be 3.
- Post it on #4: `ROZHODNUTÉ/ZISTENÉ: Dummy Audio = mode <value> (REAPER 7.65, IEM PC)`.
- Add it to the ops runbook §3.

- [ ] **Step 4: Render only the calibration projects**

```bash
$G seed-res
$G render --only cal-96000-f64,cal-96000-f32
$G fetch
python3 scripts/golden/analyze.py --bundle-json "$RAW/<id>/bundle.json" --renders "$RAW/<id>/renders" --stimuli "$B/stimuli" --out "$RAW/<id>/cal-check" --families cal
jq '.laws | with_entries(.value |= {verdict, max_residual})' "$RAW/<id>/cal-check/laws.json"
```

Expected:
- `render_bits_64` and `render_bits_32` are confirmed.
- `cal_identity`, `cal_stereo`, `cal_mono` and `trim_db` are confirmed at ≤ 1e-12.
- `peak_bw` holds its first case.
- There is one stem file per selected track and no master file. This confirms `RENDER_STEMS 2`.

If the file bit depths differ from the config (for example, the 64-bit config produced 24-bit PCM), take the render config byte that produced 64-bit float. This analysis refuses PCM with a clear message, so read the header yourself: `python3 -c "import struct,sys; d=open(sys.argv[1],'rb').read(64); print(struct.unpack_from('<HHIIHH', d, 20))" <file>`.

Post the finding on #4 and fix `RenderFormat::cfg` in a `fix(iem-rpp)` commit. Window 2 then uses the new bundle.

- [ ] **Step 5: Exercise `close-render` once, restore, bring back**

`close-render` must answer `closed: 0` (nothing runs). That proves the path works without a hung instance.

```bash
$G close-render
$G verify-restore     # must print identical: true
$G bring-back         # REAPER via its start task, then the app; both answer
```

Run the event runbook's handover checks:
- REAPER alive, with no dialog (MCP screenshot);
- the app answers and is connected to REAPER;
- the meters report `UNCONFIRMED-AUDIO` (dev time, no band), reported as such.

Post on #4 and put the same text in the owner confirmation (✅):
- `verify.json` counts (restored, quarantined, touched);
- the `renders-sha256.json` count;
- that nothing was killed.

---

### Task 14: Window 2 — full renders (IEM PC, dev time)

**Precondition:**
- the owner's "event skončil" arrived after the last "ide event";
- Task 13 is done;
- if Task 13 changed code, Task 12 was repeated for the new SHA, so that `$B` and `$sha` are the new bundle.

- [ ] **Step 1: Same opening as window 1 (no setup, no audiocfg)**

```bash
$G new --signal "<owner's message, verbatim, with its time>"
$G preflight && $G interlock && $G save-quit
```
MCP: tray **Exit** of the predecessor app.
```bash
$G app-stopped && $G backup
$G stage --bundle "$B" --sha "$sha"
$G seed-res
```

- [ ] **Step 2: Render everything, fetch**

```bash
$G render            # all projects; the watcher alarms on any ASIO module holder
$G fetch             # ~2 GB over the LAN; hashes compared file by file
```
If a render ends `hung`:
- take an MCP screenshot and dismiss the dialog with its own button;
- run `$G close-render` if the window stays;
- note the project on #4;
- continue with `$G render --only <remaining ids>` after re-running the order check. It needs a fresh window, so run `preempt` and redo the window. It never kills.

- [ ] **Step 3: Restore, bring back, report**

```bash
$G verify-restore && $G bring-back
```
Run the handover checks as in Task 13 Step 5.

Post on #4:
- the window id;
- the restore evidence (identical, counts);
- the render count and bytes;
- any finding.

---

### Task 15: Analysis, goldens commit, results

- [ ] **Step 1: Analyse into the repository**

```bash
W="$RAW/<window-2 id>"
rm -rf goldens/s1b && python3 scripts/golden/analyze.py --bundle-json "$W/bundle.json" --renders "$W/renders" --stimuli "$B/stimuli" --out goldens/s1b
jq '.laws | with_entries(.value |= {verdict, max_residual, n: (.cases | length)})' goldens/s1b/laws.json
du -sb goldens/s1b
python3 scripts/check_integrity.py
```

Expected:
- **Size:** `du` ≤ 20,971,520 bytes.
- **Verdicts:** every law has one.
  - `send_pan`, `track_pan`, `post_fader`, `send_vol`, `mute`, `summing`, `mono_media`, `bypass_*` and `trim_db` are `confirmed`.
  - A `mismatch` there is a finding, not a failure to hide.
  - `peak_bw` and `hp_bw` are `confirmed` or `mismatch`.
  - `shelf_bw` is `candidate A`, `candidate B` or `table`.
  - `hp_gain` is `ignored`, `linear gain` or `table`.
  - `mono_downmix` is `measured`, and so is `site_eq_linearity`, with its residual in dB.

- [ ] **Step 2: Record every law on #4 (durable)**

For each law, post one line: `ZISTENÉ: <law> = <verdict> (max residual <x>, <n> cases, window <id>)`.

A `mismatch` in a linear law, a `table` shelf law, or `hp_gain` ≠ `ignored` changes S2's DSP. Add that to the S2 hand-off in Task 16 and comment on #5.

- [ ] **Step 3: Denylist-scan and commit the goldens**

```bash
tmpidx="$(mktemp)"; cp .git/index "$tmpidx"; GIT_INDEX_FILE="$tmpidx" git add goldens
tree="$(GIT_INDEX_FILE="$tmpidx" git write-tree)"; rm -f "$tmpidx"
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$tree"
git add goldens
git commit -m "test(goldens): S1b measured laws and golden vectors from the IEM PC's REAPER" -m "Refs #4" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 4: Results pointer in the design note**

Append §10 "Results" to the design note: 2–5 lines pointing to `goldens/s1b/README.md` and the #4 comments, and naming any law that changed a program-spec assumption (A5, A10, A12). If a result contradicts the program spec, file the spec change as its own ticket and cite `#N`. Commit it: `docs(s1b): results pointer`.

---

### Task 16: PR, merge, hand-off

- [ ] **Step 1: Push, green CI, PR** (load the `ci-push-discipline` and `pr-merge-policy` skills)

```bash
git fetch origin && git merge origin/dev && git merge origin/main && git push origin dev
# wait for every job (Task 12 Step 1 pattern)
gh pr create -R "$REPO" --base main --head dev --title "S1b: golden renders from the IEM PC's REAPER" --body-file "$RAW/../s1b-pr-body.md"
```

The PR body covers, with no site values:
- Goal, What changed, Tests and Results (the laws table);
- Closes #4;
- the restore evidence summary;
- it ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.

Wait for every PR job, including the mutation shards and the required checks. Kill surviving mutants in the diff with tests. Merge only when mergeable and `clean`:
```bash
gh pr merge <N> -R "$REPO" --merge
```

- [ ] **Step 2: Hand-off to S2 (#5) and close-out on #4**

Post on #5 (Slovak summary plus English technical lines):
- the vector formats (`goldens/s1b/index.json`, float64 LE);
- which laws S2 implements (with verdicts);
- the limiter input: regenerate it with `iem_rpp::stimulus::hot_material(rate, 27)`, checksums in `stimulus.rs`;
- the residuals for S7/S8.

On #4, post the merged SHA and the window ids. Run the `playbook-review` skill and send the completion report.

---

## Hand-off to later sub-projects

- **S2 (#5):**
  - Implement the measured pan, shelf, HPF and downmix laws.
  - Use `goldens/s1b` as the harness input. `iem-rpp` becomes a dev-dependency so S2 can regenerate the limiter input.
  - `deny.toml` GPL exception for the limiter crate, as already planned.
- **S4 (#7):** add the importer and exporter to `iem-rpp`, reusing `rpp.rs` and `reaeq.rs`; the ReaEQ chunk encoder is byte-exact against REAPER 7.65.
- **S6 (#9):** reuse the window procedure pieces for `iemmode` (save and quit, bring-back, handover checks, never kill). The MCP registration and the Interactive-task pattern are proven here.
- **S7/S8:** the residuals list in `goldens/s1b/README.md` (Method B/C).
