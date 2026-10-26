"""`iempc event`, "ide event" on the dev box (S6 design note §5.5; split out
of iempc.py, #36): the flag first (`write_flag`; a flag it cannot write is a
warning, never a stop), the spike preempt of an open S1a/S1c window or of a
closed one still settling (`spike_preempt`), the window closed under its lock
after a failed preempt (`close_failed_window`), a kernel trace a dead `iempc
trace` left recorded stopped (iempc_trace.stop_recorded), then `iemmode
event --signal` (the owner's "ide event", which in prod keeps the band's
system; S8 lane 3), and `iemmode event --direct` when the guard is unreachable (exit 4):
all within one budget (EVENT_BUDGET_S) on one clock (`event_clock`). Right after
the flag, beside all of that, the Gen 3 device's service is stopped (#72,
`start_gen3_stop`/`end_gen3_stop`): one ssh to GEN3_HOST in a session of its
own, its output in the state folder's gen3-stop.log, waited for at most
GEN3_STOP_S at the end, a warning when it fails; neither function raises, so
it never stops, delays or changes the event path or its exit code.
iempc.py's docstring states the whole rule.

iempc.py passes itself in (`ip`) for the siblings. The names a test patches
here (SPIKE, EVENT_BUDGET_S, SPIKE_SHARE_S, SWITCH_MIN_S, GEN3_STOP_S,
`event_clock`, `ensure_flag`) live in this module only; the shared ones are
read as `core.<name>` (iempc_core.py)."""
from __future__ import annotations

import math
import os
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO

import iempc_core as core
import iempc_trace
from iempc_core import (SPIKE_DIR, Ctx, StepError, StillRunning, emit, event_now, guarded, iemmode, result,
                        spike_module, spike_window_open, spike_window_settling)

SPIKE = SPIKE_DIR / "spike_window.py"
GUARD_UNREACHABLE = 4
USAGE = 2
# The owner's "ide event" (S8 lane 3): in prod with it the guard keeps the band's
# system (live stays, maintenance ends live on the pin); without it `iemmode
# event` is the event plan in every lifecycle (no rollback, #11).
SIGNAL = "--signal"
# What an iemmode older than S8 lane 3 says to it (a usage error, exit 2, before
# any guard call). It then gets the plain event, the event plan in every lifecycle
# (in prod REAPER for this event, the PC stays prod): never a silent event.
UNKNOWN_SIGNAL = 'unknown argument "--signal"'
# The event path, all of it: one Bash call ends at 10 min, the plan's waits stay within 9.
EVENT_BUDGET_S = 540
# spike_window.py preempt's part of it (its bring-back starts REAPER itself).
SPIKE_SHARE_S = 360
# An iemmode call of the event path never starts with less than this left.
SWITCH_MIN_S = 120
OWNER_ALARM = ("iempc: the event path did not complete: alarm the owner now with the prepared question (ops runbook "
               "docs/s6-pc-runbook.md); before the guard is installed, the interim switch (event runbook) applies; the "
               "last resort is the owner's reboot, which comes back in event mode. Never force-end anything.")
# "ide event" also takes the Gen 3 device off the network (#72, design §5.5): its
# service on the box (GEN3_HOST in the private env, an ssh destination) stopped by
# one ssh started right after the flag, beside the PC's switch, and waited for at
# most GEN3_STOP_S at the end of the path; past it the ssh is left to end by itself.
GEN3_STOP_S = 5.0
GEN3_STOP = ("sudo", "-n", "systemctl", "stop", "iem-server", "iem-engine", "iem-dante")
# The stop's output (stdout and stderr) goes into this log in iempc's state
# folder, never into pipes that close when iempc ends while it runs on; each
# stop appends under a header line, and a log past GEN3_LOG_MAX is emptied
# when the next stop starts.
GEN3_LOG = "gen3-stop.log"
GEN3_LOG_MAX = 256 * 1024
# What a failed stop's warning quotes of its own output, at most.
GEN3_TAIL = 500


@dataclass
class Gen3Stop:
    """The Gen 3 device's stop, running: its ssh, the moment its bound ends,
    and its log with the offset its output starts at (None: no log)."""
    proc: subprocess.Popen
    deadline: float
    log: Path | None
    offset: int


