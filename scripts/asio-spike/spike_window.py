#!/usr/bin/env python3
"""S1a ASIO-spike window driver on the dev box (design note §5); also the
interim switch between REAPER and iemmixer until S6's `iemmode`.

A window opens only with the owner's quoted "event skončil" and never while
the "ide event" flag file exists. Every wait checks that flag every 2 s; when
it appears the driver stops the spike through its stop file, restores the
driver's preferred buffer (read back) and brings REAPER back with the handover
checks (`preempt`); a step that fails while the flag exists pre-empts too
(exit 10). PC work runs in SpikePc.psm1 over ssh; site values come only from
the private env file ($SPIKE_ENV). Nothing is ever ended by force."""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "golden"))
from golden_window import StepError, check_signal, interlock_hits, parse_meter_peaks, ps_quote  # noqa: E402

REQUIRED = (
    "PC_SSH", "PC_ROOT", "PC_ROOT_SCP", "PC_ASIO_MODULE", "PC_ASIO_DRIVER",
    "PC_BUFFER_KEY", "PC_BUFFER_NAME", "PC_BUFFER_ORIGINAL",
    "PC_REAPER_HTTP", "PC_MAIN_PROJECT", "PC_REAPER_START_TASK_PATH", "PC_REAPER_START_TASK",
    "PC_NTRACK", "PC_METER_BRIDGE", "PC_METER_HEARTBEAT", "PC_METER_ACTION",
    "PC_APP_PROCESS", "PC_APP_HTTP", "PC_ACTIVITY_CHANNELS", "RAW_DIR",
)
# The inputs the spike's band guard listens to: the site's stage inputs as card
# numbers from 1 ("101-110,121-124"), or "all" only when asked (program inputs may
# carry signal while the band is silent).
ACTIVITY_CHANNELS = re.compile(r"all|[0-9]+(-[0-9]+)?(,[0-9]+(-[0-9]+)?)*")
CPU_LIST = re.compile(r"[0-9]+(-[0-9]+)?(,[0-9]+(-[0-9]+)?)*")
MAX_INPUT = 1024
FRAMES = (32, 48, 64)
MAX_SECONDS = 36_000     # an 8 h soak with margin (S1c design note §8 W4)
POLL_S = 2.0
REPO = "zbynekdrlik/iemmixer"
BUNDLE_FILES = ("GoldenPc.psm1", "IemMeasure.psm1", "IemTuning.psm1", "SpikePc.psm1", "asio_spike.exe", "spike-task.ps1")
TASK = "-TaskPath '\\iemmixer\\' -TaskName 'iemmixer-asio-spike'"
STATE = Path(os.environ.get("SPIKE_STATE", str(Path.home() / ".local/state/iemmixer/spike-window.json")))
# Spike exit codes the owner must hear about at once (crates/iem-audio-io/examples/asio_spike/main.rs).
ALARMS = {
    5: "band activity on the stage inputs during the spike (loudest_inputs in the verdict): the band may be playing; tell the owner now, no further run",
    8: "a callback did not leave the stream within the stop wait (R6): tell the owner now, no further run",
}
EVENT_NOW = Path(os.environ.get("IEMMIXER_EVENT_NOW", str(Path.home() / ".config/iemmixer/EVENT-NOW")))


class EventNow(Exception):
    """The owner said "ide event" (the flag file exists): pre-empt."""


# ssh's own messages when it exits 255 before any session existed (refused,
# timed out, name not resolved, not authorised, host key): nothing was sent.
SSH_NOT_CONNECTED = re.compile(r"(?i)ssh: connect to host|could not resolve hostname|name or service not known|"
                               r"temporary failure in name resolution|no route to host|network is unreachable|"
                               r"permission denied|host key verification failed")


class NoReply(StepError):
    """A PC call was sent but gave no reply: the ssh session ended (a non-zero
    exit), the call outlived its bound, or its output held no complete JSON
    reply. What happened on the PC is unknown — unlike a reply that reports
    an error (a plain StepError)."""


def load_env(path: Path) -> dict[str, str]:
    if not path.is_file():
        raise StepError(f"{path}: missing (private env, plan Task 10)")
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
    for k in ("PC_BUFFER_ORIGINAL", "PC_NTRACK"):
        if not env[k].isdigit():
            raise StepError(f"{path}: {k} must be a whole number")
    check_channels(env["PC_ACTIVITY_CHANNELS"], path)
    return env


def check_channels(text: str, path: Path) -> None:
    """The spike refuses the same lists (telemetry.rs `Watched::parse`)."""
    ok = ACTIVITY_CHANNELS.fullmatch(text) is not None
    if ok and text != "all":
        for part in text.split(","):
            first, _, last = part.partition("-")
            lo, hi = int(first), int(last or first)
            ok = ok and 1 <= lo <= hi <= MAX_INPUT
    if not ok:
        raise StepError(f"{path}: PC_ACTIVITY_CHANNELS must be 'all' or card inputs from 1 like 101-110,121-124 (got {text!r})")


def event_now() -> bool:
    return EVENT_NOW.exists()


