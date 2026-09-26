#!/usr/bin/env python3
"""Engine dependency allowlist (program spec §5.2, S3 design note §2).

Every crate the `iem-engine` binary is built from — its normal and build
dependencies, transitively, on the Windows and Linux targets — must be named in
`scripts/engine-deps-allow.txt`. A new crate in the engine needs a reviewed
line there; a crate that left the closure must be removed. Reads the lockfile
through `cargo metadata --locked` (no compilation).
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ALLOW = ROOT / "scripts" / "engine-deps-allow.txt"
TARGETS = ("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu")
ROOT_CRATE = "iem-engine"


def closure(metadata: dict, root: str = ROOT_CRATE) -> set[str]:
    """Names of the crates `root` depends on (normal and build edges), itself excluded."""
    names = {p["id"]: p["name"] for p in metadata["packages"]}
    nodes = {n["id"]: n for n in metadata["resolve"]["nodes"]}
    start = next(pid for pid, name in names.items() if name == root)
    seen: set[str] = set()
    stack = [start]
    while stack:
        for dep in nodes[stack.pop()]["deps"]:
            kinds = {k.get("kind") for k in dep.get("dep_kinds", [])}
            if kinds & {None, "normal", "build"} and dep["pkg"] not in seen:
                seen.add(dep["pkg"])
                stack.append(dep["pkg"])
    return {names[pid] for pid in seen}


def read_allowlist(path: Path) -> set[str]:
    out = set()
    for line in path.read_text().splitlines():
        entry = line.split("#", 1)[0].strip()
        if entry:
            out.add(entry)
    return out


def metadata(target: str) -> dict:
    cmd = ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target]
    return json.loads(subprocess.run(cmd, cwd=ROOT, check=True, capture_output=True, text=True).stdout)


def compare(actual: set[str], allowed: set[str]) -> list[str]:
    errors = [f"{name}: in the engine's dependency closure but not allowlisted" for name in sorted(actual - allowed)]
    errors += [f"{name}: allowlisted but no longer a dependency of the engine" for name in sorted(allowed - actual)]
    return errors


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--print", action="store_true", help="print the current closure and exit")
    args = parser.parse_args(argv)
    actual: set[str] = set()
    for target in TARGETS:
        actual |= closure(metadata(target))
    if args.print:
        print("\n".join(sorted(actual)))
        return 0
    errors = compare(actual, read_allowlist(ALLOW))
    for e in errors:
        print(f"::error::{e}")
    if errors:
        print(f"engine dependencies: {len(errors)} difference(s) against {ALLOW.relative_to(ROOT)}")
        return 1
    print(f"engine dependencies: {len(actual)} crates, all allowlisted")
    return 0


if __name__ == "__main__":
    sys.exit(main())