def gen3_say(text: str) -> None:
    """One Gen 3 line on stderr; never raises (a closed stderr raises
    BrokenPipeError, a closed stream ValueError): the line is lost, the
    event path is not."""
    try:
        print(text, file=sys.stderr, flush=True)
    except Exception:   # airuleset:script-ok stderr itself failed, no channel is left for the line; the event path goes on
        return


def gen3_warning(text: str) -> None:
    gen3_say(f"iempc: WARNING: the Gen 3 device: {text}; the event path goes on")


def gen3_emit(doc: dict) -> None:
    """The Gen 3 stop's JSON line; one that cannot be written is a warning."""
    try:
        emit({"gen3_stop": doc})
    except Exception as e:   # nothing of the Gen 3 stop may reach the event path
        gen3_warning(f"its report could not be written ({e})")


def start_gen3_stop(env: dict[str, str]) -> Gen3Stop | None:
    """Starts the Gen 3 device's stop, never waiting for it. No GEN3_HOST: one
    info line, no ssh. Never raises (the event path's flag is written before
    it and the PC's switch runs after it whatever it does): a stop that cannot
    start, for whatever reason, is a warning."""
    try:
        return launch_gen3_stop(env)
    except Exception as e:   # Popen's ValueError (a NUL byte), its OSError, anything
        gen3_warning(f"its stop could not start ({e}): not stopped")
        return None


def launch_gen3_stop(env: dict[str, str]) -> Gen3Stop | None:
    host = env.get("GEN3_HOST", "")
    if not host:
        gen3_say("iempc: no GEN3_HOST in the private env: no Gen 3 device to stop")
        return None
    if host.startswith("-"):
        gen3_warning("GEN3_HOST must be an ssh destination, not an option: not stopped")
        return None
    bound = max(1, math.ceil(GEN3_STOP_S))
    cmd = ["ssh", "-o", "BatchMode=yes", "-o", f"ConnectTimeout={bound}", "-o", "ServerAliveInterval=1",
           "-o", f"ServerAliveCountMax={bound}", host, *GEN3_STOP]
    try:
        log, out, offset = open_gen3_log()
    except Exception as e:   # the log is no condition of the stop
        gen3_warning(f"its log could not be opened ({e}): its output is not kept")
        log, out, offset = None, None, 0
    try:
        # A session of its own: a hang-up of iempc's terminal never reaches a
        # stop left running.
        proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=out or subprocess.DEVNULL,
                                stderr=subprocess.STDOUT, start_new_session=True)
    finally:
        if out is not None:
            out.close()   # the ssh holds its own descriptor
    return Gen3Stop(proc, time.monotonic() + GEN3_STOP_S, log, offset)


def open_gen3_log() -> tuple[Path, BinaryIO, int]:
    """The log opened for appending, its header line written: the path, the
    file and the offset the stop's output starts at. Appending (O_APPEND), so
    a stop still running from an earlier event never writes over this one."""
    log = core.state_dir() / GEN3_LOG
    if log.is_file() and log.stat().st_size > GEN3_LOG_MAX:
        os.truncate(log, 0)
    out = open(log, "ab")
    try:
        out.write(f"--- {core.now_iso()} gen3 stop started\n".encode())
        out.flush()
        return log, out, out.tell()
    except BaseException:
        out.close()
        raise


def gen3_tail(stop: Gen3Stop) -> str:
    """This stop's output from its log, on one line, at most GEN3_TAIL characters."""
    if stop.log is None:
        return "(its output was not kept)"
    try:
        with open(stop.log, "rb") as f:
            f.seek(stop.offset)
            text = f.read().decode("utf-8", errors="replace")
    except OSError as e:
        return f"(its log {stop.log} could not be read: {e})"
    return " ".join(text.split())[-GEN3_TAIL:]


