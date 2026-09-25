# iemmixer S0 — Bootstrap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the empty public repo `zbynekdrlik/iemmixer` into a clean, scrubbed, CI-green Gen 2 codebase: fresh import of the predecessor's web app at a pinned SHA, the security baseline (no default PINs, argon2id + pepper, complete login limiter), a hosted-only CI skeleton, repo settings and rulesets, and the private ops repo skeleton — merged `dev`→`main` (ticket #2, program #1).

**Architecture:** The working folder's contents move to an archive; the folder is re-initialised with a gitleaks + denylist pre-push hook before the first commit. `main` gets one provenance-noted import commit (predecessor tree at a pinned SHA, re-laid out as a virtual Cargo workspace under `crates/`, mechanically and manually scrubbed of site data, licensed MIT OR Apache-2.0). `dev` starts with the Gen 2 version bump, then docs, CI, and the security baseline in small compilable commits. Nothing is compiled locally (Tier 0): the first push of `main` + `dev` (import + baseline together, as the program spec requires) is where every Rust test is proven in GitHub-hosted CI; after that, each fix is push-and-verify.

**Tech Stack:** Rust 1.98.1 (edition 2024), axum 0.8, Leptos 0.7 (CSR/WASM, trunk 0.21.14), Tauri 2 (tray), argon2 0.5 (argon2id, keyed), windows 0.61 (DPAPI), toml 0.9; Playwright 1.58.2; GitHub Actions (hosted only) with cargo-llvm-cov 0.9.1, cargo-mutants 27.1.0 + cargo-nextest 0.9.146, cargo-deny 0.20.2, gitleaks 8.30.1; Python 3.12 stdlib for repo tooling.

**Spec:** `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md` (copied in Task 4 from `~/.claude/work-products/iemmixer-gen2/12-program-spec.md`; approved 2026-09-24). Detail sources (private, not committed): archived draft `09-spec-public-draft.md` §5.1–5.4, §5.8, §5.11 (superseded where it conflicts with the program spec), `05-fact-public-repo-cicd-security.md`, `05-fact-engine-api-migration.md`, `04-parity-inventory-and-hazards.md`, `11-yagni-review.md`, private appendix `10-site-appendix-private-draft.md`.

## Global Constraints

- **Licence (D1):** `MIT OR Apache-2.0` for everything in S0; `LICENSE-MIT` + `LICENSE-APACHE` present from the first commit. Only the future `iem-limiter-mga` crate will be `GPL-3.0-or-later` (engine binary GPL).
- **P6 — site data never enters the public repo:** names, hosts, Dante channel numbers and real track names, PINs, keys, tokens, user paths. This plan itself is committed publicly: it names private values only by class and by the private file that holds them, and the two blocks fenced by `ops-only` HTML-comment markers (Task 1 Step 3, Task 13) are replaced by a pointer in the public copy (Task 4 Step 1); the full plan is kept in the private ops repo (Task 13).
- **Predecessor boundary:** never push predecessor history; nothing from this work is pushed to `zbynekdrlik/reaperiem`; predecessor maintenance (its PIN rotation, workflow neutralisation) is out of scope. Read it only via `git -C "$PRED" …` at the pinned SHA.
- **Branches:** exactly `main` and `dev`; PR `dev`→`main` with merge commits; never rewrite history or force-push.
- **Versions:** Gen 2 = `2.0.0-dev.N`, single source `[workspace.package].version` in `Cargo.toml` (+ the Cargo.lock entries); `main`'s import commit is `2.0.0-dev.0`; the first commit on `dev` bumps to `2.0.0-dev.1`; `dev` must be greater than `main` (SemVer precedence).
- **Tier 0:** no local cargo compilation (no build/test/check/clippy/run, and no third-party cargo subcommand such as `cargo deny` or `cargo mutants`). Allowed locally: `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p`, `rustup toolchain install`, Python scripts and their unittests. Rust test steps say what CI must show; until the first push (Task 15) Rust tests cannot run anywhere, and Task 15 is where they are proven.
- **Tests:** every feature ships tests that can fail; no `#[ignore]`, `.skip`, `.only`, `continue-on-error`; zero browser console errors/warnings in Playwright; coverage never decreases (`.github/coverage-floor`); diff-scoped mutation testing, every PR shard ≤ 20 min.
- **Login protection (program spec §5.3):** backoff, never lockout, capped at 60 s; per-client budgets count **failures only**, separately for LAN and tunnel; `CF-Connecting-IP` trusted only from a loopback peer; engineer-PIN failures from any member login also hit an **engineer budget**; limits apply before hashing (bounded concurrency, else 429); issued JWTs untouched.
- **PINs:** argon2id m=19456 KiB, t=2, p=1, keyed with a 32-byte pepper (DPAPI-protected on Windows, never backed up); no plaintext PIN files; no compiled-in credentials.
- **CI:** GitHub-hosted runners only (no self-hosted runner is ever registered on the public repo); every action pinned to a full commit SHA; top-level `permissions: contents: read`; `Swatinem/rust-cache` on every cargo job; fork-PR approval for all external contributors.
- **Identity:** commits as `zbynekdrlik <26905282+zbynekdrlik@users.noreply.github.com>` (repo-local config; never the box's global identity). The pre-push hook and the CI `secrets` job reject every commit whose author or committer email is not in `scripts/allowed-identities.txt` (this address, `noreply@github.com` for GitHub merges, Dependabot's bot address), and scan author/committer names and emails against the denylist; `gh pr merge` passes `--author-email` with this address. Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; PR bodies end with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
- **No manual-merge marker** in the new project `CLAUDE.md` (only the owner may add one).
- **Sessions:** Task 1 archives the predecessor's project files out from under the running session; after Task 4 the work continues in a fresh session started in `$WORK` (Task 4 Step 6). Tasks 15–16 (CI waits, merge) run in the main session, never in a subagent (a background CI waiter does not work in a subagent).
- **Owner event signals (D2) during S0:** the private S0 interim runbook (`$PRIV/event-runbook.md`, Task 1 Step 3; later the ops `CLAUDE.md`) applies from Task 1 on, whatever task is running.

## Review Focus

