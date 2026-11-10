"""The receiver side of our TX (#72, the gate item of comment 6547006064): what
Dante Controller's latency monitor shows for the receivers of the box's TX,
read on the box by `iem-dante-dev receivers` (crates/iem-dante-dev/src/
receivers.rs: each receiver's own heartbeat, its RX flows that carry our TX,
their latency, their largest lateness of a second and their late packets).

`words` is the box command, `parse` reads its line, `Tally` sums the reads of
a run: a receiver's late count of a flow of ours (`late_total`, the
receiver's own count since it started) summed read to read per (receiver
key, slot), a count that went back counted from 0 (its device started
again), a flow's first read counting the late packets of its listen; and
every listen's peak past the receiver's latency. Red (`late` or `over`
above 0) is the gate's `receiver_late`. `receiver_unread`: no read heard a
receiver of our TX at all, or a read did not hear every receiver of our TX
it found (its late packets would go uncounted), or more reads failed than
gave a line (a gate that cannot read never passes on it). A device whose ARC
read failed, or one without a heartbeat id, is counted (`unread_max`,
`no_id_max`) and fails nothing: it may receive nothing of ours, and the
band's network holds such devices (a wireless microphone receiver). The
peaks compare in whole µs: a sample is above 10 µs at 96 kHz, so the floor
never hides a peak a sample past the latency. Accepted limit: a receiver slot reused by another sender between two
reads counts that sender's late packets against us (a false fail, never a
miss).

Numbers only: a receiver is an opaque key (a hash of its heartbeat id, made
on the box); no name, address or id reaches a line."""
from __future__ import annotations

import re
from dataclasses import dataclass, field

SECONDS = 5   # each read listens this long to the heartbeats
KEY = re.compile(r"[0-9a-f]{16}")
COUNTS = ("devices", "unread", "no_id", "receivers", "heard")
FLOW_INTS = ("slot", "latency_us", "beats", "late", "late_total")


def words(dante_bin: str, dante_ip: str) -> list[str]:
    """The box command: `iem-dante-dev receivers` on our Dante address."""
    return [dante_bin, "receivers", "--ip", dante_ip, "--seconds", str(SECONDS)]


def _count(value) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


@dataclass(frozen=True)
class Flow:
    key: str
    slot: int
    latency_us: int
    beats: int
    peak_us: int | None
    late: int
    late_total: int


@dataclass(frozen=True)
class Read:
    receivers: int
    heard: int
    unread: int
    no_id: int
    flows: tuple[Flow, ...]


def parse(doc) -> Read | None:
    """A `{"receivers": {...}}` line's read; None for anything else."""
    r = doc.get("receivers") if isinstance(doc, dict) else None
    if not isinstance(r, dict) or not all(_count(r.get(k)) for k in COUNTS):
        return None
    if not isinstance(r.get("flows"), list) or not isinstance(r.get("red"), bool):
        return None
    flows = []
    for f in r["flows"]:
        if not isinstance(f, dict) or not isinstance(f.get("key"), str) or not KEY.fullmatch(f["key"]):
            return None
        if not all(_count(f.get(k)) for k in FLOW_INTS):
            return None
        peak = f.get("peak_us")
        if peak is not None and not _count(peak):
            return None
        flows.append(Flow(f["key"], f["slot"], f["latency_us"], f["beats"], peak, f["late"], f["late_total"]))
    return Read(r["receivers"], r["heard"], r["unread"], r["no_id"], tuple(flows))


@dataclass
class Tally:
    reads: int = 0   # reads that gave a line
    failed: int = 0   # reads that gave none
    heard_max: int = 0   # the most receivers of our TX one read heard
    unheard: int = 0   # reads that did not hear every receiver of our TX they found
    unread_max: int = 0   # the most devices one read could not read over ARC (information)
    no_id_max: int = 0   # the most devices one read found without a heartbeat id (information)
    flows_max: int = 0   # the most of our flows at them one read heard
    late: int = 0
    over: int = 0   # listens whose peak passed the receiver's latency, a flow each
    margin_us: int | None = None   # the least latency less peak over the heard flows
    last: dict[tuple[str, int], int] = field(default_factory=dict)

    def see(self, read: Read | None) -> None:
        if read is None:
            self.failed += 1
            return
        self.reads += 1
        self.heard_max = max(self.heard_max, read.heard)
        self.unread_max = max(self.unread_max, read.unread)
        self.no_id_max = max(self.no_id_max, read.no_id)
        if read.heard < read.receivers:
            self.unheard += 1
        heard = [f for f in read.flows if f.beats > 0]
        self.flows_max = max(self.flows_max, len(heard))
        for f in heard:
            k = (f.key, f.slot)
            before = self.last.get(k)
            if before is None:
                self.late += f.late
            else:
                self.late += f.late_total - before if f.late_total >= before else f.late_total
            self.last[k] = f.late_total
            if f.peak_us is not None:
                if f.peak_us > f.latency_us:
                    self.over += 1
                margin = f.latency_us - f.peak_us
                self.margin_us = margin if self.margin_us is None else min(self.margin_us, margin)

    def failures(self) -> list[str]:
        out = []
        if self.late or self.over:
            out.append("receiver_late")
        if self.heard_max == 0 or self.unheard or self.failed > self.reads:
            out.append("receiver_unread")
        return out

    def numbers(self) -> dict:
        return {"reads": self.reads, "failed": self.failed, "heard_max": self.heard_max, "unheard": self.unheard,
                "unread_max": self.unread_max, "no_id_max": self.no_id_max, "flows_max": self.flows_max, "late": self.late, "over": self.over, "margin_us": self.margin_us}
