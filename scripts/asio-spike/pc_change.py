"""PC changes in flight (#32 F2 round 3 MAJOR, decision 1: intents and
settling), for the window tools on the dev box (spike_window, tuning_window,
iempc's event path).

The window lock guards the state, never a PC call (approach 2, holding it
across the call, would make "ide event" wait for a save and quit or an enter).
So every step that changes what the event depends on (REAPER and the card, the
driver's preferred buffer, the spike, the tuning levers and journal) records an
intent {step, started, bound_s} in the state under the lock BEFORE its ssh call
and clears it after, on error too (pc_change). A preempt that finds one in
flight still brings REAPER back at once, then watches, bounded, that REAPER runs
and holds the card, and brings it back again when the late step took it down
(settle). The late step sees the window closed and undoes what it may have left
(its late handler). On the PC every such step starts only while the spike's stop
file is absent (step_guard): every preempt writes it first, and only the unwind
that closes the window removes it again (clear_stop, F2 round 3 m1).

The window state, its lock and the PC access stay in spike_window, reached at
call time (_sw), so a test that fakes spike_window's ssh seam (sw.ps) fakes it
here too. spike_window imports this module; import spike_window first."""
from __future__ import annotations

import json
import signal
import sys
import time
from collections.abc import Callable
from typing import TypeVar

from golden_window import StepError, ps_quote  # scripts/golden, put on sys.path by spike_window

STOP_FILE = "queue/stop"
STEP_REFUSED = "ide event: the step did not start"
SETTLE_S = 15.0
SAVE_QUIT_S = 120   # to-dev: Invoke-GoldenSaveQuit (save <= 15 s, quit <= 30 s, the requests and the holder read)
SET_BUFFER_S = 60   # one registry write and its read-back
RUN_START_S = 60    # the request file and the task start
# The steps that write the S1c tuning journal: the preempt never runs its own
# tuning-exit while one is in flight (two writers at once lose journal entries);
# the late step runs the exit itself.
JOURNAL_STEPS = ("enter", "exit", "apply", "undo")
# The steps that can take REAPER off the card when they land late: the save and
# quit, a preference write the driver may answer with a reset, a spike start.
# The settle reads REAPER on the PC only after one of these (review of lane G2,
# finding 6).
REAPER_STEPS = ("to-dev", "set-buffer", "run")
T = TypeVar("T")


def _sw():
    """spike_window, as loaded (run as a script it registers itself under its
    own name, so this is never a second copy with a lock state of its own)."""
    return sys.modules["spike_window"]


def step_guard(env: dict[str, str]) -> str:
    """The PC-side start of a step that changes the PC: it throws, changing
    nothing, while the spike's stop file exists — a preempt wrote it (the step
    was sent before "ide event"), or the last unwind did not remove it. It comes
    before any module import."""
    return (f"if (Test-Path -LiteralPath {_sw().pc(env, STOP_FILE)}) {{ throw '{STEP_REFUSED} (the spike stop file exists: "
            "a pre-emption runs, or the last unwind did not remove it)' }")


def changing(env: dict[str, str], body: str) -> str:
    return f"{step_guard(env)} ; {body}"


def intent_live(intent) -> bool:
    """A recorded intent whose step may still be changing the PC: its bound is
    not over yet. A later one is stale (its process ended without clearing it)."""
    return isinstance(intent, dict) and time.time() <= float(intent.get("started", 0)) + float(intent.get("bound_s", 0))


def begin_change(step: str, bound_s: float, fields: dict | None = None, check: Callable[[dict], None] | None = None) -> dict:
    """Under the window lock, on the state as saved now: the window is open and
    no "ide event" flag exists (open_state), `check` passes and no other change
    is in flight; then `fields` and the intent are saved. Returns the intent."""
    sw = _sw()
    with sw.window_lock():
        state = sw.open_state()
        if check is not None:
            check(state)
        other = state.get("in_flight")
        if intent_live(other):
            raise StepError(f"another PC step is in flight ({other['step']}, at most {other['bound_s']:g} s): wait for it")
        if other:
            sw.alarm(f"an earlier {other.get('step')} never cleared its intent (its process ended, or its call outlived "
                     "its bound): check what it left on the PC")
        # `prior`: the values `fields` replace, put back when the PC refuses the step.
        intent = {"step": step, "started": time.time(), "bound_s": bound_s,
                  "prior": {k: state.get(k) for k in (fields or {})}}
        state.update(fields or {})
        state["in_flight"] = intent
        sw.save_state(state)
        return intent


