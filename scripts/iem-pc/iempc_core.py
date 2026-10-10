"""Shared helpers of iempc.py, the dev box's control of the IEM PC (S6; split
out of iempc.py, #36): the private env, the "ide event" flag and the JSON
output, the bounded waits that watch the flag (`guarded`, `pause`, `Ctx`),
the PC calls over ssh and scp (PowerShell composers, `run_module`,
`iemmode`), this box's state, lock and dev-entry count, the S1a/S1c window's
state, and gh.

The names a test patches live here and nowhere else: EVENT_NOW, STATE_DIR,
SPIKE_STATE, POLL_S, ssh_ps, scp, gh, env_path and now_iso. Every other
module reads them as `core.<name>`, or `ip.<name>` through iempc.py, never
as a copy (`from iempc_core import <name>`), which a patch would not reach.

Imported by the Windows CI runner only to compose scripts (Test-IemStage.ps1):
`fcntl` is imported in `state_lock` alone."""
from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator

import iempc_bin

HERE = Path(__file__).resolve().parent
SPIKE_DIR = HERE.parent / "asio-spike"
REPO = "zbynekdrlik/iemmixer"
OPS_REPO = "zbynekdrlik/iemmixer-ops"
BRANCHES = ("dev", "main")
REQUIRED = ("PC_SSH", "PC_ROOT", "PC_ROOT_SCP")
SHA = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")
# PowerShell ends a single-quoted string at any of these; each is doubled inside one.
PS_SINGLE_QUOTES = "'\u2018\u2019\u201a\u201b"
POLL_S = 2.0
STATUS_S = 120
SWITCH_S = 540
INSTALL_S = 540
BOOTSTRAP_S = 540
SCP_S = 540
GH_S = 120
EVENT_NOW = Path(os.environ.get("IEMMIXER_EVENT_NOW", str(Path.home() / ".config/iemmixer/EVENT-NOW")))
STATE_DIR = Path(os.environ.get("IEMPC_STATE", str(Path.home() / ".local/share/iemmixer/iem-pc")))
SPIKE_STATE = Path(os.environ.get("SPIKE_STATE", str(Path.home() / ".local/state/iemmixer/spike-window.json")))


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


def pause(ctx: Ctx, seconds: float) -> None:
    """Waits `seconds`, looking at the flag every POLL_S: a flag that
    appeared after the command started pre-empts (EventNow), as in `guarded`."""
    end = time.monotonic() + seconds
    while True:
        if ctx.watch(abandon=True) != "ignore" and event_now():
            raise EventNow()
        left = end - time.monotonic()
        if left <= 0:
            return
        time.sleep(min(POLL_S, left))


# ---- the PC: ssh, scp, PowerShell ----

def ps_quote(value: str) -> str:
    out = "".join(c + c if c in PS_SINGLE_QUOTES else c for c in value)
    return "'" + out + "'"


def pc_join(base: str, rel: str) -> str:
    return base.rstrip("\\") + "\\" + rel.replace("/", "\\")


def remote(env: dict[str, str], rel: str) -> str:
    return f"{env['PC_SSH']}:{env['PC_ROOT_SCP'].rstrip('/')}/{rel}"


def ssh_cmd(env: dict[str, str]) -> list[str]:
    """Windows PowerShell by its full path (elevated_ps.REMOTE, #15)."""
    return ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"], elevated_ps().REMOTE]


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


