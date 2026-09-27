#!/usr/bin/env python3
"""Gen 1 -> gen 2 test parity manifest check (#25; parity report section 7, rule 4).

`docs/parity/gen1-tests.tsv` has one row per test of the predecessor at its
pinned SHA: gen1_file, gen1_test, status, gen2, reason. This check fails when

- the header's `# total: N` differs from the row count, or from the gen 1
  inventory (GEN1_TESTS): the manifest never loses a row;
- a row is malformed, repeated, or has an unknown status;
- an OBSOLETE row has no reason, or a PORTED/TRANSFORMED row cites no gen 2 test;
- a PENDING or FEATURE-GAP row's reason names neither `S7 #10` nor `S6 #9`
  (those wait for the IEM PC or for S6 code and are allowed until cutover);
  with --cutover (S8 #11) no PENDING or FEATURE-GAP row is allowed at all;
- a cited gen 2 test does not exist: `crates/**.rs::fn` must be a test fn
  (under a #[test]-style attribute) in that file, `e2e/tests/**.ts::title` a
  `test(...)` title spelled exactly so in that spec, `scripts/**.py::name` a
  `def test_...` in that file.

gen2 holds `path::test` refs separated by `; `. A Playwright title may itself
contain `; `, so a ref starts only where a known path followed by `::` starts.
"""
from __future__ import annotations

import argparse
import re
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = Path("docs/parity/gen1-tests.tsv")
GEN1_TESTS = 908  # 543 Rust + 276 Playwright + 89 Python at the predecessor's pinned SHA
COLUMNS = ("gen1_file", "gen1_test", "status", "gen2", "reason")
STATUSES = ("PORTED", "TRANSFORMED", "OBSOLETE", "PENDING", "FEATURE-GAP")
COVERED = {"PORTED", "TRANSFORMED"}
OPEN = {"PENDING", "FEATURE-GAP"}
WAITING = {"S7 #10": re.compile(r"(?<!\w)S7 #10(?!\d)"), "S6 #9": re.compile(r"(?<!\w)S6 #9(?!\d)")}
TOTAL = re.compile(r"^#\s*total:\s*(\d+)\s*$")
REF_PATH = r"(?:crates/[^\s;:]+\.rs|e2e/tests/[^\s;:]+\.ts|scripts/[^\s;:]+\.py)"
REF_SPLIT = re.compile(r";\s*(?=" + REF_PATH + "::)")
REF = re.compile(r"^(" + REF_PATH + r")::(.+)$")
RUST_ATTR = re.compile(r"^\s*#\[([^\]]*)\]\s*$")
RUST_FN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
PW_TEST = re.compile(r"""(?<![\w.$])test\(\s*(["'`])((?:\\.|(?!\1).)*)\1""", re.S)
PY_TEST = re.compile(r"^\s*(?:async\s+)?def\s+(test_[A-Za-z0-9_]*)\s*\(", re.M)


@dataclass(frozen=True)
class Row:
    line: int
    gen1_file: str
    gen1_test: str
    status: str
    gen2: tuple[str, ...]
    reason: str


def split_refs(field: str) -> tuple[str, ...]:
    field = field.strip()
    return tuple(part.strip() for part in REF_SPLIT.split(field)) if field else ()


def parse(text: str) -> tuple[int | None, list[Row], list[str]]:
    """The declared total, the rows, and the format errors."""
    declared: int | None = None
    rows: list[Row] = []
    errors: list[str] = []
    header_seen = False
    for n, raw in enumerate(text.splitlines(), start=1):
        if raw.startswith("#"):
            m = TOTAL.match(raw)
            if m:
                if declared is not None:
                    errors.append(f"line {n}: a second '# total:' line")
                declared = int(m.group(1))
            continue
        if not raw.strip():
            continue
        fields = raw.split("\t")
        if not header_seen:
            header_seen = True
            if tuple(fields) != COLUMNS:
                errors.append(f"line {n}: the column header must be {' / '.join(COLUMNS)}")
            continue
        if len(fields) != len(COLUMNS):
            errors.append(f"line {n}: {len(fields)} fields, expected {len(COLUMNS)}")
            continue
        gen1_file, gen1_test, status, gen2, reason = (f.strip() for f in fields)
        rows.append(Row(n, gen1_file, gen1_test, status, split_refs(gen2), reason))
    if declared is None:
        errors.append("no '# total: N' line")
    if not header_seen:
        errors.append("no column header line")
    return declared, rows, errors