def end_change(intent: dict, fields: dict | None = None, late: Callable[[dict, bool], None] | None = None,
               ran: bool = True) -> dict:
    """After the step's call (or its error): while the window is open, `fields`
    are merged and the intent cleared, under the lock. A step the PC refused
    (`ran` False: nothing changed there) gets what begin_change recorded for
    it put back instead (review of lane G2, finding 3). When a preempt or
    to-event closed the window meanwhile, `late(state, ran)` runs first
    (without the lock, the intent still recorded, so a settle watch goes on
    meanwhile) with the state as saved then (for a refused step, as it was
    before it: the closing unwind's own results stay saved); its error is an
    owner alarm. Returns the state as saved."""
    sw = _sw()
    prior = intent.get("prior") or {}

    def clear(st: dict) -> None:
        if st.get("in_flight") == intent:
            st["in_flight"] = None

    with sw.window_lock():
        state = sw.load_state()
        if not state.get("closed"):
            state.update((fields or {}) if ran else prior)
            clear(state)
            sw.save_state(state)
            return state
        # A preempt that waited out its settle may have taken the follow-up over
        # (exit_left_behind cleared the intent): then it is not run twice.
        mine = state.get("in_flight") == intent
        view = state if ran else {**state, **prior}
    if late is not None and mine:
        try:
            late(view, ran)
        except StepError as e:
            sw.alarm(f"{intent['step']} ended after the window was closed, and its follow-up failed ({e}): check the PC")
    return sw.update_state(change=clear)


def pc_change(step: str, bound_s: float, call: Callable[[], T], fields: dict | None = None, after: dict | None = None,
              check: Callable[[dict], None] | None = None, late: Callable[[dict, bool], None] | None = None) -> T:
    """One step that changes the PC: begin_change, the call (its ssh call runs
    without the lock, bounded by `bound_s`), end_change with `after` (what its
    success records) or, after an error, nothing. A step whose window was
    closed while it ran raises EventNow ("ide event", the usual case) or a
    StepError (to-event in another process), after its late handler. A body
    the PC refused on the stop file is "ide event" when the flag exists."""
    sw = _sw()
    intent = begin_change(step, bound_s, fields, check)
    try:
        result = call()
    except StepError as e:
        end_change(intent, late=late, ran=STEP_REFUSED not in str(e))
        if STEP_REFUSED in str(e):
            if sw.event_now():
                raise sw.EventNow() from None
            raise StepError(f"{step} did not start: the spike's stop file exists on the PC, but no \"ide event\" flag "
                            "here: a pre-emption that did not close its window, or a deadline stop, left it. Only an "
                            "unwind that closes a window removes it: to-event in this window (in a dev-time window it "
                            f"brings REAPER back), then a new window ({e})") from None
        raise
    except BaseException:
        end_change(intent, late=late)
        raise
    if end_change(intent, after, late).get("closed"):
        if sw.event_now():
            raise sw.EventNow()
        raise StepError(f"the window was closed (to-event) while {step} ran")
    return result


def reaper_on_card(env: dict[str, str]) -> bool:
    """REAPER runs and is the one holder of the ASIO module (read-only)."""
    r = _sw().ps(env, f"[pscustomobject]@{{ reaper = @(Get-Process reaper -ErrorAction SilentlyContinue).Count; "
                      f"holders = @(Get-GoldenAsioHolders -Module {ps_quote(env['PC_ASIO_MODULE'])}) }}",
                 timeout=60, event="ignore")
    if not isinstance(r, dict):
        raise StepError(f"REAPER's state could not be read (reply {json.dumps(r)})")
    holders = r.get("holders")
    holders = holders if isinstance(holders, list) else [] if holders is None else [holders]
    return bool(r.get("reaper")) and sum(str(h).lower().startswith("reaper.exe:") for h in holders) == 1


