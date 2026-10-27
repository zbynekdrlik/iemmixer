"""Tests for scripts/iem-pc/iempc_gen3_switch.py, Gen 3 phase E's RX mirror
(#72; plan docs/superpowers/plans/2026-10-27-gen3-phase-e-band-test.md Task
4). No card and no device exist in CI: dante_card.py's subprocess is a fake
that answers `subscriptions` from two tables, and gen3_arc.send a fake
device that applies each 0x3010 to ours. Every value is synthetic (P6:
documentation IPs, invented names)."""
from __future__ import annotations

import json
import struct
import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import gen3_arc  # noqa: E402
import iempc_dante as dante  # noqa: E402
import iempc_gen3_switch as switch  # noqa: E402
from iempc_test_support import Base, ip  # noqa: E402

CARD_HOST, CARD = "192.0.2.10", "Zyxqw-Iem-Card"
OURS_HOST, OURS = "192.0.2.20", "zyxqw-gen3"
STAGEBOX, FOH = "zyxqw-stagebox", "zyxqw-foh"


def doc(count: int, rows: dict[int, tuple[str, str]]) -> dict:
    return {"name": "x", "rx_count": count,
            "rx": {str(n): {"tx_channel": c, "tx_device": d, "status": 9} for n, (c, d) in sorted(rows.items())}}


class PlanTests(unittest.TestCase):
    def test_our_rx_follows_the_cards_except_its_own_returns(self) -> None:
        card = doc(8, {1: ("01 mic", STAGEBOX), 2: ("talk", FOH), 7: ("7", CARD)})
        ours = doc(10, {2: ("old", FOH), 9: ("MIX 96", OURS)})
        self.assertEqual(switch.mirror_plan(card, ours, CARD), [
            {"rx": 1, "tx_channel": "01 mic", "tx_device": STAGEBOX},
            {"rx": 2, "tx_channel": "talk", "tx_device": FOH},
            {"rx": 9, "tx_channel": None, "tx_device": None}])

    def test_an_equal_table_needs_no_change_and_clear_unsubscribes_ours(self) -> None:
        card = doc(8, {1: ("01 mic", STAGEBOX)})
        self.assertEqual(switch.mirror_plan(card, doc(10, {1: ("01 mic", STAGEBOX)}), CARD), [])
        self.assertEqual(switch.mirror_plan(None, doc(10, {1: ("01 mic", STAGEBOX), 4: ("x", FOH)}), CARD, clear=True),
                         [{"rx": 1, "tx_channel": None, "tx_device": None},
                          {"rx": 4, "tx_channel": None, "tx_device": None}])

    def test_a_card_rx_our_device_lacks_or_a_table_of_another_shape_is_refused(self) -> None:
        with self.assertRaises(switch.PlanError) as e:
            switch.mirror_plan(doc(128, {120: ("x", STAGEBOX)}), doc(96, {}), CARD)
        self.assertIn("rx 120", str(e.exception))
        # A card RX beyond ours that is not subscribed, or only to the card itself, is fine.
        self.assertEqual(switch.mirror_plan(doc(128, {120: ("120", CARD)}), doc(96, {}), CARD), [])
        bad = [None, {}, {"rx_count": 0, "rx": {}}, {"rx_count": 4, "rx": []}, doc(4, {5: ("x", FOH)}),
               {"rx_count": 4, "rx": {"x": {"tx_channel": "a", "tx_device": "b"}}},
               {"rx_count": 4, "rx": {"1": {"tx_channel": 1, "tx_device": "b"}}}]
        for d in bad:
            with self.subTest(d=d), self.assertRaises(switch.PlanError):
                switch.mirror_plan(doc(4, {}), d, CARD)
            with self.subTest(card=d), self.assertRaises(switch.PlanError):
                switch.mirror_plan(d, doc(4, {}), CARD)


class Device:
    """Our device as the 0x3010s reach it: each packet applied to its table,
    answered with a status (1, or `refuse` for the RX channels named there)."""

    def __init__(self, test: "MirrorCommandTests", rows: dict[int, tuple[str, str]]) -> None:
        self.test, self.rows, self.packets, self.refuse = test, dict(rows), [], set()
        self.after = None

    def send(self, host: str, packet: bytes, timeout: float = 2.0) -> bytes:
        self.test.assertEqual(host, OURS_HOST)   # only our own device is ever written
        self.packets.append(packet)
        _, _, seq, opcode, _ = struct.unpack(">HHHHH", packet[:10])
        rx, c_at, d_at = struct.unpack(">HHH", packet[12:18])
        word = 0x22 if rx in self.refuse else 1
        if word == 1:
            if c_at == 0:
                self.rows.pop(rx, None)
            else:
                self.rows[rx] = (packet[c_at:packet.index(b"\0", c_at)].decode(),
                                 packet[d_at:packet.index(b"\0", d_at)].decode())
        if self.after:
            self.after()
        return struct.pack(">HHHHH", 0x27FF, 10, seq, opcode, word)


