"""ARC by unicast for Gen 3 phase E (#72): a device's RX table read
(0x1002 its name, 0x1000 its counts, 0x3000 its RX pages) and subscription
writes (0x3010). netaudio 0.4.2 refuses a self-subscription and a channel
name with a space (.claude/rules/gen3.md), and it pages RX by whole pages of
16, so it never reads the last 8 of our device's 136; both are done here.

Every request is a 10-byte header (0x27ff, the length, a sequence, the
opcode, a word that is 0 in a request and the status in a reply: 1 done,
0x8112 more to come on a page) and a body. 0x3000's body is `0001`, the
first RX channel wanted, `0000`; a reply page is `[slots, count]` and
`count` 20-byte descriptors (the channel, a word, the shared descriptor's
offset, the TX channel's, the TX device's and the RX channel's name
offsets, two status words), names NUL-ended at offsets from the packet's
start; a page may end early (its size), so the next asks from the channel
after its last. A TX device "." is the device itself; a TX channel offset 0
with a TX device names the RX channel's own name. 0x3010's body is `02 01`,
the RX channel, the offsets of the TX channel's and the TX device's names
(both 0: unsubscribe), then the two names. 0x2010 (phase H, the box's HIL
gate: the name a self-subscription of our own device names) takes the same
page request from a TX channel; our device's page starts at that channel
and lists 6-byte descriptors `id, id, name offset` (arc.rs `tx_names`; a
card lists `index, id, name`, so the id is the second word).

Pure building and reading, plus `send`, the one socket call (tests replace
it). Names and addresses are never printed by this module."""
from __future__ import annotations

import socket
import struct

ARC_PORT = 4440
OPCODE = 0x3010
NAME, COUNTS, RX_PAGE, TX_NAMES = 0x1002, 0x1000, 0x3000, 0x2010
HEADER = 10
DONE = 1
MORE = 0x8112
MAX_RX = 1024
MAX_NAME = 31
# Characters Dante refuses in a channel label (dante_card.FORBIDDEN).
FORBIDDEN = frozenset("=.@")


def _name(text: str, what: str) -> bytes:
    if (not isinstance(text, str) or not 1 <= len(text) <= MAX_NAME or not text.isascii() or not text.isprintable()
            or set(text) & FORBIDDEN):
        raise ValueError(f"not a {what} Dante holds")
    return text.encode("ascii")


def subscription(rx: int, tx_channel: str | None, tx_device: str | None, seq: int) -> bytes:
    """The 0x3010 packet that subscribes our RX `rx` to `tx_channel`@`tx_device`
    on the receiving device, or unsubscribes it when both are None."""
    if type(rx) is not int or not 1 <= rx <= MAX_RX:
        raise ValueError(f"rx channel not 1 to {MAX_RX}")
    if type(seq) is not int or not 0 <= seq <= 0xFFFF:
        raise ValueError("sequence not 0 to 65535")
    if (tx_channel is None) != (tx_device is None):
        raise ValueError("a TX channel and a TX device, or neither")
    if tx_channel is None:
        body = bytes([2, 1]) + struct.pack(">HHH", rx, 0, 0)
    else:
        channel, device = _name(tx_channel, "TX channel name") + b"\0", _name(tx_device, "device name") + b"\0"
        at = HEADER + 2 + 6
        body = bytes([2, 1]) + struct.pack(">HHH", rx, at, at + len(channel)) + channel + device
    return struct.pack(">HHHHH", 0x27FF, HEADER + len(body), seq, OPCODE, 0) + body


def request(opcode: int, body: bytes, seq: int) -> bytes:
    """A request: the header, then `body`."""
    return struct.pack(">HHHHH", 0x27FF, HEADER + len(body), seq, opcode, 0) + body


def status(reply: bytes, seq: int, opcode: int = OPCODE) -> int:
    """The reply's status word, once it is the reply to `seq`'s `opcode`."""
    if len(reply) < HEADER:
        raise ValueError("a reply shorter than its header")
    _, length, got, op, word = struct.unpack(">HHHHH", reply[:HEADER])
    if got != seq or op != opcode or length != len(reply):
        raise ValueError("not the reply to this request")
    return word


