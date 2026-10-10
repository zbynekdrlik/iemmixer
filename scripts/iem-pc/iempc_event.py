"""`iempc event`, "ide event" on the dev box (S6 design note §5.5; split out
of iempc.py, #36): the flag first (`write_flag`; a flag it cannot write is a
warning, never a stop), the spike preempt of an open S1a/S1c window or of a
closed one still settling (`spike_preempt`), the window closed under its lock
after a failed preempt (`close_failed_window`), a kernel trace a dead `iempc
trace` left recorded stopped (iempc_trace.stop_recorded), then `iemmode
event --signal` (the owner's "ide event", which in prod never rolls back; S8
lane 3), and `iemmode event --direct` when the guard is unreachable (exit 4):
all within one budget (EVENT_BUDGET_S) on one clock (`event_clock`).
iempc.py's docstring states the whole rule.

iempc.py passes itself in (`ip`) for the siblings. The names a test patches
here (SPIKE, EVENT_BUDGET_S, SPIKE_SHARE_S, SWITCH_MIN_S, `event_clock`,
`ensure_flag`) live in this module only; the shared ones are read as
`core.<name>` (iempc_core.py)."""
from __future__ import annotations

import sys
import time

import iempc_core as core
import iempc_rollback
import iempc_trace
from iempc_core import (SPIKE_DIR, Ctx, StepError, StillRunning, emit, event_now, guarded, iemmode, result,
                        spike_module, spike_window_open, spike_window_settling)

SPIKE = SPIKE_DIR / "spike_window.py"
GUARD_UNREACHABLE = 4
USAGE = 2
# The owner's "ide event" (S8 lane 3): in prod `iemmode event` without it is the
# engineer's "Back to REAPER", the rollback; with it the guard never rolls back.
SIGNAL = "--signal"
# What an iemmode older than S8 lane 3 says to it (a usage error, exit 2, before
# any guard call). The plain event is the same only before the cutover: the
# guard's status says so first (an older iemmode with a newer guard in prod).
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


def plain_event_allowed(ctx: Ctx, deadline: float) -> None:
    """An iemmode that does not know --signal gets the plain `iemmode event`
    only while the guard is before the cutover (or unreachable: then
    `--direct` runs the event plan): past it the plain event is the engineer's
    button, the rollback. Anything else stops the event path for the owner."""
    code, reply, raw = iemmode(ctx.env, ["status"], switch_timeout(deadline), "ignore")
    emit(result("iemmode", ["status"], code, reply, raw))
    if code == GUARD_UNREACHABLE or iempc_rollback.lifecycle(reply) == "trial":
        return
    raise StepError(f"the PC's iemmode does not know --signal, and the guard is {iempc_rollback.lifecycle(reply)} "
                    f"(status exit {code}): a plain 'iemmode event' would be the rollback, so none was sent; "
                    "'iempc activate' of the running build brings an iemmode that knows the owner's signal")


def cmd_event(ctx: Ctx, ip) -> int:
    """The flag, the spike preempt when a window is open, then `iemmode
    event --signal` (and `--direct` on exit 4), all within EVENT_BUDGET_S.
    `--signal` makes it the owner's "ide event": in prod (after the cutover)
    the guard never rolls back for it (S8 lane 3); an iemmode that does not
    know it (exit 2, UNKNOWN_SIGNAL) gets the plain `iemmode event`."""
    dry = bool(getattr(ctx.args, "dry_run", False))
    deadline = event_clock() + EVENT_BUDGET_S
    if not dry:
        write_flag()
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
        plain_event_allowed(ctx, deadline)
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
