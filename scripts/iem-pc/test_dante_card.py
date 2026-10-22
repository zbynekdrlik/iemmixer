"""Tests for scripts/iem-pc/dante_card.py, the one module that imports
netaudio (#58 plan Task 15, outcome A). No card exists in CI: a fake device
stands in for netaudio's DanteDevice through `open_device`, and the source
is read for the calls it may make. Every value is synthetic (P6: a
documentation IP, an invented device name, the test site's channels)."""
from __future__ import annotations

import contextlib
import io
import json
import re
import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import dante_card  # noqa: E402

IP, NAME = "192.0.2.10", "Zyxqw-Iem-Card"


class Channel:
    def __init__(self, number: int, label: str | None) -> None:
        self.number, self.name, self.friendly_name = number, str(number), label


class Card:
    """The fake card's state, shared by every device opened on it (a fresh
    read after a write sees the write)."""

    def __init__(self, name: str = NAME, tx: int = 4, rx: int = 4) -> None:
        self.name = name
        self.labels = {"tx": {n: None for n in range(1, tx + 1)}, "rx": {n: None for n in range(1, rx + 1)}}
        self.calls: list[tuple] = []
        self.opened: list[str] = []
        self.deaf_to: set[tuple[str, int]] = set()   # writes the card ignores
        self.silent = False                         # answers nothing (get_controls sets error)
        self.reads = 0

    def device(self, ip: str):
        self.opened.append(ip)
        return Device(self)


class Device:
    """A stand-in for netaudio's DanteDevice: the attributes and the three
    coroutines the helper may use, recording every call."""

    def __init__(self, card: Card) -> None:
        self.card = card
        self.error = None
        self.name = ""
        self.rx_count = self.tx_count = 0
        self.tx_channels: dict = {}
        self.rx_channels: dict = {}

    async def get_controls(self) -> None:
        self.card.calls.append(("get_controls",))
        self.card.reads += 1
        print("netaudio noise on stdout")
        if self.card.silent:
            self.error = TimeoutError("synthetic")
            return
        self.name = self.card.name
        self.tx_count, self.rx_count = len(self.card.labels["tx"]), len(self.card.labels["rx"])
        self.tx_channels = {n: Channel(n, v) for n, v in self.card.labels["tx"].items()}
        self.rx_channels = {n: Channel(n, v) for n, v in self.card.labels["rx"].items()}

    async def set_channel_name(self, kind: str, number: int, label: str) -> None:
        self.card.calls.append(("set_channel_name", kind, number, label))
        if (kind, number) not in self.card.deaf_to:
            self.card.labels[kind][number] = label

    async def reset_channel_name(self, kind: str, number: int) -> None:
        self.card.calls.append(("reset_channel_name", kind, number))
        self.card.labels[kind][number] = None


