"""The Gen 3 box's two loops through the IEM card (#72): `iempc gen3-loop`
(phase B) and `iempc gen3-mix` (phase C, after the loop's description).

`iempc gen3-loop --offsets F1,F2 --bursts N --slot X` (#72 phase B, plan
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

`iempc gen3-mix --slot X --member <id> --box-input <id> --bursts N [--input
mic1] [--dbfs -30] [--ttl 2]` (#72 phase C, plan
docs/superpowers/plans/2026-10-26-gen3-phase-c-engine-on-linux.md Task 8):
the same loop, now through a member's mix in the box's engine with the
browser in it (the plan's Live 3). Our device's RX R (the box input's first
RX channel in the test site's `[engine]` table, the file deploy.sh put on
the box) receives the card's spare HIL TX X, and the engine's TX T (the
member's mix's first TX channel there) goes to the card's spare HIL returns.
The engine's output stamped t is its input stamped t - D (D: the device
status's `engine.offset`), so `ctl source T echo R D` gives T the same delay
without the mix: the echo's round trip is the engine's when the test mix adds
no delay (0 dB, centre pan, flat EQ, the limiter under its limit; pinned on
loopback by the engine's dante_loopback test). The same Spec, job and flag
rules as `gen3-loop`; in this order:

0. iempc.py's call site: refused first while a live run or a soak of this
   dev entry may still run (`iempc_live.refuse_while_running`).
1. Before any call: the arguments; R and T from the test site, `--member` a
   person of the test roster (an id either file lacks refuses); GEN3_HOST and
   GEN3_UI_URL (the box's UI, `http://<box>:8080/`) in the private env; no
   open S1a/S1c window.
2. The Gen 3 device's status (`iem-dante-dev ctl status` reads the
   engine-hosted device's control socket): its media clock ready, the
   engine's pump on it (`engine.media` 1) with an echo offset D, RX R
   receiving, a live TX flow and T not silent (the status names no flow's
   channels: a flow without T fails as not heard, never passes). Then
   `iemmode status` and no ops HIL run, as gen3-loop, and the box's build
   (`iem-engine --version`: its 40-hex commit, LIVE_SHA).
3. An engineer and a member token minted on the box as its service user
   (`sudo -n -u iemmixer`, #72 phase D Task 8) with its server's own JWT
   secret (`iem-soakclient token`, TOKEN_SECONDS) in the service user's mint
   folder, each read back over ssh's stdout (`cat`) into a new owner-only
   file of a folder of this box's state (owner-only) and removed on the box
   right after, whatever happened; their `{engineer, member}` file
   (LIVE_TOKENS) goes with its folder on every exit. A token is never on a
   command line and never printed; no PIN anywhere. Each of these commands
   is a line of scripts/gen3/sudo-lines.txt.
4. One HIL job (`iemmode job-begin`; ended as gen3-loop ends it): the engine
   restarted with the HIL flags, the device's counters, `ctl source T echo R
   D` and N bursts (the echo's round trips), `ctl source T engine`, the
   browser step "closes" (the test input's level off in the member's mix)
   and N bursts (none may be heard), "opens" (the input at 0 dB and
   unmuted, IEM VOL at 0 dB and unmuted) and N bursts (the engine's round
   trips), "closes" again, the counters again. A browser step is one test of
   `e2e/tests/gen3/member-mix.spec.ts` (`npx playwright test --config
   playwright.gen3.config.ts --grep '<title>$'` in e2e/, its output on
   stderr) with LIVE_BASE_URL (GEN3_UI_URL), LIVE_SHA, LIVE_TOKENS,
   LIVE_MEMBER (--member) and LIVE_TEST_INPUT (--box-input); one that fails
   ends the bursts there (the verdict's `browser`).
5. One JSON line `{"gen3_mix": {...}}` (`mix_verdict`, pure: numbers and the
   fixed words of MIX_FAILURES): pass when every echo and engine burst was
   heard and ended, each set equal and above 0, the engine's minus the
   echo's exactly 0, no closed burst heard, and between the two counter
   reads no restart, the clock ready and the pump on it, no hole (RX R's
   ring, the TX flows), late packet (the RX flows), late ring sample (RX R),
   pump re-bootstrap or re-anchor (count or epoch). Exit 0 when it passes
   and the job ended.

T is the engine's again at the end; an echo a failure left is runtime only
(the stack's next start makes every engine channel the engine's). X is
never printed (P6).

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import datetime as dt
import json
import math
import os
import re
import shutil
import sys
import tempfile
import time
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

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
ID = re.compile(r"[A-Za-z0-9_-]{1,64}")   # an engine input's or a person's id, as the site writes it
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
class Words:
    """How a command names itself in what the shared steps say."""
    cmd: str    # the command: the head of a failure
    test: str   # what the PC's engine must play for


LOOP = Words("gen3-loop", "the loop test")
MIX = Words("gen3-mix", "the mix test")


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


def bursts_of(ip, args, words: Words) -> int:
    if type(args.bursts) is not int or not BURSTS_MIN <= args.bursts <= BURSTS_MAX:
        raise ip.Refused(f"{words.cmd}: --bursts must be {BURSTS_MIN}..{BURSTS_MAX}")
    return args.bursts


def slot_of(ip, args, words: Words) -> int:
    if type(args.slot) is not int or not 1 <= args.slot <= 65535:
        raise ip.Refused(f"{words.cmd}: --slot must be the card's spare HIL output, a channel number of 1..65535")
    return args.slot


def signal_of(ip, args, words: Words) -> Signal:
    """The PC's test-signal burst (`--input`, `--dbfs`, `--ttl`), or Refused."""
    if not ID.fullmatch(args.input or ""):
        raise ip.Refused(f"{words.cmd}: --input must be an engine input id (1 to 64 of A-Z a-z 0-9 _ -)")
    if not (isinstance(args.dbfs, float) and math.isfinite(args.dbfs) and args.dbfs <= DBFS_MAX):
        raise ip.Refused(f"{words.cmd}: --dbfs must be at most {DBFS_MAX:g} (the HIL ceiling)")
    if not (isinstance(args.ttl, float) and 0 < args.ttl <= TTL_MAX):
        raise ip.Refused(f"{words.cmd}: --ttl must be above 0 and at most {TTL_MAX:g} s (the guard's HIL limit)")
    return Signal(args.input, args.dbfs, args.ttl)