def check_request(mode: str, frames: int | None, seconds: int, burn_us: int, stress: int, cycles: int,
                  cpu: int | None = None, threshold_us: int = 10, audio_cpus: str = "", stress_cpus: str = "") -> None:
    if mode not in ("probe", "duplex", "reopen", "hwlat"):
        raise StepError(f"unknown mode {mode}")
    if mode in ("duplex", "reopen") and frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if mode == "hwlat" and not (cpu is not None and 0 <= cpu <= 63 and 1 <= threshold_us <= 1000):
        raise StepError("hwlat needs --cpu 0..63 and --threshold-us 1..1000")
    if not (1 <= seconds <= MAX_SECONDS and 0 <= burn_us <= 300 and 0 <= stress <= 8 and 1 <= cycles <= 20):
        raise StepError(f"limits: seconds 1..{MAX_SECONDS}, burn-us 0..300, stress 0..8, cycles 1..20")
    for text in (audio_cpus, stress_cpus):
        if text and not CPU_LIST.fullmatch(text):
            raise StepError("CPU lists look like 14 or 0,1,6-13")
    # The spike's own rules: busy threads next to a reserved audio CPU need their
    # own CPUs, and those never include an audio CPU (with or without threads).
    if stress > 0 and audio_cpus and not stress_cpus:
        raise StepError("--stress with --audio-cpus needs --stress-cpus (the busy threads' own CPUs)")
    overlap = sorted(cpu_set(stress_cpus) & cpu_set(audio_cpus))
    if overlap:
        raise StepError(f"--stress-cpus and --audio-cpus overlap on processors {overlap} "
                        "(a busy thread would run next to the audio callback)")


def cpu_set(text: str) -> set[int]:
    """The processors of a CPU list like 0,1,6-13 (checked by CPU_LIST)."""
    out: set[int] = set()
    for part in (p for p in text.split(",") if p):
        first, _, last = part.partition("-")
        out.update(range(int(first), int(last or first) + 1))
    return out


def run_fields(env: dict[str, str], args) -> dict:
    """The request the PC task hands to the spike."""
    return {"mode": args.mode, "driver": env["PC_ASIO_DRIVER"], "module": env["PC_ASIO_MODULE"], "frames": args.frames or 0,
            "seconds": args.seconds, "burn_us": args.burn_us, "stress": args.stress, "panic_at": args.panic_at,
            "cycles": args.cycles,
            "cpu": -1 if getattr(args, "cpu", None) is None else args.cpu, "threshold_us": getattr(args, "threshold_us", 10),
            "audio_cpus": getattr(args, "audio_cpus", "") or "", "stress_cpus": getattr(args, "stress_cpus", "") or "",
            "activity_channels": env["PC_ACTIVITY_CHANNELS"],
            "timeout": run_timeout(args.mode, args.seconds, args.cycles)}


def run_timeout(mode: str, seconds: int, cycles: int) -> int:
    """Seconds after which the PC task writes the stop file itself."""
    return {"probe": 60, "duplex": seconds + 60, "reopen": 30 * cycles + 60, "hwlat": seconds + 60}[mode]


def buffer_touched(state: dict) -> bool:
    """A set-buffer was recorded (before its write, which may have failed
    half-way): the registry value is unknown, so it is written back and read
    back whatever the recorded value says."""
    return state.get("pref_current") is not None


def undo_plan(state: dict, spike_running: bool) -> list[str]:
    """What leaving the window (or "ide event") must do, in order. While the
    card is free a spike may be starting (the task has not launched it yet),
    so the graceful stop always runs; it is harmless when none runs. A kernel
    trace stops and the S1c mode levers revert before the buffer and REAPER
    (S1c design note §5.2); the fingerprint is read after REAPER is back.
    `rebooting` (tuning_window reboot-prepare) is card-away too: REAPER was
    quit and comes back here unless the reboot already brought it. Its clean
    unwind restored the buffer with read-back, and after the reboot REAPER may
    hold the driver, so a verified restore is not written again there (the
    bring-back reads it and refuses unless it is the original)."""
    plan: list[str] = []
    card = state.get("card")
    card_away = card in ("switching", "free", "rebooting")
    if spike_running or card_away:
        plan.append("stop-spike")
    if state.get("trace"):
        plan.append("trace-stop")
    if state.get("tuning_mode"):
        plan.append("tuning-exit")
    if buffer_touched(state) and not (card == "rebooting" and state.get("pref_restored")):
        plan.append("restore-buffer")
    if card_away:
        plan.append("bring-back")
        if state.get("fingerprint"):
            plan.append("fingerprint")
    return plan


def buffer_args(state: dict) -> str:
    """-Original (and the original text of a String value, from preflight)."""
    args = f"-Original {state['pref_original']}"
    pre = state.get("preflight") or {}
    if pre.get("kind") == "String" and pre.get("raw"):
        args += f" -Raw {ps_quote(str(pre['raw']))}"
    return args


def preflight_problems(r: dict, original: int, dev_time: bool = False) -> list[str]:
    """dev_time: the window opens after REAPER was already saved and quit
    ("event skončil" handled before the window), so REAPER must be gone and
    nothing may hold the ASIO module; otherwise REAPER must hold it."""
    problems = []
    if r["pref"] != original:
        problems.append(f"the driver's preferred buffer is {r['pref']}, the recorded original is {original}: stop and tell the owner")
    if dev_time and r["reaper"]:
        problems.append("REAPER runs (a dev-time window starts with REAPER saved and quit)")
    if not dev_time and not r["reaper"]:
        problems.append("REAPER is not running (a window starts from the event state)")
    if not r["app"]:
        problems.append("the predecessor app is not running")
    if r["spike"]:
        problems.append("a spike already runs")
    if any(dev_time or not h.lower().startswith("reaper.exe:") for h in r["holders"] or []):
        problems.append(f"unexpected ASIO module holders {r['holders']}")
    if not r["task"]:
        problems.append("the spike task is not registered (run setup)")
    if r["files"] != len(BUNDLE_FILES):
        problems.append(f"{r['files']} verified bundle files on the PC, expected {len(BUNDLE_FILES)}")
    return problems


