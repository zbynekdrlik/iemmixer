#!/usr/bin/env python3
"""The live verdict (S7 design note §6; plan Task 22). Pure, stdlib only.

`report` is the ops live.yml report job's step: it reads the run's record
directory and prints one JSON object, `{"conclusion", "summary",
"first_failure", "numbers"}`, whose summary is posted as `live/iem-pc`. The
record directory (the jobs' artifacts; PowerShell's files may carry a BOM and
CRLF):

- begin.json (pc-begin): `{"reason": BEGIN_REASONS, "push_before": count}`;
- pc.json (pc): `{"reason": PC_REASONS}`;
- bursts.jsonl (pc): one `{"t", "exit"}` per `iemmode test-signal … --listen`,
  its exit code;
- evidence.json (pc-end): `{"job_end": JOB_END, "client_log": CLIENT_LOG,
  "push_after": count}`; job_end is `left-dev` when the PC was out of dev;
- results.json (browser): Playwright's JSON report of
  playwright.live.config.ts.

A cut PC side is `cancelled`, never red ("ide event"): a cancelled pc-begin,
pc or pc-end job, a begin record `left-dev` or `not-free`, a pc record
`left-dev`, a job end `left-dev`. A cancelled browser job alone is red. Else
the checks, in order; the first that fails leads a red summary (`red: <it>;
<numbers>`), each step inside a check in the order written:

 1. begin ready: the record's reason `ready`, then the pc-begin job success;
 2. the pc job: its reason `browser-done`, at least one burst, every burst
    exit 0, then the pc job success;
 3. every expected title passed: the titles read from the spec files (the
    parity checker's `test(` rule, check_parity_manifest.PW_TEST), each with
    as many tests in the report as it appears in the specs, every one passed
    (status `expected`, expected status `passed`); then no other test in the
    report that did not pass; then the browser job success;
 4. the client-log marker found;
 5. no push subscription left behind: push_after at most push_before;
 6. the job end ok, then the pc-end job success.

Numbers come only from `live_number` annotations, `<key>=<number>` with a key
of NUMBER_KEYS and a finite decimal number, the first of each key. The summary
holds fixed codes, numbers and the titles read from the spec files (public,
this repository): never a test's error text, a title only the report holds, a
code outside the known ones or a value the records hold besides counts (P6)."""
from __future__ import annotations

import argparse
import json
import math
import re
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
if str(SCRIPTS) not in sys.path:
    sys.path.append(str(SCRIPTS))
from check_parity_manifest import PW_TEST  # noqa: E402  (the parity checker's `test(` rule)

BEGIN_REASONS = frozenset({"ready", "not-free", "left-dev", "bundle-not-active", "engine-not-up", "no-client",
                           "token-failed", "push-count-failed"})
PC_REASONS = frozenset({"browser-done", "left-dev", "browser-never-started", "burst-refused", "jobs-unreadable",
                        "not-finished"})
CANCELLED = frozenset({"left-dev", "not-free"})
NUMBER_KEYS = ("listen_hz", "listen_dbfs", "first_audio_ms", "talkback_db", "limiter_active_s", "meter_fps",
               "burst_input_dbfs", "opus_frames")
LIVE_NUMBER = "live_number"
NUMBER = re.compile(r"-?[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?")  # ASCII digits only: no "nan", "١", "1_000"
CLIENT_LOG = frozenset({"found", "missing", "unreadable"})
JOB_END = frozenset({"ok", "failed", "left-dev"})
RESULTS = ("success", "failure", "cancelled", "skipped")  # a job's `needs.<job>.result`
RESULT_CODES = frozenset(RESULTS)
JOBS = {"begin": "pc-begin", "pc": "pc", "browser": "browser", "end": "pc-end"}
PC_SIDE = ("begin", "pc", "end")
# Where a cut shows in the records, and which codes there are a cut.
CUTS = (("begin", "reason", CANCELLED),
        ("pc", "reason", CANCELLED & PC_REASONS),
        ("evidence", "job_end", CANCELLED & JOB_END))
