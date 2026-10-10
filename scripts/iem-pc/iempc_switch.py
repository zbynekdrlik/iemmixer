"""`iempc switch-test` (S7 design note §5, #10; plan Task 14): one switch from
dev to event and one back, timed by the guard's own records (`last_switch`,
iem-guard switch_log.rs), and a verdict on them. Dev time only (iempc's Spec:
no EVENT-NOW flag at the start). In this order:

0. iempc.py's call site, first: a live run of this dev entry that may still
   run (`iempc_live.refuse_while_live`: a switch would end it).
1. Refused before any switch: an open S1a/S1c window (as `iempc dev`), then
   one `iemmode status` (a new flag abandons it and runs the event path):
   the guard answers ok, in dev, no switch, no HIL job (iempc_soak's
   `settled_refusal`); the active bundle (the detail's `bundle <sha>`) is the
   running engine's build, and that engine plays, neither parked nor faulted
   (an engine already silent or broken gives no silence of a switch from a
   playing one). Then a trace an `iempc trace` that died left recorded is
   stopped (iempc_trace.stop_recorded, as `iempc dev` and `iempc event` do);
   one that may still run refuses the test (it would run through both legs
   and weigh on the times measured), and a flag that came during that stop
   runs the event path.
2. The event leg: `iemmode event` straight through the guard's pipe (the
   guard's own switch, never `--direct`). It never writes the EVENT-NOW flag:
   that flag is the owner's signal, and a test must not leave it behind. A
   flag that appears meanwhile lets the leg run to its end: it is the switch
   to event.
3. The dev leg, only when the event leg exited 0 and no flag appeared:
   `iemmode dev` (the guard owns the switch: a new flag abandons this client
   and the event path pre-empts it). One that exits 0 is a dev entry of this
   box, as `iempc dev`'s (`next_entry`). A flag before or during it: no dev
   leg, the output says so, then the event path (EventNow: its exit code is
   the command's). An event leg that did not exit 0: no dev leg either; the
   guard decides where the PC is.
4. Each leg's record is its reply's `last_switch`, or that of one `iemmode
   status` when the reply carries none; the record already seen before the
   leg is no record of it. A call that fails (an ssh error, a call left
   running past its bound) is the leg's `error`: the output is still
   printed, then the command fails with it.
5. One JSON object `{"switch_test": …}`: each leg's exit code, record (its
   `silence_ms` and step times) and the guard's `detail`; `numbers` with the
   handover time (from the first of REAPER's start or its handover through
   the app's handover), none from a record that is not the leg's own switch;
   and `verdict`'s conclusion: `success` (exit 0), `failure` (exit 1, red
   names the first failing number; a red event leg still goes back to dev) or
   `cancelled` (an "ide event" took the dev leg and nothing was red).
   Nothing is ever force-ended.

Known limits: the calls' bounds add up to 1440 s (three status reads of
STATUS_S, two switches of SWITCH_S), past one 10-minute foreground call; a
switch normally takes a minute or two. A soak running on the PC is not seen
(it changes no guard state): a switch test then ends that soak (`left-dev`).

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import datetime as dt

import iempc_rollback
import iempc_soak
import iempc_trace

SILENCE_MAX_MS = 60_000    # design note §5: dev → event silences the in-ears at most 60 s
HANDOVER_MAX_MS = 90_000   # design note §5: the handover checks finish within 90 s
HANDOVER_FIRST = ("reaper_start", "reaper_handover")
HANDOVER_LAST = "app_handover"
DETAIL_CHARS = 2000   # of the guard's reply detail, as iempc's `result` keeps output
ERROR_CHARS = 1500
NOTHING = "(nothing was switched)"
FLAG_BEFORE = "EVENT-NOW appeared before the dev leg: no dev leg, the PC stays in event; the event path follows"
FLAG_FAILED = ("EVENT-NOW appeared during an event leg that did not exit 0: no dev leg; the event path follows "
               "(it brings the PC to event)")
FLAG_DURING = "EVENT-NOW appeared during the dev leg: the guard unwinds it to event; the event path follows"
EVENT_FAILED = ("the event leg did not exit 0: no dev leg; the guard's state decides (check 'iempc status'), never "
                "force-end")


def is_ms(v) -> bool:
    return type(v) is int and v >= 0


def read_record(v) -> dict | None:
    """A `last_switch` as judged here, or None: absent or of another shape
    (as the guard's `switch_log::lenient`, a record that cannot be read is
    no record). An older guard's record without `unwound` reads it as none."""
    if not isinstance(v, dict):
        return None
    rec = {k: v.get(k) for k in ("from", "to", "ended_in", "outcome")}
    steps, silence, unwound = v.get("steps"), v.get("silence_ms"), v.get("unwound")
    if (not all(isinstance(x, str) for x in rec.values()) or not isinstance(steps, list)
            or not all(isinstance(s, dict) and isinstance(s.get("step"), str) and is_ms(s.get("ms")) for s in steps)
            or not (silence is None or is_ms(silence)) or not (unwound is None or isinstance(unwound, str))):
        return None
    return {**rec, "unwound": unwound, "started": v.get("started"), "ended": v.get("ended"), "silence_ms": silence,
            "steps": [{"step": s["step"], "ms": s["ms"]} for s in steps]}