def ps_hashtable(fields: dict) -> str:
    parts = []
    for k, v in fields.items():
        if not re.fullmatch(r"[a-z_]+", k):
            raise StepError(f"bad request field {k!r}")
        parts.append(f"{k} = {v}" if isinstance(v, int) and not isinstance(v, bool) else f"{k} = {ps_quote(str(v))}")
    return "@{ " + "; ".join(parts) + " }"


def parse_sums(text: str) -> dict[str, str]:
    sums: dict[str, str] = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        m = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not m:
            raise StepError(f"malformed SHA256SUMS line: {line!r}")
        sums[m.group(2)] = m.group(1)
    return sums


def verify_bundle(bundle: Path) -> list[str]:
    sums = parse_sums((bundle / "SHA256SUMS").read_text(encoding="utf-8"))
    if sorted(sums) != sorted(BUNDLE_FILES):
        raise StepError(f"bundle lists {sorted(sums)}, expected {sorted(BUNDLE_FILES)}")
    for name, sha in sums.items():
        if hashlib.sha256((bundle / name).read_bytes()).hexdigest() != sha:
            raise StepError(f"bundle file {name} does not match SHA256SUMS")
    return sorted(sums)


def pick_run(runs: list[dict], sha: str) -> int:
    """The successful push run on dev for exactly `sha` (P5)."""
    for r in runs:
        if (r.get("headSha"), r.get("event"), r.get("headBranch"), r.get("conclusion")) == (sha, "push", "dev", "success"):
            return int(r["databaseId"])
    raise StepError(f"no successful push run on dev for {sha}")


def verdict(report: dict) -> dict:
    """Stable = ended as planned with no missed period, no overrun, no
    position gap and no driver reset, overload or buffer-size message."""
    tel = [s.get("telemetry") or {} for s in report.get("segments", [])]
    total = {k: sum(t.get(k, 0) for t in tel) for k in ("callbacks", "late", "missed", "overruns", "position_gaps")}
    messages = {k: sum((t.get("messages") or {}).get(k, 0) for t in tel) for k in ("resets", "overloads", "buffer_size_changes")}
    worst = max(((t.get("interval_us") or {}).get("p999", 0.0) for t in tel), default=0.0)
    stable = report.get("outcome") == "done" and bool(tel) and all(v == 0 for v in messages.values()) and all(
        total[k] == 0 for k in ("missed", "overruns", "position_gaps"))
    return {"outcome": report.get("outcome"), "stable": stable, **messages, "interval_p999_us": worst, **total,
            "activity_channels": report.get("activity_channels"), "loudest_inputs": report.get("loudest_inputs", [])}


def guarded(cmd: list[str], stdin: str, timeout: float, event: str) -> str:
    """Runs `cmd` and checks the "ide event" flag every POLL_S seconds.
    event="abandon": a read-only call is left to end by itself and EventNow
    is raised at once; "finish": a changing call completes, then EventNow;
    "ignore": the pre-emption itself. Never kills anything."""
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
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
                raise NoReply(f"PC call still running after {timeout} s (bounded on the PC; check it, never kill)") from None
    if proc.returncode != 0:
        text = f"PC command failed (exit {proc.returncode}): {err.strip()[-1500:]}"
        if proc.returncode == 255 and SSH_NOT_CONNECTED.search(err):
            raise StepError(text + " (ssh never connected: nothing was sent)")
        raise NoReply(text)
    if event != "ignore" and (seen or event_now()):
        raise EventNow()
    return out


# ---- PC access (the PC is the external dependency; no unit tests below) ----

def ssh_cmd(env: dict[str, str]) -> list[str]:
    return ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", env["PC_SSH"],
            "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -"]


def ps_script(root: str, body: str) -> str:
    """The PowerShell text sw.ps sends to `powershell -Command -` on the PC:
    `body` after importing SpikePc from <root>\\bin, its result or error as
    one JSON line (single-line statements: `-Command -` reads stdin line by
    line). The Windows CI runner executes it as printed (tuning_window
    poll-script)."""
    return "\n".join([
        "$ErrorActionPreference = 'Stop'",
        "$ProgressPreference = 'SilentlyContinue'",
        f"try {{ Import-Module (Join-Path {ps_quote(root)} 'bin\\SpikePc.psm1') -Force ; $r = & {{ {body} }} ; $o = [pscustomobject]@{{ ok = $true; r = $r }} }} "
        f"catch {{ $o = [pscustomobject]@{{ ok = $false; error = \"$_\" }} }} ; ConvertTo-Json -InputObject $o -Depth 8 -Compress",
    ])


def ps(env: dict[str, str], body: str, timeout: float = 300, event: str = "finish"):
    """Runs `body` on the PC (ps_script); PC errors come back as {ok: false}
    and raise StepError, a call without a complete reply raises NoReply."""
    out = [line for line in guarded(ssh_cmd(env), ps_script(env["PC_ROOT"], body) + "\n", timeout, event).splitlines() if line.strip()]
    try:
        doc = json.loads(out[-1]) if out else None
    except ValueError:
        doc = None
    if not isinstance(doc, dict) or "ok" not in doc:
        raise NoReply("no complete reply from the PC")
    if not doc["ok"]:
        raise StepError(f"PC step failed: {doc['error']}")
    return doc["r"]


SCP = ("scp", "-q", "-o", "BatchMode=yes")
SCP_BOUND_S = 600.0
REMOTE_PATH = re.compile(r"^[^/:]+:")