CUT_TEXT = {"left-dev": "the pc left dev", "not-free": "the pc was not free"}
OUTCOMES = {"unexpected": "failed", "skipped": "skipped", "flaky": "flaky"}  # Playwright's test outcome
SPEC_FILE = re.compile(r"\.(?:spec|test)\.[cm]?[jt]sx?$")  # Playwright's default testMatch
TEXT = (("first_audio_ms", "first audio", " ms"), ("talkback_db", "talkback", " dB"),
        ("limiter_active_s", "limiter", " s"), ("meter_fps", "meters", " fps"),
        ("burst_input_dbfs", "burst input", " dBFS"), ("opus_frames", "opus frames", ""))
UNREADABLE = object()  # a file that holds no JSON


class Bad(ValueError):
    """A record the verdict cannot read; its text (fixed words) is the reason."""


def _get(d: object, key: str) -> object:
    return d.get(key) if isinstance(d, dict) else None


def _count(v: object) -> bool:
    return type(v) is int and v >= 0


def _is(v: object, codes: frozenset) -> bool:
    """Whether a record's value is one of the codes (a list or a dict is none, never a TypeError)."""
    return isinstance(v, str) and v in codes


def _code(v: object, known: frozenset) -> str:
    """A record's code as printed: known codes only."""
    return v if _is(v, known) else "unknown"


def _record(d: object, name: str) -> dict:
    if d is None:
        raise Bad(f"no {name} record")
    if not isinstance(d, dict):
        raise Bad(f"the {name} record is unreadable")
    return d


def tests(results: object) -> list[tuple[str, str, list]]:
    """(title, outcome, annotations) of every test in Playwright's JSON report, depth first in its
    order (a file's specs, then its describe blocks); Bad when the report has another shape."""
    if results is None:
        raise Bad("no browser results")
    unreadable = Bad("the browser results are unreadable")
    if not isinstance(results, dict) or not isinstance(results.get("suites"), list):
        raise unreadable
    out: list[tuple[str, str, list]] = []
    stack = list(reversed(results["suites"]))
    while stack:
        suite = stack.pop()
        specs, inner = _get(suite, "specs"), _get(suite, "suites")
        if not isinstance(specs, list) or not isinstance(inner, (list, type(None))):
            raise unreadable
        for spec in specs:
            title, entries = _get(spec, "title"), _get(spec, "tests")
            if not isinstance(title, str) or not isinstance(entries, list):
                raise unreadable
            for t in entries:
                if not isinstance(t, dict):
                    raise unreadable
                status, notes = t.get("status"), t.get("annotations")
                if status == "expected" and t.get("expectedStatus") == "passed":
                    word = "passed"
                else:
                    word = OUTCOMES.get(status, "not passed") if isinstance(status, str) else "not passed"
                out.append((title, word, notes if isinstance(notes, list) else []))
        stack.extend(reversed(inner or []))
    return out


def live_numbers(results: object) -> dict:
    """The `live_number` annotations' known keys and finite values, the first of each key, in
    NUMBER_KEYS' order; whole numbers as integers. Bad when the report cannot be read."""
    found: dict = {}
    for _title, _word, notes in tests(results):
        for a in notes:
            text = _get(a, "description")
            if _get(a, "type") != LIVE_NUMBER or not isinstance(text, str):
                continue
            key, sep, value = text.partition("=")
            if not sep or key not in NUMBER_KEYS or key in found or not NUMBER.fullmatch(value):
                continue
            v = float(value)
            if math.isfinite(v):
                found[key] = int(v) if v.is_integer() and abs(v) < 2 ** 53 else v
    return {k: found[k] for k in NUMBER_KEYS if k in found}


@dataclass
class _Run:
    results: object
    begin: object
    pc: object
    evidence: object
    bursts: list | None
    titles: list[str] | None
    jobs: dict


def _job(r: _Run, key: str) -> str | None:
    result = r.jobs.get(key)
    return None if result == "success" else f"{JOBS[key]} job {_code(result, RESULT_CODES)}"


def _begin_ready(r: _Run) -> str | None:
    reason = _record(r.begin, "begin").get("reason")
    return f"begin {_code(reason, BEGIN_REASONS)}" if reason != "ready" else _job(r, "begin")