def native_script(exe: str, args: list[str], checks: tuple[str, ...] = (), then: str = "") -> str:
    """Runs a native program and prints {exit, out, err, note} as the last line;
    `exit` is null when the program did not start (or a check threw). `then`
    runs once $x and $a are set and may point $x elsewhere (iempc_bin: the
    admin-only copy, `note` saying why not, #15). PSModulePath is pinned
    before the first command (elevated_ps.PIN, #15)."""
    pre = "".join(c + " ; " for c in checks)
    return "\n".join([
        "$ErrorActionPreference = 'Continue'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"{elevated_ps().PIN} ; try {{ {pre}$x = {ps_quote(exe)} ; $a = {ps_args(args)} ; {then}$r = @(& $x @a 2>&1) ; "
        "$c = $LASTEXITCODE ; "
        "$out = @($r | Where-Object { $_ -isnot [System.Management.Automation.ErrorRecord] } | ForEach-Object { \"$_\" }) -join \"`n\" ; "
        "$err = @($r | Where-Object { $_ -is [System.Management.Automation.ErrorRecord] } | ForEach-Object { $_.Exception.Message }) -join \"`n\" ; "
        "$o = [pscustomobject]@{ exit = $c; out = $out; err = $err; note = $iemNote } } "
        "catch { $o = [pscustomobject]@{ exit = $null; out = ''; err = \"$_\"; note = $iemNote } } ; "
        "ConvertTo-Json -InputObject $o -Compress",
    ])


def elevated_ps():
    """scripts/asio-spike/elevated_ps.py (#15): the module path pin and the
    remote command every ssh call uses, the stage, the admin-only folders and
    TEMP of an elevated ssh session. Loaded at the first PC call, never at
    import: a module of constants and string composers that imports only `re`,
    and no other S1a/S1c code reaches the event path."""
    if str(SPIKE_DIR) not in sys.path:
        sys.path.insert(0, str(SPIKE_DIR))
    import elevated_ps as ep
    return ep


def module_script(body: str, module: str | None = None, module_hex: str | None = None, pre: str = "", fin: str = "",
                  elevated_root: str | None = None) -> str:
    """Runs `body` and prints {ok, r} or {ok: false, error} as the last line.
    `module`: a module this box uploaded into a run folder of the user's root
    (a bundle's IemPc.psm1): its bytes are read once and checked by
    `module_hex`, staged admin-only under the elevated root, checked again
    there and imported only from there (#15, elevated_ps.staged_import).
    `elevated_root`: another elevated root than the PC's (the CI self-test).
    PSModulePath is pinned before the first command (elevated_ps.PIN, #15)."""
    ep = elevated_ps()
    load = ""
    if module is not None:
        if not HEX64.fullmatch(module_hex or ""):
            raise StepError(f"not a sha256: {module_hex!r}")
        root = ep.ROOT if elevated_root is None else ps_quote(elevated_root)
        load = ep.staged_import(ps_quote(module), module.rsplit("\\", 1)[-1], module_hex, root) + " ; "
    tail = f" finally {{ {fin} }}" if fin else ""
    return "\n".join([
        "$ErrorActionPreference = 'Stop'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"{ep.PIN} ; try {{ {pre}{load}$r = & {{ {body} }} ; $o = [pscustomobject]@{{ ok = $true; r = $r }} }} "
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
               checks: tuple[str, ...] = (), then: str = "") -> dict:
    doc = last_json(ssh_ps(env, native_script(exe, args, checks, then), timeout, event))
    code = doc.get("exit")
    if code is not None and not isinstance(code, int):
        raise StepError(f"the PC reported a non-numeric exit code: {code!r}")
    if doc.get("note"):
        iempc_bin.noted(str(doc["note"]))
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


# What the guard's status says of its lifecycle (`lifecycle::status`, Rust):
# nothing before the cutover.
PROD_SINCE = "prod since "


def guard_lifecycle(reply) -> str | None:
    """`trial` or `prod` from one guard reply's detail (pure, S8); None
    without a reply or a detail."""
    detail = reply.get("detail") if isinstance(reply, dict) else None
    if not isinstance(detail, str):
        return None
    return "prod" if PROD_SINCE in detail else "trial"


def owner_alarms(reply: dict | None) -> list[dict]:
    """Alarms the agent turns into the prepared owner question (not yet acknowledged).

    The guard's Alarm serializes its acknowledgement as `acked`.
    """
    if not reply or not isinstance(reply.get("alarms"), list):
        return []
    return [a for a in reply["alarms"] if isinstance(a, dict) and a.get("owner_question") is True and not a.get("acked")]


def call(env: dict[str, str], exe: str, args: list[str], timeout: float, event: str,
         checks: tuple[str, ...] = (), json_reply: bool = True, then: str = "") -> tuple[int, dict | None, dict]:
    raw = run_native(env, exe, args, timeout, event, checks, then)
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
    """iemmode from the admin-only bin while the guard last seen runs its
    build and it reads back, else PC_BIN's (#15, iempc_bin); the reply's
    guard_build is what the next call compares."""
    code, reply, raw = call(env, pc_join(env["PC_BIN"], "iemmode.exe"), args, timeout, event, checks,
                            then=iempc_bin.pick(sys.modules[__name__]))
    iempc_bin.seen(sys.modules[__name__], reply)
    return code, reply, raw


def result(label: str, args: list[str], code: int, reply: dict | None, raw: dict) -> dict:
    out: dict = {label: args, "exit": code, "reply": reply}
    if reply is None:
        out["output"] = raw["out"][-2000:]
    if raw["err"]:
        out["stderr"] = raw["err"][-2000:]
    return out


def pc_mkdir(ctx: Ctx, rel: str, event: str) -> None:
    run_module(ctx.env, f"New-Item -ItemType Directory -Force -Path {ps_quote(pc_join(ctx.env['PC_ROOT'], rel))} | Out-Null ; 'ok'",
               STATUS_S, event)


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
    import fcntl   # the dev box's lock; the Windows CI runner imports this module only to compose (Test-IemStage.ps1)
    with open(state_dir() / "iempc.lock", "a+", encoding="utf-8") as f:
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Refused("another iempc command runs on this box; wait for it ('iempc event' never waits)") from None
        yield


def current_entry() -> int:
    """The dev entry: counted up by every successful `iempc dev` and
    switch-test dev leg (0 before the first). Known limit: the guard also
    enters dev by itself (rehearse-teardown's re-entry), which this box never
    sees, so "once per SHA per dev entry" means per `iempc dev` or
    switch-test dev leg until the guard's status exposes a dev-entry id to
    key the dispatch record on."""
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


def spike_window_settling() -> bool:
    """A closed S1a/S1c window whose preempt (or to-event) still watches a PC
    change that was in flight (spike_window's settle, without the lock): its
    `settling` record is there and its bound not over. `spike_window.py
    preempt` waits for that watch, so iemmode event never starts the guard's
    bring-back next to the settle's (review of lane G2, finding 1)."""
    try:
        state = json.loads(SPIKE_STATE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    s = state.get("settling") if isinstance(state, dict) else None
    return isinstance(s, dict) and isinstance(s.get("until"), (int, float)) and time.time() <= s["until"]


def refuse_open_window(cmd: str) -> None:
    """The card goes to the guard only after the S1a/S1c window handed it over."""
    if spike_window_open():
        raise Refused(f"an S1a/S1c spike window is open ({SPIKE_STATE}): '{cmd}' waits until 'iempc handover-s1a' "
                      "has handed the card over")


def spike_module():
    """spike_window.py, imported for the hand-over and for closing a window
    after a failed preempt (the preempt itself runs as its own process). Its
    state file is the one this box reads (SPIKE_STATE), so both lock and read
    one file."""
    if str(SPIKE_DIR) not in sys.path:
        sys.path.insert(0, str(SPIKE_DIR))
    import spike_window

    spike_window.STATE = SPIKE_STATE
    return spike_window


# ---- GitHub ----

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
