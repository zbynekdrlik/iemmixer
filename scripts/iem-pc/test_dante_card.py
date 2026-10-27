"""Tests for scripts/iem-pc/dante_card.py, the one module that imports
netaudio (#58 plan Task 15, outcome A; the safety review of --apply). No
card exists in CI: a fake device stands in for netaudio's DanteDevice
through `open_device` and behaves as netaudio does (a card that does not
answer leaves the name and the channels empty, its error None), stub
netaudio modules run the real `open_device`, and the source is read for the
calls it may make. Every value is synthetic (P6: a documentation IP, an
invented device name, the test site's channels)."""
from __future__ import annotations

import contextlib
import io
import ipaddress
import json
import re
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import dante_card  # noqa: E402

IP, NAME = "192.0.2.10", "Zyxqw-Iem-Card"
SPARE = "Zyxqw-Iem-Card-2x"


class Channel:
    def __init__(self, number: int, label: str | None) -> None:
        self.number, self.name, self.friendly_name = number, str(number), label


class Subscription:
    """netaudio's DanteSubscription as get_controls fills it: the RX channel by
    its name (netaudio keeps no number on it), the TX channel and device, the
    status code."""

    def __init__(self, rx_name: str, tx_channel: str, tx_device: str, status: int) -> None:
        self.rx_channel_name, self.tx_channel_name, self.tx_device_name = rx_name, tx_channel, tx_device
        self.status_code = status


class Card:
    """The fake card's state, shared by every device opened on it (a fresh
    read after a write sees the write)."""

    def __init__(self, name: str = NAME, tx: int = 4, rx: int = 4) -> None:
        self.name = name
        self.labels = {"tx": {n: None for n in range(1, tx + 1)}, "rx": {n: None for n in range(1, rx + 1)}}
        self.calls: list[tuple] = []
        self.opened: list[str] = []
        self.deaf_to: set[tuple[str, int]] = set()      # writes the card ignores
        self.raise_on: dict[tuple[str, int], Exception] = {}   # writes whose command raises, unsent
        self.silent = False                           # answers nothing: no name, no channels
        self.partial = None                           # "tx" or "rx": that kind's last channel missing
        self.no_counts = False                        # names itself, reports no channel counts
        self.after_write = None                       # called after each write that returned
        self.keeps_name = False                       # set_name returns, the name stays
        self.subs: dict[int, tuple[str, str, int]] = {}   # rx number: (tx channel, tx device, status)
        self.reads = 0

    def device(self, ip: str):
        self.opened.append(ip)
        return Device(self)


class Device:
    """A stand-in for netaudio's DanteDevice: the attributes and the three
    coroutines the helper may use, recording every call. Like netaudio's
    get_controls it leaves `error` None whether the card answered or not."""

    def __init__(self, card: Card) -> None:
        self.card = card
        self.error = None
        self.name = ""
        self.rx_count = self.tx_count = 0
        self.tx_channels: dict = {}
        self.rx_channels: dict = {}
        self.subscriptions: list = []

    async def get_controls(self) -> None:
        self.card.calls.append(("get_controls",))
        self.card.reads += 1
        print("netaudio noise on stdout")
        self.error = None
        if self.card.silent:
            return
        self.name = self.card.name
        if self.card.no_counts:   # netaudio's count command got no reply: no channel is read
            return
        self.tx_count, self.rx_count = len(self.card.labels["tx"]), len(self.card.labels["rx"])
        self.tx_channels = {n: Channel(n, v) for n, v in self.card.labels["tx"].items()}
        self.rx_channels = {n: Channel(n, v) for n, v in self.card.labels["rx"].items()}
        self.subscriptions = [Subscription(self.rx_channels[n].name, *sub) for n, sub in self.card.subs.items()]
        if self.card.partial:
            channels = getattr(self, f"{self.card.partial}_channels")
            del channels[max(channels)]

    async def set_channel_name(self, kind: str, number: int, label: str) -> None:
        await self._write(kind, number, label)

    async def reset_channel_name(self, kind: str, number: int) -> None:
        await self._write(kind, number, None)

    async def set_name(self, name: str) -> None:
        self.card.calls.append(("set_name", name))
        if ("name", 0) in self.card.raise_on:
            raise self.card.raise_on[("name", 0)]
        if not self.card.keeps_name:
            self.card.name = name

    async def _write(self, kind: str, number: int, label: str | None) -> None:
        self.card.calls.append(("set_channel_name", kind, number, label) if label is not None
                               else ("reset_channel_name", kind, number))
        if (kind, number) in self.card.raise_on:
            raise self.card.raise_on[(kind, number)]
        if (kind, number) not in self.card.deaf_to:
            self.card.labels[kind][number] = label
        if self.card.after_write is not None:
            self.card.after_write()