1. A client that reaches the server from a **non-loopback peer while sending `CF-Connecting-IP`** (a LAN attacker, or a tunnel ingress misconfigured to a LAN address) — expected: the header is ignored and the peer IP is the budget key; tests `cf_header_is_trusted_only_from_loopback` (Task 9) and `forged_cf_header_from_a_lan_peer_is_ignored` (Task 10).
2. A **corrupt, truncated or plaintext pepper / PIN-hash file** at start-up — expected: `start_server` returns an error before binding any port (fail loud; the tray exits instead of running without a server), never regenerates the pepper or ignores the file; tests `wrong_length_file_is_an_error_not_a_new_pepper` (Task 8), `a_plaintext_value_is_a_load_error`, `corrupt_json_is_a_load_error`, `start_refuses_a_plaintext_pin_store`, `start_refuses_a_corrupt_pepper` (Task 10).
3. A **distributed guessing run against the engineer PIN** from many addresses — expected: after 30 failures/hour the whole origin is spaced to 1 attempt / 5 s, LAN logins unaffected, tracker memory bounded; tests `engineer_budget_spaces_a_whole_origin_after_thirty_failures_an_hour`, `tracked_keys_are_bounded` (Task 9).
4. A **fresh install** (engineer PIN never provisioned, members without a PIN) — expected: every login is 401, there is no default PIN, and a missing hash costs the same verification work; tests `member_without_a_pin_cannot_log_in` (Task 10), `missing_hash_never_verifies` (Task 8).
5. A **site config that carries secrets or plaintext PINs** (`jwt_secret`, `vapid_private_key`, `engineer_pin`, `pins`, e.g. copied from the predecessor's YAML) — expected: a load error, never silent acceptance; tests `test_secrets_are_never_read_from_the_site_file` (Task 6), `test_plaintext_pins_are_rejected_in_the_site_file` (Task 10).
6. A **tunnel client with an IPv6 address rotating through its /64** — expected: every address of the /64 shares one per-client budget (IPv4 keys unchanged; LAN peers are IPv4 because the listeners bind `0.0.0.0`); test `ipv6_clients_share_a_budget_per_64_prefix` (Task 9).
7. **Guessing from the venue network** (by design, not a bug): more than 30 LAN failures in an hour space every LAN login of that origin to one per 5 s (the engineer budget is per origin, and any member login may carry an engineer-PIN guess); tunnel logins are unaffected; test `lan_origin_budget_spacing_is_intended` (Task 9).
8. **Parallel attempts from one client** — admission does not reserve: attempts in flight are all admitted until their failures are recorded, so the in-flight count is bounded by the hashing gate (2 running + 8 queued, else 429), and the recorded failures apply to the next attempt; tests `in_flight_attempts_are_bounded_by_the_gate` (Task 9), `hash_gate_refuses_beyond_concurrency_plus_queue` (Task 9).
9. The **raw REAPER passthrough** (`/api/reaper/*`, X10) — expected: gone in S0, 404 even with an engineer token; test `raw_reaper_passthrough_is_gone` (Task 10).
10. A **commit that carries a personal identity** (author or committer email outside `scripts/allowed-identities.txt`, or a denylisted name/email in the identity fields) — expected: the pre-push hook and the CI `secrets` job block it without printing the address; tests `test_identity_outside_the_allowed_set_is_rejected_without_printing_it`, `test_author_identity_is_scanned_against_the_denylist` (Task 1).

## Scope decisions (recorded on #2 in Task 2)

- **Import the whole predecessor server, REAPER-coupled modules included, marked for S5 — not feature-gated.** Reasons: (1) the server does not compile without them (`AppState::new` calls `proxy::collect_valid_input_indices`, `start_server` calls `poller::discover_members`/`spawn_poller`, `routes` registers proxy/preset/snapshot/backup handlers), so a trimmed import needs a throwaway server shell that S5 rewrites anyway; (2) their ~540 unit tests parse REAPER strings and run without REAPER on hosted runners, and the 53 mock Playwright tests need the real server; (3) the approved program spec budgets their deletion in S5 (“+2k/−3.6k”, “iem-server ~60 % reused”); (4) a `reaper` feature gate would touch ~40 call sites and add a second build configuration that S5 deletes, while nothing from S0 is deployed (G8, S6), so a gate protects nothing. Dropped: `tests/reaper_live.rs` (needs live REAPER) and the `integration` feature; live E2E specs (they return via HIL in S6/S7); every non-app path (docs, REAPER scripts, VST, MCP, RPP, screenshots, workflows).
- **What the predecessor's CI proved, and what it did not.** It ran `cargo test -p iem-core` and `cargo test -p iem-server --features audio,test-helpers`, clippy on the root package plus `-p iem-server --features audio`, and the mock E2E. It never ran: the TLS-gated server tests (`cfg(all(test, feature = "tls"))` in `lib.rs`), the 59 `iem-ui` unit tests, clippy with `--workspace --all-targets --all-features` or for wasm32, and anything in `src-tauri` on Windows. Its `iem-mixer/Cargo.lock` is stale (last changed 2026-03-28 at 1.121.0; p256, hkdf, aes-gcm, proptest, tracing-test and others are missing), so Task 2 Step 10 resolves dozens of crates fresh. Task 15 expects failures from exactly these never-run tests and lints and from fresh-resolved dependencies.
- **Deferred to S5 with the engine rewrite:** X7 (EQ ownership check) only — it lives in REAPER-coupled code that S5 deletes; the hardcoded ELEVATED_MEMBER becomes the placeholder `member1` until S5's `mix_view`. **X10 ships in S0:** the raw `/api/reaper/*` passthrough (one route in `routes.rs`, `reaper_proxy` there and `proxy::proxy_reaper`) is removed in Task 10 — nothing in the UI or the mock E2E uses it — so S0 carries the whole X8–X10 security baseline of spec §3.4.
- **REAPER-era track-name patterns** (`HANDn mic`, `<member> mic`/`gtr`/`kl` suffixes, one tech-input name) stay in the imported code and tests until S5 replaces them with the spec §3.1 topology; `config/test-site.toml` uses the spec's public names (`KEYS`, `CONTENT`, the stems names) wherever the imported code does not key on the REAPER-era name.
- **Mock E2E runs without REAPER:** the REAPER-era endpoints keep failing as they do (S5 deletes them); their exact console messages are declared per `describe` from one documented list (`REAPER_ABSENT` in `e2e/tests/support/fixtures.ts`, deleted in S5) instead of adding degraded modes to code S5 removes.
- **Dependabot:** alerts on; version-update PRs and automatic security-fix PRs off — both create `dependabot/*` branches, and the repo has exactly `main` and `dev`; cargo-deny advisories fail CI, and crates and action pins are updated by hand on `dev`.
- **Windows build:** plain `cargo build` of `iem-server` + `iem-tray` (no NSIS: cut by the program spec §9); only `push` runs upload the binaries (fork PRs never yield binaries, P5).

## Shell variables (paste at the start of every task)

```bash
export WORK="$HOME/devel/iemmixer"
export PRED="$HOME/devel/reaperiem"
export SHA=03be5b97deafdc5d765516014c3f59370edd534b   # reaperiem origin/main, "Merge pull request #210", 2026-09-23
export ARCHIVE="$HOME/devel/archive/iemmixer-seed-2026-09-24"
export WP="$HOME/.claude/work-products/iemmixer-gen2"
export PRIV="$HOME/.config/iemmixer"
export OPS="$HOME/devel/iemmixer-ops"
export REPO=zbynekdrlik/iemmixer
```

## File Structure

```
Cargo.toml                         virtual workspace; [workspace.package] version = single version source; profiles incl. mutants
Cargo.lock                         imported (stale since 2026-03-28), re-resolved: renamed/removed packages, dozens of crates resolved fresh
rust-toolchain.toml                pins 1.98.1 + rustfmt, clippy, llvm-tools-preview, wasm32
deny.toml                          cargo-deny policy
.cargo/mutants.toml                cargo-mutants config (profile, nextest, excludes)
.gitignore / .gitleaks.toml        hygiene
LICENSE-MIT / LICENSE-APACHE       D1
README.md / SECURITY.md / CONTRIBUTING.md / CLAUDE.md
.claude/rules/*.md                 path-scoped playbook (7 files)
.github/workflows/ci.yml           hosted CI (integrity, lint, test+coverage, wasm, e2e, windows, supply-chain, secrets, version, mutants-list on dev pushes, mutation-warmup + mutation shards on PRs)
.github/workflows/mutation-full.yml on-demand full mutation sweep (workflow_dispatch)
.github/coverage-floor             line-coverage floor (ratchet)
config/iemmixer.example.toml       every site key documented, made-up members
config/test-site.toml              synthetic site (real shape) for CI/E2E/tests
docs/provenance/import-manifest.txt TAKE manifest of the import
docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md
docs/superpowers/plans/2026-09-24-s0-bootstrap.md
scripts/denylist_scan.py (+test)   private-denylist scanner + commit-identity check (hook + CI)
scripts/allowed-identities.txt     the only author/committer emails allowed in history
scripts/denylist-allow.txt         sha256 line keys of reviewed ordinary text (starts empty)
scripts/scrub_import.py (+test)    scrubber for imports from the predecessor
scripts/check_version.py (+test)   version consistency + dev > main
scripts/check_integrity.py (+test) skips, pins, forbidden workflow constructs, force-kill (I8)
scripts/check_disposal_safety.py (+test)  imported Leptos disposal gate
crates/iem-core/                   imported; config → TOML, SiteLinks, share URL, version label; build.rs rerun key without .git
crates/iem-server/                 imported; + secrets.rs, pepper.rs (+pepper/dpapi.rs), pin_hash.rs, login_guard.rs, provision.rs;
                                   pin_store.rs rewritten; auth.rs login/change_pin rewritten; AppState::try_new; raw REAPER
                                   passthrough removed (X10); tests/pin_cli.rs
crates/iem-ui/                     imported (was iem-mixer/iem-ui); site links context, 429 text, version test id
crates/iem-tray/                   imported (was iem-mixer/src-tauri); renamed, new identifier, share URL from config
e2e/                               6 imported mock specs + support/{pins,fixtures}.ts + 3 new specs
```
Private, never in the public repo: `$PRIV/{denylist.txt,denylist-terms.txt,denylist-credentials.txt,scrub-map.tsv,site.env,forbidden-roots.txt,event-runbook.md}`, `.git/hooks/pre-push`, `$OPS` (the private ops repo; it never receives the credential entries of the denylist or the PIN rows of the scrub map).

---

### Task 1: Archive the seed folder, re-initialise, private inputs, pre-push hook

**Files:**
- Create (working tree, committed in Task 2): `scripts/denylist_scan.py`, `scripts/test_denylist_scan.py`, `scripts/allowed-identities.txt`
- Create (private): `$PRIV/denylist-terms.txt`, `$PRIV/denylist-credentials.txt`, `$PRIV/denylist.txt` (both concatenated), `$PRIV/scrub-map.tsv`, `$PRIV/site.env`, `$PRIV/forbidden-roots.txt`, `$PRIV/event-runbook.md`, `$WORK/.git/hooks/pre-push`, `~/.local/bin/gitleaks`
- Move: everything in `$WORK` → `$ARCHIVE`

**Interfaces:**
- Produces: `python3 scripts/denylist_scan.py --denylist FILE [--allow FILE] [--identities FILE] [--repo DIR] (--tree REV)... (--commits "REVLIST ARGS")...` → exit 0 clean, 1 hits, 2 usage/empty denylist; output lines `…: denylist entry N` and `… <author|committer> email is not an allowed identity`, never a term or an address. Commit mode scans each commit's author/committer names and emails together with its message. `--hash PATH LINE` prints the allowlist key `sha256(path + "\n" + line)`. Module functions `line_key(path, line)`, `compile_term(term)`, `main(argv)`.
- Produces: pre-push hook contract — blocks predecessor remotes, predecessor roots, commits by identities outside `scripts/allowed-identities.txt`, gitleaks findings, denylist hits; fails closed when the denylist, the identity list, the scanner or gitleaks is missing.

- [x] **Step 1: Move the whole folder content into the archive, record the predecessor roots, neutralise the archive's git**

The live Claude session runs in `$WORK`: its project files (`.claude/settings.json` with the PreToolUse hook `.claude/hooks/block-cargo.sh`, the predecessor `CLAUDE.md` with its manual-merge marker and REAPER/LAN rules, the MCP config) move into the archive with everything else. Until the session restarts (Task 4 Step 6) every Bash call may print a missing-hook error — expected and harmless; never recreate the archived files in `$WORK`. The session still holds the predecessor `CLAUDE.md` in context: it does not apply to the new repository.

```bash
set -euo pipefail
mkdir -p "$ARCHIVE" "$PRIV"
chmod 700 "$PRIV"
shopt -s dotglob nullglob
mv "$WORK"/* "$ARCHIVE"/
shopt -u dotglob nullglob
[ -z "$(ls -A "$WORK")" ] || { echo "working folder not empty" >&2; exit 1; }
echo "working folder empty"
# Roots of both predecessor clones, recorded before the archive's remote-tracking refs go away
{ git -C "$PRED" rev-list --max-parents=0 --all; git -C "$ARCHIVE" rev-list --max-parents=0 --all; } \
  | sort -u > "$PRIV/forbidden-roots.txt"
chmod 600 "$PRIV/forbidden-roots.txt"
wc -l < "$PRIV/forbidden-roots.txt"     # expect 2 (main/dev root + orphan backup root)
for r in $(git -C "$ARCHIVE" remote); do git -C "$ARCHIVE" remote remove "$r"; done
[ -z "$(git -C "$ARCHIVE" remote)" ] || { echo "the archive still has remotes" >&2; exit 1; }
[ -x "$ARCHIVE/.git/hooks/pre-push" ] || { echo "the archived blocking hook is missing" >&2; exit 1; }
echo "archived blocking hook kept"
chmod -R a-w "$ARCHIVE"
```
Expected: `working folder empty`, `2`, `archived blocking hook kept`. Everything later reads the archive only (the credential extraction in Step 3 reads `$PRED`, which holds the same history).

- [x] **Step 2: Install gitleaks 8.30.1 (checksum-verified)**

```bash
set -euo pipefail
tmp="$(mktemp -d)"
curl -fsSL -o "$tmp/gitleaks.tar.gz" https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_x64.tar.gz
echo "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb  $tmp/gitleaks.tar.gz" | sha256sum -c -
tar -xzf "$tmp/gitleaks.tar.gz" -C "$tmp" gitleaks
mkdir -p "$HOME/.local/bin"
install -m 0755 "$tmp/gitleaks" "$HOME/.local/bin/gitleaks"
gitleaks version
```
Expected: `…: OK`, then `8.30.1`.

- [x] **Step 3: Build the private inputs and the S0 event runbook (values come from the private appendix and the pinned tree; never paste them into any public file, issue or log)**

> *Private detail: see the iemmixer-ops copy of this plan (`docs/plans/2026-09-24-s0-bootstrap.md`).*

- [x] **Step 3b: Allowed commit identities (committed in Task 2)**

`scripts/allowed-identities.txt`:
```text
# Author and committer emails allowed in this repository's history (checked by
# scripts/denylist_scan.py --identities in the pre-push hook and in CI).
26905282+zbynekdrlik@users.noreply.github.com
# GitHub itself: committer of merges made on github.com / by `gh pr merge`
noreply@github.com
# Dependabot (alerts only today; kept so a future Dependabot PR is not blocked)
49699333+dependabot[bot]@users.noreply.github.com
```

- [x] **Step 4: Initialise the empty folder with the noreply identity and the public remote only**

```bash
set -euo pipefail
cd "$WORK"
git init -b main
git config user.name zbynekdrlik
git config user.email 26905282+zbynekdrlik@users.noreply.github.com
git config commit.gpgsign false
git remote add origin https://github.com/zbynekdrlik/iemmixer.git
git remote -v
```
Expected: only `origin https://github.com/zbynekdrlik/iemmixer.git` (fetch and push).

- [x] **Step 5: Write the failing scanner tests**

`scripts/test_denylist_scan.py`:
```python
"""Tests for scripts/denylist_scan.py (run: python3 -m unittest discover -s scripts)."""
from __future__ import annotations

import contextlib
import io
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import denylist_scan as ds  # noqa: E402

TERMS = ["zyxname", "10.9.", "ghost-host.example"]


def git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True)


class DenylistScanTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "test@example.org")
        git(self.repo, "config", "user.name", "test")
        git(self.repo, "config", "commit.gpgsign", "false")
        self.deny = self.tmp / "deny.txt"
        self.deny.write_text("# test terms\n" + "\n".join(TERMS) + "\n", encoding="utf-8")

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp)

    def commit(self, files: dict[str, str | bytes], message: str = "change") -> None:
        for rel, content in files.items():
            path = self.repo / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content if isinstance(content, bytes) else content.encode("utf-8"))
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", message)

    def scan(self, *extra: str) -> tuple[int, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = ds.main(["--denylist", str(self.deny), "--repo", str(self.repo), *extra])
        return code, out.getvalue() + err.getvalue()

    def test_clean_tree_passes(self) -> None:
        self.commit({"a.txt": "nothing private here\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)

    def test_term_in_content_is_reported_without_revealing_it(self) -> None:
        self.commit({"a.txt": "hello ZyxName!\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("a.txt:1", out)
        self.assertIn("denylist entry 1", out)
        self.assertNotIn("zyxname", out.lower())

    def test_term_inside_a_longer_word_is_not_a_hit(self) -> None:
        self.commit({"a.txt": "prezyxnamed zyxnameless\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)

    def test_underscore_does_not_hide_a_term(self) -> None:
        self.commit({"a.rs": "let x_zyxname_y = 1;\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)

    def test_letters_with_diacritics_count_as_word_characters(self) -> None:
        self.commit({"a.md": "šzyxname\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)

    def test_term_ending_in_a_dot_matches_an_address(self) -> None:
        self.commit({"a.txt": "addr 10.9.3.4\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)

    def test_term_starting_with_a_digit_needs_a_left_boundary(self) -> None:
        self.commit({"a.txt": "addr 110.9.3.4\n"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)

    def test_term_in_a_path_is_reported(self) -> None:
        self.commit({"docs/zyxname-notes.md": "x\n"})
        code, out = self.scan("--tree", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("docs/", out)
        self.assertIn("path", out)

    def test_history_hit_is_found_by_commits_mode_only(self) -> None:
        self.commit({"a.txt": "zyxname\n"})
        (self.repo / "a.txt").write_text("clean\n", encoding="utf-8")
        git(self.repo, "commit", "-q", "-am", "clean up")
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)
        self.assertEqual(self.scan("--commits", "HEAD")[0], 1)

    def test_commit_message_hit(self) -> None:
        self.commit({"a.txt": "clean\n"}, message="fix for ghost-host.example")
        code, out = self.scan("--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("commit metadata", out)

    def test_author_identity_is_scanned_against_the_denylist(self) -> None:
        git(self.repo, "config", "user.email", "zyxname@example.org")
        self.commit({"a.txt": "clean\n"})
        code, out = self.scan("--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("commit metadata", out)
        self.assertNotIn("zyxname", out.lower())

    def test_identity_outside_the_allowed_set_is_rejected_without_printing_it(self) -> None:
        ids = self.tmp / "ids.txt"
        ids.write_text("# allowed\ntest@example.org\n", encoding="utf-8")
        git(self.repo, "config", "user.email", "someone.private@example.net")
        self.commit({"a.txt": "clean\n"})
        code, out = self.scan("--identities", str(ids), "--commits", "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("author email is not an allowed identity", out)
        self.assertIn("committer email is not an allowed identity", out)
        self.assertNotIn("someone.private", out)

    def test_allowed_identities_pass(self) -> None:
        ids = self.tmp / "ids.txt"
        ids.write_text("TEST@example.org\n", encoding="utf-8")
        self.commit({"a.txt": "clean\n"})
        self.assertEqual(self.scan("--identities", str(ids), "--commits", "HEAD")[0], 0)

    def test_allowlisted_line_is_skipped(self) -> None:
        self.commit({"a.txt": "keep zyxname here\n"})
        allow = self.tmp / "allow.txt"
        allow.write_text(ds.line_key("a.txt", "keep zyxname here") + "  a.txt reviewed\n", encoding="utf-8")
        self.assertEqual(self.scan("--allow", str(allow), "--tree", "HEAD", "--commits", "HEAD")[0], 0)

    def test_binary_content_is_skipped_but_its_path_is_scanned(self) -> None:
        self.commit({"bin.dat": b"\0zyxname"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 0)
        self.commit({"zyxname.bin": b"\0x"})
        self.assertEqual(self.scan("--tree", "HEAD")[0], 1)

    def test_empty_denylist_is_a_usage_error(self) -> None:
        self.commit({"a.txt": "x\n"})
        self.deny.write_text("# nothing\n\n", encoding="utf-8")
        self.assertEqual(self.scan("--tree", "HEAD")[0], 2)

    def test_hash_mode_prints_the_line_key(self) -> None:
        self.commit({"a.txt": "one\ntwo\n"})
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = ds.main(["--repo", str(self.repo), "--hash", "a.txt", "2"])
        self.assertEqual(code, 0)
        self.assertEqual(out.getvalue().strip(), ds.line_key("a.txt", "two"))


if __name__ == "__main__":
    unittest.main()
```

- [x] **Step 6: Run the tests to verify they fail**

Run: `cd "$WORK" && PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/test_denylist_scan.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'denylist_scan'`.

- [x] **Step 7: Write the scanner**

`scripts/denylist_scan.py`:
```python
#!/usr/bin/env python3
"""Scan git content for private site data (program spec P6).

The denylist (one term per line, `#` comments) is private: a local file for
the pre-push hook, the DENYLIST secret in CI. Output never contains a term, a
matched line or an email address — only locations and the entry number.

Commit mode scans each commit's author/committer names and emails together
with its message and added lines; with `--identities FILE` it also rejects
every commit whose author or committer email is not listed there.

Matching is case-insensitive. A term that starts (ends) with a letter or digit
must not be preceded (followed) by one, where letters include diacritics and
`_` is a separator: `kit` does not hit `kitten`, `x_kit_y` is a hit, and a
term ending in `.` such as `10.0.` hits `10.0.0.5`.
"""
from __future__ import annotations

import argparse
import hashlib
import re
import shlex
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

EXIT_CLEAN = 0
EXIT_HIT = 1
EXIT_USAGE = 2


@dataclass(frozen=True)
class Hit:
    where: str
    entry: int

    def render(self) -> str:
        return f"{self.where}: denylist entry {self.entry}"


@dataclass(frozen=True)
class IdentityProblem:
    where: str
    role: str

    def render(self) -> str:
        return f"{self.where}: {self.role} email is not an allowed identity"


def load_identities(path: Path | None) -> set[str] | None:
    if path is None:
        return None
    return {
        line.strip().lower()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.strip().startswith("#")
    }


def load_terms(path: Path) -> list[str]:
    terms: list[str] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if line and not line.startswith("#"):
            terms.append(line)
    return terms


def compile_term(term: str) -> re.Pattern[str]:
    left = r"(?<![^\W_])" if term[:1].isalnum() else ""
    right = r"(?![^\W_])" if term[-1:].isalnum() else ""
    return re.compile(left + re.escape(term) + right, re.IGNORECASE)


def line_key(path: str, line: str) -> str:
    return hashlib.sha256(f"{path}\n{line}".encode("utf-8")).hexdigest()


def load_allow(path: Path | None) -> set[str]:
    if path is None or not path.exists():
        return set()
    keys: set[str] = set()
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if line and not line.startswith("#"):
            keys.add(line.split()[0])
    return keys


class Scanner:
    def __init__(self, terms: list[str], allow: set[str]) -> None:
        self.patterns = [compile_term(term) for term in terms]
        self.allow = allow

    def entries_in(self, text: str) -> list[int]:
        return [number for number, pattern in enumerate(self.patterns, start=1) if pattern.search(text)]

    def scan_path(self, path: str, prefix: str) -> list[Hit]:
        return [Hit(f"{prefix}{path}: path", entry) for entry in self.entries_in(path)]

    def scan_line(self, path: str, line: str, where: str) -> list[Hit]:
        entries = self.entries_in(line)
        if not entries or line_key(path, line) in self.allow:
            return []
        return [Hit(where, entry) for entry in entries]


def git(repo: Path, *args: str) -> bytes:
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True).stdout


def decode(data: bytes) -> str:
    return data.decode("utf-8", errors="replace")


def scan_tree(scanner: Scanner, repo: Path, rev: str) -> list[Hit]:
    hits: list[Hit] = []
    for entry in git(repo, "ls-tree", "-r", "-z", "--full-tree", rev).split(b"\0"):
        if not entry:
            continue
        meta, _, raw_path = entry.partition(b"\t")
        _mode, kind, obj = meta.split()
        path = decode(raw_path)
        hits += scanner.scan_path(path, "")
        if kind != b"blob":
            continue
        data = git(repo, "cat-file", "blob", decode(obj))
        if b"\0" in data:
            continue
        for number, line in enumerate(decode(data).splitlines(), start=1):
            hits += scanner.scan_line(path, line, f"{path}:{number}")
    return hits


def scan_commits(
    scanner: Scanner, repo: Path, revlist_args: list[str], identities: set[str] | None = None
) -> list[Hit | IdentityProblem]:
    hits: list[Hit | IdentityProblem] = []
    for sha in decode(git(repo, "rev-list", *revlist_args)).split():
        short = sha[:12]
        metadata = decode(git(repo, "show", "-s", "--format=%an%n%ae%n%cn%n%ce%n%B", sha))
        hits += [Hit(f"{short} commit metadata", entry) for entry in scanner.entries_in(metadata)]
        if identities is not None:
            emails = decode(git(repo, "show", "-s", "--format=%ae%n%ce", sha)).splitlines()
            for role, email in zip(("author", "committer"), emails):
                if email.strip().lower() not in identities:
                    hits.append(IdentityProblem(short, role))
        diff = decode(git(repo, "show", "--format=", "--unified=0", "--no-color", "--no-ext-diff",
                          "--no-renames", "-m", "--first-parent", sha))
        path = ""
        for line in diff.splitlines():
            if line.startswith("+++ "):
                target = line[4:]
                path = target[2:] if target.startswith("b/") else target
                hits += scanner.scan_path(path, f"{short} ")
            elif line.startswith("+"):
                hits += scanner.scan_line(path, line[1:], f"{short} {path}")
    return hits


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Scan git content for private site data.")
    parser.add_argument("--denylist", type=Path)
    parser.add_argument("--allow", type=Path)
    parser.add_argument("--identities", type=Path, help="allowed author/committer emails (commit mode)")
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument("--tree", action="append", default=[], metavar="REV")
    parser.add_argument("--commits", action="append", default=[], metavar="REVLIST")
    parser.add_argument("--hash", nargs=2, metavar=("PATH", "LINE"))
    args = parser.parse_args(argv)

    if args.hash:
        path, number = args.hash
        lines = (args.repo / path).read_text(encoding="utf-8").splitlines()
        print(line_key(path, lines[int(number) - 1]))
        return EXIT_CLEAN
    if args.denylist is None or not (args.tree or args.commits):
        parser.error("--denylist and at least one --tree or --commits are required")

    terms = load_terms(args.denylist)
    if not terms:
        print(f"denylist {args.denylist} has no terms", file=sys.stderr)
        return EXIT_USAGE
    identities = load_identities(args.identities)
    if identities is not None and not identities:
        print(f"identity list {args.identities} is empty", file=sys.stderr)
        return EXIT_USAGE
    scanner = Scanner(terms, load_allow(args.allow))
    hits: list[Hit | IdentityProblem] = []
    for rev in args.tree:
        hits += scan_tree(scanner, args.repo, rev)
    for spec in args.commits:
        hits += scan_commits(scanner, args.repo, shlex.split(spec), identities)
    for hit in hits:
        print(hit.render())
    if hits:
        print(f"{len(hits)} finding(s)", file=sys.stderr)
        return EXIT_HIT
    print("denylist: clean")
    return EXIT_CLEAN


if __name__ == "__main__":
    sys.exit(main())
```

- [x] **Step 8: Run the tests to verify they pass**

Run: `cd "$WORK" && PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/test_denylist_scan.py -v`
Expected: `Ran 17 tests … OK`.

- [x] **Step 9: Install the pre-push hook (before any commit exists)**

`$WORK/.git/hooks/pre-push` (mode 0755; local, never versioned):
```bash
#!/usr/bin/env bash
# iemmixer pre-push guard (local, not versioned). Blocks pushes to the
# predecessor, predecessor history, commits by identities outside
# scripts/allowed-identities.txt, gitleaks findings and denylist hits.
# Fails closed when any input is missing.
set -euo pipefail

remote_name="$1"
remote_url="$2"
repo_root="$(git rev-parse --show-toplevel)"
denylist="${IEMMIXER_DENYLIST:-$HOME/.config/iemmixer/denylist.txt}"
roots_file="${IEMMIXER_FORBIDDEN_ROOTS:-$HOME/.config/iemmixer/forbidden-roots.txt}"
scanner="$repo_root/scripts/denylist_scan.py"
allow="$repo_root/scripts/denylist-allow.txt"
identities="$repo_root/scripts/allowed-identities.txt"

if [[ "$remote_url" == *reaperiem* ]]; then
  echo "BLOCKED: nothing is ever pushed to the predecessor ($remote_name)." >&2
  exit 1
fi
[[ -s "$denylist" ]] || { echo "BLOCKED: denylist missing or empty: $denylist" >&2; exit 1; }
[[ -s "$roots_file" ]] || { echo "BLOCKED: forbidden-roots list missing: $roots_file" >&2; exit 1; }
[[ -f "$scanner" ]] || { echo "BLOCKED: $scanner missing" >&2; exit 1; }
[[ -s "$identities" ]] || { echo "BLOCKED: $identities missing or empty" >&2; exit 1; }
command -v gitleaks > /dev/null || { echo "BLOCKED: gitleaks not installed" >&2; exit 1; }

zero=0000000000000000000000000000000000000000
while read -r local_ref local_sha _remote_ref remote_sha; do
  [[ "$local_sha" == "$zero" ]] && continue
  for root in $(git rev-list --max-parents=0 "$local_sha"); do
    if grep -qx "$root" "$roots_file"; then
      echo "BLOCKED: $local_ref contains predecessor history (root $root)." >&2
      exit 1
    fi
  done
  if [[ "$remote_sha" == "$zero" ]]; then
    range="$local_sha --not --remotes=$remote_name"
  else
    range="$remote_sha..$local_sha"
  fi
  python3 "$scanner" --repo "$repo_root" --denylist "$denylist" --allow "$allow" \
    --identities "$identities" --tree "$local_sha" --commits "$range" \
    || { echo "BLOCKED: denylist or identity findings in $local_ref (see above)." >&2; exit 1; }
  gitleaks git --no-banner --redact --log-opts="$range" "$repo_root" \
    || { echo "BLOCKED: gitleaks findings in $local_ref." >&2; exit 1; }
done
exit 0
```
```bash
chmod 0755 "$WORK/.git/hooks/pre-push"
```

- [x] **Step 10: Prove the hook blocks and passes (scratch repo, synthetic term)**

```bash
set -euo pipefail
t="$(mktemp -d)"
git init -q --bare "$t/remote.git"
git init -q -b main "$t/work"
git -C "$t/work" config user.email test@example.org; git -C "$t/work" config user.name test
git -C "$t/work" remote add origin "$t/remote.git"
mkdir -p "$t/work/scripts"; cp "$WORK/scripts/denylist_scan.py" "$t/work/scripts/"
printf 'test@example.org\n' > "$t/work/scripts/allowed-identities.txt"
cp "$WORK/.git/hooks/pre-push" "$t/work/.git/hooks/pre-push"
printf 'zyxprivateterm\n' > "$t/deny.txt"; printf '%s\n' 0000000000000000000000000000000000000001 > "$t/roots.txt"
export IEMMIXER_DENYLIST="$t/deny.txt" IEMMIXER_FORBIDDEN_ROOTS="$t/roots.txt"
echo "zyxprivateterm" > "$t/work/leak.txt"; git -C "$t/work" add -A; git -C "$t/work" commit -qm leak
if git -C "$t/work" push -q origin main; then echo "UNEXPECTED: leak pushed"; exit 1; else echo "blocked as expected"; fi
git -C "$t/work" rm -q leak.txt; echo "clean" > "$t/work/ok.txt"; git -C "$t/work" add -A; git -C "$t/work" commit -qm clean
if git -C "$t/work" push -q origin main; then echo "UNEXPECTED: history leak pushed"; exit 1; else echo "history blocked as expected"; fi
new_clean_repo() {
  git init -q -b main "$1"; git -C "$1" config user.email "$2"; git -C "$1" config user.name test
  git -C "$1" remote add origin "$t/remote.git"; mkdir -p "$1/scripts"
  cp "$WORK/scripts/denylist_scan.py" "$1/scripts/"; cp "$WORK/.git/hooks/pre-push" "$1/.git/hooks/"
  printf 'test@example.org\n' > "$1/scripts/allowed-identities.txt"
  echo ok > "$1/ok.txt"; git -C "$1" add -A; git -C "$1" commit -qm ok
}
new_clean_repo "$t/stranger" someone@example.net
if git -C "$t/stranger" push -q origin main; then echo "UNEXPECTED: foreign identity pushed"; exit 1; else echo "identity blocked as expected"; fi
new_clean_repo "$t/clean" test@example.org
git -C "$t/clean" push -q origin main || { echo "UNEXPECTED: clean push blocked" >&2; exit 1; }
echo "clean push passed"
unset IEMMIXER_DENYLIST IEMMIXER_FORBIDDEN_ROOTS
```
Expected: `blocked as expected`, `history blocked as expected`, `identity blocked as expected`, `clean push passed`.

No commit in this task (the scanner files and `scripts/allowed-identities.txt` are committed in Task 2).

---

### Task 2: Import commit on `main` (TAKE manifest, layout, scrub, licence)

**Files:**
- Create: `Cargo.toml`, `.gitignore`, `.gitleaks.toml`, `LICENSE-MIT`, `LICENSE-APACHE`, `README.md`, `docs/provenance/import-manifest.txt`, `scripts/scrub_import.py`, `scripts/test_scrub_import.py`, `e2e/tests/support/pins.ts`
- Import (from `$SHA`, re-laid out): `Cargo.lock`, `crates/iem-core/**`, `crates/iem-server/**` (minus `tests/reaper_live.rs`), `crates/iem-ui/**` (minus `.gitignore`), `crates/iem-tray/**` (was `iem-mixer/src-tauri`), `e2e/{package.json,package-lock.json,playwright.config.ts}`, `e2e/tests/{smoke,auth-security,member-photo,auto-redirect,login-keyboard,pwa}.spec.ts`, `scripts/{check_disposal_safety.py,test_check_disposal_safety.py}`
- Commit (written in Task 1): `scripts/denylist_scan.py`, `scripts/test_denylist_scan.py`, `scripts/allowed-identities.txt`
- Modify (manual scrub): `crates/iem-core/src/config.rs`, `crates/iem-server/src/auth.rs`, `crates/iem-server/src/lib.rs`, `crates/iem-server/src/tunnel_watch.rs`, `crates/iem-server/src/{proxy,poller}.rs` (Dante literals), six REAPER-coupled module headers, all four crate manifests, `crates/iem-tray/{build.rs,tauri.conf.json,src/main.rs,src/lib.rs}`, `e2e/tests/login-keyboard.spec.ts`, `scripts/check_disposal_safety.py`

**Interfaces:**
- Consumes: `scripts/denylist_scan.py` (Task 1), `$PRIV/scrub-map.tsv`, `$PRIV/denylist.txt`.
- Produces: `python3 scripts/scrub_import.py --map FILE --root DIR --report FILE` (kinds `literal`, `word`, `ts-ident`; rule 0 = predecessor issue refs `#N` → `reaperiem#N`); layout `crates/{iem-core,iem-server,iem-ui,iem-tray}`; crate `iem-tray` (lib `iem_tray`, bin `iem-tray`); `e2e/tests/support/pins.ts` exports `ENGINEER_PIN`, `MEMBER_PIN`; post-scrub placeholders as listed in Task 1 Step 3 and `.claude/rules/public-repo-hygiene.md` (later tasks quote them); Dante literals only from the synthetic map (Step 8 gate).

- [x] **Step 1: Write the failing scrub-tool tests**

`scripts/test_scrub_import.py`:
```python
"""Tests for scripts/scrub_import.py."""
from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import scrub_import as si  # noqa: E402

MAP = (
    "ts-ident\t4321\tMEMBER_PIN\n"
    "literal\tghost.example\tmixer.example.org\n"
    "word\tzyxname\tmember1\n"
    "word\t4321\t<PIN>\n"
)


class ScrubTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp())
        self.root = self.tmp / "tree"
        self.root.mkdir()
        self.map = self.tmp / "map.tsv"
        self.map.write_text(MAP, encoding="utf-8")
        self.report = self.tmp / "report.tsv"

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp)

    def write(self, rel: str, content: str | bytes) -> Path:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content if isinstance(content, bytes) else content.encode("utf-8"))
        return path

    def run_scrub(self) -> int:
        return si.main(["--map", str(self.map), "--root", str(self.root), "--report", str(self.report)])

    def test_word_rule_keeps_case_style_and_boundaries(self) -> None:
        path = self.write("a.rs", 'let t = "ZYXNAME inear"; // zyxname\nlet n = "Zyxname"; let k = prezyxname; let u = x_zyxname;\n')
        self.assertEqual(self.run_scrub(), 0)
        self.assertEqual(
            path.read_text(encoding="utf-8"),
            'let t = "MEMBER1 inear"; // member1\nlet n = "Member1"; let k = prezyxname; let u = x_member1;\n',
        )

    def test_literal_rule_is_case_insensitive(self) -> None:
        path = self.write("b.rs", 'const H: &str = "GHOST.example";\n')
        self.run_scrub()
        self.assertEqual(path.read_text(encoding="utf-8"), 'const H: &str = "mixer.example.org";\n')

    def test_ts_ident_replaces_quoted_pin_and_adds_the_import(self) -> None:
        path = self.write("tests/x.spec.ts", 'import { test } from "@playwright/test";\nconst a = "4321"; const b = \'4321\';\n// default PIN 4321\n')
        self.run_scrub()
        self.assertEqual(
            path.read_text(encoding="utf-8"),
            'import { MEMBER_PIN } from "./support/pins";\nimport { test } from "@playwright/test";\n'
            "const a = MEMBER_PIN; const b = MEMBER_PIN;\n// default PIN <PIN>\n",
        )

    def test_quoted_pin_outside_typescript_becomes_the_word_placeholder(self) -> None:
        path = self.write("c.rs", 'let pin = Some("4321".to_string());\n')
        self.run_scrub()
        self.assertEqual(path.read_text(encoding="utf-8"), 'let pin = Some("<PIN>".to_string());\n')

    def test_issue_refs_are_rewritten_in_comments_and_markdown_only(self) -> None:
        rs = self.write("d.rs", 'let c = "#000"; // see #179 and reaperiem#12\n')
        md = self.write("e.md", "Fixed in #202.\n# Heading\n")
        css = self.write("f.css", "a { color: #000; }\n")
        self.run_scrub()
        self.assertEqual(rs.read_text(encoding="utf-8"), 'let c = "#000"; // see reaperiem#179 and reaperiem#12\n')
        self.assertEqual(md.read_text(encoding="utf-8"), "Fixed in reaperiem#202.\n# Heading\n")
        self.assertEqual(css.read_text(encoding="utf-8"), "a { color: #000; }\n")

    def test_binary_files_are_untouched(self) -> None:
        path = self.write("g.bin", b"\0zyxname")
        self.run_scrub()
        self.assertEqual(path.read_bytes(), b"\0zyxname")

    def test_a_private_path_aborts(self) -> None:
        self.write("zyxname-notes.md", "x\n")
        with self.assertRaises(SystemExit):
            self.run_scrub()

    def test_malformed_map_is_rejected(self) -> None:
        self.map.write_text("word\tonly-two-columns\n", encoding="utf-8")
        with self.assertRaises(ValueError):
            self.run_scrub()

    def test_report_names_rules_not_values(self) -> None:
        self.write("a.rs", "zyxname\n")
        self.run_scrub()
        report = self.report.read_text(encoding="utf-8")
        self.assertIn("a.rs\t3\t1", report)
        self.assertNotIn("zyxname", report)


if __name__ == "__main__":
    unittest.main()
```

- [x] **Step 2: Run the tests to verify they fail**

Run: `cd "$WORK" && PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/test_scrub_import.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'scrub_import'`.

- [x] **Step 3: Write the scrub tool**

`scripts/scrub_import.py`:
```python
#!/usr/bin/env python3
"""Scrub an imported tree of site data (program spec P6).

Used for every import from the private predecessor at a pinned SHA. The map
file is private (tab-separated `kind private public`, applied in order):

  literal   case-insensitive substring
  word      case-insensitive whole word (letters incl. diacritics and digits;
            `_` separates); the case style of each match is kept
  ts-ident  in *.ts only: "private" / 'private' -> IDENT, plus
            `import { IDENT } from "./support/pins";` at the top of the file

Also rewrites predecessor issue references (#123 -> reaperiem#123) in *.md
files and in the `//` comment part of *.rs, *.ts and *.js lines (rule 0).
Binary files are skipped; a path matching a literal/word rule aborts (fix the
TAKE manifest). The report lists path, rule number and count, never a value.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

KINDS = {"literal", "word", "ts-ident"}
ISSUE_REF = re.compile(r"(?<![\w/#-])#(\d{1,4})\b")
COMMENT_SUFFIXES = {".rs", ".ts", ".js"}
PINS_MODULE = "./support/pins"


@dataclass(frozen=True)
class Rule:
    number: int
    kind: str
    private: str
    public: str


def load_rules(path: Path) -> list[Rule]:
    rules: list[Rule] = []
    for line_no, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not raw.strip() or raw.startswith("#"):
            continue
        parts = raw.split("\t")
        if len(parts) != 3 or parts[0] not in KINDS or not parts[1]:
            raise ValueError(f"map line {line_no}: expected kind<TAB>private<TAB>public")
        rules.append(Rule(len(rules) + 1, parts[0], parts[1], parts[2]))
    return rules


def styled(match: str, public: str) -> str:
    if match.isupper():
        return public.upper()
    if match[:1].isupper():
        return public[:1].upper() + public[1:]
    return public


def word_pattern(private: str) -> re.Pattern[str]:
    left = r"(?<![^\W_])" if private[:1].isalnum() else ""
    right = r"(?![^\W_])" if private[-1:].isalnum() else ""
    return re.compile(left + re.escape(private) + right, re.IGNORECASE)


def apply_rule(rule: Rule, text: str, suffix: str) -> tuple[str, int]:
    if rule.kind == "literal":
        return re.subn(re.escape(rule.private), lambda _m: rule.public, text, flags=re.IGNORECASE)
    if rule.kind == "word":
        return word_pattern(rule.private).subn(lambda m: styled(m.group(0), rule.public), text)
    if suffix != ".ts":
        return text, 0
    quoted = re.compile("([\"'])" + re.escape(rule.private) + r"\1")
    return quoted.subn(lambda _m: rule.public, text)


def rewrite_issue_refs(text: str, suffix: str) -> tuple[str, int]:
    if suffix == ".md":
        return ISSUE_REF.subn(r"reaperiem#\1", text)
    if suffix not in COMMENT_SUFFIXES:
        return text, 0
    total = 0
    out: list[str] = []
    for line in text.splitlines(keepends=True):
        code, sep, comment = line.partition("//")
        if sep:
            comment, count = ISSUE_REF.subn(r"reaperiem#\1", comment)
            total += count
            line = code + sep + comment
        out.append(line)
    return "".join(out), total


def scrub_file(path: Path, rules: list[Rule]) -> list[tuple[int, int]]:
    data = path.read_bytes()
    if b"\0" in data:
        return []
    text = data.decode("utf-8")
    counts: list[tuple[int, int]] = []
    idents: set[str] = set()
    for rule in rules:
        text, count = apply_rule(rule, text, path.suffix)
        if count:
            counts.append((rule.number, count))
            if rule.kind == "ts-ident":
                idents.add(rule.public)
    text, count = rewrite_issue_refs(text, path.suffix)
    if count:
        counts.append((0, count))
    if idents:
        text = f'import {{ {", ".join(sorted(idents))} }} from "{PINS_MODULE}";\n' + text
    if counts:
        path.write_text(text, encoding="utf-8")
    return counts


def scrub_tree(root: Path, rules: list[Rule]) -> dict[str, list[tuple[int, int]]]:
    path_rules = [rule for rule in rules if rule.kind in {"literal", "word"}]
    report: dict[str, list[tuple[int, int]]] = {}
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        rel = path.relative_to(root)
        if ".git" in rel.parts or "node_modules" in rel.parts:
            continue
        rel_text = rel.as_posix()
        for rule in path_rules:
            if apply_rule(rule, rel_text, "")[1]:
                raise SystemExit(f"path {rel_text} matches map rule {rule.number}: drop it from the TAKE manifest")
        counts = scrub_file(path, rules)
        if counts:
            report[rel_text] = counts
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Scrub an imported tree of site data.")
    parser.add_argument("--map", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args(argv)
    rules = load_rules(args.map)
    if not rules:
        print("scrub map has no rules", file=sys.stderr)
        return 2
    report = scrub_tree(args.root, rules)
    with args.report.open("w", encoding="utf-8") as out:
        out.write("path\trule\tcount\n")
        for rel, counts in report.items():
            for number, count in counts:
                out.write(f"{rel}\t{number}\t{count}\n")
    total = sum(count for counts in report.values() for _, count in counts)
    print(f"scrubbed {total} occurrence(s) in {len(report)} file(s); report: {args.report}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [x] **Step 4: Run the tests to verify they pass**

Run: `cd "$WORK" && PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/test_scrub_import.py -v`
Expected: `Ran 9 tests … OK`.

- [x] **Step 5: Extract the TAKE manifest from the pinned SHA and lay it out**

```bash
set -euo pipefail
cd "$WORK"
STAGE="$(mktemp -d)"
git -C "$PRED" archive --format=tar "$SHA" -- \
  iem-mixer/Cargo.lock \
  iem-mixer/crates/iem-core iem-mixer/crates/iem-server \
  iem-mixer/iem-ui iem-mixer/src-tauri \
  iem-mixer/e2e/package.json iem-mixer/e2e/package-lock.json iem-mixer/e2e/playwright.config.ts \
  iem-mixer/e2e/tests/smoke.spec.ts iem-mixer/e2e/tests/auth-security.spec.ts \
  iem-mixer/e2e/tests/member-photo.spec.ts iem-mixer/e2e/tests/auto-redirect.spec.ts \
  iem-mixer/e2e/tests/login-keyboard.spec.ts iem-mixer/e2e/tests/pwa.spec.ts \
  scripts/check_disposal_safety.py scripts/test_check_disposal_safety.py \
  | tar -x -C "$STAGE"
rm "$STAGE/iem-mixer/crates/iem-server/tests/reaper_live.rs" "$STAGE/iem-mixer/iem-ui/.gitignore"
mkdir -p crates e2e/tests scripts
mv "$STAGE/iem-mixer/crates/iem-core" crates/iem-core
mv "$STAGE/iem-mixer/crates/iem-server" crates/iem-server
mv "$STAGE/iem-mixer/iem-ui" crates/iem-ui
mv "$STAGE/iem-mixer/src-tauri" crates/iem-tray
mv "$STAGE/iem-mixer/Cargo.lock" Cargo.lock
mv "$STAGE/iem-mixer/e2e/package.json" "$STAGE/iem-mixer/e2e/package-lock.json" "$STAGE/iem-mixer/e2e/playwright.config.ts" e2e/
mv "$STAGE"/iem-mixer/e2e/tests/*.spec.ts e2e/tests/
mv "$STAGE/scripts/check_disposal_safety.py" "$STAGE/scripts/test_check_disposal_safety.py" scripts/
find "$STAGE" -type f | wc -l     # expect 0: everything extracted was placed
rm -r "$STAGE"
```

- [x] **Step 6: Write the workspace manifest and fix the four crate manifests**

`Cargo.toml`:
```toml
[workspace]
members = ["crates/iem-core", "crates/iem-server", "crates/iem-ui", "crates/iem-tray"]
resolver = "2"

[workspace.package]
version = "2.0.0-dev.0"
edition = "2024"
authors = ["iemmixer contributors"]
license = "MIT OR Apache-2.0"
repository = "https://github.com/zbynekdrlik/iemmixer"

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true

[profile.dev]
opt-level = 1
```

In each of `crates/iem-core/Cargo.toml`, `crates/iem-server/Cargo.toml`, `crates/iem-ui/Cargo.toml`, replace the `version`, `edition`, `authors`, `license` lines of `[package]` with:
```toml
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
```
and set the descriptions: iem-core `"Core types, configuration and protocol for iemmixer"`, iem-server `"HTTPS/WebSocket server of iemmixer"`, iem-ui `"Leptos WASM web UI of iemmixer"`. In `crates/iem-ui/Cargo.toml` change `iem-core = { path = "../crates/iem-core", default-features = false }` to `iem-core = { path = "../iem-core", default-features = false }`. In `crates/iem-server/Cargo.toml` delete the line `integration = []  # gates live REAPER integration tests in tests/reaper_live.rs`.

`crates/iem-tray/Cargo.toml` (full content):
```toml
[package]
name = "iem-tray"
version.workspace = true
edition.workspace = true
authors.workspace = true
license.workspace = true
repository.workspace = true
description = "iemmixer Windows tray shell (Tauri 2)"
default-run = "iem-tray"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
tauri = { version = "2", features = ["tray-icon"] }
tauri-plugin-shell = "2"
tauri-plugin-single-instance = "2"
iem-server = { path = "../iem-server", features = ["tls", "audio"] }
iem-core = { path = "../iem-core" }
tokio = { version = "1", features = ["rt-multi-thread", "sync", "time", "macros"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
tracing-appender = "0.2"
dirs = "6.0"
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

[[bin]]
name = "iem-tray"
path = "src/main.rs"
```

- [x] **Step 7: Manual scrub and identity edits (items 1–12 before the mechanical scrub, item 13 right after it)**

1. `crates/iem-core/src/config.rs`: delete `DEFAULT_MEMBER_PIN` and `DEFAULT_ENGINEER_PIN` with their doc comments; delete `pub fn validate_pin` (dead code outside tests) and `pub enum PinValidation` with its doc comment; delete the tests `test_pin_validation`, `test_default_pin_required_when_no_pin_configured`, the test whose name starts with `test_engineer_pin_default_`, `test_engineer_pin_config_overrides_default` and `test_config_pin_overrides_default` (keep `make_test_member`).
2. `crates/iem-server/src/auth.rs`, `login`: replace the block from the comment `// 1. Check engineer PIN` through the line `if constant_time_eq(&req.pin, eng_pin) {` with
```rust
    // 1. Check the engineer PIN — only a configured one; there is no default.
    if let Some(eng_pin) = config.engineer_pin.as_deref()
        && constant_time_eq(&req.pin, eng_pin)
    {
```
   replace the comment line starting `// 3. Fall through to config validation` with `// 3. No stored PIN matched.`; delete the comment `// Check default member PIN` and the `if constant_time_eq(&req.pin, iem_core::config::DEFAULT_MEMBER_PIN) { … }` block below it. In `change_pin`, replace `constant_time_eq(old_pin, iem_core::config::DEFAULT_MEMBER_PIN)` with `false`.
3. `crates/iem-server/src/lib.rs`: `#[folder = "../../iem-ui/dist/"]` → `#[folder = "../iem-ui/dist/"]`; in the TLS block `.join("iem-mixer")` → `.join("iemmixer")`.
4. Prepend this line to `crates/iem-server/src/{proxy,poller,backup_capture,backup_restore,preset_routes,snapshot_routes}.rs` (each file starts with `//!` doc lines):
```rust
//! S5: REAPER control plane — replaced by the engine client (program spec §6 S5); imported only so the server builds and its tests run.
```
5. `crates/iem-server/src/tunnel_watch.rs` module doc: replace the four lines starting `//! The app runs unelevated as` through `//! deploy job).` with
```rust
//! The app runs unelevated; granting it start/stop rights on the
//! `cloudflared` service is a one-time elevated setup step owned by S6.
```
6. `crates/iem-tray/src/main.rs`: `iem_mixer_app::run();` → `iem_tray::run();`.
7. `crates/iem-tray/build.rs`: `.join("../.git/HEAD")` → `.join("../../.git/HEAD")`.
8. `crates/iem-tray/src/lib.rs`: both `.join("iem-mixer")` → `.join("iemmixer")`; `"iem-mixer.log"` → `"iemmixer.log"`; `"iem_mixer=debug"` → `"iem_tray=debug"`; `"error while running IEM Mixer"` → `"error while running the iemmixer tray"`.
9. `crates/iem-tray/tauri.conf.json` (full content; no `version` — Cargo is the single source; bundling off, program spec cuts NSIS):
```json
{
  "$schema": "https://raw.githubusercontent.com/tauri-apps/tauri/tauri-v2.0.0/crates/tauri-utils/schema.json",
  "productName": "iemmixer",
  "identifier": "io.github.zbynekdrlik.iemmixer",
  "build": {
    "frontendDist": "../iem-ui/dist",
    "devUrl": "http://localhost:80"
  },
  "app": {
    "withGlobalTauri": false,
    "windows": [
      {
        "title": "iemmixer",
        "url": "http://localhost:80",
        "width": 800,
        "height": 600,
        "visible": false,
        "resizable": true,
        "minWidth": 400,
        "minHeight": 400
      }
    ]
  },
  "bundle": {
    "active": false,
    "icon": ["icons/32x32.png", "icons/128x128.png", "icons/headphones.ico", "icons/icon.png"],
    "createUpdaterArtifacts": false
  }
}
```
10. `scripts/check_disposal_safety.py`: `SCAN_ROOT = REPO_ROOT / "iem-mixer" / "iem-ui" / "src"` → `SCAN_ROOT = REPO_ROOT / "crates" / "iem-ui" / "src"`.
11. `e2e/tests/login-keyboard.spec.ts`, test `full keyboard PIN entry triggers auto-submit`: replace the comment line starting `// Type 4 digits (default PIN` and the four `await page.keyboard.press(...)` lines below it with
```ts
    // Type the member PIN — four digits auto-submit and redirect
    for (const digit of MEMBER_PIN) {
      await page.keyboard.press(digit);
    }
```
   and add `import { MEMBER_PIN } from "./support/pins";` as the file's first line. In the test `can enter PIN digits using keyboard number keys`, replace the digit of each of its three `page.keyboard.press("<digit>")` calls with `"4"` (they pressed leading digits of the predecessor's default member PIN; the test only counts dots).
12. `e2e/tests/support/pins.ts` (new):
```ts
/**
 * E2E PINs come from the environment: CI generates them per run and
 * provisions them with `iem-server pin …`. No credential is committed.
 */
function requiredPin(name: string): string {
  const value = process.env[name];
  if (!value || !/^\d{4}$/.test(value)) {
    throw new Error(`${name} must be a 4-digit PIN (set by the e2e job in .github/workflows/ci.yml)`);
  }
  return value;
}

export const ENGINEER_PIN = requiredPin("E2E_ENGINEER_PIN");
export const MEMBER_PIN = requiredPin("E2E_MEMBER_PIN");
```
13. **Dante channel numbers (P6):** the imported code keeps ~56 real Dante channel literals in doc comments, tests and fixtures — `crates/iem-core/src/config.rs` (the `dante_outputs` doc example, the tests from `make_test_member` on), `crates/iem-server/src/proxy.rs` and `crates/iem-server/src/poller.rs` — and the scrub keeps the role order, so `memberN` plus those numbers would reproduce the private per-role TX map. Rewrite every `dante_output_l` / `dante_output_r` / `dante_input` literal and every `[a, b]` Dante pair (map inserts, YAML/TOML test strings, `Some(&[a, b])` assertions) to the synthetic map of `config/test-site.toml`, consistently within each test: member N → `[69+2N, 70+2N]` (member1 = 71/72 … member9 = 87/88), engineer → `[91, 92]`, any other output (e.g. a translator or a made-up track) → `[89, 90]`, input k → `100+k` (101–124); the doc example becomes `"MEMBER1" -> [71, 72]`. Do this after Step 8's mechanical scrub has renamed the members (the names tell the role), then re-run Step 8's Dante gate.

- [x] **Step 8: Mechanical scrub, then confirm nothing private is left**

```bash
set -euo pipefail
cd "$WORK"
report="$(mktemp)"
PYTHONDONTWRITEBYTECODE=1 python3 scripts/scrub_import.py --map "$PRIV/scrub-map.tsv" --root "$WORK" --report "$report"
column -t -s $'\t' "$report" | head -80
```
Expected: a report with rule numbers and counts only. The scrub leaves the Rust code consistent (identifiers and strings change together). Known follow-up: tests that compare sorted member lists may now order differently — Task 15 fixes only those expectations.

Review by hand: every row of the private map marked `# also an ordinary word` (Task 1 Step 3) — for its rule number, open each file the report lists and check that every replacement is an id, never prose (UI or comment text); restore prose by hand.

Then apply Step 7 item 13 and run the Dante gate:
```bash
set -euo pipefail
cd "$WORK"
python3 - <<'EOF'
import re, subprocess
SYNTHETIC = set(range(71, 93)) | set(range(101, 125))
PATTERN = r'dante_|^[[:space:]]*"?[A-Za-z0-9_]+"?[[:space:]]*[:=][[:space:]]*\[[0-9]{1,3}, ?[0-9]{1,3}\]'
grep = subprocess.run(["git", "grep", "-n", "--no-index", "-E", PATTERN, "--", "crates", "e2e"],
                      capture_output=True, text=True)
if grep.returncode not in (0, 1):
    raise SystemExit(grep.stderr)
bad = set()
for line in grep.stdout.splitlines():
    path, number, text = line.split(":", 2)
    nums = [int(n) for n in re.findall(r'dante_(?:output_[lr]|input)"?\s*[:=,]\s*(\d+)', text)]
    nums += [int(n) for pair in re.findall(r"\[(\d{1,3}),\s?(\d{1,3})\]", text) for n in pair]
    if any(n not in SYNTHETIC for n in nums):
        bad.add(f"{path}:{number}")
print("\n".join(sorted(bad)) or "dante literals: synthetic only")
raise SystemExit(1 if bad else 0)
EOF
```
Expected: `dante literals: synthetic only` (outputs 71–92, inputs 101–124). A listed line is a real channel number left over: rewrite it per item 13. Vectors of track indices (`pinned: vec![…]`, `soloed: vec![…]`) are not Dante numbers and are not matched.

- [x] **Step 9: Licence files, README, provenance manifest, hygiene files**

```bash
set -euo pipefail
cd "$WORK"
curl -fsSL https://www.apache.org/licenses/LICENSE-2.0.txt -o LICENSE-APACHE
echo "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30  LICENSE-APACHE" | sha256sum -c -
```

`LICENSE-MIT`:
```text
MIT License

Copyright (c) 2026 the iemmixer contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

`README.md`:
```markdown
# iemmixer

A personal in-ear-monitor (IEM) mixer for a church band: every musician mixes their own in-ear feed from a phone. Gen 2 replaces the REAPER-based predecessor with a native Rust audio engine; until the cutover the predecessor serves every event.

**Status:** S0 (bootstrap). The imported web app — server, Leptos UI, Windows tray — builds and is tested in CI; the engine arrives in S1–S3. See the program spec in `docs/superpowers/specs/`.

## Layout

| Path | Contents |
|---|---|
| `crates/iem-core` | Shared types, site config, protocol (WASM-safe) |
| `crates/iem-server` | HTTPS/WebSocket server, login protection, push, backups (REAPER control plane until S5) |
| `crates/iem-ui` | Leptos progressive web app |
| `crates/iem-tray` | Windows tray shell (Tauri 2) |
| `e2e` | Playwright browser tests against a synthetic site |
| `config` | `iemmixer.example.toml` (every key documented), `test-site.toml` (synthetic site for CI) |
| `scripts` | CI and repository-security tooling |

## Building and testing

All compilation and tests run in GitHub Actions on hosted runners (`.github/workflows/ci.yml`): formatting and clippy, unit tests with a coverage floor, the WASM build, browser E2E with a clean console, diff-scoped mutation testing, supply-chain and secret scans, and a Windows build.

## Security

See `SECURITY.md`. No credentials are compiled in; PINs are argon2id hashes keyed with a DPAPI-protected pepper; logins are rate-limited per client and origin and never locked out.

## Provenance

Imported from the private predecessor repository `zbynekdrlik/reaperiem` at commit `03be5b97deafdc5d765516014c3f59370edd534b`, scrubbed of site data; its history stays private (`docs/provenance/import-manifest.txt`).

## License

Licensed under either of `LICENSE-APACHE` or `LICENSE-MIT` at your option. A future limiter crate (a port of a GPL-3.0-or-later JSFX limiter) will be GPL-3.0-or-later and make the engine binary GPL (program spec D1).
```

`docs/provenance/import-manifest.txt`:
```text
Source: zbynekdrlik/reaperiem (private) at 03be5b97deafdc5d765516014c3f59370edd534b (origin/main, 2026-09-23)
Method: git archive of the paths below, then scripts/scrub_import.py with the private map, manual scrub (S0 plan Task 2 Step 7:
identities, removed default PINs, Dante channel numbers rewritten to the synthetic map), denylist + gitleaks scans.
Lockfile: the predecessor's iem-mixer/Cargo.lock was stale (last changed 2026-03-28 at 1.121.0); the import re-resolves it with
`cargo metadata`, so dozens of crates are resolved fresh at import time rather than taken from the predecessor's tested set.

TAKE (source -> destination)
iem-mixer/Cargo.lock                         -> Cargo.lock
iem-mixer/crates/iem-core/                   -> crates/iem-core/
iem-mixer/crates/iem-server/                 -> crates/iem-server/   (without tests/reaper_live.rs)
iem-mixer/iem-ui/                            -> crates/iem-ui/       (without .gitignore)
iem-mixer/src-tauri/                         -> crates/iem-tray/
iem-mixer/e2e/{package.json,package-lock.json,playwright.config.ts} -> e2e/
iem-mixer/e2e/tests/{smoke,auth-security,member-photo,auto-redirect,login-keyboard,pwa}.spec.ts -> e2e/tests/
scripts/{check_disposal_safety.py,test_check_disposal_safety.py} -> scripts/

DROP (everything else), by class: root workspace package and README, app config files with site data,
live E2E specs and credential fixtures, the VST plugin and its submodule, MCP server, REAPER scripts and JSFX,
REAPER projects, screenshots, legacy web pages, docs and plans, workflows, repo-level config and hooks.
Kept for later imports from the same SHA: live E2E specs (S6/S7 via HIL), Windows service setup script (S6),
REAPER scripts and projects as references for the importer/golden generator (S1b/S4).
```

`.gitignore`:
```gitignore
/target/
/crates/iem-ui/dist/
/crates/iem-tray/gen/
node_modules/
/e2e/test-results/
/e2e/playwright-report/
__pycache__/
*.log
.DS_Store
Thumbs.db
.idea/
.vscode/
*.swp
# Harness-local Claude Code settings (never shared)
.claude/settings.local.json
# Runtime data and secrets are created on the PC, never committed
# (exact file names: a `pepper.*` glob would also hide src/pepper.rs)
secrets/
pin_hashes.json
pepper.dpapi
pepper.test
/config/site.toml
.env
*.env.local
TARGETS.md
credentials.json
*.pem
*.key
```

`.gitleaks.toml`:
```toml
# gitleaks: the default rule set. An allowlist entry needs a comment naming the
# test fixture and why it is not a secret; a real secret is never allowlisted.
[extend]
useDefault = true
```

- [x] **Step 10: Re-resolve the lockfile (non-compiling) and format**

```bash
set -euo pipefail
cd "$WORK"
cargo metadata --format-version 1 > /dev/null
cargo metadata --locked --format-version 1 > /dev/null
echo "Cargo.lock consistent"
grep -A1 -E '^name = "(iem-core|iem-server|iem-ui|iem-tray)"$' Cargo.lock
grep -c '^name = "iem-mixer' Cargo.lock    # expect 0
cargo fmt --all
```
Expected: four workspace packages at `2.0.0-dev.0`, no `iem-mixer*` package left. The stale imported lockfile (Scope decisions) means `cargo metadata` resolves dozens of crates fresh here; that is expected, and Task 15 handles any deprecation or advisory they bring.

- [x] **Step 11: Stage exactly the manifest, scan, and check nothing was left unstaged or ignored**

```bash
set -euo pipefail
cd "$WORK"
git add Cargo.toml Cargo.lock .gitignore .gitleaks.toml LICENSE-MIT LICENSE-APACHE README.md \
  crates e2e scripts docs/provenance
# Untracked anywhere, or ignored under the import paths (a harness-local .claude/settings.local.json is fine)
leftover="$(git ls-files --others --exclude-standard; git ls-files --others --ignored --exclude-standard -- crates e2e scripts docs)"
[ -z "$leftover" ] || { printf 'not staged:\n%s\n' "$leftover" >&2; exit 1; }
echo "nothing left unstaged"
tree="$(git write-tree)"
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$tree"
gitleaks dir --no-banner --redact .
```
Expected: `nothing left unstaged`, `denylist: clean`, gitleaks `no leaks found`. On a denylist hit: add the missing scrub-map row (or fix the manual edit), re-run Step 8 on the file, re-stage. On a gitleaks finding in a test fixture (e.g. a JWT test key literal): add a `[[allowlists]]` entry to `.gitleaks.toml` with `description`, the exact `paths` and a `regexes` entry matching only that literal, and a comment explaining why it is not a secret.

- [x] **Step 12: Record the scope decision, the architecture and the hand-offs on the tickets (public text)**

```bash
set -euo pipefail
body="$(mktemp)"
cat > "$body" <<'EOF'
ROZHODNUTÉ (S0 import scope):
- The whole predecessor server is imported at reaperiem@03be5b97 (scrubbed), REAPER-coupled modules included and marked for S5 replacement; not feature-gated. Reasons: the server does not compile without them, the program spec budgets their deletion in S5 (+2k/−3.6k), and a feature gate would add a second build configuration S5 deletes. Dropped: the live-REAPER integration test, live E2E specs (back via HIL in S6/S7), all non-app paths.
- Correction of the earlier "proven by the predecessor's CI" rationale: that CI ran `cargo test -p iem-core`, `cargo test -p iem-server --features audio,test-helpers`, a narrow clippy and the mock E2E. It never ran the TLS-gated server tests, the 59 iem-ui unit tests, clippy over `--workspace --all-targets --all-features` or wasm32, or anything of the tray on Windows, and its lockfile is stale (2026-03-28), so dozens of crates resolve fresh. S0's first CI run is where all of these are proven.
- X10 (raw `/api/reaper/*` passthrough) is removed in S0 with a router test; only X7 (EQ ownership) is deferred to S5 (comment on #8). ELEVATED_MEMBER is the placeholder `member1` until S5's `mix_view`.
- REAPER-era track-name patterns in the imported code and tests (`HANDn mic`, `<member> mic`/`gtr`/`kl` suffixes, one tech-input name) stay until S5 replaces them with the spec §3.1 topology; `config/test-site.toml` uses the spec's public names wherever the code does not key on the REAPER-era name.
- Mock E2E runs without REAPER: the REAPER-era endpoints keep failing as they do; their exact console messages are declared per describe from one list (`REAPER_ABSENT`, deleted in S5) instead of degraded modes in code S5 removes.
- Layout: virtual workspace `crates/{iem-core,iem-server,iem-ui,iem-tray}`; single version source `[workspace.package]`; Gen 2 versions `2.0.0-dev.N`.
- Dependabot: alerts only. Version-update and security-fix PRs are off: both create `dependabot/*` branches and the repository has exactly `main` and `dev`. cargo-deny advisories fail CI; crates and action pins are updated by hand on `dev`.

Architektúra (new components):
- Login limiter (`login_guard.rs`): candidates `governor` and `tower_governor`. Both count every request against a GCRA quota (`RateLimiter::check_key` consumes a cell per call; `tower_governor` runs as middleware before the handler, keyed by a `KeyExtractor`). Spec §5.3 needs failure-only budgets, a per-(client, member) exponential streak, a per-origin engineer budget that spaces a whole origin, `CF-Connecting-IP` trusted only from a loopback peer, and admission before hashing tied to a bounded hashing gate; none of these fit a request quota, and the limits would sit before the PIN check that decides what counts. Chosen: a pure module with the clock injected (~250 LoC plus table tests) and `tokio::sync::Semaphore` for the gate.
- PIN provisioning CLI (`iem-server pin …`): candidate `clap` (derive). Two fixed subcommands and one positional argument; the contract (PIN on stdin, exit 2 on usage or invalid input) is a slice match of ~20 lines. Chosen: std only; the S6 guard CLI (`iemmode`) is where `clap` pays off.
- Pepper protection: candidates `keyring` (Windows Credential Manager: another store in the user's vault, different backup and roaming behaviour), `windows-dpapi` (small single-maintainer wrapper) and the `windows` crate (Microsoft's bindings, already in the dependency graph through Tauri). Chosen: `windows` 0.61 with `CryptProtectData`/`CryptUnprotectData` (~40 LoC, Windows-only module tested by the Windows CI job).
- PIN hashing: `argon2` (RustCrypto) with `Argon2::new_with_secret` for the pepper; site config: `toml` 0.9 with serde `deny_unknown_fields`, replacing the archived `serde_yaml`.
Plan: docs/superpowers/plans/2026-09-24-s0-bootstrap.md (committed in Task 4).
EOF
gh issue comment 2 -R "$REPO" --body-file "$body"
```

Record the deferred work on the sub-project tickets in the same step (the durable place each later design note starts from):
```bash
set -euo pipefail
body="$(mktemp)"
post() { printf '%s\n' "$2" > "$body"; gh issue comment "$1" -R "$REPO" --body-file "$body"; }
post 3 "Hand-off from S0 (#2): (1) the S0 interim event runbook in the private ops repo is replaced by this sub-project's interim switch script; (2) one PC-only credential must be added to the private denylist (ops issue filed in S0)."
post 5 "Hand-off from S0 (#2): (1) deny.toml gets a [[licenses.exceptions]] entry allowing GPL-3.0-or-later for iem-limiter-mga (and for the engine crate that links it), per D1; (2) add the per-PR fuzz job and the rt-safety checks for the DSP and limiter kernels (spec §5.2)."
post 6 "Hand-off from S0 (#2): add the rt-safety job (assert_no_alloc + rtsan, invariant I7), the per-PR fuzz job plus the nightly shard, and the engine dependency allowlist in the supply-chain job (spec §5.2)."
post 8 "Hand-off from S0 (#2): (1) X7 EQ ownership check (input EQ by its owner or the engineer, bus EQ by its member or the engineer); (2) mix_view from the site config replaces the member1 placeholder for ELEVATED_MEMBER; (3) LoginGuard::stats() on the engineer page with the band-activity banner; (4) talk lock and handshake (spec §5.3); (5) delete REAPER_ABSENT from e2e/tests/support/fixtures.ts with the REAPER control plane; (6) replace the REAPER-era track-name patterns with the spec §3.1 topology."
post 9 "Hand-off from S0 (#2): (1) the tunnel ingress must target 127.0.0.1 (the login limiter trusts CF-Connecting-IP only from a loopback peer) — add a HIL check; (2) final PC paths: the pepper outside the roaming profile and apart from the PIN hashes (the archived draft proposed %LOCALAPPDATA%\\iemmixer\\); (3) owner-present bootstrap runs \`iem-server pin set-engineer\`; (4) hil/iem-pc required check posted by the ops GitHub App; (5) the attest job (no id-token in build jobs, attest by digest on dev/main pushes) and the dispatch job that triggers hil.yml (spec §5.2); (6) the edge rate-limit rule on the login path at the tunnel provider (spec §5.3; ops issue filed in S0); (7) import the Windows cloudflared service setup script from the pinned SHA (scrubbed)."
post 10 "Hand-off from S0 (#2): the predecessor's live elevated spec probes /api/reaper/NTRACK, which S0 removed (X10); port it against the engine protocol."
```

- [x] **Step 13: Commit the import on `main`**

```bash
cd "$WORK"
git commit -F - <<'EOF'
Import from zbynekdrlik/reaperiem@03be5b97 (private); prior history retained there

Fresh start for Gen 2: the predecessor's web app (server, Leptos UI, tray,
mock E2E) from a git archive of the pinned SHA, re-laid out as a virtual
workspace under crates/, scrubbed of site data (names -> member1..member9,
hosts/IPs -> documentation values, compiled-in default PINs removed, E2E
PINs from the environment), licensed MIT OR Apache-2.0. The TAKE manifest is
docs/provenance/import-manifest.txt. REAPER-coupled server modules are kept
for S5 and marked in their module docs.

Refs #2

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
EOF
git log --oneline
```
Expected: one commit on `main`.

---

### Task 3: `dev` branch and the Gen 2 version bump (first commit on `dev`)

**Files:**
- Modify: `Cargo.toml` (`[workspace.package].version`), `Cargo.lock`

**Interfaces:**
- Produces: `dev` at `2.0.0-dev.1`, strictly greater than `main` (`2.0.0-dev.0`).

- [x] **Step 1: Create `dev` and bump**

```bash
set -euo pipefail
cd "$WORK"
git switch -c dev
sed -i 's/^version = "2.0.0-dev.0"$/version = "2.0.0-dev.1"/' Cargo.toml
grep -n '^version = ' Cargo.toml          # expect line with 2.0.0-dev.1
cargo metadata --format-version 1 > /dev/null
grep -A1 -E '^name = "(iem-core|iem-server|iem-ui|iem-tray)"$' Cargo.lock | grep -c '2.0.0-dev.1'   # expect 4
```

- [x] **Step 2: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to 2.0.0-dev.1 (Gen 2 dev line)" -m "Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Program spec, this plan, project playbook, SECURITY and CONTRIBUTING

**Files:**
- Create: `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md`, `docs/superpowers/plans/2026-09-24-s0-bootstrap.md`, `CLAUDE.md`, `.claude/rules/{public-repo-hygiene,security-baseline,e2e,ci-rust-toolchain,leptos-view-macro,pan-and-send-domains,tunnel-watchdog}.md`, `SECURITY.md`, `CONTRIBUTING.md`, `scripts/denylist-allow.txt`

**Interfaces:**
- Produces: the playbook router the later tasks' rules plug into; `scripts/denylist-allow.txt` format `<sha256>  <path> — <reason>`.

- [ ] **Step 1: Copy the approved spec and a redacted public copy of this plan**

```bash
set -euo pipefail
cd "$WORK"
mkdir -p docs/superpowers/specs docs/superpowers/plans
cp "$WP/12-program-spec.md" docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md
python3 - "$WP/13-plan-S0.md" docs/superpowers/plans/2026-09-24-s0-bootstrap.md <<'EOF'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
fence = re.compile(r"^<!-- ops-only:begin -->\n.*?^<!-- ops-only:end -->\n", re.S | re.M)
note = "> *Private detail: see the iemmixer-ops copy of this plan (`docs/plans/2026-09-24-s0-bootstrap.md`).*\n"
public, count = fence.subn(note, text)
assert count == 2, f"expected 2 ops-only blocks, found {count}"
assert not re.search(r"^<!-- ops-only:(begin|end) -->$", public, re.M), "unbalanced ops-only markers"
open(sys.argv[2], "w", encoding="utf-8").write(public)
print(f"public plan: {count} private blocks redacted")
EOF
```
Expected: `public plan: 2 private blocks redacted`. The full plan goes to the private ops repo in Task 13.

One line of the Slovak owner summary (spec §0) contains an ordinary word that is also a denylisted spelling (the Step 5 scan reports it). In the **public copy only**, reword that one sentence with the same meaning and without the word — never by an allowlist entry, whose reason text would point at the word. The committed copy is the canonical spec from now on; the unmodified approved text stays archived in the private ops repo (Task 13).

- [ ] **Step 2: Write `CLAUDE.md`**

```markdown
# iemmixer

Gen 2 of the band's in-ear-monitor mixer: a native Rust audio engine replacing REAPER. **Public repository** — site data never enters it (program spec P6).

- Program spec: `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md` (approved 2026-09-24). Each sub-project gets a design note and a plan in `docs/superpowers/` before code.
- Tickets: this repository's issues (#1 program, one ticket per sub-project). Predecessor issues are written `reaperiem#N`; `#N` always means this repository.

## Playbook router

- Public-repo hygiene, denylist, scrubbing, imports from the predecessor → `.claude/rules/public-repo-hygiene.md`
- Login protection, PIN hashing, pepper, secrets, provisioning → `.claude/rules/security-baseline.md`
- Playwright E2E (console guard, env PINs, login budgets) → `.claude/rules/e2e.md`
- CI Rust toolchain, `--locked`, mutation gate → `.claude/rules/ci-rust-toolchain.md`
- Leptos `view!` gotchas and disposal safety → `.claude/rules/leptos-view-macro.md`
- Pan domains + send_index (REAPER-era server code) → `.claude/rules/pan-and-send-domains.md`
- Cloudflare tunnel watchdog, LAN URL / public host → `.claude/rules/tunnel-watchdog.md`

## Always-apply rules

**Owner event signals (D2).** "ide event" (an event is coming) → immediately stop everything iemmixer on the IEM PC, start REAPER and the predecessor app, verify the handover, and confirm back to the owner. "event skončil" (the event ended) → save and quit REAPER, stop the predecessor app gracefully, start iemmixer, continue development. Never switch on your own and never ask whether an event is running. A reboot always comes back in event mode. Nothing is ever force-killed.
**Until S1a's interim switch script (S6: `iemmode`)** follow the "Owner event signals — S0 interim runbook" in the private `iemmixer-ops` `CLAUDE.md` (on the dev box also `~/.config/iemmixer/event-runbook.md`, which exists before the ops repo does): PC access path, read-only checks of REAPER, the predecessor app and the handover, graceful starts only. Nothing of iemmixer runs on the PC yet, so "event skončil" needs no PC action: confirm to the owner that development continues.

**Predecessor boundary.** Never push to `zbynekdrlik/reaperiem`, never change its code, config or deployment; read it only through `~/devel/reaperiem` at a pinned SHA.

**Site data (P6).** Never commit names, hosts, IPs, Dante channel numbers, real track names, PINs, keys or tokens. Real site values live only in the private `zbynekdrlik/iemmixer-ops` (credential strings not even there: only in `~/.config/iemmixer/` and the CI secret). Every clone installs the pre-push hook (gitleaks + private denylist + allowed commit identities); CI repeats all three.

**Dante.** Never change Dante subscriptions or any Dante device other than the IEM PC's own card.

**Builds.** Tier 0: no local cargo compilation — push `dev` and verify in CI. Local `cargo fmt`, `cargo metadata`, `cargo tree`, `cargo update -p` are fine; `cargo deny` and `cargo mutants` run in CI only.

**Versions.** One version for the workspace: `[workspace.package].version` in `Cargo.toml` (`2.0.0-dev.N` until cutover). `scripts/check_version.py` enforces consistency, and dev > main on PRs.
```

- [ ] **Step 3: Write the rule files**

`.claude/rules/public-repo-hygiene.md`:
```markdown
---
paths:
  - "scripts/**"
  - "config/**"
  - "docs/**"
  - "e2e/**"
  - ".gitleaks.toml"
  - "README.md"
  - "CLAUDE.md"
---

# Public-repo hygiene (program spec P6)

- This repository is public. Site data never enters it: band member names, host names, IPs, Dante device names and channel numbers, real track names, PINs, keys, tokens, user paths, the REAPER project name. Real values live only in the private `zbynekdrlik/iemmixer-ops`.
- Placeholders: `member1`…`member9` (MEMBER_1…MEMBER_9; `member1` = ELEVATED_MEMBER), `engineer`, TRANSLATOR; hosts `mixer.example.org`, LAN `http://10.0.0.10`; synthetic Dante numbers (member N = 69+2N/70+2N, i.e. 71–88; any other output 89/90; engineer 91/92; inputs 101–124); input names from program spec §3.1 where the code does not key on a REAPER-era name.
- **Every clone installs the pre-push hook** (S0 plan, Task 1 Step 9): gitleaks + `scripts/denylist_scan.py` with the private denylist `~/.config/iemmixer/denylist.txt` and `scripts/allowed-identities.txt`. CI repeats all three in the `secrets` job (`DENYLIST` secret). The denylist's non-credential terms are also kept in the ops repo; its credential entries only on the box and in the secret.
- **Scanner output never shows a term or an address** — only `path:line: denylist entry N` or `<sha> author|committer email is not an allowed identity`; open the local denylist to see entry N.
- **Allowlist** `scripts/denylist-allow.txt`: sha256 keys of single lines that are ordinary text equal to a denylisted term: `python3 scripts/denylist_scan.py --hash <path> <line>`, with a neutral reason ("reviewed ordinary prose, not site data") that never names or hints at the term. Prefer rewording the line. Never allowlist a real private value; never blanket-allow a term.
- **Importing more predecessor code** (S1b/S4/S5): `git -C ~/devel/reaperiem archive <pinned SHA> -- <paths>` into a temp dir, `python3 scripts/scrub_import.py --map ~/.config/iemmixer/scrub-map.tsv --root <dir> --report <tmp>`, rewrite Dante channel literals to the synthetic map and run the Dante gate (S0 plan, Task 2 Step 8), then scan the staged tree (`git write-tree` → `--tree`) before committing; add the paths to `docs/provenance/import-manifest.txt`.
- Commits use the GitHub noreply identity only (`scripts/allowed-identities.txt`); `gh pr merge` passes `--author-email` with it.
```

`.claude/rules/security-baseline.md`:
```markdown
---
paths:
  - "crates/iem-server/src/auth.rs"
  - "crates/iem-server/src/login_guard.rs"
  - "crates/iem-server/src/pin_hash.rs"
  - "crates/iem-server/src/pin_store.rs"
  - "crates/iem-server/src/pepper.rs"
  - "crates/iem-server/src/pepper/**"
  - "crates/iem-server/src/secrets.rs"
  - "crates/iem-server/src/provision.rs"
  - "crates/iem-server/src/bin/server.rs"
  - "crates/iem-server/src/lib.rs"
  - "crates/iem-server/src/routes.rs"
  - "crates/iem-core/src/config.rs"
---

# Security baseline (program spec §5.3)

- **No compiled-in credentials.** No default PIN, no default JWT key. `jwt_secret` and `vapid_private` are generated on first run in `<config dir>/secrets/` (owner-only, created exclusively, never overwritten). The site config rejects secret and PIN keys (`deny_unknown_fields`; secret fields are `#[serde(skip)]`).
- **PINs** are argon2id PHC strings (m=19456 KiB, t=2, p=1) keyed with a 32-byte pepper (`Argon2::new_with_secret`). `pin_hashes.json` holds only `$argon2id$` values — anything else is a load error. The pepper is DPAPI-protected on Windows (`pepper.dpapi`), a plain owner-only test file elsewhere (Linux is test-only). An unreadable pepper is an error, never regenerated (that would void every PIN). Backups never contain PINs.
- **Start-up fails loud:** `start_server` builds its state with `AppState::try_new(…)?`, so a bad pepper or PIN store returns an error before any port is bound, and the tray exits when its server fails before it is ready. `AppState::new` (panicking) exists only for tests (`cfg(test)` / `test-helpers`).
- **No raw REAPER passthrough** (X10): `/api/reaper/*` is gone (404); never add a generic proxy to an engine or REAPER control surface.
- **Provisioning:** first engineer PIN via `iem-server pin set-engineer` (PIN on stdin, server stopped); members get PINs by engineer reset in the UI (F3) or `iem-server pin set-member <id>`.
- **Login protection** (`login_guard.rs`): admission before any hashing; failures only; separate LAN/tunnel origins; `CF-Connecting-IP` trusted only from a loopback peer (the tunnel ingress must target 127.0.0.1 — S6 checks); IPv6 clients keyed by their /64; admission does not reserve (in-flight attempts are bounded by the hashing gate); per (client, member): 3 free failures then 1, 2, 4 … s, forgotten after 15 quiet minutes; per client: 20 failures / 10 min → 60 s spacing; engineer budget per origin: > 30 failures / h → 5 s spacing for the whole origin (every failure may be an engineer-PIN guess); every delay ≤ 60 s, never a lockout; 429 + `Retry-After`; hashing gate 2 running + 8 queued, else 429; issued JWTs untouched. `LoginGuard::stats()` feeds the engineer page in S5.
- **Keep these tests:** limiter tables, forged header from a non-loopback peer, IPv6 keyed per /64, the intended LAN-origin spacing, in-flight attempts bounded by the gate, bounded memory, gate cancellation, plaintext store rejected, pepper never regenerated, start-up refuses a bad pepper or PIN store, the raw REAPER passthrough stays gone, the CLI never reads the PIN from argv.
- `pepper/dpapi.rs` is Windows-only: excluded from mutation testing and run by the `windows` CI job (`cargo test -p iem-server --lib pepper::`).
```

`.claude/rules/e2e.md`:
```markdown
---
paths:
  - "e2e/**"
---

# E2E (Playwright, mock — no audio hardware)

- Specs import `test`/`expect` from `./support/fixtures`: an auto fixture fails any test whose browser console shows an error, a warning or a page error. A test that deliberately provokes a failed request declares exactly that message with `test.use({ allowedConsole: [/…/] })` in its own `describe`, with a comment saying why. Never a global allowance; where the message is an app bug, fix the app.
- **The one documented environment class — REAPER absent until S5:** the server runs without REAPER, so pages that load REAPER-era mixer state get the failures listed in `REAPER_ABSENT` (`support/fixtures.ts`). A `describe` whose pages call those endpoints declares `test.use({ allowedConsole: REAPER_ABSENT })` with the comment `// REAPER absent in mock E2E until S5`. Add a message to the list only as an anchored exact pattern copied from a CI log line that shows it, and only for a REAPER-era endpoint; S5 deletes the list.
- PINs come from `E2E_ENGINEER_PIN` / `E2E_MEMBER_PIN` via `./support/pins` (CI generates them per run and provisions them with `iem-server pin …`). No credential is committed.
- The server runs with `config/test-site.toml`. REAPER is absent (the REAPER-era poller fails fast against `127.0.0.1:1`); cloudflared is absent (the tunnel is Down, so members see the tunnel banner).
- Login budgets are shared by the whole run: tests that fail logins act as tunnel clients from their own `203.0.113.N` (`CF-Connecting-IP` from a loopback peer is trusted) and keep the total under 30 failures per origin.
- **UI login attempts wait for their response:** `const r = page.waitForResponse(…/api/auth POST…)` before typing, `(await r).status()` after, and the PIN dots cleared before the next attempt — asserting on a still-visible "Invalid PIN" races the request.
- Live specs (real PC, audio) come back in S6/S7 through HIL in the private ops repo — never in public CI.
```

`.claude/rules/ci-rust-toolchain.md`:
```markdown
---
paths:
  - ".github/workflows/*.yml"
  - "rust-toolchain.toml"
  - ".cargo/mutants.toml"
  - "deny.toml"
  - "crates/**/*.rs"
---

# CI Rust toolchain

- **Pinned toolchain** in `rust-toolchain.toml` (1.98.1 + rustfmt, clippy, llvm-tools-preview, wasm32). Every job runs `rustup toolchain install` first. Bump it in its own commit: a new stable can add clippy lints (e.g. `byte_char_slices`: write `*b"OIEM"`, not `[b'O', …]`) — read the lint name in `gh run view --log-failed` before editing code.
- **`--locked` everywhere:** cargo commands in CI use `--locked`; the UI builds with `trunk build --release --locked`; any `cargo install` in CI MUST be `--locked` (an unpinned trunk once pulled a broken CSS dependency with zero UI change). Tools come prebuilt from `taiki-e/install-action` at pinned versions.
- **`Swatinem/rust-cache` on every cargo job.** Linux jobs exclude `iem-tray` (Tauri); the `windows` job lints, tests (DPAPI) and builds it.
- **Mutation gate:** `.cargo/mutants.toml` (profile `mutants`, nextest, excludes with reasons); PR-only, diff-scoped, each shard ≤ 20 min. Shards are 0-based (`--shard k/n`, `n = strategy.job-total`); the shard count is the length of the `shard:` matrix in `ci.yml`. The `mutation-warmup` job builds the `mutants` profile once and saves the dependency cache (`shared-key: mutation`) that every shard restores; shards run `--copy-target=true` after a no-op warm-up so each mutant rebuilds only workspace crates. `mutants-list` (every `dev` push) counts the next PR's diff-scoped mutants and fails when they need more shards than the matrix has at `MUTANTS_PER_SHARD` — resize the matrix before opening the PR. An overrun is a setup bug: add shards or narrow scope (fallback: `--in-place --jobs 1` with more shards), never raise the timeout. `crates/iem-core/build.rs` and `crates/iem-tray/build.rs` key their rerun on `GITHUB_SHA` when there is no `.git` (cargo-mutants' scratch copy), so `BUILD_TIME` does not force a full rebuild per mutant. The full sweep is `mutation-full.yml` (on demand).
- **Every `uses:` is pinned to a full commit SHA** with a `# vX.Y.Z` comment; `scripts/check_integrity.py` fails otherwise. Pins and crates are updated by hand in their own commit on `dev` (no Dependabot version PRs: they would create `dependabot/*` branches).
- **Required checks** on `main` are regenerated from the job list and the matrix length whenever shards change (S0 plan, Task 15 Step 6); a skipped required job counts as passing, so every job a required job `needs` is itself required.
```

`.claude/rules/leptos-view-macro.md`:
```markdown
---
paths:
  - "crates/iem-ui/**/*.rs"
---

# Leptos `view!` macro gotchas (iem-ui)

## Never put a bare comparison inline in a `view!` attribute or `when=`

The `view!` macro tokenizes `>` / `>=` / `<` as tag boundaries. An inline comparison such as `<Show when=move || snapshots.get().len() >= 50>` does not fail to parse — it mis-tokenizes and surfaces as an unrelated `E0308 mismatched types` deep in the expansion (reaperiem#206, `snapshot_modal.rs`). Bind the predicate to a named closure first (`let at_limit = move || snapshots.get().len() >= MAX_SNAPSHOTS;`), then write `<Show when=at_limit>`. Same for `class:=`, `style:=` and any attribute expression with `<`, `>`, `>=`, `<=`.

## Signal writes after an await or in a JS callback use `try_*`

`scripts/check_disposal_safety.py` (the `integrity` CI job) rejects `.set()` / `.update()` inside `spawn_local` blocks and `Closure::wrap` callbacks — the component may already be disposed. Use `try_set` / `try_update`.
```

`.claude/rules/pan-and-send-domains.md`:
```markdown
---
paths:
  - "crates/iem-server/src/*.rs"
  - "crates/iem-core/src/{types,snapshot,preset}.rs"
---

# Pan domains + send_index on restore (REAPER-era server code, deleted in S5)

**Pan:** the poller converts REAPER→UI on read, so `Channel.pan`, cache, snapshots and presets ALL hold **0..1 (0.5 = center)**. REAPER `SET/…/SEND/M/PAN` expects **−1..1** → call `ui_pan_to_reaper` at the REAPER write only (WS `SetPan`, `restore_send_pan` in both REST restores). reaperiem#203: a raw write panned every mix half-right. The backup path is raw −1..1 end to end — leave it.

**send_index:** bulk writes (restore/replay) resolve per track via `resolve_send_index` (discovered `mix_send_index`; `Err` if missing — no fallback). Never hardcode 0 or the member's own index for mix channels (reaperiem#204).

**Elevated member:** the REAPER-era code hardcodes the ELEVATED_MEMBER id as the placeholder `member1`; S5 replaces it with `mix_view` from the site config.
```

`.claude/rules/tunnel-watchdog.md`:
```markdown
---
paths:
  - "crates/iem-server/src/tunnel_watch.rs"
  - "crates/iem-server/src/tunnel_watch/**"
  - "crates/iem-core/src/tunnel.rs"
  - "crates/iem-ui/src/components/tunnel_status.rs"
---

# Cloudflare tunnel watchdog

- **Health = cloudflared `/ready`, never the service state.** A RUNNING `cloudflared` with 0 edge connections is exactly the event failure (QUIC blocked). `http://127.0.0.1:20241/ready` → `{"status":200,"readyConnections":N}`, **HTTP 503** when N = 0 (treated as 0, like timeouts and garbage).
- **Pure state machine** (`TunnelWatch::observe(ready, now)`, `Instant` injected): Down on the first 0, restart after 120 s continuous, cooldown 600 s, `Restarting` for 60 s grace then back to `Down` (keeps `since`). Tests use synthetic instants — never sleep.
- **Tunnel flags live in the service ImagePath** (`tunnel --protocol http2 --metrics 127.0.0.1:20241 run --token …`), never an env var; `--metrics` MUST be pinned or the watchdog restarts a healthy tunnel every 10 min.
- **Stop takes up to 30 s** (`--grace-period`) → wait ≤ 45 s for STOPPED; `sc start` 1056 = still stopping → wait and retry once; `sc stop` 1062 = already stopped; 5 = access denied (service rights missing — the one-time elevated setup is S6 work). The outcome is published as `last_restart_ok`.
- **Never print the token.** `sc.exe qc` and the ImagePath carry `--token <secret>`; the code only uses `sc query/stop/start`.
- **Windows-only code is not compiled on Linux CI:** `*_sc_windows` and `restart_service_blocking` are excluded from mutation testing; their pure parts are unit-tested.
- **LAN URL and public host come from the site config** (`lan_url`, `https_domain` → `GET /api/site` → UI context; the tray's "Copy URL" uses `Config::share_url()`). Members on the public URL get no `TunnelStatus` while the tunnel is down; the client-side `LanHint` shows when the page host equals the configured public host.
- Server-side `sc.exe` runs with `CREATE_NO_WINDOW` (the app is a tray GUI).
```

- [ ] **Step 4: Write `SECURITY.md` and `CONTRIBUTING.md`**

`SECURITY.md`:
```markdown
# Security policy

## Reporting a vulnerability

Report vulnerabilities privately with GitHub's "Report a vulnerability" button on this repository (private vulnerability reporting). Please do not open a public issue. You will get an answer within 7 days.

## Scope

The server, web UI and tray in this repository, and later the engine and guard. Site configuration and deployment live in a private repository and are out of scope.

## Baseline

- No credentials in the code or the repository; secrets are generated on the target PC.
- PINs are stored as argon2id hashes keyed with a DPAPI-protected pepper.
- Login attempts are rate-limited per client and per origin and never locked out.
- CI: SHA-pinned actions, read-only default token, secret scanning with push protection, gitleaks, a private denylist scan, cargo-deny.
- No self-hosted runner is registered on this repository; pull requests from forks never produce deployable builds.

## Supported versions

Only the latest `main`.
```

`CONTRIBUTING.md`:
```markdown
# Contributing

- Open pull requests against `dev`; `main` only receives merges from `dev`.
- Workflows on pull requests from forks run only after a maintainer approves them (required for all external contributors). Fork pull requests never produce deployable builds.
- On fork pull requests the `secrets` check fails by design: the private denylist is not available to forks, so a maintainer runs the scan locally before merging, and the merge push to `dev` runs it again.
- Commits must use your GitHub noreply address (`<id>+<login>@users.noreply.github.com`): CI rejects author or committer emails outside `scripts/allowed-identities.txt`; a maintainer adds an accepted contributor's noreply address to that file (never a personal address).
- Every change needs tests that can fail; CI must be fully green: lint, unit tests with the coverage floor, WASM build, browser E2E with a clean console, diff-scoped mutation testing, supply chain, secret scans, Windows build.
- Never add site-specific data (names, hosts, addresses, PINs, keys) — see `.claude/rules/public-repo-hygiene.md`.
- Contributions are licensed MIT OR Apache-2.0 like the project (the future `iem-limiter-mga` crate: GPL-3.0-or-later).
```

- [ ] **Step 5: Scan (reword the one flagged spec sentence), commit**

`scripts/denylist-allow.txt` (starts with no entries):
```text
# Reviewed single lines of ordinary text that equal a denylisted term:
# "<sha256>  <path> — reviewed ordinary prose, not site data"
# Key: python3 scripts/denylist_scan.py --hash <path> <line>. Prefer rewording the
# line; the reason never names or hints at the term; never a real private value.
```
```bash
set -euo pipefail
cd "$WORK"
git add docs CLAUDE.md .claude/rules SECURITY.md CONTRIBUTING.md scripts/denylist-allow.txt
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$(git write-tree)" || true
```
Expected: exactly one hit, in the program spec's Slovak owner summary (§0). Reword that sentence in `docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md` as Step 1 describes (same meaning, the flagged word gone), then:
```bash
set -euo pipefail
cd "$WORK"
git add docs/superpowers/specs/2026-09-24-iemmixer-gen2-program.md
python3 scripts/denylist_scan.py --denylist "$PRIV/denylist.txt" --allow scripts/denylist-allow.txt --tree "$(git write-tree)"
git commit -m "docs: program spec, S0 plan, project playbook, security and contributing notes" -m "The committed program spec is the approved text with one sentence of the Slovak owner summary reworded (same meaning). The plan is the public copy; its private blocks live in the ops repo. Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected: `denylist: clean` before the commit, with no allowlist entry. Any other hit is a real problem: fix the text, never allowlist it. (`.claude/rules` only: a harness-local `.claude/settings.local.json` is ignored and never staged.)

- [ ] **Step 6: Restart the session in `$WORK`**

The running session still carries the archived predecessor `CLAUDE.md` (its manual-merge marker and REAPER/LAN rules) and a project hook that no longer exists. Stop here and continue Tasks 5–16 in a fresh Claude Code session started in `$WORK`, so the new `CLAUDE.md` and `.claude/rules/` load; merging then follows the new `CLAUDE.md` (no manual marker). The new session resumes from durable state: `git log`, this plan's checkboxes and #2.

---

### Task 5: CI skeleton, version and integrity gates

**Files:**
- Create: `.github/workflows/ci.yml`, `.github/workflows/mutation-full.yml`, `.github/coverage-floor`, `rust-toolchain.toml`, `deny.toml`, `.cargo/mutants.toml`, `scripts/check_version.py`, `scripts/test_check_version.py`, `scripts/check_integrity.py`, `scripts/test_check_integrity.py`
- Modify: `Cargo.toml` (add `[profile.mutants]`), `crates/iem-core/build.rs`, `crates/iem-tray/build.rs` (rerun key without `.git`)

**Interfaces:**
- Consumes: `scripts/denylist_scan.py`, `scripts/check_disposal_safety.py`, `iem-server pin …` CLI (Task 11), `config/test-site.toml` (Task 6), E2E support modules (Task 12).
- Produces: check contexts `integrity`, `lint`, `test`, `wasm`, `e2e`, `windows`, `supply-chain`, `secrets`, `version`, `mutation-warmup`, `mutation shard 0`…`mutation shard <n−1>` (n = the `shard:` matrix length, 8 to start; Task 15 sizes it and makes these required), plus `mutants-list` on `dev` pushes only (not required); `python3 scripts/check_version.py [--base-ref REF]`; `python3 scripts/check_integrity.py`.

- [ ] **Step 1: Write the failing tests for the two gate scripts**

`scripts/test_check_version.py`:
```python
"""Tests for scripts/check_version.py."""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_version as cv  # noqa: E402

CRATE = '[package]\nname = "{name}"\nversion.workspace = true\n'


def write_tree(root: Path, version: str, lock_versions: dict[str, str] | None = None, tauri_version: bool = False) -> None:
    (root / "Cargo.toml").write_text(f'[workspace]\nmembers = []\n\n[workspace.package]\nversion = "{version}"\n', encoding="utf-8")
    for name in cv.CRATES:
        (root / "crates" / name).mkdir(parents=True, exist_ok=True)
        (root / "crates" / name / "Cargo.toml").write_text(CRATE.format(name=name), encoding="utf-8")
    tauri = {"productName": "iemmixer"}
    if tauri_version:
        tauri["version"] = version
    (root / "crates" / "iem-tray" / "tauri.conf.json").write_text(json.dumps(tauri), encoding="utf-8")
    locks = lock_versions or {name: version for name in cv.CRATES}
    lock = "version = 4\n" + "".join(f'\n[[package]]\nname = "{n}"\nversion = "{v}"\n' for n, v in locks.items())
    (root / "Cargo.lock").write_text(lock, encoding="utf-8")


class CompareTests(unittest.TestCase):
    def test_semver_precedence(self) -> None:
        self.assertGreater(cv.compare("2.0.0-dev.1", "2.0.0-dev.0"), 0)
        self.assertGreater(cv.compare("2.0.0-dev.10", "2.0.0-dev.9"), 0)
        self.assertGreater(cv.compare("2.0.0", "2.0.0-dev.5"), 0)
        self.assertGreater(cv.compare("2.0.1-dev.0", "2.0.0"), 0)
        self.assertGreater(cv.compare("2.0.0-dev.1.1", "2.0.0-dev.1"), 0)
        self.assertGreater(cv.compare("2.0.0-dev.a", "2.0.0-dev.1"), 0)
        self.assertEqual(cv.compare("2.0.0-dev.3", "2.0.0-dev.3"), 0)
        self.assertLess(cv.compare("1.9.9", "2.0.0-dev.0"), 0)

    def test_rejects_non_semver(self) -> None:
        with self.assertRaises(ValueError):
            cv.parse("2.0")


class ConsistencyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def test_consistent_tree_passes(self) -> None:
        write_tree(self.root, "2.0.0-dev.1")
        self.assertEqual(cv.consistency_errors(self.root), [])

    def test_crate_with_its_own_version_fails(self) -> None:
        write_tree(self.root, "2.0.0-dev.1")
        (self.root / "crates" / "iem-ui" / "Cargo.toml").write_text('[package]\nname = "iem-ui"\nversion = "1.0.0"\n', encoding="utf-8")
        self.assertTrue(any("iem-ui" in e for e in cv.consistency_errors(self.root)))

    def test_tauri_version_fails(self) -> None:
        write_tree(self.root, "2.0.0-dev.1", tauri_version=True)
        self.assertTrue(any("tauri.conf.json" in e for e in cv.consistency_errors(self.root)))

    def test_stale_lockfile_fails(self) -> None:
        write_tree(self.root, "2.0.0-dev.2", lock_versions={n: "2.0.0-dev.1" for n in cv.CRATES})
        self.assertEqual(len(cv.consistency_errors(self.root)), len(cv.CRATES))


class BaseRefTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        for args in (["init", "-q", "-b", "main"], ["config", "user.email", "t@example.org"], ["config", "user.name", "t"],
                     ["config", "commit.gpgsign", "false"]):
            subprocess.run(["git", "-C", str(self.root), *args], check=True)
        write_tree(self.root, "2.0.0-dev.0")
        subprocess.run(["git", "-C", str(self.root), "add", "-A"], check=True)
        subprocess.run(["git", "-C", str(self.root), "commit", "-qm", "base"], check=True)

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def test_bumped_head_passes(self) -> None:
        write_tree(self.root, "2.0.0-dev.1")
        self.assertEqual(cv.main(["--root", str(self.root), "--base-ref", "main"]), 0)

    def test_unbumped_head_fails(self) -> None:
        self.assertEqual(cv.main(["--root", str(self.root), "--base-ref", "main"]), 1)


if __name__ == "__main__":
    unittest.main()
```

`scripts/test_check_integrity.py`:
```python
"""Tests for scripts/check_integrity.py."""
from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_integrity as ci  # noqa: E402

PINNED = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n"


class IntegrityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        self.put("crates/a/src/lib.rs", "#[test]\nfn ok() {}\n")
        self.put("e2e/tests/a.spec.ts", 'test("ok", async () => {});\n')
        self.put(".github/workflows/ci.yml", "jobs:\n  a:\n    steps:\n" + PINNED)

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def put(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def test_clean_tree(self) -> None:
        self.assertEqual(ci.violations(self.root), [])

    def test_ignored_rust_test(self) -> None:
        self.put("crates/a/src/lib.rs", "#[test]\n#[ignore]\nfn skipped() {}\n")
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_skipped_or_focused_e2e(self) -> None:
        for body in ('test.skip("x", async () => {});', 'test.only("x", async () => {});',
                     'test.describe.skip("x", () => {});', 'test.fixme("x", async () => {});'):
            self.put("e2e/tests/a.spec.ts", body + "\n")
            self.assertEqual(len(ci.violations(self.root)), 1, body)

    def test_forbidden_workflow_constructs(self) -> None:
        for line in ("    continue-on-error: true\n", "    runs-on: [self-hosted, x]\n", "on: pull_request_target\n"):
            self.put(".github/workflows/ci.yml", "jobs:\n" + line + PINNED)
            self.assertEqual(len(ci.violations(self.root)), 1, line)

    def test_unpinned_action(self) -> None:
        self.put(".github/workflows/ci.yml", "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v7\n")
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_force_kill_command(self) -> None:
        self.put("scripts/stop.ps1", "taskkill /F /IM engine.exe\n")
        self.assertEqual(len(ci.violations(self.root)), 1)


if __name__ == "__main__":
    unittest.main()
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd "$WORK" && PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts/test_check_version.py scripts/test_check_integrity.py -v`
Expected: FAIL — `ModuleNotFoundError` for `check_version` and `check_integrity`.

- [ ] **Step 3: Write `scripts/check_version.py`**

```python
#!/usr/bin/env python3
"""Version gate: one version for the whole workspace; on PRs to main the head
version must be greater than main's (SemVer 2.0 precedence).

Single source: [workspace.package].version in Cargo.toml. Every crate uses
`version.workspace = true`, tauri.conf.json sets no version, and Cargo.lock
records that version for every workspace package.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CRATES = ["iem-core", "iem-server", "iem-ui", "iem-tray"]
SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z.-]+))?$")


def parse(version: str) -> tuple[tuple[int, int, int], list[str] | None]:
    match = SEMVER.match(version)
    if not match:
        raise ValueError(f"not a SemVer version: {version!r}")
    core = (int(match[1]), int(match[2]), int(match[3]))
    return core, (match[4].split(".") if match[4] else None)


def compare(a: str, b: str) -> int:
    (core_a, pre_a), (core_b, pre_b) = parse(a), parse(b)
    if core_a != core_b:
        return -1 if core_a < core_b else 1
    if pre_a == pre_b:
        return 0
    if pre_a is None:
        return 1
    if pre_b is None:
        return -1
    for x, y in zip(pre_a, pre_b):
        if x == y:
            continue
        x_num, y_num = x.isdigit(), y.isdigit()
        if x_num and y_num:
            return -1 if int(x) < int(y) else 1
        if x_num != y_num:
            return -1 if x_num else 1
        return -1 if x < y else 1
    return -1 if len(pre_a) < len(pre_b) else 1


def workspace_version(cargo_toml: str) -> str:
    return tomllib.loads(cargo_toml)["workspace"]["package"]["version"]


def consistency_errors(root: Path) -> list[str]:
    errors: list[str] = []
    version = workspace_version((root / "Cargo.toml").read_text(encoding="utf-8"))
    parse(version)
    for crate in CRATES:
        manifest = tomllib.loads((root / "crates" / crate / "Cargo.toml").read_text(encoding="utf-8"))
        if manifest["package"].get("version") != {"workspace": True}:
            errors.append(f"crates/{crate}/Cargo.toml must use version.workspace = true")
    tauri = json.loads((root / "crates" / "iem-tray" / "tauri.conf.json").read_text(encoding="utf-8"))
    if "version" in tauri:
        errors.append("crates/iem-tray/tauri.conf.json must not set a version (Cargo is the single source)")
    lock = tomllib.loads((root / "Cargo.lock").read_text(encoding="utf-8"))
    locked = {package["name"]: package["version"] for package in lock["package"] if package["name"] in CRATES}
    for crate in CRATES:
        if locked.get(crate) != version:
            errors.append(f"Cargo.lock has {crate} {locked.get(crate)}, expected {version}")
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Workspace version gate.")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--base-ref", help="e.g. origin/main: the head version must be greater")
    args = parser.parse_args(argv)
    errors = consistency_errors(args.root)
    head = workspace_version((args.root / "Cargo.toml").read_text(encoding="utf-8"))
    if args.base_ref:
        base_text = subprocess.run(["git", "-C", str(args.root), "show", f"{args.base_ref}:Cargo.toml"],
                                   check=True, capture_output=True, text=True).stdout
        base = workspace_version(base_text)
        if compare(head, base) <= 0:
            errors.append(f"version {head} must be greater than {args.base_ref} ({base}): bump [workspace.package].version first")
        else:
            print(f"version bump OK: {base} -> {head}")
    for error in errors:
        print(f"::error::{error}")
    if errors:
        return 1
    print(f"version {head}: consistent")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 4: Write `scripts/check_integrity.py`**

```python
#!/usr/bin/env python3
"""Integrity gate: no ignored/skipped/focused tests, no continue-on-error,
self-hosted runners or pull_request_target, every action pinned to a full
commit SHA, and no force-kill command anywhere (program spec I8)."""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SELF = {"scripts/check_integrity.py", "scripts/test_check_integrity.py"}
RUST_IGNORE = re.compile(r"#\[\s*ignore")
E2E_SKIP = re.compile(r"\b(?:test|it|describe)(?:\.describe)?\.(?:skip|only|fixme)\s*\(|\btest\.fail\s*\(|function assume\(")
WORKFLOW_FORBIDDEN = re.compile(r"continue-on-error|self-hosted|pull_request_target")
USES = re.compile(r"^\s*-?\s*uses:\s*(\S+)")
PINNED = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")
FORCE_KILL = re.compile(r"(?i)\btaskkill\b|terminateprocess|stop-process|\bshutdown(?:\.exe)?\s+/f\b")
CODE_SUFFIXES = (".rs", ".ts", ".js", ".py", ".sh", ".ps1", ".yml", ".yaml", ".toml")


def files(root: Path, base: str, suffixes: tuple[str, ...]) -> list[Path]:
    top = root / base
    if not top.is_dir():
        return []
    return sorted(p for p in top.rglob("*") if p.is_file() and p.suffix in suffixes and "node_modules" not in p.parts)


def lines(path: Path) -> list[tuple[int, str]]:
    return list(enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), start=1))


def violations(root: Path) -> list[str]:
    found: list[str] = []
    for path in files(root, "crates", (".rs",)):
        rel = path.relative_to(root).as_posix()
        found += [f"{rel}:{n}: #[ignore] test" for n, line in lines(path) if RUST_IGNORE.search(line)]
    for path in files(root, "e2e", (".ts",)):
        rel = path.relative_to(root).as_posix()
        found += [f"{rel}:{n}: skipped or focused E2E test" for n, line in lines(path) if E2E_SKIP.search(line)]
    for path in files(root, ".github/workflows", (".yml", ".yaml")):
        rel = path.relative_to(root).as_posix()
        for n, line in lines(path):
            if WORKFLOW_FORBIDDEN.search(line):
                found.append(f"{rel}:{n}: forbidden workflow construct")
            match = USES.match(line)
            if match and not match.group(1).startswith("./") and not PINNED.match(match.group(1)):
                found.append(f"{rel}:{n}: action not pinned to a full commit SHA: {match.group(1)}")
    for base in ("crates", "e2e", "scripts", ".github"):
        for path in files(root, base, CODE_SUFFIXES):
            rel = path.relative_to(root).as_posix()
            if rel in SELF:
                continue
            found += [f"{rel}:{n}: force-kill command (program spec I8)" for n, line in lines(path) if FORCE_KILL.search(line)]
    return found


def main() -> int:
    found = violations(ROOT)
    for item in found:
        print(f"::error::{item}")
    if found:
        return 1
    print("integrity: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 5: Run the script tests and the gates on the repo**

Run:
```bash
cd "$WORK"
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p 'test_*.py' -v
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_version.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_integrity.py
```
Expected: all tests OK; `version 2.0.0-dev.1: consistent`; `integrity: clean` (the imported E2E specs contain no skip/only; the imported Rust tests no `#[ignore]` — the live integration test was dropped).

- [ ] **Step 6: Toolchain, cargo-deny, build-script rerun key, mutants config, coverage floor**

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "1.98.1"
components = ["rustfmt", "clippy", "llvm-tools-preview"]
targets = ["wasm32-unknown-unknown"]
profile = "minimal"
```

Append to `Cargo.toml`:
```toml

[profile.mutants]
inherits = "test"
debug = "none"
```

`deny.toml`:
```toml
[graph]
targets = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc", "wasm32-unknown-unknown"]
all-features = true

[advisories]
version = 2
yanked = "deny"
unmaintained = "workspace"
ignore = []

[licenses]
version = 2
allow = [
  "MIT", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause", "ISC",
  "Unicode-3.0", "Zlib", "MPL-2.0", "BSL-1.0", "CC0-1.0", "0BSD", "CDLA-Permissive-2.0", "Unlicense", "MIT-0",
]
confidence-threshold = 0.9
unused-allowed-license = "allow"

[bans]
multiple-versions = "allow"
wildcards = "allow"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
```

`crates/iem-core/build.rs` and `crates/iem-tray/build.rs`: replace the final block (from the comment `// Rebuild if git HEAD changes` through the `cargo:rerun-if-changed` line) with
```rust
    // Rebuild when git HEAD moves. Without a `.git` (cargo-mutants' scratch
    // copy, a source archive) key the rerun on the CI commit instead, so
    // BUILD_TIME does not change on every build and force a full rebuild.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let git_head = std::path::Path::new(&manifest_dir).join("../../.git/HEAD");
    if git_head.exists() {
        println!("cargo:rerun-if-changed={}", git_head.display());
    } else {
        println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    }
```
(`iem-tray/build.rs` keeps its trailing `tauri_build::build()`.)

`.cargo/mutants.toml`:
```toml
# cargo-mutants: PR gate in .github/workflows/ci.yml (diff-scoped; the shard
# count is the `shard:` matrix length, sized by the `mutants-list` job), full
# sweep in .github/workflows/mutation-full.yml (on demand). Both pass
# --copy-target=true after a warm-up build. Tests run per package only.
profile = "mutants"
test_tool = "nextest"

exclude_globs = [
  # Windows-only DPAPI calls: not compiled on the Linux runners; the `windows`
  # CI job runs their tests (`cargo test -p iem-server --lib pepper::`).
  "crates/iem-server/src/pepper/dpapi.rs",
  # Tauri tray shell: built and linted on Windows only.
  "crates/iem-tray/**",
]

exclude_re = [
  # Server bootstrap and WebSocket handlers need a bound server, a JWT and a
  # WS client; the Playwright suite covers them (whole-body replacements only).
  "replace start_server -> ",
  "replace run_server -> ",
  "replace handle_ws with \\(\\)",
  "handle_talkback_ws",
  "talkback_diagnostics_handler",
  # Test-only constructors.
  "new_for_test",
  "make_test_state_with_bad_reaper",
  # REAPER-era background loops (deleted in S5).
  "poll_reaper_and_broadcast",
  "try_persist_auto_snapshot",
  # Tunnel watchdog loop and the Windows sc.exe restart.
  "spawn_tunnel_watch",
  "restart_service_blocking",
  "sc_windows",
  # Browser-only fetches and context accessors in the UI; covered by
  # e2e/tests/site-links.spec.ts and e2e/tests/login-protection.spec.ts.
  "iem-ui/src/api\\.rs.*replace (login|get_site_links) ",
  "iem-ui/src/components/tunnel_status\\.rs.*replace site_links ",
]
```

`.github/coverage-floor` (bootstrap value; Task 15 sets the measured floor before the PR merges):
```text
0
```

No `.github/dependabot.yml`: version-update PRs would create `dependabot/*` branches (Scope decisions). Dependabot alerts are enabled in Task 14; crates and action pins are updated by hand on `dev`.

- [ ] **Step 7: Write `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main, dev]
  pull_request:
    branches: [main, dev]

permissions:
  contents: read

concurrency:
  group: ci-${{ github.event_name }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'push' }}

env:
  CARGO_TERM_COLOR: always

jobs:
  integrity:
    name: integrity
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Integrity scan (skips, pins, workflow constructs, force-kill)
        run: python3 scripts/check_integrity.py
      - name: Script self-tests
        run: |
          set -euo pipefail
          python3 -m unittest discover -s scripts -p 'test_*.py' -v
          python3 scripts/test_check_disposal_safety.py
      - name: Leptos disposal safety
        run: python3 scripts/check_disposal_safety.py
      - name: Version consistency
        run: python3 scripts/check_version.py
      - name: Tray icons
        run: |
          set -euo pipefail
          python3 -m venv "$RUNNER_TEMP/venv"
          "$RUNNER_TEMP/venv/bin/pip" install --quiet pillow==11.3.0
          cd crates/iem-tray/icons
          "$RUNNER_TEMP/venv/bin/python" verify_icons.py

  lint:
    name: lint
    runs-on: ubuntu-24.04
    timeout-minutes: 25
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - name: Placeholder UI dist (rust-embed needs the folder)
        run: mkdir -p crates/iem-ui/dist && echo placeholder > crates/iem-ui/dist/index.html
      - name: rustfmt
        run: cargo fmt --all -- --check
      - name: Clippy (native, all features; the tray is linted on Windows)
        run: cargo clippy --locked --workspace --exclude iem-tray --all-targets --all-features -- -D warnings
      - name: Clippy (UI for wasm32)
        run: cargo clippy --locked -p iem-ui --target wasm32-unknown-unknown -- -D warnings

  test:
    name: test
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - name: Placeholder UI dist (rust-embed needs the folder)
        run: mkdir -p crates/iem-ui/dist && echo placeholder > crates/iem-ui/dist/index.html
      - uses: taiki-e/install-action@7623a79cdfecb99d681017af368ca353d9f49bb5 # v2.87.19
        with:
          tool: cargo-llvm-cov@0.9.1
      - name: Tests with line coverage (iem-core, iem-server) against the floor
        run: |
          set -euo pipefail
          cargo llvm-cov --locked --package iem-core --package iem-server --all-features \
            --json --summary-only --output-path "$RUNNER_TEMP/coverage.json"
          pct="$(jq '.data[0].totals.lines.percent' "$RUNNER_TEMP/coverage.json")"
          floor="$(tr -d '[:space:]' < .github/coverage-floor)"
          echo "line coverage: ${pct}% (floor ${floor}%)"
          awk -v p="$pct" -v f="$floor" 'BEGIN { exit !(p >= f) }' \
            || { echo "::error::line coverage ${pct}% is below the floor ${floor}%"; exit 1; }
      - name: UI unit tests (native)
        run: cargo test --locked -p iem-ui --lib

  wasm:
    name: wasm
    runs-on: ubuntu-24.04
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - uses: taiki-e/install-action@7623a79cdfecb99d681017af368ca353d9f49bb5 # v2.87.19
        with:
          tool: trunk@0.21.14
      - name: Build the UI (release)
        working-directory: crates/iem-ui
        run: trunk build --release --locked
      - name: Verify the bundle
        run: |
          set -euo pipefail
          test -f crates/iem-ui/dist/index.html
          ls crates/iem-ui/dist/*.wasm
      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: wasm-dist
          path: crates/iem-ui/dist/
          retention-days: 1
          if-no-files-found: error

  e2e:
    name: e2e
    needs: [wasm]
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
        with:
          name: wasm-dist
          path: crates/iem-ui/dist
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - name: Build the server
        run: cargo build --locked --release -p iem-server --features standalone,audio
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0
        with:
          node-version: "22"
          cache: npm
          cache-dependency-path: e2e/package-lock.json
      - uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: ~/.cache/ms-playwright
          key: ${{ runner.os }}-playwright-${{ hashFiles('e2e/package-lock.json') }}
      - name: Install Playwright
        working-directory: e2e
        run: |
          set -euo pipefail
          npm ci
          npx playwright install --with-deps chromium
      - name: Provision the synthetic site and per-run PINs
        run: |
          set -euo pipefail
          site_dir="$RUNNER_TEMP/site"
          mkdir -p "$site_dir"
          cp config/test-site.toml "$site_dir/iemmixer.toml"
          export IEMMIXER_CONFIG="$site_dir/iemmixer.toml"
          read -r eng mem < <(python3 -c 'import random; a, b = random.SystemRandom().sample(range(10000), 2); print(f"{a:04d} {b:04d}")')
          echo "::add-mask::$eng"
          echo "::add-mask::$mem"
          {
            echo "IEMMIXER_CONFIG=$IEMMIXER_CONFIG"
            echo "E2E_ENGINEER_PIN=$eng"
            echo "E2E_MEMBER_PIN=$mem"
          } >> "$GITHUB_ENV"
          printf '%s\n' "$eng" | ./target/release/iem-server pin set-engineer
          members="$(python3 -c 'import sys, tomllib; site = tomllib.load(open(sys.argv[1], "rb")); print(" ".join(m["name"].lower() for m in site["members"] if m["name"].lower() != "engineer"))' "$IEMMIXER_CONFIG")"
          for member in $members; do
            printf '%s\n' "$mem" | ./target/release/iem-server pin set-member "$member"
          done
      - name: Start the server
        env:
          RUST_LOG: info
          PORT: "8080"
        run: |
          set -euo pipefail
          nohup ./target/release/iem-server > "$RUNNER_TEMP/server.log" 2>&1 &
          for i in $(seq 1 30); do
            if curl -sf http://127.0.0.1:8080/api/version > /dev/null; then echo "server ready after ${i}s"; exit 0; fi
            sleep 1
          done
          cat "$RUNNER_TEMP/server.log"
          exit 1
      - name: Playwright (mock E2E, clean console)
        working-directory: e2e
        env:
          CI: "true"
          E2E_BASE_URL: http://127.0.0.1:8080
        run: npx playwright test --reporter=list
      - name: Upload failure evidence
        if: failure()
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: e2e-failure
          path: |
            e2e/test-results/
            ${{ runner.temp }}/server.log
          retention-days: 7

  windows:
    name: windows
    needs: [wasm]
    runs-on: windows-2025
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8.0.1
        with:
          name: wasm-dist
          path: crates/iem-ui/dist
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - name: Clippy (server + tray, Windows-only code)
        run: cargo clippy --locked -p iem-server -p iem-tray --all-targets --all-features -- -D warnings
      - name: Pepper DPAPI tests
        run: "cargo test --locked -p iem-server --all-features --lib pepper::"
      - name: Build release binaries
        run: cargo build --locked --release -p iem-server -p iem-tray --features iem-server/standalone
      - name: Upload the binaries (push runs only — pull requests, forks included, never yield binaries)
        if: github.event_name == 'push'
        uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: windows-binaries
          path: |
            target/release/iem-server.exe
            target/release/iem-tray.exe
          retention-days: 7
          if-no-files-found: error

  supply-chain:
    name: supply-chain
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - uses: taiki-e/install-action@7623a79cdfecb99d681017af368ca353d9f49bb5 # v2.87.19
        with:
          tool: cargo-deny@0.20.2
      - name: cargo-deny (advisories, bans, licences, sources)
        run: cargo deny --locked check advisories bans licenses sources

  secrets:
    name: secrets
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - name: Install gitleaks 8.30.1 (checksum-verified)
        run: |
          set -euo pipefail
          curl -fsSL -o "$RUNNER_TEMP/gitleaks.tar.gz" https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_x64.tar.gz
          echo "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb  $RUNNER_TEMP/gitleaks.tar.gz" | sha256sum -c -
          tar -xzf "$RUNNER_TEMP/gitleaks.tar.gz" -C "$RUNNER_TEMP" gitleaks
      - name: gitleaks (full history)
        run: '"$RUNNER_TEMP/gitleaks" git --no-banner --redact --exit-code 1 .'
      - name: Private denylist and commit identities (tree and history)
        env:
          DENYLIST: ${{ secrets.DENYLIST }}
          EVENT: ${{ github.event_name }}
          SAME_REPO: ${{ github.event.pull_request.head.repo.full_name == github.repository }}
          # On pull requests scan the PR head's history, not GitHub's synthetic merge commit
          SCAN_REV: ${{ github.event.pull_request.head.sha || github.sha }}
        run: |
          set -euo pipefail
          if [ -z "$DENYLIST" ]; then
            if [ "$EVENT" = "pull_request" ] && [ "$SAME_REPO" != "true" ]; then
              echo "::error::Fork pull request: the private denylist is unavailable here; a maintainer runs the scan locally before merging (CONTRIBUTING.md), and the merge push to dev runs it again."
              exit 1
            fi
            echo "::error::The DENYLIST secret is missing."
            exit 1
          fi
          umask 077
          printf '%s\n' "$DENYLIST" > "$RUNNER_TEMP/denylist.txt"
          python3 scripts/denylist_scan.py --denylist "$RUNNER_TEMP/denylist.txt" \
            --allow scripts/denylist-allow.txt --identities scripts/allowed-identities.txt \
            --tree HEAD --commits "$SCAN_REV"

  version:
    name: version
    if: github.event_name == 'pull_request' && github.base_ref == 'main'
    runs-on: ubuntu-24.04
    timeout-minutes: 5
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - name: Head version greater than main
        run: python3 scripts/check_version.py --base-ref origin/main

  mutants-list:
    name: mutants-list
    if: github.event_name == 'push' && github.ref == 'refs/heads/dev'
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: taiki-e/install-action@7623a79cdfecb99d681017af368ca353d9f49bb5 # v2.87.19
        with:
          tool: cargo-mutants@27.1.0
      - name: The next dev→main PR's mutants fit the shard matrix
        env:
          # Mutants one shard tests within its 20-minute budget, setup included.
          # Task 16 measures shard durations and adjusts this value.
          MUTANTS_PER_SHARD: "20"
        run: |
          set -euo pipefail
          git diff origin/main...HEAD -- '*.rs' > "$RUNNER_TEMP/pr.diff"
          count=0
          if [ -s "$RUNNER_TEMP/pr.diff" ]; then
            cargo mutants --list --in-diff "$RUNNER_TEMP/pr.diff" \
              --package iem-core --package iem-server --package iem-ui > "$RUNNER_TEMP/mutants.txt"
            count="$(wc -l < "$RUNNER_TEMP/mutants.txt")"
          fi
          [ "$(grep -cE '^ +shard: \[' .github/workflows/ci.yml)" -eq 1 ] \
            || { echo "::error::expected exactly one shard matrix in ci.yml"; exit 1; }
          commas="$(grep -E '^ +shard: \[' .github/workflows/ci.yml | tr -cd ',' | wc -c)"
          shards=$((commas + 1))
          needed=$(( (count + MUTANTS_PER_SHARD - 1) / MUTANTS_PER_SHARD ))
          echo "mutants in the dev→main diff: $count; shards: $shards; needed at <= $MUTANTS_PER_SHARD per shard: $needed"
          [ "$needed" -le "$shards" ] \
            || { echo "::error::$count mutants need $needed shards but the matrix has $shards: extend the shard list"; exit 1; }

  mutation-warmup:
    name: mutation-warmup
    if: github.event_name == 'pull_request'
    needs: [test]
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
        with:
          shared-key: mutation
      - name: Placeholder UI dist (rust-embed needs the folder)
        run: mkdir -p crates/iem-ui/dist && echo placeholder > crates/iem-ui/dist/index.html
      - uses: taiki-e/install-action@7623a79cdfecb99d681017af368ca353d9f49bb5 # v2.87.19
        with:
          tool: cargo-nextest@0.9.146
      - name: Build the mutants profile once (the shards restore this cache)
        run: cargo nextest run --locked --cargo-profile mutants --no-run -p iem-core -p iem-server -p iem-ui --all-features --all-targets

  mutation:
    name: mutation shard ${{ matrix.shard }}
    if: github.event_name == 'pull_request'
    needs: [mutation-warmup]
    runs-on: ubuntu-24.04
    timeout-minutes: 20
    strategy:
      fail-fast: false
      matrix:
        # 0-based (cargo-mutants --shard k/n); the length is the shard count
        shard: [0, 1, 2, 3, 4, 5, 6, 7]
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
        with:
          shared-key: mutation
          save-if: false
      - name: Placeholder UI dist (rust-embed needs the folder)
        run: mkdir -p crates/iem-ui/dist && echo placeholder > crates/iem-ui/dist/index.html
      - uses: taiki-e/install-action@7623a79cdfecb99d681017af368ca353d9f49bb5 # v2.87.19
        with:
          tool: cargo-mutants@27.1.0,cargo-nextest@0.9.146
      - name: Warm target (dependencies restored; workspace crates build once, then are copied per job)
        run: cargo nextest run --locked --cargo-profile mutants --no-run -p iem-core -p iem-server -p iem-ui --all-features --all-targets
      - name: Diff-scoped mutants
        run: |
          set -euo pipefail
          git diff "origin/${{ github.base_ref }}...HEAD" -- '*.rs' > "$RUNNER_TEMP/pr.diff"
          if [ ! -s "$RUNNER_TEMP/pr.diff" ]; then
            echo "No Rust changes against ${{ github.base_ref }}: no mutants to test."
            exit 0
          fi
          cargo mutants --in-diff "$RUNNER_TEMP/pr.diff" --baseline=skip --jobs 2 --copy-target=true \
            --shard "${{ matrix.shard }}/${{ strategy.job-total }}" \
            --package iem-core --package iem-server --package iem-ui \
            --all-features -- --all-targets
```

- [ ] **Step 8: Write `.github/workflows/mutation-full.yml` (on demand, `/mutation-sweep`)**

```yaml
name: mutation-full

on:
  workflow_dispatch:

permissions:
  contents: read

jobs:
  mutants:
    name: mutation full shard ${{ matrix.shard }}
    runs-on: ubuntu-24.04
    timeout-minutes: 180
    strategy:
      fail-fast: false
      matrix:
        # 0-based (cargo-mutants --shard k/n)
        shard: [0, 1, 2, 3, 4, 5, 6, 7]
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Rust toolchain (rust-toolchain.toml)
        run: rustup toolchain install
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - name: Placeholder UI dist (rust-embed needs the folder)
        run: mkdir -p crates/iem-ui/dist && echo placeholder > crates/iem-ui/dist/index.html
      - uses: taiki-e/install-action@7623a79cdfecb99d681017af368ca353d9f49bb5 # v2.87.19
        with:
          tool: cargo-mutants@27.1.0,cargo-nextest@0.9.146
      - name: Warm target (copied into each cargo-mutants build directory)
        run: cargo nextest run --locked --cargo-profile mutants --no-run -p iem-core -p iem-server -p iem-ui --all-features --all-targets
      - name: Full-tree mutants (survivors are reported, then filed as test-quality issues)
        run: |
          set -uo pipefail
          cargo mutants --baseline=skip --jobs 2 --copy-target=true --shard "${{ matrix.shard }}/${{ strategy.job-total }}" \
            --package iem-core --package iem-server --package iem-ui --all-features -- --all-targets
          code=$?
          if [ -f mutants.out/missed.txt ]; then echo "--- missed mutants ---"; cat mutants.out/missed.txt; fi
          case "$code" in
            0|2|3) echo "cargo-mutants finished (exit $code)"; exit 0 ;;
            *) echo "::error::cargo-mutants failed to run (exit $code)"; exit "$code" ;;
          esac
      - uses: actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7.0.1
        with:
          name: mutants-shard-${{ matrix.shard }}
          path: mutants.out/
          retention-days: 14
          if-no-files-found: error
```
(Exit 2 = missed mutants, 3 = timeouts: the sweep's contract is "ran + survivors reported"; the `/mutation-sweep` skill files them as issues.)

- [ ] **Step 9: Pre-resolve the two cargo-deny findings the imported lockfile is likely to raise**

cargo-deny itself runs only in CI (a third-party cargo subcommand is not a Tier 0 local command), so check the resolved lockfile for the two known candidates with non-compiling commands:
```bash
set -euo pipefail
cd "$WORK"
grep -A1 -E '^name = "(glib|aws-lc-sys)"$' Cargo.lock || echo "neither glib nor aws-lc-sys in Cargo.lock"
cargo tree --locked --target all -e normal,build -i aws-lc-sys 2>/dev/null | head -5 || true
```
- **`glib` below 0.20** (Tauri's Linux GTK stack; advisory RUSTSEC-2024-0429, unsound `VariantStrIter`): it enters the graph only because `deny.toml` evaluates `x86_64-unknown-linux-gnu` with all features, and the tray ships for Windows only. File the tracking issue, then ignore the advisory with its number:
  ```bash
  body="$(mktemp)"
  printf '%s\n' "cargo-deny flags RUSTSEC-2024-0429 (glib < 0.20, unsound VariantStrIter). glib enters only through Tauri's Linux GTK stack; iem-tray is built and shipped for Windows only and no iemmixer code calls the unsound API. deny.toml ignores the advisory with this issue's number; remove the ignore when Tauri moves to glib >= 0.20." "" "Scope-gate: an advisory ignored in deny.toml needs a tracking issue (security, supply chain)." > "$body"
  gh issue create -R "$REPO" --title "cargo-deny: glib 0.18 advisory via Tauri's Linux stack" --body-file "$body"
  ```
  and in `deny.toml` `[advisories]`: `ignore = [{ id = "RUSTSEC-2024-0429", reason = "glib < 0.20 only via Tauri's Linux GTK stack; the tray ships for Windows only; tracked in #<N>" }]`.
- **`aws-lc-sys`** (licence expression includes `OpenSSL`, not in the allow list; it comes with rustls' default `aws-lc-rs` provider): add a crate-scoped exception, never a global allowance:
  ```toml
  # aws-lc-sys (rustls' default aws-lc-rs crypto provider) is ISC AND (Apache-2.0 OR ISC) AND OpenSSL
  [[licenses.exceptions]]
  crate = "aws-lc-sys"
  allow = ["OpenSSL"]
  ```
Add each entry only if its crate is in the lockfile; any other cargo-deny finding is handled in Task 15.

- [ ] **Step 10: Local checks (non-compiling) and commit**

```bash
set -euo pipefail
cd "$WORK"
rustup toolchain install
cargo metadata --locked --format-version 1 > /dev/null
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_integrity.py
python3 -c 'import yaml; [yaml.safe_load(open(p)) for p in (".github/workflows/ci.yml", ".github/workflows/mutation-full.yml")]; print("yaml ok")'
cargo fmt --all
git add .github rust-toolchain.toml deny.toml .cargo Cargo.toml scripts crates/iem-core/build.rs crates/iem-tray/build.rs
git commit -m "ci: hosted CI skeleton, version and integrity gates, cargo-deny, mutation config" -m "Build scripts key their rerun on GITHUB_SHA when .git is absent (cargo-mutants scratch copies). Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected: toolchain 1.98.1 installed, `integrity: clean`, `yaml ok`.

---

### Task 6: Site config in TOML and generated runtime secrets

**Files:**
- Create: `config/iemmixer.example.toml`, `config/test-site.toml`, `crates/iem-server/src/secrets.rs`
- Modify: `crates/iem-core/Cargo.toml`, `crates/iem-core/src/config.rs`, `crates/iem-server/src/lib.rs`, `crates/iem-server/src/bin/server.rs`, `crates/iem-server/src/auth.rs` (test helper only), `crates/iem-tray/src/lib.rs`, `crates/iem-ui/src/pages/landing.rs` (config hint)

**Interfaces:**
- Produces: `iem_core::Config::load(path) -> Result<Config, ConfigError>` (TOML, `deny_unknown_fields`); new fields `lan_url: Option<String>`, `vapid_subject: String`; `jwt_secret`/`vapid_private_key` stay `String` fields but `#[serde(skip)]`; removed: `validate_security`, `persist_jwt_to_config`, `JWT_CONFIG_KEY`, `default_jwt_secret`.
- Produces: `iem_server::secrets::{SECRETS_DIR, JWT_SECRET_FILE, VAPID_PRIVATE_FILE, Secrets { jwt_secret: String, vapid_private_key: String }, load_or_create(dir: &Path) -> io::Result<Secrets>}` and `pub(crate) fn write_new_private(path: &Path, data: &[u8]) -> io::Result<()>`; `start_server` fills `config.jwt_secret`/`vapid_private_key` from `<config_dir>/secrets/`.
- Produces: `IEMMIXER_CONFIG` (path of the site TOML) for the `iem-server` binary.

- [ ] **Step 1: Write the committed site files**

`config/iemmixer.example.toml`:
```toml
# iemmixer site configuration — example documenting every key. The real site
# file lives only in the private ops repository and on the PC.
# Secrets never go here: the JWT key, VAPID key, PIN hashes and pepper are
# generated on the PC under `<config dir>/secrets/`. Unknown keys are errors.

# REAPER HTTP control surface (the predecessor engine; replaced in S5).
reaper_url = "http://127.0.0.1:8080"
# HTTP port of the web app.
port = 80
# HTTPS for phones (PWA install); certificate and key next to this file.
tls = false
https_port = 443
tls_cert = "cert.pem"
tls_key = "key.pem"
# Public host served through the tunnel (HTTPS redirect, tray "Copy URL",
# LAN hint on pages opened through it).
https_domain = "mixer.example.org"
# Local-network URL shown to band members while the tunnel is down.
lan_url = "http://10.0.0.10"
# Public IP of the venue network (LAN/WAN badge); detected when absent.
# local_public_ip = "198.51.100.7"
# Web Push contact (VAPID "sub" claim).
vapid_subject = "mailto:admin@example.org"
# cloudflared readiness endpoint polled by the tunnel watchdog.
tunnel_ready_url = "http://127.0.0.1:20241/ready"
# Automatic backups (local time, HH:MM) and their retention in days.
backup_schedule = ["13:00", "21:00"]
backup_retention_days = 60

# Dante TX pair per member bus, keyed by the REAPER track-name prefix.
[dante_outputs]
MEMBER1 = [71, 72]
ENGINEER = [91, 92]

# Band members (login grid); the id is the lower-cased name.
[[members]]
name = "Member1"
dante_output_l = 71
dante_output_r = 72

[[members]]
name = "Engineer"
dante_output_l = 91
dante_output_r = 92

# Inputs on every member's mixer. `category` = mics | stems | tech (derived
# from the name when absent); a member owns the input named "<MEMBER> mic".
[[inputs]]
name = "MEMBER1 mic"
dante_input = 101
default_level_db = 0.0

[[inputs]]
name = "CONTENT"
dante_input = 124
default_level_db = 0.0
category = "tech"
```

`config/test-site.toml`:
```toml
# Synthetic site for CI, E2E and tests: the real site's shape (9 members plus
# the engineer, 24 inputs) with placeholder names and synthetic channel
# numbers. Never real site data (program spec P6). Input names follow program
# spec §3.1 (KEYS, CONTENT, the stems) except where the imported REAPER-era
# code keys on a name pattern until S5: "<MEMBER> mic" (ownership), "gtr"
# (category), "HANDn mic" and "ENGINEER mic" (tech).
reaper_url = "http://127.0.0.1:1"
port = 8080
tls = false
https_domain = "mixer.example.org"
lan_url = "http://10.0.0.10"
vapid_subject = "mailto:admin@example.org"
backup_schedule = ["13:00", "21:00"]
backup_retention_days = 60

[dante_outputs]
MEMBER1 = [71, 72]
MEMBER2 = [73, 74]
MEMBER3 = [75, 76]
MEMBER4 = [77, 78]
MEMBER5 = [79, 80]
MEMBER6 = [81, 82]
MEMBER7 = [83, 84]
MEMBER8 = [85, 86]
MEMBER9 = [87, 88]
ENGINEER = [91, 92]

[[members]]
name = "Member1"
dante_output_l = 71
dante_output_r = 72

[[members]]
name = "Member2"
dante_output_l = 73
dante_output_r = 74

[[members]]
name = "Member3"
dante_output_l = 75
dante_output_r = 76

[[members]]
name = "Member4"
dante_output_l = 77
dante_output_r = 78

[[members]]
name = "Member5"
dante_output_l = 79
dante_output_r = 80

[[members]]
name = "Member6"
dante_output_l = 81
dante_output_r = 82

[[members]]
name = "Member7"
dante_output_l = 83
dante_output_r = 84

[[members]]
name = "Member8"
dante_output_l = 85
dante_output_r = 86

[[members]]
name = "Member9"
dante_output_l = 87
dante_output_r = 88

[[members]]
name = "Engineer"
dante_output_l = 91
dante_output_r = 92

[[inputs]]
name = "MEMBER1 mic"
dante_input = 101
default_level_db = 0.0

[[inputs]]
name = "MEMBER2 mic"
dante_input = 102
default_level_db = 0.0

[[inputs]]
name = "MEMBER3 mic"
dante_input = 103
default_level_db = 0.0

[[inputs]]
name = "MEMBER4 mic"
dante_input = 104
default_level_db = 0.0

[[inputs]]
name = "MEMBER4 gtr"
dante_input = 105
default_level_db = 0.0

[[inputs]]
name = "MEMBER5 mic"
dante_input = 106
default_level_db = 0.0

[[inputs]]
name = "MEMBER6 mic"
dante_input = 107
default_level_db = 0.0

[[inputs]]
name = "MEMBER7 mic"
dante_input = 108
default_level_db = 0.0

[[inputs]]
name = "KEYS"
dante_input = 109
default_level_db = 0.0
category = "mics"

[[inputs]]
name = "MEMBER8 mic"
dante_input = 110
default_level_db = 0.0

[[inputs]]
name = "MEMBER9 mic"
dante_input = 111
default_level_db = 0.0

[[inputs]]
name = "DRUMS"
dante_input = 112
default_level_db = 0.0

[[inputs]]
name = "BASS"
dante_input = 113
default_level_db = 0.0

[[inputs]]
name = "INST"
dante_input = 114
default_level_db = 0.0

[[inputs]]
name = "OTHER"
dante_input = 115
default_level_db = 0.0

[[inputs]]
name = "BGVS"
dante_input = 116
default_level_db = 0.0

[[inputs]]
name = "CLICK"
dante_input = 117
default_level_db = 0.0

[[inputs]]
name = "GUIDE"
dante_input = 118
default_level_db = 0.0

[[inputs]]
name = "IEMONLY"
dante_input = 119
default_level_db = 0.0

[[inputs]]
name = "HAND1 mic"
dante_input = 120
default_level_db = 0.0

[[inputs]]
name = "HAND2 mic"
dante_input = 121
default_level_db = 0.0

[[inputs]]
name = "HAND3 mic"
dante_input = 122
default_level_db = 0.0

[[inputs]]
name = "ENGINEER mic"
dante_input = 123
default_level_db = 0.0

[[inputs]]
name = "CONTENT"
dante_input = 124
default_level_db = 0.0
category = "tech"
```

- [ ] **Step 2: Write the failing config tests (`crates/iem-core/src/config.rs`, `mod tests`)**

Delete the four tests of the removed secret persistence — `test_validate_security_generates_on_default`, `test_validate_security_keeps_custom`, `test_validate_security_persists_to_file` and `test_validate_security_no_overwrite_custom` (they call `validate_security`, which Step 3 deletes; `secrets::tests` in Step 4 covers generation, keeping and never overwriting). Replace `test_dante_outputs_yaml_parsing`, `test_backup_schedule_custom`, `test_tunnel_ready_url_default_and_yaml_default` and `test_tunnel_ready_url_custom` with (Dante pairs from the synthetic map):
```rust
    #[test]
    fn test_dante_outputs_toml_parsing() {
        let text = r#"
reaper_url = "http://127.0.0.1:8080"
port = 80
inputs = []

[dante_outputs]
MEMBER1 = [71, 72]
MEMBER2 = [73, 74]
MEMBER3 = [75, 76]
"#;
        let config: Config = toml::from_str(text).expect("TOML should parse");
        assert_eq!(config.dante_outputs.get("MEMBER1"), Some(&[71, 72]));
        assert_eq!(config.dante_outputs.get("MEMBER3"), Some(&[75, 76]));
    }

    #[test]
    fn test_backup_schedule_custom() {
        let text = "reaper_url = \"http://test:8080\"\nbackup_schedule = [\"09:00\", \"13:00\", \"18:00\", \"22:00\"]\nbackup_retention_days = 30\n";
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(config.backup_schedule.len(), 4);
        assert_eq!(config.backup_retention_days, 30);
    }

    #[test]
    fn test_tunnel_ready_url_default_and_toml_default() {
        assert_eq!(Config::default().tunnel_ready_url, "http://127.0.0.1:20241/ready");
        let config: Config = toml::from_str("reaper_url = \"http://test:8080\"\n").unwrap();
        assert_eq!(config.tunnel_ready_url, "http://127.0.0.1:20241/ready");
    }

    #[test]
    fn test_tunnel_ready_url_custom() {
        let config: Config = toml::from_str("tunnel_ready_url = \"http://127.0.0.1:9999/ready\"\n").unwrap();
        assert_eq!(config.tunnel_ready_url, "http://127.0.0.1:9999/ready");
    }

    #[test]
    fn test_secrets_are_never_read_from_the_site_file() {
        for key in ["jwt_secret", "vapid_private_key"] {
            let text = format!("{key} = \"value\"\n");
            assert!(toml::from_str::<Config>(&text).is_err(), "{key} must be rejected");
        }
        assert!(Config::default().jwt_secret.is_empty(), "no compiled-in JWT key");
    }

    #[test]
    fn test_unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("reaper_urll = \"http://x\"\n").is_err());
    }

    #[test]
    fn test_site_extras_default_and_parse() {
        let defaults = Config::default();
        assert_eq!(defaults.lan_url, None);
        assert_eq!(defaults.vapid_subject, "mailto:admin@example.org");
        let config: Config = toml::from_str(
            "lan_url = \"http://10.0.0.20\"\nvapid_subject = \"mailto:ops@example.org\"\n",
        )
        .unwrap();
        assert_eq!(config.lan_url.as_deref(), Some("http://10.0.0.20"));
        assert_eq!(config.vapid_subject, "mailto:ops@example.org");
    }

    #[test]
    fn test_committed_site_files_parse() {
        let site: Config = toml::from_str(include_str!("../../../config/test-site.toml"))
            .expect("config/test-site.toml");
        assert_eq!(site.members.len(), 10);
        assert_eq!(site.inputs.len(), 24);
        assert_eq!(site.dante_outputs.len(), 10);
        assert_eq!(site.lan_url.as_deref(), Some("http://10.0.0.10"));
        assert_eq!(site.https_domain.as_deref(), Some("mixer.example.org"));
        let example: Config = toml::from_str(include_str!("../../../config/iemmixer.example.toml"))
            .expect("config/iemmixer.example.toml");
        assert_eq!(example.members.len(), 2);
    }

    #[test]
    fn test_load_reads_a_toml_file_and_reports_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.toml");
        std::fs::write(&good, "port = 8081\n").unwrap();
        assert_eq!(Config::load(&good).unwrap().port, 8081);
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "port = \"eighty\"\n").unwrap();
        assert!(matches!(Config::load(&bad), Err(ConfigError::Parse(_))));
        assert!(matches!(Config::load(dir.path().join("missing.toml")), Err(ConfigError::Io(_))));
    }
```
Expected in CI (Task 15): these pass only with Steps 3–4.

- [ ] **Step 3: Switch iem-core to TOML and drop secret persistence**

`crates/iem-core/Cargo.toml` `[dependencies]`: replace `serde_yaml = { version = "0.9", optional = true }` with `toml = { version = "0.9", optional = true }`; delete the `rand_core` line; `[features]`: `config = ["dep:toml"]`, `vapid = ["dep:p256", "dep:base64"]`; add
```toml
[dev-dependencies]
tempfile = "3"
```

`crates/iem-core/src/config.rs`:
- Struct attributes: `#[derive(Debug, Clone, Serialize, Deserialize)]` then `#[serde(deny_unknown_fields)]` on `pub struct Config`.
- Replace the `jwt_secret` and `vapid_private_key` fields (with their doc comments and serde attributes) by:
```rust
    /// JWT signing key. Never read from the site file: the server loads it
    /// from `<config dir>/secrets/jwt_secret` (`iem_server::secrets`).
    #[serde(skip)]
    pub jwt_secret: String,

    /// VAPID private key (base64url P-256 scalar). Never read from the site
    /// file: loaded from `<config dir>/secrets/vapid_private`.
    #[serde(skip)]
    pub vapid_private_key: String,
```
- After the `local_public_ip` field add:
```rust
    /// Local-network URL of the mixer, shown to band members while the
    /// tunnel is down (e.g. "http://10.0.0.10").
    #[serde(default)]
    pub lan_url: Option<String>,

    /// Web Push contact (VAPID `sub` claim).
    #[serde(default = "default_vapid_subject")]
    pub vapid_subject: String,
```
- Delete `fn default_jwt_secret()`; add
```rust
fn default_vapid_subject() -> String {
    "mailto:admin@example.org".to_string()
}
```
- In `impl Default for Config`: `jwt_secret: String::new(),`, keep `vapid_private_key: String::new(),`, add `lan_url: None,` and `vapid_subject: default_vapid_subject(),`.
- Replace `load` with:
```rust
    /// Load the site configuration from a TOML file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let content =
            std::fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::Io(e.to_string()))?;
        toml::from_str(&content).map_err(|e| ConfigError::Parse(e.to_string()))
    }
```
- Delete `const JWT_CONFIG_KEY`, `pub fn validate_security` and `fn persist_jwt_to_config`.

- [ ] **Step 4: Write the failing secrets tests, then `crates/iem-server/src/secrets.rs`**

```rust
//! Runtime secrets generated on the PC — never in git or the site config
//! (program spec §5.3): the JWT signing key and the VAPID private key, one
//! owner-only file each in `<config dir>/secrets/`. A file is created once and
//! never overwritten; an empty or unreadable file is an error, not a reason to
//! generate a new secret.

use std::io::{self, Write};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::{OsRng, RngCore};

/// Sub-directory of the config directory holding every runtime secret.
pub const SECRETS_DIR: &str = "secrets";
/// JWT signing key (base64url of 32 random bytes).
pub const JWT_SECRET_FILE: &str = "jwt_secret";
/// VAPID private key (base64url P-256 scalar).
pub const VAPID_PRIVATE_FILE: &str = "vapid_private";

/// Secrets the server needs at start-up.
#[derive(Clone)]
pub struct Secrets {
    pub jwt_secret: String,
    pub vapid_private_key: String,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets").finish_non_exhaustive()
    }
}

/// Load the secrets from `dir`, creating each missing one.
pub fn load_or_create(dir: &Path) -> io::Result<Secrets> {
    std::fs::create_dir_all(dir)?;
    let jwt_secret = read_or_create(&dir.join(JWT_SECRET_FILE), new_jwt_secret)?;
    let vapid_private_key = read_or_create(&dir.join(VAPID_PRIVATE_FILE), new_vapid_private_key)?;
    Ok(Secrets { jwt_secret, vapid_private_key })
}

fn new_jwt_secret() -> String {
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    URL_SAFE_NO_PAD.encode(key)
}

fn new_vapid_private_key() -> String {
    URL_SAFE_NO_PAD.encode(p256::SecretKey::random(&mut OsRng).to_bytes())
}

fn read_or_create(path: &Path, generate: fn() -> String) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let value = text.trim().to_string();
            if value.is_empty() {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is empty", path.display()),
                ))
            } else {
                Ok(value)
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let value = generate();
            write_new_private(path, value.as_bytes())?;
            tracing::info!(path = %path.display(), "generated a new runtime secret");
            Ok(value)
        }
        Err(e) => Err(e),
    }
}

/// Create `path` exclusively (never overwrite) and write `data`; owner-only on Unix.
pub(crate) fn write_new_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn creates_both_secrets_once_and_reloads_them() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path()).unwrap();
        let second = load_or_create(dir.path()).unwrap();
        assert_eq!(first.jwt_secret, second.jwt_secret);
        assert_eq!(first.vapid_private_key, second.vapid_private_key);
        assert_eq!(URL_SAFE_NO_PAD.decode(&first.jwt_secret).unwrap().len(), 32);
        assert!(iem_core::Config::vapid_public_key_base64url(&first.vapid_private_key).is_ok());
    }

    #[test]
    fn two_installations_get_different_secrets() {
        let a = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        let b = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        assert_ne!(a.jwt_secret, b.jwt_secret);
        assert_ne!(a.vapid_private_key, b.vapid_private_key);
    }

    #[test]
    fn an_empty_secret_file_is_an_error_not_a_new_secret() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(JWT_SECRET_FILE), "").unwrap();
        assert_eq!(load_or_create(dir.path()).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read_to_string(dir.path().join(JWT_SECRET_FILE)).unwrap(), "");
    }

    #[test]
    fn an_existing_secret_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(JWT_SECRET_FILE), "kept-value\n").unwrap();
        assert_eq!(load_or_create(dir.path()).unwrap().jwt_secret, "kept-value");
    }

    #[test]
    fn write_new_private_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        write_new_private(&path, b"first").unwrap();
        assert!(write_new_private(&path, b"second").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
    }

    #[cfg(unix)]
    #[test]
    fn secret_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_create(dir.path()).unwrap();
        for name in [JWT_SECRET_FILE, VAPID_PRIVATE_FILE] {
            let mode = std::fs::metadata(dir.path().join(name)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{name}");
        }
    }

    #[test]
    fn debug_output_never_shows_secrets() {
        let secrets = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        let shown = format!("{secrets:?}");
        assert!(!shown.contains(&secrets.jwt_secret));
        assert!(!shown.contains(&secrets.vapid_private_key));
    }
}
```

- [ ] **Step 5: Wire the secrets into the server, the binary and the tray**

`crates/iem-server/src/lib.rs`: add `pub mod secrets;` to the module list (alphabetical, after `pub mod routes;`); at the top of `start_server`, replace `let state = AppState::new(server_config.config, &server_config.config_dir);` with
```rust
    let mut config = server_config.config;
    let secrets = secrets::load_or_create(&server_config.config_dir.join(secrets::SECRETS_DIR))?;
    config.jwt_secret = secrets.jwt_secret;
    config.vapid_private_key = secrets.vapid_private_key;
    let state = AppState::new(config, &server_config.config_dir);
```
`crates/iem-server/src/bin/server.rs`: replace the config block (from `// Load config from file or environment variable` through the closing `};` of `let config = match …`) with
```rust
    // Site config (TOML). A missing or invalid file stops the server.
    let config_path =
        std::env::var("IEMMIXER_CONFIG").unwrap_or_else(|_| "iemmixer.toml".to_string());
    let config = Config::load(&config_path)
        .map_err(|e| anyhow::anyhow!("loading site config {config_path}: {e}"))?;
    tracing::info!(
        path = %config_path,
        members = config.members.len(),
        inputs = config.inputs.len(),
        "site config loaded"
    );
```
and delete the two `if config.members.is_empty()` / `if config.inputs.is_empty()` warnings (a TOML site always carries them; Task 11 replaces this file anyway).
`crates/iem-server/src/auth.rs` `mod tests`, replace the body of `fn test_config() -> Config` with:
```rust
        Config {
            jwt_secret: "test_secret_for_auth_testing".to_string(),
            engineer_pin: Some("9999".to_string()),
            ..Config::default()
        }
```
and delete the now-unused `use std::collections::HashMap;` in that test module.
`crates/iem-tray/src/lib.rs`: replace the config-loading block (from `// Load configuration` through the end of `let config = if config_path.exists() { … };`) with
```rust
    // Site configuration (TOML); runtime secrets are generated by the server.
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("iemmixer")
        .join("iemmixer.toml");

    let config = if config_path.exists() {
        Config::load(&config_path).unwrap_or_else(|e| {
            tracing::error!(error = %e, path = %config_path.display(), "Failed to load the site config, using defaults");
            Config::default()
        })
    } else {
        tracing::warn!(path = %config_path.display(), "No site config found, using defaults");
        Config::default()
    };
```
(The tray's glue — this fallback, the menu and "Copy URL" — is proven by the S6 checklist, program spec F27; its logic lives in `Config::load` and `Config::share_url`, both unit-tested in iem-core.)
`crates/iem-ui/src/pages/landing.rs`, empty state: `"Add band members in config.yaml to get started."` → `"Add band members in iemmixer.toml to get started."` and `"Config location: %APPDATA%\\iem-mixer\\config.yaml"` → `"Config location: %APPDATA%\\iemmixer\\iemmixer.toml"`.

- [ ] **Step 6: Lockfile, format, commit**

```bash
set -euo pipefail
cd "$WORK"
cargo metadata --format-version 1 > /dev/null
cargo metadata --locked --format-version 1 > /dev/null
grep -c 'serde_yaml' Cargo.lock   # expect 0 (no other crate used it)
cargo fmt --all
if git grep -n -E 'validate_security|persist_jwt_to_config|JWT_CONFIG_KEY' -- crates; then
  echo "leftover references to the removed secret persistence (above)" >&2; exit 1
fi
git add config crates Cargo.lock
git commit -m "feat(config): TOML site config, generated runtime secrets, no secrets in the site file" -m "Committed config/iemmixer.example.toml and the synthetic config/test-site.toml; the real site.toml lives in the private ops repo. Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected in CI (Task 15): config and secrets tests pass.

---

### Task 7: Hosts from the site config (LAN URL, public host, VAPID contact, tray URL)

**Files:**
- Modify: `crates/iem-core/src/tunnel.rs`, `crates/iem-core/src/config.rs`, `crates/iem-server/src/routes.rs`, `crates/iem-server/src/push.rs`, `crates/iem-server/src/proxy.rs` (SOS push call), `crates/iem-server/src/lib.rs` (HTTPS log line), `crates/iem-ui/src/api.rs`, `crates/iem-ui/src/router.rs`, `crates/iem-ui/src/components/tunnel_status.rs`, `crates/iem-tray/src/lib.rs`, `crates/iem-tray/src/tray.rs`

**Interfaces:**
- Consumes: `Config.lan_url`, `Config.https_domain`, `Config.vapid_subject` (Task 6).
- Produces: `iem_core::tunnel::{SiteLinks { lan_url: Option<String>, public_host: Option<String> }, member_banner_text(Option<&str>) -> String, reconnect_lan_hint(&str) -> String, needs_lan_hint(&str, Option<&str>) -> bool}`; `Config::site_links(&self) -> SiteLinks`; `Config::share_url(&self) -> Option<String>` (the tray's "Copy URL"); `GET /api/site` → `SiteLinks` JSON; UI context `RwSignal<SiteLinks>`; `push::build_vapid_header(key, endpoint, subject)`, `push::send_push(client, key, subject, sub, payload)`, `push::send_push_to_engineers(client, key, subject, store, payload)`; `tray::setup_tray(app, port, share_url: Option<String>)`.

- [ ] **Step 1: Write the failing core tests (`crates/iem-core/src/tunnel.rs`)**

Replace the tests `member_banner_points_to_the_lan_url` and `lan_hint_only_for_the_public_host` with:
```rust
    #[test]
    fn member_banner_points_to_the_configured_lan_url() {
        assert_eq!(
            member_banner_text(Some("http://10.0.0.10")),
            "Internetový prístup nefunguje — na tejto sieti otvorte http://10.0.0.10"
        );
        assert_eq!(member_banner_text(None), "Internetový prístup nefunguje");
        assert_eq!(
            reconnect_lan_hint("http://10.0.0.10"),
            "Ak nejde internet a ste na miestnej sieti, otvorte http://10.0.0.10"
        );
    }

    #[test]
    fn lan_hint_only_for_the_configured_public_host() {
        let host = Some("mixer.example.org");
        assert!(needs_lan_hint("mixer.example.org", host));
        assert!(needs_lan_hint("MIXER.example.org", host));
        assert!(!needs_lan_hint("10.0.0.10", host));
        assert!(!needs_lan_hint("localhost", host));
        assert!(!needs_lan_hint("mixer.example.org", None));
    }

    #[test]
    fn site_links_wire_format() {
        let links = SiteLinks {
            lan_url: Some("http://10.0.0.10".to_string()),
            public_host: Some("mixer.example.org".to_string()),
        };
        let json = serde_json::to_value(&links).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"lan_url": "http://10.0.0.10", "public_host": "mixer.example.org"})
        );
        let empty: SiteLinks = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, SiteLinks::default());
    }
```
And in `crates/iem-core/src/config.rs` `mod tests`:
```rust
    #[test]
    fn test_site_links_come_from_lan_url_and_https_domain() {
        let config = Config {
            lan_url: Some("http://10.0.0.10".to_string()),
            https_domain: Some("mixer.example.org".to_string()),
            ..Config::default()
        };
        let links = config.site_links();
        assert_eq!(links.lan_url.as_deref(), Some("http://10.0.0.10"));
        assert_eq!(links.public_host.as_deref(), Some("mixer.example.org"));
        assert_eq!(Config::default().site_links(), crate::tunnel::SiteLinks::default());
    }

    #[test]
    fn test_share_url_prefers_the_public_host_then_the_lan_url() {
        let both = Config {
            lan_url: Some("http://10.0.0.10".to_string()),
            https_domain: Some("mixer.example.org".to_string()),
            ..Config::default()
        };
        assert_eq!(both.share_url().as_deref(), Some("https://mixer.example.org"));
        let lan_only = Config { lan_url: Some("http://10.0.0.10".to_string()), ..Config::default() };
        assert_eq!(lan_only.share_url().as_deref(), Some("http://10.0.0.10"));
        assert_eq!(Config::default().share_url(), None);
    }
```

- [ ] **Step 2: Implement the core side**

`crates/iem-core/src/tunnel.rs`: replace everything from the `lan_url!` macro through the end of `pub fn needs_lan_hint` with:
```rust
/// Where the mixer is reachable, from the site config (`lan_url`,
/// `https_domain`); served at `GET /api/site`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SiteLinks {
    #[serde(default)]
    pub lan_url: Option<String>,
    #[serde(default)]
    pub public_host: Option<String>,
}

const MEMBER_BANNER_PREFIX: &str = "Internetový prístup nefunguje";

/// Banner text for band members while internet access is broken.
pub fn member_banner_text(lan_url: Option<&str>) -> String {
    match lan_url {
        Some(url) => format!("{MEMBER_BANNER_PREFIX} — na tejto sieti otvorte {url}"),
        None => MEMBER_BANNER_PREFIX.to_string(),
    }
}

/// Hint under the "Reconnecting" banner for pages opened via the public host.
pub fn reconnect_lan_hint(lan_url: &str) -> String {
    format!("Ak nejde internet a ste na miestnej sieti, otvorte {lan_url}")
}

/// Whether a page loaded from `hostname` depends on the tunnel.
pub fn needs_lan_hint(hostname: &str, public_host: Option<&str>) -> bool {
    public_host.is_some_and(|host| hostname.eq_ignore_ascii_case(host))
}
```
`crates/iem-core/src/config.rs`, in `impl Config`:
```rust
    /// LAN URL and public host for the UI (`GET /api/site`).
    pub fn site_links(&self) -> crate::tunnel::SiteLinks {
        crate::tunnel::SiteLinks {
            lan_url: self.lan_url.clone(),
            public_host: self.https_domain.clone(),
        }
    }

    /// URL the tray's "Copy URL" shares: the public host over HTTPS, else the
    /// LAN URL, else none.
    pub fn share_url(&self) -> Option<String> {
        self.https_domain
            .as_ref()
            .map(|domain| format!("https://{domain}"))
            .or_else(|| self.lan_url.clone())
    }
```

- [ ] **Step 3: Write the failing server tests**

`crates/iem-server/src/routes.rs`, new module at the end of the file:
```rust
#[cfg(test)]
mod site_links_tests {
    use super::*;
    use axum::http::Request;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn site_links_come_from_the_site_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = iem_core::Config {
            lan_url: Some("http://10.0.0.10".to_string()),
            https_domain: Some("mixer.example.org".to_string()),
            ..iem_core::Config::default()
        };
        let state = AppState::new(config, dir.path());
        let app = Router::new().route("/api/site", get(get_site_links)).with_state(state);
        let resp = app
            .oneshot(Request::builder().uri("/api/site").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"lan_url": "http://10.0.0.10", "public_host": "mixer.example.org"})
        );
    }
}
```
`crates/iem-server/src/push.rs` `mod tests`: change the existing call to `build_vapid_header(&key_b64, "https://fcm.googleapis.com/fcm/send/test", "mailto:admin@example.org")` and add:
```rust
    #[test]
    fn test_vapid_jwt_carries_the_configured_subject() {
        let sk = p256::SecretKey::random(&mut rand_core::OsRng);
        let key_b64 = B64.encode(sk.to_bytes());
        let (jwt, _) =
            build_vapid_header(&key_b64, "https://push.example.org/send/1", "mailto:ops@example.org")
                .unwrap();
        let payload = jwt.split('.').nth(1).unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&B64.decode(payload).unwrap()).unwrap();
        assert_eq!(claims["sub"], "mailto:ops@example.org");
        assert_eq!(claims["aud"], "https://push.example.org");
    }
