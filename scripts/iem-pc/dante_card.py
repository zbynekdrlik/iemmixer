"""The IEM card's Dante channel labels, read and written by IP (#58 plan
Task 15, outcome A on #62). The one module that imports netaudio (the
predecessor's Dante library, 0.0.10): iempc_dante runs it in a subprocess
with the netaudio venv's python (`-I`, this file's path as an argument), so
iempc itself never imports it.

The card sits on a routed subnet: mDNS does not reach the dev box, unicast
does, so the card is addressed by its IP on the ARC port, never discovered.

  read <ip> <expected-name>    stdout: one JSON object {name, rx_count,
                               tx_count, tx: {"<n>": label}, rx: {...}}
  write <ip> <expected-name>   stdin: a JSON list of {type: "tx"|"rx",
                               channel, label}; stdout: one JSON object
                               {written, mismatches: [{type, channel, want,
                               got}], tx, rx} (the labels read back)

Before anything is read out or written, the device answering at <ip> must
name itself <expected-name>; else nothing is written, exit 1. It writes
channel labels only: a label equal to the channel's default (its number) is
a reset of the label, any other a new label. Nothing else of the card
(routing, its clock, its audio format, its own name) and no other device is
ever touched: only `get_controls`, `set_channel_name` and
`reset_channel_name` of the one device are called (test_dante_card checks
the source). Every written label is read back from a fresh read of the
card; a label that does not read back is a mismatch, reported (exit 0: the
caller decides).

Exit 0 answered, 1 the card did not answer, is not the expected device, or
lacks a channel asked for (nothing written), 2 usage. netaudio prints its
own errors on stdout; they go to stderr here, so stdout holds one JSON line.
Tests inject a fake device through `open_device`."""
from __future__ import annotations

import asyncio
import contextlib
import ipaddress
import json
import sys

ARC_PORT = 4440
TYPES = ("tx", "rx")
MAX_LABEL = 31
# Characters Dante refuses in a channel label.
FORBIDDEN = frozenset("=.@")


class Usage(Exception):
    """Bad arguments or input: nothing was asked of the card (exit 2)."""


class Refused(Exception):
    """The card did not answer, is not the expected device, or lacks a channel (exit 1)."""


def open_device(ip: str):
    """A netaudio DanteDevice for the card at `ip`, addressed by IP on the ARC
    port (no mDNS). The one place netaudio is imported; tests replace it."""
    from netaudio.dante.const import SERVICE_ARC
    from netaudio.dante.device import DanteDevice

    device = DanteDevice(server_name=f"iempc-{ip}")
    device.ipv4 = ipaddress.ip_address(ip)
    device.services = {"arc": {"type": SERVICE_ARC, "port": ARC_PORT}}
    return device


async def connect(ip: str, expected: str):
    """The device at `ip`, read, once it names itself `expected`."""
    device = open_device(ip)
    with contextlib.redirect_stdout(sys.stderr):
        await device.get_controls()
    if device.error:
        raise Refused(f"the card did not answer ({type(device.error).__name__})")
    if device.name != expected:
        raise Refused("the device answering at the configured host is not the expected one")
    return device


def labels(device) -> dict:
    """{"tx": {"<n>": label}, "rx": {...}} as the card holds them now."""
    def of(channels) -> dict:
        return {str(n): c.friendly_name or c.name for n, c in sorted(channels.items())}

    return {"tx": of(device.tx_channels), "rx": of(device.rx_channels)}


def parse_changes(text: str) -> list[dict]:
    """The write verb's stdin: a list of {type, channel, label}, each label
    one Dante can hold. Usage when not."""
    try:
        doc = json.loads(text)
    except ValueError:
        raise Usage("stdin is not JSON") from None
    if not isinstance(doc, list) or not doc:
        raise Usage("stdin must be a non-empty list of {type, channel, label}")
    out = []
    for c in doc:
        if not isinstance(c, dict) or set(c) != {"type", "channel", "label"}:
            raise Usage("each change is exactly {type, channel, label}")
        kind, channel, label = c["type"], c["channel"], c["label"]
        if kind not in TYPES or type(channel) is not int or channel < 1:
            raise Usage("a change names no tx/rx channel")
        if (not isinstance(label, str) or not 1 <= len(label) <= MAX_LABEL or not label.isascii()
                or not label.isprintable() or set(label) & FORBIDDEN):
            raise Usage(f"{kind} {channel}: not a label Dante holds (1 to {MAX_LABEL} printable ASCII characters, "
                        "none of = . @)")
        out.append({"type": kind, "channel": channel, "label": label})
    if len({(c["type"], c["channel"]) for c in out}) != len(out):
        raise Usage("a channel is named twice")
    return out


async def read(ip: str, expected: str) -> dict:
    device = await connect(ip, expected)
    return {"name": device.name, "rx_count": device.rx_count, "tx_count": device.tx_count, **labels(device)}


async def write(ip: str, expected: str, changes: list[dict]) -> dict:
    try:
        device = await connect(ip, expected)
    except Refused as e:
        raise Refused(f"{e}: nothing was written") from None
    held = labels(device)
    missing = [c for c in changes if str(c["channel"]) not in held[c["type"]]]
    if missing:
        raise Refused(f"the card has no {missing[0]['type']} channel {missing[0]['channel']}: nothing was written")
    with contextlib.redirect_stdout(sys.stderr):
        for c in changes:
            if c["label"] == str(c["channel"]):
                await device.reset_channel_name(c["type"], c["channel"])
            else:
                await device.set_channel_name(c["type"], c["channel"], c["label"])
    try:
        after = labels(await connect(ip, expected))
    except Refused as e:
        raise Refused(f"{len(changes)} labels were written, but not read back: {e}") from None
    mismatches = [{**c, "want": c["label"], "got": after[c["type"]].get(str(c["channel"]))}
                  for c in changes if after[c["type"]].get(str(c["channel"])) != c["label"]]
    for m in mismatches:
        del m["label"]
    return {"written": len(changes), "mismatches": mismatches, **after}


def main(argv: list[str], stdin) -> int:
    try:
        if len(argv) != 3 or argv[0] not in ("read", "write"):
            raise Usage("usage: dante_card.py read|write <ip> <expected-name>")
        verb, ip, expected = argv
        try:
            ipaddress.ip_address(ip)
        except ValueError:
            raise Usage("<ip> is not an IP address") from None
        if not expected or not expected.isprintable():
            raise Usage("<expected-name> is empty or not printable")
        if verb == "read":
            out = asyncio.run(read(ip, expected))
        else:
            out = asyncio.run(write(ip, expected, parse_changes(stdin.read())))
    except Usage as e:
        print(f"dante_card: {e}", file=sys.stderr)
        return 2
    except Refused as e:
        print(f"dante_card: {e}", file=sys.stderr)
        return 1
    print(json.dumps(out))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:], sys.stdin))
