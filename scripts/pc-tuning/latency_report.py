#!/usr/bin/env python3
"""S1c measurement summaries (design note §4): one JSON per step from the
spike's report, xperf's dpcisr text, the PC's poll samples (CPU counters,
active plan, governor, callback-thread priority) and the System log; plus the
hwlat and near-glitch views. Pure functions; tuning_window.py feeds them."""
from __future__ import annotations

import re

PERIOD_US = 333          # B = 32 at 96 kHz
WATCH_BUDGET_US = 128    # the xperf bucket edge above 100 us (design note §4.4)
LIMITS_US = (64, 128, 256, 512)

# A section header; a trailing colon is the same header (review M2). Any other
# form is not read, and a text without a DPC module fails closed.
_SECTION = re.compile(r"^\s*(DPC|Interrupt|ISR)\s+Info\s*:?\s*$", re.I)
_TOTAL = re.compile(r"^\s*Total\s*=\s*(\d+)\s+for module\s+(\S+)")
_BUCKET = re.compile(r"^\s*Elapsed Time,\s*>\s*(\d+)\s*usecs(?:\s+AND\s+<=\s*(\d+)\s*usecs)?,\s*(\d+)")
# A per-CPU table header cell: `CPU 3 Usage` (whole-trace and interval
# tables) or a bare `CPU 3` (the distribution table).
_CPU_COLUMN = re.compile(r"CPU\s+(\d+)(?:\s+Usage)?", re.I)
_MARKER = re.compile(r"iemmixer-glitch kind=(\S+) at_qpc=(-?\d+) emit_qpc=(-?\d+) freq=(\d+) value=(\d+)")
_MARKER_ID = "3b6c1e0a-5d2f-4c8e-9a71-0e4f2d9b8c11"
NEAR_EVENTS = ("DPC", "TimedDPC", "ThreadedDPC", "Interrupt", "CSwitch", "ReadyThread")


def _cpu_columns(line: str) -> list[str] | None:
    """The CPU numbers of a per-CPU table's header row, in column order, or
    None when `line` is no such header. Tolerates a leading label column,
    spaces before the commas and a trailing comma."""
    cells = [c.strip() for c in line.split(",")]
    while cells and not cells[-1]:
        cells.pop()
    if cells and not cells[0]:
        cells = cells[1:]
    cpus = [m.group(1) for c in cells if (m := _CPU_COLUMN.fullmatch(c))]
    return cpus if cpus and len(cpus) == len(cells) else None


def _read_usage_table(lines: list[str], i: int, cpus: list[str], usage: dict) -> int:
    """Reads the table whose header row was lines[i - 1]; returns the index of
    its closing blank line. Only the whole-trace table (its units row ends in
    `Module`) is per module: each row is one `usec %` cell per CPU column and
    the module name last, into usage[module][cpu] = usec. The 1-second
    interval and the distribution tables hold time or usage-% buckets, no
    module, and are passed over."""
    per_module = i < len(lines) and lines[i].rsplit(",", 1)[-1].strip().lower() == "module"
    i += 1
    while i < len(lines) and lines[i].strip():
        if per_module:
            *cells, module = (c.strip() for c in lines[i].split(","))
            if len(cells) != len(cpus):
                raise ValueError(f"dpcisr: a per-module usage row (line {i + 1}) has {len(cells)} CPU columns, the header {len(cpus)}")
            try:
                usage[module] = {cpu: int(cell.split()[0]) for cpu, cell in zip(cpus, cells)}
            except (IndexError, ValueError):
                raise ValueError(f"dpcisr: an unreadable per-module usage row (line {i + 1})") from None
        i += 1
    return i