def scp(src: str, dst: str, event: str = "ignore") -> None:
    """Copies one file over ssh, bounded at SCP_BOUND_S. event="abandon" (an
    analysis download a preempt must not wait for): the "ide event" flag is
    checked every POLL_S; on the flag the copy is interrupted and EventNow
    raised at once, and a copy that ended while the flag appeared raises it
    too. "ignore": the copy runs to its end."""
    proc = subprocess.Popen([*SCP, src, dst], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                            stderr=subprocess.PIPE, text=True, preexec_fn=sigint_default)
    deadline = time.monotonic() + SCP_BOUND_S
    while True:
        try:
            _, err = proc.communicate(timeout=POLL_S)
            break
        except subprocess.TimeoutExpired:
            if event == "abandon" and event_now():
                interrupt_copy(proc, dst)
                raise EventNow() from None
            if time.monotonic() > deadline:
                interrupt_copy(proc, dst)
                raise StepError(f"scp still running after {SCP_BOUND_S:g} s: interrupted ({src})") from None
    if proc.returncode != 0:
        raise StepError(f"scp failed: {err.strip()[-800:]}")
    if event == "abandon" and event_now():
        raise EventNow()


def sigint_default() -> None:
    """In the scp child before it starts: SIGINT's default action, so the
    interrupt below works even when this process ignores SIGINT (an ignored
    signal is inherited across exec)."""
    signal.signal(signal.SIGINT, signal.SIG_DFL)


def interrupt_copy(proc: subprocess.Popen, dst: str) -> None:
    """A local scp is stopped the way an operator stops it, with Ctrl-C:
    SIGINT, on which scp closes its ssh session (so the PC stops sending)
    and exits by itself; then a bounded wait for that exit. Nothing is ended
    harder: a copy still running after the wait is reported and left alone.
    A partial download is removed."""
    proc.send_signal(signal.SIGINT)
    try:
        proc.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        alarm(f"scp (pid {proc.pid}) did not exit within 10 s of Ctrl-C; it is left to end by itself")
    if not REMOTE_PATH.match(dst):
        Path(dst).unlink(missing_ok=True)


def remote(env: dict[str, str], rel: str) -> str:
    return f"{env['PC_SSH']}:{env['PC_ROOT_SCP']}/{rel}"


def pc(env: dict[str, str], rel: str) -> str:
    return ps_quote(env["PC_ROOT"] + "\\" + rel.replace("/", "\\"))


def raw_dir(env: dict[str, str], state: dict) -> Path:
    d = Path(env["RAW_DIR"]).expanduser() / "asio-spike" / state["id"]
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    return d


def bundle_dir(env: dict[str, str], sha: str) -> Path:
    return Path(env["RAW_DIR"]).expanduser() / "asio-spike" / "bundles" / sha


# ---- state ----

def load_state() -> dict:
    if not STATE.is_file():
        raise StepError("no window: run 'new --signal ...' first")
    return json.loads(STATE.read_text(encoding="utf-8"))


# Decision A of the #32 review: ONE exclusive dev-box lock (flock on a file
# next to STATE) serialises every read-modify-write of the window state, every
# bring-back and every unwind across processes (spike_window, tuning_window,
# iempc event's preempt). Reentrant per thread; waited for in bounded steps.
LOCK_WAIT_S = 1200.0     # longer than a whole unwind (stop, restore, bring-back)
_held = threading.local()


@contextmanager
def window_lock() -> Iterator[None]:
    """Holds the window lock. A holder that does not let go within
    LOCK_WAIT_S is an owner alarm and a StepError, never a wait forever."""
    if getattr(_held, "depth", 0):
        _held.depth += 1
        try:
            yield
        finally:
            _held.depth -= 1
        return
    import fcntl   # the dev box's lock; the Windows CI runner imports this module only for poll-script
    path = STATE.with_name(STATE.name + ".lock")
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
    try:
        deadline = time.monotonic() + LOCK_WAIT_S
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() > deadline:
                    alarm(f"the window lock {path} was not free within {LOCK_WAIT_S:g} s: another window process "
                          "holds it (an unwind or a bring-back?); check it before acting by hand")
                    raise StepError(f"the window lock was not free within {LOCK_WAIT_S:g} s") from None
                time.sleep(0.05)
        _held.depth = 1
        try:
            yield
        finally:
            _held.depth = 0
    finally:
        os.close(fd)   # closing the descriptor releases the lock


def save_state(state: dict) -> None:
    """Writes the whole state atomically under the window lock, through a
    temp file of this writer's own."""
    with window_lock():
        STATE.parent.mkdir(parents=True, exist_ok=True)
        tmp = STATE.with_name(f"{STATE.name}.{os.getpid()}.{threading.get_ident()}.tmp")
        tmp.write_text(json.dumps(state, indent=1), encoding="utf-8")
        tmp.replace(STATE)


def update_state(fields: dict | None = None, change: Callable[[dict], None] | None = None) -> dict:
    """One read-modify-write of the state as saved NOW, under the window lock:
    `fields` merged in, then `change` applied. Returns the saved state."""
    with window_lock():
        state = load_state()
        state.update(fields or {})
        if change is not None:
            change(state)
        save_state(state)
        return state


def open_state() -> dict:
    state = load_state()
    if state.get("closed"):
        raise StepError(f"window {state['id']} is closed: open a new one")
    if event_now():
        raise EventNow()
    return state


def need_preflight(state: dict) -> None:
    """A dev-time window has a free card from the start: no buffer write
    and no run before its preflight passed."""
    if "preflight" not in state:
        raise StepError("run preflight first")


def spike_running(env: dict[str, str], event: str = "abandon") -> bool:
    return bool(ps(env, "@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count", timeout=60, event=event))


# ---- commands ----

