"""`iempc gen3-loop --offsets F1,F2 --bursts N --slot X` (#72 phase B, plan
docs/superpowers/plans/2026-10-26-gen3-phase-b-audio-through.md Task 8 Step
2): the loop through the IEM card that proves a real Dante receiver hears our
Gen 3 device sample-exact (the plan's "The loop test").

Our device's RX 1 receives the card's spare HIL TX X, and its TX 1 echoes RX 1
F samples later (`iem-dante-dev ctl source 1 echo 1 F` on the Gen 3 box: the
TX sample stamped T is the RX sample stamped T - F); for the test the owner
subscribes the card's spare HIL return X to our TX 1. The engine's HIL test
signal then goes card -> our device -> card, and its D5(b) latency probe
reports the round trip in samples (`engine.loopback_samples` in `iemmode
status`). Every Dante receiver plays a sample at its stamp plus its latency,
so the round trip is a constant plus F: equal over the bursts at one offset,
and exactly F2 - F1 apart between the two offsets.

Dev time, locked, a PC command (iempc's Spec: refused while EVENT-NOW exists;
a flag during the run abandons it and runs the event path). In this order:

0. iempc.py's call site, first: a live run or a soak of this dev entry that
   may still run refuses it (`iempc_live.refuse_while_running`: the loop
   restarts the engine in a HIL job and silences every mix's TX).
1. Before any call: the arguments, GEN3_HOST in the private env (an ssh
   destination), no open S1a/S1c window (the card is the window's).
2. The Gen 3 device (`iem-dante-dev ctl status` over ssh, as the service's
   user, the one the control socket admits): its media clock ready, RX 1
   receiving, a live TX flow (a receiver takes our TX). Else refused,
   nothing changed.
3. `iemmode status`: settled in dev, the active bundle's engine playing
   (`iempc_soak.runs_refusal`); then no ops HIL run that may still hold the
   PC (`iempc_live.hil_refusal`: both begin a HIL job, the second to reach
   the PC is cancelled).
4. `iemmode job-begin <run>` (`run`: the Unix time). Refused: nothing
   changed, no job, no source.
5. Inside the job (`iemmode job-end <run>` at its end whatever failed,
   except once the flag exists: the switch out of dev ends the job, and no
   call may delay the event path):
   - `iemmode activate <the active bundle>`: inside a HIL job the guard
     restarts the engine with the HIL flags (the test signal and the loopback
     return need them), as HIL v1 does; `iemmode status` polled until the new
     engine (its start count above the one before) plays, its HIL outputs
     holding X (ENGINE_WAIT_S);
   - the Gen 3 counters read (the window's start);
   - for each offset: `ctl source 1 echo 1 F`, then N bursts: `iemmode
     test-signal <input> <dbfs> <ttl>`, and `iemmode status` polled until X
     carried the burst and a later status shows every spare output silent;
     that status's `loopback_samples` is the burst's round trip (the engine
     clears it when a signal starts: 0 is none measured). A burst not heard
     on X, or not ended, within its TTL plus BURST_WAIT_S counts as not heard;
   - the Gen 3 counters again (the window's end).
6. One JSON line, numbers and fixed words only (`verdict`, pure): pass when
   every burst was heard and ended, the N round trips at each offset are equal
   and above 0, the second offset's minus the first's is F2 - F1, and between
   the two counter reads the device did not restart, its clock stayed ready,
   and no hole (RX 1's ring, the TX flows' echo), late packet (the RX flows),
   late sample (RX 1's ring) or re-anchor (the count or the epoch) came.
   Exit 0 when it passes and the job ended, else 1.

`iemmode test-signal` sounds on every spare output of the site's `[guard]
hil_tx` (the guard has no choice of one): `--slot` names the one whose return
our device echoes, and each burst must be heard on it. The probe takes the
earliest return onset over every spare return the engine opened, so another
return still looped elsewhere gives the same round trip at both offsets, never
F2 - F1 apart: the verdict fails then, it never passes falsely.

P6: X is the card's channel number, so it is never printed (the verdict goes
onto #72); GEN3_HOST stays in the private env. The echo's last offset stays
set at the end (the plan's Task 11 puts the source back to silence).

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import datetime as dt
import json
import math
import re
import sys
import time
from dataclasses import dataclass

import iempc_live
import iempc_soak

# The device on the Gen 3 box (scripts/gen3/deploy.sh installs it there).
DANTE_BIN = "/opt/iemmixer/bin/iem-dante-dev"
SSH = ("ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10")
TX = 1   # our TX channel that echoes
RX = 1   # our RX channel it echoes, subscribed to the card's spare HIL TX
RECEIVING = 0x0101_0009   # ARC 0x3000's Receiving, as the status's `channels` name it
CTL_S = 30   # one ctl call over ssh: the connect, then the socket's own 1 s
ENGINE_WAIT_S = 90   # the engine's restart inside the job, until it plays
BURST_WAIT_S = 30   # past a burst's TTL, for the status that shows it ended
BURSTS_MIN = 2   # equal round trips need two at least
BURSTS_MAX = 20
OFFSET_MAX = 65535   # the echo reads a ring of 65 536 samples
OFFSET = re.compile(r"[0-9]{1,5}")
INPUT = re.compile(r"[A-Za-z0-9_-]{1,64}")
INPUT_DEFAULT = "mic1"   # HIL v1's test input
DBFS_DEFAULT = -30.0   # HIL v1's level
DBFS_MAX = -20.0   # the HIL ceiling (the guard refuses above it)
TTL_DEFAULT = 2.0
TTL_MAX = 60.0   # the guard's HIL_MAX_TTL_S
# The failures a verdict names, in its order.
FAILURES = ("not_heard", "no_measurement", "unequal", "difference", "restarted", "not_ready", "holes", "late",
            "ring_late", "re_anchor")
# The window's counters, each from one Gen 3 status (`window_counts`).
COUNTS = ("ready", "polls", "re_anchors", "epoch", "holes", "late", "ring_late")


@dataclass(frozen=True)
class Signal:
    input: str
    dbfs: float
    ttl: float


def job_run() -> int:
    """The HIL job's run number (tests replace it)."""
    return int(time.time())


