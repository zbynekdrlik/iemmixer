"""The soak gate's pure judge (#130): the failure words and the functions turning its reads into the verdict."""
from __future__ import annotations

PUMP_COUNTS = ("late", "missed", "overruns", "rebootstraps")
WINDOW_KEYS = ("holdovers", "in_holdover", "re_anchors", "clock_not_ready", "pump_free", "pump_late", "pump_missed",
               "pump_rebootstraps")
# The failures a verdict names, in its order.
FAILURES = ("not_finished", "client", "harness", "polls", "health", "restarted", "pump_stalled", "pump_late",
            "pump_missed", "pump_overruns", "pump_rebootstraps", "clock_not_ready", "holdover", "re_anchor",
            "clock_restart_unsent", "clock_not_recovered", "rx_late", "tx_holes", "rx_holes", "rx_off",
            "receiver_late", "receiver_unread", "latency_over_budget")


def steps(values: list[dict]) -> list[tuple[int, int]]:
    """Each step's (rise of cumulative counters, summed over their keys; how
    many went back), one per read after the first. A key first read later
    counts from 0; one that went back counts its new value from 0."""
    out = []
    prev = values[0] if values else {}
    for cur in values[1:]:
        rise = back = 0
        for key, value in cur.items():
            before = prev.get(key, 0)
            if value >= before:
                rise += value - before
            else:
                rise += value
                back += 1
        out.append((rise, back))
        prev = {**prev, **cur}
    return out


def growth(values: list[dict]) -> tuple[int, int]:
    """(the rise read to read, summed; how many went back): `steps` summed."""
    s = steps(values)
    return sum(r for r, _ in s), sum(b for _, b in s)


def split(values: list[dict], inside: list[bool]) -> tuple[int, int, int]:
    """(the rise outside the window, the rise inside it, how many went back):
    a step belongs to the window when its later read lies in it."""
    out = into = back = 0
    for (rise, b), cur in zip(steps(values), inside[1:]):
        if cur:
            into += rise
        else:
            out += rise
        back += b
    return out, into, back


def hole(times: list[float], start: float, end: float) -> float:
    """The longest wait between the run's start, its good reads and its end."""
    edges = [start, *times, end]
    return max(b - a for a, b in zip(edges, edges[1:]))


def judged(samples: list[dict], inside: list[bool] | None = None) -> dict:
    """The pump's, the clock's and the flows' numbers over the reads (pure).
    `inside` marks the reads in a dantesync restart's window (#130): there
    the clock's words and the pump's re-bootstraps, missed and late blocks
    and free pacing go into `window`, information; blocks that do not rise,
    a read without the pump, overruns and every flow's count judge as
    anywhere."""
    inside = [*(inside or []), *[False] * (len(samples) - len(inside or []))]
    engines = [(s["engine"], i) for s, i in zip(samples, inside) if s["engine"] is not None]
    e_inside = [i for _, i in engines]
    out: dict = {}
    window = dict.fromkeys(WINDOW_KEYS, 0)
    for key in PUMP_COUNTS:
        rise, into, _ = split([{key: e[key]} for e, _ in engines], e_inside)
        if key == "overruns":   # a block's own time: no clock causes it
            out[f"pump_{key}"] = rise + into
        else:
            out[f"pump_{key}"], window[f"pump_{key}"] = rise, into
    blocks = [e["blocks"] for e, _ in engines]
    pairs = list(zip(blocks, blocks[1:]))
    free = [s["engine"] is not None and s["engine"]["media"] != 1 for s in samples]
    window["pump_free"] = sum(1 for f, i in zip(free, inside) if f and i)
    out["pump_stalled"] = (sum(1 for s in samples if s["engine"] is None)
                           + sum(1 for f, i in zip(free, inside) if f and not i)
                           + sum(1 for a, b in pairs if b == a))
    out["max_process_us"] = engines[-1][0]["max_process_us"] if engines else None
    not_ready = [s["ready"] != 1 for s in samples]
    out["clock_not_ready"] = sum(1 for n, i in zip(not_ready, inside) if n and not i)
    window["clock_not_ready"] = sum(1 for n, i in zip(not_ready, inside) if n and i)
    out["holdovers"], window["holdovers"], h_back = split([{"n": s["holdovers"]} for s in samples], inside)
    out["in_holdover"] = sum(1 for s, i in zip(samples, inside) if s["holdover"] and not i)
    window["in_holdover"] = sum(1 for s, i in zip(samples, inside) if s["holdover"] and i)
    anchors, window["re_anchors"], a_back = split([{"n": s["re_anchors"]} for s in samples], inside)
    epochs = [s["epoch"] for s in samples]
    moved = [a != b for a, b in zip(epochs, epochs[1:])]
    out["re_anchors"] = anchors + sum(1 for m, i in zip(moved, inside[1:]) if m and not i)
    window["re_anchors"] += sum(1 for m, i in zip(moved, inside[1:]) if m and i)
    back = sum(1 for a, b in pairs if b < a) + h_back + a_back
    for key in ("rx_late", "tx_holes", "rx_holes"):
        out[key], b = growth([s[key] for s in samples])
        back += b
    out["rx_off"] = sum(1 for s in samples if s["rx_off"])
    out["lateness_max"] = max((s["lateness"] for s in samples), default=None)
    out["counts_back"] = back
    out["window"] = window
    return out


def clean(s: dict) -> bool:
    """The clock ready, out of holdover, the pump on the media clock."""
    return s["ready"] == 1 and not s["holdover"] and s["engine"] is not None and s["engine"]["media"] == 1


def recovered(samples: list[dict], times: list[float], at: float, window: float) -> float | None:
    """Seconds from the restart to the first clean read after the window's
    last read that is not clean (after the restart when every read in it
    is); None without one (pure; `times` the reads' after the baseline,
    `samples` with it)."""
    reads = [(t - at, s) for t, s in zip(times, samples[1:]) if t >= at]
    last_bad = max((k for k, (t, s) in enumerate(reads) if t <= window and not clean(s)), default=-1)
    return next((round(t, 1) for t, s in reads[last_bad + 1:] if clean(s)), None)
