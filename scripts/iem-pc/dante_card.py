"""The IEM card's Dante channel labels, read and written by IP (#58 plan
Task 15, outcome A on #62). The one module that imports netaudio (the
predecessor's Dante library, 0.0.10): iempc_dante runs it in a subprocess
with the netaudio venv's python (`-I`, this file's path as an argument), so
iempc itself never imports netaudio.

The card sits on a routed subnet: mDNS does not reach the dev box, unicast
does, so the card is addressed by its IP on the ARC port, never discovered.

  read <ip> <expected-name>             stdout: one JSON object {name,
                                        rx_count, tx_count, tx: {"<n>": label},
                                        rx: {...}}
  write <ip> <expected-name> <flag>     stdin: a JSON list of {type: "tx"|"rx",
                                        channel, label}; stdout: one JSON
                                        object {sent, failed, error, read_back,
                                        mismatches, tx, rx}
  rename <ip> <expected-name> <new-name>
                                        stdout: one JSON object {renamed, name,
                                        error} (Gen 3 phase E's switch, #72:
                                        the card takes a spare name so the box
                                        can take the card's, and back)

<ip> is a unicast IPv4 address. Before anything is read out or written, the
device answering at <ip> must name itself <expected-name> and give a complete
read: a name, and as many TX and RX channels as it reports (netaudio's
get_controls leaves its error None whether the card answered or not, so
silence shows only as an empty read). Else nothing is written, exit 1.

It writes channel labels and, through `rename` alone, the card's own name: a
label equal to the channel's default (its number) is a reset of the label,
any other a new label. Nothing else of the card (routing, its clock, its
audio format) and no other device is ever touched: only `get_controls`,
`set_channel_name`, `reset_channel_name` and `set_name` of the one device are
called (test_dante_card checks the source). The new name must be one Dante
holds (1 to 31 letters, digits and hyphens, no hyphen at either end); that
no other device announces it is the caller's check (the dev box's mDNS does
not reach the card's subnet).

A rename, once sent, ends with one JSON line, exit 0: `renamed` is whether a
fresh read finds the card under the new name, `name` the name it holds then
(null when it did not answer), `error` what went wrong, else null.

Once writing has started, the helper always ends with one JSON line, exit 0,
whatever happens, so the caller learns which labels changed:
- `sent`: the changes whose command returned, in order;
- `failed`: the change whose command raised (netaudio's commands catch only a
  timeout), which ends the writes; else null;
- the <flag> file (iempc's EVENT-NOW) is checked before each write: when it
  appears, the writes stop;
- then a fresh read of the card: `read_back` says whether it answered, `tx`
  and `rx` are the labels it holds then (null when not read back), and
  `mismatches` lists each sent or failed change whose label did not read back
  ([] when not read back: `read_back` says so);
- `error`: what stopped the writes or the read back, else null.

Exit 0 written (see `error`), 1 the card did not answer, is not the expected
device, gave an incomplete read or lacks a channel asked for (nothing
written), 2 usage. netaudio prints its own errors on stdout; they go to
stderr here, so stdout holds one JSON line. Tests inject a fake device
through `open_device`."""
from __future__ import annotations

import asyncio
import contextlib
import ipaddress
import json
import re
import sys
from pathlib import Path

ARC_PORT = 4440
TYPES = ("tx", "rx")
MAX_LABEL = 31
MAX_CHANNEL = 255
# Characters Dante refuses in a channel label.
FORBIDDEN = frozenset("=.@")
# A Dante device name: letters, digits and hyphens, 1 to 31, no hyphen at an end.
DEVICE_NAME = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,29}[A-Za-z0-9])?")


class Usage(Exception):
    """Bad arguments or input: nothing was asked of the card (exit 2)."""


class Refused(Exception):
    """The card did not answer, is not the expected device, gave an incomplete
    read, or lacks a channel (exit 1, nothing written)."""


def unicast_ipv4(text: str) -> bool:
    """`text` is one IPv4 address of one host: not IPv6, multicast,
    loopback, unspecified, or reserved (240/4, which holds the broadcast
    address). A link-local address stays allowed: Dante cards use it when
    no DHCP answers. iempc_dante checks its env with this too."""
    try:
        address = ipaddress.IPv4Address(text)
    except ValueError:
        return False
    return not (address.is_multicast or address.is_loopback or address.is_unspecified or address.is_reserved)


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
    """The device at `ip`, read completely, once it names itself `expected`."""
    device = open_device(ip)
    with contextlib.redirect_stdout(sys.stderr):
        await device.get_controls()
    if not device.name:
        raise Refused("the card did not answer")
    if device.name != expected:
        raise Refused("the device answering at the configured host is not the expected one")
    for kind in TYPES:
        count, channels = getattr(device, f"{kind}_count"), getattr(device, f"{kind}_channels")
        if not (type(count) is int and count > 0 and len(channels) == count):
            raise Refused(f"incomplete read of the card's {kind} channels")
    return device


