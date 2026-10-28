"""Tests for scripts/iem-pc/gen3_arc.py, Gen 3 phase E's ARC reads and
subscription writes (#72). The 0x3010 layout is the one phases B-D sent by
hand to the IEM card and to our own device, which both answered status 1;
the reads were checked live against both on 2026-10-27 (the card's 31
subscriptions as netaudio reads them, our device's 136 RX). `FakeDante`
answers as our device's arc.rs does (test_iempc_gen3_switch uses it too).
Every value is synthetic (P6: invented names, a loopback socket)."""
from __future__ import annotations

import re
import socket
import struct
import sys
import threading
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import gen3_arc  # noqa: E402


class FakeDante:
    """A Dante device on the ARC port: its name, counts and RX pages (up to
    `page` a page, as arc.rs's page may end early), and 0x3010 applied to its
    table (`refuse`: RX channels answered 0x22). `rows`: {rx: (tx channel,
    tx device, status)}; a TX device equal to its own name is sent as ".",
    a TX channel equal to the RX name as offset 0."""

    def __init__(self, name: str, rx_count: int, rows: dict, page: int = 16) -> None:
        self.name, self.rx_count, self.rows, self.page = name, rx_count, dict(rows), page
        self.refuse: set[int] = set()
        self.packets: list[bytes] = []
        self.after = None
        self.wrong_page = False
        # Phase H: its TX channels' current names (0x2010, arc.rs `tx_names`),
        # `tx_names` for those renamed.
        self.tx_count = 96
        self.tx_names: dict[int, str] = {}

    def rx_name(self, n: int) -> str:
        return f"{n:03d} zyxqw in"

    def tx_name(self, n: int) -> str:
        return self.tx_names.get(n, f"{n:03d} zyxqw out")

    def handle(self, packet: bytes) -> bytes:
        self.packets.append(packet)
        _, _, seq, opcode, _ = struct.unpack(">HHHHH", packet[:10])
        word, body = gen3_arc.DONE, b""
        if opcode == gen3_arc.NAME:
            body = self.name.encode() + b"\0"
        elif opcode == gen3_arc.COUNTS:
            body = struct.pack(">BBHHHH", 0, 0, 4, self.rx_count, 4, 8)
        elif opcode == gen3_arc.RX_PAGE:
            first = struct.unpack(">H", packet[12:14])[0] + (1 if self.wrong_page else 0)
            last = min(first + self.page - 1, self.rx_count)
            table, names = bytearray(2 + 16 * 20), bytearray()
            base = 10 + len(table)

            def put(text: str) -> int:
                at = base + len(names)
                names.extend(text.encode() + b"\0")
                return at
            for i, n in enumerate(range(first, last + 1)):
                r_at = put(self.rx_name(n))
                c_at = d_at = sub = 0
                if n in self.rows:
                    channel, device, sub = self.rows[n]
                    c_at = 0 if channel == self.rx_name(n) else put(channel)
                    d_at = put("." if device == self.name else device)
                table[2 + 20 * i:2 + 20 * i + 16] = struct.pack(">8H", n, 6, 0, c_at, d_at, r_at, 0, sub)
            table[0], table[1] = 16, last - first + 1
            body = bytes(table) + bytes(names)
            word = gen3_arc.MORE if last < self.rx_count else gen3_arc.DONE
        elif opcode == gen3_arc.TX_NAMES:
            # arc.rs `page`: up to 32 slots of `id, id, name` from the first
            # item asked, the first item's u32 0 before the names.
            first = struct.unpack(">H", packet[12:14])[0]
            last = min(first + 31, self.tx_count)
            slots = min(32, self.tx_count)
            table, names = bytearray(2 + 6 * slots), bytearray(4)
            base = 10 + len(table)
            for i, n in enumerate(range(first, last + 1)):
                at = base + len(names)
                names.extend(self.tx_name(n).encode() + b"\0")
                table[2 + 6 * i:8 + 6 * i] = struct.pack(">3H", n, n, at)
            table[0], table[1] = slots, max(0, last - first + 1)
            body = bytes(table) + bytes(names)
            word = gen3_arc.MORE if last < self.tx_count else gen3_arc.DONE
        elif opcode == gen3_arc.OPCODE:
            rx, c_at, d_at = struct.unpack(">HHH", packet[12:18])
            if rx in self.refuse:
                word = 0x22
            elif c_at == 0:
                self.rows.pop(rx, None)
            else:
                self.rows[rx] = (packet[c_at:packet.index(b"\0", c_at)].decode(),
                                 packet[d_at:packet.index(b"\0", d_at)].decode(), 9)
        if self.after:
            self.after()
        return struct.pack(">HHHHH", 0x27FF, 10 + len(body), seq, opcode, word) + body


class Network:
    """gen3_arc.send for a few FakeDante devices by host."""

    def __init__(self, **devices: FakeDante) -> None:
        self.devices = {host.replace("_", "."): d for host, d in devices.items()}
        self.hosts: list[str] = []

    def send(self, host: str, packet: bytes, timeout: float = 2.0) -> bytes:
        self.hosts.append(host)
        if host not in self.devices:
            raise TimeoutError("timed out")
        return self.devices[host].handle(packet)