def checked(ip, args) -> tuple[tuple[int, int], int, int, Signal]:
    """The command line's values, or Refused before any call."""
    offsets = offsets_of(ip, args.offsets)
    n = bursts_of(ip, args, LOOP)
    return offsets, n, slot_of(ip, args, LOOP), signal_of(ip, args, LOOP)


def gen3_host(ip, env: dict[str, str], words: Words) -> str:
    host = env.get("GEN3_HOST", "")
    if not host:
        raise ip.Refused(f"{words.cmd}: no GEN3_HOST in the private env (the Gen 3 box's ssh destination)")
    if host.startswith("-"):
        raise ip.Refused(f"{words.cmd}: GEN3_HOST must be an ssh destination, not an option")
    return host


# ---- the Gen 3 device ----

def ctl(ip, host: str, words: list[str], event: str) -> str:
    """`iem-dante-dev ctl <words>` on the Gen 3 box, its stdout. Bounded by
    CTL_S, the flag watched as `event` says (iempc_core.guarded)."""
    return ip.guarded([*SSH, host, DANTE_BIN, "ctl", *words], "", CTL_S, event)


def device_status(ip, ctx, host: str, words: Words) -> dict:
    out = ctl(ip, host, ["status"], ctx.watch(abandon=True))
    lines = [line for line in out.splitlines() if line.strip()]
    try:
        doc = json.loads(lines[-1]) if lines else None
    except ValueError:
        doc = None
    if not isinstance(doc, dict):
        raise ip.StepError(f"{words.cmd}: the Gen 3 device's status is not a JSON object")
    return doc


def set_source(ip, ctx, host: str, tx: int, source: list[str]) -> None:
    """`ctl source <tx> <source...>`. The answer goes to stderr (`1>&2`, the
    box's shell): ctl's exit 0 is its `ok`, and a refusal's `error: <reason>`
    lands in the StepError, which quotes stderr."""
    ctl(ip, host, ["source", str(tx), *source, "1>&2"], ctx.watch(abandon=False))


