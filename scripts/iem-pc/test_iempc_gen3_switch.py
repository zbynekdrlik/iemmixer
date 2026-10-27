"""Tests for scripts/iem-pc/iempc_gen3_switch.py, Gen 3 phase E's RX mirror
(#72; plan docs/superpowers/plans/2026-10-27-gen3-phase-e-band-test.md Task
4). No card and no device exist in CI: gen3_arc.send reaches two
FakeDante devices (test_gen3_arc), which answer as our device's arc.rs does
and apply each 0x3010. Every value is synthetic (P6: documentation IPs,
invented names)."""
from __future__ import annotations

import struct
import sys
import tomllib
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import gen3_arc  # noqa: E402
import iempc_gen3_switch as switch  # noqa: E402
from iempc_test_support import Base, ip  # noqa: E402
from test_gen3_arc import FakeDante, Network  # noqa: E402

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

    def test_the_cards_own_returns_are_not_mirrored_under_its_usual_name_either(self) -> None:
        # Live, 2026-10-27: the card keeps its self-subscriptions under the
        # name it had (not "."), so with the card renamed to its spare name
        # they read as subscriptions to the card's usual name, which our
        # device then holds.
        card = doc(128, {1: ("01 mic", STAGEBOX), 125: ("125", CARD)})
        self.assertEqual(switch.mirror_plan(card, doc(136, {}), "Zyxqw-Spare", also=(CARD,)),
                         [{"rx": 1, "tx_channel": "01 mic", "tx_device": STAGEBOX}])

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


class TakeoverTests(unittest.TestCase):
    """Phase E Task 3: our device config under the card's name (#72)."""
    TEST = (HERE.parent.parent / "config" / "test-dante-device.toml").read_text(encoding="utf-8")

    def labels(self, count: int) -> dict[int, str]:
        return {n: f"ZYXQW inear L {n:02d}" for n in range(1, count + 1)}

    def test_name_state_file_and_tx_names_change_and_nothing_else(self) -> None:
        base = tomllib.loads(self.TEST)
        got = switch.takeover_config(self.TEST, self.labels(200), CARD)
        new = tomllib.loads(got)
        self.assertEqual((new["name"], new["state_file"]), (CARD, switch.TAKEOVER_STATE))
        self.assertEqual(new["tx"], [f"ZYXQW inear L {n:02d}" for n in range(1, len(base["tx"]) + 1)])
        self.assertEqual({k: v for k, v in new.items() if k not in ("name", "state_file", "tx")},
                         {k: v for k, v in base.items() if k not in ("name", "state_file", "tx")})
        # The lines outside the three entries are the same lines.
        kept = [line for line in self.TEST.splitlines() if not line.startswith(("name = ", "state_file = "))]
        rx_and_rest = [line for line in kept if '"' not in line or not line.startswith("  ")]
        for line in rx_and_rest:
            self.assertIn(line, got.splitlines())

    def test_labels_that_do_not_cover_our_tx_or_repeat_are_refused(self) -> None:
        count = len(tomllib.loads(self.TEST)["tx"])
        labels = self.labels(count)
        for bad in ({**{n: v for n, v in labels.items() if n != count}},
                    {**labels, 2: labels[1]}, {**labels, 3: "a.b"}, {**labels, 4: ""}):
            with self.assertRaises(switch.TakeoverError):
                switch.takeover_config(self.TEST, bad, CARD)
        with self.assertRaises(switch.TakeoverError):
            switch.takeover_config(self.TEST, labels, "not a name")
        with self.assertRaises(switch.TakeoverError):
            switch.takeover_config(self.TEST.replace("\ntx = [", "\ntx2 = ["), labels, CARD)