class Gen2Tests:
    """Test names per gen 2 file, read on first use."""

    def __init__(self, root: Path) -> None:
        self.root = root
        self.cache: dict[str, set[str] | None] = {}

    def names(self, rel: str) -> set[str] | None:
        if rel not in self.cache:
            path = self.root / rel
            if ".." in Path(rel).parts or not path.is_file():
                self.cache[rel] = None
            else:
                text = path.read_text(encoding="utf-8", errors="replace")
                if rel.endswith(".rs"):
                    self.cache[rel] = rust_tests(text)
                elif rel.endswith(".ts"):
                    self.cache[rel] = playwright_titles(text)
                else:
                    self.cache[rel] = set(PY_TEST.findall(text))
        return self.cache[rel]


def rust_tests(text: str) -> set[str]:
    """Functions directly under an attribute run of which one is a test attribute."""
    found: set[str] = set()
    is_test = False
    for line in text.splitlines():
        attr = RUST_ATTR.match(line)
        if attr:
            # `test`, `tokio::test(..)`, `tracing_test::traced_test`; never `cfg(test)`
            if re.search(r"(?:^|::)(?:test|traced_test)\b", attr.group(1).strip()):
                is_test = True
            continue
        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            continue
        fn = RUST_FN.match(line)
        if fn and is_test:
            found.add(fn.group(1))
        is_test = False
    return found


def playwright_titles(text: str) -> set[str]:
    return {re.sub(r"\\(.)", r"\1", m.group(2)) for m in PW_TEST.finditer(text)}


def check(rows: list[Row], declared: int | None, root: Path, cutover: bool = False,
          expected: int = GEN1_TESTS) -> tuple[list[str], Counter[str]]:
    """Errors, and the count of rows allowed until cutover per waiting reason."""
    errors: list[str] = []
    waiting: Counter[str] = Counter()
    if declared is not None and declared != len(rows):
        errors.append(f"the header declares {declared} rows, the manifest has {len(rows)}")
    if declared is not None and declared != expected:
        errors.append(f"the header declares {declared} rows, the gen 1 inventory has {expected}")
    seen: dict[tuple[str, str], int] = {}
    tests = Gen2Tests(root)
    for row in rows:
        where = f"line {row.line} ({row.gen1_file}::{row.gen1_test})"
        key = (row.gen1_file, row.gen1_test)
        if not row.gen1_file or not row.gen1_test:
            errors.append(f"{where}: gen1_file and gen1_test are required")
        if key in seen:
            errors.append(f"{where}: repeats line {seen[key]}")
        seen.setdefault(key, row.line)
        if row.status not in STATUSES:
            errors.append(f"{where}: unknown status {row.status!r}")
            continue
        if row.status == "OBSOLETE" and not row.reason:
            errors.append(f"{where}: OBSOLETE needs a reason")
        if row.status in COVERED and not row.gen2:
            errors.append(f"{where}: {row.status} needs a gen2 test")
        if row.status in OPEN:
            names = [name for name, pattern in WAITING.items() if pattern.search(row.reason)]
            if cutover:
                errors.append(f"{where}: {row.status} at cutover")
            elif names:
                waiting[names[0]] += 1
            else:
                errors.append(f"{where}: {row.status} without 'S7 #10' or 'S6 #9' in its reason")
        for ref in row.gen2:
            m = REF.match(ref)
            if not m:
                errors.append(f"{where}: {ref!r} is not crates/..rs::fn, e2e/tests/..ts::title or scripts/..py::test")
                continue
            path, name = m.group(1), m.group(2)
            names_in_file = tests.names(path)
            if names_in_file is None:
                errors.append(f"{where}: {path} does not exist")
            elif name not in names_in_file:
                errors.append(f"{where}: no test {name!r} in {path}")
    return errors, waiting


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--cutover", action="store_true", help="S8 #11: no PENDING or FEATURE-GAP row is allowed")
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    manifest = args.root / MANIFEST
    if not manifest.is_file():
        print(f"::error::{MANIFEST} is missing")
        return 1
    declared, rows, errors = parse(manifest.read_text(encoding="utf-8"))
    more, waiting = check(rows, declared, args.root, cutover=args.cutover)
    errors += more
    for error in errors:
        print(f"::error::{MANIFEST}: {error}")
    counts = Counter(row.status for row in rows)
    summary = ", ".join(f"{status} {counts[status]}" for status in STATUSES)
    if errors:
        print(f"parity manifest: {len(errors)} problem(s); {len(rows)} rows ({summary})")
        return 1
    allowed = "; ".join(f"{name}: {count}" for name, count in sorted(waiting.items())) or "none"
    print(f"parity manifest: {len(rows)} rows ({summary}); allowed until cutover: {sum(waiting.values())} ({allowed})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