# ---- arguments ----

def offsets_of(ip, text: str) -> tuple[int, int]:
    parts = (text or "").split(",")
    if len(parts) == 2 and all(OFFSET.fullmatch(p) for p in parts):
        f1, f2 = int(parts[0]), int(parts[1])
        if 1 <= f1 <= OFFSET_MAX and 1 <= f2 <= OFFSET_MAX and f1 != f2:
            return f1, f2
    raise ip.Refused(f"gen3-loop: --offsets must be two different echo offsets F1,F2 of 1..{OFFSET_MAX} samples")


def checked(ip, args) -> tuple[tuple[int, int], int, int, Signal]:
    """The command line's values, or Refused before any call."""
    offsets = offsets_of(ip, args.offsets)
    if type(args.bursts) is not int or not BURSTS_MIN <= args.bursts <= BURSTS_MAX:
        raise ip.Refused(f"gen3-loop: --bursts must be {BURSTS_MIN}..{BURSTS_MAX}")
    if type(args.slot) is not int or not 1 <= args.slot <= 65535:
        raise ip.Refused("gen3-loop: --slot must be the card's spare HIL output, a channel number of 1..65535")
    if not INPUT.fullmatch(args.input or ""):
        raise ip.Refused("gen3-loop: --input must be an engine input id (1 to 64 of A-Z a-z 0-9 _ -)")
    if not (isinstance(args.dbfs, float) and math.isfinite(args.dbfs) and args.dbfs <= DBFS_MAX):
        raise ip.Refused(f"gen3-loop: --dbfs must be at most {DBFS_MAX:g} (the HIL ceiling)")
    if not (isinstance(args.ttl, float) and 0 < args.ttl <= TTL_MAX):
        raise ip.Refused(f"gen3-loop: --ttl must be above 0 and at most {TTL_MAX:g} s (the guard's HIL limit)")
    return offsets, args.bursts, args.slot, Signal(args.input, args.dbfs, args.ttl)


def gen3_host(ip, env: dict[str, str]) -> str:
    host = env.get("GEN3_HOST", "")
    if not host:
        raise ip.Refused("gen3-loop: no GEN3_HOST in the private env (the Gen 3 box's ssh destination)")
    if host.startswith("-"):
        raise ip.Refused("gen3-loop: GEN3_HOST must be an ssh destination, not an option")
    return host


# ---- the Gen 3 device ----

def ctl(ip, host: str, words: list[str], event: str) -> str:
    """`iem-dante-dev ctl <words>` on the Gen 3 box, its stdout. Bounded by
    CTL_S, the flag watched as `event` says (iempc_core.guarded)."""
    return ip.guarded([*SSH, host, DANTE_BIN, "ctl", *words], "", CTL_S, event)