def cmd_new(env, args) -> None:
    check_signal(args.signal)
    if event_now():
        raise StepError(f"{EVENT_NOW} exists: an event is on, no window")
    with window_lock():
        if STATE.is_file() and not json.loads(STATE.read_text(encoding="utf-8")).get("closed"):
            raise StepError("the last window is still open: finish it (to-event) or run preempt")
        wid = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        # --dev-time: REAPER was already saved and quit, so the card is free from the start and
        # "ide event" (preempt, to-event) brings REAPER back with the handover checks.
        save_state({"id": wid, "signal": args.signal, "card": "free" if args.dev_time else "reaper", "dev_time": bool(args.dev_time),
                    "pref_original": int(env["PC_BUFFER_ORIGINAL"]), "pref_current": None, "pref_restored": False,
                    "runs": [], "closed": False})
    print(wid)


def cmd_fetch_bundle(env, args) -> None:
    sha = args.sha
    subprocess.run(["git", "fetch", "-q", "origin", "dev"], check=True)
    if subprocess.run(["git", "merge-base", "--is-ancestor", sha, "origin/dev"], check=False).returncode != 0:
        raise StepError(f"{sha} is not on origin/dev")
    listing = subprocess.run(
        ["gh", "run", "list", "-R", REPO, "--workflow", "ci.yml", "--branch", "dev", "--event", "push", "--limit", "50",
         "--json", "databaseId,headSha,event,headBranch,conclusion"], check=True, capture_output=True, text=True)
    run_id = pick_run(json.loads(listing.stdout), sha)
    dest = bundle_dir(env, sha)
    if dest.exists():
        raise StepError(f"{dest} exists")
    subprocess.run(["gh", "run", "download", str(run_id), "-R", REPO, "-n", f"asio-spike-{sha}", "-D", str(dest)], check=True)
    files = verify_bundle(dest)
    (dest.parent / f"{sha}.source-sha").write_text(sha + "\n", encoding="utf-8")
    print(json.dumps({"bundle": str(dest), "run": run_id, "files": files}))


def cmd_setup(env, args) -> None:
    state = open_state()
    bundle = bundle_dir(env, args.sha)
    if (bundle.parent / f"{args.sha}.source-sha").read_text(encoding="utf-8").strip() != args.sha:
        raise StepError("bundle .source-sha differs from --sha (P5: only the reviewed dev commit's bundle)")
    verify_bundle(bundle)
    dirs = ", ".join(pc(env, d) for d in ("bin", "queue", "status"))
    guarded(ssh_cmd(env), f"New-Item -ItemType Directory -Force -Path {dirs} | Out-Null\n", 60, "finish")
    for name in (*BUNDLE_FILES, "SHA256SUMS"):
        scp(str(bundle / name), remote(env, f"bin/{name}"))
    names = ps(env, f"$n = Test-SpikeSums -Bin {pc(env, 'bin')} ; Register-SpikeTask -Root {ps_quote(env['PC_ROOT'])} ; $n")
    update_state({"bundle_sha": args.sha})
    print(json.dumps({"setup": args.sha, "verified": names, "task": "registered"}))


def cmd_preflight(env, args) -> None:
    state = open_state()
    dev_time = bool(state.get("dev_time"))
    if dev_time:
        early = state["card"] == "free" and state["pref_current"] is None and not state["runs"]
    else:
        early = state["card"] == "reaper"
    if not early:
        raise StepError("preflight belongs before to-dev (a dev-time window: before any set-buffer or run)")
    r = ps(env, " ; ".join([
        f"$p = Get-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])}",
        f"$h = Get-GoldenAsioHolders -Module {ps_quote(env['PC_ASIO_MODULE'])}",
        f"$s = Test-SpikeSums -Bin {pc(env, 'bin')}",
        "[pscustomobject]@{ pref = $p.value; kind = $p.kind; raw = $p.raw; holders = @($h); "
        "reaper = @(Get-Process reaper -ErrorAction SilentlyContinue).Count; "
        f"app = @(Get-Process -Name {ps_quote(env['PC_APP_PROCESS'])} -ErrorAction SilentlyContinue).Count; "
        "spike = @(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count; "
        f"task = [bool](Get-ScheduledTask {TASK} -ErrorAction SilentlyContinue); files = @($s).Count }}",
    ]), timeout=120, event="abandon")
    problems = preflight_problems(r, state["pref_original"], dev_time)
    if problems:
        raise StepError("; ".join(problems))
    update_state({"preflight": r})
    print(json.dumps({"preflight": r}))


def cmd_to_dev(env, args) -> None:
    state = open_state()
    if state["card"] != "reaper" or "preflight" not in state:
        raise StepError("run preflight first (REAPER must hold the card)")
    texts = ps(env, f"Get-GoldenMeterSamples -Http {ps_quote(env['PC_REAPER_HTTP'])} -Seconds 60", timeout=180, event="abandon")
    hits = interlock_hits([parse_meter_peaks(t) for t in texts])
    if hits:
        raise StepError(f"band activity: peaks above -50 dBFS on tracks {sorted(hits)}; no switch, alarm the owner")
    update_state({"card": "switching"})
    r = ps(env, f"Invoke-GoldenSaveQuit -Http {ps_quote(env['PC_REAPER_HTTP'])} -Project {ps_quote(env['PC_MAIN_PROJECT'])} -AsioModule {ps_quote(env['PC_ASIO_MODULE'])}", timeout=120)
    update_state({"card": "free"})
    print(json.dumps({"to-dev": r, "app": "kept running"}))


def cmd_set_buffer(env, args) -> None:
    state = open_state()
    need_preflight(state)
    if state["card"] != "free":
        raise StepError("the card is not free (run to-dev)")
    if args.frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if spike_running(env):
        raise StepError("a spike runs")
    update_state({"pref_current": args.frames, "pref_restored": False})   # recorded before the write: preempt restores
    r = ps(env, f"Set-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])} -Value {args.frames} -Original {state['pref_original']}")
    print(json.dumps({"set-buffer": r}))


