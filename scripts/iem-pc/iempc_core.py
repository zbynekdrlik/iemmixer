"""Shared helpers of iempc.py, the dev box's control of the Gen 3 box (split out
of iempc.py, #36): the private env, the "ide event" flag and the JSON output, the
bounded waits that watch the flag (`guarded`, `pause`, `Ctx`), this box's state
and lock, and gh.

The names a test patches live here and nowhere else: EVENT_NOW, STATE_DIR,
POLL_S, gh, env_path and now_iso. Every other module reads them as
`core.<name>`, or `ip.<name>` through iempc.py, never as a copy
(`from iempc_core import <name>`), which a patch would not reach."""
from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import fcntl
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator

REPO = "zbynekdrlik/iemmixer"
SHA = re.compile(r"[0-9a-f]{40}")
POLL_S = 2.0
GH_S = 120
EVENT_NOW = Path(os.environ.get("IEMMIXER_EVENT_NOW", str(Path.home() / ".config/iemmixer/EVENT-NOW")))
STATE_DIR = Path(os.environ.get("IEMPC_STATE", str(Path.home() / ".local/share/iemmixer/iem-pc")))


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
        raise StepError(f"{path}: missing (the private env)")
    env: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep:
            raise StepError(f"{path}: not KEY=VALUE: {key}")
        env[key.strip()] = value.strip().strip('"')
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


# ---- waits (the EVENT-NOW discipline) ----

def guarded(cmd: list[str], stdin: str, timeout: float, event: str, *, env: dict[str, str] | None = None,
            cwd: str | None = None) -> str:
    """Runs `cmd` and checks the "ide event" flag every POLL_S seconds.
    event="abandon": a read-only call is left to end by itself and EventNow
    is raised at once; "finish": a changing call completes, then EventNow;
    "ignore": the pre-emption itself, or a read-only call started while the
    flag already existed. A flag seen once counts even when it is gone by the
    end. Past `timeout` the call is left running and StillRunning is raised.
    Never ends anything by force. `env` (the whole environment) and `cwd`
    default to this process's (the Gen 3 browser step's LIVE_* values,
    iempc_gen3)."""
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            encoding="utf-8", errors="replace", env=env, cwd=cwd)
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
                raise StillRunning(f"{Path(cmd[0]).name} still running after {timeout} s (never force-end)") from None
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
        one). "abandon" for a read-only call; "finish" for a change the call
        makes itself."""
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