def device_status(ip, ctx, host: str) -> dict:
    out = ctl(ip, host, ["status"], ctx.watch(abandon=True))
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        doc = json.loads(lines[-1]) if lines else None
    except ValueError:
        doc = None
    if not isinstance(doc, dict):
        raise ip.StepError("gen3-loop: the Gen 3 device's status is not a JSON object")
    return doc


def set_echo(ip, ctx, host: str, offset: int) -> None:
    """TX 1 echoes RX 1 `offset` samples later. The answer goes to stderr
    (`1>&2`, the box's shell): ctl's exit 0 is its `ok`, and a refusal's
    `error: <reason>` lands in the StepError, which quotes stderr."""
    ctl(ip, host, ["source", str(TX), "echo", str(RX), str(offset), "1>&2"], ctx.watch(abandon=False))


def count(doc, key: str) -> int:
    value = doc.get(key) if isinstance(doc, dict) else None
    if type(value) is not int or value < 0:
        raise ValueError(key)
    return value


def rows(status: dict, key: str) -> list[dict]:
    value = status.get(key)
    if not isinstance(value, list) or not all(isinstance(r, dict) for r in value):
        raise ValueError(key)
    return value


def channel(status: dict) -> dict | None:
    """RX 1's row of the status's `channels` (none while it is not subscribed
    and its ring counted nothing)."""
    return next((c for c in rows(status, "channels") if c.get("rx") == RX), None)


def window_counts(status: dict) -> dict[str, int]:
    """The loop window's counters from one Gen 3 status (pure); ValueError
    when it is not the device's shape. `holes`: RX 1's ring and every TX
    flow's echo (samples); `late`: every RX flow's late packets; `ring_late`:
    RX 1's ring, samples behind its reader."""
    clock = status.get("clock")
    ch = channel(status)
    return {
        "ready": count(clock, "ready"),
        "polls": count(clock, "polls"),
        "re_anchors": count(clock, "re_anchors"),
        "epoch": count(clock, "epoch"),
        "holes": (count(ch, "holes") if ch else 0) + sum(count(t, "holes") for t in rows(status, "tx")),
        "late": sum(count(r, "late") for r in rows(status, "rx")),
        "ring_late": count(ch, "late") if ch else 0,
    }


def device_refusal(status: dict) -> str | None:
    """Why the Gen 3 device cannot carry the loop now, from its status
    (pure); None when it can."""
    try:
        counts = window_counts(status)
        ch = channel(status)
        live_tx = any(t.get("live") == 1 for t in rows(status, "tx"))
    except ValueError:
        return "its status is not the device's"
    if counts["ready"] != 1:
        return "its media clock is not ready"
    if ch is None or ch.get("status") != RECEIVING:
        return f"RX {RX} is not receiving (subscribe it to the card's spare HIL TX first)"
    if not live_tx:
        return f"no receiver takes its TX (no live TX flow: the card's spare HIL return is not subscribed to TX {TX})"
    return None


def counts_of(ip, ctx, host: str) -> dict[str, int]:
    try:
        return window_counts(device_status(ip, ctx, host))
    except ValueError as e:
        raise ip.StepError(f"gen3-loop: the Gen 3 device's status lacks {e}") from None


# ---- the PC ----

def said(reply, raw: dict) -> str:
    """What an iemmode answer says, for a refusal's words."""
    if isinstance(reply, dict) and isinstance(reply.get("detail"), str):
        return reply["detail"][-500:]
    return (raw.get("out") or raw.get("err") or "")[-500:]


def spare_peaks(engine) -> dict[int, float] | None:
    """The engine's HIL spare outputs and each one's peak since its previous
    Status (`engine.hil`: [{tx, peak}]); None without that list."""
    hil = engine.get("hil") if isinstance(engine, dict) else None
    if not isinstance(hil, list):
        return None
    peaks = {}
    for out in hil:
        tx, peak = (out.get("tx"), out.get("peak")) if isinstance(out, dict) else (None, None)
        if type(tx) is not int or type(peak) not in (int, float):
            return None
        peaks[tx] = float(peak)
    return peaks


def round_trip(engine: dict) -> int:
    """The D5(b) probe's round trip in samples, 0 while none was measured."""
    value = engine.get("loopback_samples")
    return value if type(value) is int and value > 0 else 0


