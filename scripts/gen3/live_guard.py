#!/usr/bin/env python3
"""The live guard of the Gen 3 device (#72 phase B, plan
docs/superpowers/plans/2026-10-26-gen3-phase-b-audio-through.md Task 8 Step
1): run on the dev box for a whole live session with our device on the
band's Dante network, it stops the device the moment its traffic loops.

    live_guard.py --rate <the device config's rate> [--seconds <n>]

Every second (READ_EVERY_S) it reads `iem-dante-dev ctl status` on the box
over ssh, as the service's user (the one the control socket admits), and
computes per-second rates from the differences between two reads (every
count in the status is cumulative). It runs `sudo -n systemctl stop
iem-server iem-engine iem-dante` (the whole Gen 3 stack, phase C) on the
box at once and exits 1 when:

- any (port, peer, opcode) of the requests our device received, or the sum
  past the status's tables (`uncounted`), runs above REQUEST_RATE (20) a
  second for SUSTAIN_S (3 s): phase A's loop was 2 250 a second, Dante
  Controller settles at one per 10 s;
- our own flow requests (`sent`, opcode 256) to one peer come more than one
  within FLOW_SPAN_S (5 s): the manager's bound is one per 9 s;
- our own ARC reads of a transmitter's channels (`sent`, opcode 4096, once a
  directory read) of one peer come more than one within FLOW_SPAN_S: the
  directory's bound is one per 9 s from the last read's end (plan Task 9b);
- a TX or an RX flow passes PACKET_MARGIN (1.25) x rate / fpp packets a
  second.

It exits 0 when the EVENT-NOW flag appears (the event's own stop runs: "ide
event" stops the service), when --seconds have passed, or on SIGTERM, SIGINT
or SIGHUP. It prints one numbers-only JSON line every 10 s (LINE_EVERY_S) and
one at its end (`end`: why it ended, a fixed word; the trip's numbers).

The packets' judge (#72 phase D, plan
docs/superpowers/plans/2026-10-26-gen3-phase-d-the-box.md Task 3): each RX
flow's `lateness_min_ns` in the status (the least lateness of the
heartbeat's last second in ns: our clock against the stamps the
grandmaster's time put on the audio) goes into the 10-second lines as
`judge`, one entry per RX flow that had one in the line's reads: `flow`,
`min_ns` and `max_ns` (the smallest and the largest of those seconds),
`last_ns` (the last read's) and `reads` (how many reads had one). The end's
line sums up the whole run per RX flow (plan Task 8): `judge` [{flow, n,
min, p1, p50, p99, max, slope_ns_per_s}], every read that had one (n; a
second can be read twice or missed: the guard reads about once a
heartbeat), its percentiles by nearest rank (the value of rank ceil(p n /
100) in the sorted reads) in ns, and the least-squares slope of lateness
over the reads' time in ns a second (null for one read, or reads all at one
time). Reported, never a trip; no `judge` when no RX flow had one.

The box's clock (dantesync's, read by the device; owner, 2026-10-26): the
lines add the last read's `clock_ready`, `clock_holdover`, `clock_holdovers`
(runs of holdover), `clock_re_anchors` and `clock_freq_ppb` (dantesync's
frequency adjustment) when the status's `clock` has them, cumulative where
the status's are. Reported, never a trip; a status without them changes
nothing else.

The rates never read high from the ssh's own timing: a read's status was
taken somewhere between the read's start and its end, so a rate divides by
the widest the gap between two statuses can be (the later read's end minus
the earlier read's start), and a span counts as within 5 s only when that
widest gap is. A read that outlives READ_S is left to end by itself (never
ended by force) and no second one starts beside it; a failed or unreadable
read only counts (the next good one is compared with the last good one). A
read whose counts went back (the service restarted) starts the comparison
again. The stop is bounded by STOP_S and is never retried: a failed one is a
warning naming the command to run by hand.

The box's ssh destination is GEN3_HOST of iempc's private env ($PC_ENV, else
~/.config/iemmixer/iem-pc.env), read as iempc's `load_env` reads it; the flag
is $IEMMIXER_EVENT_NOW, else ~/.config/iemmixer/EVENT-NOW. Exit 2 before any
call: a bad command line, no env file or GEN3_HOST, or the flag already there
(every Gen 3 script refuses during an event).

P6: stdout holds keys, numbers and fixed words only (null for a slope the
judge cannot draw; the status's own peer ids and opcodes, never an address
or a name); stderr names no value of the private env. Stdlib only, runs
under `python3 -I`."""
from __future__ import annotations