```

- [ ] **Step 4: Implement the server side**

`crates/iem-server/src/routes.rs`: in `api_routes` add `.route("/api/site", get(get_site_links))` after the `/api/version` route, and add
```rust
/// `GET /api/site` — where the mixer is reachable (LAN URL, public host), for
/// the UI's tunnel banner and reconnect hint. Public: it holds no secret.
async fn get_site_links(State(state): State<AppState>) -> Json<iem_core::tunnel::SiteLinks> {
    Json(state.config.read().await.site_links())
}
```
`crates/iem-server/src/push.rs`: `build_vapid_header(vapid_private_key_b64: &str, endpoint: &str, subject: &str)` with `"sub": subject,` in the claims; `send_push(client, vapid_private_key_b64: &str, subject: &str, sub, payload)` passing `subject` on; `send_push_to_engineers(client, vapid_key: &str, subject: &str, push_store, payload)` calling `send_push(client, vapid_key, subject, sub, payload)`.
`crates/iem-server/src/proxy.rs`, in the SOS push block: replace
```rust
                                    let vapid_key =
                                        state.config.read().await.vapid_private_key.clone();
```
with
```rust
                                    let (vapid_key, vapid_subject) = {
                                        let config = state.config.read().await;
                                        (config.vapid_private_key.clone(), config.vapid_subject.clone())
                                    };
