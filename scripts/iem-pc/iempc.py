#!/usr/bin/env python3
"""Dev-box control of the IEM PC (S6, design note §5.1, §5.5, §6, §7).

`iemmode` over ssh with the EVENT-NOW discipline, attested bundles from CI
(fetch, install), HIL dispatch on the private ops repo, PC bootstrap through
the bundle's IemPc.psm1 (dev time only), and the hand-over of an open S1a
window.

"ide event": the flag file (~/.config/iemmixer/EVENT-NOW) exists. `event`
writes it first when it is missing (a flag it cannot write is a warning,
never a stop), pre-empts an open S1a/S1c spike window (spike_window.py
preempt), then runs `iemmode event`, and `iemmode event --direct` when the
guard is unreachable (exit 4). The event path has one budget that fits one
Bash call (EVENT_BUDGET_S): the spike preempt gets SPIKE_SHARE_S of it, no
`iemmode` call starts while the preempt still runs, and none starts with
less than SWITCH_MIN_S left. `event` never waits for another command.

Every other PC step waits for dev time: commands that change the PC refuse
while the flag exists, and `status` then reports this box only (`--pc`
asks the guard anyway). Every PC wait sees a new flag within 2 s: a
read-only call or a switch the guard owns is abandoned (the guard pre-empts
itself), a change the call makes itself completes first; then the command
runs the event path itself (exit 10). `dev`, `rehearse-teardown` and
`install` (except `--first`) refuse while an S1a/S1c window is open:
`handover-s1a` hands the card over first. `dispatch-hil` checks the flag
again right before it dispatches.

Site values come only from the private env file ($PC_ENV, default
~/.config/iemmixer/iem-pc.env): PC_SSH (the ssh destination), PC_ROOT (the
root folder on the PC, Windows form), PC_ROOT_SCP (the same folder as scp
names it) and PC_BIN (optional, default: bin under PC_ROOT). Nothing is ever
ended by force.

Known limits (S6 Task 16): `iemmode event --direct` runs the switch inside
the ssh session, so a session cut before it ends (a Bash timeout) stops it
half-way; the next `iemmode event` resumes from the guard state. A
pre-emption inside another command adds a whole event budget to that
command's own time. The dev-entry count behind `dispatch-hil` sees only
`iempc dev`, not a dev entry the guard makes by itself (an interlock retry,
rehearse-teardown's re-entry)."""
from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import zipfile
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterator

HERE = Path(__file__).resolve().parent
SPIKE_DIR = HERE.parent / "asio-spike"
SPIKE = SPIKE_DIR / "spike_window.py"
REPO = "zbynekdrlik/iemmixer"
OPS_REPO = "zbynekdrlik/iemmixer-ops"
CI_WORKFLOW = "ci.yml"
HIL_WORKFLOW = "hil.yml"
BRANCHES = ("dev", "main")
REQUIRED = ("PC_SSH", "PC_ROOT", "PC_ROOT_SCP")
# The bundle job's files (plan Task 12); the guard's install checks the same set.
BUNDLE_REQUIRED = (
    "iem-engine.exe", "iem-server.exe", "iemmixer-guard.exe", "iemmode.exe", "iem-tray.exe", "iem-migrate.exe",
    "hil-v1.ps1", "IemPc.psm1", "manifest.json",
)
SHA = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")
# A bundle member: a file name, or one directory level (`tuning/<name>`).
MEMBER = re.compile(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)?")
SUMS_LINE = re.compile(r"([0-9a-f]{64})  (\S+)")
FUNCTION = re.compile(r"(Get|Test|Set|Add|Register|Grant|Remove)-Iem[A-Za-z0-9]+")
READ_ONLY_VERBS = ("Get", "Test")
PARAM_NAME = re.compile(r"-[A-Za-z][A-Za-z0-9]*")
RUNNER_FUNCTION = "Register-IemRunner"
RUNNER_TOKEN = re.compile(r"[A-Za-z0-9]{20,200}")
# PowerShell ends a single-quoted string at any of these; each is doubled inside one.
PS_SINGLE_QUOTES = "'\u2018\u2019\u201a\u201b"
GUARD_UNREACHABLE = 4
PREEMPTED = 10
POLL_S = 2.0
STATUS_S = 120
SWITCH_S = 540
INSTALL_S = 540
BOOTSTRAP_S = 540
SCP_S = 540
# The event path, all of it: one Bash call ends at 10 min, the plan's waits stay within 9.
EVENT_BUDGET_S = 540
# spike_window.py preempt's part of it (its bring-back starts REAPER itself).
SPIKE_SHARE_S = 360
# An iemmode call of the event path never starts with less than this left.
SWITCH_MIN_S = 120
GH_S = 120
DOWNLOAD_S = 540
# What reading a zip member can raise besides StepError: bad JSON or UTF-8, a
# CRC error, a cut or corrupt deflate stream, an unknown compression method.
UNREADABLE = (ValueError, EOFError, NotImplementedError, zipfile.BadZipFile, zlib.error)
EVENT_NOW = Path(os.environ.get("IEMMIXER_EVENT_NOW", str(Path.home() / ".config/iemmixer/EVENT-NOW")))
STATE_DIR = Path(os.environ.get("IEMPC_STATE", str(Path.home() / ".local/share/iemmixer/iem-pc")))
SPIKE_STATE = Path(os.environ.get("SPIKE_STATE", str(Path.home() / ".local/state/iemmixer/spike-window.json")))
OWNER_ALARM = ("iempc: the event path did not complete: alarm the owner now with the prepared question (ops runbook "
               "docs/s6-pc-runbook.md); before the guard is installed, the interim switch (event runbook) applies; the "
               "last resort is the owner's reboot, which comes back in event mode. Never force-end anything.")


class StepError(Exception):
    """A step failed; the message says what to do next."""


class Refused(StepError):
    """Refused before anything was touched (no event path follows)."""


class StillRunning(StepError):
    """A call outlived its bound and was left running (never force-ended)."""


class EventNow(Exception):
    """The owner said "ide event" (the flag file appeared): pre-empt."""