import argparse
import json
import math
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

DANTE_BIN = "/opt/iemmixer/bin/iem-dante-dev"
STOP = ("sudo", "-n", "systemctl", "stop", "iem-server", "iem-engine", "iem-dante")
SSH_OPTIONS = ("-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-o", "ServerAliveInterval=1",
               "-o", "ServerAliveCountMax=5")
READ_EVERY_S = 1.0
LINE_EVERY_S = 10.0
READ_S = 10.0   # one status read: the connect (5 s), then the control socket's own 1 s
STOP_S = 20.0   # the stop: the connect, then the unit's own 10 s stop bound
REQUEST_RATE = 20.0
SUSTAIN_S = 3.0
FLOW_REQUEST = 0x0100   # `sent`'s opcode of our flow requests (0x0101: stops)
FLOW_SPAN_S = 5.0
FLOW_REQUESTS_MAX = 1   # within FLOW_SPAN_S, to one peer
ARC_READ = 0x1000   # `sent`'s opcode once an ARC directory read (its pages: 0x2000, 0x2010)
ARC_READS_MAX = 1   # within FLOW_SPAN_S, of one peer
PACKET_MARGIN = 1.25
RATES = (44100, 48000, 88200, 96000, 176400, 192000)
UNCOUNTED = ("uncounted",)
# The status's clock fields the lines carry (the device's, from dantesync's
# time server): all but freq_ppb are counts or flags, never below 0.
CLOCK_COUNTS = ("ready", "holdover", "holdovers", "re_anchors")
CLOCK_SIGNED = ("freq_ppb",)
PERCENTILES = (1, 50, 99)   # the end's judge, by nearest rank
EXIT_TRIPPED = 1
EXIT_USAGE = 2


class Usage(Exception):
    """Refused before any call (exit 2)."""


# ---- one status, as numbers ----

@dataclass(frozen=True)
class Sample:
    """One read of the device's status: `t` the middle of the read on this
    box's monotonic clock and `u` half its length (the status was taken
    within t ± u), then the cumulative counts the guard compares."""
    t: float
    u: float
    requests: dict[tuple, int]   # (port, peer, op) -> n, and UNCOUNTED -> uncounted
    flow_requests: dict[int, int]   # peer -> our flow requests
    arc_reads: dict[int, int]   # peer -> our ARC directory reads
    tx: dict[int, tuple[int, int]]   # flow -> (fpp, packets)
    rx: dict[int, tuple[int, int]]
    tx_live: int
    rx_granted: int
    polls: int
    # The engine's pump (phase C), when an engine runs: (late, missed,
    # rebootstraps), cumulative. Reported in the lines, never a trip: the
    # guard watches our traffic, not the engine's timing.
    pump: tuple[int, int, int] | None = None
    # The packets' judge (phase D): RX flow -> the least lateness of the
    # heartbeat's last second, ns, for the flows whose status has one.
    # Reported in the lines, never a trip.
    judge: dict[int, int] = field(default_factory=dict)
    # The box's clock (phase D Task 8): the CLOCK_COUNTS and CLOCK_SIGNED
    # fields the status's `clock` has, by name. Reported, never a trip.
    clock: dict[str, int] = field(default_factory=dict)


def number(doc: dict, key: str) -> int:
    value = doc.get(key) if isinstance(doc, dict) else None
    if type(value) is not int or value < 0:
        raise ValueError(key)
    return value


