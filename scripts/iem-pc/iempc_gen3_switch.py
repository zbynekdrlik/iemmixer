"""Gen 3 phase E's switch between iemmixer 2.x and 3.x (#72; plan
docs/superpowers/plans/2026-10-27-gen3-phase-e-band-test.md). This first
part is the RX mirror (Task 4): our own Dante device's RX table made equal
to the IEM card's, so the box receives the inputs the card receives.

`iempc gen3-mirror [--dry-run] [--clear]` (dev time, locked, no PC call):
both tables are read by IP with dante_card.py's `subscriptions` verb (the
card from DANTE_IEM_HOST/DANTE_IEM_NAME, our device from GEN3_DANTE_IP and
GEN3_DANTE_NAME, NETAUDIO_PYTHON for both); `mirror_plan` lists what differs;
each change is one ARC 0x3010 to our device (gen3_arc), the flag read before
each; then our table is read again and must need no change. `--clear`
unsubscribes every RX of ours instead (the way back frees the sources'
flows). Only our own device is written (the Dante rule): the card is read.

A card RX subscribed to the card's own TX (its spare HIL returns) is not
mirrored: our RX stays unsubscribed there. Names reach only stdout, never a
log or a refusal's words."""
from __future__ import annotations

import sys

import dante_card
import gen3_arc
import iempc_dante

OURS_KEYS = ("GEN3_DANTE_IP", "GEN3_DANTE_NAME")
SEQ_BASE = 0x4700   # our requests' sequence numbers (any 16-bit value; one per change)


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


def ours_env(ip, env: dict[str, str]) -> iempc_dante.Card:
    """Our device from the private env, as dante_card.py reads it (its IP, its
    expected name, the netaudio python the card's keys name)."""
    missing = [k for k in OURS_KEYS if not env.get(k)]
    if missing:
        raise ip.Refused(f"gen3-mirror: {', '.join(missing)} missing in the private env (GEN3_DANTE_IP: our Gen 3 "
                         "device's IP on the Dante network, GEN3_DANTE_NAME: its device name)")
    host, name = env["GEN3_DANTE_IP"], env["GEN3_DANTE_NAME"]
    if not dante_card.unicast_ipv4(host):
        raise ip.Refused("gen3-mirror: GEN3_DANTE_IP is not a unicast IPv4 address")
    if not dante_card.DEVICE_NAME.fullmatch(name):
        raise ip.Refused("gen3-mirror: GEN3_DANTE_NAME is not a Dante device name")
    card = iempc_dante.card_env(ip, env, need=True)
    return iempc_dante.Card(host, name, card.python)


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
    card = iempc_dante.card_env(ip, ctx.env, need=True)
    ours = ours_env(ip, ctx.env)
    card_doc = None if args.clear else iempc_dante.ask_card(ip, card, "subscriptions")
    try:
        changes = mirror_plan(card_doc, iempc_dante.ask_card(ip, ours, "subscriptions"), card.name, args.clear)
    except PlanError as e:
        raise ip.Refused(f"gen3-mirror: {e}: nothing was sent") from None
    if args.dry_run:
        ip.emit({"gen3_mirror": {"dry_run": True, "changes": changes}})
        return 0
    done, stopped = send_changes(ip, ours.host, changes)
    try:
        left = mirror_plan(card_doc, iempc_dante.ask_card(ip, ours, "subscriptions"), card.name, args.clear)
    except (PlanError, ip.StepError) as e:
        left, stopped = None, "; ".join(x for x in (stopped, f"not read back: {e}") if x)
    ip.emit({"gen3_mirror": {"sent": len(done), "of": len(changes), "left": left, "error": stopped}})
    if stopped or left:
        print(f"iempc: gen3-mirror: {stopped or f'{len(left)} rx channels did not read back as wanted'}",
              file=sys.stderr, flush=True)
        return 1
    return 0