```
and the call `crate::push::send_push_to_engineers(&http_client, &vapid_key, &push_store, …)` with `crate::push::send_push_to_engineers(&http_client, &vapid_key, &vapid_subject, &push_store, …)`.
`crates/iem-server/src/lib.rs`: `tracing::info!("HTTPS server on https://mixer.example.org");` → `tracing::info!(port = https_port, "HTTPS server listening");`.

- [ ] **Step 5: UI — site links context, banner and hint**

`crates/iem-ui/src/api.rs`, add:
```rust
/// Where the mixer is reachable (LAN URL, public host) — `GET /api/site`.
pub async fn get_site_links() -> Result<iem_core::tunnel::SiteLinks, String> {
    let resp = Request::get(&format!("{}/site", API_BASE))
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;
    if resp.ok() {
        resp.json().await.map_err(|e| format!("Parse error: {}", e))
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
}
```
`crates/iem-ui/src/router.rs` (full content):
```rust
//! Application router

use iem_core::tunnel::SiteLinks;
use leptos::prelude::*;
use leptos_router::components::{Route, Router, Routes};
use leptos_router::path;
use wasm_bindgen_futures::spawn_local;

use crate::pages::{
    landing::LandingPage, login::LoginPage, mixer::MixerPage, not_found::NotFoundPage,
};

/// Main application component with routing
#[component]
pub fn App() -> impl IntoView {
    // Where the mixer is reachable (site config, GET /api/site); read by the
    // tunnel banner and the reconnect hint. Fetched once per page load, so it
    // is known before the connection can drop.
    let site_links = RwSignal::new(SiteLinks::default());
    provide_context(site_links);
    spawn_local(async move {
        match crate::api::get_site_links().await {
            Ok(links) => {
                let _ = site_links.try_set(links);
            }
            Err(e) => leptos::logging::log!("site links unavailable: {e}"),
        }
    });

    view! {
        <Router>
            <Routes fallback=|| view! { <NotFoundPage /> }>
                <Route path=path!("/") view=LandingPage />
                <Route path=path!("/login") view=LoginPage />
                <Route path=path!("/:member") view=MixerPage />
            </Routes>
        </Router>
    }
}
```
`crates/iem-ui/src/components/tunnel_status.rs`: replace the module doc and imports with
```rust
//! Internet access (Cloudflare tunnel) status UI.
//!
//! The server pushes `ServerMsg::TunnelStatus` on connect and on every change.
//! Texts come from `iem_core::tunnel` (unit tested there); the LAN URL and the
//! public host come from the site config (`GET /api/site`, provided as
//! context by `router::App`).

use iem_core::tunnel::{SiteLinks, member_banner_text, needs_lan_hint, reconnect_lan_hint};
use iem_core::{TunnelState, TunnelStatusInfo};
use leptos::prelude::*;

fn site_links() -> RwSignal<SiteLinks> {
    use_context::<RwSignal<SiteLinks>>().unwrap_or_else(|| RwSignal::new(SiteLinks::default()))
}
```
keep `TunnelIndicator` unchanged, and replace `TunnelBanner` and `LanHint` with
```rust
/// Band-member banner shown while the tunnel is Down/Restarting: tells people
/// on the venue network to open the local address instead.
#[component]
pub fn TunnelBanner(status: ReadSignal<Option<TunnelStatusInfo>>) -> impl IntoView {
    let links = site_links();
    let broken = move || status.get().is_some_and(|s| s.state.is_broken());
    let text = move || links.with(|l| member_banner_text(l.lan_url.as_deref()));
    view! {
        <Show when=broken fallback=|| ()>
            <div class="tunnel-banner" data-testid="tunnel-banner" role="alert">
                {text}
            </div>
        </Show>
    }
}

/// Extra line in the "Reconnecting" banner for pages opened via the public
/// host: while the tunnel is down their WebSocket cannot reconnect at all.
#[component]
pub fn LanHint() -> impl IntoView {
    let links = site_links();
    let hostname = web_sys::window()
        .and_then(|w| w.location().hostname().ok())
        .unwrap_or_default();
    move || {
        links
            .with(|l| match (&l.lan_url, needs_lan_hint(&hostname, l.public_host.as_deref())) {
                (Some(lan_url), true) => Some(reconnect_lan_hint(lan_url)),
                _ => None,
            })
            .map(|hint| view! { <div class="lan-hint" data-testid="lan-hint">{hint}</div> })
    }
}
```

- [ ] **Step 6: Tray — share URL from the site config**

`crates/iem-tray/src/lib.rs`: after `let port = config.port;` add
```rust
    let share_url = config.share_url();
```
and in `.setup(move |app| { … })` call `tray::setup_tray(&handle, port, share_url)`.
`crates/iem-tray/src/tray.rs`: delete `const REMOTE_URL`; change the signature to `pub fn setup_tray(app: &AppHandle, port: u16, share_url: Option<String>) -> Result<(), Box<dyn std::error::Error>>`; build the copy item with
```rust
    let (copy_label, copy_enabled) = match &share_url {
        Some(url) => (format!("📋 {url}"), true),
        None => ("No public URL configured".to_string(), false),
    };
    let copy_url_item = MenuItem::with_id(app, "copy_url", copy_label, copy_enabled, None::<&str>)?;
```
in the menu handler use `"copy_url" => { if let Some(url) = &share_url { copy_url_to_clipboard(app, url); } }`; and replace `copy_url_to_clipboard` with
```rust
/// Copy the share URL to the clipboard (quoted as a JSON string literal).
fn copy_url_to_clipboard(app: &AppHandle, url: &str) {
    tracing::info!(url, "copying the share URL to the clipboard");
    if let Some(window) = app.get_webview_window("main") {
        let quoted = serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string());
        let js = format!("navigator.clipboard.writeText({quoted})");
        let _ = window.eval(&js);
    }
}
```

- [ ] **Step 7: Format and commit**

```bash
cd "$WORK" && cargo fmt --all
git add crates
git commit -m "feat: LAN URL, public host and VAPID contact from the site config" -m "GET /api/site feeds the UI's tunnel banner and reconnect hint; the tray copies the configured public URL. Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected in CI (Task 15): the new core (incl. `test_share_url_prefers_the_public_host_then_the_lan_url`), route and push tests pass; `wasm` builds the UI.