def set_echo(ip, ctx, host: str, offset: int) -> None:
    """TX 1 echoes RX 1 `offset` samples later."""
    set_source(ip, ctx, host, TX, ["echo", str(RX), str(offset)])


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


def channel(status: dict, rx: int) -> dict | None:
    """RX `rx`'s row of the status's `channels` (none while it is not
    subscribed and its ring counted nothing)."""
    return next((c for c in rows(status, "channels") if c.get("rx") == rx), None)


def window_counts(status: dict, rx: int) -> dict[str, int]:
    """The loop window's counters from one Gen 3 status (pure); ValueError
    when it is not the device's shape. `holes`: RX `rx`'s ring and every TX
    flow (an echo's or the engine's samples); `late`: every RX flow's late
    packets; `ring_late`: RX `rx`'s ring, samples behind its reader."""
    clock = status.get("clock")
    ch = channel(status, rx)
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
        counts = window_counts(status, RX)
        ch = channel(status, RX)
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
        return window_counts(device_status(ip, ctx, host, LOOP), RX)
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


def engine_waiting(reply, sha: str, spawns: int, words: Words) -> str | None:
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
    return iempc_soak.playing_refusal(engine, words.test)


def pc_refusal(reply, words: Words) -> tuple[str | None, str | None, int]:
    """From the first `iemmode status` (pure): why the PC cannot run the test
    (None when it can), the active bundle and the engine's start count."""
    sha = iempc_soak.active_bundle(reply) if isinstance(reply, dict) else None
    if sha is None:
        return iempc_soak.settled_refusal(reply) or "no active bundle", None, 0
    why = iempc_soak.runs_refusal(reply, sha, words.test)
    spawns = reply["engine"].get("spawns") if why is None else None
    if why is None and type(spawns) is not int:
        why = "the guard's status names no engine start count"
    return why, sha, spawns if type(spawns) is int else 0


def wait_engine(ctx, ip, sha: str, spawns: int, slot: int, words: Words) -> None:
    deadline = time.monotonic() + ENGINE_WAIT_S
    while True:
        code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
        why = engine_waiting(reply if code == 0 else None, sha, spawns, words)
        if why is None:
            peaks = spare_peaks(reply["engine"])
            if peaks is None or slot not in peaks:
                raise ip.StepError(f"{words.cmd}: the spare output --slot names is not among the restarted engine's "
                                   "HIL outputs (the site's [guard] hil_tx)")
            return
        if time.monotonic() > deadline:
            raise ip.StepError(f"{words.cmd}: the engine restarted in the HIL job does not play within "
                               f"{ENGINE_WAIT_S} s: {why}")
        ip.pause(ctx, ip.POLL_S)


def number(value: float) -> str:
    return f"{value:g}"


def burst(ctx, ip, slot: int, signal: Signal, words: Words) -> tuple[int, bool]:
    """One test-signal burst: its round trip, and whether X carried it and a
    later status showed every spare output silent (heard and ended)."""
    args = ["test-signal", signal.input, number(signal.dbfs), number(signal.ttl)]
    code, reply, raw = ip.iemmode(ctx.env, args, ip.STATUS_S, ctx.watch(abandon=True))
    if code != 0:
        raise ip.StepError(f"{words.cmd}: iemmode test-signal refused (exit {code}): {said(reply, raw)}")
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


def restart_in_job(ctx, ip, sha: str, spawns: int, slot: int, words: Words) -> None:
    """`iemmode activate` of the active bundle inside the HIL job (the guard
    restarts the engine with the HIL flags), then the wait until it plays."""
    code, reply, raw = ip.iemmode(ctx.env, ["activate", sha], ip.SWITCH_S, ctx.watch(abandon=False))
    if code != 0:
        raise ip.StepError(f"{words.cmd}: iemmode activate {sha} failed in the HIL job (exit {code}): "
                           f"{said(reply, raw)}")
    wait_engine(ctx, ip, sha, spawns, slot, words)