def _pc(r: _Run) -> str | None:
    reason = _record(r.pc, "pc").get("reason")
    if reason != "browser-done":
        return f"pc {_code(reason, PC_REASONS)}"
    if not r.bursts:
        return "no bursts"
    for n, b in enumerate(r.bursts, 1):
        if not isinstance(b, dict):
            return f"burst {n} is unreadable"
        code = b.get("exit")
        if type(code) is not int:
            return f"burst {n} has no exit code"
        if code != 0:
            return f"burst {n} exited {code}"
    return _job(r, "pc")


def _titles_passed(r: _Run) -> str | None:
    if r.titles is None:
        return "the live specs are unreadable"
    if not r.titles:
        return "no live spec titles"
    got = tests(r.results)
    want = Counter(r.titles)
    for title in want:  # in the specs' order
        words = [w for t, w, _ in got if t == title]
        if len(words) < want[title]:
            return f"live test missing: {title}"
        bad = next((w for w in words if w != "passed"), None)
        if bad:
            return f"live test {bad}: {title}"
    if any(w != "passed" for t, w, _ in got if t not in want):
        return "a live test outside the read titles did not pass"
    return _job(r, "browser")


def _client_log(r: _Run) -> str | None:
    log = _record(r.evidence, "end").get("client_log")
    return None if log == "found" else f"client log marker {_code(log, CLIENT_LOG)}"


def _push(r: _Run) -> str | None:
    before, after = _get(r.begin, "push_before"), _get(r.evidence, "push_after")
    if not (_count(before) and _count(after)):
        return "push counts unreadable"
    return f"push subscriptions left behind ({before} -> {after})" if after > before else None


def _job_end(r: _Run) -> str | None:
    end = _record(r.evidence, "end").get("job_end")
    return f"job end {_code(end, JOB_END)}" if end != "ok" else _job(r, "end")


CHECKS = (_begin_ready, _pc, _titles_passed, _client_log, _push, _job_end)


def _cancelled(r: _Run) -> str | None:
    """Why the PC side was cut ("ide event" never makes red), else None."""
    for key in PC_SIDE:
        if r.jobs.get(key) == "cancelled":
            return f"the {JOBS[key]} job was cancelled"
    for name, key, codes in CUTS:
        v = _get(getattr(r, name), key)
        if _is(v, codes):
            return CUT_TEXT[v]
    return None


def _numbers(r: _Run) -> dict:
    n: dict = {}
    if r.titles is not None:
        n["live_specs"] = len(r.titles)
    if r.bursts is not None:
        n["bursts"] = len(r.bursts)
    before, after = _get(r.begin, "push_before"), _get(r.evidence, "push_after")
    if _count(before):
        n["push_before"] = before
    if _count(after):
        n["push_after"] = after
    try:
        live = live_numbers(r.results)
    except Bad:  # no readable report has no live numbers; check 3 names it
        live = {}
    return {**n, **live}


def _num(v: int | float) -> str:
    """Whole numbers bare, others at two decimals without trailing zeros (information only:
    the specs judge these numbers, the verdict does not)."""
    if isinstance(v, int):
        return str(v)
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    return "0" if s == "-0" else s


def _text(n: dict, log: object) -> str:
    parts = [f"{n[k]} {label}" for k, label in (("live_specs", "live specs"), ("bursts", "bursts")) if k in n]
    listen = [f"{_num(n[k])} {unit}" for k, unit in (("listen_hz", "Hz"), ("listen_dbfs", "dBFS")) if k in n]
    if listen:
        parts.append("listen " + " ".join(listen))
    parts += [f"{label} {_num(n[k])}{unit}" for k, label, unit in TEXT if k in n]
    if _is(log, CLIENT_LOG):
        parts.append(f"client log {log}")
    if "push_before" in n and "push_after" in n:
        parts.append(f"push {n['push_before']} -> {n['push_after']}")
    return ", ".join(parts)


def _outcome(conclusion: str, summary: str) -> dict:
    return {"conclusion": conclusion, "summary": summary, "first_failure": None, "numbers": {}}


