"""The mic-to-ear latency guard (#122): every engine input's path, from a
source's Dante sample stamp to the in-ear output's playout on the receiving
device, against the budget of 1 ms.

path = D + the input's RX flow delay + the in-ear receivers' latency, in
samples at the device's rate (`sample_rate` of the box's device config; the
budget is 1 ms at that rate, rounded down, never above 1 ms: 96 at 96 kHz).
Every flow is matched by channel numbers, which `ctl status` names (dstack
v0.6.137):

- D is the engine's offset (`ctl status` `engine.offset`: B + P + W);
- the RX flow delay is the worst `delay` of the status `rx[]` flows whose
  `rx_channels` (our RX channels, 1-based) hold the input's RX channel; two
  flows feeding one channel take the worst;
- the in-ear receivers' latency is the worst `receiver_latency_us` of the
  status `tx[]` flows whose `tx_channels` (our TX channels, 1-based, null for
  an empty slot) carry an in-ear output.

The in-ear outputs are the TX channels of every `[[engine.mixes]]` of the
site but the engineer's (`[engine] engineer`, the mix with the listen tap:
the engineer's headphones, not an in-ear): the band members' sets and every
other mix (the translator's mono mix too, so a mix the site adds is guarded
unless it is the engineer's). A TX flow is `in_ear` when it carries one of
them and its receiver latency is known, `tx_unread` when it carries one and
its `receiver_latency_us` is 0 or missing (dstack's "not known": a multicast
flow, a receiver not read), `tx_unmapped` when it has channels but no
readable `tx_channels`, and `non_inear` when it carries only other channels
(the engineer's mix, a spare output); `tx_unread` and `tx_unmapped` fail the
guard.

Not in the number: the converters, the wireless mic link and the in-ear
transmitters (hardware, outside Dante).

Every `[[engine.inputs]]` of the site is guarded but those on the exemption
list (`GEN3_LATENCY_EXEMPT`, `id:reason,id:reason`, `parse_exempt`):
playback sources and inputs nobody uses. An exempt input is `exempt` and
never fails. Every other input fails closed: one on a channel without a
subscription (our subscriptions from ARC, `gen3_arc.read_rx`) is
`unsubscribed`; one on a subscribed channel no RX flow carries, or whose path
cannot be read (no engine object, no in-ear flow), is `unknown`; both fail
like `over`. A channel in `GEN3_OPTIONAL_RX` (a source a musician carries
away, which fails no gate off) that no RX flow carries and that does not
show Receiving is `off`: it adds
nothing to its input's path, and an input whose every channel is `off` reads
`off` and never fails (the soak of 2026-11-19 failed on the keys input, its
source switched off).

Pure functions; `engine_inputs` and `in_ear_tx` read a site's text,
`sample_rate` a device config's. Rows carry the RX channels (site data):
`public` drops them before anything is printed; no device name is read into
a row and an exemption's reason is never printed."""
from __future__ import annotations

import math
import tomllib

FAILING = ("over", "unknown", "unsubscribed")
TX_FAILING = ("tx_unread", "tx_unmapped")


def _engine(site_text: str) -> dict:
    engine = tomllib.loads(site_text).get("engine")
    return engine if isinstance(engine, dict) else {}


def _channels(value) -> bool:
    """A non-empty list of channel numbers (1-based)."""
    return isinstance(value, list) and bool(value) and all(type(n) is int and n >= 1 for n in value)


def engine_inputs(site_text: str) -> list[tuple[str, list[int]]]:
    """Every `[[engine.inputs]]` of the site: (id, RX channels); ValueError
    when the site has no such list or an entry is not an id with channels."""
    inputs = _engine(site_text).get("inputs")
    if not isinstance(inputs, list) or not inputs:
        raise ValueError("the site has no [[engine.inputs]]")
    out = []
    for item in inputs:
        if not isinstance(item, dict):
            raise ValueError("an [[engine.inputs]] entry is not a table")
        ident, rx = item.get("id"), item.get("rx")
        if not isinstance(ident, str) or not ident:
            raise ValueError("an [[engine.inputs]] entry has no id")
        if not _channels(rx):
            raise ValueError("an input's rx is not a list of channel numbers")
        out.append((ident, list(rx)))
    return out