---

### Task 8: PIN hashing and the pepper (standalone modules)

**Files:**
- Create: `crates/iem-server/src/pin_hash.rs`, `crates/iem-server/src/pepper.rs`, `crates/iem-server/src/pepper/dpapi.rs`
- Modify: `crates/iem-server/Cargo.toml`, `crates/iem-server/src/lib.rs` (module list)

**Interfaces:**
- Produces: `pin_hash::{PEPPER_LEN: usize = 32, PIN_LEN: usize = 4, PIN_M_COST_KIB = 19456, PIN_T_COST = 2, PIN_P_COST = 1, is_valid_pin_format(&str) -> bool, PinHasher}` with `PinHasher::new([u8; 32])`, `#[cfg(test)] PinHasher::for_tests([u8; 32])` (minimal cost), `hash(&self, pin: &str) -> String` (PHC), `verify(&self, pin: &str, phc: &str) -> bool`, `verify_optional(&self, pin: &str, phc: Option<&str>) -> bool` (dummy work when `None`); `PinHasher: Clone + Debug` (Debug never prints the pepper).
- Produces: `pepper::{PEPPER_FILE, load_or_create(dir: &Path) -> io::Result<[u8; 32]>}` (`pepper.dpapi` on Windows, `pepper.test` elsewhere).

- [ ] **Step 1: Dependencies**

`crates/iem-server/Cargo.toml` `[dependencies]`: add `argon2 = "0.5"`; add
```toml
[target.'cfg(windows)'.dependencies]
windows = { version = "0.61", features = ["Win32_Foundation", "Win32_Security_Cryptography"] }
```
`crates/iem-server/src/lib.rs`: add `pub mod pepper;` (after `pub mod customization_store;`) and `pub mod pin_hash;` (after `pub mod photo_store;`).

