#!/usr/bin/env python3
"""S1a ASIO-spike window driver on the dev box (design note §5); also the
interim switch between REAPER and iemmixer until S6's `iemmode`.

A window opens only with the owner's quoted "event skončil" and never while
the "ide event" flag file exists. Every wait checks that flag every 2 s; when
it appears the driver stops the spike through its stop file, restores the
driver's preferred buffer (read back) and brings REAPER back with the handover
checks (`preempt`), then watches a PC change still in flight until it is
over (settle); a step that fails while the flag exists pre-empts too
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
from golden_window import StepError, check_signal, ps_quote  # noqa: E402

# Run as a script this module is __main__: it is registered under its own name
# too, so pc_change (which reaches the state, the lock and the PC through it)
# finds this very module, never a second copy with a lock state of its own.
sys.modules.setdefault("spike_window", sys.modules[__name__])
# The PC-change protocol (F2 round 3 MAJOR), its own module next to this one;
# the window tools use it through this module.
from pc_change import (JOURNAL_STEPS, REAPER_STEPS, RUN_START_S, SAVE_QUIT_S, SET_BUFFER_S, STEP_REFUSED, STOP_FILE,  # noqa: E402,F401
                       begin_change, changing, clear_stop, close_out, end_change, exit_on_signals, intent_live, pc_change,
                       reaper_on_card, settle_live, step_guard, unwind_closing, wait_for_settle)
import elevated_ps  # noqa: E402  (TEMP before the tuning modules' import, #15)
# The pure rules (env, request, undo plan, sums, verdict), re-exported.
from spike_rules import (CPU_LIST, FRAMES, MAX_SECONDS, REQUIRED, buffer_args, buffer_touched,  # noqa: E402,F401
                         check_request, cpu_set, load_env, parse_sums, pick_run,
                         ps_hashtable, run_fields, run_timeout, undo_plan, verdict)

POLL_S = 2.0
REPO = "zbynekdrlik/iemmixer"
BUNDLE_FILES = ("GoldenPc.psm1", "IemMeasure.psm1", "IemTuning.psm1", "SpikePc.psm1", "asio_spike.exe", "spike-task.ps1")
TASK = "-TaskPath '\\iemmixer\\' -TaskName 'iemmixer-asio-spike'"
STATE = Path(os.environ.get("SPIKE_STATE", str(Path.home() / ".local/state/iemmixer/spike-window.json")))
# Spike exit codes the owner must hear about at once (crates/iem-audio-io/examples/asio_spike/main.rs).
ALARMS = {
    8: "a callback did not leave the stream within the stop wait (R6): tell the owner now, no further run",
}
EVENT_NOW = Path(os.environ.get("IEMMIXER_EVENT_NOW", str(Path.home() / ".config/iemmixer/EVENT-NOW")))


class EventNow(Exception):
    """The owner said "ide event" (the flag file exists): pre-empt."""


# ssh's own lines when it exits 255 before any session existed: nothing was
# sent. Each is anchored to its pre-session shape (review round 3, m8): the
# connect phase (refused, timed out, no route, network unreachable), the name
# lookup, the auth refusal and the host key. The same words anywhere else (the
# network dropping mid-call, a remote program's error) leave the PC's fate
# unknown: NoReply.
SSH_NOT_CONNECTED = re.compile(r"(?im)^(?:ssh: connect to host \S+ port \d+: .+"
                               r"|ssh: could not resolve hostname \S+: .+"
                               r"|\S+: permission denied \([\w,-]+\)\.?"
                               r"|host key verification failed\.)\s*$")


class NoReply(StepError):
    """A PC call was sent but gave no reply: the ssh session ended (a non-zero
    exit), the call outlived its bound, or its output held no complete JSON
    reply. What happened on the PC is unknown — unlike a reply that reports
    an error (a plain StepError)."""


def event_now() -> bool:
    return EVENT_NOW.exists()


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


def verify_bundle(bundle: Path) -> list[str]:
    sums = parse_sums((bundle / "SHA256SUMS").read_text(encoding="utf-8"))
    if sorted(sums) != sorted(BUNDLE_FILES):
        raise StepError(f"bundle lists {sorted(sums)}, expected {sorted(BUNDLE_FILES)}")
    for name, sha in sums.items():
        if hashlib.sha256((bundle / name).read_bytes()).hexdigest() != sha:
            raise StepError(f"bundle file {name} does not match SHA256SUMS")
    return sorted(sums)


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
def window_lock(wait_s: float | None = None) -> Iterator[None]:
    """Holds the window lock. A holder that does not let go within `wait_s`
    (default LOCK_WAIT_S) is an owner alarm and a StepError, never a wait
    forever."""
    wait_s = LOCK_WAIT_S if wait_s is None else wait_s
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
        deadline = time.monotonic() + wait_s
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() > deadline:
                    alarm(f"the window lock {path} was not free within {wait_s:g} s: another window process "
                          "holds it (an unwind or a bring-back?); check it before acting by hand")
                    raise StepError(f"the window lock was not free within {wait_s:g} s") from None
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


def update_state(fields: dict | None = None, change: Callable[[dict], None] | None = None,
                 wait_s: float | None = None) -> dict:
    """One read-modify-write of the state as saved NOW, under the window lock
    (waited for at most `wait_s`, default LOCK_WAIT_S): `fields` merged in,
    then `change` applied. Returns the saved state."""
    with window_lock(wait_s):
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
    # A window that ended without a closing unwind (its process died, a deadline stop) left the
    # spike's stop file, and every changing step of this window would refuse on it. No spike runs
    # (checked above), so it is stale: removed here under the lock, never while "ide event" holds
    # the flag (the event path owns it then). clear_stop keeps it while the spike or its task runs.
    stop = 'kept: the "ide event" flag exists' if event_now() else clear_stop(env)
    if stop.startswith("error"):
        raise StepError(f"the spike's stop file left by an earlier window could not be removed ({stop})")
    update_state({"preflight": dict(r, stop_file=stop)})
    print(json.dumps({"preflight": r, "stop_file": stop}))


def need_reaper_card(state: dict) -> None:
    if state["card"] != "reaper" or "preflight" not in state:
        raise StepError("run preflight first (REAPER must hold the card)")


def need_free_card(state: dict) -> None:
    need_preflight(state)
    if state["card"] != "free":
        raise StepError("the card is not free (run to-dev)")


def late_quit(env: dict[str, str]) -> Callable[[dict, bool], None]:
    """A save and quit that ended after the window closed. While the preempt's
    settle still watches (its record is live) that watch covers it: an alarm
    here would send the owner to run the event path again, next to the
    settle's own bring-back (review of lane G2, finding 1). Without a settle
    (the window closed after a failed preempt, or the settle's bound is over)
    REAPER is read here and the owner alarmed when it is off the card. A quit
    the PC refused touched nothing."""
    def check(state: dict, ran: bool) -> None:
        if not ran or settle_live(state):
            return
        if not reaper_on_card(env):
            alarm("REAPER is not on the card after a save and quit that ended after the pre-emption: run the event "
                  "path again (iempc event, or the interim switch) so REAPER comes back")
    return check


def late_buffer(env: dict[str, str]) -> Callable[[dict, bool], None]:
    """A buffer write that ended after the window closed: the preempt restored
    the original before the bring-back, so the value is read again; it is
    never written here, REAPER may hold the driver (I2). A write the PC
    refused wrote nothing."""
    def check(state: dict, ran: bool) -> None:
        if not ran:
            return
        r = ps(env, f"Get-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])}",
               timeout=60, event="ignore")
        if (r or {}).get("value") != state["pref_original"]:
            alarm(f"the driver's preferred buffer reads {(r or {}).get('value')} after a set-buffer that ended after the "
                  f"pre-emption, not the original {state['pref_original']}: it is not written while REAPER may hold the "
                  "driver (I2); tell the owner")
    return check


def cmd_to_dev(env, args) -> None:
    need_reaper_card(open_state())
    # No stage reading (#38, owner 2026-10-06): the owner's "event skončil" opened
    # the window, and only his signal decides whether the PC may change; other
    # devices on the Dante network feed the card's inputs, so a level proves nothing.
    # "switching" is recorded with the intent, before the save and quit: a preempt
    # meanwhile brings REAPER back (card away) and settles until the quit is over.
    body = (f"Invoke-GoldenSaveQuit -Http {ps_quote(env['PC_REAPER_HTTP'])} -Project {ps_quote(env['PC_MAIN_PROJECT'])} "
            f"-AsioModule {ps_quote(env['PC_ASIO_MODULE'])}")
    r = pc_change("to-dev", SAVE_QUIT_S, lambda: ps(env, changing(env, body), timeout=SAVE_QUIT_S),
                  fields={"card": "switching"}, after={"card": "free"}, check=need_reaper_card, late=late_quit(env))
    print(json.dumps({"to-dev": r, "app": "kept running"}))


def cmd_set_buffer(env, args) -> None:
    state = open_state()
    need_free_card(state)
    if args.frames not in FRAMES:
        raise StepError(f"--frames must be one of {FRAMES}")
    if spike_running(env):
        raise StepError("a spike runs")
    # pref_current is recorded with the intent, before the write: a preempt restores it.
    body = (f"Set-SpikeBufferPref -Key {ps_quote(env['PC_BUFFER_KEY'])} -Name {ps_quote(env['PC_BUFFER_NAME'])} "
            f"-Value {args.frames} -Original {state['pref_original']}")
    r = pc_change("set-buffer", SET_BUFFER_S, lambda: ps(env, changing(env, body), timeout=SET_BUFFER_S),
                  fields={"pref_current": args.frames, "pref_restored": False}, check=need_free_card, late=late_buffer(env))
    print(json.dumps({"set-buffer": r}))


def cmd_run(env, args, on_poll=None) -> dict:
    state = open_state()
    need_free_card(state)
    check_request(args.mode, args.frames, args.seconds, args.burn_us, args.stress, args.cycles,
                  getattr(args, "cpu", None), getattr(args, "threshold_us", 10),
                  getattr(args, "audio_cpus", "") or "", getattr(args, "stress_cpus", "") or "")
    current = state["pref_current"] if state["pref_current"] is not None else state["pref_original"]
    if args.mode in ("duplex", "reopen") and args.frames != current:
        raise StepError(f"the driver's preferred buffer is {current}: run set-buffer --frames {args.frames} first")
    fields = run_fields(env, args)
    # The start never removes the stop file and refuses while it exists (F2 round 3,
    # m1): only the unwind that closes the window removes it (clear_stop).
    body = f"$id = Write-GoldenRequest -Root {ps_quote(env['PC_ROOT'])} -Kind 'spike' -Fields {ps_hashtable(fields)} ; Start-SpikeTask ; $id"
    rid = pc_change("run", RUN_START_S, lambda: ps(env, changing(env, body), timeout=RUN_START_S), check=need_free_card)
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


def measure_import(root: str) -> str:
    """The import of the S1c modules from the verified bundle (IemMeasure loads
    IemTuning, whose Add-Type compiles), TEMP and TMP first at the admin-only
    <elevated root>\\temp: csc writes and loads its DLL there, never in the
    session user's TEMP (#15, elevated_ps.temp_first)."""
    return f"{elevated_ps.temp_first()} ; Import-Module (Join-Path {ps_quote(root)} 'bin\\IemMeasure.psm1') -Force -Global"


def tuning_body(env: dict[str, str], body: str) -> str:
    """A PC body that loads the S1c modules from the verified bundle first."""
    for k in ("PC_TUNING_ROOT", "PC_XPERF"):
        if not env.get(k):
            raise StepError(f"{k} missing in the private env (S1c plan Task 12)")
    return f"{measure_import(env['PC_ROOT'])} ; {body}"


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
            intent = state.get("in_flight")
            if intent_live(intent) and intent.get("step") in JOURNAL_STEPS:
                # Another process's enter/exit/apply/undo still writes the tuning
                # journal: two writers at once lose entries, so that step runs the
                # exit itself once its call is back (its late handler, MAJOR).
                done.append({"tuning-exit": {"deferred": f"{intent['step']} is in flight: it runs the exit when its call is back"}})
                continue
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
        intent = state.get("in_flight")
        done = unwind_closing(env, state, running=False)
    # The window is closed with REAPER back; a change of another process still in
    # flight is watched out without the lock (settle), then the stop file goes.
    print(json.dumps({"to-event": done}), flush=True)
    close_out(env, intent, done)


def cmd_preempt(env, args=None) -> None:
    """Brings REAPER back once, whoever asks: under the window lock the state
    is read again, and a window another process already closed (REAPER back)
    is left alone. A PC change in flight is settled after the bring-back, and
    the stop file removed once the window closed (close_out)."""
    with window_lock():
        state = load_state()
        closed = bool(state.get("closed"))
        if not closed:
            running = spike_running(env, event="ignore")
            intent = state.get("in_flight")
            print(json.dumps({"preempt": state["id"], "plan": undo_plan(state, running), "in_flight": intent}), flush=True)
            state["preempted"] = True
            done = unwind_closing(env, state, running)
    if closed:
        print(json.dumps({"preempt": state["id"], "plan": [], "note": "window already closed"}), flush=True)
        wait_for_settle()   # the process that closed it may still settle a PC change
        return
    print(json.dumps({"done": done}), flush=True)
    close_out(env, intent, done)


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
    exit_on_signals()
    sys.exit(main(sys.argv[1:]))