def in_ear_tx(site_text: str) -> frozenset[int]:
    """The in-ear outputs: the TX channels of every `[[engine.mixes]]` but
    the one `[engine] engineer` names; ValueError when the site names no
    engineer, has no such mix, or an entry is not an id with channels."""
    engine = _engine(site_text)
    engineer, mixes = engine.get("engineer"), engine.get("mixes")
    if not isinstance(engineer, str) or not engineer:
        raise ValueError("the site names no engineer")
    if not isinstance(mixes, list) or not mixes:
        raise ValueError("the site has no [[engine.mixes]]")
    out: set[int] = set()
    seen = False
    for item in mixes:
        if not isinstance(item, dict) or not isinstance(item.get("id"), str) or not _channels(item.get("tx")):
            raise ValueError("an [[engine.mixes]] entry is not an id with tx channels")
        if item["id"] == engineer:
            seen = True
        else:
            out.update(item["tx"])
    if not seen:
        raise ValueError("the engineer's mix is not among [[engine.mixes]]")
    return frozenset(out)


def sample_rate(device_text: str) -> int:
    """The device config's `sample_rate`; ValueError when it has none."""
    rate = tomllib.loads(device_text).get("sample_rate")
    if type(rate) is not int or rate <= 0:
        raise ValueError("the device config has no sample_rate")
    return rate


def budget_samples(rate: int) -> int:
    """1 ms at `rate` in whole samples, rounded down (never above 1 ms)."""
    return rate // 1000


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


def parse_budgets(text: str) -> dict[str, int]:
    """`id:samples,id:samples` (GEN3_LATENCY_BUDGET): an input's own budget
    in samples, the owner's call for a device that cannot reach 1 ms (its
    path pinned at today's value, so growth still fails); empty for none;
    ValueError for an item without an id, a budget not a whole number above
    0, or an id twice."""
    out: dict[str, int] = {}
    for item in (w.strip() for w in text.split(",")):
        if not item:
            continue
        ident, sep, value = item.partition(":")
        ident, value = ident.strip(), value.strip()
        if not sep or not ident or not value.isdecimal() or int(value) <= 0 or ident in out:
            raise ValueError("GEN3_LATENCY_BUDGET is not a list of id:samples")
        out[ident] = int(value)
    return out


def samples_of_us(us: float, rate: int) -> int:
    """`us` microseconds in whole samples at `rate`, rounded up."""
    return math.ceil(us * rate / 1_000_000)


def tx_flows(status: dict, in_ear: frozenset[int]) -> list[dict]:
    """Each TX flow with channels: {receiver_latency_us, state}, `state`
    in_ear (carries an in-ear output, latency known), tx_unread (carries one,
    no latency known: dstack's 0), tx_unmapped (no readable `tx_channels`)
    or non_inear (carries no in-ear output). dstack states the latency in
    whole us (truncated from ns), so a path can read at most one sample
    short."""
    out = []
    for flow in status.get("tx") or []:
        if not isinstance(flow, dict):
            continue
        channels, us = flow.get("channels"), flow.get("receiver_latency_us")
        if type(channels) is not int or channels <= 0:
            continue
        known = us if type(us) in (int, float) and us > 0 else None
        carried = flow.get("tx_channels")
        if not isinstance(carried, list) or not all(n is None or (type(n) is int and n >= 1) for n in carried):
            out.append({"receiver_latency_us": known, "state": "tx_unmapped"})
        elif not in_ear.intersection(n for n in carried if n is not None):
            out.append({"receiver_latency_us": known, "state": "non_inear"})
        elif known is None:
            # dstack's 0 is "not known" (a multicast flow, a receiver not read):
            # it feeds an in-ear at a latency nobody read, so it fails the guard.
            out.append({"receiver_latency_us": None, "state": "tx_unread"})
        else:
            out.append({"receiver_latency_us": known, "state": "in_ear"})
    return out


def in_ear_samples(tx: list[dict], rate: int) -> int | None:
    """The in-ear receivers' latency in samples: the worst
    `receiver_latency_us` of the in_ear flows of `tx` (`tx_flows`); None
    when there is none."""
    worst = max((f["receiver_latency_us"] for f in tx if f["state"] == "in_ear"), default=None)
    return None if worst is None else samples_of_us(worst, rate)