def clock_fields(clock: dict) -> dict[str, int]:
    """The clock fields the lines carry, those the status's `clock` has (an
    older device's has fewer: nothing else changes); ValueError for one that
    is there but no integer (or below 0, but for the frequency)."""
    out = {key: number(clock, key) for key in CLOCK_COUNTS if key in clock}
    for key in CLOCK_SIGNED:
        if key in clock:
            if type(clock[key]) is not int:
                raise ValueError(key)
            out[key] = clock[key]
    return out


def entries(status: dict, key: str) -> list[dict]:
    value = status.get(key)
    if not isinstance(value, list) or not all(isinstance(e, dict) for e in value):
        raise ValueError(key)
    return value


def sample(t0: float, t1: float, status: dict) -> Sample:
    """The numbers of one status read between t0 and t1 (pure); ValueError
    when the status is not the device's shape."""
    requests = {(number(e, "port"), number(e, "peer"), number(e, "op")): number(e, "n")
                for e in entries(status, "requests")}
    requests[UNCOUNTED] = number(status, "uncounted")
    flow_requests = {number(e, "peer"): number(e, "n") for e in entries(status, "sent")
                     if number(e, "op") == FLOW_REQUEST}
    arc_reads = {number(e, "peer"): number(e, "n") for e in entries(status, "sent")
                 if number(e, "op") == ARC_READ}
    tx = {number(e, "flow"): (number(e, "fpp"), number(e, "packets")) for e in entries(status, "tx")}
    rx = {number(e, "flow"): (number(e, "fpp"), number(e, "packets")) for e in entries(status, "rx")}
    judge = {number(e, "flow"): number(e, "lateness_min_ns") for e in entries(status, "rx")
             if "lateness_min_ns" in e}
    clock = status.get("clock")
    return Sample(t=(t0 + t1) / 2, u=(t1 - t0) / 2, requests=requests, flow_requests=flow_requests,
                  arc_reads=arc_reads, tx=tx, rx=rx,
                  tx_live=sum(1 for e in entries(status, "tx") if e.get("live") == 1),
                  rx_granted=sum(1 for e in entries(status, "rx") if e.get("granted") == 1),
                  polls=number(clock, "polls"), pump=pump_counts(status), judge=judge,
                  clock=clock_fields(clock))


def pump_counts(status: dict) -> tuple[int, int, int] | None:
    """The engine pump's late, missed and rebootstraps; None without an
    `engine` object (the device alone). An engine object of another shape
    is no sample, as any other part."""
    engine = status.get("engine")
    if engine is None:
        return None
    return number(engine, "late"), number(engine, "missed"), number(engine, "rebootstraps")


def counters(s: Sample) -> dict[tuple, int]:
    """Every cumulative count of a sample, keyed: one that goes back means the
    device started again."""
    out: dict[tuple, int] = {("req", *k): n for k, n in s.requests.items()}
    out.update({("sent", p): n for p, n in s.flow_requests.items()})
    out.update({("arc", p): n for p, n in s.arc_reads.items()})
    out.update({("tx", f): v[1] for f, v in s.tx.items()})
    out.update({("rx", f): v[1] for f, v in s.rx.items()})
    out[("polls",)] = s.polls
    return out


def went_back(a: Sample, b: Sample) -> bool:
    later = counters(b)
    return any(later.get(k, 0) < n for k, n in counters(a).items())


# ---- the decisions ----

@dataclass(frozen=True)
class Trip:
    kind: str   # requests, flow_requests, arc_reads or packets
    numbers: dict


@dataclass(frozen=True)
class Least:
    """One RX flow's judge over some reads: the smallest and the largest of
    the seconds' least lateness read (ns), the last read's, and how many
    reads had one."""
    min_ns: int
    max_ns: int
    last_ns: int
    reads: int

    def then(self, later: Least) -> Least:
        """This one and `later` (the reads after it) together."""
        return Least(min(self.min_ns, later.min_ns), max(self.max_ns, later.max_ns), later.last_ns,
                     self.reads + later.reads)


def judged(into: dict[int, Least], later: dict[int, Least]) -> None:
    """Adds `later`'s judge (per RX flow) to `into`."""
    for flow, least in later.items():
        into[flow] = into[flow].then(least) if flow in into else least