def in_job(ctx, ip, host: str, sha: str, spawns: int, offsets: tuple[int, int], n: int, slot: int,
           signal: Signal) -> tuple[list[list[tuple[int, bool]]], dict, dict]:
    restart_in_job(ctx, ip, sha, spawns, slot, LOOP)
    before = counts_of(ip, ctx, host)
    bursts = []
    for offset in offsets:
        set_echo(ip, ctx, host, offset)
        bursts.append([burst(ctx, ip, slot, signal, LOOP) for _ in range(n)])
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
    host = gen3_host(ip, ctx.env, LOOP)
    ip.refuse_open_window("gen3-loop")
    try:
        status = device_status(ip, ctx, host, LOOP)
    except ip.StepError as e:
        raise ip.Refused(f"no loop test: the Gen 3 device did not answer ({e}) (nothing was changed)") from None
    why = device_refusal(status)
    if why:
        raise ip.Refused(f"no loop test: the Gen 3 device: {why} (nothing was changed)")
    code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    why, sha, spawns = pc_refusal(reply, LOOP) if code == 0 else (f"iemmode status failed (exit {code})", None, 0)
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


# ==== gen3-mix (#72 phase C, plan Task 8) ====

REPO = Path(__file__).resolve().parent.parent.parent
SITE = REPO / "config" / "test-site.toml"   # deploy.sh puts it on the box as the server's and the engine's site
ROSTER = REPO / "config" / "test-roster.toml"   # and this beside it as the roster
E2E = REPO / "e2e"
ENGINE_BIN = "/opt/iemmixer/bin/iem-engine"
SOAKCLIENT = "/opt/iemmixer/bin/iem-soakclient"
# The path of the box server's JWT key file, beside its site file.
SIGNER_PATH = "/var/lib/iemmixer/site/secrets/jwt_secret"
# The service user's mint folder on the box (#72 phase D Task 8; provision
# makes it, 0700 iemmixer): each token there only between its mint and its read.
BOX_TOKENS = "/var/lib/iemmixer/mint"
# Every call on the box's tokens runs as the service user, the jwt_secret's
# owner: a line of scripts/gen3/sudo-lines.txt each.
AS_SERVICE = ("sudo", "-n", "-u", "iemmixer")
TOKENS = ("engineer", "member")
TOKEN_SECONDS = 900   # the run: the job, 3 N bursts and four browser steps
JWT = re.compile(r"[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+")
BUILD_LINE = re.compile(r"iem-engine \S+\+([0-9a-f]{40})")   # `iem-engine <version>+<GITHUB_SHA>`
ENGINE = "engine"   # the status's word for an engine source
SPEC_CONFIG = "playwright.gen3.config.ts"
SPEC_S = 300   # one browser step: npx, Chromium, the page, the runner's socket (the config's test timeout is 120 s)
# The browser steps (e2e/tests/gen3/member-mix.spec.ts titles, matched whole by --grep).
OPENS = "the member opens the test input in their mix"
CLOSES = "the member closes the test input in their mix"
# The PIN variables of the mock E2E run: never in a browser step's environment.
NO_PINS = ("E2E_ENGINEER_PIN", "E2E_MEMBER_PIN")
# The failures a mix verdict names, in its order.
MIX_FAILURES = ("browser", "not_heard", "no_measurement", "unequal", "difference", "closed_heard", "restarted",
                "not_ready", "not_media", "holes", "late", "ring_late", "rebootstraps", "re_anchor")
# The counters whose difference over the window is judged (each only ever grows while the device runs).
MIX_COUNTS = ("polls", "re_anchors", "epoch", "holes", "late", "ring_late", "blocks", "rebootstraps")


@dataclass
class MixRun:
    """What the job measured: each phase's bursts (round trip, heard and
    ended), and whether every browser step passed."""
    echo: list[tuple[int, bool]] = field(default_factory=list)
    closed: list[tuple[int, bool]] = field(default_factory=list)
    engine: list[tuple[int, bool]] = field(default_factory=list)
    browser: bool = True


# ---- arguments and the test site ----

def checked_mix(ip, args) -> tuple[int, str, str, int, Signal]:
    """The command line's values, or Refused before any call."""
    slot = slot_of(ip, args, MIX)
    if not ID.fullmatch(args.member or ""):
        raise ip.Refused("gen3-mix: --member must be a person's id (1 to 64 of A-Z a-z 0-9 _ -)")
    if not ID.fullmatch(args.box_input or ""):
        raise ip.Refused("gen3-mix: --box-input must be an engine input id (1 to 64 of A-Z a-z 0-9 _ -)")
    return slot, args.member, args.box_input, bursts_of(ip, args, MIX), signal_of(ip, args, MIX)