def parse_dpcisr(text: str) -> dict:
    """xperf -a dpcisr: per kind (dpc/isr) and module the count, the upper edge
    of the highest non-empty bucket (an open last bucket reports its lower
    edge with open=True) and the counts in buckets starting at or above each
    limit; per-CPU usage from the whole-trace usage table (columns = CPU
    numbers, rows = modules) as {kind: {module: {cpu: usec}}}, every CPU
    column kept (0 = the module did not run there). Raises ValueError when a
    module was parsed but its per-CPU usage was not: the watched-CPU budget
    would otherwise pass unchecked (fail closed)."""
    out: dict = {"dpc": {}, "isr": {}, "usage": {"dpc": {}, "isr": {}}}
    kind = None
    module = None
    lines = text.splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        i += 1
        m = _SECTION.match(line)
        if m:
            kind = "dpc" if m.group(1).lower() == "dpc" else "isr"
            module = None
            continue
        if kind is None:
            continue
        if (cpus := _cpu_columns(line)) is not None:
            i = _read_usage_table(lines, i, cpus, out["usage"][kind])
            module = None
            continue
        if m := _TOTAL.match(line):
            module = m.group(2)
            out[kind][module] = {"count": int(m.group(1)), "max_us": 0, "open": False, "over": {str(x): 0 for x in LIMITS_US}}
        elif (m := _BUCKET.match(line)) and module is not None:
            lo, hi, n = int(m.group(1)), m.group(2), int(m.group(3))
            if n == 0:
                continue
            entry = out[kind][module]
            edge = int(hi) if hi is not None else lo
            if edge >= entry["max_us"]:
                entry["max_us"], entry["open"] = edge, hi is None
            for limit in LIMITS_US:
                if lo >= limit:
                    entry["over"][str(limit)] += n
    # A traced run on Windows always has DPCs: none read means the text was not
    # xperf's dpcisr (empty, foreign, an unknown header) — never "no findings".
    if not out["dpc"]:
        raise ValueError("dpcisr: no DPC module read (empty or not xperf -a dpcisr output): the budgets cannot be checked")
    # Counts only: a module name can be a private driver's (P6), and this text
    # is what an operator pastes into a ticket.
    for kind in ("dpc", "isr"):
        missing = set(out[kind]) - set(out["usage"][kind])
        if missing:
            raise ValueError(f"dpcisr: no per-CPU usage for {len(missing)} of {len(out[kind])} {kind} modules: "
                             "the watched-CPU budget cannot be checked")
    return out


def budget_findings(parsed: dict, watch_lps: list[int]) -> list[str]:
    """Modules above the budget on a watched CPU (the card's, the audio one:
    one with usage there) and modules reaching a full period anywhere."""
    watched = {str(x) for x in watch_lps}
    findings = []
    for kind in ("dpc", "isr"):
        for module, e in parsed[kind].items():
            shown = f"above {e['max_us']}" if e["open"] else f"up to {e['max_us']}"
            cpus = {cpu for cpu, us in parsed["usage"][kind].get(module, {}).items() if us > 0}
            if e["max_us"] > WATCH_BUDGET_US and cpus & watched:
                findings.append(f"{kind} {module}: {shown} us on a watched CPU (budget {WATCH_BUDGET_US})")
            if e["max_us"] >= PERIOD_US:
                findings.append(f"{kind} {module}: {shown} us (a full period is {PERIOD_US})")
    return findings