def cmd_run(env, args, on_poll=None) -> dict:
    state = open_state()
    need_preflight(state)
    if state["card"] != "free":
        raise StepError("the card is not free (run to-dev)")
    check_request(args.mode, args.frames, args.seconds, args.burn_us, args.stress, args.cycles,
                  getattr(args, "cpu", None), getattr(args, "threshold_us", 10),
                  getattr(args, "audio_cpus", "") or "", getattr(args, "stress_cpus", "") or "")
    current = state["pref_current"] if state["pref_current"] is not None else state["pref_original"]
    if args.mode in ("duplex", "reopen") and args.frames != current:
        raise StepError(f"the driver's preferred buffer is {current}: run set-buffer --frames {args.frames} first")
    fields = run_fields(env, args)
    rid = ps(env, f"Remove-Item -LiteralPath {pc(env, 'queue/stop')} -ErrorAction SilentlyContinue ; "
                  f"$id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'spike' -Fields {ps_hashtable(fields)} ; Start-SpikeTask ; $id")
    update_state(change=lambda st: st["runs"].append({"request": rid, **fields}))
    status_path, progress_path = pc(env, f"status/{rid}.json"), pc(env, f"status/{rid}.progress.json")
    watch = (f"$s = {status_path} ; $p = {progress_path} ; [pscustomobject]@{{ "
             "status = $(if (Test-Path -LiteralPath $s) { Get-Content -LiteralPath $s -Raw | ConvertFrom-Json } else { $null }); "
             "progress = $(if (Test-Path -LiteralPath $p) { Get-Content -LiteralPath $p -Raw | ConvertFrom-Json } else { $null }) }")
    deadline = time.monotonic() + fields["timeout"] + 120
    next_pc = 0.0
    while True:
        if event_now():
            raise EventNow()
        if time.monotonic() >= next_pc:
            next_pc = time.monotonic() + 10
            st = ps(env, watch, timeout=60, event="abandon")
            if on_poll is not None:
                on_poll(st)
            if st.get("progress"):
                print(json.dumps({"progress": st["progress"]}), flush=True)
            phase = (st.get("status") or {}).get("state")
            if phase in ("exited", "failed", "refused"):
                break
            if time.monotonic() > deadline:
                ps(env, f"New-Item -ItemType File -Force -Path {pc(env, 'queue/stop')} | Out-Null ; 'stop'", timeout=60)
                raise StepError("the spike has not exited: stop file written; watch it, never kill; alarm the owner if it stays")
        time.sleep(POLL_S)
    if phase != "exited":
        raise StepError(f"spike request {phase}: {json.dumps(st['status'].get('results'))}")
    out = raw_dir(env, state)
    scp(remote(env, f"status/{rid}.report.json"), str(out / f"{rid}.report.json"))
    scp(remote(env, f"status/{rid}.stderr.txt"), str(out / f"{rid}.stderr.txt"))
    report = json.loads((out / f"{rid}.report.json").read_text(encoding="utf-8"))
    v = verdict(report) if args.mode != "hwlat" else None
    update_state(change=lambda st: next(r for r in st["runs"] if r.get("request") == rid).update(verdict=v))
    code = st["status"]["results"][0]["exit"]
    print(json.dumps({"run": rid, "exit": code, "verdict": v}), flush=True)
    if code in ALARMS:
        print(f"OWNER ALARM: {ALARMS[code]}", file=sys.stderr, flush=True)
    # Outcome "error" (exit 1): the spike could not set up what was asked (a stress
    # thread or the hwlat scanner not placed or raised), so nothing it reports
    # measured the requested setup — a failed step, never a result.
    if report.get("outcome") == "error":
        raise StepError(f"spike request {rid} failed (exit {code}): {report.get('error') or 'no error text'} "
                        f"(report {out / f'{rid}.report.json'})")
    return {"run": rid, "exit": code, "verdict": v, "report": str(out / f"{rid}.report.json")}


def tuning_body(env: dict[str, str], body: str) -> str:
    """A PC body that loads the S1c modules from the verified bundle first."""
    for k in ("PC_TUNING_ROOT", "PC_XPERF"):
        if not env.get(k):
            raise StepError(f"{k} missing in the private env (S1c plan Task 12)")
    return (f"Import-Module (Join-Path {ps_quote(env['PC_ROOT'])} 'bin\\IemMeasure.psm1') -Force -Global ; {body}")


def tuning_profile(env: dict[str, str]) -> str:
    return ps_quote(env["PC_TUNING_ROOT"] + "\\profile.json")


# Every trace stop (#32 MAJOR-1, MINOR-1, F2 round 3 item 4): IemMeasure in its
# stop-only mode — IemTuning is not imported, so no Add-Type compile runs and the
# stop depends neither on the tuning modules loading nor on PC_TUNING_ROOT or
# PC_XPERF — and Stop-IemTraceSessions with the trace's run folder, which decides
# whose kernel trace it is. It makes at most four logman calls (the session list,
# the kernel logger's query, two stops), each bounded on the PC at
# TRACE_STOP_LOGMAN_S plus a 5 s output read, inside the TRACE_STOP_CALL_S bound
# of the ssh call.
TRACE_STOP_LOGMAN_S = 20
TRACE_STOP_CALL_S = 120


def trace_stop_import(env: dict[str, str]) -> str:
    return f"Import-Module (Join-Path {ps_quote(env['PC_ROOT'])} 'bin\\IemMeasure.psm1') -ArgumentList 'stop-only' -Force -Global"