@dataclass
class Peaks:
    """The largest values seen since the last line (what the lines print),
    and the packets' judge of the reads since then: per RX flow its Least
    (the line's) and each read's (t, least lateness in ns) (the end's
    summary)."""
    request_rate: float = 0.0
    flow_requests: int = 0
    packet_ratio: float = 0.0
    arc_reads: int = 0
    judge: dict[int, Least] = field(default_factory=dict)
    series: dict[int, list[tuple[float, int]]] = field(default_factory=dict)


@dataclass
class Watch:
    """The guard's decisions (pure): `feed` one sample after another, a Trip
    when the device's traffic broke a bound."""
    rate: int
    last: Sample | None = None
    over: dict[tuple, float] = field(default_factory=dict)   # request key -> when its run above the rate began
    recent: list[Sample] = field(default_factory=list)   # the samples a 5 s span may reach back to
    peaks: Peaks = field(default_factory=Peaks)
    restarts: int = 0

    def feed(self, s: Sample) -> Trip | None:
        judged(self.peaks.judge, {flow: Least(ns, ns, ns, 1) for flow, ns in s.judge.items()})
        for flow, ns in s.judge.items():
            self.peaks.series.setdefault(flow, []).append((s.t, ns))
        a = self.last
        if a is not None and s.t + s.u <= a.t - a.u:
            return None   # no time between them (a clock that did not move): wait for the next
        self.last = s
        if a is None or went_back(a, s):
            if a is not None:
                self.restarts += 1   # the service started again: compare from here on
            self.over.clear()
            self.recent = [s]
            return None
        width = (s.t + s.u) - (a.t - a.u)
        return self.requests(a, s, width) or self.flows(s) or self.packets(a, s, width)

    def requests(self, a: Sample, s: Sample, width: float) -> Trip | None:
        for key in sorted(set(a.requests) | set(s.requests), key=str):
            rate = (s.requests.get(key, 0) - a.requests.get(key, 0)) / width
            self.peaks.request_rate = max(self.peaks.request_rate, rate)
            if rate <= REQUEST_RATE:
                self.over.pop(key, None)
                continue
            since = self.over.setdefault(key, a.t)
            if s.t - since >= SUSTAIN_S:
                where = {"uncounted": 1} if key == UNCOUNTED else dict(zip(("port", "peer", "op"), key))
                return Trip("requests", {**where, "rate": round(rate, 1), "seconds": round(s.t - since, 1)})
        return None

    def flows(self, s: Sample) -> Trip | None:
        self.recent = [a for a in self.recent if (s.t + s.u) - (a.t - a.u) <= FLOW_SPAN_S] + [s]
        for a in self.recent[:-1]:
            for peer, n in sorted(s.arc_reads.items()):
                more = n - a.arc_reads.get(peer, 0)
                self.peaks.arc_reads = max(self.peaks.arc_reads, more)
                if more > ARC_READS_MAX:
                    span = (s.t + s.u) - (a.t - a.u)
                    return Trip("arc_reads", {"peer": peer, "reads": more, "within_s": round(span, 1)})
            for peer, n in sorted(s.flow_requests.items()):
                more = n - a.flow_requests.get(peer, 0)
                self.peaks.flow_requests = max(self.peaks.flow_requests, more)
                if more > FLOW_REQUESTS_MAX:
                    span = (s.t + s.u) - (a.t - a.u)
                    return Trip("flow_requests", {"peer": peer, "requests": more, "within_s": round(span, 1)})
        return None

    def packets(self, a: Sample, s: Sample, width: float) -> Trip | None:
        for direction, before, now in (("tx", a.tx, s.tx), ("rx", a.rx, s.rx)):
            for flow, (fpp, packets) in sorted(now.items()):
                fpp_before, packets_before = before.get(flow, (0, 0))
                # The lenient limit when a slot's flow changed between the reads.
                fpps = [f for f in (fpp, fpp_before) if f > 0]
                per_second = self.rate / (min(fpps) if fpps else 1)
                pps = (packets - packets_before) / width
                self.peaks.packet_ratio = max(self.peaks.packet_ratio, pps / per_second)
                if pps > PACKET_MARGIN * per_second:
                    return Trip("packets", {"direction": direction, "flow": flow, "pps": round(pps, 1),
                                            "limit": round(PACKET_MARGIN * per_second, 1)})
        return None

    def take_peaks(self) -> Peaks:
        peaks, self.peaks = self.peaks, Peaks()
        return peaks