- [ ] **Step 2: Write `pin_hash.rs` with its tests (tests first in the file's `mod tests`)**

```rust
//! PIN hashing (program spec §5.3): argon2id with OWASP parameters
//! (m = 19 MiB, t = 2, p = 1), keyed with a secret pepper so a copied hash
//! file cannot be brute-forced offline. Stored as PHC strings.

use std::sync::{Arc, OnceLock};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::OsRng;

/// Length of the pepper (argon2 secret) in bytes.
pub const PEPPER_LEN: usize = 32;
/// PIN length (4 digits, unchanged from the predecessor — spec P9).
pub const PIN_LEN: usize = 4;
/// argon2id memory cost in KiB (19 MiB).
pub const PIN_M_COST_KIB: u32 = 19 * 1024;
/// argon2id iterations.
pub const PIN_T_COST: u32 = 2;
/// argon2id parallelism.
pub const PIN_P_COST: u32 = 1;

const DUMMY_PIN: &str = "no-pin-set";

/// Whether `pin` is exactly [`PIN_LEN`] ASCII digits.
pub fn is_valid_pin_format(pin: &str) -> bool {
    pin.len() == PIN_LEN && pin.bytes().all(|b| b.is_ascii_digit())
}

/// Hashes and verifies PINs with a fixed pepper.
#[derive(Clone)]
pub struct PinHasher {
    pepper: Arc<[u8; PEPPER_LEN]>,
    params: Params,
    dummy: Arc<OnceLock<String>>,
}

impl std::fmt::Debug for PinHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinHasher").finish_non_exhaustive()
    }
}

impl PinHasher {
    /// Production hasher (OWASP parameters).
    pub fn new(pepper: [u8; PEPPER_LEN]) -> Self {
        let params = Params::new(PIN_M_COST_KIB, PIN_T_COST, PIN_P_COST, None)
            .expect("OWASP argon2id parameters are valid");
        Self::with_params(pepper, params)
    }

    /// Minimal-cost hasher for fast tests.
    #[cfg(test)]
    pub fn for_tests(pepper: [u8; PEPPER_LEN]) -> Self {
        let params = Params::new(Params::MIN_M_COST, 1, 1, None).expect("minimal argon2 parameters are valid");
        Self::with_params(pepper, params)
    }

    fn with_params(pepper: [u8; PEPPER_LEN], params: Params) -> Self {
        Self { pepper: Arc::new(pepper), params, dummy: Arc::new(OnceLock::new()) }
    }

    fn argon2(&self) -> Argon2<'_> {
        Argon2::new_with_secret(&self.pepper[..], Algorithm::Argon2id, Version::V0x13, self.params.clone())
            .expect("a 32-byte pepper is within the argon2 secret limit")
    }

    /// argon2id PHC string of `pin` with a fresh random salt.
    pub fn hash(&self, pin: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        self.argon2()
            .hash_password(pin.as_bytes(), &salt)
            .expect("argon2id hashing with valid parameters cannot fail")
            .to_string()
    }

    /// Whether `pin` matches the PHC string `phc` (false for malformed input).
    pub fn verify(&self, pin: &str, phc: &str) -> bool {
        match PasswordHash::new(phc) {
            Ok(parsed) => self.argon2().verify_password(pin.as_bytes(), &parsed).is_ok(),
            Err(_) => false,
        }
    }

    /// Like [`Self::verify`]; with no stored hash it still does one
    /// verification's work (against a dummy hash) and returns false, so a
    /// missing PIN is not revealed by timing.
    pub fn verify_optional(&self, pin: &str, phc: Option<&str>) -> bool {
        match phc {
            Some(phc) => self.verify(pin, phc),
            None => {
                let dummy = self.dummy.get_or_init(|| self.hash(DUMMY_PIN));
                let _ = self.verify(pin, dummy);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEPPER_A: [u8; PEPPER_LEN] = [7u8; PEPPER_LEN];
    const PEPPER_B: [u8; PEPPER_LEN] = [9u8; PEPPER_LEN];

    #[test]
    fn hash_verifies_the_same_pin_and_rejects_another() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        let phc = hasher.hash("2468");
        assert!(hasher.verify("2468", &phc));
        assert!(!hasher.verify("2469", &phc));
    }

    #[test]
    fn hashes_are_salted() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        assert_ne!(hasher.hash("2468"), hasher.hash("2468"));
    }

    #[test]
    fn the_pepper_is_part_of_the_hash() {
        let phc = PinHasher::for_tests(PEPPER_A).hash("2468");
        assert!(!PinHasher::for_tests(PEPPER_B).verify("2468", &phc));
    }

    #[test]
    fn production_parameters_are_owasp_argon2id() {
        let hasher = PinHasher::new(PEPPER_A);
        let phc = hasher.hash("2468");
        assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
        assert!(hasher.verify("2468", &phc));
    }

    #[test]
    fn malformed_hashes_never_verify() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        assert!(!hasher.verify("2468", "not-a-phc"));
        assert!(!hasher.verify("2468", ""));
    }

    #[test]
    fn missing_hash_never_verifies() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        assert!(!hasher.verify_optional("2468", None));
        assert!(!hasher.verify_optional("no-pin-set", None));
    }

    #[test]
    fn verify_optional_checks_a_present_hash() {
        let hasher = PinHasher::for_tests(PEPPER_A);
        let phc = hasher.hash("2468");
        assert!(hasher.verify_optional("2468", Some(&phc)));
        assert!(!hasher.verify_optional("1357", Some(&phc)));
    }

    #[test]
    fn pin_format_is_exactly_four_ascii_digits() {
        for ok in ["0000", "2468", "9999"] {
            assert!(is_valid_pin_format(ok), "{ok}");
        }
        for bad in ["", "123", "12345", "12a4", " 123", "１２３４"] {
            assert!(!is_valid_pin_format(bad), "{bad:?}");
        }
    }

    #[test]
    fn debug_output_never_shows_the_pepper() {
        let shown = format!("{:?}", PinHasher::for_tests(PEPPER_A));
        assert_eq!(shown, "PinHasher { .. }");
    }
}
```

- [ ] **Step 3: Write `pepper.rs` and `pepper/dpapi.rs`**

`crates/iem-server/src/pepper.rs`:
```rust
//! The PIN pepper: 32 random bytes created once per data directory.
//! Windows: DPAPI-protected for the current user (`pepper.dpapi`).
//! Other platforms are test-only: an owner-only plain file (`pepper.test`).
//! A pepper file that exists but cannot be read is an error — never replaced,
//! because a new pepper would silently invalidate every PIN hash.

use std::io;
use std::path::Path;

use rand_core::{OsRng, RngCore};

use crate::pin_hash::PEPPER_LEN;

#[cfg(windows)]
mod dpapi;

/// File name of the stored pepper.
#[cfg(windows)]
pub const PEPPER_FILE: &str = "pepper.dpapi";
/// File name of the stored pepper.
#[cfg(not(windows))]
pub const PEPPER_FILE: &str = "pepper.test";

/// Load the pepper from `dir`, creating it on first use.
pub fn load_or_create(dir: &Path) -> io::Result<[u8; PEPPER_LEN]> {
    let path = dir.join(PEPPER_FILE);
    match std::fs::read(&path) {
        Ok(stored) => {
            let raw = unprotect(&stored)?;
            <[u8; PEPPER_LEN]>::try_from(raw.as_slice()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} does not hold a {PEPPER_LEN}-byte pepper", path.display()),
                )
            })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut pepper = [0u8; PEPPER_LEN];
            OsRng.fill_bytes(&mut pepper);
            std::fs::create_dir_all(dir)?;
            crate::secrets::write_new_private(&path, &protect(&pepper)?)?;
            tracing::info!(path = %path.display(), "created a new PIN pepper");
            Ok(pepper)
        }
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    dpapi::protect(data)
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    dpapi::unprotect(data)
}

#[cfg(not(windows))]
fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    tracing::warn!("PIN pepper stored unprotected: only Windows protects it (DPAPI); other platforms are test-only");
    Ok(data.to_vec())
}

#[cfg(not(windows))]
fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    Ok(data.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_once_and_reloads_the_same_pepper() {
        let dir = tempfile::tempdir().unwrap();
        let first = load_or_create(dir.path()).unwrap();
        let second = load_or_create(dir.path()).unwrap();
        assert_eq!(first, second);
        assert_ne!(first, [0u8; PEPPER_LEN]);
    }

    #[test]
    fn different_directories_get_different_peppers() {
        let a = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        let b = load_or_create(tempfile::tempdir().unwrap().path()).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn wrong_length_file_is_an_error_not_a_new_pepper() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PEPPER_FILE);
        std::fs::write(&path, b"short").unwrap();
        assert!(load_or_create(dir.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"short");
    }

    #[cfg(not(windows))]
    #[test]
    fn test_platforms_store_the_raw_pepper() {
        let dir = tempfile::tempdir().unwrap();
        let pepper = load_or_create(dir.path()).unwrap();
        assert_eq!(std::fs::read(dir.path().join(PEPPER_FILE)).unwrap(), pepper.to_vec());
    }

    #[cfg(windows)]
    #[test]
    fn windows_stores_the_pepper_dpapi_protected() {
        let dir = tempfile::tempdir().unwrap();
        let pepper = load_or_create(dir.path()).unwrap();
        let stored = std::fs::read(dir.path().join(PEPPER_FILE)).unwrap();
        assert_ne!(stored, pepper.to_vec());
        assert!(stored.len() > PEPPER_LEN);
    }

    #[cfg(unix)]
    #[test]
    fn pepper_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        load_or_create(dir.path()).unwrap();
        let mode = std::fs::metadata(dir.path().join(PEPPER_FILE)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
```

`crates/iem-server/src/pepper/dpapi.rs`:
```rust
//! DPAPI (current user) protection of the PIN pepper — Windows only.
//! Excluded from mutation testing (not compiled on Linux CI); the `windows`
//! CI job runs `pepper::` tests against it.

use std::io;

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

pub(super) fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob_for(data)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: `input` points at `data` for the duration of the call; on success
    // DPAPI fills `output` with a LocalAlloc'd buffer that `take_output` frees.
    unsafe {
        CryptProtectData(&input, PCWSTR::null(), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
    }
    .map_err(io::Error::other)?;
    Ok(take_output(output))
}

pub(super) fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob_for(data)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe { CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output) }
        .map_err(io::Error::other)?;
    Ok(take_output(output))
}

fn blob_for(data: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
    let len = u32::try_from(data.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "DPAPI input too large"))?;
    Ok(CRYPT_INTEGER_BLOB { cbData: len, pbData: data.as_ptr().cast_mut() })
}

fn take_output(output: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: DPAPI set pbData/cbData to a valid LocalAlloc'd buffer.
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    // SAFETY: the buffer came from LocalAlloc and is freed exactly once.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
    }
    bytes
}
```

- [ ] **Step 4: Lockfile, format, commit**

```bash
set -euo pipefail
cd "$WORK"
cargo metadata --format-version 1 > /dev/null
cargo metadata --locked --format-version 1 > /dev/null
grep -A1 '^name = "argon2"$' Cargo.lock
cargo fmt --all
git add crates Cargo.lock
git commit -m "feat(auth): argon2id PIN hashing with a DPAPI-protected pepper" -m "Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected in CI (Task 15): `pin_hash::` and `pepper::` tests pass on Linux; the `windows` job runs `pepper::` against DPAPI.

---

### Task 9: Login limiter and hashing gate (standalone module)

**Files:**
- Create: `crates/iem-server/src/login_guard.rs`
- Modify: `crates/iem-server/src/lib.rs` (add `pub mod login_guard;` after `pub mod customization_store;`)

**Interfaces:**
- Produces: `login_guard::{Origin { Lan, Tunnel }, ClientKey { origin: Origin, ip: IpAddr }, ClientKey::from_request(peer: IpAddr, headers: &HeaderMap) -> ClientKey` (IPv4 keys as is, IPv6 keys reduced to their /64), LoginGuard::new(), LoginGuard::check(&self, &ClientKey, member: &str, now: Instant) -> Result<(), Duration>, LoginGuard::record_failure(&self, &ClientKey, &str, Instant) -> FailureEffect, LoginGuard::record_success(&self, &ClientKey, &str), LoginGuard::stats(&self) -> LoginStats { lan_failures: u64, tunnel_failures: u64, engineer_budget_trips: u64 }, FailureEffect { Counted, EngineerBudgetExhausted }, streak_delay(u32) -> Duration, HashGate::new(concurrency, max_waiting), HashGate::acquire(&self) -> Option<OwnedSemaphorePermit> (async), HashGate::waiting(&self) -> usize}`; constants `FREE_FAILURES=3, STREAK_DECAY=15 min, MAX_DELAY=60 s, CLIENT_WINDOW=10 min, CLIENT_WINDOW_FAILURES=20, CLIENT_SPACING=60 s, ENGINEER_WINDOW=60 min, ENGINEER_WINDOW_FAILURES=30, ENGINEER_SPACING=5 s, MAX_TRACKED=4096, HASH_CONCURRENCY=2, HASH_QUEUE=8`.

- [ ] **Step 1: Write the module with its tests**

```rust
//! Login protection (program spec §5.3). Pure admission logic with the clock
//! injected, plus a bounded gate for argon2id work:
//!
//! - budgets count failures only, separately for LAN and tunnel clients;
//! - `CF-Connecting-IP` is trusted only from a loopback peer (the tunnel
//!   connector on the same PC); an IPv6 client is keyed by its /64, so it
//!   cannot rotate addresses within its prefix to reset its budgets;
//! - per (client, member): three free failures, then 1, 2, 4 … s;
//! - per client: 20 failures in 10 minutes → 60 s spacing;
//! - engineer budget per origin: the engineer PIN works from any member login,
//!   so every failure counts; over 30 in an hour → 5 s spacing for the origin;
//! - every delay ≤ 60 s — never a lockout; the caller answers 429 +
//!   `Retry-After` before any hashing.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Consecutive failures per (client, member) that carry no delay.
pub const FREE_FAILURES: u32 = 3;
/// A (client, member) streak is forgotten after this long without a failure.
pub const STREAK_DECAY: Duration = Duration::from_secs(15 * 60);
/// Upper bound of every delay (never a lockout).
pub const MAX_DELAY: Duration = Duration::from_secs(60);
/// Sliding window of the per-client budget (across members).
pub const CLIENT_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Failures per client within [`CLIENT_WINDOW`] before spacing applies.
pub const CLIENT_WINDOW_FAILURES: usize = 20;
/// Spacing once a client has used its window budget.
pub const CLIENT_SPACING: Duration = Duration::from_secs(60);
/// Sliding window of the engineer budget (per origin).
pub const ENGINEER_WINDOW: Duration = Duration::from_secs(60 * 60);
/// Failures per origin within [`ENGINEER_WINDOW`] before origin spacing applies.
pub const ENGINEER_WINDOW_FAILURES: usize = 30;
/// Spacing between admitted attempts of an origin over its engineer budget.
pub const ENGINEER_SPACING: Duration = Duration::from_secs(5);
/// Upper bound of tracked keys per map (memory bound under a flood).
pub const MAX_TRACKED: usize = 4096;
/// argon2id hashes running at once (each costs 19 MiB).
pub const HASH_CONCURRENCY: usize = 2;
/// Requests allowed to wait for a hashing slot; more get 429 at once.
pub const HASH_QUEUE: usize = 8;

/// Where a login comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    Lan,
    Tunnel,
}

/// Budget key of a client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientKey {
    pub origin: Origin,
    pub ip: IpAddr,
}

/// Budget key of an address: IPv4 as is, IPv6 reduced to its /64 (a single
/// subscriber usually holds a whole /64). LAN peers are IPv4 in practice: the
/// listeners bind `0.0.0.0`.
fn budget_ip(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

impl ClientKey {
    /// `CF-Connecting-IP` counts only when the TCP peer is loopback; every
    /// other peer is a LAN client keyed by its own address.
    pub fn from_request(peer: IpAddr, headers: &HeaderMap) -> Self {
        let peer = peer.to_canonical();
        if peer.is_loopback()
            && let Some(ip) = headers
                .get("cf-connecting-ip")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<IpAddr>().ok())
        {
            return Self { origin: Origin::Tunnel, ip: budget_ip(ip) };
        }
        Self { origin: Origin::Lan, ip: budget_ip(peer) }
    }
}

/// What a recorded failure changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureEffect {
    Counted,
    /// This failure pushed its origin over the engineer budget.
    EngineerBudgetExhausted,
}

/// Counters for the engineer page (wired in S5).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct LoginStats {
    pub lan_failures: u64,
    pub tunnel_failures: u64,
    pub engineer_budget_trips: u64,
}

#[derive(Debug, Clone, Copy)]
struct Streak {
    failures: u32,
    last_failure: Instant,
}

#[derive(Debug, Default)]
struct OriginBudget {
    failures: VecDeque<Instant>,
    last_admitted: Option<Instant>,
}

#[derive(Debug, Default)]
struct Inner {
    streaks: HashMap<(ClientKey, String), Streak>,
    clients: HashMap<ClientKey, VecDeque<Instant>>,
    lan: OriginBudget,
    tunnel: OriginBudget,
    stats: LoginStats,
}

impl Inner {
    fn origin_mut(&mut self, origin: Origin) -> &mut OriginBudget {
        match origin {
            Origin::Lan => &mut self.lan,
            Origin::Tunnel => &mut self.tunnel,
        }
    }
}

/// Delay owed after `failures` consecutive failures: none up to
/// [`FREE_FAILURES`] − 1, then 1, 2, 4 … s, capped at [`MAX_DELAY`].
pub fn streak_delay(failures: u32) -> Duration {
    if failures < FREE_FAILURES {
        return Duration::ZERO;
    }
    let exponent = failures - FREE_FAILURES;
    if exponent >= 6 {
        return MAX_DELAY;
    }
    Duration::from_secs(1u64 << exponent).min(MAX_DELAY)
}

fn prune(window: &mut VecDeque<Instant>, now: Instant, span: Duration) {
    while let Some(&oldest) = window.front() {
        if now.saturating_duration_since(oldest) >= span {
            window.pop_front();
        } else {
            break;
        }
    }
}

fn evict_oldest_streak(map: &mut HashMap<(ClientKey, String), Streak>) {
    if let Some(key) = map.iter().min_by_key(|(_, s)| s.last_failure).map(|(k, _)| k.clone()) {
        map.remove(&key);
    }
}

fn evict_oldest_client(map: &mut HashMap<ClientKey, VecDeque<Instant>>) {
    if let Some(key) = map.iter().min_by_key(|(_, w)| w.back().copied()).map(|(k, _)| *k) {
        map.remove(&key);
    }
}

/// Failure bookkeeping shared by every login and PIN-change request.
#[derive(Debug, Default)]
pub struct LoginGuard {
    inner: Mutex<Inner>,
}

impl LoginGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admission before any hashing. `Err(wait)` → answer 429 with `Retry-After`.
    pub fn check(&self, client: &ClientKey, member: &str, now: Instant) -> Result<(), Duration> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (*client, member.to_string());
        if let Some(streak) = inner.streaks.get(&key).copied() {
            let since = now.saturating_duration_since(streak.last_failure);
            if since >= STREAK_DECAY {
                inner.streaks.remove(&key);
            } else {
                let delay = streak_delay(streak.failures);
                if since < delay {
                    return Err(delay - since);
                }
            }
        }
        if let Some(window) = inner.clients.get_mut(client) {
            prune(window, now, CLIENT_WINDOW);
            if window.len() >= CLIENT_WINDOW_FAILURES
                && let Some(&last) = window.back()
            {
                let since = now.saturating_duration_since(last);
                if since < CLIENT_SPACING {
                    return Err(CLIENT_SPACING - since);
                }
            }
        }
        let budget = inner.origin_mut(client.origin);
        prune(&mut budget.failures, now, ENGINEER_WINDOW);
        if budget.failures.len() > ENGINEER_WINDOW_FAILURES
            && let Some(last) = budget.last_admitted
        {
            let since = now.saturating_duration_since(last);
            if since < ENGINEER_SPACING {
                return Err(ENGINEER_SPACING - since);
            }
        }
        budget.last_admitted = Some(now);
        Ok(())
    }

    /// Count a failed PIN check.
    pub fn record_failure(&self, client: &ClientKey, member: &str, now: Instant) -> FailureEffect {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (*client, member.to_string());
        let failures = match inner.streaks.get(&key) {
            Some(streak) if now.saturating_duration_since(streak.last_failure) < STREAK_DECAY => {
                streak.failures.saturating_add(1)
            }
            _ => 1,
        };
        if !inner.streaks.contains_key(&key) && inner.streaks.len() >= MAX_TRACKED {
            evict_oldest_streak(&mut inner.streaks);
        }
        inner.streaks.insert(key, Streak { failures, last_failure: now });

        if !inner.clients.contains_key(client) && inner.clients.len() >= MAX_TRACKED {
            evict_oldest_client(&mut inner.clients);
        }
        let window = inner.clients.entry(*client).or_default();
        prune(window, now, CLIENT_WINDOW);
        window.push_back(now);
        if window.len() > CLIENT_WINDOW_FAILURES {
            window.pop_front();
        }

        match client.origin {
            Origin::Lan => inner.stats.lan_failures += 1,
            Origin::Tunnel => inner.stats.tunnel_failures += 1,
        }
        let budget = inner.origin_mut(client.origin);
        prune(&mut budget.failures, now, ENGINEER_WINDOW);
        let before = budget.failures.len();
        budget.failures.push_back(now);
        if budget.failures.len() > ENGINEER_WINDOW_FAILURES + 1 {
            budget.failures.pop_front();
        }
        if before == ENGINEER_WINDOW_FAILURES {
            inner.stats.engineer_budget_trips += 1;
            FailureEffect::EngineerBudgetExhausted
        } else {
            FailureEffect::Counted
        }
    }

    /// A successful PIN check ends that (client, member) streak.
    pub fn record_success(&self, client: &ClientKey, member: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.streaks.remove(&(*client, member.to_string()));
    }

    pub fn stats(&self) -> LoginStats {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).stats
    }
}

/// Bounded concurrency for argon2id work: `concurrency` running, at most
/// `max_waiting` queued; beyond that `acquire` returns `None` at once.
#[derive(Debug)]
pub struct HashGate {
    permits: Arc<Semaphore>,
    waiting: AtomicUsize,
    max_waiting: usize,
}

struct WaitingSlot<'a>(&'a AtomicUsize);

impl Drop for WaitingSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl HashGate {
    pub fn new(concurrency: usize, max_waiting: usize) -> Self {
        Self { permits: Arc::new(Semaphore::new(concurrency)), waiting: AtomicUsize::new(0), max_waiting }
    }

    /// A hashing slot, or `None` when the queue is full. A cancelled waiter
    /// (client gone) frees its queue place.
    pub async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            return Some(permit);
        }
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= self.max_waiting {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let _slot = WaitingSlot(&self.waiting);
        Arc::clone(&self.permits).acquire_owned().await.ok()
    }

    /// Requests currently queued for a slot.
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn lan(last: u8) -> ClientKey {
        ClientKey { origin: Origin::Lan, ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, last)) }
    }

    fn tunnel(last: u8) -> ClientKey {
        ClientKey { origin: Origin::Tunnel, ip: IpAddr::V4(Ipv4Addr::new(203, 0, 113, last)) }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn headers(cf: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(value) = cf {
            h.insert("cf-connecting-ip", value.parse().unwrap());
        }
        h
    }

    #[test]
    fn streak_delay_doubles_from_the_third_failure_and_caps_at_sixty_seconds() {
        let table = [(0, 0), (1, 0), (2, 0), (3, 1), (4, 2), (5, 4), (6, 8), (7, 16), (8, 32), (9, 60), (10, 60), (u32::MAX, 60)];
        for (failures, expected) in table {
            assert_eq!(streak_delay(failures), secs(expected), "failures={failures}");
        }
    }

    #[test]
    fn three_failures_then_backoff() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        let c = lan(1);
        for _ in 0..3 {
            assert_eq!(guard.check(&c, "member1", t0), Ok(()));
            guard.record_failure(&c, "member1", t0);
        }
        assert_eq!(guard.check(&c, "member1", t0), Err(secs(1)));
        assert_eq!(guard.check(&c, "member1", t0 + Duration::from_millis(400)), Err(Duration::from_millis(600)));
        assert_eq!(guard.check(&c, "member1", t0 + secs(1)), Ok(()));
        guard.record_failure(&c, "member1", t0 + secs(1));
        assert_eq!(guard.check(&c, "member1", t0 + secs(1)), Err(secs(2)));
    }

    #[test]
    fn the_streak_is_per_member() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        for _ in 0..3 {
            guard.record_failure(&lan(1), "member1", t0);
        }
        assert_eq!(guard.check(&lan(1), "member2", t0), Ok(()));
    }

    #[test]
    fn success_clears_the_streak() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        for _ in 0..3 {
            guard.record_failure(&lan(1), "member1", t0);
        }
        guard.record_success(&lan(1), "member1");
        assert_eq!(guard.check(&lan(1), "member1", t0), Ok(()));
    }

    #[test]
    fn the_streak_decays_after_fifteen_quiet_minutes() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        for _ in 0..5 {
            guard.record_failure(&lan(1), "member1", t0);
        }
        assert_eq!(guard.check(&lan(1), "member1", t0 + STREAK_DECAY), Ok(()));
        guard.record_failure(&lan(1), "member1", t0 + STREAK_DECAY);
        assert_eq!(guard.check(&lan(1), "member1", t0 + STREAK_DECAY), Ok(()));
    }

    #[test]
    fn lan_and_tunnel_budgets_are_separate_for_the_same_address() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        let ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7));
        let via_tunnel = ClientKey { origin: Origin::Tunnel, ip };
        let on_lan = ClientKey { origin: Origin::Lan, ip };
        for _ in 0..3 {
            guard.record_failure(&via_tunnel, "member1", t0);
        }
        assert!(guard.check(&via_tunnel, "member1", t0).is_err());
        assert_eq!(guard.check(&on_lan, "member1", t0), Ok(()));
    }

    #[test]
    fn a_client_is_spaced_sixty_seconds_after_twenty_failures_in_ten_minutes() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        let c = lan(2);
        for i in 0..20u64 {
            let member = format!("member{i}");
            assert_eq!(guard.check(&c, &member, t0 + secs(i)), Ok(()));
            guard.record_failure(&c, &member, t0 + secs(i));
        }
        let last = t0 + secs(19);
        assert_eq!(guard.check(&c, "fresh", last + secs(10)), Err(secs(50)));
        assert_eq!(guard.check(&c, "fresh", last + secs(60)), Ok(()));
    }

    #[test]
    fn old_failures_leave_the_client_window() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        for i in 0..20u64 {
            guard.record_failure(&lan(3), &format!("member{i}"), t0);
        }
        assert!(guard.check(&lan(3), "fresh", t0 + secs(1)).is_err());
        assert_eq!(guard.check(&lan(3), "fresh", t0 + CLIENT_WINDOW), Ok(()));
    }

    #[test]
    fn engineer_budget_spaces_a_whole_origin_after_thirty_failures_an_hour() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        let mut effects = Vec::new();
        for i in 0..31u8 {
            let member = format!("member{i}");
            assert_eq!(guard.check(&tunnel(i), &member, t0), Ok(()));
            effects.push(guard.record_failure(&tunnel(i), &member, t0));
        }
        assert_eq!(effects.iter().filter(|e| **e == FailureEffect::EngineerBudgetExhausted).count(), 1);
        assert_eq!(effects[30], FailureEffect::EngineerBudgetExhausted);
        assert_eq!(guard.check(&tunnel(200), "member1", t0), Err(ENGINEER_SPACING));
        assert_eq!(guard.check(&tunnel(200), "member1", t0 + ENGINEER_SPACING), Ok(()));
        assert_eq!(guard.check(&tunnel(201), "member2", t0 + ENGINEER_SPACING + secs(1)), Err(secs(4)));
        assert_eq!(guard.check(&lan(9), "member1", t0), Ok(()), "LAN is never slowed by tunnel failures");
        assert_eq!(
            guard.stats(),
            LoginStats { lan_failures: 0, tunnel_failures: 31, engineer_budget_trips: 1 }
        );
    }

    #[test]
    fn lan_origin_budget_spacing_is_intended() {
        // By design: one person guessing from the venue network slows every LAN
        // login to one per 5 s once the LAN origin has more than 30 failures in
        // an hour (any member login may carry an engineer-PIN guess); tunnel
        // logins keep their own budget.
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        for i in 0..31u8 {
            guard.record_failure(&lan(i), &format!("member{i}"), t0);
        }
        assert_eq!(guard.check(&lan(200), "member1", t0), Ok(()));
        assert_eq!(guard.check(&lan(201), "member2", t0), Err(ENGINEER_SPACING));
        assert_eq!(guard.check(&tunnel(1), "member1", t0), Ok(()));
    }

    #[test]
    fn in_flight_attempts_are_bounded_by_the_gate() {
        // Admission does not reserve: attempts of one client that are still
        // hashing are all admitted, so their number is bounded only by the
        // hashing gate (HASH_CONCURRENCY running + HASH_QUEUE waiting, see
        // `hash_gate_refuses_beyond_concurrency_plus_queue`); once their
        // failures are recorded they apply to the next attempt.
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        let c = lan(4);
        let in_flight = HASH_CONCURRENCY + HASH_QUEUE;
        for _ in 0..in_flight {
            assert_eq!(guard.check(&c, "member1", t0), Ok(()));
        }
        for _ in 0..in_flight {
            guard.record_failure(&c, "member1", t0);
        }
        assert!(guard.check(&c, "member1", t0).is_err());
    }

    #[test]
    fn every_delay_is_at_most_sixty_seconds() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        let c = tunnel(77);
        for n in 0..200u64 {
            let at = t0 + Duration::from_millis(n * 10);
            if let Err(wait) = guard.check(&c, "member1", at) {
                assert!(wait <= MAX_DELAY, "wait {wait:?}");
            }
            guard.record_failure(&c, "member1", at);
        }
    }

    #[test]
    fn tracked_keys_are_bounded() {
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        for i in 0..(MAX_TRACKED as u32 + 10) {
            let c = ClientKey { origin: Origin::Lan, ip: IpAddr::V4(Ipv4Addr::from(0x0a00_0000 + i)) };
            guard.record_failure(&c, "member1", t0 + Duration::from_millis(u64::from(i)));
        }
        let inner = guard.inner.lock().unwrap();
        assert!(inner.streaks.len() <= MAX_TRACKED);
        assert!(inner.clients.len() <= MAX_TRACKED);
        assert!(inner.lan.failures.len() <= ENGINEER_WINDOW_FAILURES + 1);
    }

    #[test]
    fn cf_header_is_trusted_only_from_loopback() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            ClientKey::from_request(loopback, &headers(Some("203.0.113.5"))),
            ClientKey { origin: Origin::Tunnel, ip: "203.0.113.5".parse().unwrap() }
        );
        let lan_peer: IpAddr = "10.0.0.20".parse().unwrap();
        assert_eq!(
            ClientKey::from_request(lan_peer, &headers(Some("203.0.113.5"))),
            ClientKey { origin: Origin::Lan, ip: lan_peer }
        );
    }

    #[test]
    fn loopback_without_header_is_a_local_lan_client() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(ClientKey::from_request(loopback, &headers(None)), ClientKey { origin: Origin::Lan, ip: loopback });
    }

    #[test]
    fn ipv4_mapped_loopback_counts_as_loopback() {
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert_eq!(ClientKey::from_request(mapped, &headers(Some("203.0.113.6"))).origin, Origin::Tunnel);
    }

    #[test]
    fn garbage_header_falls_back_to_the_peer() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            ClientKey::from_request(loopback, &headers(Some("not-an-ip"))),
            ClientKey { origin: Origin::Lan, ip: loopback }
        );
    }

    #[test]
    fn ipv6_clients_share_a_budget_per_64_prefix() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let a = ClientKey::from_request(loopback, &headers(Some("2001:db8:1:2:aaaa::1")));
        let b = ClientKey::from_request(loopback, &headers(Some("2001:db8:1:2:bbbb:cccc:dddd:eeee")));
        let other = ClientKey::from_request(loopback, &headers(Some("2001:db8:1:3::1")));
        assert_eq!(a, b, "one /64 is one client");
        assert_ne!(a, other);
        assert_eq!(a.ip, "2001:db8:1:2::".parse::<IpAddr>().unwrap());
        let v4 = ClientKey::from_request(loopback, &headers(Some("203.0.113.8")));
        assert_eq!(v4.ip, "203.0.113.8".parse::<IpAddr>().unwrap(), "IPv4 keys are unchanged");
        let guard = LoginGuard::new();
        let t0 = Instant::now();
        for _ in 0..3 {
            guard.record_failure(&a, "member1", t0);
        }
        assert!(guard.check(&b, "member1", t0).is_err(), "a rotated address inherits the streak");
        assert_eq!(guard.check(&other, "member1", t0), Ok(()));
    }

    #[test]
    fn header_value_is_trimmed() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(ClientKey::from_request(loopback, &headers(Some(" 203.0.113.7 "))).ip, "203.0.113.7".parse::<IpAddr>().unwrap());
    }

    #[tokio::test]
    async fn hash_gate_refuses_beyond_concurrency_plus_queue() {
        let gate = Arc::new(HashGate::new(2, 8));
        let first = gate.acquire().await.expect("first permit");
        let second = gate.acquire().await.expect("second permit");
        let mut waiters = Vec::new();
        for _ in 0..8 {
            let g = Arc::clone(&gate);
            waiters.push(tokio::spawn(async move { g.acquire().await.is_some() }));
        }
        for _ in 0..1000 {
            if gate.waiting() == 8 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.waiting(), 8);
        assert!(gate.acquire().await.is_none(), "the 11th concurrent request is refused at once");
        drop(first);
        drop(second);
        for waiter in waiters {
            assert!(waiter.await.unwrap());
        }
        assert_eq!(gate.waiting(), 0);
    }

    #[tokio::test]
    async fn a_cancelled_waiter_frees_its_queue_place() {
        let gate = Arc::new(HashGate::new(1, 1));
        let _held = gate.acquire().await.expect("permit");
        let g = Arc::clone(&gate);
        let waiter = tokio::spawn(async move { g.acquire().await.is_some() });
        for _ in 0..1000 {
            if gate.waiting() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(gate.waiting(), 1);
        waiter.abort();
        let _ = waiter.await;
        assert_eq!(gate.waiting(), 0);
    }
}
```

- [ ] **Step 2: Format and commit**

```bash
cd "$WORK" && cargo fmt --all
git add crates
git commit -m "feat(auth): login limiter (failure budgets, engineer budget, never lockout) and hashing gate" -m "Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected in CI (Task 15): all `login_guard::` tests pass.

---

### Task 10: Wire hashed PINs and login protection into the server and UI

**Files:**
- Rewrite: `crates/iem-server/src/pin_store.rs`
- Modify: `crates/iem-server/src/lib.rs` (AppState, `AppState::try_new`, `start_server` serving, start-up tests), `crates/iem-server/src/auth.rs` (login, change_pin, helpers, tests), `crates/iem-server/src/routes.rs` and `crates/iem-server/src/proxy.rs` (X10 passthrough removal), `crates/iem-server/src/backup_capture.rs`, `crates/iem-server/src/backup_restore.rs`, `crates/iem-core/src/config.rs` (remove `pins`, `engineer_pin`), `crates/iem-ui/src/api.rs`, `crates/iem-tray/src/lib.rs` (exit when the server fails to start)

**Interfaces:**
- Consumes: `pin_hash`, `pepper`, `login_guard`, `secrets::SECRETS_DIR` (Tasks 6, 8, 9).
- Produces: `pin_store::{PIN_HASHES_FILE = "pin_hashes.json", ENGINEER_ID = "engineer", PinStore::load(secrets_dir: &Path) -> io::Result<PinStore>, engineer_hash(&self) -> Option<&str>, member_hash(&self, &str) -> Option<&str>, set_engineer_hash(&mut self, String) -> io::Result<()>, set_member_hash(&mut self, &str, String) -> io::Result<()>}`; `AppState::try_new(Config, &Path) -> io::Result<AppState>` (production), `AppState::new(Config, &Path) -> AppState` only under `cfg(any(test, feature = "test-helpers"))`; `AppState` fields `pin_hasher: PinHasher`, `login_guard: Arc<LoginGuard>`, `hash_gate: Arc<HashGate>`; no `/api/reaper/*` route and no `proxy::proxy_reaper`; `auth::login`/`auth::change_pin` take `ConnectInfo<SocketAddr>` and return `Response` errors (429 + `Retry-After`); `auth::too_many_attempts(Duration) -> Response`; `backup_restore::pins_skip_notice(&MixerBackup) -> Option<SkippedEntry>`; UI `api::login_error_message(status: u16, retry_after: Option<&str>) -> String`.

- [ ] **Step 1: Rewrite `pin_store.rs` (tests included)**

```rust
//! Argon2id PIN hashes (program spec §5.3): `<secrets>/pin_hashes.json`.
//! Never plaintext: every stored value must be an argon2id PHC string, and a
//! file that holds anything else is a load error instead of being ignored.
//! The predecessor's plaintext `pins.json` is never read.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic_write;

/// File name inside the secrets directory.
pub const PIN_HASHES_FILE: &str = "pin_hashes.json";
/// Member id of the engineer; its PIN is the engineer PIN.
pub const ENGINEER_ID: &str = "engineer";

#[derive(Debug, Default, Serialize, Deserialize)]
struct PinFile {
    #[serde(default)]
    engineer: Option<String>,
    #[serde(default)]
    members: BTreeMap<String, String>,
}

/// Engineer and member PIN hashes, persisted atomically.
#[derive(Debug)]
pub struct PinStore {
    file: PinFile,
    path: PathBuf,
}

fn check_phc(owner: &str, phc: &str) -> io::Result<()> {
    if phc.starts_with("$argon2id$") {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the PIN entry for {owner} is not an argon2id hash"),
        ))
    }
}

impl PinStore {
    /// Load from `secrets_dir` (empty when the file does not exist yet).
    pub fn load(secrets_dir: &Path) -> io::Result<Self> {
        let path = secrets_dir.join(PIN_HASHES_FILE);
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<PinFile>(&text).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display()))
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => PinFile::default(),
            Err(e) => return Err(e),
        };
        if let Some(phc) = &file.engineer {
            check_phc(ENGINEER_ID, phc)?;
        }
        for (member, phc) in &file.members {
            check_phc(member, phc)?;
        }
        Ok(Self { file, path })
    }

    pub fn engineer_hash(&self) -> Option<&str> {
        self.file.engineer.as_deref()
    }

    pub fn member_hash(&self, member_id: &str) -> Option<&str> {
        self.file.members.get(member_id).map(String::as_str)
    }

    pub fn set_engineer_hash(&mut self, phc: String) -> io::Result<()> {
        check_phc(ENGINEER_ID, &phc)?;
        self.file.engineer = Some(phc);
        self.save()
    }

    pub fn set_member_hash(&mut self, member_id: &str, phc: String) -> io::Result<()> {
        iem_core::config::validate_member_id(member_id)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        if member_id == ENGINEER_ID {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the engineer PIN is stored with set_engineer_hash",
            ));
        }
        check_phc(member_id, &phc)?;
        self.file.members.insert(member_id.to_string(), phc);
        self.save()
    }

    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.file).map_err(io::Error::other)?;
        atomic_write(&self.path, &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pin_hash::{PEPPER_LEN, PinHasher};

    fn hasher() -> PinHasher {
        PinHasher::for_tests([1u8; PEPPER_LEN])
    }

    #[test]
    fn an_empty_directory_has_no_hashes() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::load(dir.path()).unwrap();
        assert!(store.engineer_hash().is_none());
        assert!(store.member_hash("member1").is_none());
    }

    #[test]
    fn hashes_survive_a_reload_and_never_contain_the_pin() {
        let dir = tempfile::tempdir().unwrap();
        let h = hasher();
        {
            let mut store = PinStore::load(dir.path()).unwrap();
            store.set_engineer_hash(h.hash("2468")).unwrap();
            store.set_member_hash("member1", h.hash("1357")).unwrap();
        }
        let store = PinStore::load(dir.path()).unwrap();
        assert!(h.verify("2468", store.engineer_hash().unwrap()));
        assert!(h.verify("1357", store.member_hash("member1").unwrap()));
        let text = std::fs::read_to_string(dir.path().join(PIN_HASHES_FILE)).unwrap();
        assert!(!text.contains("\"2468\"") && !text.contains("\"1357\""));
    }

    #[test]
    fn a_plaintext_value_is_a_load_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PIN_HASHES_FILE), r#"{"members":{"member1":"1357"}}"#).unwrap();
        assert_eq!(PinStore::load(dir.path()).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn corrupt_json_is_a_load_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PIN_HASHES_FILE), "{not json").unwrap();
        assert_eq!(PinStore::load(dir.path()).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn the_predecessor_plaintext_file_is_never_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pins.json"), r#"{"member1":"1357"}"#).unwrap();
        assert!(PinStore::load(dir.path()).unwrap().member_hash("member1").is_none());
    }

    #[test]
    fn the_engineer_is_not_a_member_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        let err = store.set_member_hash(ENGINEER_ID, hasher().hash("2468")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn member_ids_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(store.set_member_hash("../x", hasher().hash("2468")).is_err());
    }

    #[test]
    fn non_phc_values_are_refused_on_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = PinStore::load(dir.path()).unwrap();
        assert!(store.set_member_hash("member1", "1357".to_string()).is_err());
        assert!(store.set_engineer_hash("2468".to_string()).is_err());
        assert!(!dir.path().join(PIN_HASHES_FILE).exists());
    }
}
```

- [ ] **Step 2: AppState, secrets directory and connect-info serving (`crates/iem-server/src/lib.rs`)**

In `pub struct AppState`, replace the `pin_store` field doc with `/// Argon2id PIN hashes (`<config dir>/secrets/pin_hashes.json`)` and add after it:
```rust
    /// argon2id hasher keyed with the pepper (`<config dir>/secrets/`)
    pub pin_hasher: pin_hash::PinHasher,
    /// Login failure budgets (program spec §5.3)
    pub login_guard: Arc<login_guard::LoginGuard>,
    /// Bounded concurrency for argon2id work
    pub hash_gate: Arc<login_guard::HashGate>,
```
Rename `pub fn new(config: Config, config_dir: &std::path::Path) -> Self` to the fallible production constructor, loading the pepper and the PIN hashes with `?` before `Self {` and wrapping the literal in `Ok(…)`:
```rust
    /// Production constructor. Loads the PIN pepper and the PIN hashes from
    /// `<config dir>/secrets/`; an unreadable pepper or a corrupt or plaintext
    /// PIN store is an error — never regenerated, never ignored — so the server
    /// refuses to start.
    pub fn try_new(config: Config, config_dir: &std::path::Path) -> std::io::Result<Self> {
        let secrets_dir = config_dir.join(secrets::SECRETS_DIR);
        let pepper = pepper::load_or_create(&secrets_dir)?;
        let pin_store = pin_store::PinStore::load(&secrets_dir)?;
        // … the existing body up to `Self {` unchanged …
        Ok(Self {
            // … existing fields …
        })
    }

    /// Test constructor: panics where `try_new` returns an error.
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn new(config: Config, config_dir: &std::path::Path) -> Self {
        Self::try_new(config, config_dir).expect("test AppState: pepper and PIN store")
    }
```
In the struct literal set `pin_store: Arc::new(RwLock::new(pin_store)),`, `pin_hasher: pin_hash::PinHasher::new(pepper),`, `login_guard: Arc::new(login_guard::LoginGuard::new()),`, `hash_gate: Arc::new(login_guard::HashGate::new(login_guard::HASH_CONCURRENCY, login_guard::HASH_QUEUE)),`. `new_for_test` keeps calling `Self::new` (test-helpers only). Every other caller of `AppState::new` is a test (`proxy.rs`, `routes.rs`, `tunnel_watch/tests.rs`, the new tests of this plan).
In `start_server`: `let state = AppState::new(config, &server_config.config_dir);` → `let state = AppState::try_new(config, &server_config.config_dir).context("loading the PIN pepper and PIN hashes")?;` (`use anyhow::Context as _;`); the HTTPS server `.serve(https_app.into_make_service())` → `.serve(https_app.into_make_service_with_connect_info::<SocketAddr>())`; `axum::serve(listener, app).await?;` → `axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;`.
`crates/iem-tray/src/lib.rs`, in the wait for server readiness: the arm `Ok(Err(_)) => tracing::error!("Server startup channel dropped"),` becomes
```rust
            Ok(Err(_)) => {
                // The server returned before it was ready (e.g. a corrupt pepper
                // or PIN store): refuse to run a tray without a server.
                tracing::error!("the server failed to start — see the error above; exiting");
                std::process::exit(1);
            }
```
Start-up tests, new module at the end of `crates/iem-server/src/lib.rs`:
```rust
#[cfg(test)]
mod startup_tests {
    use super::*;

    fn server_config(dir: &std::path::Path) -> ServerConfig {
        ServerConfig { port: 0, config: Config::default(), config_dir: dir.to_path_buf() }
    }

    #[tokio::test]
    async fn start_refuses_a_plaintext_pin_store() {
        let dir = tempfile::tempdir().unwrap();
        let secrets_dir = dir.path().join(secrets::SECRETS_DIR);
        std::fs::create_dir_all(&secrets_dir).unwrap();
        std::fs::write(secrets_dir.join(pin_store::PIN_HASHES_FILE), r#"{"members":{"member1":"1357"}}"#).unwrap();
        let err = start_server(server_config(dir.path()), None).await.unwrap_err();
        assert!(format!("{err:#}").contains("argon2id"), "{err:#}");
    }

    // Linux only: on Windows the pepper file is DPAPI data and the error text differs.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn start_refuses_a_corrupt_pepper() {
        let dir = tempfile::tempdir().unwrap();
        let secrets_dir = dir.path().join(secrets::SECRETS_DIR);
        std::fs::create_dir_all(&secrets_dir).unwrap();
        std::fs::write(secrets_dir.join(pepper::PEPPER_FILE), b"short").unwrap();
        let err = start_server(server_config(dir.path()), None).await.unwrap_err();
        assert!(format!("{err:#}").contains("pepper"), "{err:#}");
        assert_eq!(std::fs::read(secrets_dir.join(pepper::PEPPER_FILE)).unwrap(), b"short", "never replaced");
    }
}
```
Both return before any port is bound (the JWT/VAPID secrets are created in the temp dir first, which is harmless).

- [ ] **Step 2b: Remove the raw REAPER passthrough (X10) with a failing test first**

Add to `crates/iem-server/src/routes.rs` `mod tests` (it already has `make_test_token`):
```rust
    /// X10: the raw REAPER passthrough is gone — even an engineer gets 404.
    #[tokio::test]
    async fn raw_reaper_passthrough_is_gone() {
        use axum::body::Body;
        use axum::http::{Method, Request, StatusCode};
        use tower::ServiceExt;

        let secret = "passthrough-test-secret";
        let dir = tempfile::tempdir().unwrap();
        let config = iem_core::Config { jwt_secret: secret.to_string(), ..iem_core::Config::default() };
        let state = AppState::new(config, dir.path());
        let router = api_routes(state.clone()).with_state(state);
        let token = make_test_token(secret, "engineer", true);
        for method in [Method::GET, Method::POST] {
            let resp = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri("/api/reaper/_/NTRACK")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{method}");
        }
    }
```
(Before the removal it answers 502 — REAPER is unreachable — which proves the test sees the route.) Then delete, in `routes.rs`, the comment `// Raw REAPER proxy` with its `.route("/api/reaper/{*path}", any(reaper_proxy))` line and the whole `reaper_proxy` handler (from its doc comment `/// REAPER proxy handler (engineer-only, requires auth)`); in `proxy.rs`, the whole `pub async fn proxy_reaper` with its doc comment. Remove the imports that become unused — `any` and `Method` in `routes.rs`, `body::Body` and `Method` in `proxy.rs` (the test modules import their own) — and let CI's clippy `-D warnings` confirm nothing else went unused.

- [ ] **Step 3: Backups never carry PINs**

`crates/iem-server/src/backup_capture.rs`: replace the block from `// --- 7. Read PINs ---` through its `tracing::info!(…);` with
```rust
    // --- 7. PINs are never part of a backup (security baseline) ---
    let pins = std::collections::HashMap::new();
```
(keeping `pins,` in the `MixerBackup` literal). `crates/iem-server/src/backup_restore.rs`: delete the preview block from `// --- PINs ---` through its closing `}` of the `for` loop; replace the apply block from `// --- Apply PINs (only if changed) ---` through its closing `}` with
```rust
    // --- PINs are never restored from backups (security baseline) ---
    if let Some(notice) = pins_skip_notice(&backup) {
        skipped.push(notice);
    }
```
and add near the top-level helpers:
```rust
/// Backups made by the predecessor may carry plaintext PINs; they are never
/// restored — the engineer resets PINs instead.
pub fn pins_skip_notice(backup: &MixerBackup) -> Option<SkippedEntry> {
    (!backup.pins.is_empty()).then(|| SkippedEntry {
        category: RestoreCategory::Pin,
        description: "pins".to_string(),
        reason: "PINs are never restored from backups; reset them on the engineer page".to_string(),
    })
}
```
with a unit test in the file's test module:
```rust
    #[test]
    fn pins_in_a_legacy_backup_are_skipped_with_a_notice() {
        let mut backup = MixerBackup::default();
        assert!(pins_skip_notice(&backup).is_none());
        backup.pins.insert("member1".to_string(), "<PIN>".to_string());
        let notice = pins_skip_notice(&backup).expect("notice");
        assert_eq!(notice.category, RestoreCategory::Pin);
    }
```
(`MixerBackup` derives `Default`; `RestoreCategory` derives `PartialEq` and `Debug`.)

- [ ] **Step 4: Remove the plaintext PIN fields from the site config**

`crates/iem-core/src/config.rs`: delete the `pins` and `engineer_pin` fields with their doc comments and serde attributes, and `pins: HashMap::new(),` / `engineer_pin: None,` from `Default`. Add to `mod tests`:
```rust
    #[test]
    fn test_plaintext_pins_are_rejected_in_the_site_file() {
        for text in ["engineer_pin = \"2468\"\n", "[pins]\nmember1 = \"2468\"\n"] {
            assert!(toml::from_str::<Config>(text).is_err(), "{text}");
        }
    }
```
`crates/iem-server/src/auth.rs` `test_config()`: drop the `engineer_pin: Some("9999".to_string()),` line.

- [ ] **Step 5: Rewrite `login` and `change_pin` in `crates/iem-server/src/auth.rs`**

Replace the file header, imports, `LoginRequest`…`ENGINEER_TOKEN_EXPIRY_SECS`, `login` and `change_pin` (everything above `fn issue_token`, plus `change_pin` below it) with:
```rust
//! Authentication: PIN login → JWT, PIN changes, token checks.
//!
//! Login protection (program spec §5.3): admission by `LoginGuard` before any
//! hashing, a bounded hashing gate, argon2id verification of PIN hashes,
//! failure-only budgets, never a lockout.

use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use iem_core::{ApiError, AuthClaims};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::login_guard::{ClientKey, FailureEffect};
use crate::pin_hash::{PinHasher, is_valid_pin_format};
use crate::pin_store::ENGINEER_ID;

/// Login request payload
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub member: String,
    pub pin: String,
}

/// Change PIN request payload
#[derive(Debug, Deserialize)]
pub struct ChangePinRequest {
    /// Required for members (their current PIN), ignored for engineers
    pub old_pin: Option<String>,
    pub new_pin: String,
    /// Target member — required for engineers, ignored for members (JWT `sub`)
    pub member: Option<String>,
}

/// Login response with JWT token
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub member: String,
    pub engineer: bool,
    pub expires_in: u64,
}

/// Token expiration for members (7 days)
const MEMBER_TOKEN_EXPIRY_SECS: u64 = 7 * 24 * 60 * 60;
/// Token expiration for engineers (7 days — same as members)
const ENGINEER_TOKEN_EXPIRY_SECS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinMatch {
    Engineer,
    Member,
    None,
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(ApiError::new(code, message))).into_response()
}

fn member_not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(ApiError::not_found("Member"))).into_response()
}

/// 429 with `Retry-After` in whole seconds (at least 1).
pub fn too_many_attempts(wait: Duration) -> Response {
    let secs = wait.as_millis().div_ceil(1000).max(1);
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, secs.to_string())],
        Json(ApiError::new("TOO_MANY_ATTEMPTS", "Too many attempts, try again later")),
    )
        .into_response()
}

/// Run argon2id work on the blocking pool, admitted by the hashing gate.
async fn with_hasher<T: Send + 'static>(
    state: &AppState,
    job: impl FnOnce(&PinHasher) -> T + Send + 'static,
) -> Result<T, Response> {
    let Some(permit) = state.hash_gate.acquire().await else {
        tracing::warn!("PIN hashing gate full — answering 429");
        return Err(too_many_attempts(Duration::from_secs(1)));
    };
    let hasher = state.pin_hasher.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job(&hasher)
    })
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "PIN hashing task failed");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "HASH_ERROR", "PIN check failed")
    })
}

async fn member_exists(state: &AppState, member: &str) -> bool {
    state.discovered_members.read().await.iter().any(|m| m.id() == member)
}

fn record_failure(state: &AppState, client: &ClientKey, member: &str, now: Instant) {
    if state.login_guard.record_failure(client, member, now) == FailureEffect::EngineerBudgetExhausted {
        tracing::warn!(
            origin = ?client.origin,
            "login failures exhausted the engineer budget — attempts from this origin are now spaced"
        );
    }
}

/// Handle login and return a JWT.
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, Response> {
    let client = ClientKey::from_request(peer.ip(), &headers);
    let now = Instant::now();
    if let Err(wait) = state.login_guard.check(&client, &req.member, now) {
        tracing::info!(origin = ?client.origin, member = %req.member, wait_ms = wait.as_millis() as u64, "login throttled");
        return Err(too_many_attempts(wait));
    }
    if !req.member.is_empty() && !member_exists(&state, &req.member).await {
        return Err(member_not_found());
    }
    let (engineer_hash, member_hash) = {
        let store = state.pin_store.read().await;
        (
            store.engineer_hash().map(str::to_owned),
            store.member_hash(&req.member).map(str::to_owned),
        )
    };
    let pin = req.pin.clone();
    let matched = with_hasher(&state, move |hasher| {
        if hasher.verify_optional(&pin, engineer_hash.as_deref()) {
            PinMatch::Engineer
        } else if hasher.verify_optional(&pin, member_hash.as_deref()) {
            PinMatch::Member
        } else {
            PinMatch::None
        }
    })
    .await?;
    let config = state.config.read().await;
    match matched {
        PinMatch::Engineer => {
            state.login_guard.record_success(&client, &req.member);
            issue_token(&config, ENGINEER_ID, true).map_err(IntoResponse::into_response)
        }
        PinMatch::Member => {
            state.login_guard.record_success(&client, &req.member);
            issue_token(&config, &req.member, false).map_err(IntoResponse::into_response)
        }
        PinMatch::None => {
            record_failure(&state, &client, &req.member, now);
            tracing::info!(origin = ?client.origin, member = %req.member, "login failed: invalid PIN");
            Err(error_response(StatusCode::UNAUTHORIZED, "INVALID_PIN", "Invalid PIN"))
        }
    }
}
```
and, in place of the old `change_pin`:
```rust
/// Change a PIN.
///
/// - **Engineers**: set any member's PIN (`member` required, no old PIN), or
///   the engineer PIN with `member = "engineer"`.
/// - **Members**: change their own PIN (`old_pin` required, `member` ignored);
///   a wrong current PIN counts against the login budgets.
pub async fn change_pin(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ChangePinRequest>,
) -> Result<StatusCode, Response> {
    let claims = {
        let config = state.config.read().await;
        extract_claims_from_header(&headers, &config.jwt_secret).map_err(IntoResponse::into_response)?
    };
    if !is_valid_pin_format(&req.new_pin) {
        return Err(error_response(StatusCode::BAD_REQUEST, "INVALID_FORMAT", "PIN must be exactly 4 digits"));
    }
    let target = if claims.engineer {
        let member = req.member.as_deref().unwrap_or("");
        if member.is_empty() {
            return Err(error_response(StatusCode::BAD_REQUEST, "MISSING_MEMBER", "Engineer must specify target member"));
        }
        if member != ENGINEER_ID && !member_exists(&state, member).await {
            return Err(member_not_found());
        }
        member.to_string()
    } else {
        let old_pin = req.old_pin.clone().unwrap_or_default();
        if old_pin.is_empty() {
            return Err(error_response(StatusCode::BAD_REQUEST, "MISSING_OLD_PIN", "Current PIN is required"));
        }
        let client = ClientKey::from_request(peer.ip(), &headers);
        let now = Instant::now();
        if let Err(wait) = state.login_guard.check(&client, &claims.sub, now) {
            return Err(too_many_attempts(wait));
        }
        let current = state.pin_store.read().await.member_hash(&claims.sub).map(str::to_owned);
        let old_ok = with_hasher(&state, move |hasher| hasher.verify_optional(&old_pin, current.as_deref())).await?;
        if !old_ok {
            record_failure(&state, &client, &claims.sub, now);
            return Err(error_response(StatusCode::UNAUTHORIZED, "INVALID_PIN", "Current PIN is incorrect"));
        }
        state.login_guard.record_success(&client, &claims.sub);
        claims.sub.clone()
    };
    let new_pin = req.new_pin.clone();
    let phc = with_hasher(&state, move |hasher| hasher.hash(&new_pin)).await?;
    let saved = {
        let mut store = state.pin_store.write().await;
        if target == ENGINEER_ID {
            store.set_engineer_hash(phc)
        } else {
            store.set_member_hash(&target, phc)
        }
    };
    saved.map_err(|e| {
        tracing::error!(error = %e, member = %target, "failed to save the PIN hash");
        error_response(StatusCode::INTERNAL_SERVER_ERROR, "IO_ERROR", "Failed to save PIN")
    })?;
    tracing::info!(member = %target, by = %claims.sub, "PIN changed");
    Ok(StatusCode::OK)
}
```
Keep `issue_token`, `extract_claims_from_header`, `verify_token`, `verify_member_access`, `extract_claims` and the existing `mod tests` unchanged (the old `use iem_core::config::constant_time_eq;` import is gone).

- [ ] **Step 6: Add the router-level tests to `crates/iem-server/src/auth.rs`**

```rust
#[cfg(test)]
mod login_tests {
    use super::*;
    use crate::login_guard::HashGate;
    use crate::pin_hash::{PEPPER_LEN, PinHasher};
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request;
    use axum::routing::post;
    use std::sync::Arc;
    use tower::util::ServiceExt;

    const ENGINEER_PIN: &str = "2468";
    const MEMBER_PIN: &str = "1357";
    const WRONG_PIN: &str = "9753";
    const NEW_PIN: &str = "8642";
    const SECRET: &str = "login-test-secret";
    const LAN: [u8; 4] = [10, 0, 0, 50];
    const LOOPBACK: [u8; 4] = [127, 0, 0, 1];

    fn discovered(name: &str) -> iem_core::DiscoveredMember {
        iem_core::DiscoveredMember {
            name: name.to_string(),
            track_index: 0,
            dante_output_l: 71,
            dante_output_r: 72,
            send_index: 0,
            mix_send_index: None,
            mix_send_indices: std::collections::HashMap::new(),
        }
    }

    /// member1 (PIN set), member2 (no PIN yet), engineer; fast test hasher.
    async fn test_state(dir: &std::path::Path) -> AppState {
        let config = iem_core::Config { jwt_secret: SECRET.to_string(), ..iem_core::Config::default() };
        let mut state = AppState::new(config, dir);
        state.pin_hasher = PinHasher::for_tests([5u8; PEPPER_LEN]);
        {
            let mut members = state.discovered_members.write().await;
            members.push(discovered("MEMBER1"));
            members.push(discovered("MEMBER2"));
            members.push(discovered("ENGINEER"));
        }
        let engineer_hash = state.pin_hasher.hash(ENGINEER_PIN);
        let member_hash = state.pin_hasher.hash(MEMBER_PIN);
        {
            let mut store = state.pin_store.write().await;
            store.set_engineer_hash(engineer_hash).unwrap();
            store.set_member_hash("member1", member_hash).unwrap();
        }
        state
    }

    fn app(state: AppState, peer: [u8; 4]) -> axum::Router {
        axum::Router::new()
            .route("/api/auth", post(login))
            .route("/api/auth/change-pin", post(change_pin))
            .with_state(state)
            .layer(MockConnectInfo(SocketAddr::from((peer, 40000))))
    }

    async fn post_json(app: &axum::Router, uri: &str, body: serde_json::Value, headers: &[(&str, &str)]) -> Response {
        let mut req = Request::builder().method("POST").uri(uri).header("content-type", "application/json");
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        app.clone().oneshot(req.body(Body::from(body.to_string())).unwrap()).await.unwrap()
    }

    async fn login_as(app: &axum::Router, member: &str, pin: &str, headers: &[(&str, &str)]) -> Response {
        post_json(app, "/api/auth", serde_json::json!({ "member": member, "pin": pin }), headers).await
    }

    async fn json_body(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn bearer(member: &str, engineer: bool) -> String {
        let config = iem_core::Config { jwt_secret: SECRET.to_string(), ..iem_core::Config::default() };
        format!("Bearer {}", issue_token(&config, member, engineer).unwrap().0.token)
    }

    async fn change(app: &axum::Router, auth: &str, body: serde_json::Value) -> Response {
        post_json(app, "/api/auth/change-pin", body, &[("authorization", auth)]).await
    }

    #[tokio::test]
    async fn member_pin_logs_the_member_in() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = login_as(&app, "member1", MEMBER_PIN, &[]).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = json_body(resp).await;
        assert_eq!(body["member"], "member1");
        assert_eq!(body["engineer"], false);
    }

    #[tokio::test]
    async fn engineer_pin_works_from_any_member_login() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let body = json_body(login_as(&app, "member1", ENGINEER_PIN, &[]).await).await;
        assert_eq!(body["engineer"], true);
        assert_eq!(body["member"], "engineer");
    }

    #[tokio::test]
    async fn engineer_login_needs_the_engineer_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        assert_eq!(login_as(&app, "engineer", MEMBER_PIN, &[]).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(login_as(&app, "engineer", ENGINEER_PIN, &[]).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn wrong_pin_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = login_as(&app, "member1", WRONG_PIN, &[]).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(json_body(resp).await["code"], "INVALID_PIN");
    }

    #[tokio::test]
    async fn member_without_a_pin_cannot_log_in() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        assert_eq!(login_as(&app, "member2", MEMBER_PIN, &[]).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(login_as(&app, "member2", "0000", &[]).await.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unknown_member_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        assert_eq!(login_as(&app, "member7", MEMBER_PIN, &[]).await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn fourth_attempt_after_three_failures_is_throttled_even_with_the_right_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        for _ in 0..3 {
            assert_eq!(login_as(&app, "member1", WRONG_PIN, &[]).await.status(), StatusCode::UNAUTHORIZED);
        }
        let resp = login_as(&app, "member1", MEMBER_PIN, &[]).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "1");
        assert_eq!(json_body(resp).await["code"], "TOO_MANY_ATTEMPTS");
    }

    #[tokio::test]
    async fn failures_of_one_client_do_not_slow_another() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path()).await;
        let first = app(state.clone(), LAN);
        for _ in 0..3 {
            login_as(&first, "member1", WRONG_PIN, &[]).await;
        }
        let other = app(state, [10, 0, 0, 51]);
        assert_eq!(login_as(&other, "member1", MEMBER_PIN, &[]).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn tunnel_failures_do_not_slow_lan_logins() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path()).await;
        let tunnel = app(state.clone(), LOOPBACK);
        let cf = [("cf-connecting-ip", "203.0.113.9")];
        for _ in 0..3 {
            assert_eq!(login_as(&tunnel, "member1", WRONG_PIN, &cf).await.status(), StatusCode::UNAUTHORIZED);
        }
        assert_eq!(login_as(&tunnel, "member1", MEMBER_PIN, &cf).await.status(), StatusCode::TOO_MANY_REQUESTS);
        let lan = app(state, LAN);
        assert_eq!(login_as(&lan, "member1", MEMBER_PIN, &[]).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn forged_cf_header_from_a_lan_peer_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        for n in 0..3 {
            let ip = format!("203.0.113.{n}");
            let status = login_as(&app, "member1", WRONG_PIN, &[("cf-connecting-ip", ip.as_str())]).await.status();
            assert_eq!(status, StatusCode::UNAUTHORIZED);
        }
        let resp = login_as(&app, "member1", MEMBER_PIN, &[("cf-connecting-ip", "203.0.113.99")]).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn success_resets_the_failure_streak() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        for _ in 0..2 {
            login_as(&app, "member1", WRONG_PIN, &[]).await;
        }
        assert_eq!(login_as(&app, "member1", MEMBER_PIN, &[]).await.status(), StatusCode::OK);
        for _ in 0..2 {
            assert_eq!(login_as(&app, "member1", WRONG_PIN, &[]).await.status(), StatusCode::UNAUTHORIZED);
        }
        assert_eq!(login_as(&app, "member1", MEMBER_PIN, &[]).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn full_hashing_gate_answers_429_without_counting_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path()).await;
        state.hash_gate = Arc::new(HashGate::new(0, 0));
        let app = app(state.clone(), LAN);
        let resp = login_as(&app, "member1", MEMBER_PIN, &[]).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "1");
        assert_eq!(state.login_guard.stats().lan_failures, 0);
    }

    #[test]
    fn retry_after_is_whole_seconds_rounded_up() {
        for (wait, expected) in [(Duration::from_millis(1), "1"), (Duration::from_millis(1001), "2"), (Duration::from_secs(60), "60")] {
            assert_eq!(too_many_attempts(wait).headers()[header::RETRY_AFTER], expected);
        }
    }

    #[tokio::test]
    async fn member_changes_own_pin_with_the_current_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(&app, &bearer("member1", false), serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": NEW_PIN })).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(login_as(&app, "member1", NEW_PIN, &[]).await.status(), StatusCode::OK);
        assert_eq!(login_as(&app, "member1", MEMBER_PIN, &[]).await.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_current_pin_is_rejected_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let auth = bearer("member1", false);
        for _ in 0..3 {
            let resp = change(&app, &auth, serde_json::json!({ "old_pin": WRONG_PIN, "new_pin": NEW_PIN })).await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
        let resp = change(&app, &auth, serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": NEW_PIN })).await;
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn member_token_changes_only_its_own_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let body = serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": NEW_PIN, "member": "member2" });
        assert_eq!(change(&app, &bearer("member1", false), body).await.status(), StatusCode::OK);
        assert_eq!(login_as(&app, "member2", NEW_PIN, &[]).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(login_as(&app, "member1", NEW_PIN, &[]).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn engineer_resets_a_member_pin_without_the_old_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(&app, &bearer("engineer", true), serde_json::json!({ "new_pin": NEW_PIN, "member": "member2" })).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(login_as(&app, "member2", NEW_PIN, &[]).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn engineer_can_rotate_the_engineer_pin() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(&app, &bearer("engineer", true), serde_json::json!({ "new_pin": NEW_PIN, "member": "engineer" })).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_body(login_as(&app, "engineer", NEW_PIN, &[]).await).await["engineer"], true);
        assert_eq!(login_as(&app, "engineer", ENGINEER_PIN, &[]).await.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn new_pin_must_be_four_digits() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(&app, &bearer("member1", false), serde_json::json!({ "old_pin": MEMBER_PIN, "new_pin": "12a4" })).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn engineer_reset_of_an_unknown_member_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = change(&app, &bearer("engineer", true), serde_json::json!({ "new_pin": NEW_PIN, "member": "member7" })).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn change_pin_requires_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let app = app(test_state(dir.path()).await, LAN);
        let resp = post_json(&app, "/api/auth/change-pin", serde_json::json!({ "new_pin": NEW_PIN }), &[]).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
```

- [ ] **Step 7: UI — show the wait time on 429 (`crates/iem-ui/src/api.rs`)**

In `login`, replace
```rust
    } else if resp.status() == 401 {
        Err("Invalid PIN".to_string())
    } else {
        Err(format!("Server error: {}", resp.status()))
    }
```
with
```rust
    } else {
        let retry_after = resp.headers().get("retry-after");
        Err(login_error_message(resp.status(), retry_after.as_deref()))
    }
```
and add
```rust
/// User-facing text of a failed login (`status` = HTTP status,
/// `retry_after` = the `Retry-After` header).
pub fn login_error_message(status: u16, retry_after: Option<&str>) -> String {
    match status {
        401 => "Invalid PIN".to_string(),
        429 => {
            let secs = retry_after.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(1);
            format!("Too many attempts. Try again in {secs} s")
        }
        other => format!("Server error: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_errors_are_readable() {
        assert_eq!(login_error_message(401, None), "Invalid PIN");
        assert_eq!(login_error_message(429, Some("7")), "Too many attempts. Try again in 7 s");
        assert_eq!(login_error_message(429, None), "Too many attempts. Try again in 1 s");
        assert_eq!(login_error_message(429, Some("soon")), "Too many attempts. Try again in 1 s");
        assert_eq!(login_error_message(500, None), "Server error: 500");
    }
}
```

- [ ] **Step 8: Format and commit**

```bash
cd "$WORK" && cargo fmt --all
git add crates
git commit -m "feat(auth): hashed PIN store, login protection on login and PIN change, no PINs in backups, no raw REAPER passthrough" -m "No compiled-in or plaintext PINs anywhere; a bad pepper or PIN store stops start-up; /api/reaper/* is gone (X10); the login page shows the wait time on 429. Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected in CI (Task 15): `pin_store::`, `auth::login_tests::`, `startup_tests::`, `routes::tests::raw_reaper_passthrough_is_gone`, the backup notice test and the UI `login_errors_are_readable` pass.

---

### Task 11: PIN provisioning CLI (`iem-server pin …`)

**Files:**
- Create: `crates/iem-server/src/provision.rs`, `crates/iem-server/tests/pin_cli.rs`
- Rewrite: `crates/iem-server/src/bin/server.rs`
- Modify: `crates/iem-server/src/lib.rs` (add `pub mod provision;` after `pub mod preset_store;`), `crates/iem-server/Cargo.toml` (test target)

**Interfaces:**
- Consumes: `Config::load`, `pepper::load_or_create`, `PinHasher::new`, `PinStore`, `secrets::SECRETS_DIR`.
- Produces: `provision::{PinTarget { Engineer, Member(String) }, PinTarget::label(&self) -> String, ProvisionError { Invalid(String), Io(io::Error), Config(String) }, config_dir_of(&Path) -> PathBuf, read_pin(impl BufRead) -> Result<String, ProvisionError>, check_target(&Config, &PinTarget) -> Result<(), ProvisionError>, store_pin(&Path, &PinTarget, &str) -> Result<(), ProvisionError>, run(&Path, &PinTarget, impl BufRead) -> Result<(), ProvisionError>}`; binary contract `iem-server` (serve), `iem-server pin set-engineer`, `iem-server pin set-member <id>` — PIN on stdin; exit 0 ok, 2 invalid input/usage, 1 I/O or config error.

- [ ] **Step 1: Write the failing integration test**

`crates/iem-server/Cargo.toml`, next to the existing `[[test]]`:
```toml
[[test]]
name = "pin_cli"
path = "tests/pin_cli.rs"
required-features = ["standalone"]
```
`crates/iem-server/tests/pin_cli.rs`:
```rust
//! `iem-server pin …` end to end: the binary reads the PIN from stdin and
//! stores only an argon2id hash next to the site config.

use std::io::Write;
use std::process::{Command, Output, Stdio};

const SITE: &str = "[[members]]\nname = \"Member1\"\ndante_output_l = 71\ndante_output_r = 72\n\n\
[[members]]\nname = \"Engineer\"\ndante_output_l = 91\ndante_output_r = 92\n";

fn site_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("iemmixer.toml"), SITE).unwrap();
    dir
}

fn run_pin(dir: &std::path::Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_iem-server"))
        .args(args)
        .env("IEMMIXER_CONFIG", dir.join("iemmixer.toml"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn iem-server");
    // The child may exit before reading stdin (invalid target); a broken pipe
    // here is expected in that case and the exit code is what the test checks.
    let _ = child.stdin.take().expect("stdin").write_all(stdin.as_bytes());
    child.wait_with_output().expect("wait for iem-server")
}

fn stored(dir: &std::path::Path) -> serde_json::Value {
    let text = std::fs::read_to_string(dir.join("secrets").join("pin_hashes.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn set_engineer_stores_only_a_hash() {
    let dir = site_dir();
    let out = run_pin(dir.path(), &["pin", "set-engineer"], "2468\n");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stored(dir.path())["engineer"].as_str().unwrap().starts_with("$argon2id$"));
    let text = std::fs::read_to_string(dir.path().join("secrets").join("pin_hashes.json")).unwrap();
    assert!(!text.contains("\"2468\""));
}

#[test]
fn set_member_stores_the_member_hash() {
    let dir = site_dir();
    let out = run_pin(dir.path(), &["pin", "set-member", "member1"], "1357\n");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stored(dir.path())["members"]["member1"].as_str().unwrap().starts_with("$argon2id$"));
}

#[test]
fn an_invalid_pin_is_rejected_with_exit_2() {
    let dir = site_dir();
    let out = run_pin(dir.path(), &["pin", "set-engineer"], "12a4\n");
    assert_eq!(out.status.code(), Some(2));
    assert!(!dir.path().join("secrets").join("pin_hashes.json").exists());
}

#[test]
fn the_engineer_cannot_be_set_as_a_member() {
    let dir = site_dir();
    assert_eq!(run_pin(dir.path(), &["pin", "set-member", "engineer"], "2468\n").status.code(), Some(2));
}

#[test]
fn an_unknown_member_is_rejected() {
    let dir = site_dir();
    assert_eq!(run_pin(dir.path(), &["pin", "set-member", "member9"], "2468\n").status.code(), Some(2));
}

#[test]
fn an_unknown_command_prints_usage_with_exit_2() {
    let dir = site_dir();
    let out = run_pin(dir.path(), &["pin"], "");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}

#[test]
fn a_missing_site_config_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(run_pin(dir.path(), &["pin", "set-engineer"], "2468\n").status.code(), Some(1));
}
```

- [ ] **Step 2: Write `provision.rs` (with unit tests)**

```rust
//! PIN provisioning for `iem-server pin …`: the first engineer PIN at
//! bootstrap (server stopped), member PINs for tests. Members normally get
//! their PIN from an engineer reset in the UI (F3). The PIN is read from
//! stdin, never from argv (process listings, shell history).

use std::io::BufRead;
use std::path::{Path, PathBuf};

use crate::pepper;
use crate::pin_hash::{PinHasher, is_valid_pin_format};
use crate::pin_store::{ENGINEER_ID, PinStore};
use crate::secrets::SECRETS_DIR;

/// Whose PIN is being set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinTarget {
    Engineer,
    Member(String),
}

impl PinTarget {
    pub fn label(&self) -> String {
        match self {
            Self::Engineer => "engineer".to_string(),
            Self::Member(id) => format!("member {id}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    /// Bad input: exit code 2.
    #[error("{0}")]
    Invalid(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("site config: {0}")]
    Config(String),
}

/// Directory holding the site config, its data and `secrets/`.
pub fn config_dir_of(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Read one line from `input` and check it is a valid PIN.
pub fn read_pin(mut input: impl BufRead) -> Result<String, ProvisionError> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let pin = line.trim_end_matches(['\r', '\n']).to_string();
    if is_valid_pin_format(&pin) {
        Ok(pin)
    } else {
        Err(ProvisionError::Invalid("the PIN must be exactly 4 digits".to_string()))
    }
}

/// A member target must be a configured member other than the engineer.
pub fn check_target(config: &iem_core::Config, target: &PinTarget) -> Result<(), ProvisionError> {
    if let PinTarget::Member(id) = target {
        if id == ENGINEER_ID {
            return Err(ProvisionError::Invalid("the engineer PIN is set with `pin set-engineer`".to_string()));
        }
        iem_core::config::validate_member_id(id).map_err(ProvisionError::Invalid)?;
        if !config.members.iter().any(|m| m.id() == *id) {
            return Err(ProvisionError::Invalid(format!("unknown member `{id}` (not in the site config)")));
        }
    }
    Ok(())
}

/// Hash `pin` with the installation's pepper and store it.
pub fn store_pin(config_dir: &Path, target: &PinTarget, pin: &str) -> Result<(), ProvisionError> {
    let secrets_dir = config_dir.join(SECRETS_DIR);
    let hasher = PinHasher::new(pepper::load_or_create(&secrets_dir)?);
    let mut store = PinStore::load(&secrets_dir)?;
    let phc = hasher.hash(pin);
    match target {
        PinTarget::Engineer => store.set_engineer_hash(phc)?,
        PinTarget::Member(id) => store.set_member_hash(id, phc)?,
    }
    Ok(())
}

/// The whole `pin set-…` command.
pub fn run(config_path: &Path, target: &PinTarget, input: impl BufRead) -> Result<(), ProvisionError> {
    let config = iem_core::Config::load(config_path).map_err(|e| ProvisionError::Config(e.to_string()))?;
    check_target(&config, target)?;
    let pin = read_pin(input)?;
    store_pin(&config_dir_of(config_path), target, &pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> iem_core::Config {
        toml::from_str(
            "[[members]]\nname = \"Member1\"\ndante_output_l = 71\ndante_output_r = 72\n",
        )
        .unwrap()
    }

    #[test]
    fn read_pin_accepts_four_digits_and_trims_the_newline() {
        assert_eq!(read_pin("2468\r\n".as_bytes()).unwrap(), "2468");
        assert!(matches!(read_pin("246\n".as_bytes()), Err(ProvisionError::Invalid(_))));
        assert!(matches!(read_pin("".as_bytes()), Err(ProvisionError::Invalid(_))));
    }

    #[test]
    fn check_target_rules() {
        let config = site();
        assert!(check_target(&config, &PinTarget::Engineer).is_ok());
        assert!(check_target(&config, &PinTarget::Member("member1".to_string())).is_ok());
        for bad in ["engineer", "member9", "../x"] {
            assert!(matches!(check_target(&config, &PinTarget::Member(bad.to_string())), Err(ProvisionError::Invalid(_))), "{bad}");
        }
    }

    #[test]
    fn config_dir_is_the_parent_or_the_current_directory() {
        assert_eq!(config_dir_of(Path::new("/srv/site/iemmixer.toml")), PathBuf::from("/srv/site"));
        assert_eq!(config_dir_of(Path::new("iemmixer.toml")), PathBuf::from("."));
    }

    #[test]
    fn store_pin_writes_a_hash_the_installation_can_verify() {
        let dir = tempfile::tempdir().unwrap();
        store_pin(dir.path(), &PinTarget::Engineer, "2468").unwrap();
        let secrets = dir.path().join(SECRETS_DIR);
        let hasher = PinHasher::new(pepper::load_or_create(&secrets).unwrap());
        let store = PinStore::load(&secrets).unwrap();
        assert!(hasher.verify("2468", store.engineer_hash().unwrap()));
    }

    #[test]
    fn labels_name_the_target() {
        assert_eq!(PinTarget::Engineer.label(), "engineer");
        assert_eq!(PinTarget::Member("member1".to_string()).label(), "member member1");
    }
}
```
Add `toml = "0.9"` to `[dev-dependencies]` of `crates/iem-server/Cargo.toml` (the `site()` helper parses TOML directly).

- [ ] **Step 3: Rewrite `crates/iem-server/src/bin/server.rs`**

```rust
//! `iem-server`: the standalone mixer server (CI E2E and the PC) and PIN
//! provisioning.
//!
//!   iem-server                      run the server
//!   iem-server pin set-engineer     read a PIN from stdin, store its hash as the engineer PIN
//!   iem-server pin set-member <id>  read a PIN from stdin, store its hash for member <id>
//!
//! The site config is `$IEMMIXER_CONFIG` (default `iemmixer.toml`); runtime
//! data and secrets live next to it.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use iem_core::Config;
use iem_server::ServerConfig;
use iem_server::provision::{self, PinTarget, ProvisionError};

const USAGE: &str =
    "usage: iem-server [pin set-engineer | pin set-member <member-id>]   (the PIN is read from stdin)";

fn config_path() -> PathBuf {
    PathBuf::from(std::env::var("IEMMIXER_CONFIG").unwrap_or_else(|_| "iemmixer.toml".to_string()))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => match run_server() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("iem-server: {e:#}");
                ExitCode::FAILURE
            }
        },
        ["pin", "set-engineer"] => pin_command(PinTarget::Engineer),
        ["pin", "set-member", member] => pin_command(PinTarget::Member((*member).to_string())),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn pin_command(target: PinTarget) -> ExitCode {
    let stdin = std::io::stdin();
    match provision::run(&config_path(), &target, stdin.lock()) {
        Ok(()) => {
            eprintln!("iem-server: stored the {} PIN hash", target.label());
            ExitCode::SUCCESS
        }
        Err(e @ ProvisionError::Invalid(_)) => {
            eprintln!("iem-server: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("iem-server: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_server() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("iem_server=info".parse()?),
        )
        .init();
    tracing::info!("Starting the iemmixer server v{}", iem_core::VERSION);
    let path = config_path();
    let config = Config::load(&path).with_context(|| format!("loading site config {}", path.display()))?;
    tracing::info!(
        path = %path.display(),
        members = config.members.len(),
        inputs = config.inputs.len(),
        "site config loaded"
    );
    let port = match std::env::var("PORT") {
        Ok(p) => p.parse().with_context(|| format!("PORT={p} is not a port number"))?,
        Err(_) => config.port,
    };
    let config_dir = provision::config_dir_of(&path);
    let runtime = tokio::runtime::Runtime::new().context("creating the tokio runtime")?;
    runtime.block_on(iem_server::start_server(ServerConfig { port, config, config_dir }, None))
}
```

- [ ] **Step 4: Lockfile, format, commit**

```bash
set -euo pipefail
cd "$WORK"
cargo metadata --format-version 1 > /dev/null
cargo metadata --locked --format-version 1 > /dev/null
cargo fmt --all
git add crates Cargo.lock
git commit -m "feat(server): iem-server pin set-engineer / set-member (PIN on stdin, hash only)" -m "First engineer PIN at bootstrap; members get PINs by engineer reset. Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected in CI (Task 15): `provision::` unit tests and `tests/pin_cli.rs` (7 tests) pass; the `e2e` job provisions with this binary.

---

### Task 12: E2E — clean-console guard, env PINs, version label, login protection, site links

**Files:**
- Create: `e2e/tests/support/fixtures.ts`, `e2e/tests/version.spec.ts`, `e2e/tests/login-protection.spec.ts`, `e2e/tests/site-links.spec.ts`
- Modify: `e2e/tests/support/pins.ts`, the six imported specs (import line; `REAPER_ABSENT` in the describes the inventory marks), `e2e/tests/smoke.spec.ts` (network-error describe), `crates/iem-core/src/lib.rs` (version label), `crates/iem-ui/src/pages/landing.rs`, `crates/iem-ui/src/pages/mixer/mod.rs`

**Interfaces:**
- Consumes: `/api/version`, `/api/site`, 429 behaviour, CI-provisioned PINs.
- Produces: `./support/fixtures` exports `test` (with auto `consoleGuard` and option `allowedConsole: RegExp[]`), `expect`, type `Page`, `REAPER_ABSENT: RegExp[]` (deleted in S5); `./support/pins` adds `wrongPin(): string`; `iem_core::version_label() == format!("v{VERSION}")`; `data-testid="version"` on every header version label.

- [ ] **Step 1: Version label = `v` + the Cargo version (core + UI)**

`crates/iem-core/src/lib.rs`: replace `full_version` and `version_label` with
```rust
/// Full version string for display, e.g. "2.0.0-dev.3 (24.09.2026 10:45)".
pub fn full_version() -> String {
    let timestamp = build_time().parse::<i64>().unwrap_or(0);
    if timestamp == 0 {
        format!("{VERSION} (local)")
    } else {
        let datetime = chrono::DateTime::from_timestamp(timestamp, 0)
            .map(|dt| dt.format("%d.%m.%Y %H:%M").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        format!("{VERSION} ({datetime})")
    }
}

/// Version label for display: `v` + the Cargo version (pre-releases already
/// carry `-dev.N`), e.g. "v2.0.0-dev.3".
pub fn version_label() -> String {
    format!("v{VERSION}")
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn version_label_is_v_plus_the_cargo_version() {
        assert_eq!(version_label(), format!("v{VERSION}"));
    }

    #[test]
    fn full_version_starts_with_the_cargo_version() {
        assert!(full_version().starts_with(&format!("{VERSION} (")));
    }
}
```
In `crates/iem-ui/src/pages/landing.rs` and in both header spans of `crates/iem-ui/src/pages/mixer/mod.rs`: `<span class="header-version-number">` → `<span class="header-version-number" data-testid="version">`.

- [ ] **Step 2: Support modules**

`e2e/tests/support/fixtures.ts`:
```ts
import { test as base, expect } from "@playwright/test";

type ConsoleGuard = {
  /** Console messages this test deliberately provokes (e.g. a 401 it asks for). */
  allowedConsole: RegExp[];
  consoleGuard: void;
};

/**
 * Every test fails if the browser console shows an error, a warning or a page
 * error that it did not declare in `allowedConsole` (clean-console rule).
 */
export const test = base.extend<ConsoleGuard>({
  allowedConsole: [[], { option: true }],
  consoleGuard: [
    async ({ page, allowedConsole }, use) => {
      const problems: string[] = [];
      page.on("console", (msg) => {
        if (msg.type() !== "error" && msg.type() !== "warning") return;
        const text = msg.text();
        if (allowedConsole.some((pattern) => pattern.test(text))) return;
        problems.push(`[${msg.type()}] ${text}`);
      });
      page.on("pageerror", (error) => problems.push(`[pageerror] ${error.message}`));
      await use();
      expect(problems, "browser console must stay clean").toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };
export type { Page } from "@playwright/test";

/**
 * REAPER is absent in mock E2E until S5 replaces the REAPER control plane
 * (.claude/rules/e2e.md): pages that load REAPER-era mixer state get exactly
 * these failures. Declare per describe with
 * `test.use({ allowedConsole: REAPER_ABSENT })` and the comment
 * `// REAPER absent in mock E2E until S5`. Every entry is an anchored exact
 * message copied from a CI log line; S5 deletes this list.
 */
export const REAPER_ABSENT: RegExp[] = [
  /^Failed to load resource: the server responded with a status of 502 \(Bad Gateway\)$/,
];
```
Append to `e2e/tests/support/pins.ts`:
```ts

/** A 4-digit PIN that is neither the member PIN nor the engineer PIN. */
export function wrongPin(): string {
  for (let offset = 1; offset < 10; offset++) {
    const candidate = String((Number(MEMBER_PIN) + offset) % 10000).padStart(4, "0");
    if (candidate !== ENGINEER_PIN) return candidate;
  }
  throw new Error("two PINs cannot block nine candidates");
}
```

- [ ] **Step 3: Migrate the six imported specs to the guard**

In each of `e2e/tests/{smoke,auth-security,member-photo,auto-redirect,login-keyboard,pwa}.spec.ts`, change the module specifier of the `@playwright/test` import to `"./support/fixtures"` (same imported names). In `smoke.spec.ts`, inside `test.describe("Network Error UX - Issue #56", …)` (after scrub: `reaperiem#56` only in comments; the describe title keeps its text), add as the first statement:
```ts
  // These tests abort /api/members on purpose; Chrome reports the aborted request.
  test.use({ allowedConsole: [/Failed to load resource: net::ERR_CONNECTION_FAILED/] });
```

**REAPER-absent inventory (before the first push).** The guard is new to these specs, and the server runs without REAPER (`/api/mixer/*` answers 502; the imported `auth-security.spec.ts` comments on it). For each spec — the six imported ones and the new `site-links.spec.ts` — list the pages its tests open and the API/WebSocket calls those pages make (read `crates/iem-ui/src/api.rs`, `crates/iem-ui/src/pages/**` and the specs), and mark the calls that reach REAPER-era handlers (`proxy.rs`, `poller.rs`, `preset_routes.rs`, `snapshot_routes.rs`, `backup_*`). Every `describe` whose tests open a page making such a call gets, as its first statement:
```ts
  // REAPER absent in mock E2E until S5
  test.use({ allowedConsole: REAPER_ABSENT });
```
(import `REAPER_ABSENT` from `./support/fixtures`; where a describe already declares its own messages, spread both lists). Record the inventory table (spec → pages → REAPER-era calls → describes marked) in a comment on #2. When the first CI run shows another REAPER-caused message, add it to `REAPER_ABSENT` as an anchored exact pattern copied from the log, only for a REAPER-era endpoint; any other console message is an app bug to fix.
In `auth-security.spec.ts`, rename the test `engineer member rejects default member PIN` to `engineer member rejects a member PIN` and its comment to `// Try to login as "engineer" with a member PIN — must be rejected`.

- [ ] **Step 4: New specs**

`e2e/tests/version.spec.ts`:
```ts
import { test, expect } from "./support/fixtures";

test.describe("Version label (version-on-dashboard)", () => {
  test("landing page shows the backend version as v<semver>", async ({ page }) => {
    const api = await (await page.request.get("/api/version")).json();
    await page.goto("/");
    const label = page.getByTestId("version").first();
    await expect(label).toBeVisible();
    const text = ((await label.textContent()) ?? "").trim();
    expect(text).toMatch(/^v\d+\.\d+\.\d+(-dev\.\d+)?$/);
    expect(text).toBe(`v${api.version}`);
  });
});
```

`e2e/tests/login-protection.spec.ts`:
```ts
import type { APIRequestContext, Response } from "@playwright/test";
import { test, expect } from "./support/fixtures";
import { MEMBER_PIN, wrongPin } from "./support/pins";

// Each test acts as a tunnel client from its own TEST-NET-3 address
// (CF-Connecting-IP from a loopback peer is trusted), so its failures never
// slow other tests; the run stays far below the 30-failure engineer budget.
function tunnelClient(n: number): Record<string, string> {
  return { "CF-Connecting-IP": `203.0.113.${n}` };
}

async function firstMember(request: APIRequestContext): Promise<string> {
  const members = (await (await request.get("/api/members")).json()) as Array<{ id: string }>;
  return members[0].id;
}

test.describe("Login protection (program spec §5.3)", () => {
  test("backoff after three failures, then the right PIN works again (never a lockout)", async ({ request }) => {
    const headers = tunnelClient(11);
    const member = await firstMember(request);
    for (let i = 0; i < 3; i++) {
      const failed = await request.post("/api/auth", { headers, data: { member, pin: wrongPin() } });
      expect(failed.status()).toBe(401);
    }
    const throttled = await request.post("/api/auth", { headers, data: { member, pin: MEMBER_PIN } });
    expect(throttled.status()).toBe(429);
    const retryAfter = Number(throttled.headers()["retry-after"]);
    expect(retryAfter).toBeGreaterThanOrEqual(1);
    expect(retryAfter).toBeLessThanOrEqual(60);
    await new Promise((resolve) => setTimeout(resolve, retryAfter * 1000 + 200));
    const ok = await request.post("/api/auth", { headers, data: { member, pin: MEMBER_PIN } });
    expect(ok.status()).toBe(200);
  });

  test("someone else's failures do not slow another client", async ({ request }) => {
    const member = await firstMember(request);
    for (let i = 0; i < 3; i++) {
      const failed = await request.post("/api/auth", { headers: tunnelClient(12), data: { member, pin: wrongPin() } });
      expect(failed.status()).toBe(401);
    }
    const other = await request.post("/api/auth", { headers: tunnelClient(13), data: { member, pin: MEMBER_PIN } });
    expect(other.status()).toBe(200);
    const lan = await request.post("/api/auth", { data: { member, pin: MEMBER_PIN } });
    expect(lan.status()).toBe(200);
  });

  test.describe("login page", () => {
    // The wrong PINs below are deliberate: Chrome reports each rejected request.
    test.use({ allowedConsole: [/status of 401/, /status of 429/] });

    test("shows the wait time after repeated wrong PINs", async ({ page }) => {
      await page.setExtraHTTPHeaders(tunnelClient(14));
      const member = await firstMember(page.request);
      await page.goto(`/login?member=${member}&next=/${member}`);
      await expect(page.locator(".numpad")).toBeVisible({ timeout: 10000 });
      const wrong = wrongPin();
      const isLogin = (r: Response) => r.url().endsWith("/api/auth") && r.request().method() === "POST";
      // Wait for each attempt's own response (a still-visible "Invalid PIN"
      // would race the next request). Three free failures, then 1, 2, 4 s:
      // the attempt typed right after the third failure is normally throttled;
      // on a slow runner the next one is, so allow up to six attempts.
      const statuses: number[] = [];
      while (!statuses.includes(429) && statuses.length < 6) {
        const response = page.waitForResponse(isLogin);
        for (const digit of wrong) await page.keyboard.press(digit);
        statuses.push((await response).status());
        if (statuses[statuses.length - 1] === 401) {
          await expect(page.getByText("Invalid PIN")).toBeVisible();
          await expect(page.locator(".pin-dot.filled")).toHaveCount(0);
        }
      }
      expect(statuses.slice(0, 3)).toEqual([401, 401, 401]);
      expect(statuses[statuses.length - 1]).toBe(429);
      await expect(page.getByText(/Too many attempts\. Try again in \d+ s/)).toBeVisible();
    });
  });
});
```

`e2e/tests/site-links.spec.ts`:
```ts
import { REAPER_ABSENT, test, expect } from "./support/fixtures";
import { MEMBER_PIN } from "./support/pins";

test.describe("Site links from the site config", () => {
  // REAPER absent in mock E2E until S5 (the mixer page loads REAPER-era state)
  test.use({ allowedConsole: REAPER_ABSENT });

  test("GET /api/site returns the test site's LAN URL and public host", async ({ request }) => {
    const site = await (await request.get("/api/site")).json();
    expect(site).toEqual({ lan_url: "http://10.0.0.10", public_host: "mixer.example.org" });
  });

  test("the member banner points at the configured LAN URL while the tunnel is down", async ({ page }) => {
    // CI runs no cloudflared, so the tunnel watchdog reports Down after its first poll.
    const members = (await (await page.request.get("/api/members")).json()) as Array<{ id: string }>;
    const member = members.find((m) => m.id !== "engineer")!.id;
    const auth = await (await page.request.post("/api/auth", { data: { member, pin: MEMBER_PIN } })).json();
    await page.goto("/");
    await page.evaluate(({ token, member, engineer }) => {
      localStorage.setItem("iem_token", JSON.stringify({ token, member, engineer }));
    }, auth);
    await page.goto(`/${member}`);
    await expect(page.getByTestId("tunnel-banner")).toContainText("http://10.0.0.10", { timeout: 15000 });
  });
});
```

- [ ] **Step 5: Gates and commit**

```bash
set -euo pipefail
cd "$WORK"
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_integrity.py
cargo fmt --all
git add e2e crates
git commit -m "test(e2e): clean-console guard, env PINs, version label, login protection and site-link specs" -m "Refs #2" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
Expected in CI (Task 15): the `e2e` job passes every spec with a clean console. A console message the guard reports is handled per `.claude/rules/e2e.md`: fix the app when it is a bug; declare it with `allowedConsole` only in the `describe` of a test that provokes it on purpose; a message caused by REAPER's absence goes into `REAPER_ABSENT` (exact, from the log) and is declared by the describes the inventory marks — never a broader pattern.

---

### Task 13: Private ops repo skeleton and the CI denylist secret

**Files (private repo `$OPS`, never in the public repo):**
- Create: `README.md`, `CLAUDE.md` (with the S0 event runbook), `site/site.toml`, `security/denylist-terms.txt`, `security/scrub-map.tsv` (without PIN rows), `tools/yaml_to_site_toml.py`, `tools/test_yaml_to_site_toml.py`, `docs/plans/2026-09-24-s0-bootstrap.md` (the full plan), `docs/archive/09-spec-public-draft.md`, `docs/archive/10-site-appendix-private-draft.md`, `docs/archive/12-program-spec-approved.md`, `.gitignore`

**Interfaces:**
- Produces: private repo `zbynekdrlik/iemmixer-ops` (`main` + `dev`); public repo Actions secret `DENYLIST` = `$PRIV/denylist.txt`; ops issues for the PC-only credential and the edge rate-limit rule.

> *Private detail: see the iemmixer-ops copy of this plan (`docs/plans/2026-09-24-s0-bootstrap.md`).*

---

### Task 14: Public repo settings before the first push

**Files:** none (GitHub settings via `gh api`, read back).

**Interfaces:**
- Produces: actions allowlist (GitHub-owned + `Swatinem/rust-cache@*`, `taiki-e/install-action@*`), SHA pinning required, read-only token, no PR approval by workflows, fork-PR approval for all external contributors, wiki off, merge commits only, head branches never auto-deleted (`dev` is the head of every release PR), secret scanning + push protection + non-provider patterns, Dependabot alerts (no Dependabot PRs), private vulnerability reporting.

- [ ] **Step 1: Apply**

```bash
set -euo pipefail
gh api -X PUT "repos/$REPO/actions/permissions" -F enabled=true -f allowed_actions=selected -F sha_pinning_required=true
gh api -X PUT "repos/$REPO/actions/permissions/selected-actions" --input - <<'EOF'
{"github_owned_allowed": true, "verified_allowed": false, "patterns_allowed": ["Swatinem/rust-cache@*", "taiki-e/install-action@*"]}
EOF
gh api -X PUT "repos/$REPO/actions/permissions/workflow" -f default_workflow_permissions=read -F can_approve_pull_request_reviews=false
gh api -X PUT "repos/$REPO/actions/permissions/fork-pr-contributor-approval" -f approval_policy=all_external_contributors
gh api -X PATCH "repos/$REPO" -F has_wiki=false -F allow_merge_commit=true -F allow_squash_merge=false -F allow_rebase_merge=false -F delete_branch_on_merge=false
gh api -X PATCH "repos/$REPO" --input - <<'EOF'
{"security_and_analysis": {"secret_scanning": {"status": "enabled"}, "secret_scanning_push_protection": {"status": "enabled"}, "secret_scanning_non_provider_patterns": {"status": "enabled"}}}
EOF
gh api -X PUT "repos/$REPO/vulnerability-alerts"
gh api -X PUT "repos/$REPO/private-vulnerability-reporting"
```

- [ ] **Step 2: Read back**

```bash
gh api "repos/$REPO/actions/permissions"
gh api "repos/$REPO/actions/permissions/selected-actions"
gh api "repos/$REPO/actions/permissions/workflow"
gh api "repos/$REPO/actions/permissions/fork-pr-contributor-approval"
gh api "repos/$REPO" --jq '{private, has_wiki, allow_merge_commit, allow_squash_merge, allow_rebase_merge, delete_branch_on_merge, security_and_analysis}'
gh api -i "repos/$REPO/vulnerability-alerts" | head -1
gh api "repos/$REPO/private-vulnerability-reporting"
gh api "repos/$REPO/actions/runners" --jq '.total_count'
```
Expected: `allowed_actions: selected`, `sha_pinning_required: true`, the two patterns, `read`, `all_external_contributors`, `has_wiki: false`, only merge commits, `delete_branch_on_merge: false`, the three secret-scanning features `enabled`, `HTTP/2.0 204` for vulnerability alerts, `{"enabled": true}`, `0` self-hosted runners.

---

### Task 15: First push, CI to green, coverage floor, rulesets

**Files:**
- Modify (only as CI demands): any file of Tasks 2–12; `.github/coverage-floor`

**Interfaces:**
- Consumes: everything above.
- Produces: `main` (import) and `dev` on GitHub; a fully green CI run on `dev`; rulesets `main protection` and `dev protection`.

- [ ] **Step 1: Pre-push checks (load the `ci-push-discipline` skill first)**

```bash
set -euo pipefail
cd "$WORK"
[ -z "$(git status --porcelain)" ] || { git status --porcelain >&2; echo "the tree is not clean" >&2; exit 1; }
# An ignored file under a content path would be silently missing from the commits
ignored="$(git ls-files --others --ignored --exclude-standard -- crates scripts e2e/tests config docs)"
[ -z "$ignored" ] || { printf 'ignored but needed?\n%s\n' "$ignored" >&2; exit 1; }
git log --oneline main; git log --oneline main..dev
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts -p 'test_*.py'
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_integrity.py
PYTHONDONTWRITEBYTECODE=1 python3 scripts/check_version.py
cargo fmt --all -- --check
cargo metadata --locked --format-version 1 > /dev/null
```

- [ ] **Step 2: Push `main` and `dev` together (the hook scans both)**

```bash
cd "$WORK" && git push -u origin main dev
```
Expected: the hook prints `denylist: clean` twice (no identity or denylist findings) and gitleaks finds nothing; both branches created. Only `dev` has workflows, so one CI run starts (push to `dev`).

- [ ] **Step 3: Monitor the run to a terminal state (main session; one background waiter, then recover)**

The first run is long (a fat-LTO Tauri release build on Windows, a release server build for E2E), so it is a long wait, never repeated foreground polls (`block-ci-poll-repeat.sh` blocks a second loop for the same run). In the main session — never in a subagent — launch ONE waiter with `run_in_background: true`:
```bash
run_id="$(gh run list -R "$REPO" --branch dev --event push --limit 1 --json databaseId --jq '.[0].databaseId')"
echo "run $run_id"
timeout 10800 bash -c 'while :; do
  s=$(gh run view '"$run_id"' -R '"$REPO"' --json status,conclusion,jobs --jq "if .status==\"completed\" then \"TERMINAL \"+.status+\" \"+(.conclusion//\"\") elif ([.jobs[]?|select(.conclusion==\"failure\" or .conclusion==\"timed_out\")]|length)>0 then \"JOBFAIL \"+([.jobs[]?|select(.conclusion==\"failure\" or .conclusion==\"timed_out\")]|map(.name)|join(\", \")) else \"PENDING \"+.status end" 2>/dev/null) || s="ERROR"
  case "$s" in
    "TERMINAL "*) echo "TERMINAL: ${s#TERMINAL }"; exit 0 ;;
    "JOBFAIL "*) echo "JOB FAILED (run still in progress): ${s#JOBFAIL }"; exit 0 ;;
  esac
  sleep 60
done'
```
It wakes the session once (terminal state, first failed job, or the 3-hour budget). Recovery is not optional: on the next turn re-read the run from GitHub (`gh run view "$run_id" -R "$REPO" --json status,conclusion,jobs --jq '.jobs[] | "\(.name): \(.status) \(.conclusion)"'`) rather than trusting silence, and relaunch one fresh waiter if the old one is gone (compaction or memory pressure can drop it). End each waiting turn with `⏳ WORKING`.

- [ ] **Step 4: Fix loop — one batched commit per cycle**

For every failed job: `gh run view "$run_id" -R "$REPO" --log-failed`, find the root cause (superpowers:systematic-debugging), fix all failures of the cycle in one commit, push, monitor again (Step 3). Expected failure classes after an import that CI never fully covered (Scope decisions), with the only acceptable fixes:
- compile errors in new code or path edits → fix the code;
- **imported tests and lints that never ran before** — the TLS-gated server tests (`--all-features` enables `tls`), the 59 `iem-ui` unit tests, clippy over `--workspace --all-targets --all-features`, clippy for wasm32, the tray on Windows → fix the code; never `allow` a lint without a comment giving the reason; a failing never-run test is investigated like any other finding;
- **fresh-resolved dependencies** (the stale lockfile re-resolved in Task 2) — deprecations, changed APIs, new clippy lints from newer crate versions → fix the call sites, or pin one crate back with `cargo update -p <crate> --precise <version>` (non-compiling) with the reason in the commit message;
- a test comparing member lists or names whose order changed because of the placeholder scrub → adjust that test's expected data only (the commit message says “import-scrub correction”); any other failing imported test is a real finding: investigate;
- `mutants-list` fails (the next PR's mutants need more shards than the matrix has) → extend the `shard:` list in `ci.yml` (0-based, consecutive) to the count it prints; Step 6 derives the required checks from it;
- cargo-deny advisory → `cargo update -p <crate>` (non-compiling) to a fixed version; licence finding → a crate-scoped `[[licenses.exceptions]]` entry only if the licence is acceptable, with a comment naming why; never ignore a vulnerability without a filed issue (Task 5 Step 9 shows the form);
- gitleaks/denylist/identity → fix the content (the hook should have caught it locally);
- Playwright console messages → per `.claude/rules/e2e.md` (an app bug is fixed; a REAPER-absence message goes into `REAPER_ABSENT`, exact).
Never raise a timeout, never mark a test ignored or skipped, never use `continue-on-error`.

- [ ] **Step 5: Set the coverage floor from the first green `test` job**

```bash
job_id="$(gh run view "$run_id" -R "$REPO" --json jobs --jq '.jobs[] | select(.name == "test") | .databaseId')"
gh run view "$run_id" -R "$REPO" --job "$job_id" --log | grep -oE 'line coverage: [0-9.]+%'
```
Write the integer part of that percentage into `.github/coverage-floor` (e.g. `61.87%` → `61`), commit `ci: set the line-coverage floor to the measured baseline` (Refs #2 + attribution), push, and monitor to green again.

- [ ] **Step 6: Rulesets (after the first push, so they did not block it)**

```bash
set -euo pipefail
cd "$WORK"
commas="$(grep -E '^ +shard: \[' .github/workflows/ci.yml | tr -cd ',' | wc -c)"
shards=$((commas + 1))
# Every job a required job needs is required too: a skipped job would count as passing
checks="$(python3 -c 'import json, sys
n = int(sys.argv[1])
jobs = ["integrity", "lint", "test", "wasm", "e2e", "windows", "supply-chain", "secrets", "version", "mutation-warmup"]
print(json.dumps(jobs + [f"mutation shard {k}" for k in range(n)]))' "$shards")"
echo "$checks"
main_json="$(mktemp)"
python3 -c 'import json, sys
checks = json.loads(sys.argv[1])
print(json.dumps({
  "name": "main protection", "target": "branch", "enforcement": "active",
  "conditions": {"ref_name": {"include": ["refs/heads/main"], "exclude": []}},
  "rules": [
    {"type": "deletion"}, {"type": "non_fast_forward"},
    {"type": "pull_request", "parameters": {"required_approving_review_count": 0, "dismiss_stale_reviews_on_push": False,
      "require_code_owner_review": False, "require_last_push_approval": False,
      "required_review_thread_resolution": False, "allowed_merge_methods": ["merge"]}},
    {"type": "required_status_checks", "parameters": {"strict_required_status_checks_policy": False,
      "do_not_enforce_on_create": False,
      "required_status_checks": [{"context": c, "integration_id": 15368} for c in checks]}}],
  "bypass_actors": []}))' "$checks" > "$main_json"
gh api -X POST "repos/$REPO/rulesets" --input "$main_json" --jq '.id'
gh api -X POST "repos/$REPO/rulesets" --input - --jq '.id' <<'EOF'
{"name": "dev protection", "target": "branch", "enforcement": "active",
 "conditions": {"ref_name": {"include": ["refs/heads/dev"], "exclude": []}},
 "rules": [{"type": "deletion"}, {"type": "non_fast_forward"}], "bypass_actors": []}
EOF
gh api "repos/$REPO/rulesets" --jq '.[] | "\(.id) \(.name) \(.enforcement)"'
```
Expected: two active rulesets. (15368 = the GitHub Actions app, so only Actions can satisfy the contexts; `hil/iem-pc` is added in S6 with the ops app as its source.)

---

### Task 16: PR `dev`→`main`, merge, onboarding, ticket

**Files:** none (GitHub + airuleset registry).

**Interfaces:**
- Consumes: green `dev` (Task 15), rulesets, the `DENYLIST` secret, the ops repo (Task 13), the hand-off comments (Task 2 Step 12).
- Produces: `main` = S0 merged with a merge commit at `2.0.0-dev.1`; `dev` fast-forwarded to it; #2 closed; iemmixer and iemmixer-ops in the airuleset registry.

- [ ] **Step 1: Open the PR (load the `pr-merge-policy` skill)**

```bash
body="$(mktemp)"
cat > "$body" <<'EOF'
S0 bootstrap of Gen 2 (program spec §6 S0):

- Fresh import of the predecessor's web app at a pinned SHA (scrubbed, provenance in docs/provenance/import-manifest.txt), MIT OR Apache-2.0.
- Security baseline (spec §3.4 X8–X10, §5.3): no compiled-in credentials, generated runtime secrets, argon2id PIN hashes with a DPAPI-protected pepper, start-up refuses a bad pepper or PIN store, complete login limiter (failure budgets per LAN/tunnel client with IPv6 keyed per /64, engineer budget, never lockout, bounded hashing), PIN provisioning CLI, no PINs in backups, raw REAPER passthrough removed.
- Site config in TOML (example + synthetic test site); hosts from the site config.
- Hosted CI: integrity, lint, tests + coverage floor, WASM, mock E2E with a clean console, Windows build + DPAPI tests, cargo-deny, gitleaks + private denylist + commit-identity check, version gate, diff-scoped mutation (warm-up + SHARDS shards).
- Program spec and S0 plan in docs/superpowers/; project playbook in CLAUDE.md and .claude/rules/.

Closes #2

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
shards=$(( $(grep -E '^ +shard: \[' "$WORK/.github/workflows/ci.yml" | tr -cd ',' | wc -c) + 1 ))
sed -i "s/SHARDS shards/$shards shards/" "$body"
gh pr create -R "$REPO" --base main --head dev --title "S0: fresh import, security baseline, CI skeleton" --body-file "$body"
```

- [ ] **Step 2: Monitor the PR's CI to terminal (every required check of Task 15 Step 6, mutation warm-up and shards included), fix like Task 15 Step 4**

Wait with the Task 15 Step 3 waiter (main session), taking the run id from `gh run list -R "$REPO" --branch dev --event pull_request --limit 1`. Surviving mutants in the diff are unfinished work: add assertions that kill them (never exclude a mutant without a reason comment in `.cargo/mutants.toml`). A shard over 20 minutes is a setup bug: more shards or narrower scope (fallback in `.claude/rules/ci-rust-toolchain.md`), never a longer timeout. When the shards are green, record each shard's duration:
```bash
gh run view "$run_id" -R "$REPO" --json jobs --jq '.jobs[] | select(.name | startswith("mutation")) | "\(.name) \(.startedAt) \(.completedAt)"'
```
Post the durations and the mutant count on #2. If the slowest shard took more than 15 minutes, lower `MUTANTS_PER_SHARD` in the `mutants-list` job (and extend the matrix to match) in the next `dev` commit so the next PR keeps ≥ 25 % headroom; if every shard finished under 8 minutes, it may be raised.

- [ ] **Step 3: Merge when every check is green and the PR is clean**

```bash
pr="$(gh pr list -R "$REPO" --head dev --base main --json number --jq '.[0].number')"
gh pr view "$pr" -R "$REPO" --json mergeable,mergeStateStatus,statusCheckRollup --jq '{mergeable, mergeStateStatus, failing: [.statusCheckRollup[] | select(.conclusion != "SUCCESS") | .name]}'
# The merge commit's author must be the noreply identity (its committer is GitHub: noreply@github.com)
gh pr merge "$pr" -R "$REPO" --merge --author-email 26905282+zbynekdrlik@users.noreply.github.com
gh issue view 2 -R "$REPO" --json state --jq .state    # expect CLOSED
git -C "$WORK" fetch origin
git -C "$WORK" log -1 --format='%ae %ce' origin/main    # expect the noreply address and noreply@github.com
```
Expected: `MERGEABLE`, `CLEAN`, no failing checks; merged with a merge commit whose author and committer are in `scripts/allowed-identities.txt` (the post-merge `secrets` job checks it again).

- [ ] **Step 4: Sync `dev` with `main` and confirm the post-merge run**

```bash
set -euo pipefail
cd "$WORK"
git fetch origin
git switch dev
git merge --ff-only origin/main
git push origin dev
```
Monitor the resulting `push` runs on `main` and `dev` to green (Task 15 Step 3). The next sub-project's first commit on `dev` bumps to `2.0.0-dev.2`.

- [ ] **Step 5: Onboarding (airuleset registry) — dry run first**

```bash
python3 ~/devel/airuleset/airuleset.py onboard-project "$WORK" --dry-run
python3 ~/devel/airuleset/airuleset.py onboard-project "$WORK"
python3 ~/devel/airuleset/airuleset.py onboard-project "$OPS" --dry-run
python3 ~/devel/airuleset/airuleset.py onboard-project "$OPS"
python3 ~/devel/airuleset/airuleset.py onboard-project "$WORK" --audit
```
Expected for iemmixer: git/remote/branches/CLAUDE.md `satisfied`, no foundation ticket (CI and version label exist), registry `applied`. If the gitignore step appended entries, commit them on `dev` (`chore: onboarding .gitignore hygiene`, Refs #2), push, and monitor to green. The ops repo may get a “no CI” foundation ticket — expected until S6 adds `hil.yml`; leave it open and linked to S6.

- [ ] **Step 6: Close out on the ticket**

First add any hand-off item discovered during S0 to its sub-project ticket (a comment, like Task 2 Step 12). Then:
```bash
set -euo pipefail
shards=$(( $(grep -E '^ +shard: \[' "$WORK/.github/workflows/ci.yml" | tr -cd ',' | wc -c) + 1 ))
body="$(mktemp)"
printf '%s\n' "S0 merged (PR #$pr: S0: fresh import, security baseline, CI skeleton). CI green incl. $shards mutation shards; coverage floor $(cat "$WORK/.github/coverage-floor")%; rulesets active; ops repo zbynekdrlik/iemmixer-ops created. Deferred work is on the sub-project tickets (hand-off comments from S0): #3 (S1a: ASIO spike), #5 (S2: DSP a limiter), #6 (S3: jadro enginu), #8 (S5: server a UI na engine protokole, incl. X7), #9 (S6: ASIO backend, guard a HIL, incl. attest/dispatch jobs and the edge rate limit), #10 (S7: plný HIL). Private follow-ups are ops issues (PC-only credential for the denylist; edge rate limit at the tunnel provider)." > "$body"
gh issue comment 2 -R "$REPO" --body-file "$body"
```

---

## Hand-off to later sub-projects (posted on each sub-project's ticket in Task 2 Step 12; each goes into that sub-project's design note)

- **S1a (#3):** the S0 interim event runbook (ops `CLAUDE.md`) is replaced by the interim switch script; add the PC-only credential to the denylist (ops issue from Task 13).
- **S2 (#5):** `deny.toml` `[[licenses.exceptions]]` allowing GPL-3.0-or-later for `iem-limiter-mga` and the engine crate that links it (D1); fuzz and rt-safety checks for the DSP and limiter kernels.
- **S3 (#6):** rt-safety job (`assert_no_alloc` + rtsan, I7); per-PR fuzz job plus the nightly shard; engine dependency allowlist in the supply-chain job (spec §5.2).
- **S5 (#8):** X7 EQ ownership check (in REAPER-coupled code deleted by S5); `mix_view` from the site config replaces the `member1` placeholder; `LoginGuard::stats()` on the engineer page with the band-activity banner; talk lock, handshake; delete `REAPER_ABSENT` from the E2E fixtures; replace the REAPER-era track-name patterns with the spec §3.1 topology.
- **S6 (#9):** tunnel ingress must target `127.0.0.1` (the limiter trusts `CF-Connecting-IP` only from loopback) — add a HIL check; final PC paths for `secrets/` and the pepper — the pepper outside the roaming profile and apart from the PIN hashes (the archived draft proposed `%LOCALAPPDATA%\iemmixer\`); owner-present bootstrap runs `iem-server pin set-engineer`; `hil/iem-pc` required check from the ops GitHub App; the attest job and the dispatch job for `hil.yml` (spec §5.2); the edge rate-limit rule on the login path at the tunnel provider (spec §5.3, ops issue from Task 13); import the Windows cloudflared service setup script from the pinned SHA (scrubbed).
- **S7 (#10):** the predecessor's live elevated spec probes `/api/reaper/NTRACK`, removed in S0 (X10) — port it against the engine protocol.