def trace_stop_call(trace_dir: str) -> str:
    return f"Stop-IemTraceSessions -Dir {ps_quote(trace_dir)} -TimeoutSeconds {TRACE_STOP_LOGMAN_S}"


def trace_stop_body(env: dict[str, str], trace_dir: str) -> str:
    """The trace stop as one PC body; its reply goes through check_trace_stop.
    Idempotent: with nothing running it stops nothing and succeeds."""
    return f"{trace_stop_import(env)} ; {trace_stop_call(trace_dir)}"


def check_trace_stop(reply) -> dict:
    """A trace stop's reply (Stop-IemTraceSessions): the PC throws on any error
    and on a kernel logger that runs but is not ours (its output file is not
    under the trace's run folder); a reply that is no stop result, or that
    names a kept session, fails here too. Only a confirmed stop clears
    state["trace"]; any other keeps it recorded (#32 MAJOR-1)."""
    if not isinstance(reply, dict) or not isinstance(reply.get("stopped"), list) or reply.get("kept") != []:
        raise StepError(f"the trace stop is not confirmed (reply {json.dumps(reply)})")
    return reply


def bring_back(env: dict[str, str], state: dict) -> dict:
    """REAPER through our own start task (only if it does not run), then the
    S1a handover checks."""
    return ps(env, "Invoke-SpikeBringBack " + " ".join([
        f"-Http {ps_quote(env['PC_REAPER_HTTP'])}",
        f"-StartTaskPath {ps_quote(env['PC_REAPER_START_TASK_PATH'])} -StartTask {ps_quote(env['PC_REAPER_START_TASK'])}",
        f"-NTrack {int(env['PC_NTRACK'])} -BridgeState {ps_quote(env['PC_METER_BRIDGE'])}",
        f"-BridgeAction {ps_quote(env['PC_METER_ACTION'])} -Heartbeat {ps_quote(env['PC_METER_HEARTBEAT'])}",
        f"-AsioModule {ps_quote(env['PC_ASIO_MODULE'])} -AppHttp {ps_quote(env['PC_APP_HTTP'])}",
        f"-BufferKey {ps_quote(env['PC_BUFFER_KEY'])} -BufferName {ps_quote(env['PC_BUFFER_NAME'])} {buffer_args(state)}",
    ]), timeout=240, event="ignore")


def fingerprint_diff(baseline: dict, current: dict) -> list[dict]:
    keys = sorted(set(baseline) | set(current))
    return [{"key": k, "baseline": baseline.get(k, "<absent>"), "current": current.get(k, "<absent>")}
            for k in keys if str(baseline.get(k, "<absent>")) != str(current.get(k, "<absent>"))]


def alarm(text: str) -> None:
    print(f"OWNER ALARM: {text}", file=sys.stderr, flush=True)


def alarm_exit_problems(rows) -> None:
    """Exit-IemTuningMode reports a schema-1 journal's global-section entry it
    could not convert as a row with action "problem"; the exit itself completed.
    The owner hears which entries (cmd_exit and the unwind's tuning-exit step)."""
    listed = rows if isinstance(rows, list) else [] if rows is None else [rows]
    problems = [r for r in listed if isinstance(r, dict) and r.get("action") == "problem"]
    if problems:
        alarm(f"the tuning-mode exit completed, but {len(problems)} journal entry(ies) could not be converted: "
              + "; ".join(f"{r.get('key')}: {r.get('error')}" for r in problems)
              + "; check them on the PC before the next S1c window")


def unwind(env: dict[str, str], state: dict, running: bool, bring_back_reaper: bool = True) -> list:
    """Stop the spike, stop a trace, revert the S1c mode levers, restore the
    buffer (read back), bring REAPER back (it reads the buffer again and
    refuses while a spike or its task runs), compare the fingerprint. A failed
    trace stop, tuning exit or fingerprint alarms the owner and never holds
    REAPER back (S1c design note §5.2). Without bring_back_reaper (before an
    approved reboot) the card stays free and the window stays open. A spike
    not confirmed gone may still hold the driver: the trace stop and the mode
    exit still run (neither touches the driver), but the unwind alarms and
    stops before the buffer write and the bring-back (I3; set-buffer refuses
    the same write), leaving the window open for a later preempt. It runs
    under the window lock: callers read `state` under the same lock, so no
    other process changes it meanwhile and one bring-back is all there is."""
    with window_lock():
        return _unwind(env, state, running, bring_back_reaper)


