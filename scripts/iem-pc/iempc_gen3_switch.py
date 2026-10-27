"""Gen 3 phase E's switch between iemmixer 2.x and 3.x (#72; plan
docs/superpowers/plans/2026-10-27-gen3-phase-e-band-test.md). This first
part is the RX mirror (Task 4): our own Dante device's RX table made equal
to the IEM card's, so the box receives the inputs the card receives.

`iempc gen3-mirror [--dry-run] [--clear]` (dev time, locked, no PC call):
both tables are read by IP over ARC (gen3_arc.read_rx: netaudio never reads
the last 8 of our device's 136 RX), each device first naming itself as
expected (the card DANTE_IEM_HOST/DANTE_IEM_NAME, our device GEN3_DANTE_IP
and GEN3_DANTE_NAME); `mirror_plan` lists what differs;
each change is one ARC 0x3010 to our device (gen3_arc), the flag read before
each; then our table is read again and must need no change. `--clear`
unsubscribes every RX of ours instead (the way back frees the sources'
flows). Only our own device is written (the Dante rule): the card is read.

A card RX subscribed to the card's own TX (its spare HIL returns) is not
mirrored: our RX stays unsubscribed there. Names reach only stdout, never a
log or a refusal's words."""
from __future__ import annotations

import json
import sys
import tomllib
from dataclasses import dataclass

import dante_card
import gen3_arc

KEYS = (("DANTE_IEM_HOST", "DANTE_IEM_NAME", "the IEM card"), ("GEN3_DANTE_IP", "GEN3_DANTE_NAME",
                                                               "our Gen 3 device"))
SEQ_BASE = 0x4700   # our requests' sequence numbers (any 16-bit value; one per change)


TAKEOVER_STATE = "/var/lib/iem-dante/takeover.toml"


class TakeoverError(Exception):
    """The card's labels cannot become our TX names, or the config is not one to edit."""


def takeover_config(ops_text: str, labels: dict[int, str], card_name: str) -> str:
    """Our device config under the card's name (phase E Task 3, the takeover
    proven live on #72): `name` the card's, `state_file` one of its own
    (a name saved in the usual state file would win over the config's), and
    every TX name the card's label of that channel, so each channel a
    receiver subscribes to by name exists on our device. Every other line is
    kept as it is. The config's `name`, `state_file` and `tx = [` ... `]`
    must each be one top-level entry (as the ops and test configs write
    them)."""
    if not dante_card.DEVICE_NAME.fullmatch(card_name):
        raise TakeoverError("the card's name is not a Dante device name")
    lines = ops_text.splitlines()
    starts = [i for i, line in enumerate(lines) if line.startswith("tx = [")]
    ends = [i for i, line in enumerate(lines) if line == "]" and starts and i > starts[0]]
    names = [i for i, line in enumerate(lines) if line.startswith("name = ")]
    states = [i for i, line in enumerate(lines) if line.startswith("state_file = ")]
    if len(starts) != 1 or not ends or len(names) != 1 or len(states) != 1:
        raise TakeoverError("the device config has no single name, state_file and tx = [ ... ] to replace")
    first, last = starts[0], ends[0]
    count = len(tomllib.loads(ops_text)["tx"])
    want = [labels.get(n) for n in range(1, count + 1)]
    if any(not isinstance(w, str) or not w or set(w) & dante_card.FORBIDDEN for w in want):
        raise TakeoverError(f"the card has no label for each of our {count} TX channels")
    if len(set(want)) != count:
        raise TakeoverError("the card's labels of our TX channels are not unique")
    out = list(lines)
    out[names[0]] = f"name = {json.dumps(card_name)}"
    out[states[0]] = f"state_file = {json.dumps(TAKEOVER_STATE)}"
    out[first:last + 1] = ["tx = [", *(f"  {json.dumps(w)}," for w in want), "]"]
    text = "\n".join(out) + "\n"
    new, old = tomllib.loads(text), tomllib.loads(ops_text)
    if {k: v for k, v in new.items() if k not in ("name", "state_file", "tx")} != \
            {k: v for k, v in old.items() if k not in ("name", "state_file", "tx")} or new["tx"] != want:
        raise TakeoverError("the edited config does not read back as the takeover")
    return text


class PlanError(Exception):
    """A table that is not dante_card.py's answer, or a card RX our device lacks."""


def table(doc, what: str) -> tuple[int, dict[int, tuple[str, str]]]:
    """`doc` (a `subscriptions` answer) as its RX count and {rx: (tx channel, tx device)}."""
    count, rx = (doc.get("rx_count"), doc.get("rx")) if isinstance(doc, dict) else (None, None)
    if type(count) is not int or count < 1 or not isinstance(rx, dict):
        raise PlanError(f"{what}: not a subscriptions answer")
    out = {}
    for key, sub in rx.items():
        ok = (isinstance(key, str) and key.isdigit() and 1 <= int(key) <= count and isinstance(sub, dict)
              and isinstance(sub.get("tx_channel"), str) and isinstance(sub.get("tx_device"), str))
        if not ok:
            raise PlanError(f"{what}: not a subscriptions answer")
        out[int(key)] = (sub["tx_channel"], sub["tx_device"])
    return count, out