class ReadTests(unittest.TestCase):
    def read(self, device: FakeDante) -> dict:
        with mock.patch.object(gen3_arc, "send", Network(**{"192_0_2_30": device}).send):
            return gen3_arc.read_rx("192.0.2.30")

    def test_every_page_is_read_also_a_last_partial_one_and_pages_that_end_early(self) -> None:
        rows = {1: ("01 mic", "zyxqw-stagebox", 9), 130: ("talk", "zyxqw-foh", 1), 136: ("x", "zyxqw-foh", 9)}
        for page in (16, 5):
            with self.subTest(page=page):
                device = FakeDante("zyxqw-gen3", 136, rows, page=page)
                got = self.read(device)
                self.assertEqual(got, {"name": "zyxqw-gen3", "rx_count": 136, "rx": {
                    "1": {"tx_channel": "01 mic", "tx_device": "zyxqw-stagebox", "status": 9},
                    "130": {"tx_channel": "talk", "tx_device": "zyxqw-foh", "status": 1},
                    "136": {"tx_channel": "x", "tx_device": "zyxqw-foh", "status": 9}}})
                pages = sum(1 for p in device.packets if struct.unpack(">H", p[6:8])[0] == gen3_arc.RX_PAGE)
                self.assertEqual(pages, -(-136 // page))

    def test_the_device_itself_and_a_channel_of_the_rx_name_are_named_in_full(self) -> None:
        device = FakeDante("Zyxqw-Iem-Card", 8, {})
        device.rows = {7: ("7", "Zyxqw-Iem-Card", 4), 3: (device.rx_name(3), "zyxqw-foh", 9)}
        got = self.read(device)["rx"]
        self.assertEqual(got["7"], {"tx_channel": "7", "tx_device": "Zyxqw-Iem-Card", "status": 4})
        self.assertEqual(got["3"]["tx_channel"], device.rx_name(3))
        # On the wire the device itself is ".": the read names it.
        self.assertIn(b".\0", device.handle(gen3_arc.request(gen3_arc.RX_PAGE, struct.pack(">HHH", 1, 1, 0), 1)))

    def test_a_page_that_is_not_the_one_asked_or_a_refused_request_is_an_error(self) -> None:
        device = FakeDante("zyxqw-gen3", 20, {})
        device.wrong_page = True
        with self.assertRaises(ValueError):
            self.read(device)
        device = FakeDante("zyxqw-gen3", 20, {})
        original = device.handle
        device.handle = lambda packet: original(packet)[:8] + struct.pack(">H", 0x22) + original(packet)[10:]
        with self.assertRaises(ValueError):
            self.read(device)

    def test_labels_outside_the_reply_are_an_error(self) -> None:
        self.assertIsNone(gen3_arc.label(b"abc", 0))
        self.assertEqual(gen3_arc.label(b"0123456789ab\0", 10), "ab")
        for offset in (13, 99):
            with self.assertRaises(ValueError):
                gen3_arc.label(b"0123456789ab\0", offset)
        with self.assertRaises(ValueError):
            gen3_arc.label(b"0123456789ab", 10)


class TxNameTests(unittest.TestCase):
    """Phase H: a TX channel's current name, the name the box's HIL gate
    self-subscribes our spare return to (0x2010, arc.rs `tx_names`)."""

    def name(self, device: FakeDante, tx: int) -> str:
        with mock.patch.object(gen3_arc, "send", Network(**{"192_0_2_30": device}).send):
            return gen3_arc.tx_name("192.0.2.30", tx)

    def test_the_page_from_the_channel_names_it_also_renamed_and_the_last(self) -> None:
        device = FakeDante("zyxqw-gen3", 136, {})
        device.tx_names = {94: "Zyxqw Spare 1"}
        self.assertEqual(self.name(device, 94), "Zyxqw Spare 1")
        self.assertEqual(self.name(device, 95), "095 zyxqw out")
        self.assertEqual(self.name(device, 96), "096 zyxqw out")
        asked = [struct.unpack(">HH", p[6:8] + p[12:14]) for p in device.packets]
        self.assertEqual(asked, [(gen3_arc.TX_NAMES, 94), (gen3_arc.TX_NAMES, 95), (gen3_arc.TX_NAMES, 96)])

    def test_a_channel_the_page_does_not_list_or_a_refusal_is_an_error(self) -> None:
        device = FakeDante("zyxqw-gen3", 136, {})
        with self.assertRaises(ValueError):
            self.name(device, 97)   # past its 96 TX channels: an empty page
        for bad in (0, 1025, True):
            with self.assertRaises(ValueError):
                self.name(device, bad)
        original = device.handle
        device.handle = lambda packet: original(packet)[:8] + struct.pack(">H", 0x22) + original(packet)[10:]
        with self.assertRaises(ValueError):
            self.name(device, 94)

    def test_a_card_s_index_id_name_page_is_read_by_its_id(self) -> None:
        # A card lists `index, id, name` of its named channels only: the id is the second word.
        names = b"zyxqw b\0"
        table = struct.pack(">BB", 2, 2) + struct.pack(">3H", 1, 7, 0) + struct.pack(">3H", 2, 9, 10 + 14)
        reply = struct.pack(">HHHHH", 0x27FF, 10 + len(table) + len(names), 1, gen3_arc.TX_NAMES, 1) + table + names
        self.assertEqual(gen3_arc.tx_name_of(reply, 9), "zyxqw b")
        with self.assertRaises(ValueError):
            gen3_arc.tx_name_of(reply, 7)   # its name offset is 0: no name
        with self.assertRaises(ValueError):
            gen3_arc.tx_name_of(reply[:20], 9)


class PacketTests(unittest.TestCase):
    def test_a_subscription_names_its_tx_channel_and_device_by_offset(self) -> None:
        got = gen3_arc.subscription(102, "ZYXQW 1", "zyxqw-card", 0x4242)
        want = (bytes.fromhex("27ff 0025 4242 3010 0000") + bytes.fromhex("0201 0066 0012 001a")
                + b"ZYXQW 1\0zyxqw-card\0")
        self.assertEqual(got, want)

    def test_an_unsubscribe_has_no_names_and_zero_offsets(self) -> None:
        self.assertEqual(gen3_arc.subscription(7, None, None, 1),
                         bytes.fromhex("27ff 0012 0001 3010 0000 0201 0007 0000 0000"))

    def test_what_dante_does_not_hold_is_refused(self) -> None:
        bad = [(0, "a", "b", 1), (1025, "a", "b", 1), (True, "a", "b", 1), (1, "a", "b", -1), (1, "a", "b", 65536),
               (1, "a", None, 1), (1, None, "b", 1), (1, "", "b", 1), (1, "a.b", "b", 1), (1, "a", "b@c", 1),
               (1, "a=b", "b", 1), (1, "X" * 32, "b", 1), (1, "Zyx" + chr(0x17D), "b", 1), (1, "a\tb", "b", 1)]
        for args in bad:
            with self.assertRaises(ValueError, msg=repr(args)):
                gen3_arc.subscription(*args)
        self.assertEqual(len(gen3_arc.subscription(1024, "X" * 31, "Y" * 31, 65535)), 10 + 8 + 32 + 32)

    def test_the_reply_status_is_read_only_from_the_reply_to_this_request(self) -> None:
        def reply(seq: int, opcode: int, word: int, extra: bytes = b"") -> bytes:
            return struct.pack(">HHHHH", 0x27FF, 10 + len(extra), seq, opcode, word) + extra

        self.assertEqual(gen3_arc.status(reply(9, 0x3010, 1), 9), gen3_arc.DONE)
        self.assertEqual(gen3_arc.status(reply(9, 0x3010, 0x22, b"\0\0"), 9), 0x22)
        for wrong in (reply(8, 0x3010, 1), reply(9, 0x3000, 1), reply(9, 0x3010, 1)[:9],
                      reply(9, 0x3010, 1) + b"\0"):
            with self.assertRaises(ValueError):
                gen3_arc.status(wrong, 9)


class SendTests(unittest.TestCase):
    def test_send_reaches_the_arc_port_and_returns_the_reply(self) -> None:
        server = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.addCleanup(server.close)
        server.bind(("127.0.0.1", 0))
        seen = []

        def answer() -> None:
            data, peer = server.recvfrom(2048)
            seen.append(data)
            seq = struct.unpack(">H", data[4:6])[0]
            server.sendto(struct.pack(">HHHHH", 0x27FF, 10, seq, 0x3010, 1), peer)

        t = threading.Thread(target=answer)
        t.start()
        packet = gen3_arc.subscription(3, None, None, 77)
        with mock.patch.object(gen3_arc, "ARC_PORT", server.getsockname()[1]):
            got = gen3_arc.send("127.0.0.1", packet, timeout=2.0)
        t.join(5)
        self.assertEqual(seen, [packet])
        self.assertEqual(gen3_arc.status(got, 77), gen3_arc.DONE)

    def test_no_reply_is_a_timeout(self) -> None:
        server = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.addCleanup(server.close)
        server.bind(("127.0.0.1", 0))
        with mock.patch.object(gen3_arc, "ARC_PORT", server.getsockname()[1]):
            with self.assertRaises(TimeoutError):
                gen3_arc.send("127.0.0.1", gen3_arc.subscription(3, None, None, 1), timeout=0.2)


class SourceTests(unittest.TestCase):
    SOURCE = (HERE / "gen3_arc.py").read_text(encoding="utf-8")

    def test_the_module_prints_nothing_and_sends_only_from_send(self) -> None:
        self.assertIsNone(re.search(r"\bprint\(", self.SOURCE))
        self.assertEqual(len(re.findall(r"\.sendto\(", self.SOURCE)), 1)
        self.assertIn(".sendto(", self.SOURCE.split("def send(", 1)[1])


if __name__ == "__main__":
    unittest.main()
