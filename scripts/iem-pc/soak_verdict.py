#!/usr/bin/env python3
"""The soak verdict (S7 design note §4; plan Task 7). Pure, stdlib only.

`report` is the ops soak.yml report job's step: it reads the PC job's record
directory (result.json, polls.jsonl: one `{"t", "exit", "status"}` per
`iemmode status` poll, soakclient.json: the iem-soakclient summary) and
prints one JSON object, `{"conclusion", "summary", "first_failure",
"numbers"}`, whose summary is posted as `soak/iem-pc`. A cancelled PC job, a
missing record or the reason `left-dev` is `cancelled`: "ide event" never
makes red. Any other PC failure is `failure` naming its reason code; else the
soak is judged. The checks, in order; the first that fails leads a red
summary (`red: <it>; <numbers>`):

 1. polls exist; each has exit 0, mode dev, no switch and an engine;
 2. every engine runs the SHA, all under one engine pid;
 3. the polls span the hours, with no hole over POLL_HOLE_S;
 4. missed +0;  5. resets +0 (last poll minus the first);
 6. both histograms in the first and the last poll, their top above
    LATE_US, no bucket gone back;
 7. late: intervals of LATE_US or more at most LATE_PER_MILLE of the soak's;
 8. the callback's own time at p99.9 (iem_audio_io::hist::quantile_us's
    rank) at most PROCESS_P999_US;
 9. the harness ran the hours to its end;  10. no gap;  11. no reconnect;
12. at least FRAMES_PERCENT of the expected listen frames (with gaps measured
    between frames, gaps 0 alone cannot tell a thinned stream from a full one).

Counts before the first poll never count: every gate reads last minus first.
`harness` is CI's e2e step: the checks 9 to 12 with CI's bounds; it prints
the problems and exits 1 when there are any.

The summary holds numbers and fixed words only (P6): never a member id, a
host or a SHA. The harness's and the PC job's reason codes are printed only
when they are known codes."""
from __future__ import annotations

import argparse
import json
import math
import re
import sys
from pathlib import Path

LATE_US = 347             # S1a p99.9 interval at B = 32 (design §3): intervals of 347 µs or more are late
LATE_PER_MILLE = 2        # ≤ 0.2 % of the soak's intervals
PROCESS_P999_US = 83      # 25 % of the 333 µs period
P999 = 999                # per mille
HOURS = 8.0
POLL_HOLE_S = 300         # a longer time without a poll is a hole in the record
FRAMES_PERCENT = 99       # the harness got at least this share of its expected frames
DRIFT = "tuning drift:"   # the guard's hourly drift alarm (information only)
SHA = re.compile(r"[0-9a-f]{40}")
# The ops soak job's reason codes (result.json) and iem-soakclient's (`Reason::code`).
PC_REASONS = frozenset({"finished", "left-dev", "not-finished", "bundle-not-active", "no-client",
                        "harness-did-not-end"})
HARNESS_REASONS = frozenset({"site-unreadable", "not-http", "login-refused", "not-engineer", "server-gone",
                             "cpu-sets"})
# The summary's whole-number fields the verdict reads (besides `complete` and `seconds`).
HARNESS_COUNTS = ("frames", "expected_frames", "gaps", "reconnects", "decode_errors", "meter_frames")


class Bad(ValueError):
    """A record the verdict cannot read, or a count gone back; its text (numbers and fixed words) is the reason."""


def _count(v: object, where: str) -> int:
    if type(v) is not int or v < 0:
        raise Bad(f"{where} is unreadable")
    return v


def _get(d: object, key: str) -> object:
    return d.get(key) if isinstance(d, dict) else None


def _engine(poll: object) -> object:
    return _get(_get(poll, "status"), "engine")


def hist(pairs: object, where: str) -> dict[int, int]:
    """A sparse histogram `[[bucket, count], …]` as {bucket: count}."""
    if pairs is None:
        raise Bad(f"{where} is missing")
    if not isinstance(pairs, list):
        raise Bad(f"{where} is unreadable")
    h: dict[int, int] = {}
    for e in pairs:
        if not isinstance(e, list) or len(e) != 2:
            raise Bad(f"{where} is unreadable")
        b, c = _count(e[0], where), _count(e[1], where)
        if b in h:
            raise Bad(f"{where} is unreadable")
        h[b] = c
    return h


def delta(first: dict[int, int], last: dict[int, int], where: str) -> dict[int, int]:
    """Bucketwise last minus first, empty buckets left out."""
    d = {}
    for b in sorted(first.keys() | last.keys()):
        n = last.get(b, 0) - first.get(b, 0)
        if n < 0:
            raise Bad(f"{where} bucket {b} went back")
        if n:
            d[b] = n
    return d