def end_gen3_stop(stop: Gen3Stop | None) -> None:
    """Waits for the stop until its bound (never longer) and reports it: one
    JSON line, and one warning when it failed or did not answer in time. A stop
    still running is left to end by itself. Never raises: the event's exit code
    is the PC's alone."""
    if stop is None:
        return
    try:
        doc = gen3_outcome(stop)
    except Exception as e:   # nothing of the Gen 3 stop may reach the event path
        gen3_warning(f"its stop could not be read ({e})")
        doc = {"ok": False, "error": str(e)[-500:]}
    gen3_emit(doc)


def gen3_outcome(stop: Gen3Stop) -> dict:
    """The stop's report, warning when it failed or did not answer in time."""
    try:
        # A floor, so a stop that already ended past the bound is still read.
        code = stop.proc.wait(timeout=max(stop.deadline - time.monotonic(), 0.1))
    except subprocess.TimeoutExpired:
        kept = f"its output goes to {stop.log}" if stop.log else "its output is not kept"
        gen3_warning(f"its stop did not answer within {GEN3_STOP_S:g} s and is left to end by itself ({kept}); "
                     "read 'systemctl is-active iem-server iem-engine iem-dante' on the box")
        return {"ok": False, "running": True}
    if code != 0:
        gen3_warning(f"its stop failed (exit {code}): {gen3_tail(stop)}")
    return {"ok": code == 0, "exit": code}


def ensure_flag() -> bool:
    """Writes the "ide event" flag (`date -Iseconds`) unless it exists; True when written."""
    if event_now():
        return False
    core.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
    try:
        with open(core.EVENT_NOW, "x", encoding="utf-8") as f:
            f.write(core.now_iso() + "\n")
    except FileExistsError:
        return False
    return True


def write_flag() -> None:
    """`event` writes the flag first; a flag it cannot write (no folder, a
    full or read-only disk) is a warning, never a stop of the event path."""
    try:
        written = ensure_flag()
    except OSError as e:
        print(f"iempc: WARNING: the flag {core.EVENT_NOW} was not written ({e}); the event path goes on; write the flag by "
              "hand so no dev-time command runs", file=sys.stderr, flush=True)
        emit({"flag": str(core.EVENT_NOW), "written": False, "error": str(e)})
        return
    if written:
        emit({"flag": str(core.EVENT_NOW), "written": True})


def event_clock() -> float:
    """The event path's one clock (its budget, each call's share); the tests
    run the path on a fake one, so no branch depends on this process's speed."""
    return time.monotonic()


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


def close_failed_window(deadline: float, error: str) -> None:
    """After a FAILED spike preempt (F2 round 3, m2 and decision 2): the window
    is closed under spike_window's lock before `iemmode event`, so a window
    preempt another process still has queued finds it closed and starts no
    second bring-back next to the guard's (one meter-bridge trigger, the #9
    lesson). The lock is waited for at most what the event budget leaves above
    the guard's SWITCH_MIN_S; a lock that stays taken means a window process
    may be bringing REAPER back itself: no iemmode call. Nothing else in the
    state changes: the guard's event plan brings REAPER back."""
    sw = spike_module()
    seen: dict = {}

    def close(st: dict) -> None:
        if not st.get("closed"):
            st["closed"] = True
            st["closed_by"] = {"by": "iempc event after a failed spike preempt", "at": core.now_iso(), "error": error[-500:]}
        if sw.intent_live(st.get("in_flight")):
            seen["in_flight"] = st["in_flight"]

    wait = deadline - event_clock() - SWITCH_MIN_S
    try:
        if wait <= 0:
            raise sw.StepError(f"no time left in the event budget to wait for the window lock ({max(wait, 0):.0f} s)")
        sw.update_state(change=close, wait_s=wait)
    except sw.StepError as e:
        raise StepError(f"the S1a/S1c window could not be closed after the failed spike preempt ({e}): no iemmode call "
                        "while a window process may hold the window lock and bring REAPER back itself (one bring-back, "
                        "the #9 lesson); run 'iempc event' again once it is free (spike_window.py status)") from None
    except (OSError, ValueError) as e:
        # An unreadable window state: no window process can bring REAPER back from it
        # either (each preempt reads it first), so the guard's event path goes on.
        print(f"iempc: WARNING: the S1a/S1c window state could not be read to close it ({e}); the event path goes on",
              file=sys.stderr, flush=True)
        emit({"spike_window": "unreadable", "after": "a failed spike preempt"})
        return
    if seen:
        # No settle watches it on this path (review of lane G2, finding 5): the step's
        # own late handler sees the window closed, and the guard takes REAPER.
        print(f"iempc: WARNING: {seen['in_flight'].get('step')} is still in flight in the closed window: its own "
              "follow-up runs when its call is back; check REAPER once iemmode event is done", file=sys.stderr, flush=True)
    emit({"spike_window": "closed", "after": "a failed spike preempt", **seen})


