"""The mic-to-ear latency guard (#122): every engine input's path, from a
source's Dante sample stamp to the in-ear output's playout on the receiving
device, against the budget of 1 ms.

path = D + the input's RX flow delay + the in-ear receiver's latency, in
samples at the device's rate (`sample_rate` of the box's device config; the
budget is 1 ms at that rate, rounded up: 96 at 96 kHz):

- D is the engine's offset (`ctl status` `engine.offset`: B + P + W);
- the RX flow delay is the worst `delay` of the status `rx[]` flows whose
  `device` is the transmitter the input's RX channel is subscribed to (our
  subscriptions from ARC, `gen3_arc.read_rx`): several flows from one device
  take the worst;
- the in-ear receiver's latency is the worst `receiver_latency_us` of the
  status `tx[]` flows that carry channels and whose receiver plays them at
  `IN_EAR_MAX_US` (500 us) or less. That cut is the in-ear receivers' class
  (the stage rack plays our flows at 250 us): `ctl status` does not yet name
  the receiver of a TX flow (dstack#298), so the class stands in for it. A
  TX flow with channels above the cut is a receiver that is not an in-ear one
  only when its exact latency in us is listed (`GEN3_LATENCY_NON_INEAR_TX_US`
  in iempc's private env, `parse_non_inear`); any other is
  `tx_unclassified` and fails the guard: it may be an in-ear played later.

Not in the number: the converters, the wireless mic link and the in-ear
transmitters (hardware, outside Dante).

Every `[[engine.inputs]]` of the site is guarded but those on the exemption
list (`GEN3_LATENCY_EXEMPT`, `id:reason,id:reason`, `parse_exempt`):
playback sources and inputs nobody uses. An exempt input is `exempt` and
never fails. Every other input fails closed: one whose subscription is not
receiving, whose transmitter has no flow in the status, or whose path cannot
be read (no engine object, no in-ear flow) is `unknown`; one with no
subscription is `unsubscribed`; both fail like `over`.

Pure functions; `engine_inputs` reads a site's text, `sample_rate` a device
config's. Rows carry the device's name and the RX channels (site data):
`public` drops both before anything is printed; an exemption's reason is
never printed."""
from __future__ import annotations

import math
import tomllib

IN_EAR_MAX_US = 500   # the in-ear receivers' class (above)
RECEIVING = (9, 10)   # ARC 0x3000's Receiving, unicast or multicast (the status word's low 16 bits)
FAILING = ("over", "unknown", "unsubscribed")


def engine_inputs(site_text: str) -> list[tuple[str, list[int]]]:
    """Every `[[engine.inputs]]` of the site: (id, RX channels); ValueError
    when the site has no such list or an entry is not an id with channels."""
    engine = tomllib.loads(site_text).get("engine")
    inputs = engine.get("inputs") if isinstance(engine, dict) else None
    if not isinstance(inputs, list) or not inputs:
        raise ValueError("the site has no [[engine.inputs]]")
    out = []
    for item in inputs:
        if not isinstance(item, dict):
            raise ValueError("an [[engine.inputs]] entry is not a table")
        ident, rx = item.get("id"), item.get("rx")
        if not isinstance(ident, str) or not ident:
            raise ValueError("an [[engine.inputs]] entry has no id")
        if not isinstance(rx, list) or not rx or not all(type(n) is int and n >= 1 for n in rx):
            raise ValueError("an input's rx is not a list of channel numbers")
        out.append((ident, list(rx)))
    return out


def sample_rate(device_text: str) -> int:
    """The device config's `sample_rate`; ValueError when it has none."""
    rate = tomllib.loads(device_text).get("sample_rate")
    if type(rate) is not int or rate <= 0:
        raise ValueError("the device config has no sample_rate")
    return rate


def budget_samples(rate: int) -> int:
    """1 ms at `rate` in whole samples, rounded up."""
    return math.ceil(rate / 1000)


def parse_exempt(text: str) -> dict[str, str]:
    """`id:reason,id:reason` (GEN3_LATENCY_EXEMPT): {id: reason}; empty for
    none; ValueError for an item without an id or a reason, or an id twice."""
    out: dict[str, str] = {}
    for item in (w.strip() for w in text.split(",")):
        if not item:
            continue
        ident, sep, reason = item.partition(":")
        ident, reason = ident.strip(), reason.strip()
        if not sep or not ident or not reason or ident in out:
            raise ValueError("GEN3_LATENCY_EXEMPT is not a list of id:reason")
        out[ident] = reason
    return out