def first_channel(value) -> int | None:
    """The first channel number of a `rx`/`tx` list, None when it has none."""
    first = value[0] if isinstance(value, list) and value else None
    return first if type(first) is int and 1 <= first <= 65535 else None


def site_channels(site: dict, roster: dict, member: str, box_input: str) -> tuple[int, int]:
    """R, the box input's first RX channel, and T, the member's mix's first TX
    channel, from the site's `[engine]` table; the member must be a person of
    the roster (pure). ValueError names what is missing."""
    engine = site.get("engine")
    if not isinstance(engine, dict):
        raise ValueError("the test site has no [engine] table")
    people = roster.get("people")
    if not isinstance(people, list) or not any(isinstance(p, dict) and p.get("id") == member for p in people):
        raise ValueError("--member is no person of the test roster")
    mixes, inputs = engine.get("mixes"), engine.get("inputs")
    tx = next((first_channel(m.get("tx")) for m in mixes or [] if isinstance(m, dict) and m.get("id") == member), None)
    if tx is None:
        raise ValueError("--member has no mix with a TX channel in the test site's [engine] table")
    rx = next((first_channel(i.get("rx")) for i in inputs or [] if isinstance(i, dict) and i.get("id") == box_input),
              None)
    if rx is None:
        raise ValueError("--box-input is no input with an RX channel in the test site's [engine] table")
    return rx, tx


def mix_channels(ip, member: str, box_input: str) -> tuple[int, int]:
    """`site_channels` of this checkout's test site and roster, or Refused."""
    try:
        site = tomllib.loads(SITE.read_text(encoding="utf-8"))
        roster = tomllib.loads(ROSTER.read_text(encoding="utf-8"))
        return site_channels(site, roster, member, box_input)
    except (OSError, UnicodeError, tomllib.TOMLDecodeError) as e:
        raise ip.Refused(f"gen3-mix: the test site or roster cannot be read ({type(e).__name__})") from None
    except ValueError as e:
        raise ip.Refused(f"gen3-mix: {e}") from None


def ui_url(ip, env: dict[str, str]) -> str:
    """GEN3_UI_URL of the private env (never printed): the browser's origin."""
    url = env.get("GEN3_UI_URL", "")
    if not url:
        raise ip.Refused("gen3-mix: no GEN3_UI_URL in the private env (the box's UI, http://<box>:8080/)")
    if not url.startswith(("http://", "https://")):
        raise ip.Refused("gen3-mix: GEN3_UI_URL must be an http:// or https:// URL")
    return url


# ---- the box ----

def engine_offset(status: dict) -> int:
    """D, the engine's output offset (`engine.offset`); ValueError when the
    status has none or it is no echo offset."""
    engine = status.get("engine")
    value = engine.get("offset") if isinstance(engine, dict) else None
    if type(value) is not int or not 1 <= value <= OFFSET_MAX:
        raise ValueError("engine.offset")
    return value


def mix_counts(status: dict, rx: int) -> dict[str, int]:
    """The mix window's counters from one status of the engine-hosted device
    (pure): the loop's for RX `rx`, and the pump's `media`, `blocks` and
    `rebootstraps`; ValueError when it is not that shape."""
    engine = status.get("engine")
    return {**window_counts(status, rx), "media": count(engine, "media"), "blocks": count(engine, "blocks"),
            "rebootstraps": count(engine, "rebootstraps")}


def mix_device_refusal(status: dict, rx: int, tx: int) -> str | None:
    """Why the engine-hosted device cannot carry the mix test now, from its
    status (pure); None when it can."""
    try:
        counts = mix_counts(status, rx)
        ch = channel(status, rx)
        live_tx = any(t.get("live") == 1 for t in rows(status, "tx"))
        sounding = any(s.get("tx") == tx for s in rows(status, "sources"))
    except ValueError:
        return "its status is not the engine-hosted device's (no engine object)"
    if counts["ready"] != 1:
        return "its media clock is not ready"
    if counts["media"] != 1:
        return "the engine's pump is not on the media clock"
    try:
        engine_offset(status)
    except ValueError:
        return f"the engine's offset is no echo offset of 1..{OFFSET_MAX}"
    if ch is None or ch.get("status") != RECEIVING:
        return f"RX {rx} is not receiving (subscribe it to the card's spare HIL TX first)"
    if not live_tx:
        return f"no receiver takes its TX (no live TX flow: the card's spare HIL returns are not subscribed to TX {tx})"
    if not sounding:
        return f"TX {tx} is silent (no engine on it, or a test source set it silent)"
    return None