def report(results: object, begin: object, pc: object, evidence: object, bursts: list | None,
           titles: list[str] | None, pc_results: dict) -> dict:
    """{"conclusion": "success"|"failure"|"cancelled", "summary", "first_failure", "numbers"}.
    `results` is Playwright's report, `begin`/`pc`/`evidence` the records (None when absent,
    UNREADABLE when no JSON), `bursts` the burst lines (None when absent), `titles` the expected
    titles (None when the specs cannot be read) and `pc_results` the four jobs' results by
    `begin`, `pc`, `browser`, `end`."""
    r = _Run(results, begin, pc, evidence, bursts, titles, pc_results)
    cut = _cancelled(r)
    if cut:
        return _outcome("cancelled", f"cancelled: {cut}")
    first = None
    for check in CHECKS:
        try:
            first = check(r)
        except Bad as e:
            first = str(e)
        if first:
            break
    numbers = _numbers(r)
    text = _text(numbers, _get(evidence, "client_log"))
    if first:
        summary = f"red: {first}; {text}" if text else f"red: {first}"
    else:
        summary = f"green: {text}"
    return {"conclusion": "failure" if first else "success", "summary": summary, "first_failure": first,
            "numbers": numbers}


def titles(specs: Path) -> list[str]:
    """The expected titles: every spec file under the folder (Playwright's default testMatch), in
    path order, its `test(` titles in source order, unescaped as the parity checker does. Bad when
    the folder is no folder or a spec file cannot be read as UTF-8."""
    if not specs.is_dir():
        raise Bad("the live specs are unreadable")
    files = sorted((p for p in specs.rglob("*") if SPEC_FILE.search(p.name) and p.is_file()),
                   key=lambda p: p.relative_to(specs).as_posix())
    out = []
    for p in files:
        try:
            text = p.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as e:
            raise Bad("the live specs are unreadable") from e
        out += [re.sub(r"\\(.)", r"\1", m.group(2)) for m in PW_TEST.finditer(text)]
    return out


def read_json(path: Path) -> object:
    """The file's JSON (UTF-8, a BOM tolerated); None when it is absent, UNREADABLE when it holds no JSON."""
    try:
        return json.loads(path.read_text(encoding="utf-8-sig"))
    except FileNotFoundError:
        return None
    except ValueError:  # a UnicodeDecodeError too: the check that reads the file names it
        return UNREADABLE


def read_lines(path: Path) -> list | None:
    """bursts.jsonl, one burst per line (a BOM tolerated); a line that is no JSON (a line cut
    short) is None, which check 2 names. None when the file is absent."""
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        return None
    lines = []
    for line in data.removeprefix(b"\xef\xbb\xbf").split(b"\n"):
        if line.strip():
            try:
                lines.append(json.loads(line.decode("utf-8")))
            except ValueError:  # a UnicodeDecodeError too: check 2 names the burst unreadable
                lines.append(None)
    return lines


def cmd_report(a: argparse.Namespace) -> int:
    d = Path(a.dir)
    try:
        expected = titles(Path(a.specs))
    except Bad:  # check 3 names the specs unreadable
        expected = None
    out = report(read_json(d / "results.json"), read_json(d / "begin.json"), read_json(d / "pc.json"),
                 read_json(d / "evidence.json"), read_lines(d / "bursts.jsonl"), expected,
                 {"begin": a.begin, "pc": a.pc, "browser": a.browser, "end": a.end})
    print(json.dumps(out, allow_nan=False))  # strict JSON for jq: a NaN raises here, never reaches the job
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(prog="live_verdict.py", description=__doc__.split("\n", 1)[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("report", help="the ops report job: the record directory as one JSON object")
    r.add_argument("--dir", required=True, help="the record directory")
    r.add_argument("--specs", required=True, help="the live specs' folder (e2e/tests/live)")
    for flag, job in (("--begin", "pc-begin"), ("--pc", "pc"), ("--browser", "browser"), ("--end", "pc-end")):
        r.add_argument(flag, required=True, choices=RESULTS, help=f"the {job} job's result")
    return cmd_report(ap.parse_args(argv))


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