def handover_ms(steps: list[dict]) -> int | None:
    """The handover's time (ms): from the first of REAPER's start or its
    handover through the first app handover after it, both included (the
    guard's `silence_ms` rule); None when either end is missing."""
    start = next((i for i, s in enumerate(steps) if s["step"] in HANDOVER_FIRST), None)
    if start is None:
        return None
    end = next((i for i in range(start, len(steps)) if steps[i]["step"] == HANDOVER_LAST), None)
    if end is None:
        return None
    return sum(s["ms"] for s in steps[start:end + 1])


def switch_refusal(reply) -> str | None:
    """Why the PC cannot run a switch test now, from one `iemmode status`
    reply (pure); None when it can."""
    why = iempc_soak.settled_refusal(reply)
    if why:
        return why
    if iempc_rollback.lifecycle(reply) in ("prod", "rolling_back"):
        # S8 lane 3: past the cutover a plain `iemmode event` is the rollback.
        return "the PC is past the cutover: its event leg would be the rollback (a switch test is the trial's)"
    bundle = iempc_soak.active_bundle(reply)
    if bundle is None:
        return "no active bundle (the guard's status names none)"
    engine = reply.get("engine")
    if not isinstance(engine, dict):
        return "no engine runs (the guard's status shows none)"
    if engine.get("build") != bundle:
        return f"the running engine's build is {engine.get('build')!r}, not the active bundle {bundle}"
    return iempc_soak.playing_refusal(engine, "a switch test")


def own_event(rec: dict | None) -> bool:
    """The record is a switch dev → event of its own (no unwound entry in it)."""
    return rec is not None and (rec["from"], rec["to"], rec["unwound"]) == ("dev", "event", None)


def own_dev(rec: dict | None) -> bool:
    """The record is a switch from event: the dev entry, or its unwind."""
    return rec is not None and rec["from"] == "event"


def event_problem(code: int | None, rec: dict | None) -> str | None:
    """The event leg's first failure: its call, its record, its end, its
    silence, the handover, its exit code."""
    if code is None:
        return "event leg: the iemmode event call failed"
    if not own_event(rec):
        return f"event leg: no record of a switch dev → event (iemmode event exit {code})"
    if (rec["outcome"], rec["ended_in"]) != ("done", "event"):
        return f"event leg: outcome {rec['outcome']}, ended in {rec['ended_in']}"
    silence = rec["silence_ms"]
    if silence is None:
        return "event-leg silence: none"
    if silence > SILENCE_MAX_MS:
        return f"event-leg silence {silence} ms > {SILENCE_MAX_MS} ms"
    handover = handover_ms(rec["steps"])
    if handover is None:
        return "handover: none"
    if handover > HANDOVER_MAX_MS:
        return f"handover {handover} ms > {HANDOVER_MAX_MS} ms"
    if code != 0:
        return f"event leg: iemmode event exit {code}"
    return None


def dev_problem(code: int | None, rec: dict | None) -> str | None:
    """The dev leg's first failure: its call, its record, unwound none (an
    unwound entry also ends `done`, #10 2026-10-07), its target dev, outcome
    done, its exit code."""
    if code is None:
        return "dev leg: the iemmode dev call failed"
    if not own_dev(rec):
        return f"dev leg: no record of a switch from event (iemmode dev exit {code})"
    if rec["unwound"] is not None:
        return f"dev leg: unwound (its {rec['unwound']} entry went back to event)"
    if rec["to"] != "dev":
        return f"dev leg: to {rec['to']}, not dev"
    if rec["outcome"] != "done":
        return f"dev leg: outcome {rec['outcome']}"
    if code != 0:
        return f"dev leg: iemmode dev exit {code}"
    return None


def _ms(v: int | None) -> str:
    return "none" if v is None else f"{v} ms"