class HelperTests(unittest.TestCase):
    def setUp(self) -> None:
        self.card = Card()
        patcher = mock.patch.object(dante_card, "open_device", self.card.device)
        patcher.start()
        self.addCleanup(patcher.stop)
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.flag = Path(tmp.name) / "EVENT-NOW"

    def run_helper(self, *argv: str, stdin: str = "") -> tuple[int, str, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = dante_card.main(list(argv), io.StringIO(stdin))
        return code, out.getvalue(), err.getvalue()

    def write(self, changes: list[dict]) -> tuple[int, dict | None, str]:
        code, out, err = self.run_helper("write", IP, NAME, str(self.flag), stdin=json.dumps(changes))
        return code, (json.loads(out) if out else None), err

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
        code, doc, err = self.write(changes)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.writes(), [("set_channel_name", "tx", 1, "ZYXQW inear L 01"),
                                         ("reset_channel_name", "rx", 3)])
        self.assertEqual((doc["sent"], doc["failed"], doc["error"], doc["read_back"], doc["mismatches"]),
                         (changes, None, None, True, []))
        self.assertEqual((doc["tx"]["1"], doc["rx"]["3"]), ("ZYXQW inear L 01", "3"))
        self.assertEqual(self.card.reads, 2)   # one read before, one fresh read back after
        self.assertEqual(self.card.opened, [IP, IP])

    def test_a_label_that_does_not_read_back_is_a_mismatch(self) -> None:
        self.card.deaf_to.add(("tx", 2))
        changes = [{"type": "tx", "channel": 2, "label": "ZYXQW inear R 02"},
                   {"type": "tx", "channel": 3, "label": "QWXYZ inear L 03"}]
        code, doc, err = self.write(changes)
        self.assertEqual(code, 0, err)
        self.assertEqual(doc["mismatches"], [{"type": "tx", "channel": 2, "want": "ZYXQW inear R 02", "got": "2"}])

    def test_a_write_that_fails_part_way_still_reads_back_and_reports_what_was_sent(self) -> None:
        """netaudio's commands catch only a timeout: another error (a refused
        connection) ends the loop. What was sent, the one that failed, and a
        fresh read back still come out as one JSON line, exit 0."""
        self.card.raise_on[("tx", 2)] = ConnectionRefusedError(111, "Connection refused")
        changes = [{"type": "tx", "channel": 1, "label": "ZYXQW inear L 01"},
                   {"type": "tx", "channel": 2, "label": "ZYXQW inear R 02"},
                   {"type": "tx", "channel": 3, "label": "QWXYZ inear L 03"}]
        code, doc, err = self.write(changes)
        self.assertEqual(code, 0, err)
        self.assertEqual((doc["sent"], doc["failed"], doc["read_back"]), (changes[:1], changes[1], True))
        self.assertIn("tx 2", doc["error"])
        self.assertIn("ConnectionRefusedError", doc["error"])
        # The one that failed is read back too: here it did not land.
        self.assertEqual(doc["mismatches"], [{"type": "tx", "channel": 2, "want": "ZYXQW inear R 02", "got": "2"}])
        self.assertEqual(doc["tx"]["3"], "3")   # never sent

    def test_a_read_back_that_fails_still_reports_what_was_sent(self) -> None:
        self.card.after_write = lambda: setattr(self.card, "silent", True)
        changes = [{"type": "tx", "channel": 1, "label": "ZYXQW inear L 01"}]
        code, doc, err = self.write(changes)
        self.assertEqual(code, 0, err)
        self.assertEqual((doc["sent"], doc["read_back"], doc["mismatches"], doc["tx"]), (changes, False, [], None))
        self.assertIn("not read back: the card did not answer", doc["error"])

    def test_the_event_flag_stops_the_writes_and_the_rest_is_read_back(self) -> None:
        self.card.after_write = lambda: self.flag.write_text("ide event\n", encoding="utf-8")
        changes = [{"type": "tx", "channel": 1, "label": "ZYXQW inear L 01"},
                   {"type": "tx", "channel": 2, "label": "ZYXQW inear R 02"}]
        code, doc, err = self.write(changes)
        self.assertEqual(code, 0, err)
        self.assertEqual(self.writes(), [("set_channel_name", "tx", 1, "ZYXQW inear L 01")])
        self.assertEqual((doc["sent"], doc["failed"], doc["read_back"]), (changes[:1], None, True))
        self.assertIn("EVENT-NOW appeared", doc["error"])

    def test_another_device_no_answer_or_an_incomplete_read_writes_nothing(self) -> None:
        changes = json.dumps([{"type": "tx", "channel": 1, "label": "ZYXQW inear L 01"}])
        self.card.name = "Qwxyz-Other-Card"
        for verb, argv, stdin in (("read", ("read", IP, NAME), ""),
                                  ("write", ("write", IP, NAME, str(self.flag)), changes)):
            code, out, err = self.run_helper(*argv, stdin=stdin)
            self.assertEqual((code, out), (1, ""), verb)
            self.assertIn("not the expected one", err)
            self.assertNotIn("Qwxyz", err)   # the device's own name is never printed in a refusal
        self.assertIn("nothing was written", err)
        # netaudio's get_controls leaves error None when the card is silent: no name is no answer.
        self.card.name, self.card.silent = NAME, True
        code, out, err = self.run_helper("write", IP, NAME, str(self.flag), stdin=changes)
        self.assertEqual((code, out), (1, ""))
        self.assertIn("the card did not answer: nothing was written", err)
        # A read that lost a page of channels, or got no channel counts, is no read.
        self.card.silent = False
        for partial, no_counts in (("tx", False), ("rx", False), (None, True)):
            self.card.partial, self.card.no_counts = partial, no_counts
            code, out, err = self.run_helper("read", IP, NAME)
            self.assertEqual((code, out), (1, ""), (partial, no_counts))
            self.assertIn(f"incomplete read of the card's {partial or 'tx'} channels", err)
        self.assertEqual(self.writes(), [])

    def rename(self, new: str = SPARE, expected: str = NAME) -> tuple[int, dict | None, str]:
        code, out, err = self.run_helper("rename", IP, expected, new)
        return code, (json.loads(out) if out else None), err

    def test_rename_gives_the_card_a_new_name_and_reads_it_back(self) -> None:
        # Gen 3 phase E's switch (#72): the card takes a spare name, so the
        # box can take the card's.
        code, doc, err = self.rename()
        self.assertEqual(code, 0, err)
        self.assertEqual(doc, {"renamed": True, "name": SPARE, "error": None})
        self.assertEqual(self.writes(), [("set_name", SPARE)])
        self.assertEqual((self.card.reads, self.card.opened), (2, [IP, IP]))
        # And back, from the spare name.
        code, doc, err = self.rename(NAME, SPARE)
        self.assertEqual((code, doc), (0, {"renamed": True, "name": NAME, "error": None}), err)

    def test_a_rename_the_card_does_not_take_or_that_fails_is_reported_with_the_name_it_holds(self) -> None:
        self.card.keeps_name = True
        code, doc, err = self.rename()
        self.assertEqual(code, 0, err)
        self.assertEqual(doc, {"renamed": False, "name": NAME, "error": "the card does not hold the new name"})
        self.card.keeps_name = False
        self.card.raise_on[("name", 0)] = ConnectionRefusedError(111, "Connection refused")
        code, doc, err = self.rename()
        self.assertEqual(code, 0, err)
        self.assertEqual((doc["renamed"], doc["name"]), (False, NAME))
        self.assertIn("renaming failed (ConnectionRefusedError", doc["error"])
        # A card silent after the command: not read back, the name unknown.
        self.card.raise_on.clear()
        self.card.after_write = None
        self.card.calls.clear()
        original = Device.set_name

        async def then_silent(device, name):
            await original(device, name)
            self.card.silent = True
        with mock.patch.object(Device, "set_name", then_silent):
            code, doc, err = self.rename()
        self.assertEqual((code, doc["renamed"], doc["name"]), (0, False, None), err)
        self.assertIn("not read back: the card did not answer", doc["error"])

    def test_a_rename_of_another_device_or_of_a_silent_card_sends_nothing(self) -> None:
        self.card.name = "Qwxyz-Other-Card"
        code, doc, err = self.rename()
        self.assertEqual((code, doc), (1, None))
        self.assertIn("not the expected one: nothing was renamed", err)
        self.assertNotIn("Qwxyz", err)
        self.card.name, self.card.silent = NAME, True
        code, doc, err = self.rename()
        self.assertEqual((code, doc), (1, None))
        self.assertIn("the card did not answer: nothing was renamed", err)
        self.assertEqual(self.writes(), [])

    def test_a_name_dante_does_not_hold_is_refused_before_the_card_is_asked(self) -> None:
        for new in ("", "X" * 32, "-zyxqw", "zyxqw-", "zyx qw", "zyx.qw", "zyx@qw", "Zyx" + chr(0x17D), NAME):
            code, doc, err = self.rename(new)
            self.assertEqual((code, doc), (2, None), new)
            self.assertTrue(err.startswith("dante_card: "), new)
        self.assertEqual((self.card.opened, self.card.calls), ([], []))
        self.assertEqual(self.rename("Z" * 31)[0], 0)   # 31 characters: the longest Dante holds

    def test_subscriptions_prints_each_subscribed_rx_channel_by_number(self) -> None:
        # Gen 3 phase E (#72): the card's RX table, mirrored onto our device.
        self.card.subs = {1: ("01 ZYXQW mic", "zyxqw-stagebox", 9), 3: ("3", NAME, 4)}
        code, out, err = self.run_helper("subscriptions", IP, NAME)
        self.assertEqual(code, 0, err)
        self.assertEqual(out.count("\n"), 1)
        self.assertEqual(json.loads(out), {"name": NAME, "rx_count": 4, "rx": {
            "1": {"tx_channel": "01 ZYXQW mic", "tx_device": "zyxqw-stagebox", "status": 9},
            "3": {"tx_channel": "3", "tx_device": NAME, "status": 4}}})
        self.assertEqual(self.writes(), [])
        # Another device, or a silent one, is refused as for a read.
        self.card.name = "Qwxyz-Other-Card"
        self.assertEqual(self.run_helper("subscriptions", IP, NAME)[:2], (1, ""))

    def test_a_subscription_whose_rx_channel_is_not_on_the_card_is_an_incomplete_read(self) -> None:
        self.card.subs = {2: ("x", "zyxqw-stagebox", 9)}
        original = Device.get_controls

        async def renamed_rx(device):
            await original(device)
            device.subscriptions[0].rx_channel_name = "zyxqw-gone"
        with mock.patch.object(Device, "get_controls", renamed_rx):
            code, out, err = self.run_helper("subscriptions", IP, NAME)
        self.assertEqual((code, out), (1, ""))
        self.assertIn("a subscription names no rx channel of the card", err)

    def test_a_channel_the_card_lacks_writes_nothing(self) -> None:
        changes = [{"type": "tx", "channel": 1, "label": "ZYXQW inear L 01"},
                   {"type": "rx", "channel": 9, "label": "09 ZYXQW mic"}]
        code, doc, err = self.write(changes)
        self.assertEqual((code, doc), (1, None))
        self.assertIn("no rx channel 9: nothing was written", err)
        self.assertEqual(self.writes(), [])

    def test_usage_is_refused_before_the_card_is_asked(self) -> None:
        one = json.dumps([{"type": "tx", "channel": 1, "label": "x"}])
        flag = str(self.flag)
        bad = [((), ""), (("erase", IP, NAME), ""), (("read", "card.example", NAME), ""), (("read", IP, ""), ""),
               (("read", IP, NAME, flag), ""), (("write", IP, NAME), one), (("write", IP, NAME, "EVENT-NOW"), one)]
        bad += [(("read", host, NAME), "") for host in ("224.0.0.251", "127.0.0.1", "0.0.0.0", "255.255.255.255",
                                                         "240.0.0.1", "::1", "2001:db8::10")]
        bad += [(("write", IP, NAME, flag), stdin) for stdin in (
            "not json", "[]", json.dumps([{"type": "tx", "channel": 1}]),
            json.dumps([{"type": "mix", "channel": 1, "label": "x"}]),
            json.dumps([{"type": "tx", "channel": 0, "label": "x"}]),
            json.dumps([{"type": "tx", "channel": 256, "label": "x"}]),
            json.dumps([{"type": "tx", "channel": True, "label": "x"}]),
            json.dumps([{"type": "tx", "channel": 1, "label": "X" * 32}]),
            json.dumps([{"type": "tx", "channel": 1, "label": "a.b"}]),
            json.dumps([{"type": "tx", "channel": 1, "label": "Zyx" + chr(0x17D)}]),
            json.dumps([{"type": "tx", "channel": 1, "label": "a"}, {"type": "tx", "channel": 1, "label": "b"}]))]
        for argv, stdin in bad:
            code, out, err = self.run_helper(*argv, stdin=stdin)
            self.assertEqual((code, out), (2, ""), argv)
            self.assertTrue(err.startswith("dante_card: "), argv)
        self.assertEqual((self.card.opened, self.card.calls), ([], []))
        # The highest channel Dante numbers is still a channel.
        top = [{"type": "rx", "channel": 255, "label": "x"}]
        self.assertEqual(dante_card.parse_changes(json.dumps(top)), top)


