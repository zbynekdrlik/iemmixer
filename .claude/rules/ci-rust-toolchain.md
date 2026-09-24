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