def label(reply: bytes, offset: int) -> str | None:
    """The NUL-ended name at `offset`, None for offset 0."""
    if offset == 0:
        return None
    end = reply.find(b"\0", offset)
    if offset >= len(reply) or end < 0:
        raise ValueError("a name outside the reply")
    return reply[offset:end].decode("utf-8", errors="replace")


def rx_page(reply: bytes, own: str) -> list[dict]:
    """A 0x3000 reply page's RX channels: {rx, name, tx_channel, tx_device,
    status}, tx_* None when not subscribed."""
    if len(reply) < HEADER + 2:
        raise ValueError("a page shorter than its count")
    count, out = reply[HEADER + 1], []
    for i in range(count):
        at = HEADER + 2 + 20 * i
        if at + 16 > len(reply):
            raise ValueError("a page shorter than its descriptors")
        n, _, _, c_at, d_at, r_at, _, sub = struct.unpack(">8H", reply[at:at + 16])
        name, device = label(reply, r_at), label(reply, d_at)
        channel = (label(reply, c_at) or name) if device else None
        out.append({"rx": n, "name": name, "tx_channel": channel, "tx_device": own if device == "." else device,
                    "status": sub})
    return out


def read_rx(ip: str, seq: int = 0x4600) -> dict:
    """The device at `ip`: {"name", "rx_count", "rx": {"<n>": {tx_channel,
    tx_device, status}}}, its subscribed RX channels only, every page read."""
    def ask(opcode: int, body: bytes) -> bytes:
        nonlocal seq
        seq = (seq + 1) & 0xFFFF
        reply = send(ip, request(opcode, body, seq))
        if status(reply, seq, opcode) not in (DONE, MORE):
            raise ValueError(f"opcode {opcode:#06x} refused")
        return reply

    reply = ask(NAME, b"")
    own = reply[HEADER:].split(b"\0", 1)[0].decode("utf-8", errors="replace")
    reply = ask(COUNTS, b"")
    if len(reply) < HEADER + 6:
        raise ValueError("a counts reply shorter than its counts")
    rx_count = struct.unpack(">H", reply[HEADER + 4:HEADER + 6])[0]
    rx, first = {}, 1
    while first <= rx_count:
        page = rx_page(ask(RX_PAGE, struct.pack(">HHH", 1, first, 0)), own)
        if not page or page[0]["rx"] != first or any(b["rx"] != a["rx"] + 1 for a, b in zip(page, page[1:])):
            raise ValueError(f"the page from rx {first} is not that page")
        for ch in page:
            if ch["tx_device"] is not None:
                rx[str(ch["rx"])] = {k: ch[k] for k in ("tx_channel", "tx_device", "status")}
        first = page[-1]["rx"] + 1
    return {"name": own, "rx_count": rx_count, "rx": rx}


def tx_name_of(reply: bytes, tx: int) -> str:
    """TX channel `tx`'s current name from a 0x2010 reply page (pure)."""
    if len(reply) < HEADER + 2:
        raise ValueError("a page shorter than its count")
    for i in range(reply[HEADER + 1]):
        at = HEADER + 2 + 6 * i
        if at + 6 > len(reply):
            raise ValueError("a page shorter than its descriptors")
        _, ident, name_at = struct.unpack(">3H", reply[at:at + 6])
        if ident == tx:
            name = label(reply, name_at)
            if not name:
                raise ValueError("a TX channel without a name")
            return name
    raise ValueError(f"the page from tx {tx} does not list it")


def tx_name(ip: str, tx: int, seq: int = 0x4700) -> str:
    """Our device's TX channel `tx`'s current name, by `ip` (0x2010 from
    that channel)."""
    if type(tx) is not int or not 1 <= tx <= MAX_RX:
        raise ValueError(f"tx channel not 1 to {MAX_RX}")
    seq = (seq + 1) & 0xFFFF
    reply = send(ip, request(TX_NAMES, struct.pack(">HHH", 1, tx, 0), seq))
    if status(reply, seq, TX_NAMES) not in (DONE, MORE):
        raise ValueError(f"opcode {TX_NAMES:#06x} refused")
    return tx_name_of(reply, tx)


def send(ip: str, packet: bytes, timeout: float = 2.0) -> bytes:
    """One request to `ip`'s ARC port and its reply."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(timeout)
        s.sendto(packet, (ip, ARC_PORT))
        reply, _ = s.recvfrom(2048)
    return reply