def _unwind(env: dict[str, str], state: dict, running: bool, bring_back_reaper: bool) -> list:
    done = []
    gone = True
    for step in undo_plan(state, running):
        if step in ("restore-buffer", "bring-back") and not gone:
            alarm("the spike did not stop within 60 s and may still hold the driver: no buffer write and no REAPER "
                  "start (I3); the last resort is the owner's reboot, which comes back in event mode")
            raise StepError("the spike did not stop within 60 s: the driver's preferred buffer is not written and REAPER "
                            "cannot start (I3); alarm the owner now")
        if step == "stop-spike":
            gone = bool(ps(env, f"(Stop-SpikeGracefully -Root {ps_quote(env['PC_ROOT'])} -Seconds 60).gone", timeout=120, event="ignore"))
            done.append({"stop-spike": gone})
        elif step == "trace-stop":
            try:
                r = check_trace_stop(ps(env, trace_stop_body(env, state["trace"]), timeout=TRACE_STOP_CALL_S, event="ignore"))
                state["trace"] = None
                save_state(state)
                done.append({"trace-stop": r})
            except StepError as e:
                # The trace stays recorded: trace-stop or the next preempt retries it.
                alarm(f"the kernel trace did not stop ({e}); it stays recorded in the window: tuning_window trace-stop retries it")
                done.append({"trace-stop": {"error": str(e)}})
        elif step == "tuning-exit":
            try:
                r = ps(env, tuning_body(env, f"Exit-IemTuningMode -ProfilePath {tuning_profile(env)}"), timeout=240, event="ignore")
                state["tuning_mode"] = False
                save_state(state)
                done.append({"tuning-exit": r})
                alarm_exit_problems(r)
            except StepError as e:
                alarm(f"the S1c mode levers were not all reverted ({e}); REAPER still comes back")
                done.append({"tuning-exit": {"error": str(e)}})
        elif step == "restore-buffer":
            r = ps(env, f"Set-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])} "
                        f"-Value {state['pref_original']} {buffer_args(state)}", event="ignore")
            state["pref_current"], state["pref_restored"] = state["pref_original"], True
            save_state(state)
            done.append({"restore-buffer": r})
        elif step == "bring-back":
            if not bring_back_reaper:
                break
            r = bring_back(env, state)
            state["card"] = "reaper"
            save_state(state)
            done.append({"bring-back": r})
        elif step == "fingerprint":
            try:
                current = ps(env, tuning_body(env, f"Get-IemReaperFingerprint -ProfilePath {tuning_profile(env)}"), timeout=120, event="ignore")
                diff = fingerprint_diff(json.loads(Path(state["fingerprint"]).read_text(encoding="utf-8")), current)
                if diff:
                    alarm(f"REAPER mode differs from the baseline: {json.dumps(diff)}")
                done.append({"fingerprint": diff})
            except (StepError, OSError, ValueError) as e:
                alarm(f"the fingerprint could not be read ({e})")
                done.append({"fingerprint": {"error": str(e)}})
    if bring_back_reaper:
        state["closed"] = True
        save_state(state)
    return done


def cmd_to_event(env, args) -> None:
    with window_lock():   # the state is read where no other unwind or bring-back runs
        state = open_state()
        if spike_running(env):
            raise StepError("a spike runs: wait for it, or preempt")
        print(json.dumps({"to-event": unwind(env, state, running=False)}))


def cmd_preempt(env, args=None) -> None:
    """Brings REAPER back once, whoever asks: under the window lock the state
    is read again, and a window another process already closed (REAPER back)
    is left alone."""
    with window_lock():
        state = load_state()
        if state.get("closed"):
            print(json.dumps({"preempt": state["id"], "plan": [], "note": "window already closed"}))
            return
        running = spike_running(env, event="ignore")
        print(json.dumps({"preempt": state["id"], "plan": undo_plan(state, running)}), flush=True)
        state["preempted"] = True
        print(json.dumps({"done": unwind(env, state, running)}))


# Steps inside an open window: an error in one while the flag exists pre-empts.
WINDOW_STEPS = ("setup", "preflight", "to-dev", "set-buffer", "run", "to-event")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    sub = ap.add_subparsers(dest="cmd", required=True)
    new = sub.add_parser("new")
    new.add_argument("--signal", required=True)
    new.add_argument("--dev-time", action="store_true", help="REAPER is already saved and quit: the card is free from the start")
    for name in ("fetch-bundle", "setup"):
        sub.add_parser(name).add_argument("--sha", required=True)
    for name in ("preflight", "to-dev", "to-event", "preempt", "status"):
        sub.add_parser(name)
    sub.add_parser("set-buffer").add_argument("--frames", type=int, required=True)
    run = sub.add_parser("run")
    run.add_argument("--mode", required=True, choices=("probe", "duplex", "reopen", "hwlat"))
    run.add_argument("--frames", type=int)
    run.add_argument("--seconds", type=int, default=600)
    run.add_argument("--burn-us", type=int, default=0)
    run.add_argument("--stress", type=int, default=0)
    run.add_argument("--panic-at", type=int, default=0)
    run.add_argument("--cycles", type=int, default=5)
    run.add_argument("--cpu", type=int)
    run.add_argument("--threshold-us", type=int, default=10)
    run.add_argument("--audio-cpus", default="")
    run.add_argument("--stress-cpus", default="")
    args = ap.parse_args(argv)
    if args.cmd == "status":
        print(json.dumps({"event_now": event_now(), "state": load_state() if STATE.is_file() else None}, indent=1))
        return 0
    handlers = {"new": cmd_new, "fetch-bundle": cmd_fetch_bundle, "setup": cmd_setup, "preflight": cmd_preflight,
                "to-dev": cmd_to_dev, "set-buffer": cmd_set_buffer, "run": cmd_run, "to-event": cmd_to_event, "preempt": cmd_preempt}
    try:
        env = load_env(Path(os.environ.get("SPIKE_ENV", str(Path.home() / ".config/iemmixer/asio-spike.env"))))
    except StepError as e:
        print(f"spike_window: {e}", file=sys.stderr)
        return 1
    try:
        handlers[args.cmd](env, args)
        return 0
    except EventNow:
        print(json.dumps({"event": "ide event (flag file)", "action": "preempt"}), flush=True)
    except StepError as e:
        print(f"spike_window: {e}", file=sys.stderr, flush=True)
        # A hung save/quit or an ssh error while "ide event" is on must still bring REAPER back.
        if args.cmd not in WINDOW_STEPS or not event_now():
            return 1
        print(json.dumps({"event": "ide event (flag file) after a failed step", "action": "preempt"}), flush=True)
    try:
        cmd_preempt(env)
    except StepError as e:
        print(f"spike_window: preempt: {e}", file=sys.stderr)
        return 1
    return 10


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