def mirror_plan(card_doc, ours_doc, card_name: str, clear: bool = False) -> list[dict]:
    """The changes that make our RX table equal to the card's (each card RX n
    subscribed to another device: ours n the same; every other RX of ours
    unsubscribed), or, with `clear`, every RX of ours unsubscribed. Only the
    RX channels that differ, in channel order; tx_channel and tx_device None
    is an unsubscribe."""
    our_count, ours = table(ours_doc, "our device")
    want: dict[int, tuple[str, str]] = {}
    if not clear:
        _, card = table(card_doc, "the card")
        want = {n: sub for n, sub in card.items() if sub[1] != card_name}
        beyond = [n for n in want if n > our_count]
        if beyond:
            raise PlanError(f"the card's rx {min(beyond)} is subscribed and our device has {our_count} rx channels")
    changes = []
    for n in sorted(set(want) | set(ours)):
        if want.get(n) != ours.get(n):
            tx_channel, tx_device = want.get(n, (None, None))
            changes.append({"rx": n, "tx_channel": tx_channel, "tx_device": tx_device})
    return changes


@dataclass(frozen=True)
class Device:
    """A Dante device from the private env: its unicast IP and its expected name."""
    host: str
    name: str


def devices(ip, env: dict[str, str]) -> tuple[Device, Device]:
    """The card and our device from the private env; a key missing or not
    what it names refuses (its value is never printed)."""
    out = []
    for host_key, name_key, what in KEYS:
        missing = [k for k in (host_key, name_key) if not env.get(k)]
        if missing:
            raise ip.Refused(f"gen3-mirror: {', '.join(missing)} missing in the private env ({what}'s IP on the "
                             "Dante network and its device name)")
        if not dante_card.unicast_ipv4(env[host_key]):
            raise ip.Refused(f"gen3-mirror: {host_key} is not a unicast IPv4 address")
        if not dante_card.DEVICE_NAME.fullmatch(env[name_key]):
            raise ip.Refused(f"gen3-mirror: {name_key} is not a Dante device name")
        out.append(Device(env[host_key], env[name_key]))
    return out[0], out[1]


def read(ip, device: Device, what: str) -> dict:
    """`device`'s RX table, once it names itself as expected (StepError: no
    answer or another device)."""
    try:
        doc = gen3_arc.read_rx(device.host)
    except (OSError, ValueError) as e:
        raise ip.StepError(f"gen3-mirror: {what} could not be read ({type(e).__name__})") from None
    if doc["name"] != device.name:
        raise ip.StepError(f"gen3-mirror: the device answering as {what} is not the expected one")
    return doc


def send_changes(ip, host: str, changes: list[dict]) -> tuple[list[dict], str | None]:
    """Each change as one 0x3010 to `host`, the flag read before each: what was
    done (status 1) and what stopped the rest (None: all done)."""
    done = []
    for i, c in enumerate(changes):
        if ip.event_now():
            return done, f"the flag appeared: {len(done)} of {len(changes)} changes sent"
        seq = (SEQ_BASE + i) & 0xFFFF
        try:
            word = gen3_arc.status(gen3_arc.send(host, gen3_arc.subscription(c["rx"], c["tx_channel"], c["tx_device"],
                                                                             seq)), seq)
        except (OSError, ValueError) as e:
            return done, f"rx {c['rx']}: {type(e).__name__}: {len(done)} of {len(changes)} changes sent"
        if word != gen3_arc.DONE:
            return done, f"rx {c['rx']}: status {word:#06x}: {len(done)} of {len(changes)} changes sent"
        done.append(c)
    return done, None


def run_mirror(ctx, ip) -> int:
    """`iempc gen3-mirror` (dev time, locked; no PC call)."""
    args = ctx.args
    card, ours = devices(ip, ctx.env)
    card_doc = None if args.clear else read(ip, card, "the card")
    try:
        changes = mirror_plan(card_doc, read(ip, ours, "our device"), card.name, args.clear)
    except PlanError as e:
        raise ip.Refused(f"gen3-mirror: {e}: nothing was sent") from None
    if args.dry_run:
        ip.emit({"gen3_mirror": {"dry_run": True, "changes": changes}})
        return 0
    done, stopped = send_changes(ip, ours.host, changes)
    try:
        left = mirror_plan(card_doc, read(ip, ours, "our device"), card.name, args.clear)
    except (PlanError, ip.StepError) as e:
        left, stopped = None, "; ".join(x for x in (stopped, f"not read back: {e}") if x)
    ip.emit({"gen3_mirror": {"sent": len(done), "of": len(changes), "left": left, "error": stopped}})
    if stopped or left:
        print(f"iempc: gen3-mirror: {stopped or f'{len(left)} rx channels did not read back as wanted'}",
              file=sys.stderr, flush=True)
        return 1
    return 0