# ---- the box ----

def env_path() -> Path:
    return Path(os.environ.get("PC_ENV", str(Path.home() / ".config/iemmixer/iem-pc.env")))


def flag_path() -> Path:
    return Path(os.environ.get("IEMMIXER_EVENT_NOW", str(Path.home() / ".config/iemmixer/EVENT-NOW")))


def gen3_host(path: Path) -> str:
    """GEN3_HOST of iempc's private env, read as iempc_core.load_env reads
    the file (comments and blank lines skipped, KEY=VALUE split at the first
    `=`, both sides trimmed, the value's quotes dropped, the last one counts)."""
    if not path.is_file():
        raise Usage(f"{path}: missing (iempc's private env, which names GEN3_HOST)")
    host = ""
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep:
            raise Usage(f"{path}: not KEY=VALUE: {key}")
        if key.strip() == "GEN3_HOST":
            host = value.strip().strip('"')
    if not host:
        raise Usage(f"{path}: no GEN3_HOST (the Gen 3 box's ssh destination)")
    if host.startswith("-"):
        raise Usage(f"{path}: GEN3_HOST must be an ssh destination, not an option")
    return host


def say(text: str) -> None:
    print(f"live_guard: {text}", file=sys.stderr, flush=True)


def emit(doc: dict) -> None:
    print(json.dumps(doc), flush=True)


class Box:
    """The ssh calls to the Gen 3 box: one status read at a time, the stop."""

    def __init__(self, host: str) -> None:
        self.host = host
        self.pending: subprocess.Popen | None = None   # a read past READ_S, left to end by itself
        self.failing = False

    def busy(self) -> bool:
        """A read that outlived its bound still runs: no second one beside it."""
        if self.pending is None:
            return False
        if self.pending.poll() is None:
            return True
        self.pending.communicate()   # it ended: reaped, its late answer dropped
        self.pending = None
        return False

    def read(self) -> dict | None:
        """One status, or None (a failed, unreadable or too slow read)."""
        cmd = ["ssh", *SSH_OPTIONS, self.host, DANTE_BIN, "ctl", "status"]
        try:
            proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                    text=True, encoding="utf-8", errors="replace")
        except OSError as e:
            return self.failed(f"ssh did not start ({e.strerror or type(e).__name__})")
        try:
            out, err = proc.communicate(timeout=READ_S)
        except subprocess.TimeoutExpired:
            self.pending = proc
            return self.failed(f"a status read still runs after {READ_S:g} s (left to end by itself)")
        if proc.returncode != 0:
            return self.failed(f"a status read failed (exit {proc.returncode}): {' '.join(err.split())[-300:]}")
        lines = [line for line in out.splitlines() if line.strip()]
        try:
            doc = json.loads(lines[-1]) if lines else None
        except ValueError:
            doc = None
        if not isinstance(doc, dict):
            return self.failed("a status read gave no JSON object")
        if self.failing:
            say("status reads answer again")
            self.failing = False
        return doc

    def failed(self, why: str) -> None:
        """Says why a read failed, once per run of failures (not every
        second); the read's result is None."""
        if not self.failing:
            say(f"{why}; the guard reads on")
            self.failing = True
        return None

    def stop(self) -> int | None:
        """The device's stop: ssh's exit code, None when it did not end
        within STOP_S (left to end by itself) or did not start."""
        cmd = ["ssh", *SSH_OPTIONS, self.host, *STOP]
        hand = "stop it by hand: ssh <GEN3_HOST> " + " ".join(STOP)
        try:
            proc = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                    text=True, encoding="utf-8", errors="replace", start_new_session=True)
        except OSError as e:
            say(f"WARNING: the stop did not start ({e.strerror or type(e).__name__}): {hand}")
            return None
        try:
            out, _ = proc.communicate(timeout=STOP_S)
        except subprocess.TimeoutExpired:
            say(f"WARNING: the stop still runs after {STOP_S:g} s (left to end by itself): check it, or {hand}")
            return None
        if proc.returncode != 0:
            say(f"WARNING: the stop failed (exit {proc.returncode}): {' '.join(out.split())[-300:]}; {hand}")
        else:
            say("the Gen 3 stack stopped")
        return proc.returncode