class MirrorCommandTests(Base):
    def setUp(self) -> None:
        super().setUp()
        self.env_text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(self.env_text + f"DANTE_IEM_HOST={CARD_HOST}\nDANTE_IEM_NAME={CARD}\n"
                                 f"GEN3_DANTE_IP={OURS_HOST}\nGEN3_DANTE_NAME={OURS}\n", encoding="utf-8")
        self.card = FakeDante(CARD, 128, {1: ("01 mic", STAGEBOX, 9), 2: ("talk", FOH, 9), 125: ("125", CARD, 4)})
        self.device = FakeDante(OURS, 136, {3: ("MIX 96", OURS, 9), 135: ("125", CARD, 9)})
        self.net = Network(**{CARD_HOST.replace(".", "_"): self.card, OURS_HOST.replace(".", "_"): self.device})
        patcher = mock.patch.object(gen3_arc, "send", self.net.send)
        patcher.start()
        self.addCleanup(patcher.stop)

    def writes(self) -> list[tuple[str, bytes]]:
        """Every 0x3010 sent, by host."""
        out = []
        for host, device in self.net.devices.items():
            out += [(host, p) for p in device.packets if struct.unpack(">H", p[6:8])[0] == gen3_arc.OPCODE]
        return out

    def raise_flag(self) -> None:
        ip.EVENT_NOW.parent.mkdir(parents=True, exist_ok=True)
        ip.EVENT_NOW.write_text("ide event\n", encoding="utf-8")

    def test_the_mirror_makes_our_table_the_cards_and_reads_it_back(self) -> None:
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.device.rows, {1: ("01 mic", STAGEBOX, 9), 2: ("talk", FOH, 9)})
        self.assertEqual(docs, [{"gen3_mirror": {"sent": 4, "of": 4, "left": [], "error": None}}])
        self.assertEqual({h for h, _ in self.writes()}, {OURS_HOST})   # the card is only read
        self.assertEqual((self.pc.calls, self.pc.scps), ([], []))   # no PC call
        # Run again: nothing differs, nothing is sent.
        before = len(self.writes())
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual((code, docs[0]["gen3_mirror"]["of"], len(self.writes())), (0, 0, before), err)

    def test_a_dry_run_lists_the_changes_and_sends_nothing(self) -> None:
        code, docs, err = self.run_main("gen3-mirror", "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[0]["gen3_mirror"]["changes"], [
            {"rx": 1, "tx_channel": "01 mic", "tx_device": STAGEBOX},
            {"rx": 2, "tx_channel": "talk", "tx_device": FOH},
            {"rx": 3, "tx_channel": None, "tx_device": None},
            {"rx": 135, "tx_channel": None, "tx_device": None}])
        self.assertEqual(self.writes(), [])

    def test_clear_unsubscribes_every_rx_of_ours_without_reading_the_card(self) -> None:
        code, docs, err = self.run_main("gen3-mirror", "--clear")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.device.rows, {})
        self.assertNotIn(CARD_HOST, self.net.hosts)

    def test_a_refused_change_stops_the_rest_and_fails(self) -> None:
        self.device.refuse.add(2)
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual(code, 1)
        report = docs[0]["gen3_mirror"]
        self.assertEqual((report["sent"], report["of"], len(report["left"])), (1, 4, 3))
        self.assertIn("rx 2: status 0x0022", err)
        self.assertEqual(set(self.device.rows), {1, 3, 135})   # rx 3 and 135 never sent

    def test_the_flag_stops_the_rest(self) -> None:
        self.device.after = lambda: self.raise_flag() if self.writes() else None
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual(code, 1)
        self.assertEqual(len(self.writes()), 1)
        self.assertIn("the flag appeared: 1 of 4 changes sent", docs[0]["gen3_mirror"]["error"])

    def test_the_command_is_dev_time_locked_and_refused_while_event_now(self) -> None:
        spec = ip.COMMANDS["gen3-mirror"]
        self.assertEqual((spec.fn, spec.pc, spec.dev_time, spec.locked), (ip.cmd_gen3_mirror, False, True, True))
        self.raise_flag()
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual((code, docs, self.net.hosts), (1, [], []))
        self.assertIn("runs only in dev time", err)

    def test_missing_or_bad_keys_send_nothing(self) -> None:
        for drop in ("GEN3_DANTE_IP", "GEN3_DANTE_NAME", "DANTE_IEM_HOST", "DANTE_IEM_NAME"):
            ip.env_path().write_text("".join(line + "\n" for line in (self.env_text + f"DANTE_IEM_HOST={CARD_HOST}\n"
                                     f"DANTE_IEM_NAME={CARD}\nGEN3_DANTE_IP={OURS_HOST}\nGEN3_DANTE_NAME={OURS}\n")
                                     .splitlines() if not line.startswith(drop)), encoding="utf-8")
            code, docs, err = self.run_main("gen3-mirror")
            self.assertEqual((code, docs), (1, []), drop)
            self.assertIn(drop, err)
        ip.env_path().write_text(self.env_text + f"DANTE_IEM_HOST={CARD_HOST}\nDANTE_IEM_NAME={CARD}\n"
                                 f"GEN3_DANTE_IP=zyxqw.example\nGEN3_DANTE_NAME={OURS}\n", encoding="utf-8")
        code, docs, err = self.run_main("gen3-mirror")
        self.assertIn("GEN3_DANTE_IP is not a unicast IPv4 address", err)
        self.assertEqual(self.net.hosts, [])

    def test_another_device_or_no_answer_sends_nothing(self) -> None:
        self.device.name = "zyxqw-other"
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("the device answering as our device is not the expected one", err)
        self.assertNotIn("zyxqw-other", err)
        del self.net.devices[OURS_HOST]
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("our device could not be read (TimeoutError)", err)
        self.assertEqual(self.writes(), [])

    def test_a_card_rx_ours_lacks_sends_nothing(self) -> None:
        self.card.rx_count, self.card.rows[137] = 140, ("x", STAGEBOX, 9)
        code, docs, err = self.run_main("gen3-mirror")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("the card's rx 137 is subscribed and our device has 136 rx channels: nothing was sent", err)
        self.assertEqual(self.writes(), [])

if __name__ == "__main__":
    unittest.main()
