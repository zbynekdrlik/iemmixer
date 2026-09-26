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
CODE_SUFFIXES = (".rs", ".ts", ".js", ".py", ".sh", ".ps1", ".psm1", ".yml", ".yaml", ".toml")


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