def settle_until(intent: dict) -> float:
    """The latest end of a settle on `intent` (wall clock): SETTLE_S after the
    step's remaining bound, taken between 0 and its whole bound, so a clock
    set back or forward never stretches the watch (review of lane G2,
    finding 6)."""
    bound = float(intent["bound_s"])
    now = time.time()
    return now + min(max(float(intent["started"]) + bound - now, 0.0), bound) + SETTLE_S


def settle_live(state: dict) -> bool:
    """A preempt or to-event of some process still settles a PC change: its
    record (`settling`, saved with the closing unwind) is there and its bound
    not over (a settler that died holds nothing up past it)."""
    s = state.get("settling")
    return isinstance(s, dict) and time.time() <= float(s.get("until", 0))


def settle(env: dict[str, str], intent: dict) -> dict:
    """After a bring-back that found a PC change in flight: watches that REAPER
    runs and holds the card, every POLL_S, and brings it back again (under the
    window lock, an owner alarm) when the late step took it down. The watch
    ends SETTLE_S after the step's process cleared its intent (its call and
    follow-up are back), and at the latest SETTLE_S after the intent's own
    bound. It runs without the lock, so that process can clear the intent. A
    read or a bring-back that fails is an owner alarm and the watch goes on
    (review of lane G2, finding 5); `on_card` tells whether its last read saw
    REAPER on the card. Only a step in REAPER_STEPS can take REAPER off the
    card: after any other (the journal steps) the watch reads no PC, it only
    waits from the state for the step and its follow-up (an exit) to end, so
    the event path does not run the guard's own exit next to it (finding 6)."""
    sw = _sw()
    end = settle_until(intent)
    cleared = False
    reads = intent["step"] in REAPER_STEPS
    watched = {"step": intent["step"], "checks": 0, "brought_back_again": 0, "errors": 0, "on_card": False if reads else None}
    while True:
        if not cleared and sw.load_state().get("in_flight") != intent:
            cleared = True
            end = min(end, time.time() + SETTLE_S)
        if not reads:
            if cleared or time.time() >= end:
                return watched
            time.sleep(sw.POLL_S)
            continue
        watched["checks"] += 1
        try:
            watched["on_card"] = reaper_on_card(env)
            if not watched["on_card"]:
                with sw.window_lock():
                    sw.alarm(f"REAPER was down or off the card after the bring-back ({intent['step']} was still in "
                             "flight): it is brought back again")
                    sw.bring_back(env, sw.load_state())
                watched["brought_back_again"] += 1
                watched["on_card"] = True   # the bring-back's own handover checks passed
        except StepError as e:
            watched["errors"] += 1
            watched["on_card"] = False
            sw.alarm(f"while {intent['step']} was in flight REAPER could not be read or brought back ({e}): the watch goes on")
        if time.time() >= end:
            return watched
        time.sleep(sw.POLL_S)


def wait_for_settle() -> None:
    """A preempt that finds the window closed: the process that closed it may
    still settle a PC change (without the lock). The caller (iempc event) must
    not start the guard's bring-back next to the settle's, so this returns only
    once that settle is over (review of lane G2, finding 1)."""
    sw = _sw()
    while settle_live(sw.load_state()):
        time.sleep(sw.POLL_S)


