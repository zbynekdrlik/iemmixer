"""ARC subscription writes (opcode 0x3010) by unicast: the one Dante command
the Gen 3 tools send themselves (#72 phase E). netaudio 0.4.2 refuses a
self-subscription and a channel name with a space (.claude/rules/gen3.md),
so the packet is built here, the layout phases B-D sent by hand: a 10-byte
header (0x27ff, the length, a sequence, the opcode, 0), then `02 01`, the RX
channel, the offsets of the TX channel's and the TX device's names from the
packet's start (both 0: unsubscribe), then the two names, each NUL-ended.
The reply's last header word is its status: 1 is done.

Pure building and reading, plus `send`, the one socket call (tests replace
it). Names and addresses are never printed by this module."""
from __future__ import annotations

import socket
import struct

ARC_PORT = 4440
OPCODE = 0x3010
HEADER = 10
DONE = 1
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


def status(reply: bytes, seq: int) -> int:
    """The reply's status word, once it is the reply to `seq`'s 0x3010."""
    if len(reply) < HEADER:
        raise ValueError("a reply shorter than its header")
    _, length, got, opcode, word = struct.unpack(">HHHHH", reply[:HEADER])
    if got != seq or opcode != OPCODE or length != len(reply):
        raise ValueError("not the reply to this request")
    return word


def send(ip: str, packet: bytes, timeout: float = 2.0) -> bytes:
    """One request to `ip`'s ARC port and its reply."""
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.settimeout(timeout)
        s.sendto(packet, (ip, ARC_PORT))
        reply, _ = s.recvfrom(2048)
    return reply
