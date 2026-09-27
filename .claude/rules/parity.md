---
paths:
  - "docs/parity/**"
  - "scripts/check_parity_manifest.py"
  - "scripts/test_check_parity_manifest.py"
---

# Gen 1 test parity manifest (#25; parity report section 7, rule 4)

- `docs/parity/gen1-tests.tsv` has one row per gen 1 test (908, the predecessor at `03be5b97`): `gen1_file`, `gen1_test`, `status`, `gen2`, `reason`. `scripts/check_parity_manifest.py` runs in the CI integrity job.
- **A removed or renamed gen 2 test updates the manifest in the same commit.** Every `gen2` ref is checked: a Rust ref must be a test fn (under `#[test]`, `#[tokio::test]` or `traced_test`) in the file it cites, a Playwright ref a `test(...)` title spelled exactly so in that spec (describe titles do not count), a Python ref a `def test_...` in that file.
- **The manifest never loses a row.** The header's `# total:` must equal the row count and the pinned gen 1 inventory (`GEN1_TESTS` = 908). A gen 1 test that no longer matters becomes OBSOLETE with a one-line reason; its row stays.
- `gen2` holds `path::test` refs separated by `; `. A new ref starts only at `crates/...rs::`, `e2e/tests/...ts::` or `scripts/...py::`, so a title may contain `; ` or `::`.
- PENDING or FEATURE-GAP is allowed only while its reason names `S7 #10` (the IEM PC run) or `S6 #9` (F30 install-site); the check prints how many remain. S8 (#11) runs `python3 scripts/check_parity_manifest.py --cutover`, which allows none.
- Gen 1 names are scrubbed (P6) with `scripts/scrub_import.py`'s private map; the map lacks the one tech-input name the S0 plan keeps out, which is written as `content`. Run the denylist tree scan after any edit.