class MirrorCommandTests(Base):
    def setUp(self) -> None:
        super().setUp()
        python = self.tmp / "netaudio" / "bin" / "python"
        python.parent.mkdir(parents=True)
        python.write_text("", encoding="utf-8")
        python.chmod(0o755)
        self.env_text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(self.env_text + f"DANTE_IEM_HOST={CARD_HOST}\nDANTE_IEM_NAME={CARD}\n"
                                 f"NETAUDIO_PYTHON={python}\nGEN3_DANTE_IP={OURS_HOST}\nGEN3_DANTE_NAME={OURS}\n",
                                 encoding="utf-8")
        self.card = {1: ("01 mic", STAGEBOX), 2: ("talk", FOH), 125: ("125", CARD)}
        self.card_count = 128
        self.device = Device(self, {3: ("MIX 96", OURS)})
        self.asked: list[tuple[str, str]] = []
        for p in (mock.patch.object(dante, "helper", self.helper), mock.patch.object(gen3_arc, "send", self.device.send)):
            p.start()
            self.addCleanup(p.stop)

    def raise_flag(self) -> None:
        ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
        ip.EVENT_NOW.write_text("ide event\n", encoding="utf-8")

    def helper(self, ip_mod, cmd: list[str], stdin: str) -> str:
        verb, host, name = cmd[3], cmd[4], cmd[5]
        self.asked.append((verb, host))
        self.assertEqual(verb, "subscriptions")
        if host == CARD_HOST:
            self.assertEqual(name, CARD)
            return json.dumps(doc(self.card_count, self.card)) + "\n"
        self.assertEqual((host, name), (OURS_HOST, OURS))
        return json.dumps(doc(136, self.device.rows)) + "\n"

    def test_the_mirror_makes_our_table_the_cards_and_reads_it_back(self) -> None:
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.device.rows, {1: ("01 mic", STAGEBOX), 2: ("talk", FOH)})
        self.assertEqual(docs, [{"gen3_mirror": {"sent": 3, "of": 3, "left": [], "error": None}}])
        self.assertEqual(self.asked, [("subscriptions", CARD_HOST), ("subscriptions", OURS_HOST),
                                      ("subscriptions", OURS_HOST)])
        self.assertEqual((self.pc.calls, self.pc.scps), ([], []))   # no PC call
        # Run again: nothing differs, nothing is sent.
        self.device.packets.clear()
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual((code, docs[0]["gen3_mirror"]["of"], self.device.packets), (0, 0, []), err)

    def test_a_dry_run_lists_the_changes_and_sends_nothing(self) -> None:
        code, docs, err = self.run_main("gen3-mirror", "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[0]["gen3_mirror"]["changes"][0], {"rx": 1, "tx_channel": "01 mic", "tx_device": STAGEBOX})
        self.assertEqual(self.device.packets, [])

    def test_clear_unsubscribes_every_rx_of_ours_without_reading_the_card(self) -> None:
        self.device.rows[1] = ("01 mic", STAGEBOX)
        code, docs, err = self.run_main("gen3-mirror", "--clear")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.device.rows, {})
        self.assertNotIn(("subscriptions", CARD_HOST), self.asked)

    def test_a_refused_change_stops_the_rest_and_fails(self) -> None:
        self.device.refuse.add(2)
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual(code, 1)
        report = docs[0]["gen3_mirror"]
        self.assertEqual((report["sent"], report["of"], len(report["left"])), (1, 3, 2))
        self.assertIn("rx 2: status 0x0022", err)
        self.assertEqual(self.device.rows, {1: ("01 mic", STAGEBOX), 3: ("MIX 96", OURS)})   # rx 3 never sent

    def test_the_flag_stops_the_rest(self) -> None:
        self.device.after = lambda: self.raise_flag()
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual(code, 1)
        self.assertEqual(len(self.device.packets), 1)
        self.assertIn("the flag appeared: 1 of 3 changes sent", docs[0]["gen3_mirror"]["error"])

    def test_the_command_is_dev_time_locked_and_refused_while_event_now(self) -> None:
        spec = ip.COMMANDS["gen3-mirror"]
        self.assertEqual((spec.fn, spec.pc, spec.dev_time, spec.locked), (ip.cmd_gen3_mirror, False, True, True))
        self.raise_flag()
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual((code, docs, self.asked), (1, [], []))
        self.assertIn("runs only in dev time", err)

    def test_missing_keys_send_nothing(self) -> None:
        for drop in ("GEN3_DANTE_IP", "GEN3_DANTE_NAME", "DANTE_IEM_HOST"):
            text = ip.env_path().read_text(encoding="utf-8")
            ip.env_path().write_text("".join(line + "\n" for line in text.splitlines() if not line.startswith(drop)),
                                     encoding="utf-8")
            code, docs, err = self.run_main("gen3-mirror")
            self.assertEqual((code, docs), (1, []), drop)
            self.assertIn(drop, err)
        self.assertEqual(self.device.packets, [])

    def test_a_card_rx_ours_lacks_sends_nothing(self) -> None:
        self.card_count, self.card[137] = 140, ("x", STAGEBOX)
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("the card's rx 137 is subscribed and our device has 136 rx channels: nothing was sent", err)
        self.assertEqual(self.device.packets, [])

if __name__ == "__main__":
    unittest.main()
