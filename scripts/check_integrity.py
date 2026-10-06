#!/usr/bin/env python3
"""Integrity gate: no ignored/skipped/focused tests, no continue-on-error,
self-hosted runners or pull_request_target, every action pinned to a full
commit SHA with its version comment, no force-kill verb anywhere, comments
included (program spec I8), no restart but the one marked literal form (I8),
job breakaway only in iem-win's spawn glue (S6),
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
# restart ends every process too: see restart_lines.
FORCE_KILL = re.compile(
    r"(?i)\btaskkill\b|\btskill\b|\bpskill\b|terminateprocess|terminatejobobject|kill_on_job_close|stop-process"
    r"|\.kill\s*\(|\bstart_kill\b|\bkill_on_drop\b|\.terminate\s*\("
    r"|-(?:method)?name\s+['\"]?terminate\b|\bwmic\b.*\b(?:call\s+terminate|delete)\b"
    r"|(?:\bforeach-object|%)\s+(?:-membername\s+)?['\"]?kill\b")
# Restarts and shutdowns (#32 B1, review m6/m7, F2 round 3 m9 and its decision
# 3). Reading a restart's arguments failed again and again (non-literal flags, a
# constant OR-ed with a number, a program named in one statement and its flags
# in the next, an argument list over several lines), so the rule is simple:
# every restart mechanism is a violation unless its line carries RESTART_MARKER
# and the mechanism is in the one literally safe form, RESTART_SAFE. The one
# legitimate call is tuning_window.REBOOT_REQUEST. Another graceful form (an
# API call with literal flags, say) needs its own safe form and test first.
RESTART_MARKER = "iemmixer:graceful-restart"
RESTART_MECHANISMS = (
    re.compile(r"(?i)\bshutdown\.exe\b"),                                       # the program, in any form
    re.compile(r"(?i)(?<![\w.$-])shutdown\s+[/-](?!(?:a|\?)(?!\w))[a-z?]"),      # a command line with a switch (/a aborts)
    re.compile(r"(?i)(?:^|[;|&{(`\"'])\s*shutdown\s+[$@`]"),                    # a command whose arguments are variables
    re.compile(r"(?i)(?:&|\bstart-process\b|-filepath\b)\s*['\"]?shutdown(?:\.exe)?['\"]?(?![\w.(])"),
    re.compile(r"(?i)\bcommand::new\s*\(\s*r?#*\"shutdown"),                   # Rust's process API
    re.compile(r"(?i)\b(?:system|popen|exec\w*|spawn\w*|run|call|check_call|check_output|start|processstartinfo)"
               r"\s*\(\s*\[?\s*[rbuf]?[\"'`]shutdown\b"),                       # Python, JS, .NET process APIs
    re.compile(r"(?i)\bfilename\s*=\s*[\"']shutdown\b"),                       # .NET ProcessStartInfo
    re.compile(r"(?i)\[\s*[rbuf]?[\"']shutdown(?:\.exe)?[\"']"),                # an argv starting with it
    re.compile(r"(?i)\b(?:restart|stop)-computer\b"),
    re.compile(r"(?i)\bwin32shutdown(?:tracker)?\b|\.reboot\s*\(|-methodname\s+['\"]?(?:reboot|shutdown)\b"
               r"|\bwmic\b.*\bcall\s+(?:reboot|shutdown|win32shutdown)\b"),
    re.compile(r"(?i)\binitiate(?:system)?shutdown(?:ex)?[aw]?\b|\bexitwindows(?:ex)?\b|\b(?:nt|zw)shutdownsystem\b"),
    re.compile(r"\bEWX_[A-Z_]+\b|\bSHUTDOWN_(?:FORCE_OTHERS|FORCE_SELF|GRACE_OVERRIDE|HYBRID|INSTALL_UPDATES|NOREBOOT"
               r"|POWEROFF|RESTART|RESTARTAPPS|SKIP_SVC_PRESHUTDOWN|SOFT_REBOOT)\b"),
)
# An argv whose program and first switch sit on two lines (a formatter puts
# each list element on a line of its own).
RESTART_ARGV = re.compile(r"(?i)[\"']shutdown(?:\.exe)?[\"']\s*,\s*[rbuf]?[\"'][/-]")
# The literally safe form: an immediate (/t 0, never a delay: Microsoft, "If
# the timeout period is greater than 0, the /f parameter is implied"), planned
# restart without /f, as a whole PowerShell command: literal switches only,
# the reason literal, the comment single-quoted (no expansion), and the command
# ended right there (a `;`, the end of the string holding it, a comment or the
# line's end).
RESTART_SAFE = re.compile(r"(?i)&\s*shutdown\.exe /r /t 0(?: /d [pu]:\d{1,3}:\d{1,5})?(?: /c '[^'$`\"\\;|&\r\n]*')?"
                          r"(?=\s*(?:[;\"#]|$))")
RESTART = "restart without the graceful-restart marker and its literal safe form (program spec I8)"


def restart_lines(rows: list[tuple[int, str]]) -> set[int]:
    """The lines holding a restart that is not allowed: any mechanism, unless
    the line carries RESTART_MARKER and every mechanism on it lies inside a
    RESTART_SAFE command."""
    found: set[int] = set()
    for i, (n, text) in enumerate(rows):
        pair = text + "\n" + (rows[i + 1][1] if i + 1 < len(rows) else "")
        starts = [m.start() for rx in RESTART_MECHANISMS for m in rx.finditer(text)]
        starts += [m.start() for m in RESTART_ARGV.finditer(pair) if m.start() < len(text)]
        if not starts:
            continue
        safe = [m.span() for m in RESTART_SAFE.finditer(text)] if RESTART_MARKER in text else []
        if any(not any(a <= s < b for a, b in safe) for s in starts):
            found.add(n)
    return found


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


def bundle_violations(root: Path, required: bool = False) -> list[str]:
    """A drift between the two lists keeps CI green while fetch-bundle rejects
    every artifact on the dev box (#32 E5). A tree without spike_window.py
    has nothing to compare, unless `required` (the repository itself)."""
    source = root / BUNDLE_SOURCE
    if not source.is_file():
        return [f"{BUNDLE_SOURCE}: missing (the spike bundle's file list)"] if required else []
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


def violations(root: Path, repository: bool = False) -> list[str]:
    """Everything the gate refuses under `root`; `repository`: root is this
    repository, whose required files must exist."""
    found: list[str] = bundle_violations(root, required=repository)
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
            rows = lines(path)
            restarts = restart_lines(rows)
            for n, line in rows:
                if FORCE_KILL.search(line):
                    found.append(f"{rel}:{n}: force-kill command (program spec I8)")
                elif n in restarts:
                    found.append(f"{rel}:{n}: {RESTART}")
                if BREAKAWAY.search(line) and not rel.startswith(BREAKAWAY_HOME):
                    found.append(f"{rel}:{n}: job breakaway outside iem-win (S6 design note §5.1)")
    goldens = root / "goldens"
    if goldens.is_dir():
        total = sum(p.stat().st_size for p in goldens.rglob("*") if p.is_file())
        if total > 20 * 1024 * 1024:
            found.append(f"goldens/: {total} bytes, over the 20 MB budget (spec §3.5)")
    return found


def main() -> int:
    found = violations(ROOT, repository=True)
    for item in found:
        print(f"::error::{item}")
    if found:
        return 1
    print("integrity: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
