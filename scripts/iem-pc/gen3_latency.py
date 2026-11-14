"""The mic-to-ear latency guard (#122): every mic input's path, from a
source's Dante sample stamp to the in-ear output's playout on the receiving
device, against the budget of 1 ms.

path = D + the input's RX flow delay + the in-ear receiver's latency, in
samples at the device's rate (96 kHz; the engine refuses any other):

- D is the engine's offset (`ctl status` `engine.offset`: B + P + W);
- the RX flow delay is the worst `delay` of the status `rx[]` flows whose
  `device` is the transmitter the input's RX channel is subscribed to (our
  subscriptions from ARC, `gen3_arc.read_rx`): several flows from one device
  take the worst;
- the in-ear receiver's latency is the worst `receiver_latency_us` of the
  status `tx[]` flows that carry channels and whose receiver plays them at
  `IN_EAR_MAX_US` (500 us) or less. That cut is the in-ear receivers' class
  (the stage rack plays our flows at 250 us; a receiver at 1 ms is not an
  in-ear one): `ctl status` does not yet name the receiver of a TX flow, so
  the class stands in for it until dstack puts the receiver per TX flow into
  the status.

Not in the number: the converters, the wireless mic link and the in-ear
transmitters (hardware, outside Dante).

A mic input is an `[[engine.inputs]]` id starting with `mic`. An input whose
subscription is not receiving, whose transmitter has no flow in the status,
or whose path cannot be read (no engine object, no in-ear flow) is `unknown`,
never a pass; an input with no subscription at all has no path and is
`unsubscribed` (nothing to guard).

Pure functions; `read_inputs` reads a site's text. Rows carry the device's
name and the RX channels (site data): `public` drops both before anything is
printed."""
from __future__ import annotations

import math
import tomllib

RATE = 96_000
BUDGET_SAMPLES = 96   # 1 ms at 96 kHz
IN_EAR_MAX_US = 500   # the in-ear receivers' class (above)
RECEIVING = (9, 10)   # ARC 0x3000's Receiving, unicast or multicast (the status word's low 16 bits)


def mic_inputs(site_text: str) -> list[tuple[str, list[int]]]:
    """The site's mic inputs: (id, RX channels) of each `[[engine.inputs]]`
    whose id starts with `mic`; ValueError when the site has no such list."""
    engine = tomllib.loads(site_text).get("engine")
    inputs = engine.get("inputs") if isinstance(engine, dict) else None
    if not isinstance(inputs, list):
        raise ValueError("the site has no [[engine.inputs]]")
    out = []
    for item in inputs:
        if not isinstance(item, dict):
            raise ValueError("an [[engine.inputs]] entry is not a table")
        ident, rx = item.get("id"), item.get("rx")
        if not isinstance(ident, str) or not ident.startswith("mic"):
            continue
        if not isinstance(rx, list) or not rx or not all(type(n) is int and n >= 1 for n in rx):
            raise ValueError("a mic input's rx is not a list of channel numbers")
        out.append((ident, list(rx)))
    return out


def samples_of_us(us: float) -> int:
    """`us` microseconds in whole samples at RATE, rounded up."""
    return math.ceil(us * RATE / 1_000_000)


def in_ear_samples(status: dict) -> int | None:
    """The in-ear receivers' latency in samples: the worst
    `receiver_latency_us` of the TX flows with channels whose receiver plays
    them at IN_EAR_MAX_US or less; None when no such flow."""
    worst = None
    for flow in status.get("tx") or []:
        if not isinstance(flow, dict):
            continue
        channels, us = flow.get("channels"), flow.get("receiver_latency_us")
        if type(channels) is not int or channels <= 0 or type(us) not in (int, float):
            continue
        if 0 < us <= IN_EAR_MAX_US:
            worst = us if worst is None else max(worst, us)
    return None if worst is None else samples_of_us(worst)


def flow_delay(status: dict, device: str) -> int | None:
    """The worst `delay` of the RX flows from `device`; None when it has none."""
    delays = [f["delay"] for f in status.get("rx") or []
              if isinstance(f, dict) and f.get("device") == device and type(f.get("delay")) is int]
    return max(delays) if delays else None


def offset(status: dict) -> int | None:
    engine = status.get("engine")
    d = engine.get("offset") if isinstance(engine, dict) else None
    return d if type(d) is int else None


def _channel(status: dict, subscriptions: dict, rx: int, d: int | None, ear: int | None) -> dict:
    sub = subscriptions.get(str(rx))
    if not isinstance(sub, dict) or not sub.get("tx_device"):
        return {"rx": rx, "device": None, "samples": None, "state": "unsubscribed"}
    device = sub["tx_device"]
    code = sub.get("status")
    delay = flow_delay(status, device)
    if type(code) is not int or code & 0xFFFF not in RECEIVING or delay is None or d is None or ear is None:
        return {"rx": rx, "device": device, "samples": None, "state": "unknown"}
    return {"rx": rx, "device": device, "samples": d + delay + ear, "state": "known"}


def paths(status: dict, subscriptions: dict, inputs: list[tuple[str, list[int]]]) -> list[dict]:
    """Per mic input: {input, rx, device, samples, ms, over_budget, state};
    `state` ok, over, unknown or unsubscribed. An input on several channels
    takes its worst: unknown when any channel is, else the longest path."""
    d, ear = offset(status), in_ear_samples(status)
    rows = []
    for ident, channels in inputs:
        each = [_channel(status, subscriptions, rx, d, ear) for rx in channels]
        subscribed = [c for c in each if c["state"] != "unsubscribed"]
        unknown = [c for c in subscribed if c["state"] == "unknown"]
        if not subscribed:
            pick, state = each[0], "unsubscribed"
        elif unknown:
            pick, state = unknown[0], "unknown"
        else:
            pick = max(subscribed, key=lambda c: c["samples"])
            state = "over" if pick["samples"] > BUDGET_SAMPLES else "ok"
        samples = pick["samples"] if state in ("ok", "over") else None
        rows.append({"input": ident, "rx": channels, "device": pick["device"], "samples": samples,
                     "ms": None if samples is None else round(samples * 1000 / RATE, 2),
                     "over_budget": state == "over", "state": state})
    return rows


def verdict(rows: list[dict]) -> dict:
    """{pass, budget_samples, paths}: a pass only when no mic input is over
    the budget or unknown (and at least one path is known)."""
    known = [r for r in rows if r["state"] in ("ok", "over")]
    bad = [r for r in rows if r["state"] in ("over", "unknown")]
    return {"pass": int(bool(known) and not bad), "budget_samples": BUDGET_SAMPLES, "paths": public(rows)}


def public(rows: list[dict]) -> list[dict]:
    """The rows as a gate may print them: the input id and the numbers, never
    the device's name or the channel numbers (P6)."""
    return [{k: r[k] for k in ("input", "samples", "ms", "over_budget", "state")} for r in rows]