def flow_delay(status: dict, rx: int) -> int | None:
    """The worst `delay` of the RX flows whose `rx_channels` hold our RX
    channel `rx`; None when no flow carries it."""
    delays = [f["delay"] for f in status.get("rx") or []
              if isinstance(f, dict) and isinstance(f.get("rx_channels"), list)
              and rx in f["rx_channels"] and type(f.get("delay")) is int]
    return max(delays) if delays else None


def receiving(status: dict, rx: int) -> bool:
    """Whether the status's `channels` row of our RX channel `rx` shows a
    subscription that receives (the status word's low 16 bits 9 unicast or 10
    multicast, as `iempc_gen3.is_receiving`)."""
    return any(isinstance(c, dict) and c.get("rx") == rx and type(c.get("status")) is int
               and (c["status"] & 0xFFFF) in (9, 10) for c in status.get("channels") or [])


def offset(status: dict) -> int | None:
    engine = status.get("engine")
    d = engine.get("offset") if isinstance(engine, dict) else None
    return d if type(d) is int else None


def _channel(status: dict, subscriptions: dict, rx: int, d: int | None, ear: int | None,
             optional: frozenset[int]) -> dict:
    sub = subscriptions.get(str(rx))
    if not isinstance(sub, dict) or not sub.get("tx_device"):
        return {"rx": rx, "samples": None, "state": "unsubscribed"}
    delay = flow_delay(status, rx)
    # Off only when it receives nothing: a receiving channel no flow maps
    # (an unreadable mapping) stays unknown, as a required one.
    if delay is None and rx in optional and not receiving(status, rx):
        return {"rx": rx, "samples": None, "state": "off"}
    if delay is None or d is None or ear is None:
        return {"rx": rx, "samples": None, "state": "unknown"}
    return {"rx": rx, "samples": d + delay + ear, "state": "known"}


def paths(status: dict, subscriptions: dict, inputs: list[tuple[str, list[int]]], rate: int,
          tx: list[dict], exempt: dict[str, str] | None = None,
          budgets: dict[str, int] | None = None,
          optional: frozenset[int] = frozenset()) -> list[dict]:
    """Per input: {input, rx, samples, ms, over_budget, state}; `state` ok,
    over, unknown, unsubscribed, off or exempt. `tx` is `tx_flows`'s list. An
    input on several channels takes its worst over the channels that are not
    `off` (an `optional` channel no flow carries): unknown when any is,
    unsubscribed when any has no subscription, else the longest path; `off`
    when every channel is. An input in `budgets` is over above its own
    budget, every other above 1 ms."""
    d, ear, budget = offset(status), in_ear_samples(tx, rate), budget_samples(rate)
    rows = []
    for ident, channels in inputs:
        if ident in (exempt or {}):
            rows.append({"input": ident, "rx": channels, "samples": None, "ms": None,
                         "over_budget": False, "state": "exempt"})
            continue
        states = [c for c in (_channel(status, subscriptions, rx, d, ear, optional) for rx in channels)
                  if c["state"] != "off"]
        kinds = {c["state"] for c in states}
        if not states:
            state, samples = "off", None
        elif "unknown" in kinds:
            state, samples = "unknown", None
        elif "unsubscribed" in kinds:
            state, samples = "unsubscribed", None
        else:
            samples = max(c["samples"] for c in states)
            state = "over" if samples > (budgets or {}).get(ident, budget) else "ok"
        rows.append({"input": ident, "rx": channels, "samples": samples,
                     "ms": None if samples is None else round(samples * 1000 / rate, 2),
                     "over_budget": state == "over", "state": state})
    return rows


def verdict(rows: list[dict], rate: int, tx: list[dict]) -> dict:
    """{pass, budget_samples, rate, paths, tx}: a pass only when at least one
    path is known, no input is over, unknown or unsubscribed, and no TX flow
    is tx_unread or tx_unmapped."""
    known = [r for r in rows if r["state"] in ("ok", "over")]
    bad = [r for r in rows if r["state"] in FAILING]
    unread = [f for f in tx if f["state"] in TX_FAILING]
    return {"pass": int(bool(known) and not bad and not unread), "budget_samples": budget_samples(rate),
            "rate": rate, "paths": public(rows), "tx": tx}


def public(rows: list[dict]) -> list[dict]:
    """The rows as a gate may print them: the input id and the numbers, never
    the channel numbers (P6) or an exemption's reason."""
    return [{k: r[k] for k in ("input", "samples", "ms", "over_budget", "state")} for r in rows]
