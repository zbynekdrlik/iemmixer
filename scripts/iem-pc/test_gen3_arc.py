"""Tests for scripts/iem-pc/gen3_arc.py, the ARC 0x3010 subscription writes
of Gen 3 phase E (#72). The packet layout is the one phases B-D sent by hand
to the IEM card and to our own device, which both answered status 1. Every
value is synthetic (P6: invented names, a loopback socket)."""
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