def switch_timeout(deadline: float) -> float:
    """What an iemmode call of the event path may take: the rest of the one
    budget. It never starts with less than SWITCH_MIN_S left, since a cut
    `--direct` session stops its switch half-way."""
    left = deadline - event_clock()
    if left < SWITCH_MIN_S:
        raise StepError(f"the event path has {max(left, 0):.0f} s of its {EVENT_BUDGET_S} s budget left, less than the "
                        f"{SWITCH_MIN_S} s an iemmode call gets: run 'iempc event' again (a new budget)")
    return left


def cmd_event(ctx: Ctx, ip) -> int:
    """The flag, the spike preempt when a window is open, then `iemmode
    event --signal` (and `--direct` on exit 4), all within EVENT_BUDGET_S.
    `--signal` makes it the owner's "ide event": in prod (after the cutover)
    the guard keeps the band's system (S8 lane 3); an iemmode that does not
    know it (exit 2, UNKNOWN_SIGNAL) gets the plain `iemmode event`, the
    event plan in every lifecycle. Right after the flag the Gen 3 device's
    stop starts beside all of it (`start_gen3_stop`); it is collected at the
    end within GEN3_STOP_S of its start and never changes the exit code."""
    dry = bool(getattr(ctx.args, "dry_run", False))
    deadline = event_clock() + EVENT_BUDGET_S
    if dry:
        if ctx.env.get("GEN3_HOST"):
            gen3_emit({"plan": "systemctl stop iem-server iem-engine iem-dante"})
        return pc_event(ctx, ip, deadline, dry)
    write_flag()
    # Started after the flag and never waited for here, so the PC's switch is
    # not delayed; collected at the end, whatever the PC's path did. Neither
    # raises, so the PC's switch always runs and its exit code (or its
    # StepError) is what the event path ends with.
    stop = start_gen3_stop(ctx.env)
    try:
        return pc_event(ctx, ip, deadline, dry)
    finally:
        end_gen3_stop(stop)


def pc_event(ctx: Ctx, ip, deadline: float, dry: bool) -> int:
    """The IEM PC's part of the event path, after the flag: the spike window,
    a recorded trace, then iemmode event (`--direct` on exit 4)."""
    if spike_window_open() or spike_window_settling():
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
            if not pre["ok"]:
                close_failed_window(deadline, pre.get("error", ""))
    if not dry:   # a trace whose dev-box process died (#15); never raises. guarded sees its
        # bound only at its next poll: two polls stay with iemmode event's minimum.
        iempc_trace.stop_recorded(ctx, ip, deadline - event_clock() - SWITCH_MIN_S - 2 * core.POLL_S)
    args = ["event", "--dry-run", SIGNAL] if dry else ["event", SIGNAL]
    code, reply, raw = iemmode(ctx.env, args, switch_timeout(deadline), "ignore")
    emit(result("iemmode", args, code, reply, raw))
    if code == USAGE and UNKNOWN_SIGNAL in (raw.get("err") or ""):
        args = [a for a in args if a != SIGNAL]
        code, reply, raw = iemmode(ctx.env, args, switch_timeout(deadline), "ignore")
        emit(result("iemmode", args, code, reply, raw))
    if code == GUARD_UNREACHABLE:
        args = [*args, "--direct"]
        code, reply, raw = iemmode(ctx.env, args, switch_timeout(deadline), "ignore")
        emit(result("iemmode", args, code, reply, raw))
    if code != 0 and not dry:
        print(OWNER_ALARM, file=sys.stderr, flush=True)
    return code