def mix_counts_of(ip, ctx, host: str, rx: int) -> dict[str, int]:
    try:
        return mix_counts(device_status(ip, ctx, host, MIX), rx)
    except ValueError as e:
        raise ip.StepError(f"gen3-mix: the Gen 3 device's status lacks {e}") from None


def box_build(ip, ctx, host: str) -> str:
    """The commit the box's engine was built from (`iem-engine --version`);
    StepError when it names none (a local build)."""
    out = ip.guarded([*SSH, host, ENGINE_BIN, "--version"], "", CTL_S, ctx.watch(abandon=True))
    lines = [line.strip() for line in out.splitlines() if line.strip()]
    m = BUILD_LINE.fullmatch(lines[-1]) if lines else None
    if not m:
        raise ip.StepError("the box's iem-engine --version names no CI build (<version>+<40 hex>)")
    return m.group(1)


# ---- the tokens ----

def box_token(name: str) -> str:
    return f"{BOX_TOKENS}/{name}.token"


def mint_tokens(ip, ctx, host: str, member: str, folder: Path) -> Path:
    """An engineer and a member token minted on the box as the service user
    with its server's own JWT secret, each read back over ssh's stdout into a
    new owner-only file of `folder` (this box's, owner-only) and removed on
    the box whatever happened; returns their `{engineer, member}` file
    (LIVE_TOKENS), owner-only. A token never travels on a command line and is
    never printed. One ssh call per command (one `sudo -n` each, as the sudo
    template lists it): the removal of any left from an earlier run (the
    mint never writes over a file), the two mints, the two reads."""
    remove = [*SSH, host, *AS_SERVICE, "rm", "-f", *map(box_token, TOKENS)]
    try:
        ip.guarded(remove, "", CTL_S, ctx.watch(abandon=False))
        for name in TOKENS:
            sub = ["--sub", "engineer", "--engineer"] if name == "engineer" else ["--sub", member]
            ip.guarded([*SSH, host, *AS_SERVICE, SOAKCLIENT, "token", "--jwt-secret-file", SIGNER_PATH, *sub,
                        "--seconds", str(TOKEN_SECONDS), "--out", box_token(name)], "", CTL_S,
                       ctx.watch(abandon=False))
        for name in TOKENS:
            token = ip.guarded([*SSH, host, *AS_SERVICE, "cat", box_token(name)], "", CTL_S,
                               ctx.watch(abandon=False))
            keep_token(ip, folder / f"{name}.token", token, name)
    finally:
        remove_box_tokens(ip, host)
    return tokens_file(ip, folder)


def keep_token(ip, path: Path, token: str, name: str) -> None:
    """A token read from the box into a new owner-only file (never over
    one); StepError naming no token when it cannot be written."""
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(token)
    except OSError as e:
        raise ip.StepError(f"gen3-mix: the {name} token read from the box was not kept "
                           f"({e.strerror or type(e).__name__})") from None


def remove_box_tokens(ip, host: str) -> None:
    """`rm -f` of the minted tokens on the box as the service user, after any
    outcome (a flag too: a credential is never left there). A failure is a
    warning naming the command to run by hand; it never raises."""
    words = [*AS_SERVICE, "rm", "-f", *map(box_token, TOKENS)]
    try:
        ip.guarded([*SSH, host, *words], "", CTL_S, "ignore")
    except ip.StepError as e:
        print(f"iempc: WARNING: the tokens minted on the Gen 3 box were not removed ({e}): remove them by hand "
              f"(ssh <GEN3_HOST> {' '.join(words)})", file=sys.stderr, flush=True)