# ---- env, flag, output ----

def env_path() -> Path:
    return Path(os.environ.get("PC_ENV", str(Path.home() / ".config/iemmixer/iem-pc.env")))


def load_env(path: Path) -> dict[str, str]:
    if not path.is_file():
        raise StepError(f"{path}: missing (private env, ops runbook docs/s6-pc-runbook.md)")
    env: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep:
            raise StepError(f"{path}: not KEY=VALUE: {key}")
        env[key.strip()] = value.strip().strip('"')
    missing = [k for k in REQUIRED if not env.get(k)]
    if missing:
        raise StepError(f"{path}: missing {', '.join(missing)}")
    if env["PC_SSH"].startswith("-"):
        raise StepError(f"{path}: PC_SSH must be an ssh destination, not an option")
    if not env.get("PC_BIN"):
        env["PC_BIN"] = pc_join(env["PC_ROOT"], "bin")
    return env


def event_now() -> bool:
    return EVENT_NOW.exists()


def ensure_flag() -> bool:
    """Writes the "ide event" flag (`date -Iseconds`) unless it exists; True when written."""
    if event_now():
        return False
    EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
    try:
        with open(EVENT_NOW, "x", encoding="utf-8") as f:
            f.write(now_iso() + "\n")
    except FileExistsError:
        return False
    return True


def write_flag() -> None:
    """`event` writes the flag first; a flag it cannot write (no folder, a
    full or read-only disk) is a warning, never a stop of the event path."""
    try:
        written = ensure_flag()
    except OSError as e:
        print(f"iempc: WARNING: the flag {EVENT_NOW} was not written ({e}); the event path goes on; write the flag by "
              "hand so no dev-time command runs", file=sys.stderr, flush=True)
        emit({"flag": str(EVENT_NOW), "written": False, "error": str(e)})
        return
    if written:
        emit({"flag": str(EVENT_NOW), "written": True})


def now_iso() -> str:
    return dt.datetime.now().astimezone().isoformat(timespec="seconds")


def emit(obj: dict) -> None:
    print(json.dumps(obj, ensure_ascii=False), flush=True)


def check_sha(text: str) -> str:
    if not SHA.fullmatch(text or ""):
        raise Refused(f"not a full commit SHA (40 lowercase hex): {text!r}")
    return text


# ---- waits (the EVENT-NOW discipline of spike_window.py) ----

def guarded(cmd: list[str], stdin: str, timeout: float, event: str) -> str:
    """Runs `cmd` and checks the "ide event" flag every POLL_S seconds.
    event="abandon": a read-only call is left to end by itself and EventNow
    is raised at once; "finish": a changing call completes, then EventNow;
    "ignore": the pre-emption itself, or a read-only call started while the
    flag already existed. A flag seen once counts even when it is gone by the
    end. Past `timeout` the call is left running and StillRunning is raised.
    Never ends anything by force."""
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            encoding="utf-8", errors="replace")
    deadline = time.monotonic() + timeout
    seen = False
    data: str | None = stdin
    while True:
        try:
            out, err = proc.communicate(data, timeout=POLL_S)
            break
        except subprocess.TimeoutExpired:
            data = None  # already handed over; a retry must not send it again
            if event != "ignore" and event_now():
                seen = True
                if event == "abandon":
                    raise EventNow() from None
            if time.monotonic() > deadline:
                raise StillRunning(f"{Path(cmd[0]).name} still running after {timeout} s (bounded on the PC; "
                                   "check 'iempc status', never force-end)") from None
    if proc.returncode != 0:
        raise StepError(f"{Path(cmd[0]).name} failed (exit {proc.returncode}): {err.strip()[-1500:]}")
    if event != "ignore" and (seen or event_now()):
        raise EventNow()
    return out


@dataclass(frozen=True)
class Ctx:
    env: dict[str, str]
    args: argparse.Namespace
    flag_at_start: bool

    def watch(self, abandon: bool) -> str:
        """How a wait reacts to the flag: only a flag that appears after the
        command started pre-empts it (changing commands refuse an existing
        one). "abandon" for a read-only call or a switch the guard owns (the
        guard goes on without its client and pre-empts itself within 1 s when
        `iemmode event` arrives); "finish" for a change the call makes itself
        (a copy, an install, a bootstrap step)."""
        if self.flag_at_start:
            return "ignore"
        return "abandon" if abandon else "finish"


# ---- the PC: ssh, scp, PowerShell ----

def ps_quote(value: str) -> str:
    out = "".join(c + c if c in PS_SINGLE_QUOTES else c for c in value)
    return "'" + out + "'"


def pc_join(base: str, rel: str) -> str:
    return base.rstrip("\\") + "\\" + rel.replace("/", "\\")


def remote(env: dict[str, str], rel: str) -> str:
    return f"{env['PC_SSH']}:{env['PC_ROOT_SCP'].rstrip('/')}/{rel}"


def ssh_cmd(env: dict[str, str]) -> list[str]:
    return ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"],
            "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -"]


def ssh_ps(env: dict[str, str], script: str, timeout: float, event: str) -> str:
    """Sends `script` (complete single-line statements: `-Command -` reads
    stdin line by line) and returns the PC's stdout."""
    return guarded(ssh_cmd(env), script + "\n", timeout, event)


def scp(src: str, dst: str, event: str) -> None:
    guarded(["scp", "-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", src, dst], "", SCP_S, event)


def ps_args(args: list[str]) -> str:
    for a in args:
        if '"' in a or any(ord(c) < 32 for c in a):
            raise StepError(f"argument not allowed on the PC: {a!r}")
    return "@(" + ", ".join(ps_quote(a) for a in args) + ")"


def hash_check(path: str, hexd: str) -> str:
    """A PowerShell statement that throws unless `path` has this sha256."""
    if not HEX64.fullmatch(hexd):
        raise StepError(f"not a sha256: {hexd!r}")
    q = ps_quote(path)
    return (f"if ((Get-FileHash -LiteralPath {q} -Algorithm SHA256 -ErrorAction Stop).Hash.ToLowerInvariant() -ne "
            f"'{hexd}') {{ throw ('sha256 mismatch: ' + {q}) }}")