def verdict(event: dict, dev: dict | None) -> dict:
    """The legs (`{"exit", "record", …}`; `dev` None when no dev leg ran)
    judged (pure): `{"conclusion": "success"|"failure"|"cancelled",
    "summary", "first_failure", "numbers"}`. Red names the first failure, the
    event leg's first; no dev leg and nothing red is cancelled (only an "ide
    event" takes the dev leg of an event leg that exited 0)."""
    ev, dv = event["record"], dev["record"] if dev else None
    numbers = {"event_silence_ms": ev["silence_ms"] if own_event(ev) else None,
               "handover_ms": handover_ms(ev["steps"]) if own_event(ev) else None,
               "dev_silence_ms": dv["silence_ms"] if own_dev(dv) else None}
    text = (f"event-leg silence {_ms(numbers['event_silence_ms'])}, handover {_ms(numbers['handover_ms'])}, "
            f"dev-leg silence {_ms(numbers['dev_silence_ms'])}")
    first = event_problem(event["exit"], ev) or (dev_problem(dev["exit"], dv) if dev else None)
    if first:
        conclusion, summary = "failure", f"red: {first}; {text}"
    elif dev is None:
        conclusion, summary = "cancelled", f"cancelled: no dev leg; {text}"
    else:
        conclusion, summary = "success", f"green: {text}"
    return {"conclusion": conclusion, "summary": summary, "first_failure": first, "numbers": numbers}


def leg(ctx, ip, mode: str, watch: str, seen: dict | None) -> tuple[dict, dict | None]:
    """`iemmode <mode>` and its record (the reply's `last_switch`, else one
    `iemmode status`'s). Returns `{"exit", "record", "detail"}` (the record
    None when none was read or it is `seen`, the one read before; `exit` None
    when the switch call failed; `error` when a call failed) and the record
    read. EventNow (a new flag in a watched call) is not caught."""
    out: dict = {"exit": None, "record": None, "detail": None}
    read = None
    try:
        code, reply, _ = ip.iemmode(ctx.env, [mode], ip.SWITCH_S, watch)
        out["exit"] = code
        if isinstance(reply, dict) and isinstance(reply.get("detail"), str):
            out["detail"] = reply["detail"][-DETAIL_CHARS:]
        if not isinstance(reply, dict) or reply.get("last_switch") is None:
            _, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, watch)
        read = read_record(reply.get("last_switch") if isinstance(reply, dict) else None)
    except ip.StepError as e:
        out["error"] = str(e)[-ERROR_CHARS:]
    out["record"] = read if read != seen else None
    return out, read


def report(event: dict, dev: dict | None, no_dev: str | None = None) -> dict:
    """The command's object: the verdict, both legs and why no dev leg ran."""
    out = {**verdict(event, dev), "event_leg": event, "dev_leg": dev}
    if no_dev:
        out["no_dev_leg"] = no_dev
    return out


def finish(ip, out: dict) -> int:
    """After the output: a leg whose call failed fails the command with its
    error (iempc then runs the event path if a new flag exists); else 0 green,
    1 red."""
    for name in ("event", "dev"):
        error = (out[f"{name}_leg"] or {}).get("error")
        if error:
            raise ip.StepError(f"switch-test: the {name} leg: {error} (the guard decides where the PC ends: check "
                               "'iempc status'; never force-end)")
    return 0 if out["conclusion"] == "success" else 1


def switch_test(ctx, ip) -> int:
    """`iempc switch-test` (dev time, locked)."""
    ip.refuse_open_window("switch-test")
    code, status, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    why = f"iemmode status failed (exit {code})" if code != 0 else switch_refusal(status)
    if why:
        raise ip.Refused(f"no switch test: {why} {NOTHING}")
    soak = iempc_soak.running_soak(ip, ip.current_entry(), dt.datetime.now().astimezone())
    if soak is not None:
        raise ip.Refused(f"no switch test: a soak dispatched in this dev entry may still run (dispatched "
                         f"{soak.get('at')}, {soak.get('hours')} h): a switch would end it {NOTHING}")
    stopped = iempc_trace.stop_recorded(ctx, ip, float("inf"))
    if ip.event_now():   # that stop ran with "ignore": no switch after a flag that came meanwhile
        raise ip.EventNow()
    if not stopped:
        raise ip.StepError(f"no switch test: a trace recorded in {iempc_trace.record_path(ip)} may still run on the "
                           f"PC (above) {NOTHING}")
    event, seen = leg(ctx, ip, "event", "ignore", read_record(status.get("last_switch")))
    if ip.event_now():   # "ide event" meanwhile wins: no dev leg
        ip.emit({"switch_test": report(event, None, FLAG_BEFORE if event["exit"] == 0 else FLAG_FAILED)})
        raise ip.EventNow()
    if event["exit"] != 0:
        out = report(event, None, EVENT_FAILED)
        ip.emit({"switch_test": out})
        return finish(ip, out)
    try:
        dev, _ = leg(ctx, ip, "dev", ctx.watch(abandon=True), seen)
    except ip.EventNow:
        ip.emit({"switch_test": report(event, None, FLAG_DURING)})
        raise
    out = report(event, dev)
    if dev["exit"] == 0:
        out["dev_entry"] = ip.next_entry(None)
    ip.emit({"switch_test": out})
    return finish(ip, out)