def engine_waiting(reply, sha: str, spawns: int) -> str | None:
    """Why the engine restarted inside the job does not play yet, from one
    `iemmode status` reply (pure); None once it does."""
    if not isinstance(reply, dict) or reply.get("ok") is not True:
        return "the guard's status did not answer ok"
    engine = reply.get("engine")
    if not isinstance(engine, dict):
        return "no engine runs"
    if engine.get("build") != sha:
        return f"the engine's build is {engine.get('build')!r}, not {sha}"
    started = engine.get("spawns")
    if type(started) is not int or started <= spawns:
        return "the engine has not been started again"
    return iempc_soak.playing_refusal(engine, "the loop test")


def pc_refusal(reply) -> tuple[str | None, str | None, int]:
    """From the first `iemmode status` (pure): why the PC cannot run the loop
    (None when it can), the active bundle and the engine's start count."""
    sha = iempc_soak.active_bundle(reply) if isinstance(reply, dict) else None
    if sha is None:
        return iempc_soak.settled_refusal(reply) or "no active bundle", None, 0
    why = iempc_soak.runs_refusal(reply, sha, "the loop test")
    spawns = reply["engine"].get("spawns") if why is None else None
    if why is None and type(spawns) is not int:
        why = "the guard's status names no engine start count"
    return why, sha, spawns if type(spawns) is int else 0


def wait_engine(ctx, ip, sha: str, spawns: int, slot: int) -> None:
    deadline = time.monotonic() + ENGINE_WAIT_S
    while True:
        code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
        why = engine_waiting(reply if code == 0 else None, sha, spawns)
        if why is None:
            peaks = spare_peaks(reply["engine"])
            if peaks is None or slot not in peaks:
                raise ip.StepError("gen3-loop: the spare output --slot names is not among the restarted engine's HIL "
                                   "outputs (the site's [guard] hil_tx)")
            return
        if time.monotonic() > deadline:
            raise ip.StepError(f"gen3-loop: the engine restarted in the HIL job does not play within "
                               f"{ENGINE_WAIT_S} s: {why}")
        ip.pause(ctx, ip.POLL_S)


def number(value: float) -> str:
    return f"{value:g}"


def burst(ctx, ip, slot: int, signal: Signal) -> tuple[int, bool]:
    """One test-signal burst: its round trip, and whether X carried it and a
    later status showed every spare output silent (heard and ended)."""
    words = ["test-signal", signal.input, number(signal.dbfs), number(signal.ttl)]
    code, reply, raw = ip.iemmode(ctx.env, words, ip.STATUS_S, ctx.watch(abandon=True))
    if code != 0:
        raise ip.StepError(f"gen3-loop: iemmode test-signal refused (exit {code}): {said(reply, raw)}")
    deadline = time.monotonic() + signal.ttl + BURST_WAIT_S
    heard, trip = False, 0
    while True:
        code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
        engine = reply.get("engine") if code == 0 and isinstance(reply, dict) else None
        peaks = spare_peaks(engine)
        if peaks is not None:
            trip = round_trip(engine)
            if peaks.get(slot, 0.0) > 0:
                heard = True
            elif heard and all(p == 0 for p in peaks.values()):
                return trip, True
        if time.monotonic() > deadline:
            return trip, False
        ip.pause(ctx, ip.POLL_S)


def in_job(ctx, ip, host: str, sha: str, spawns: int, offsets: tuple[int, int], n: int, slot: int,
           signal: Signal) -> tuple[list[list[tuple[int, bool]]], dict, dict]:
    code, reply, raw = ip.iemmode(ctx.env, ["activate", sha], ip.SWITCH_S, ctx.watch(abandon=False))
    if code != 0:
        raise ip.StepError(f"gen3-loop: iemmode activate {sha} failed in the HIL job (exit {code}): "
                           f"{said(reply, raw)}")
    wait_engine(ctx, ip, sha, spawns, slot)
    before = counts_of(ip, ctx, host)
    bursts = []
    for offset in offsets:
        set_echo(ip, ctx, host, offset)
        bursts.append([burst(ctx, ip, slot, signal) for _ in range(n)])
    return bursts, before, counts_of(ip, ctx, host)