class OpenDeviceTests(unittest.TestCase):
    """The real `open_device` against stub netaudio modules: the card by its
    IP on the ARC port, nothing discovered."""

    def test_the_device_is_addressed_by_its_ip_on_the_arc_port(self) -> None:
        class DanteDevice:
            def __init__(self, server_name: str = "") -> None:
                self.server_name, self.ipv4, self.services = server_name, None, {}

        const = types.ModuleType("netaudio.dante.const")
        const.SERVICE_ARC = "_zyxqw-arc._udp.local."
        device = types.ModuleType("netaudio.dante.device")
        device.DanteDevice = DanteDevice
        stubs = {"netaudio": types.ModuleType("netaudio"), "netaudio.dante": types.ModuleType("netaudio.dante"),
                 "netaudio.dante.const": const, "netaudio.dante.device": device}
        with mock.patch.dict(sys.modules, stubs):
            d = dante_card.open_device(IP)
        self.assertIsInstance(d, DanteDevice)
        self.assertEqual(d.ipv4, ipaddress.ip_address(IP))
        self.assertEqual(d.services, {"arc": {"type": const.SERVICE_ARC, "port": 4440}})
        self.assertEqual(d.server_name, f"iempc-{IP}")


class SourceTests(unittest.TestCase):
    """What the helper may call on the card, read from its source (it is
    never run against a card in CI)."""
    SOURCE = (HERE / "dante_card.py").read_text(encoding="utf-8")

    def test_the_helper_calls_no_command_but_its_reads_its_label_writes_and_its_rename(self) -> None:
        forbidden = ("add_subscription", "remove_subscription", "set_latency", "set_sample_rate", "set_encoding",
                     "set_gain_level", "reset_name", "identify", "dante_command", "dante_send_command",
                     "get_volume", "socket")
        for name in forbidden:
            self.assertIsNone(re.search(rf"\b{name}\b", self.SOURCE), name)
        for raw in (r"\bcommand_\w+", r"\.sockets\b", r"\.send\("):
            self.assertIsNone(re.search(raw, self.SOURCE), raw)
        self.assertEqual(set(re.findall(r"await device\.(\w+)\(", self.SOURCE)),
                         {"get_controls", "set_channel_name", "reset_channel_name", "set_name"})
        # The card's own name is set in one place: the rename verb's function.
        self.assertEqual(len(re.findall(r"\.set_name\(", self.SOURCE)), 1)
        body = self.SOURCE.split("async def rename(", 1)[1].split("\ndef ", 1)[0].split("\nasync def ", 1)[0]
        self.assertIn("await device.set_name(", body)
        self.assertEqual(re.findall(r"device\.(\w+) *=", self.SOURCE), ["ipv4", "services"])

    def test_only_the_helper_imports_netaudio(self) -> None:
        importers = sorted(p.name for p in HERE.glob("*.py")
                           if re.search(r"^\s*(?:from|import) netaudio\b", p.read_text(encoding="utf-8"), re.M))
        self.assertEqual(importers, ["dante_card.py"])


if __name__ == "__main__":
    unittest.main()