# ---- the loop ----

class Clock:
    def now(self) -> float:
        return time.monotonic()

    def sleep(self, seconds: float) -> None:
        time.sleep(seconds)


@dataclass
class Ending:
    """Set by SIGTERM, SIGINT or SIGHUP: the loop ends at its next look."""
    asked: bool = False


@dataclass
class Tally:
    reads: int = 0
    failed: int = 0
    request_rate: float = 0.0
    flow_requests: int = 0
    packet_ratio: float = 0.0
    arc_reads: int = 0
    # The whole run's reads of the packets' judge, per RX flow (t, ns): at
    # most one a second a flow (an event's two hours: 7 200).
    series: dict[int, list[tuple[float, int]]] = field(default_factory=dict)

    def add(self, p: Peaks) -> None:
        self.request_rate = max(self.request_rate, p.request_rate)
        self.flow_requests = max(self.flow_requests, p.flow_requests)
        self.packet_ratio = max(self.packet_ratio, p.packet_ratio)
        self.arc_reads = max(self.arc_reads, p.arc_reads)
        for flow, points in p.series.items():
            self.series.setdefault(flow, []).extend(points)


def figures(t: float, tally: Tally, peaks: Peaks, watch: Watch, judge: dict) -> dict:
    """A line's numbers: `peaks` its largest values, `judge` its packets'
    judge (the 10-second line's per-line entries, the end's summary)."""
    last = watch.last
    return {"t_s": round(t, 1), "reads": tally.reads, "failed": tally.failed,
            "request_rate_max": round(peaks.request_rate, 1), "flow_requests_5s_max": peaks.flow_requests,
            "arc_reads_5s_max": peaks.arc_reads,
            "packet_ratio_max": round(peaks.packet_ratio, 3), "tx_flows": last.tx_live if last else 0,
            "rx_flows": last.rx_granted if last else 0, "restarts": watch.restarts,
            **clock_figures(last), **pump_figures(last), **judge}


def clock_figures(last: Sample | None) -> dict:
    """The box's clock for a line: the last read's fields, as `clock_<name>`;
    none the status lacks."""
    if last is None:
        return {}
    return {f"clock_{key}": last.clock[key] for key in (*CLOCK_COUNTS, *CLOCK_SIGNED) if key in last.clock}


def pump_figures(last: Sample | None) -> dict:
    """The engine pump's counts for a line, none without an engine."""
    if last is None or last.pump is None:
        return {}
    late, missed, rebootstraps = last.pump
    return {"pump_late": late, "pump_missed": missed, "pump_rebootstraps": rebootstraps}


def judge_figures(judge: dict[int, Least]) -> dict:
    """The packets' judge for a line: one entry per RX flow, by flow; none
    without one."""
    if not judge:
        return {}
    return {"judge": [{"flow": flow, "min_ns": j.min_ns, "max_ns": j.max_ns, "last_ns": j.last_ns,
                       "reads": j.reads} for flow, j in sorted(judge.items())]}