def cpu_rates(samples: list[dict]) -> dict[str, dict]:
    """Per logical processor from consecutive raw samples: interrupts/s and
    DPCs/s (mean, and the highest interval for interrupts), % DPC and %
    interrupt time (mean, max), % busy and % C1/C2/C3 (mean)."""
    series: dict[int, dict[str, list[float]]] = {}
    for a, b in zip(samples, samples[1:]):
        prev = {c["lp"]: c for c in a["cpus"]}
        for c in b["cpus"]:
            p = prev.get(c["lp"])
            dt = c["t100ns"] - p["t100ns"] if p else 0
            if dt <= 0:
                continue
            d = series.setdefault(c["lp"], {k: [] for k in ("int_s", "dpc_s", "dpc_pct", "int_pct", "busy_pct", "c1_pct", "c2_pct", "c3_pct")})
            d["int_s"].append((c["interrupts"] - p["interrupts"]) / (dt / 1e7))
            d["dpc_s"].append((c["dpcs"] - p["dpcs"]) / (dt / 1e7))
            d["dpc_pct"].append(100 * (c["dpc_time"] - p["dpc_time"]) / dt)
            d["int_pct"].append(100 * (c["int_time"] - p["int_time"]) / dt)
            d["busy_pct"].append(100 - 100 * (c["idle_time"] - p["idle_time"]) / dt)
            for k in ("c1", "c2", "c3"):
                d[f"{k}_pct"].append(100 * (c[f"{k}_time"] - p[f"{k}_time"]) / dt)
    out = {}
    for lp, d in sorted(series.items()):
        mean = {k: round(sum(v) / len(v), 1) for k, v in d.items()}
        out[str(lp)] = {"int_s_mean": mean["int_s"], "int_s_max": round(max(d["int_s"]), 1), "dpc_s_mean": mean["dpc_s"],
                        "dpc_pct_mean": mean["dpc_pct"], "dpc_pct_max": round(max(d["dpc_pct"]), 1),
                        "int_pct_mean": mean["int_pct"], "int_pct_max": round(max(d["int_pct"]), 1),
                        "busy_pct_mean": mean["busy_pct"], "c1_pct_mean": mean["c1_pct"], "c2_pct_mean": mean["c2_pct"], "c3_pct_mean": mean["c3_pct"]}
    return out


def glitch_counts(report: dict) -> dict:
    by_kind: dict[str, int] = {}
    first: list[dict] = []
    unreported = dropped = 0
    for seg in report.get("segments", []):
        for g in seg.get("glitches", []):
            by_kind[g["kind"]] = by_kind.get(g["kind"], 0) + 1
            if len(first) < 20:
                first.append(g)
        unreported += seg.get("glitches_unreported", 0)
        dropped += (seg.get("telemetry") or {}).get("glitches_dropped", 0)
    return {"by_kind": by_kind, "unreported": unreported, "dropped": dropped, "first": first}


def callback_view(report: dict, polls: list[dict]) -> dict:
    cpus: dict[str, int] = {}
    threads: list[int] = []
    switches = 0
    for seg in report.get("segments", []):
        t = seg.get("telemetry") or {}
        for lp, n in (t.get("callback_cpus") or {}).items():
            cpus[lp] = cpus.get(lp, 0) + n
        if t.get("callback_thread") and t["callback_thread"] not in threads:
            threads.append(t["callback_thread"])
        switches += t.get("thread_switches", 0)
    seen = [p["thread"] for p in polls if p.get("thread")]
    priority = None
    if seen:
        priority = {"base_min": min(s["base"] for s in seen), "base_max": max(s["base"] for s in seen),
                    "current_min": min(s["current"] for s in seen), "current_max": max(s["current"] for s in seen)}
    return {"cpus": cpus, "threads": threads, "thread_switches": switches, "priority": priority}


def sentinel_changes(polls: list[dict]) -> list[dict]:
    """Each new (plan, governor) pair with the time it was first seen."""
    out: list[dict] = []
    for p in polls:
        row = {"at": p.get("at"), "plan": p.get("plan"), "governor": p.get("governor")}
        if not out or (out[-1]["plan"], out[-1]["governor"]) != (row["plan"], row["governor"]):
            out.append(row)
    return out


def hwlat_summary(report: dict) -> dict:
    """One CPU's hwlat run. It measured that CPU only when it ended "done" with
    the scanner placed there (`placed`: the CPU Set IDs applied) and raised to
    TIME_CRITICAL; otherwise `failed` (the spike's outcome "error", or an
    older spike's placement/priority error text next to "done")."""
    h = report.get("hwlat") or {}
    gaps = h.get("gaps_us") or {}
    placed, priority = h.get("placed"), h.get("priority")
    failed = report.get("outcome") != "done" or not isinstance(placed, list) or priority != "time-critical"
    return {"cpu": h.get("cpu"), "outcome": report.get("outcome"), "placed": placed, "priority": priority,
            "error": report.get("error") or h.get("error"), "failed": failed, "reads": h.get("reads"), "over": h.get("over"),
            "max_us": gaps.get("max"), "p999_us": gaps.get("p999"), "largest_us": [x["gap_us"] for x in h.get("largest", [])[:5]]}