def clear_stop(env: dict[str, str]) -> str:
    """The spike's stop file, removed by the unwind that closed the window (and
    post-boot's close), never by a start, which refuses while it exists (F2
    round 3, m1). Kept while the spike or its task runs. A failure is an owner
    alarm: the next start refuses until an unwind removes it."""
    sw = _sw()
    body = (f"$s = {sw.pc(env, STOP_FILE)} ; if ((@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count -gt 0) "
            "-or (Test-SpikeTaskBusy)) { 'kept: the spike or its task runs' } elseif (Test-Path -LiteralPath $s) "
            "{ Remove-Item -LiteralPath $s ; 'removed' } else { 'absent' }")
    with sw.window_lock():
        try:
            return sw.ps(env, body, timeout=60, event="ignore")
        except StepError as e:
            sw.alarm(f"the spike's stop file was not removed ({e}): the next start refuses until an unwind removes it")
            return f"error: {e}"


def exit_left_behind(env: dict[str, str], intent: dict) -> dict | None:
    """After the settle: a journal step whose intent is still recorded never
    came back (its process ended, or its call outlived its bound), so the
    tuning-exit the unwind deferred to it never ran (review of lane G2,
    finding 2). It is taken over here: the intent is cleared first, under the
    lock (a late step that still comes back then leaves the exit to this),
    then the exit runs without the lock, with an owner alarm."""
    sw = _sw()
    with sw.window_lock():
        state = sw.load_state()
        if state.get("in_flight") != intent:
            return None   # the step came back and ran its own follow-up
        state["in_flight"] = None
        sw.save_state(state)
    sw.alarm(f"{intent['step']} never came back (its process ended, or its call outlived its bound): the tuning-exit "
             "deferred to it runs now")
    try:
        rows = sw.ps(env, sw.tuning_body(env, f"Exit-IemTuningMode -ProfilePath {sw.tuning_profile(env)}"),
                     timeout=240, event="ignore")
    except StepError as e:
        sw.alarm(f"the deferred tuning-exit failed ({e}): the S1c mode levers may stay applied through the event")
        return {"tuning-exit": {"error": str(e)}}
    sw.update_state({"tuning_mode": False})
    sw.alarm_exit_problems(rows)
    return {"tuning-exit": rows}


def unwind_closing(env: dict[str, str], state: dict, running: bool) -> list:
    """The unwind of a preempt or to-event, under the lock the caller holds: a
    PC change in flight gets its `settling` record saved with the close, so a
    late step and every other preempt see the settle that follows; a failed
    unwind drops the record again."""
    sw = _sw()
    intent = state.get("in_flight")
    if isinstance(intent, dict):
        state["settling"] = {"step": intent["step"], "until": settle_until(intent)}
    try:
        return sw.unwind(env, state, running)
    except BaseException:
        if state.pop("settling", None) is not None:
            sw.save_state(state)
        raise


def close_out(env: dict[str, str], intent, done: list) -> dict:
    """After an unwind that closed the window (REAPER back): the settle watch
    when a PC change was recorded in flight (its `settling` record goes when
    it ends), the tuning-exit a journal step that never came back still owes,
    then the stop file's clean-up."""
    sw = _sw()
    out: dict = {}
    try:
        if isinstance(intent, dict):
            out["settle"] = settle(env, intent)
            if any(isinstance(d.get("tuning-exit"), dict) and "deferred" in d["tuning-exit"] for d in done):
                out["deferred_exit"] = exit_left_behind(env, intent)
    finally:
        try:
            sw.update_state(change=lambda st: st.pop("settling", None))
        finally:
            out["stop_file"] = clear_stop(env)   # however the watch ended (review of lane G2, finding 5)
    print(json.dumps({"close": out}), flush=True)
    if out.get("settle", {}).get("on_card") is False:
        raise StepError(f"REAPER was not seen on the card at the end of the watch over {intent['step']}: the event path "
                        "brings it back (iemmode event)")
    return out


def exit_on_signals() -> None:
    """SIGTERM and SIGHUP end a window command through Python's exception path
    (SystemExit), so a PC change in flight clears its intent and runs its late
    handler (pc_change) instead of leaving them to its bound (review of lane
    G2, finding 2). Nothing on the PC is ended: its call runs on."""
    def leave(signum, frame) -> None:
        raise SystemExit(128 + signum)

    for s in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(s, leave)