def parse_non_inear(text: str) -> frozenset[int]:
    """GEN3_LATENCY_NON_INEAR_TX_US: receiver latencies in us, commas;
    ValueError for anything but whole numbers."""
    words = [w.strip() for w in text.split(",") if w.strip()]
    if not all(w.isdecimal() for w in words):
        raise ValueError("GEN3_LATENCY_NON_INEAR_TX_US is not a list of microseconds")
    return frozenset(int(w) for w in words)


def samples_of_us(us: float, rate: int) -> int:
    """`us` microseconds in whole samples at `rate`, rounded up."""
    return math.ceil(us * rate / 1_000_000)


def tx_flows(status: dict, non_inear: frozenset[int] = frozenset()) -> list[dict]:
    """Each TX flow with channels: {receiver_latency_us, state}, `state`
    in_ear (at IN_EAR_MAX_US or less), non_inear (above it and listed) or
    tx_unclassified (above it, not listed)."""
    out = []
    for flow in status.get("tx") or []:
        if not isinstance(flow, dict):
            continue
        channels, us = flow.get("channels"), flow.get("receiver_latency_us")
        if type(channels) is not int or channels <= 0 or type(us) not in (int, float) or us <= 0:
            continue
        state = "in_ear" if us <= IN_EAR_MAX_US else "non_inear" if us in non_inear else "tx_unclassified"
        out.append({"receiver_latency_us": us, "state": state})
    return out


def in_ear_samples(status: dict, rate: int) -> int | None:
    """The in-ear receivers' latency in samples: the worst
    `receiver_latency_us` of the in-ear TX flows; None when no such flow."""
    worst = max((f["receiver_latency_us"] for f in tx_flows(status) if f["state"] == "in_ear"), default=None)
    return None if worst is None else samples_of_us(worst, rate)


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


def paths(status: dict, subscriptions: dict, inputs: list[tuple[str, list[int]]], rate: int,
          exempt: dict[str, str] | None = None) -> list[dict]:
    """Per input: {input, rx, device, samples, ms, over_budget, state};
    `state` ok, over, unknown, unsubscribed or exempt. An input on several
    channels takes its worst: unknown when any channel is, unsubscribed when
    any has no subscription, else the longest path."""
    d, ear, budget = offset(status), in_ear_samples(status, rate), budget_samples(rate)
    rows = []
    for ident, channels in inputs:
        if ident in (exempt or {}):
            rows.append({"input": ident, "rx": channels, "device": None, "samples": None, "ms": None,
                         "over_budget": False, "state": "exempt"})
            continue
        each = [_channel(status, subscriptions, rx, d, ear) for rx in channels]
        unknown = [c for c in each if c["state"] == "unknown"]
        unsubscribed = [c for c in each if c["state"] == "unsubscribed"]
        if unknown:
            pick, state = unknown[0], "unknown"
        elif unsubscribed:
            pick, state = unsubscribed[0], "unsubscribed"
        else:
            pick = max(each, key=lambda c: c["samples"])
            state = "over" if pick["samples"] > budget else "ok"
        samples = pick["samples"] if state in ("ok", "over") else None
        rows.append({"input": ident, "rx": channels, "device": pick["device"], "samples": samples,
                     "ms": None if samples is None else round(samples * 1000 / rate, 2),
                     "over_budget": state == "over", "state": state})
    return rows


def verdict(rows: list[dict], rate: int, tx: list[dict]) -> dict:
    """{pass, budget_samples, rate, paths, tx}: a pass only when at least one
    path is known, no input is over, unknown or unsubscribed, and no TX flow
    is tx_unclassified."""
    known = [r for r in rows if r["state"] in ("ok", "over")]
    bad = [r for r in rows if r["state"] in FAILING]
    unclassified = [f for f in tx if f["state"] == "tx_unclassified"]
    return {"pass": int(bool(known) and not bad and not unclassified), "budget_samples": budget_samples(rate),
            "rate": rate, "paths": public(rows), "tx": tx}


def public(rows: list[dict]) -> list[dict]:
    """The rows as a gate may print them: the input id and the numbers, never
    the device's name, the channel numbers (P6) or an exemption's reason."""
    return [{k: r[k] for k in ("input", "samples", "ms", "over_budget", "state")} for r in rows]
