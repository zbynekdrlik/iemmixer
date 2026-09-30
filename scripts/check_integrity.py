#!/usr/bin/env python3
"""Integrity gate: no ignored/skipped/focused tests, no continue-on-error,
self-hosted runners or pull_request_target, every action pinned to a full
commit SHA with its version comment, no force-kill verb anywhere, comments
included (program spec I8), job breakaway only in iem-win's spawn glue (S6),
no ASIO rate, clock or control-panel call (I2), and the asio-spike bundle's
Copy-Item list equal to spike_window.BUNDLE_FILES."""
from __future__ import annotations

import ast
import re
import sys
from pathlib import Path, PurePosixPath

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
# ForEach-Object). A request plus a bounded wait is the only stop. A forced
# restart ends every process too: see forced_restart.
FORCE_KILL = re.compile(
    r"(?i)\btaskkill\b|\btskill\b|\bpskill\b|terminateprocess|terminatejobobject|kill_on_job_close|stop-process"
    r"|\.kill\s*\(|\bstart_kill\b|\bkill_on_drop\b|\.terminate\s*\("
    r"|-(?:method)?name\s+['\"]?terminate\b|\bwmic\b.*\b(?:call\s+terminate|delete)\b"
    r"|(?:\bforeach-object|%)\s+(?:-membername\s+)?['\"]?kill\b")
# Forced restarts and shutdowns (#32 B1, review m6/m7). A command's arguments
# end at the next command separator, so another command's -f, -t or -Force on
# the same line is not read as the restart's.
SEPARATOR = re.compile(r"[;|&\n]")
SHUTDOWN_CMD = re.compile(r"(?i)\bshutdown(?:\.exe)?\b(?!\s*\()")
# A switch of shutdown.exe (/r, -t, "/f", '/t','0'), with the number after it.
SWITCH = re.compile(r"(?i)(?<![\w/\-])[/-]([a-z?]{1,2})(?![a-z0-9_])(?:[\s:\"',]+(\d+))?")
COMPUTER_CMD = re.compile(r"(?i)\b(?:restart|stop)-computer\b")
# -Force and the abbreviations PowerShell accepts for it (-For is another parameter).
FORCE_PARAM = re.compile(r"(?i)(?<![\w-])-(?:f|fo|forc|force)(?![\w-])")
WIN32_SHUTDOWN = re.compile(r"(?i)\bwin32shutdown(tracker)?\b\s*(?:\(([^)]*)\))?")
FLAGS_ARG = re.compile(r"(?i)\bflags\s*=\s*(0x[0-9a-f]+|\d+)")
EXIT_WINDOWS = re.compile(r"(?i)\bexitwindowsex\s*\(\s*(0x[0-9a-f]+|\d+)\s*,")
FORCE_TOKENS = re.compile(r"(?i)\bEWX_FORCE(?:IFHUNG)?\b|\bSHUTDOWN_FORCE_(?:OTHERS|SELF)\b")


def forced_restart(line: str) -> bool:
    """A restart or shutdown that force-ends processes (I8). shutdown.exe
    passes only with an explicit /t 0 and no /f (Microsoft: "If the timeout
    period is greater than 0, the /f parameter is implied", and the default
    is 30), in any form (a command line, a quoted path, an argv array,
    -ArgumentList); "shutdown" without a switch of its own is prose or a
    method, /a (abort) is harmless. Restart-/Stop-Computer never with -Force
    or its abbreviations; WMI Win32Shutdown(Tracker) never with the force bit
    (4), and a flags value that cannot be read counts as forced; ExitWindowsEx
    never with EWX_FORCE(IFHUNG) (0x4, 0x10); InitiateShutdown never with
    SHUTDOWN_FORCE_OTHERS/SELF."""
    if FORCE_TOKENS.search(line):
        return True
    for m in SHUTDOWN_CMD.finditer(line):
        switches = SWITCH.findall(SEPARATOR.split(line[m.end():], 1)[0])
        names = {s.lower() for s, _ in switches}
        if not switches or names <= {"a", "?"}:
            continue
        if "f" in names or not any(s.lower() == "t" and v and int(v) == 0 for s, v in switches):
            return True
    for m in COMPUTER_CMD.finditer(line):
        if FORCE_PARAM.search(SEPARATOR.split(line[m.end():], 1)[0]):
            return True
    for m in WIN32_SHUTDOWN.finditer(line):
        values = [v.strip() for v in (m.group(2) or "").split(",") if v.strip()]
        flags = values[-1 if m.group(1) else 0] if values else None
        if flags is None and (a := FLAGS_ARG.search(line)):
            flags = a.group(1)
        if flags is None or not re.fullmatch(r"(?i)0x[0-9a-f]+|\d+", flags) or int(flags, 0) & 4:
            return True
    for m in EXIT_WINDOWS.finditer(line):
        if int(m.group(1), 0) & 0x14:
            return True
    return False