class HelperTests(unittest.TestCase):
    def setUp(self) -> None:
        self.card = Card()
        patcher = mock.patch.object(dante_card, "open_device", self.card.device)
        patcher.start()
        self.addCleanup(patcher.stop)

    def run_helper(self, *argv: str, stdin: str = "") -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = dante_card.main(list(argv), io.StringIO(stdin))
        return code, out.getvalue(), err.getvalue()

    def writes(self) -> list[tuple]:
        return [c for c in self.card.calls if c[0] != "get_controls"]

    def test_read_prints_the_name_counts_and_labels_as_one_json_line(self) -> None:
        self.card.labels["tx"][2] = "ZYXQW inear R 02"
        code, out, err = self.run_helper("read", IP, NAME)
        self.assertEqual(code, 0, err)
        self.assertEqual(out.count("\n"), 1)   # netaudio's own prints went to stderr
        self.assertIn("netaudio noise", err)
        self.assertEqual(json.loads(out), {"name": NAME, "rx_count": 4, "tx_count": 4,
                                           "tx": {"1": "1", "2": "ZYXQW inear R 02", "3": "3", "4": "4"},
                                           "rx": {"1": "1", "2": "2", "3": "3", "4": "4"}})
        self.assertEqual((self.card.opened, self.writes()), ([IP], []))

    def test_write_sets_labels_resets_a_default_and_reads_every_one_back(self) -> None:
        self.card.labels["rx"][3] = "OLD"
        changes = [{"type": "tx", "channel": 1, "label": "ZYXQW inear L 01"},
                   {"type": "rx", "channel": 3, "label": "3"}]   # its number: the default, a reset
        code, out, err = self.run_helper("write", IP, NAME, stdin=json.dumps(changes))
        self.assertEqual(code, 0, err)
        self.assertEqual(self.writes(), [("set_channel_name", "tx", 1, "ZYXQW inear L 01"),
                                         ("reset_channel_name", "rx", 3)])
        doc = json.loads(out)
        self.assertEqual((doc["written"], doc["mismatches"]), (2, []))
        self.assertEqual((doc["tx"]["1"], doc["rx"]["3"]), ("ZYXQW inear L 01", "3"))
        self.assertEqual(self.card.reads, 2)   # one read before, one fresh read back after
        self.assertEqual(self.card.opened, [IP, IP])

    def test_a_label_that_does_not_read_back_is_a_mismatch(self) -> None:
        self.card.deaf_to.add(("tx", 2))
        changes = [{"type": "tx", "channel": 2, "label": "ZYXQW inear R 02"},
                   {"type": "tx", "channel": 3, "label": "QWXYZ inear L 03"}]
        code, out, err = self.run_helper("write", IP, NAME, stdin=json.dumps(changes))
        self.assertEqual(code, 0, err)
        self.assertEqual(json.loads(out)["mismatches"],
                         [{"type": "tx", "channel": 2, "want": "ZYXQW inear R 02", "got": "2"}])

    def test_another_device_or_no_answer_writes_nothing(self) -> None:
        changes = json.dumps([{"type": "tx", "channel": 1, "label": "ZYXQW inear L 01"}])
        self.card.name = "Qwxyz-Other-Card"
        for verb, stdin in (("read", ""), ("write", changes)):
            code, out, err = self.run_helper(verb, IP, NAME, stdin=stdin)
            self.assertEqual((code, out), (1, ""), verb)
            self.assertIn("not the expected one", err)
            self.assertNotIn("Qwxyz", err)   # the device's own name is never printed in a refusal
        self.assertIn("nothing was written", err)
        self.card.name, self.card.silent = NAME, True
        code, out, err = self.run_helper("write", IP, NAME, stdin=changes)
        self.assertEqual((code, out), (1, ""))
        self.assertIn("did not answer (TimeoutError): nothing was written", err)
        self.assertEqual(self.writes(), [])

    def test_a_channel_the_card_lacks_writes_nothing(self) -> None:
        changes = [{"type": "tx", "channel": 1, "label": "ZYXQW inear L 01"},
                   {"type": "rx", "channel": 9, "label": "09 ZYXQW mic"}]
        code, out, err = self.run_helper("write", IP, NAME, stdin=json.dumps(changes))
        self.assertEqual((code, out), (1, ""))
        self.assertIn("no rx channel 9: nothing was written", err)
        self.assertEqual(self.writes(), [])

    def test_a_read_back_that_fails_says_the_labels_were_written(self) -> None:
        card = self.card
        original = Device.set_channel_name

        async def set_then_go_silent(device, kind, number, label):
            await original(device, kind, number, label)
            card.silent = True

        with mock.patch.object(Device, "set_channel_name", set_then_go_silent):
            code, out, err = self.run_helper("write", IP, NAME,
                                             stdin=json.dumps([{"type": "tx", "channel": 1, "label": "ZYXQW L 01"}]))
        self.assertEqual((code, out), (1, ""))
        self.assertIn("1 labels were written, but not read back", err)

    def test_usage_is_refused_before_the_card_is_asked(self) -> None:
        bad = [((), ""), (("erase", IP, NAME), ""), (("read", "card.example", NAME), ""), (("read", IP, ""), ""),
               (("write", IP, NAME), "not json"), (("write", IP, NAME), "[]"),
               (("write", IP, NAME), json.dumps([{"type": "tx", "channel": 1}])),
               (("write", IP, NAME), json.dumps([{"type": "mix", "channel": 1, "label": "x"}])),
               (("write", IP, NAME), json.dumps([{"type": "tx", "channel": 0, "label": "x"}])),
               (("write", IP, NAME), json.dumps([{"type": "tx", "channel": True, "label": "x"}])),
               (("write", IP, NAME), json.dumps([{"type": "tx", "channel": 1, "label": "X" * 32}])),
               (("write", IP, NAME), json.dumps([{"type": "tx", "channel": 1, "label": "a.b"}])),
               (("write", IP, NAME), json.dumps([{"type": "tx", "channel": 1, "label": "Zyx" + chr(0x17D)}])),
               (("write", IP, NAME), json.dumps([{"type": "tx", "channel": 1, "label": "a"},
                                                 {"type": "tx", "channel": 1, "label": "b"}]))]
        for argv, stdin in bad:
            code, out, err = self.run_helper(*argv, stdin=stdin)
            self.assertEqual((code, out), (2, ""), argv)
            self.assertTrue(err.startswith("dante_card: "), argv)
        self.assertEqual((self.card.opened, self.card.calls), ([], []))


class SourceTests(unittest.TestCase):
    """What the helper may call on the card, read from its source (it is
    never run against a card in CI)."""
    SOURCE = (HERE / "dante_card.py").read_text(encoding="utf-8")

    def test_the_helper_calls_no_command_but_its_reads_and_its_label_writes(self) -> None:
        forbidden = ("add_subscription", "remove_subscription", "set_latency", "set_sample_rate", "set_encoding",
                     "set_gain_level", "set_name", "reset_name", "identify", "dante_command", "dante_send_command",
                     "get_volume")
        for name in forbidden:
            self.assertIsNone(re.search(rf"\b{name}\b", self.SOURCE), name)
        self.assertIsNone(re.search(r"\bcommand_\w+", self.SOURCE))
        self.assertEqual(set(re.findall(r"await device\.(\w+)\(", self.SOURCE)),
                         {"get_controls", "set_channel_name", "reset_channel_name"})
        self.assertEqual(re.findall(r"device\.(\w+) *=", self.SOURCE), ["ipv4", "services"])

    def test_only_the_helper_imports_netaudio(self) -> None:
        importers = sorted(p.name for p in HERE.glob("*.py")
                           if re.search(r"^\s*(?:from|import) netaudio\b", p.read_text(encoding="utf-8"), re.M))
        self.assertEqual(importers, ["dante_card.py"])


if __name__ == "__main__":
    unittest.main()