def labels(device) -> dict:
    """{"tx": {"<n>": label}, "rx": {...}} as the card holds them now."""
    def of(channels) -> dict:
        return {str(n): c.friendly_name or c.name for n, c in sorted(channels.items())}

    return {"tx": of(device.tx_channels), "rx": of(device.rx_channels)}


def parse_changes(text: str) -> list[dict]:
    """The write verb's stdin: a list of {type, channel, label}, each label
    one Dante can hold on a channel Dante numbers. Usage when not."""
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
        if kind not in TYPES or type(channel) is not int or not 1 <= channel <= MAX_CHANNEL:
            raise Usage(f"a change names no tx/rx channel (1 to {MAX_CHANNEL})")
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


def reason(e: Exception) -> str:
    return str(e) if isinstance(e, Refused) else f"{type(e).__name__}: {e}"


async def write(ip: str, expected: str, flag: Path, changes: list[dict]) -> dict:
    try:
        device = await connect(ip, expected)
    except Refused as e:
        raise Refused(f"{e}: nothing was written") from None
    held = labels(device)
    missing = [c for c in changes if str(c["channel"]) not in held[c["type"]]]
    if missing:
        raise Refused(f"the card has no {missing[0]['type']} channel {missing[0]['channel']}: nothing was written")
    sent: list[dict] = []
    failed = None
    errors: list[str] = []
    with contextlib.redirect_stdout(sys.stderr):
        for c in changes:
            if flag.exists():
                errors.append(f"{flag} appeared: writing stopped after {len(sent)} of {len(changes)} labels")
                break
            try:
                if c["label"] == str(c["channel"]):
                    await device.reset_channel_name(c["type"], c["channel"])
                else:
                    await device.set_channel_name(c["type"], c["channel"], c["label"])
            except Exception as e:   # reported below, never swallowed: the rest is not written
                failed = c
                errors.append(f"writing {c['type']} {c['channel']} failed ({reason(e)}): "
                              f"writing stopped after {len(sent)} of {len(changes)} labels")
                break
            sent.append(c)
    try:
        after = labels(await connect(ip, expected))
    except Exception as e:   # reported in `error` and `read_back`
        after = None
        errors.append(f"not read back: {reason(e)}")
    mismatches = []
    if after is not None:
        for c in sent + ([failed] if failed else []):
            got = after[c["type"]].get(str(c["channel"]))
            if got != c["label"]:
                mismatches.append({"type": c["type"], "channel": c["channel"], "want": c["label"], "got": got})
    return {"sent": sent, "failed": failed, "error": "; ".join(errors) or None, "read_back": after is not None,
            "mismatches": mismatches, "tx": after["tx"] if after else None, "rx": after["rx"] if after else None}


async def held_name(ip: str) -> str | None:
    """The name the device at `ip` answers with now, None when it is silent."""
    device = open_device(ip)
    with contextlib.redirect_stdout(sys.stderr):
        await device.get_controls()
    return device.name or None


async def rename(ip: str, expected: str, new: str) -> dict:
    try:
        device = await connect(ip, expected)
    except Refused as e:
        raise Refused(f"{e}: nothing was renamed") from None
    errors: list[str] = []
    try:
        with contextlib.redirect_stdout(sys.stderr):
            await device.set_name(new)
    except Exception as e:   # reported below, never swallowed; the name is read back all the same
        errors.append(f"renaming failed ({reason(e)})")
    name = await held_name(ip)
    if name is None:
        errors.append("not read back: the card did not answer")
    elif name != new and not errors:
        errors.append("the card does not hold the new name")
    return {"renamed": name == new, "name": name, "error": "; ".join(errors) or None}


def main(argv: list[str], stdin) -> int:
    try:
        if not argv or (argv[0], len(argv)) not in (("read", 3), ("write", 4), ("rename", 4)):
            raise Usage("usage: dante_card.py read <ip> <expected-name> | write <ip> <expected-name> <flag> | "
                        "rename <ip> <expected-name> <new-name>")
        verb, ip, expected = argv[:3]
        if not unicast_ipv4(ip):
            raise Usage("<ip> is not a unicast IPv4 address")
        if not expected or not expected.isprintable():
            raise Usage("<expected-name> is empty or not printable")
        if verb == "read":
            out = asyncio.run(read(ip, expected))
        elif verb == "rename":
            new = argv[3]
            if not DEVICE_NAME.fullmatch(new) or new == expected:
                raise Usage("<new-name> is not a Dante device name (1 to 31 letters, digits and hyphens, no hyphen "
                            "at either end) other than <expected-name>")
            out = asyncio.run(rename(ip, expected, new))
        else:
            flag = Path(argv[3])
            if not flag.is_absolute():
                raise Usage("<flag> is not an absolute path")
            out = asyncio.run(write(ip, expected, flag, parse_changes(stdin.read())))
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