def judge_summary(series: dict[int, list[tuple[float, int]]]) -> dict:
    """The end's packets' judge (pure): per RX flow, by flow, its reads' count
    `n`, `min`, the PERCENTILES by nearest rank and `max` (ns), and
    `slope_ns_per_s`; none without a flow that had one."""
    if not series:
        return {}
    out = []
    for flow, points in sorted(series.items()):
        values = sorted(ns for _, ns in points)
        n = len(values)
        entry = {"flow": flow, "n": n, "min": values[0]}
        for p in PERCENTILES:
            entry[f"p{p}"] = values[max(1, -(-p * n // 100)) - 1]   # rank ceil(p n / 100), from 1
        entry.update(max=values[-1], slope_ns_per_s=slope(points))
        out.append(entry)
    return {"judge": out}


def slope(points: list[tuple[float, int]]) -> float | None:
    """The least-squares slope of the points (t in s, lateness in ns), ns a
    second to 0.001; None without two distinct times."""
    n = len(points)
    t_mean = math.fsum(t for t, _ in points) / n
    y_mean = math.fsum(y for _, y in points) / n
    sxx = math.fsum((t - t_mean) ** 2 for t, _ in points)
    if not sxx > 0:
        return None
    sxy = math.fsum((t - t_mean) * (y - y_mean) for t, y in points)
    return round(sxy / sxx, 3) + 0.0   # never -0.0


def run(box: Box, rate: int, seconds: float | None, clock, flag: Path, ending: Ending) -> int:
    watch = Watch(rate)
    tally = Tally()
    start = clock.now()
    next_line = start + LINE_EVERY_S

    def end(word: str, code: int, trip: Trip | None = None, stop: int | None = None) -> int:
        tally.add(watch.take_peaks())
        doc = figures(clock.now() - start, tally, Peaks(tally.request_rate, tally.flow_requests, tally.packet_ratio,
                                                        tally.arc_reads),
                      watch, judge_summary(tally.series))
        doc = {"end": word, **doc}
        if trip is not None:
            doc.update(trip=trip.numbers, stop_exit=stop)
        emit(doc)
        return code

    while True:
        tick = clock.now()
        if flag.exists():
            return end("flag", 0)
        if ending.asked:
            return end("signal", 0)
        if seconds is not None and tick - start >= seconds:
            return end("seconds", 0)
        if box.busy():
            tally.failed += 1
        else:
            t0 = clock.now()
            status = box.read()
            t1 = clock.now()
            try:
                s = sample(t0, t1, status) if status is not None else None
            except ValueError as e:
                s = box.failed(f"a status read lacks {e}")
            if s is None:
                tally.failed += 1
            else:
                tally.reads += 1
                trip = watch.feed(s)
                if trip is not None:
                    say(f"TRIPPED: {trip.kind} {json.dumps(trip.numbers)}: stopping the Gen 3 stack")
                    return end(trip.kind, EXIT_TRIPPED, trip, box.stop())
        if clock.now() >= next_line:
            peaks = watch.take_peaks()
            tally.add(peaks)
            emit(figures(clock.now() - start, tally, peaks, watch, judge_figures(peaks.judge)))
            next_line += LINE_EVERY_S
        wait = tick + READ_EVERY_S - clock.now()
        if wait > 0:
            clock.sleep(wait)


def parse(argv: list[str]) -> argparse.Namespace:
    ap = argparse.ArgumentParser(prog="live_guard.py", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--rate", type=int, required=True, help="the device config's sample rate (`rate`)")
    ap.add_argument("--seconds", type=float, help="end after this many seconds (default: until the flag or a signal)")
    args = ap.parse_args(argv)
    if args.rate not in RATES:
        raise Usage(f"--rate must be one of {', '.join(map(str, RATES))}")
    if args.seconds is not None and not args.seconds > 0:
        raise Usage("--seconds must be above 0")
    return args


def main(argv: list[str], clock=None) -> int:
    try:
        args = parse(argv)
        host = gen3_host(env_path())
        flag = flag_path()
        if flag.exists():
            raise Usage(f"{flag} exists: an event is on, and the event's own stop has taken the device off")
    except Usage as e:
        say(str(e))
        return EXIT_USAGE
    ending = Ending()

    def asked(_signum, _frame) -> None:
        ending.asked = True

    before = {sig: signal.signal(sig, asked) for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)}
    try:
        say(f"watching iem-dante every {READ_EVERY_S:g} s (a line every {LINE_EVERY_S:g} s)")
        return run(Box(host), args.rate, args.seconds, clock or Clock(), flag, ending)
    finally:
        for sig, handler in before.items():
            signal.signal(sig, handler)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