def native_script(exe: str, args: list[str], checks: tuple[str, ...] = ()) -> str:
    """Runs a native program and prints {exit, out, err} as the last line;
    `exit` is null when the program did not start (or a check threw)."""
    pre = "".join(c + " ; " for c in checks)
    return "\n".join([
        "$ErrorActionPreference = 'Continue'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"try {{ {pre}$x = {ps_quote(exe)} ; $a = {ps_args(args)} ; $r = @(& $x @a 2>&1) ; $c = $LASTEXITCODE ; "
        "$out = @($r | Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] } | ForEach-Object { \"$_\" }) -join \"`n\" ; "
        "$err = @($r | Where-Object { $_ -is [System.Management.Automation.ErrorRecord] } | ForEach-Object { $_.Exception.Message }) -join \"`n\" ; "
        "$o = [pscustomobject]@{ exit = $c; out = $out; err = $err } } "
        "catch { $o = [pscustomobject]@{ exit = $null; out = ''; err = \"$_\" } } ; ConvertTo-Json -InputObject $o -Compress",
    ])


def module_script(body: str, module: str | None = None, module_hex: str | None = None, pre: str = "", fin: str = "") -> str:
    """Runs `body` (after importing `module`, checked by its sha256) and prints
    {ok, r} or {ok: false, error} as the last line."""
    load = ""
    if module is not None:
        load = hash_check(module, module_hex or "") + f" ; Import-Module {ps_quote(module)} -Force ; "
    tail = f" finally {{ {fin} }}" if fin else ""
    return "\n".join([
        "$ErrorActionPreference = 'Stop'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"try {{ {pre}{load}$r = & {{ {body} }} ; $o = [pscustomobject]@{{ ok = $true; r = $r }} }} "
        f"catch {{ $o = [pscustomobject]@{{ ok = $false; error = \"$_\" }} }}{tail} ; "
        "ConvertTo-Json -InputObject $o -Depth 8 -Compress",
    ])


def last_json(text: str) -> dict:
    lines = [line for line in text.splitlines() if line.strip()]
    if not lines:
        raise StepError("no output from the PC")
    try:
        doc = json.loads(lines[-1].lstrip("\ufeff"))
    except ValueError:
        raise StepError(f"the PC's last line is not JSON: {lines[-1][-300:]!r}") from None
    if not isinstance(doc, dict):
        raise StepError(f"the PC's last line is not a JSON object: {lines[-1][-300:]!r}")
    return doc


def run_native(env: dict[str, str], exe: str, args: list[str], timeout: float, event: str,
               checks: tuple[str, ...] = ()) -> dict:
    doc = last_json(ssh_ps(env, native_script(exe, args, checks), timeout, event))
    code = doc.get("exit")
    if code is not None and not isinstance(code, int):
        raise StepError(f"the PC reported a non-numeric exit code: {code!r}")
    return {"exit": code, "out": doc.get("out") or "", "err": doc.get("err") or ""}


def run_module(env: dict[str, str], body: str, timeout: float, event: str, **kw):
    doc = last_json(ssh_ps(env, module_script(body, **kw), timeout, event))
    if doc.get("ok") is not True:
        raise StepError(f"PC step failed: {doc.get('error')}")
    return doc.get("r")


def parse_reply(text: str) -> dict:
    """iemmode prints one JSON object (compact or indented)."""
    t = text.lstrip("\ufeff").strip()
    if not t:
        raise StepError("iemmode printed nothing")
    try:
        doc = json.loads(t)
    except ValueError:
        try:
            doc = json.loads(t.splitlines()[-1])
        except ValueError:
            raise StepError(f"the iemmode reply is not JSON: {t[-300:]!r}") from None
    if not isinstance(doc, dict):
        raise StepError(f"the iemmode reply is not a JSON object: {t[-300:]!r}")
    return doc


def owner_alarms(reply: dict | None) -> list[dict]:
    """Alarms the agent turns into the prepared owner question (not yet acknowledged)."""
    if not reply or not isinstance(reply.get("alarms"), list):
        return []
    return [a for a in reply["alarms"] if isinstance(a, dict) and a.get("owner_question") is True and not a.get("ack")]


def call(env: dict[str, str], exe: str, args: list[str], timeout: float, event: str,
         checks: tuple[str, ...] = (), json_reply: bool = True) -> tuple[int, dict | None, dict]:
    raw = run_native(env, exe, args, timeout, event, checks)
    if raw["exit"] is None:
        name = exe.rsplit("\\", 1)[-1]
        raise StepError(f"{name} did not run on the PC: {raw['err'][-800:]}")
    code = raw["exit"]
    reply = None
    if json_reply:
        try:
            reply = parse_reply(raw["out"])
        except StepError:
            if code == 0:
                raise
    for alarm in owner_alarms(reply):
        print("OWNER QUESTION (send the prepared question from the ops runbook): " + json.dumps(alarm, ensure_ascii=False),
              file=sys.stderr, flush=True)
    return code, reply, raw


def iemmode(env: dict[str, str], args: list[str], timeout: float, event: str,
            checks: tuple[str, ...] = ()) -> tuple[int, dict | None, dict]:
    return call(env, pc_join(env["PC_BIN"], "iemmode.exe"), args, timeout, event, checks)


def result(label: str, args: list[str], code: int, reply: dict | None, raw: dict) -> dict:
    out: dict = {label: args, "exit": code, "reply": reply}
    if reply is None:
        out["output"] = raw["out"][-2000:]
    if raw["err"]:
        out["stderr"] = raw["err"][-2000:]
    return out


# ---- dev-box state ($STATE, chmod 700) ----

def state_dir() -> Path:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    os.chmod(STATE_DIR, 0o700)
    return STATE_DIR


def read_json(path: Path, default):
    """A state file of this box: absent → `default`; anything but a JSON object is an error."""
    if not path.is_file():
        return default
    try:
        doc = json.loads(path.read_text(encoding="utf-8"))
    except ValueError:
        doc = None
    if not isinstance(doc, dict):
        raise StepError(f"{path}: not a JSON object; check it by hand")
    return doc


def write_json(path: Path, doc) -> None:
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(doc, indent=1), encoding="utf-8")
    os.chmod(tmp, 0o600)
    tmp.replace(path)


@contextlib.contextmanager
def state_lock(take: bool) -> Iterator[None]:
    """One changing command at a time on this box; `event` never takes it."""
    if not take:
        yield
        return
    with open(state_dir() / "iempc.lock", "a+", encoding="utf-8") as f:
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Refused("another iempc command runs on this box; wait for it ('iempc event' never waits)") from None
        yield


def current_entry() -> int:
    """The dev entry: counted up by every successful `iempc dev` (0 before
    the first). Known limit: the guard also enters dev by itself (a due
    interlock retry, rehearse-teardown's re-entry), which this box never
    sees, so "once per SHA per dev entry" means per `iempc dev` until the
    guard's status exposes a dev-entry id to key the dispatch record on."""
    return int(read_json(state_dir() / "entry.json", {}).get("entry", 0))


def next_entry(build: str | None) -> int:
    n = current_entry() + 1
    write_json(state_dir() / "entry.json", {"entry": n, "build": build, "at": now_iso()})
    return n


def spike_window_open() -> bool:
    """An S1a/S1c window is open (spike_window.py's state file); an
    unreadable state counts as open, so the spike tool decides."""
    if not SPIKE_STATE.is_file():
        return False
    try:
        state = json.loads(SPIKE_STATE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return True
    return not (isinstance(state, dict) and state.get("closed") is True)


def refuse_open_window(cmd: str) -> None:
    """The card goes to the guard only after the S1a/S1c window handed it over."""
    if spike_window_open():
        raise Refused(f"an S1a/S1c spike window is open ({SPIKE_STATE}): '{cmd}' waits until 'iempc handover-s1a' "
                      "has handed the card over")


# ---- bundles (P5: a green push run on dev/main, attested by digest) ----

def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def check_member(name: str) -> str:
    if not MEMBER.fullmatch(name) or any(part in (".", "..") for part in name.split("/")):
        raise StepError(f"bundle entry refused: {name!r}")
    return name


def parse_sums(text: str) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        m = SUMS_LINE.fullmatch(line)
        if not m:
            raise StepError(f"malformed SHA256SUMS line: {line!r}")
        name = check_member(m.group(2))
        if name in sums:
            raise StepError(f"SHA256SUMS lists {name} twice")
        sums[name] = m.group(1)
    return sums


def check_manifest(manifest: dict, sha: str, branch: str, run: int) -> None:
    got = (manifest.get("sha"), manifest.get("branch"), str(manifest.get("run")))
    if got != (sha, branch, str(run)):
        raise StepError(f"manifest.json names {got}, expected {(sha, branch, str(run))}")


def verify_zip(path: Path, sha: str, branch: str, run: int) -> dict[str, str]:
    """Every file listed in SHA256SUMS with its hash (the sums file itself
    exempt), nothing unlisted, the required files present, and the manifest
    naming this SHA, branch and run."""
    try:
        zf = zipfile.ZipFile(path)
    except zipfile.BadZipFile as e:
        raise StepError(f"{path.name}: not a zip ({e})") from None
    member = "the directory"
    with zf:
        try:
            members: dict[str, zipfile.ZipInfo] = {}
            for info in zf.infolist():
                name = info.filename.replace("\\", "/")
                if name.endswith("/"):
                    continue
                if check_member(name) in members:
                    raise StepError(f"{path.name}: {name} appears twice")
                members[name] = info
            if "SHA256SUMS" not in members:
                raise StepError(f"{path.name}: no SHA256SUMS")
            member = "SHA256SUMS"
            sums = parse_sums(zf.read(members["SHA256SUMS"]).decode("utf-8-sig"))
            present = set(members) - {"SHA256SUMS"}
            if set(sums) != present:
                raise StepError(f"{path.name}: listed but absent {sorted(set(sums) - present)}, "
                                f"present but unlisted {sorted(present - set(sums))}")
            absent = [n for n in BUNDLE_REQUIRED if n not in sums]
            if absent:
                raise StepError(f"{path.name}: required files missing: {absent}")
            for name, want in sorted(sums.items()):
                member = name
                h = hashlib.sha256()
                with zf.open(members[name]) as f:
                    for chunk in iter(lambda: f.read(1 << 20), b""):
                        h.update(chunk)
                if h.hexdigest() != want:
                    raise StepError(f"{path.name}: {name} does not match SHA256SUMS")
            member = "manifest.json"
            manifest = json.loads(zf.read(members["manifest.json"]).decode("utf-8-sig"))
        except UNREADABLE as e:
            raise StepError(f"{path.name}: {member} is unreadable ({type(e).__name__}: {str(e)[:300]})") from None
    if not isinstance(manifest, dict):
        raise StepError(f"{path.name}: manifest.json is not an object")
    check_manifest(manifest, sha, branch, run)
    return sums


def gh(args: list[str], timeout: float = GH_S) -> str:
    """The dev box's gh (its own authentication); dev-box work only."""
    try:
        proc = subprocess.run(["gh", *args], capture_output=True, text=True, timeout=timeout, check=False)
    except FileNotFoundError:
        raise StepError("gh is not installed on this box") from None
    except subprocess.TimeoutExpired:
        raise StepError(f"gh {' '.join(args[:2])}: no answer within {timeout} s") from None
    if proc.returncode != 0:
        hint = " (this gh has no 'attestation' command: install gh >= 2.49)" if "unknown command" in proc.stderr else ""
        raise StepError(f"gh {' '.join(args[:2])} failed (exit {proc.returncode}){hint}: {proc.stderr.strip()[-800:]}")
    return proc.stdout


def pick_runs(runs: list[dict], sha: str, branches: tuple[str, ...]) -> list[dict]:
    """Successful push runs of exactly `sha` on the allowed branches (P5)."""
    return [r for r in runs if r.get("headSha") == sha and r.get("event") == "push"
            and r.get("headBranch") in branches and r.get("conclusion") == "success"]


def job_ok(jobs: list[dict], name: str) -> bool:
    return any(j.get("name") == name and j.get("conclusion") == "success" for j in jobs)


def green_run(sha: str, branches: tuple[str, ...]) -> tuple[int, str]:
    listing = gh(["run", "list", "-R", REPO, "--workflow", CI_WORKFLOW, "--event", "push", "--commit", sha,
                  "--limit", "20", "--json", "databaseId,headSha,event,headBranch,conclusion"])
    for r in pick_runs(json.loads(listing or "[]"), sha, branches):
        jobs = json.loads(gh(["run", "view", str(r["databaseId"]), "-R", REPO, "--json", "jobs"])).get("jobs") or []
        if job_ok(jobs, "bundle") and job_ok(jobs, "attest"):
            return int(r["databaseId"]), r["headBranch"]
    raise StepError(f"no green push run of {CI_WORKFLOW} on {'/'.join(branches)} for {sha} with its 'bundle' and "
                    "'attest' jobs succeeded (P5)")


def branch_head(branch: str) -> str:
    head = gh(["api", f"repos/{REPO}/git/ref/heads/{branch}", "--jq", ".object.sha"]).strip()
    if not SHA.fullmatch(head):
        raise StepError(f"the head of {branch} reads {head!r}")
    return head


def bundle_dir(sha: str) -> Path:
    return state_dir() / "bundles" / sha


def zip_path(sha: str) -> Path:
    return bundle_dir(sha) / f"iemmixer-{sha}.zip"


def load_record(sha: str) -> dict | None:
    return read_json(bundle_dir(sha) / "fetch.json", None)


def need_record(sha: str) -> dict:
    rec = load_record(sha)
    if rec is None:
        raise Refused(f"bundle {sha} is not fetched: run 'iempc fetch-bundle --sha {sha}' first")
    return rec


def check_local_zip(sha: str, rec: dict) -> Path:
    z = zip_path(sha)
    if not z.is_file():
        raise StepError(f"{z} is missing: fetch the bundle again")
    got = "sha256:" + sha256_file(z)
    if got != rec.get("digest"):
        raise StepError(f"{z}: digest {got} differs from the fetched {rec.get('digest')} (P5): refused")
    return z


def latest_record_sha() -> str:
    found = []
    for p in (state_dir() / "bundles").glob("*/fetch.json"):
        doc = read_json(p, None)
        if isinstance(doc, dict) and doc.get("sha") == p.parent.name:
            found.append((str(doc.get("fetched_at", "")), doc["sha"]))
    if not found:
        raise Refused("no fetched bundle: run 'iempc fetch-bundle --sha SHA' first")
    return max(found)[1]


def extract_member(sha: str, rec: dict, name: str) -> tuple[Path, str]:
    """A top-level file of the fetched, verified zip, checked against its sums."""
    want = (rec.get("sums") or {}).get(name)
    if "/" in name or want is None:
        raise StepError(f"{name} is not a listed top-level file of bundle {sha}")
    z = check_local_zip(sha, rec)
    with zipfile.ZipFile(z) as zf:
        infos = [i for i in zf.infolist() if i.filename.replace("\\", "/") == name]
        if len(infos) != 1:
            raise StepError(f"{name} is not in {z.name} exactly once")
        data = zf.read(infos[0])
    if hashlib.sha256(data).hexdigest() != want:
        raise StepError(f"{name} in {z.name} does not match SHA256SUMS")
    out = bundle_dir(sha) / name
    tmp = out.with_name(name + ".tmp")
    tmp.write_bytes(data)
    os.chmod(tmp, 0o600)
    tmp.replace(out)
    return out, want


def fetch_bundle(sha: str, branch: str | None = None) -> tuple[dict, bool]:
    """(record, fetched now). A fetched bundle is reused after its digest check."""
    rec = load_record(sha)
    if rec is not None:
        check_local_zip(sha, rec)
        if branch is not None and rec.get("branch") != branch:
            raise StepError(f"bundle {sha} was fetched from {rec.get('branch')}, not {branch}")
        return rec, False
    run, run_branch = green_run(sha, (branch,) if branch else BRANCHES)
    dest = bundle_dir(sha)
    if dest.exists():
        raise StepError(f"{dest} exists without a fetch record: remove it, then fetch again")
    partial = dest.with_name(sha + ".partial")
    if partial.exists():
        shutil.rmtree(partial)
    partial.mkdir(parents=True)
    os.chmod(partial, 0o700)
    try:
        gh(["run", "download", str(run), "-R", REPO, "-n", f"iemmixer-bundle-{sha}", "-D", str(partial)], DOWNLOAD_S)
        z = partial / f"iemmixer-{sha}.zip"
        if not z.is_file():
            raise StepError(f"the artifact of run {run} has no iemmixer-{sha}.zip")
        digest = "sha256:" + sha256_file(z)
        sums = verify_zip(z, sha, run_branch, run)
        gh(["attestation", "verify", str(z), "-R", REPO, "--signer-workflow", f"{REPO}/.github/workflows/{CI_WORKFLOW}",
            "--source-ref", f"refs/heads/{run_branch}", "--deny-self-hosted-runners"])
    except BaseException:
        shutil.rmtree(partial)  # a refused or cut download is never kept
        raise
    rec = {"sha": sha, "branch": run_branch, "run": run, "digest": digest, "sums": sums, "fetched_at": now_iso()}
    write_json(partial / "fetch.json", rec)
    partial.rename(dest)
    return rec, True


# ---- commands ----

def box_state() -> dict:
    return {"event_now": event_now(), "spike_window_open": spike_window_open(), "dev_entry": current_entry()}


def cmd_status(ctx: Ctx) -> int:
    """The guard's status and this box's. While the flag exists only this
    box's, unless --pc: `iemmode status` may start the guard, and a guard's
    start runs the event plan's checks (and restarts what does not serve)."""
    if ctx.flag_at_start and not ctx.args.pc:
        emit({"iemmode": None, "skipped": f"{EVENT_NOW} exists: no PC step during an event ('status --pc' asks the "
                                          "guard anyway)", **box_state()})
        return 0
    code, reply, raw = iemmode(ctx.env, ["status"], STATUS_S, ctx.watch(abandon=True))
    out = result("iemmode", ["status"], code, reply, raw)
    out.update(box_state())
    emit(out)
    return code


def spike_preempt(timeout: float) -> dict:
    """spike_window.py preempt in its own process, bounded by its share of
    the event budget. A failure lets the event path go on; a preempt still
    running at the end of its share is reported as `running`."""
    try:
        out = guarded([sys.executable, str(SPIKE), "preempt"], "", timeout, "ignore")
    except StillRunning as e:
        print(f"iempc: spike preempt: {e}", file=sys.stderr, flush=True)
        return {"ok": False, "running": True, "error": str(e)[-1500:]}
    except StepError as e:
        print(f"iempc: spike preempt: {e}", file=sys.stderr, flush=True)
        return {"ok": False, "error": str(e)[-1500:]}
    return {"ok": True, "output": out[-4000:]}


def switch_timeout(deadline: float) -> float:
    """What an iemmode call of the event path may take: the rest of the one
    budget. It never starts with less than SWITCH_MIN_S left, since a cut
    `--direct` session stops its switch half-way."""
    left = deadline - time.monotonic()
    if left < SWITCH_MIN_S:
        raise StepError(f"the event path has {max(left, 0):.0f} s of its {EVENT_BUDGET_S} s budget left, less than the "
                        f"{SWITCH_MIN_S} s an iemmode call gets: run 'iempc event' again (a new budget)")
    return left


def cmd_event(ctx: Ctx) -> int:
    """The flag, the spike preempt when a window is open, then `iemmode
    event` (and `--direct` on exit 4), all within EVENT_BUDGET_S."""
    dry = bool(getattr(ctx.args, "dry_run", False))
    deadline = time.monotonic() + EVENT_BUDGET_S
    if not dry:
        write_flag()
    if spike_window_open():
        if dry:
            emit({"spike_window": "open", "plan": "spike_window.py preempt"})
        else:
            pre = spike_preempt(SPIKE_SHARE_S)
            emit({"spike_preempt": pre})
            if pre.get("running"):
                raise StepError(f"spike_window.py preempt still runs after its {SPIKE_SHARE_S} s share of the event "
                                "budget: no iemmode call while it may still be bringing REAPER back (one meter-bridge "
                                "trigger, the #9 lesson); run 'iempc event' again once it has ended "
                                "(spike_window.py status)")
    args = ["event", "--dry-run"] if dry else ["event"]
    code, reply, raw = iemmode(ctx.env, args, switch_timeout(deadline), "ignore")
    emit(result("iemmode", args, code, reply, raw))
    if code == GUARD_UNREACHABLE:
        args = [*args, "--direct"]
        code, reply, raw = iemmode(ctx.env, args, switch_timeout(deadline), "ignore")
        emit(result("iemmode", args, code, reply, raw))
    if code != 0 and not dry:
        print(OWNER_ALARM, file=sys.stderr, flush=True)
    return code


def cmd_dev(ctx: Ctx) -> int:
    """The guard owns the switch: a new flag abandons this client at once and
    the event path pre-empts the switch."""
    refuse_open_window("dev")
    args = ["dev"]
    if ctx.args.build:
        args += ["--build", check_sha(ctx.args.build)]
    dry = bool(ctx.args.dry_run)
    if dry:
        args.append("--dry-run")
    code, reply, raw = iemmode(ctx.env, args, STATUS_S if dry else SWITCH_S, ctx.watch(abandon=True))
    out = result("iemmode", args, code, reply, raw)
    if code == 0 and not dry:
        out["dev_entry"] = next_entry(ctx.args.build)
    emit(out)
    return code


def cmd_rehearse_teardown(ctx: Ctx) -> int:
    refuse_open_window("rehearse-teardown")
    code, reply, raw = iemmode(ctx.env, ["rehearse-teardown"], SWITCH_S, ctx.watch(abandon=True))
    emit(result("iemmode", ["rehearse-teardown"], code, reply, raw))
    return code


def cmd_probe_task(ctx: Ctx) -> int:
    code, reply, raw = iemmode(ctx.env, ["probe-task"], STATUS_S, ctx.watch(abandon=False))
    emit(result("iemmode", ["probe-task"], code, reply, raw))
    return code


def cmd_fetch_bundle(ctx: Ctx) -> int:
    rec, fetched = fetch_bundle(check_sha(ctx.args.sha))
    emit({"bundle": str(zip_path(rec["sha"])), "fetched": fetched,
          **{k: rec[k] for k in ("sha", "branch", "run", "digest")}, "files": sorted(rec["sums"])})
    return 0


def pc_mkdir(ctx: Ctx, rel: str, event: str) -> None:
    run_module(ctx.env, f"New-Item -ItemType Directory -Force -Path {ps_quote(pc_join(ctx.env['PC_ROOT'], rel))} | Out-Null ; 'ok'",
               STATUS_S, event)


def cmd_install(ctx: Ctx) -> int:
    """The verified zip to the PC, then the guard installs it: `iemmode
    install`, or for the first bundle (no iemmode on the PC yet) the zip's
    own `iemmixer-guard install`, which takes the guard mutex itself. Only
    the first bundle goes in while an S1a/S1c window is open: it starts no
    guard and touches no card, while `iemmode` may start the guard."""
    env, sha = ctx.env, check_sha(ctx.args.sha)
    if not ctx.args.first:
        refuse_open_window("install")
    rec = need_record(sha)
    z = check_local_zip(sha, rec)
    mode = ctx.watch(abandon=False)
    zip_rel = f"incoming/iemmixer-{sha}.zip"
    pc_zip = pc_join(env["PC_ROOT"], zip_rel)
    pc_mkdir(ctx, "incoming", mode)
    scp(str(z), remote(env, zip_rel), mode)
    checks = [hash_check(pc_zip, rec["digest"].split(":", 1)[1])]
    if ctx.args.first:
        local, hexd = extract_member(sha, rec, "iemmixer-guard.exe")
        exe_rel = f"incoming/iemmixer-guard-{sha}.exe"
        exe = pc_join(env["PC_ROOT"], exe_rel)
        scp(str(local), remote(env, exe_rel), mode)
        checks.append(hash_check(exe, hexd))
    else:
        exe = pc_join(env["PC_BIN"], "iemmode.exe")
    code, reply, raw = call(env, exe, ["install", pc_zip], INSTALL_S, mode, tuple(checks), json_reply=not ctx.args.first)
    out = result("install", [sha], code, reply, raw)
    out["via"] = "iemmixer-guard (first bundle)" if ctx.args.first else "iemmode"
    emit(out)
    return code


def load_dispatches() -> list[dict]:
    return list(read_json(state_dir() / "dispatch.json", {}).get("dispatches", []))


def cmd_dispatch_hil(ctx: Ctx) -> int:
    """Dispatches the ops hil.yml with this box's gh authentication (design
    §7): the SHA is a branch head with a green push run; branch, run and the
    attested digest come from that run; once per SHA per dev entry."""
    sha = check_sha(ctx.args.sha) if ctx.args.sha else branch_head("dev")
    branch = next((b for b in BRANCHES if branch_head(b) == sha), None)
    if branch is None:
        raise Refused(f"{sha} is not the head of {' or '.join(BRANCHES)}")
    run, _ = green_run(sha, (branch,))
    entry = current_entry()
    done = load_dispatches()
    if any(d.get("sha") == sha and d.get("entry") == entry for d in done):
        raise Refused(f"HIL for {sha} was already dispatched in dev entry {entry}")
    rec, _ = fetch_bundle(sha, branch)
    if int(rec["run"]) != run:
        raise StepError(f"bundle {sha} was fetched from run {rec['run']}, the green run is {run}: remove the local "
                        "bundle and fetch again")
    check_local_zip(sha, rec)
    if event_now():  # "ide event" during the gh waits above: HIL is dev-time work
        raise Refused(f"{EVENT_NOW} appeared: no HIL dispatch during an event (nothing was dispatched)")
    gh(["workflow", "run", HIL_WORKFLOW, "-R", OPS_REPO, "-f", f"sha={sha}", "-f", f"branch={branch}",
        "-f", f"run={run}", "-f", f"digest={rec['digest']}"])
    record = {"sha": sha, "branch": branch, "run": run, "digest": rec["digest"], "entry": entry, "at": now_iso()}
    write_json(state_dir() / "dispatch.json", {"dispatches": (done + [record])[-200:]})
    emit({"dispatched": record})
    return 0


def read_only_function(name: str) -> bool:
    return FUNCTION.fullmatch(name) is not None and name.split("-", 1)[0] in READ_ONLY_VERBS


def ps_params(params: list[str]) -> str:
    """`-Name value` pairs and `-Switch` flags for an IemPc function."""
    out = []
    for p in params:
        if p.startswith("--"):
            raise Refused(f"{p}: iempc options go before the function name")
        if PARAM_NAME.fullmatch(p):
            out.append(p)
        elif '"' in p or any(ord(c) < 32 for c in p):
            raise Refused(f"parameter value not allowed on the PC: {p!r}")
        else:
            out.append(ps_quote(p))
    return "".join(" " + p for p in out)


def cmd_bootstrap(ctx: Ctx) -> int:
    """An IemPc.psm1 function over ssh, from the fetched and verified bundle
    (the module's sha256 is checked on the PC before it is imported). Dev
    time only, read-only functions too (design §6): each run makes a folder,
    copies the module and runs it elevated on the PC. A new flag abandons a
    read-only function and lets a changing one finish."""
    env, fn = ctx.env, ctx.args.step
    if not FUNCTION.fullmatch(fn):
        raise Refused(f"not an IemPc function: {fn!r}")
    params = ps_params(ctx.args.params)
    sha = check_sha(ctx.args.sha) if ctx.args.sha else latest_record_sha()
    rec = need_record(sha)
    local, hexd = extract_member(sha, rec, "IemPc.psm1")
    pre = fin = ""
    if fn == RUNNER_FUNCTION:
        # The one-time registration token reaches the PC on stdin only, never a command line.
        token = gh(["api", "-X", "POST", f"repos/{OPS_REPO}/actions/runners/registration-token", "--jq", ".token"]).strip()
        if not RUNNER_TOKEN.fullmatch(token):
            raise StepError("the runner registration token has an unexpected form")
        pre = f"$env:ACTIONS_RUNNER_INPUT_TOKEN = {ps_quote(token)} ; "
        fin = "Remove-Item -Path 'Env:\\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue"
    mode = ctx.watch(abandon=read_only_function(fn))
    rel = f"bootstrap/{sha}"
    pc_mkdir(ctx, rel, mode)
    scp(str(local), remote(env, f"{rel}/IemPc.psm1"), mode)
    r = run_module(env, fn + params, BOOTSTRAP_S, mode, module=pc_join(env["PC_ROOT"], f"{rel}/IemPc.psm1"),
                   module_hex=hexd, pre=pre, fin=fin)
    emit({"bootstrap": fn, "sha": sha, "result": r})
    return 0


def handover_problems(r: dict, original: int) -> list[str]:
    problems = []
    if r.get("pref") != original:
        problems.append(f"the driver's preferred buffer reads {r.get('pref')}, the original is {original}")
    if r.get("holders"):
        problems.append(f"the card is held: {r.get('holders')}")
    if r.get("spike"):
        problems.append("a spike runs")
    if r.get("task"):
        problems.append("the spike task runs")
    return problems


def spike_module():
    """spike_window.py, imported only for the hand-over (the event path runs it
    as its own process)."""
    if str(SPIKE_DIR) not in sys.path:
        sys.path.insert(0, str(SPIKE_DIR))
    import spike_window

    return spike_window


def cmd_handover_s1a(ctx: Ctx) -> int:
    """Closes an open S1a window whose card is free: no driver-module holder,
    no spike or spike task running, the preference reads the original. From
    then on "ide event" is the guard's (`iempc event`)."""
    sw = spike_module()
    try:
        spike_env = sw.load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
        state = sw.load_state()
        if state.get("closed"):
            raise Refused(f"S1a window {state.get('id')} is already closed")
        if state.get("card") != "free":
            raise Refused(f"S1a window {state.get('id')}: the card is '{state.get('card')}', not free; close it with "
                          "spike_window.py to-event")
        body = (f"$p = Get-SpikeBufferPref -Key {ps_quote(spike_env['PC_BUFFER_KEY'])} -Name {ps_quote(spike_env['PC_BUFFER_NAME'])} ; "
                f"$h = Get-GoldenAsioHolders -Module {ps_quote(spike_env['PC_ASIO_MODULE'])} ; "
                f"$t = Get-ScheduledTask {sw.TASK} -ErrorAction SilentlyContinue ; "
                "[pscustomobject]@{ pref = $p.value; holders = @($h); "
                "spike = @(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count; "
                "task = [bool]($t -and $t.State -eq 'Running') }")
        r = sw.ps(spike_env, body, timeout=STATUS_S, event=ctx.watch(abandon=True))
    except sw.EventNow:
        raise EventNow() from None
    except sw.StepError as e:
        raise StepError(str(e)) from None
    problems = handover_problems(r, int(state["pref_original"]))
    if problems:
        raise StepError("S1a window stays open: " + "; ".join(problems))
    state["closed"] = True
    state["handed_over"] = {"to": "iemmixer guard (S6)", "at": now_iso(), "checks": r}
    sw.save_state(state)
    emit({"handover-s1a": state.get("id"), "closed": True, "checks": r})
    return 0


# ---- main ----

@dataclass(frozen=True)
class Spec:
    fn: Callable[[Ctx], int]
    pc: bool        # talks to the PC: a failure after a new flag runs the event path
    dev_time: bool  # refused while the flag exists (PC changes only in dev time)
    locked: bool    # one at a time on this box


COMMANDS: dict[str, Spec] = {
    "status": Spec(cmd_status, pc=True, dev_time=False, locked=False),
    "event": Spec(cmd_event, pc=True, dev_time=False, locked=False),
    "dev": Spec(cmd_dev, pc=True, dev_time=True, locked=True),
    "rehearse-teardown": Spec(cmd_rehearse_teardown, pc=True, dev_time=True, locked=True),
    "probe-task": Spec(cmd_probe_task, pc=True, dev_time=True, locked=True),
    "fetch-bundle": Spec(cmd_fetch_bundle, pc=False, dev_time=False, locked=True),
    "install": Spec(cmd_install, pc=True, dev_time=True, locked=True),
    "dispatch-hil": Spec(cmd_dispatch_hil, pc=False, dev_time=True, locked=True),
    "bootstrap": Spec(cmd_bootstrap, pc=True, dev_time=True, locked=True),
    "handover-s1a": Spec(cmd_handover_s1a, pc=True, dev_time=True, locked=True),
}


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="iempc", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("rehearse-teardown", "probe-task", "handover-s1a"):
        sub.add_parser(name)
    sub.add_parser("status").add_argument("--pc", action="store_true",
                                          help="ask the guard even while the flag exists (it may start the guard)")
    sub.add_parser("event").add_argument("--dry-run", action="store_true")
    dev = sub.add_parser("dev")
    dev.add_argument("--build")
    dev.add_argument("--dry-run", action="store_true")
    sub.add_parser("fetch-bundle").add_argument("--sha", required=True)
    install = sub.add_parser("install")
    install.add_argument("--sha", required=True)
    install.add_argument("--first", action="store_true", help="no iemmode on the PC yet: the zip's own guard installs it")
    sub.add_parser("dispatch-hil").add_argument("--sha")
    boot = sub.add_parser("bootstrap")
    boot.add_argument("--sha", help="the fetched bundle whose IemPc.psm1 runs (default: the newest fetched)")
    boot.add_argument("step", help="an IemPc.psm1 function, e.g. Get-IemBootstrapState")
    boot.add_argument("params", nargs=argparse.REMAINDER, help="-Name value pairs and -Switch flags")
    return ap


def preempt(env: dict[str, str]) -> int:
    try:
        code = cmd_event(Ctx(env, argparse.Namespace(dry_run=False), True))
    except StepError as e:
        print(f"iempc: event: {e}", file=sys.stderr, flush=True)
        print(OWNER_ALARM, file=sys.stderr, flush=True)
        return 1
    return PREEMPTED if code == 0 else 1


def main(argv: list[str]) -> int:
    args = build_parser().parse_args(argv)
    flag_at_start = event_now()
    try:
        env = load_env(env_path())
    except StepError as e:
        print(f"iempc: {e}", file=sys.stderr)
        return 1
    spec = COMMANDS[args.cmd]
    try:
        if spec.dev_time and flag_at_start:
            raise Refused(f"{EVENT_NOW} exists: an event is on; '{args.cmd}' runs only in dev time")
        with state_lock(spec.locked):
            return spec.fn(Ctx(env, args, flag_at_start))
    except Refused as e:
        print(f"iempc: {e}", file=sys.stderr, flush=True)
        return 1
    except EventNow:
        emit({"event": "ide event (flag file)", "action": "iempc event"})
    except StepError as e:
        print(f"iempc: {e}", file=sys.stderr, flush=True)
        if args.cmd == "event":
            if not args.dry_run:
                print(OWNER_ALARM, file=sys.stderr, flush=True)
            return 1
        # A hung call or an ssh error after "ide event" arrived must still bring REAPER back.
        if not (spec.pc and event_now() and not flag_at_start):
            return 1
        emit({"event": "ide event (flag file) after a failed step", "action": "iempc event"})
    return preempt(env)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