def parse_dumper(text: str) -> tuple[dict[str, list[str]], list[list[str]]]:
    """xperf -a dumper: the header's field names per event (between
    BeginHeader and EndHeader) and the event rows, comma-split and stripped."""
    fields: dict[str, list[str]] = {}
    rows: list[list[str]] = []
    in_header = False
    for line in text.splitlines():
        s = line.strip()
        if s in ("BeginHeader", "EndHeader"):
            in_header = s == "BeginHeader"
            continue
        if not s:
            continue
        cols = [c.strip() for c in s.split(",")]
        if in_header:
            fields[cols[0]] = cols
        else:
            rows.append(cols)
    return fields, rows


def _col(fields: dict[str, list[str]], row: list[str], *names: str) -> str | None:
    header = fields.get(row[0], [])
    for n in names:
        if n in header:
            i = header.index(n)
            return row[i] if i < len(row) else None
    return None


def near_glitch(text: str, period_us: float, window_periods: int = 2) -> list[dict]:
    """For each glitch marker: DPC/ISR/context-switch rows in the window
    before the glitch. The marker's text maps it to the glitch's time exactly
    (the marker is written up to 10 ms later); without the text the window is
    the 11 ms before the marker."""
    fields, rows = parse_dumper(text)
    out = []
    for row in rows:
        line = ", ".join(row)
        if "iemmixer-glitch" not in line and _MARKER_ID not in line:
            continue
        t_marker = float(row[1])
        m = _MARKER.search(line)
        if m:
            kind, at_qpc, emit_qpc, freq = m.group(1), int(m.group(2)), int(m.group(3)), int(m.group(4))
            at_us = t_marker - (emit_qpc - at_qpc) * 1e6 / freq
            start, end, exact = at_us - window_periods * period_us, at_us, True
        else:
            kind, at_us, start, end, exact = "unknown", t_marker, t_marker - 11_000, t_marker, False
        events = []
        for r in rows:
            if r[0] not in NEAR_EVENTS or not start <= float(r[1]) <= end:
                continue
            us = _col(fields, r, "ElapsedTime", "Elapsed Time", "Duration")
            events.append({"event": r[0], "t_us": float(r[1]), "cpu": _col(fields, r, "CPU"),
                           "us": float(us) if us not in (None, "") else None,
                           "what": _col(fields, r, "Routine", "Image!Function", "New Process Name ( PID)")})
        out.append({"kind": kind, "at_us": round(at_us, 1), "exact": exact, "events": events})
    return out


def top_modules(parsed: dict, n: int = 12) -> dict:
    return {kind: sorted(({"module": m, **e} for m, e in parsed[kind].items()), key=lambda x: (-x["max_us"], -x["count"]))[:n]
            for kind in ("dpc", "isr")}


def summarize(label: str, verdict: dict | None, report: dict, dpcisr_text: str | None, polls: list[dict],
              events: list[dict], watch_lps: list[int]) -> dict:
    parsed = parse_dpcisr(dpcisr_text) if dpcisr_text is not None else None
    return {
        "label": label,
        "verdict": verdict,
        "glitches": glitch_counts(report),
        "callback": callback_view(report, polls),
        "process": report.get("process"),
        "dpcisr": top_modules(parsed) if parsed else None,
        "findings": budget_findings(parsed, watch_lps) if parsed else ["no trace"],
        "cpu": cpu_rates([p["cpu"] for p in polls if p.get("cpu")]),
        "sentinels": sentinel_changes(polls),
        "system_events": events,
    }