def tokens_file(ip, folder: Path) -> Path:
    """The copied tokens as `{engineer, member}` in `folder/tokens.json`, a
    new owner-only file; each copy is removed once read. StepError (naming no
    token) when one is not JWT-shaped."""
    tokens = {}
    for name in TOKENS:
        copy = folder / f"{name}.token"
        try:
            text = copy.read_text(encoding="utf-8").strip()
        except (OSError, UnicodeError):
            text = ""
        copy.unlink(missing_ok=True)
        if not JWT.fullmatch(text):
            raise ip.StepError(f"gen3-mix: the {name} token copied from the box is not a JWT (it is not shown)")
        tokens[name] = text
    path = folder / "tokens.json"
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(tokens, f)
    return path


def drop_folder(folder: Path) -> None:
    """The tokens' folder on this box goes on every exit; one left is a warning."""
    shutil.rmtree(folder, ignore_errors=True)
    if folder.exists():
        print(f"iempc: WARNING: {folder} (the browser's tokens) was not removed: remove it by hand", file=sys.stderr,
              flush=True)


# ---- the browser ----

def browser(ctx, ip, title: str, spec_env: dict[str, str]) -> None:
    """One test of the Gen 3 spec in e2e/ with the run's LIVE_* values and no
    PIN; Playwright's report goes to stderr (`>&2`), so a failure's StepError
    quotes its end. A flag leaves it to end by itself (the event's stop ends
    the box's stack under it) and raises EventNow at once."""
    env = {k: v for k, v in os.environ.items() if k not in NO_PINS}
    env.update(spec_env)
    cmd = ["sh", "-c", 'exec npx playwright test --config "$1" --grep "$2" >&2', "sh", SPEC_CONFIG, f"{title}$"]
    ip.guarded(cmd, "", SPEC_S, ctx.watch(abandon=True), env=env, cwd=str(E2E))


def browser_passed(ctx, ip, title: str, spec_env: dict[str, str]) -> bool:
    """`browser`, its failure (a hung one too) a warning: False ends the bursts."""
    try:
        browser(ctx, ip, title, spec_env)
    except ip.StepError as e:
        print(f"iempc: WARNING: gen3-mix: the browser step '{title}' failed: {e}", file=sys.stderr, flush=True)
        return False
    return True


# ---- the job ----

def mix_in_job(ctx, ip, host: str, sha: str, spawns: int, slot: int, signal: Signal, n: int, rx: int, tx: int,
               offset: int, spec_env: dict[str, str]) -> tuple[MixRun, dict, dict]:
    restart_in_job(ctx, ip, sha, spawns, slot, MIX)
    before = mix_counts_of(ip, ctx, host, rx)
    run = MixRun()
    set_source(ip, ctx, host, tx, ["echo", str(rx), str(offset)])
    run.echo = [burst(ctx, ip, slot, signal, MIX) for _ in range(n)]
    set_source(ip, ctx, host, tx, [ENGINE])
    for title, bursts in ((CLOSES, run.closed), (OPENS, run.engine), (CLOSES, None)):
        if not browser_passed(ctx, ip, title, spec_env):
            run.browser = False
            break
        if bursts is not None:
            bursts.extend(burst(ctx, ip, slot, signal, MIX) for _ in range(n))
    return run, before, mix_counts_of(ip, ctx, host, rx)


# ---- the verdict ----

def mix_verdict(n: int, run: MixRun, before: dict[str, int], after: dict[str, int], offset: int) -> dict:
    """The mix test's verdict (pure): numbers and the fixed words of
    MIX_FAILURES. A phase a failed browser step cut short is judged on what
    it measured."""
    echo, engine = [rt for rt, _ in run.echo], [rt for rt, _ in run.engine]
    failed = [] if run.browser else ["browser"]
    not_heard = sum(1 for phase in (run.echo, run.closed, run.engine) for _, ok in phase if not ok)
    if not_heard:
        failed.append("not_heard")
    if any(rt <= 0 for rt in echo + engine):
        failed.append("no_measurement")
    if any(len(set(phase)) > 1 for phase in (echo, engine)):
        failed.append("unequal")
    measured = all(phase and len(set(phase)) == 1 and phase[0] > 0 for phase in (echo, engine))
    difference = engine[0] - echo[0] if measured else None
    if difference:
        failed.append("difference")
    closed_heard = sum(1 for rt, _ in run.closed if rt > 0)
    if closed_heard:
        failed.append("closed_heard")
    delta = {k: after[k] - before[k] for k in MIX_COUNTS}
    restarted = any(v < 0 for v in delta.values())
    if restarted:
        failed.append("restarted")
    if before["ready"] != 1 or after["ready"] != 1:
        failed.append("not_ready")
    if before["media"] != 1 or after["media"] != 1:
        failed.append("not_media")
    if not restarted:
        for word in ("holes", "late", "ring_late", "rebootstraps"):
            if delta[word]:
                failed.append(word)
        if delta["re_anchors"] or delta["epoch"]:
            failed.append("re_anchor")
    return {
        "pass": 0 if failed or difference != 0 else 1,
        "bursts": n,
        "echo": echo,
        "engine": engine,
        "closed_heard": closed_heard,
        "not_heard": not_heard,
        "difference": difference,
        "offset": offset,
        "restarted": 1 if restarted else 0,
        **{k: (None if restarted else delta[k]) for k in ("holes", "late", "ring_late", "rebootstraps", "re_anchors")},
        "failed": failed,
    }