# Children leave the guard's job only through iem-win's spawn glue (S6 design note §5.1).
BREAKAWAY = re.compile(r"CREATE_BREAKAWAY_FROM_JOB")
BREAKAWAY_HOME = "crates/iem-win/"
# Method calls, UFCS paths (`Driver::set_sample_rate(&d, ..)`) and turbofish
# (`future::<T>(..)`) on azo's safe or raw interface (`as_raw().control_panel()`).
ASIO_SETTINGS = re.compile(
    r"(?:\.|::)\s*(?:set_sample_rate|set_clock_source|open_control_panel|control_panel|future)\s*(?:::\s*<[^>]*>\s*)?\(")
CODE_SUFFIXES = (".rs", ".ts", ".js", ".py", ".sh", ".ps1", ".psm1", ".psd1", ".cmd", ".bat", ".yml", ".yaml", ".toml")
# The one spike bundle that reaches the PC: fetch-bundle accepts exactly
# spike_window.BUNDLE_FILES, CI's asio-spike "Bundle" step copies its own list.
BUNDLE_SOURCE = "scripts/asio-spike/spike_window.py"
BUNDLE_WORKFLOW = ".github/workflows/ci.yml"
BUNDLE_JOB = "asio-spike"
JOB_KEY = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")
STEP_NAME = re.compile(r"^\s*-\s*name:\s*(.+?)\s*$")
COPY_ITEM = re.compile(r"\bCopy-Item\s+-LiteralPath\s+(.+?)\s+-Destination\b")


def files(root: Path, base: str, suffixes: tuple[str, ...]) -> list[Path]:
    top = root / base
    if not top.is_dir():
        return []
    return sorted(p for p in top.rglob("*") if p.is_file() and p.suffix in suffixes and "node_modules" not in p.parts)


def lines(path: Path) -> list[tuple[int, str]]:
    return list(enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), start=1))


def bundle_files(path: Path) -> list[str] | None:
    """spike_window.BUNDLE_FILES, read without importing the module."""
    for node in ast.parse(path.read_text(encoding="utf-8")).body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "BUNDLE_FILES" for t in node.targets):
            return sorted(ast.literal_eval(node.value))
    return None


def bundle_copies(path: Path) -> tuple[int, list[str]]:
    """The file names the asio-spike job's Bundle step copies, and the line of
    its first Copy-Item (0: no such step)."""
    job = step = None
    first, names = 0, []
    for n, line in lines(path):
        if match := JOB_KEY.match(line):
            job, step = match.group(1), None
            continue
        if match := STEP_NAME.match(line):
            step = match.group(1)
        if job == BUNDLE_JOB and step and step.startswith("Bundle") and (match := COPY_ITEM.search(line)):
            first = first or n
            names += [PurePosixPath(p.strip().strip("'\"").replace("\\", "/")).name for p in match.group(1).split(",")]
    return first, sorted(names)


def bundle_violations(root: Path) -> list[str]:
    """A drift between the two lists keeps CI green while fetch-bundle rejects
    every artifact on the dev box (#32 E5)."""
    source = root / BUNDLE_SOURCE
    if not source.is_file():
        return []
    want = bundle_files(source)
    if want is None:
        return [f"{BUNDLE_SOURCE}: no BUNDLE_FILES"]
    workflow = root / BUNDLE_WORKFLOW
    line, got = bundle_copies(workflow) if workflow.is_file() else (0, [])
    if not line:
        return [f"{BUNDLE_WORKFLOW}: no Copy-Item in the {BUNDLE_JOB} job's Bundle step (it must copy spike_window.BUNDLE_FILES)"]
    if got != want:
        return [f"{BUNDLE_WORKFLOW}:{line}: the {BUNDLE_JOB} Bundle step copies {got}, spike_window.BUNDLE_FILES lists {want} "
                "(fetch-bundle would reject every artifact)"]
    return []


def violations(root: Path) -> list[str]:
    found: list[str] = bundle_violations(root)
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
                if FORCE_KILL.search(line) or forced_restart(line):
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