def end_job(ctx, ip, run: int) -> int | None:
    """`iemmode job-end <run>`: its exit code, None when it was not asked
    (the flag: the switch out of dev ends the job) or failed. Never raises."""
    if ip.event_now():
        return None
    try:
        code, reply, raw = ip.iemmode(ctx.env, ["job-end", str(run)], ip.STATUS_S, ctx.watch(abandon=False))
    except (ip.StepError, ip.EventNow) as e:
        print(f"iempc: WARNING: iemmode job-end {run} failed ({e}): end the HIL job by hand (iemmode job-end "
              f"{run})", file=sys.stderr, flush=True)
        return None
    if code != 0:
        print(f"iempc: WARNING: iemmode job-end {run} answered exit {code} ({said(reply, raw)}): end the HIL job by "
              f"hand (iemmode job-end {run})", file=sys.stderr, flush=True)
    return code


# ---- the verdict ----

def verdict(offsets: tuple[int, int], bursts: list[list[tuple[int, bool]]], before: dict[str, int],
            after: dict[str, int]) -> dict:
    """The loop's verdict (pure): numbers and the fixed words of FAILURES."""
    f1, f2 = offsets
    trips = [[rt for rt, _ in at] for at in bursts]
    failed = []
    not_heard = sum(1 for at in bursts for _, ok in at if not ok)
    if not_heard:
        failed.append("not_heard")
    if any(rt <= 0 for at in trips for rt in at):
        failed.append("no_measurement")
    equal = all(at and len(set(at)) == 1 for at in trips)
    if not equal:
        failed.append("unequal")
    difference = trips[1][0] - trips[0][0] if equal and all(at[0] > 0 for at in trips) else None
    if difference is not None and difference != f2 - f1:
        failed.append("difference")
    delta = {k: after[k] - before[k] for k in COUNTS if k != "ready"}
    restarted = any(v < 0 for v in delta.values())
    if restarted:
        failed.append("restarted")
    if before["ready"] != 1 or after["ready"] != 1:
        failed.append("not_ready")
    if not restarted:
        for word, key in (("holes", "holes"), ("late", "late"), ("ring_late", "ring_late")):
            if delta[key]:
                failed.append(word)
        if delta["re_anchors"] or delta["epoch"]:
            failed.append("re_anchor")
    return {
        "pass": 0 if failed else 1,
        "offsets": [f1, f2],
        "bursts": len(trips[0]),
        "round_trips": trips,
        "not_heard": not_heard,
        "difference": difference,
        "expected": f2 - f1,
        "restarted": 1 if restarted else 0,
        **{k: (None if restarted else delta[k]) for k in ("holes", "late", "ring_late", "re_anchors", "epoch")},
        "failed": failed,
    }


# ---- the command ----

def run(ctx, ip) -> int:
    """`iempc gen3-loop` (dev time, locked)."""
    offsets, n, slot, signal = checked(ip, ctx.args)
    host = gen3_host(ip, ctx.env)
    ip.refuse_open_window("gen3-loop")
    try:
        status = device_status(ip, ctx, host)
    except ip.StepError as e:
        raise ip.Refused(f"no loop test: the Gen 3 device did not answer ({e}) (nothing was changed)") from None
    why = device_refusal(status)
    if why:
        raise ip.Refused(f"no loop test: the Gen 3 device: {why} (nothing was changed)")
    code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    why, sha, spawns = pc_refusal(reply) if code == 0 else (f"iemmode status failed (exit {code})", None, 0)
    if why:
        raise ip.Refused(f"no loop test: {why} (nothing was changed)")
    why = iempc_live.hil_refusal(ip, ip.current_entry(), dt.datetime.now().astimezone())
    if why:
        raise ip.Refused(f"no loop test: {why}: wait for it to end (its hil/iem-pc result), then run the loop test "
                         "(nothing was changed)")
    job = job_run()
    code, reply, raw = ip.iemmode(ctx.env, ["job-begin", str(job)], ip.STATUS_S, ctx.watch(abandon=False))
    if code != 0:
        raise ip.Refused(f"no loop test: iemmode job-begin refused (exit {code}): {said(reply, raw)} (nothing was "
                         "changed: no HIL job, no source)")
    try:
        bursts, before, after = in_job(ctx, ip, host, sha, spawns, offsets, n, slot, signal)
    except ip.EventNow:
        raise   # the switch out of dev ends the job; nothing may delay the event path
    except BaseException:
        end_job(ctx, ip, job)
        raise
    ended = end_job(ctx, ip, job)
    doc = verdict(offsets, bursts, before, after)
    ip.emit({"gen3_loop": {**doc, "job": job, "job_end": ended}})
    return 0 if doc["pass"] == 1 and ended == 0 else 1