# ---- the command ----

def run_mix(ctx, ip) -> int:
    """`iempc gen3-mix` (dev time, locked)."""
    slot, member, box_input, n, signal = checked_mix(ip, ctx.args)
    rx, tx = mix_channels(ip, member, box_input)
    host = gen3_host(ip, ctx.env, MIX)
    ui = ui_url(ip, ctx.env)
    ip.refuse_open_window("gen3-mix")
    try:
        status = device_status(ip, ctx, host, MIX)
    except ip.StepError as e:
        raise ip.Refused(f"no mix test: the Gen 3 device did not answer ({e}) (nothing was changed)") from None
    why = mix_device_refusal(status, rx, tx)
    if why:
        raise ip.Refused(f"no mix test: the Gen 3 device: {why} (nothing was changed)")
    offset = engine_offset(status)
    code, reply, _ = ip.iemmode(ctx.env, ["status"], ip.STATUS_S, ctx.watch(abandon=True))
    why, sha, spawns = pc_refusal(reply, MIX) if code == 0 else (f"iemmode status failed (exit {code})", None, 0)
    if why:
        raise ip.Refused(f"no mix test: {why} (nothing was changed)")
    why = iempc_live.hil_refusal(ip, ip.current_entry(), dt.datetime.now().astimezone())
    if why:
        raise ip.Refused(f"no mix test: {why}: wait for it to end (its hil/iem-pc result), then run the mix test "
                         "(nothing was changed)")
    try:
        build = box_build(ip, ctx, host)
    except ip.StepError as e:
        raise ip.Refused(f"no mix test: {e} (nothing was changed)") from None
    folder = Path(tempfile.mkdtemp(prefix="gen3-mix-", dir=ip.state_dir()))
    try:
        try:
            tokens = mint_tokens(ip, ctx, host, member, folder)
        except ip.StepError as e:
            raise ip.Refused(f"no mix test: the browser's tokens were not minted ({e}) (nothing was changed: no HIL "
                             "job, no source)") from None
        spec_env = {"LIVE_BASE_URL": ui, "LIVE_SHA": build, "LIVE_TOKENS": str(tokens), "LIVE_MEMBER": member,
                    "LIVE_TEST_INPUT": box_input}
        job = job_run()
        code, reply, raw = ip.iemmode(ctx.env, ["job-begin", str(job)], ip.STATUS_S, ctx.watch(abandon=False))
        if code != 0:
            raise ip.Refused(f"no mix test: iemmode job-begin refused (exit {code}): {said(reply, raw)} (nothing was "
                             "changed: no HIL job, no source)")
        try:
            run, before, after = mix_in_job(ctx, ip, host, sha, spawns, slot, signal, n, rx, tx, offset, spec_env)
        except ip.EventNow:
            raise   # the switch out of dev ends the job; nothing may delay the event path
        except BaseException:
            end_job(ctx, ip, job)
            raise
        ended = end_job(ctx, ip, job)
    finally:
        drop_folder(folder)
    doc = mix_verdict(n, run, before, after, offset)
    ip.emit({"gen3_mix": {**doc, "job": job, "job_end": ended}})
    return 0 if doc["pass"] == 1 and ended == 0 else 1
