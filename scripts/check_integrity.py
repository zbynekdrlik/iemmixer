#!/usr/bin/env python3
"""Integrity gate: no ignored/skipped/focused tests, no continue-on-error,
self-hosted runners or pull_request_target, every action pinned to a full
commit SHA with its version comment, no force-kill verb anywhere, comments
included (program spec I8), job breakaway only in iem-win's spawn glue (S6),
and no ASIO rate, clock or control-panel call (I2)."""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SELF = {"scripts/check_integrity.py", "scripts/test_check_integrity.py"}
RUST_IGNORE = re.compile(r"#\[\s*ignore")
E2E_SKIP = re.compile(r"\b(?:test|it|describe)(?:\.describe)?\.(?:skip|only|fixme)\s*\(|\btest\.fail\s*\(|function assume\(")
WORKFLOW_FORBIDDEN = re.compile(r"continue-on-error|self-hosted|pull_request_target")
USES = re.compile(r"^\s*-?\s*uses:\s*(\S+)(.*)$")
PINNED = re.compile(r"^[^@\s]+@[0-9a-f]{40}$")
# The pin's release, e.g. `# v7.0.1` (ci-rust-toolchain.md).
VERSION_COMMENT = re.compile(r"^\s+#\s*v\d+(?:\.\d+)*\s*$")
# Force-end verbs (I8): Windows' own (taskkill, tskill, Sysinternals pskill),
# PowerShell's, WMI/CIM's Terminate (-MethodName or its alias -Name; wmic's
# call terminate and delete), a job whose closing ends its processes, and the
# Rust/tokio/Python/.NET process handles' kill methods (called, or named in
# ForEach-Object). A request plus a bounded wait is the only stop.
FORCE_KILL = re.compile(
    r"(?i)\btaskkill\b|\btskill\b|\bpskill\b|terminateprocess|terminatejobobject|kill_on_job_close|stop-process"
    r"|\bshutdown(?:\.exe)?\s+/f\b|\.kill\s*\(|\bstart_kill\b|\bkill_on_drop\b|\.terminate\s*\("
    r"|-(?:method)?name\s+['\"]?terminate\b|\bwmic\b.*\b(?:call\s+terminate|delete)\b"
    r"|(?:\bforeach-object|%)\s+(?:-membername\s+)?['\"]?kill\b")
# Children leave the guard's job only through iem-win's spawn glue (S6 design note §5.1).
BREAKAWAY = re.compile(r"CREATE_BREAKAWAY_FROM_JOB")
BREAKAWAY_HOME = "crates/iem-win/"
# Method calls, UFCS paths (`Driver::set_sample_rate(&d, ..)`) and turbofish
# (`future::<T>(..)`) on azo's safe or raw interface (`as_raw().control_panel()`).
ASIO_SETTINGS = re.compile(
    r"(?:\.|::)\s*(?:set_sample_rate|set_clock_source|open_control_panel|control_panel|future)\s*(?:::\s*<[^>]*>\s*)?\(")
CODE_SUFFIXES = (".rs", ".ts", ".js", ".py", ".sh", ".ps1", ".psm1", ".psd1", ".cmd", ".bat", ".yml", ".yaml", ".toml")


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
        found += [f"{rel}:{n}: the host never changes the card's rate, clock or panel (program spec I2)" for n, line in lines(path) if ASIO_SETTINGS.search(line)]
    for path in files(root, "e2e", (".ts",)):
        rel = path.relative_to(root).as_posix()
        found += [f"{rel}:{n}: skipped or focused E2E test" for n, line in lines(path) if E2E_SKIP.search(line)]
    for path in files(root, ".github/workflows", (".yml", ".yaml")):
        rel = path.relative_to(root).as_posix()
        for n, line in lines(path):
            if WORKFLOW_FORBIDDEN.search(line):
                found.append(f"{rel}:{n}: forbidden workflow construct")
            match = USES.match(line)
            if match and not match.group(1).startswith("./"):
                if not PINNED.match(match.group(1)):
                    found.append(f"{rel}:{n}: action not pinned to a full commit SHA: {match.group(1)}")
                elif not VERSION_COMMENT.match(match.group(2)):
                    found.append(f"{rel}:{n}: pinned action without its version comment (# vX.Y.Z): {match.group(1)}")
    for base in ("crates", "e2e", "scripts", ".github"):
        for path in files(root, base, CODE_SUFFIXES):
            rel = path.relative_to(root).as_posix()
            if rel in SELF:
                continue
            for n, line in lines(path):
                if FORCE_KILL.search(line):
                    found.append(f"{rel}:{n}: force-kill command (program spec I8)")
                if BREAKAWAY.search(line) and not rel.startswith(BREAKAWAY_HOME):
                    found.append(f"{rel}:{n}: job breakaway outside iem-win (S6 design note §5.1)")
    goldens = root / "goldens"
    if goldens.is_dir():
        total = sum(p.stat().st_size for p in goldens.rglob("*") if p.is_file())
        if total > 20 * 1024 * 1024:
            found.append(f"goldens/: {total} bytes, over the 20 MB budget (spec §3.5)")
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
