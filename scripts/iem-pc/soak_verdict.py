#!/usr/bin/env python3
"""The soak client's verdict. Pure, stdlib only.

It judges an iem-soakclient summary, in order:

- the client ran to its end, and for the seconds asked;
- no more gaps than allowed (`None`: gaps are counted, not judged);
- no reconnect;
- at least FRAMES_PERCENT of the expected listen frames (with gaps measured
  between frames, gaps alone cannot tell a thinned stream from a full one);
- at least one meter frame (a real server sends them; a run against it is the
  proof that the client reads its Meters).

`harness` is CI's e2e step: it prints the problems and exits 1 when there are
any. `box_check` is the Gen 3 box's soak gate (`iempc gen3-gate soak`); its gaps
are counted, not judged (owner on #72, 2026-10-29: the browser listen is low
priority, #88).

What it prints holds numbers and fixed words only (P6): never a member id, a
host or a SHA. The client's reason codes are printed only when they are known
codes."""
from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

FRAMES_PERCENT = 99       # the harness got at least this share of its expected frames
# iem-soakclient's reason codes (`Reason::code`).
HARNESS_REASONS = frozenset({"site-unreadable", "not-http", "wrong-server", "login-refused", "not-engineer",
                             "server-gone", "connection-lost", "secret-unreadable"})
# The summary's whole-number fields the verdict reads (besides `complete` and `seconds`).
HARNESS_COUNTS = ("frames", "expected_frames", "gaps", "reconnects", "decode_errors", "meter_frames")
UNREADABLE = object()  # a harness file that holds no JSON (no summary shape: the check names it)


class Bad(ValueError):
    """A summary the verdict cannot read; its text (numbers and fixed words) is the reason."""


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


def _harness_check(summary: object, min_seconds: float, max_gaps: int | None) -> tuple[list[str], dict]:
    """The summary's checks in order (gaps skipped when `max_gaps` is None), and its numbers (none when
    unreadable)."""
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
    if h["seconds"] < min_seconds:  # rounded down at 0.1 s: a short run never reads as the bound
        problems.append(f"harness ran {math.floor(h['seconds'] * 10) / 10:g} s of {min_seconds:g} s")
    if max_gaps is not None and h["gaps"] > max_gaps:
        problems.append(f"gaps {h['gaps']}")
    if h["reconnects"]:
        problems.append(f"reconnects {h['reconnects']}")
    if h["frames"] == 0:
        problems.append("no listen frames")
    elif h["frames"] * 100 < h["expected_frames"] * FRAMES_PERCENT:
        problems.append(f"frames {h['frames'] * 10_000 // h['expected_frames'] / 100:.2f} % of expected")
    return problems, {k: v for k, v in h.items() if k != "complete"}


def _ci_check(summary: object, min_seconds: float, max_gaps: int | None) -> tuple[list[str], dict]:
    """The summary's checks, then at least one meter frame (a readable summary only)."""
    problems, h = _harness_check(summary, min_seconds, max_gaps)
    if h and h["meter_frames"] == 0:
        problems.append("no meter frames")
    return problems, h


def box_check(summary: object, min_seconds: float) -> tuple[list[str], dict]:
    """The Gen 3 box's soak gate (`iempc gen3-gate soak`, iempc_gen3_soak.py): the summary's checks and its numbers.
    Gaps are counted, not judged (owner on #72, 2026-10-29: the browser listen is low priority, #88)."""
    return _ci_check(summary, min_seconds, None)


def read_json(path: Path) -> object:
    """The file's JSON (UTF-8, a BOM tolerated); None when it is absent."""
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except FileNotFoundError:
        return None
    except ValueError as e:  # a UnicodeDecodeError too
        raise Bad(f"{path.name} is unreadable") from e


def cmd_harness(a: argparse.Namespace) -> int:
    try:
        problems, h = _ci_check(read_json(Path(a.summary)), a.min_seconds, a.max_gaps)
    except Bad as e:
        problems, h = [str(e)], {}
    for p in problems:
        print(p)
    if problems:
        return 1
    print(f"harness ok: {h['seconds']:g} s, frames {h['frames']} of {h['expected_frames']}, gaps {h['gaps']}, "
          f"reconnects {h['reconnects']}, meter frames {h['meter_frames']}")
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(prog="soak_verdict.py", description=__doc__.split("\n", 1)[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    h = sub.add_parser("harness", help="CI's e2e step: the harness summary's problems, exit 1 when any")
    h.add_argument("summary")
    h.add_argument("--min-seconds", required=True, type=float)
    h.add_argument("--max-gaps", required=True, type=int)
    return cmd_harness(ap.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
