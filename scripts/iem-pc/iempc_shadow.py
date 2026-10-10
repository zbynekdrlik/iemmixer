"""`iempc shadow-report` (S8 lane 4, design note
docs/superpowers/specs/2026-10-10-s8-cutover-rollback-design.md section 3.5;
#11): the report-only shadow imports, summarised for the owner's sign-off.

At each entry from event into dev or live the guard runs pc.toml's `shadow`
command (`iem-migrate shadow`, which writes nothing) and appends one JSON
line to <root>\\shadow\\history.jsonl on the PC: `at` (Unix ms), `entry`
(`event→dev`, `event→live`), `bundle`, then the report (`import`, `counts`,
`site`, `fit`, `state_from`, `state`, `doubts`) or `error` and `why`.

This command only reads: one `Test-Path` of that file over ssh, then scp
into a temporary folder in this box's state folder (removed when done), and
prints one JSON object (`summarise`): how many entries and of which kind,
how many were clean (no error, the import would write, no difference, no
doubt about the saved state, a saved state compared), the import's verdicts, the errors by code, and the
differences by kind (`site.<kind>.<field>`, `state.<kind>.<field>`: in how
many entries, how many in all). Counts and kinds only: never an id, a value
or an error's words (P6 discipline; the file itself names ids and stays on
the PC).

Dev time (the flag refuses it), no lock (nothing on the PC changes); a new
flag abandons the read and the event path runs. No history yet: an empty
summary.

iempc.py passes itself in (`ip`), so this module never imports it (#36)."""
from __future__ import annotations

import json
import re
import tempfile
from collections import Counter
from pathlib import Path
from typing import Iterable

REL = "shadow/history.jsonl"
ENTRIES = ("event→dev", "event→live")
# A kind or field as the guard and iem-migrate name them; anything else
# reads as "?", so no other text reaches the output.
NAME = re.compile(r"[a-z_][a-z0-9_.]{0,63}")


def _name(value) -> str:
    return value if isinstance(value, str) and NAME.fullmatch(value) else "?"


def _diffs(doc: dict, against: str) -> list[str]:
    """The kinds of one line's `site` or `state` differences."""
    items = doc.get(against)
    if not isinstance(items, list):
        return []
    out = []
    for d in items:
        d = d if isinstance(d, dict) else {}
        out.append(f"{against}.{_name(d.get('kind'))}.{_name(d.get('field'))}")
    return out


def _clean(doc: dict, kinds: list[str]) -> bool:
    return (doc.get("import") == "writes" and not kinds and doc.get("fit", 0) == 0 and doc.get("doubts", 0) == 0
            and isinstance(doc.get("state_from"), str) and doc.get("state_from") != "none")


def summarise(lines: Iterable[str]) -> dict:
    """The history's lines (pure): counts and kinds only."""
    out = {"entries": 0, "unreadable": 0, "by_entry": Counter(), "clean": 0, "uncompared": 0,
           "imports": Counter(), "errors": Counter(), "with_differences": 0, "first": None, "last": None}
    entries_by_kind: Counter = Counter()
    total_by_kind: Counter = Counter()
    for raw in lines:
        if not raw.strip():
            continue
        try:
            doc = json.loads(raw)
        except ValueError:
            doc = None
        if not isinstance(doc, dict):
            out["unreadable"] += 1
            continue
        out["entries"] += 1
        entry = doc.get("entry")
        out["by_entry"][entry if entry in ENTRIES else "?"] += 1
        at = doc.get("at")
        if isinstance(at, int) and not isinstance(at, bool):
            out["first"] = at if out["first"] is None else min(out["first"], at)
            out["last"] = at if out["last"] is None else max(out["last"], at)
        if "error" in doc:
            out["errors"][_name(doc["error"])] += 1
            continue
        out["imports"][_name(doc.get("import"))] += 1
        kinds = _diffs(doc, "site") + _diffs(doc, "state")
        total_by_kind.update(kinds)
        entries_by_kind.update(set(kinds))
        if kinds:
            out["with_differences"] += 1
        if doc.get("state_from") in (None, "none"):
            out["uncompared"] += 1
        if _clean(doc, kinds):
            out["clean"] += 1
    for key in ("by_entry", "imports", "errors"):
        out[key] = dict(sorted(out[key].items()))
    out["differences"] = {k: {"entries": entries_by_kind[k], "total": total_by_kind[k]}
                          for k in sorted(total_by_kind)}
    return out


def run(ctx, ip) -> int:
    """`iempc shadow-report` (dev time, read-only)."""
    env = ctx.env
    watch = ctx.watch(abandon=True)
    path = ip.pc_join(env["PC_ROOT"], REL)
    there = ip.run_module(env, f"Test-Path -LiteralPath {ip.ps_quote(path)} -PathType Leaf", ip.STATUS_S, watch)
    if there is not True:
        ip.emit({"shadow_report": summarise([]), "history": "none"})
        return 0
    with tempfile.TemporaryDirectory(dir=ip.state_dir()) as tmp:
        local = Path(tmp) / "history.jsonl"
        ip.scp(ip.remote(env, REL), str(local), watch)
        if not local.is_file():
            raise ip.StepError(f"scp of {REL} left no file")
        text = local.read_text(encoding="utf-8", errors="replace")
    ip.emit({"shadow_report": summarise(text.split("\n"))})
    return 0
