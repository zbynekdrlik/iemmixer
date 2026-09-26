---
paths:
  - "crates/iem-rpp/src/read.rs"
  - "crates/iem-rpp/src/legacy.rs"
  - "crates/iem-rpp/src/import.rs"
  - "crates/iem-rpp/src/export.rs"
  - "crates/iem-rpp/src/topology.rs"
  - "crates/iem-rpp/src/aliases.rs"
  - "crates/iem-rpp/src/band.rs"
  - "crates/iem-rpp/src/backup.rs"
  - "crates/iem-rpp/src/sitegen.rs"
  - "crates/iem-migrate/**"
  - "crates/iem-server/src/band_import.rs"
  - "crates/iem-core/src/band.rs"
---

# Migration (S4: importer, exporter, band data)

- **Files only, fail loudly:** `iem-migrate` never talks to REAPER, the predecessor app or the PC. Every unmappable name, key, routing or plug-in is an error listing *all* offenders (`Problems`); `--partial` only downgrades *missing* categories. Design note: `docs/superpowers/specs/2026-09-26-s4-migration-design.md`.
- **Private inputs** live in the ops repo (`site/aliases.toml`, `site/eras.toml`, `tools/eras_from_history.py`, `tools/check_import.sh`); public tests use `sitegen` (synthetic predecessor-shaped projects over `config/test-site.toml` + `synthetic_routing`, `track_name` = `"<ID> trk"`, a group instance `"<MIX>.<GROUP> trk"`). Never put a real track name, member id, channel or PIN in the public repo.
- **Mapping (#20):** armed track → input; track with one `HWOUT` → mix (stereo: ReaEQ + limiter; mono: no plug-ins, imported flat and without limiter); any other track → a group instance, aliased to the **group id**, feeding exactly one mix at unity/centre/unmuted. Receives are levels; a missing receive is an off level. The master must be muted; input faders feed only it and are ignored (noted). Mix and strip pans must be 0. `Routing` records what the project holds; `project`/`compare` look only at that. Aliases: `[members] x = { id, mix, archived }`, no `master` key (S4 files are refused).
- **Round trip:** the exporter patches only mix values into the *original* project and keeps a value's text when the state equals it within 1e-9 dB (`SAME_DB`), so `export(p, import(p)) == p` byte for byte; `export_checked` re-imports before returning. Values the project cannot hold (an audible level without a receive, a mono mix's EQ/limiter, a strip without an instance) are reported in `Exported::dropped`, never written (§4.3). Outputs are created exclusively (`create_new`); the source is never written.
- **Transactional `band` (#20):** everything goes through `stage::Stage` — a staging copy next to the band directory (`.<name>.iem-migrate`), a `.iem-migrate-complete` marker, then two renames; `stage::recover` runs first and rolls a crashed swap forward (complete staging) or back. `band_cmd::run_with` takes a fault hook (tests inject a failure at every `Step`); a dry run only reports leftovers.
- **Caps:** the import runs the state through the engine's `reconcile`; a value moved by more than `CAP_TOLERANCE_DB` (1e-5) fails. REAPER stores +12 dB as 3.981072 (7·10⁻⁷ over the cap) — tolerated, the engine caps it.
- **Eras:** keys are REAPER track numbers (1-based) of the layout at the item's time; between eras both neighbours are tried and must agree or exactly one must map. Customizations use the newest era. Pan in presets/snapshots is the UI's 0…1 (`p·2−1`); snapshot volume is linear, preset volume dB; the backup JSON is linear with −1…1 pan.
- **PIN rule:** `PinStore::import_*_hash` + the additive `imported` list; `set_*_hash` drop the mark (never overwrite an iemmixer-set PIN). Default PINs come from `--legacy-default-pins` (a private file), never from code. Reports never print a PIN, secret or key; `LegacyConfig`, `DefaultPins`, `PinRequest` have redacting `Debug`.
- **Push:** always write the `push_subs_v2_migrated` marker next to `push_subscriptions.json`, or the server's one-time cleanup empties the list.
- **Test PEM keys** are built at runtime (`format!("-----BEGIN {k} KEY-----…", k = "PRIVATE")`): a literal key header trips gitleaks and the local staging hook.
- **Clippy traps seen here:** a test helper returning `&mut` from `(&mut T, &str)` needs a named lifetime (E0106); `needless_borrows_for_generic_args` fires on `std::fs::write(&x, …)` / `rename(&tmp, …)` when `x` is not used afterwards — pass it by value.
- **Real-site check:** `PROJECT_PATH=<path> tools/check_import.sh <dev sha>` (ops) downloads the `iem-migrate-linux-<sha>` artifact of the `migrate-bin` job and dry-runs import (`--expect tracks=45,sends=268,eqs=44,limiters=10,trims=24`) and band on the backup-branch data; it prints counts only.