def at_or_above(h: dict[int, int], us: int) -> int:
    return sum(c for b, c in h.items() if b >= us)


def quantile_us(h: dict[int, int], per_mille: int) -> int | None:
    """The upper edge (µs) of the bucket at rank ⌈per_mille·n/1000⌉, at least
    1: the rule of Rust's iem_audio_io::hist::quantile_us. None when empty."""
    rank = max(1, (per_mille * sum(h.values()) + 999) // 1000)
    seen = 0
    for b in sorted(h):
        seen += h[b]
        if seen >= rank:
            return b + 1
    return None


class _Soak:
    """The polls under judgement and the numbers found so far."""

    def __init__(self, polls: list, sha: str, hours: float) -> None:
        self.polls, self.sha, self.hours = polls, sha, hours
        self.numbers: dict = {"polls": len(polls)}
        self.interval: dict[int, int] | None = None
        self.process: dict[int, int] | None = None

    def end(self, i: int) -> tuple[str, dict]:
        """The first (0) or the last (-1) poll's name and engine."""
        if not self.polls:
            raise Bad("no polls")
        name = f"poll {1 if i == 0 else len(self.polls)}"
        e = _engine(self.polls[i])
        if not isinstance(e, dict):
            raise Bad(f"{name} has no engine")
        return name, e


def _polls_ok(s: _Soak) -> str | None:
    if not s.polls:
        return "no polls"
    for n, p in enumerate(s.polls, 1):
        st, code = _get(p, "status"), _get(p, "exit")
        if not isinstance(st, dict):
            return f"poll {n} is unreadable"
        if type(code) is not int:
            return f"poll {n} has no exit code"
        if code != 0:
            return f"poll {n} exited {code}"
        if st.get("mode") != "dev":
            return f"poll {n} is out of dev"
        if st.get("switching") is not None:
            return f"poll {n} is switching"
        if not isinstance(st.get("engine"), dict):
            return f"poll {n} has no engine"
    return None


def _one_engine(s: _Soak) -> str | None:
    pids = set()
    for n, p in enumerate(s.polls, 1):
        e = _engine(p)
        if not isinstance(e, dict):
            continue  # check 1 names it
        if e.get("build") != s.sha:
            return f"poll {n} runs another build"
        if type(e.get("pid")) is not int:
            return f"poll {n} has no engine pid"
        pids.add(e["pid"])
        if len(pids) > 1:
            return f"poll {n} runs another engine pid"
    return None


def _span(s: _Soak) -> str | None:
    ts = []
    for n, p in enumerate(s.polls, 1):
        t = _get(p, "t")
        if type(t) not in (int, float) or not math.isfinite(t):
            raise Bad(f"poll {n} has no time")
        ts.append(t)
    if not ts:
        raise Bad("no polls")
    steps = [b - a for a, b in zip(ts, ts[1:])]
    for n, step in enumerate(steps, 1):
        if step < 0:
            return f"poll {n + 1} is older than poll {n}"
    span = ts[-1] - ts[0]
    s.numbers["polled_hours"] = math.floor(span / 36) / 100  # rounded down: a short soak never reads as long enough
    if span < s.hours * 3600:
        return f"polled {s.numbers['polled_hours']:.2f} h of {s.hours:g} h"
    for n, step in enumerate(steps, 1):
        if step > POLL_HOLE_S:
            return f"no poll for {step:g} s after poll {n}"
    return None


def _counter(key: str):
    def check(s: _Soak) -> str | None:
        (_, first), (where, last) = s.end(0), s.end(-1)
        d = _count(last.get(key), f"{key} of {where}") - _count(first.get(key), f"{key} of poll 1")
        s.numbers[key] = d
        return f"{key} {d:+d}" if d else None
    return check


def _hists(s: _Soak) -> str | None:
    ends = (s.end(0), s.end(-1))
    read = {k: [hist(e.get(f"{k}_hist"), f"the {k} histogram of {w}") for w, e in ends]
            for k in ("interval", "process")}
    for where, e in ends:
        top = e.get("hist_top_us")
        if type(top) is not int or top <= LATE_US:
            return f"the histogram top in {where} is not above {LATE_US} µs"
    s.interval = delta(*read["interval"], "the interval histogram")
    s.process = delta(*read["process"], "the process histogram")
    return None


def _late_percent(late: int, total: int) -> str:
    """Rounded up at 0.001 %: a red share never reads as the bound."""
    return f"{-(-late * 100_000 // total) / 1000:.3f}"


def _late(s: _Soak) -> str | None:
    if s.interval is None:
        raise Bad("no interval histogram")
    total, late = sum(s.interval.values()), at_or_above(s.interval, LATE_US)
    s.numbers["intervals"], s.numbers["late_intervals"] = total, late
    if total == 0:
        return "no intervals in the soak"
    if late * 1000 > total * LATE_PER_MILLE:
        return f"late {_late_percent(late, total)} % (≥ {LATE_US} µs) above {LATE_PER_MILLE / 10:g} %"
    return None


def _process(s: _Soak) -> str | None:
    if s.process is None:
        raise Bad("no process histogram")
    q = quantile_us(s.process, P999)
    s.numbers["process_p999_us"] = q
    if q is None:
        return "no process times in the soak"
    if q > PROCESS_P999_US:
        return f"process p99.9 {q} µs above {PROCESS_P999_US} µs"
    return None


# Checks 1 to 8 in the verdict's order (9 to 12 are the harness's).
CHECKS = (_polls_ok, _one_engine, _span, _counter("missed"), _counter("resets"), _hists, _late, _process)


def _information(s: _Soak) -> dict:
    """Numbers that never decide: the 1.5-period late counter and overruns
    (last minus first), the last poll's longest callback, the drift alarms
    raised during the soak (each once)."""
    info: dict = {}
    if not s.polls:
        return info
    first, last = _engine(s.polls[0]), _engine(s.polls[-1])
    for key, name in (("late", "late_counter"), ("overruns", "overruns")):
        if type(_get(first, key)) is int and type(_get(last, key)) is int:
            info[name] = last[key] - first[key]
    if type(_get(last, "process_max_us")) in (int, float):
        info["process_max_us"] = last["process_max_us"]
    t0 = _get(s.polls[0], "t")
    if type(t0) in (int, float):
        alarms = [a for p in s.polls for a in _get(_get(p, "status"), "alarms") or [] if isinstance(a, dict)]
        info["drift_alarms"] = len({a.get("id") for a in alarms if str(a.get("text")).startswith(DRIFT)
                                    and type(a.get("at")) in (int, float) and a["at"] >= t0})
    return info


def _harness(summary: object) -> dict:
    """The iem-soakclient summary's numbers; Bad when it holds another shape."""
    if not isinstance(summary, dict):
        raise Bad("the harness summary is unreadable")
    h = {k: summary.get(k) for k in HARNESS_COUNTS}
    seconds, complete = summary.get("seconds"), summary.get("complete")
    if (any(type(v) is not int or v < 0 for v in h.values()) or type(complete) is not bool
            or type(seconds) not in (int, float) or not 0 <= seconds < math.inf):
        raise Bad("the harness summary is unreadable")
    return {**h, "seconds": seconds, "complete": complete}


def _harness_check(summary: object, min_seconds: float, max_gaps: int) -> tuple[list[str], dict]:
    """The checks 9 to 12 in order, and the harness's numbers (none when unreadable)."""
    if summary is None:
        return ["no harness summary"], {}
    try:
        h = _harness(summary)
    except Bad as e:
        return [str(e)], {}
    problems = []
    if not h["complete"]:
        code = summary.get("error")
        problems.append(f"harness incomplete ({code})" if code in HARNESS_REASONS else "harness incomplete")
    if h["seconds"] < min_seconds:
        problems.append(f"harness ran {h['seconds']:g} s of {min_seconds:g} s")
    if h["gaps"] > max_gaps:
        problems.append(f"gaps {h['gaps']}")
    if h["reconnects"]:
        problems.append(f"reconnects {h['reconnects']}")
    if h["frames"] == 0:
        problems.append("no listen frames")
    elif h["frames"] * 100 < h["expected_frames"] * FRAMES_PERCENT:
        problems.append(f"frames {h['frames'] * 10_000 // h['expected_frames'] / 100:.2f} % of expected")
    return problems, {k: v for k, v in h.items() if k != "complete"}


def harness_problems(summary: object, min_seconds: float, max_gaps: int) -> list[str]:
    """The harness's checks 9 to 12, in order; [] when it passed."""
    return _harness_check(summary, min_seconds, max_gaps)[0]


def _text(n: dict) -> str:
    parts = [f"{n['polled_hours']:.2f} h"] if "polled_hours" in n else []
    parts.append(f"{n['polls']} polls")
    parts += [f"{k} {n[k]:+d}" for k in ("missed", "resets") if k in n]
    if n.get("intervals"):
        parts.append(f"late {_late_percent(n['late_intervals'], n['intervals'])} % (≥ {LATE_US} µs)")
    if n.get("process_p999_us") is not None:
        parts.append(f"process p99.9 {n['process_p999_us']} µs")
    parts += [f"{k} {n[k]}" for k in ("gaps", "reconnects", "frames") if k in n]
    return ", ".join(parts)


def verdict(polls: list | None, harness: object, sha: str, hours: float = HOURS) -> dict:
    """{"conclusion": "success"|"failure", "summary": str, "first_failure": str|None, "numbers": {…}}"""
    s = _Soak(list(polls or []), sha, hours)
    failures = []
    for check in CHECKS:  # every check runs, so the numbers are complete; the first failure leads
        try:
            why = check(s)
        except Bad as e:
            why = str(e)
        if why:
            failures.append(why)
    problems, harness_numbers = _harness_check(harness, hours * 3600, 0)
    failures += problems
    numbers = {**s.numbers, **_information(s), **harness_numbers}
    first = failures[0] if failures else None
    summary = f"red: {first}; {_text(numbers)}" if first else f"green: {_text(numbers)}"
    return {"conclusion": "failure" if first else "success", "summary": summary, "first_failure": first,
            "numbers": numbers}


def _outcome(conclusion: str, summary: str, first: str | None = None) -> dict:
    return {"conclusion": conclusion, "summary": summary, "first_failure": first, "numbers": {}}


def report(pc_result: str, record: object, polls: list | None, harness: object, sha: str, hours: float) -> dict:
    """The ops report job's conclusion: cancelled for a cancelled PC job, a missing record or reason
    left-dev ("ide event" never makes red); failure for any other PC failure, naming its reason code;
    else verdict()."""
    reason = _get(record, "reason")
    if pc_result == "cancelled":
        return _outcome("cancelled", "cancelled: the pc job was cancelled")
    if record is None:
        return _outcome("cancelled", "cancelled: no record of the pc job")
    if reason == "left-dev":
        return _outcome("cancelled", "cancelled: the pc left dev")
    if pc_result != "success" or _get(record, "conclusion") != "success" or reason != "finished":
        job = pc_result if pc_result in ("success", "failure", "skipped") else "unknown"
        first = f"pc job {job} ({reason if reason in PC_REASONS else 'unknown'})"
        return _outcome("failure", f"red: {first}", first)
    return verdict(polls, harness, sha, hours)


def read_json(path: Path) -> object:
    """The file's JSON (UTF-8, a BOM tolerated); None when it is absent."""
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except FileNotFoundError:
        return None
    except ValueError as e:  # a UnicodeDecodeError too
        raise Bad(f"{path.name} is unreadable") from e


def read_polls(path: Path) -> list | None:
    """polls.jsonl, one poll per line (a BOM tolerated); a line that is no
    JSON (a status iemmode could not answer, a line cut short) is None, which
    check 1 names. None when the file is absent."""
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        return None
    polls = []
    for line in data.removeprefix(b"\xef\xbb\xbf").split(b"\n"):
        if line.strip():
            try:
                polls.append(json.loads(line.decode("utf-8")))
            except ValueError:  # a UnicodeDecodeError too: the poll is named unreadable
                polls.append(None)
    return polls


def _sha(v: str) -> str:
    if not SHA.fullmatch(v):
        raise argparse.ArgumentTypeError("a 40-hex commit SHA")
    return v


def _hours(v: str) -> float:
    h = float(v)
    if not 0 < h <= 10:
        raise argparse.ArgumentTypeError("hours above 0, at most 10")
    return h


def cmd_report(a: argparse.Namespace) -> int:
    d = Path(a.dir)
    try:
        out = report(a.pc, read_json(d / "result.json"), read_polls(d / "polls.jsonl"),
                     read_json(d / "soakclient.json"), a.sha, a.hours)
    except Bad as e:  # an unreadable record file: a cancelled PC job stays cancelled
        out = (_outcome("cancelled", "cancelled: the pc job was cancelled") if a.pc == "cancelled"
               else _outcome("failure", f"red: {e}", str(e)))
    print(json.dumps(out))
    return 0


def cmd_harness(a: argparse.Namespace) -> int:
    try:
        problems, h = _harness_check(read_json(Path(a.summary)), a.min_seconds, a.max_gaps)
    except Bad as e:
        problems, h = [str(e)], {}
    for p in problems:
        print(p)
    if problems:
        return 1
    print(f"harness ok: {h['seconds']:g} s, frames {h['frames']} of {h['expected_frames']}, gaps {h['gaps']}, "
          f"reconnects {h['reconnects']}")
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(prog="soak_verdict.py", description=__doc__.split("\n", 1)[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("report", help="the ops report job: the record directory as one JSON object")
    r.add_argument("--pc", required=True, choices=("success", "failure", "cancelled", "skipped"))
    r.add_argument("--dir", required=True)
    r.add_argument("--sha", required=True, type=_sha)
    r.add_argument("--hours", type=_hours, default=HOURS)
    h = sub.add_parser("harness", help="CI's e2e step: the harness summary's problems, exit 1 when any")
    h.add_argument("summary")
    h.add_argument("--min-seconds", required=True, type=float)
    h.add_argument("--max-gaps", required=True, type=int)
    a = ap.parse_args(argv)
    return cmd_report(a) if a.cmd == "report" else cmd_harness(a)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
