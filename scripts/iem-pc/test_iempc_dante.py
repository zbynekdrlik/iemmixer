"""Tests for scripts/iem-pc/iempc_dante.py: `iempc dante-labels` (#58 plan
Task 15, outcome A: the sheet and `--apply`, with the safety review's
fixes). The roster and the site are the synthetic config/test-roster.toml
and config/test-site.toml (P6: synthetic people and channel numbers); names
with letters a label cannot hold are built from code points, so this file
stays ASCII. No card exists in CI: a fake stands in for dante_card.py's
subprocess, and a stand-in program for the netaudio python."""
from __future__ import annotations

import json
import os
import shutil
import sys
import time
import tomllib
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_dante as dante  # noqa: E402
from iempc_test_support import Base, ip  # noqa: E402

CONFIG = Path(__file__).resolve().parents[2] / "config"
ROSTER = (CONFIG / "test-roster.toml").read_text(encoding="utf-8")
SITE = (CONFIG / "test-site.toml").read_text(encoding="utf-8")


def changed(text: str, *edits: tuple[str, str]) -> str:
    """`text` with each (old, new) replaced once; each old must be there."""
    for old, new in edits:
        if old not in text:
            raise AssertionError(f"{old!r} is not in the synthetic file")
        text = text.replace(old, new, 1)
    return text


def rows(roster: str = ROSTER, site: str = SITE) -> list[dante.Label]:
    return dante.labels(tomllib.loads(roster), tomllib.loads(site))


def by_channel(labels: list[dante.Label], kind: str) -> dict[int, dante.Label]:
    return {r.channel: r for r in labels if r.kind == kind}


class LabelTests(unittest.TestCase):
    def test_labels_follow_the_main_lineup_and_the_convention(self) -> None:
        labels = rows()
        tx, rx = by_channel(labels, "tx"), by_channel(labels, "rx")
        # Every set the main lineup places a person on: NAME inear L/R, the number last.
        self.assertEqual(sorted(tx), list(range(71, 89)))
        self.assertEqual((tx[71].label, tx[72].label), ("MEMBER 1 inear L 71", "MEMBER 1 inear R 72"))
        self.assertEqual((tx[87].label, tx[88].label), ("MEMBER 9 inear L 87", "MEMBER 9 inear R 88"))
        self.assertEqual((tx[71].of, tx[71].person, tx[71].shortened), ("member1", "member1", False))
        # Every input it gives an owner: the number first, then NAME and the input's type.
        self.assertEqual(sorted(rx), list(range(101, 111)))
        self.assertEqual([rx[c].label for c in (101, 104, 105, 110)],
                         ["101 MEMBER 1 mic", "104 MEMBER 4 mic", "105 MEMBER 4 gtr", "110 MEMBER 9 mic"])
        self.assertEqual((rx[105].of, rx[105].person), ("mic5", "member4"))
        # Only the main lineup names the card: youth borrows sets and mics under the main person's name.
        self.assertNotIn("MEMBER 10", " ".join(r.label for r in labels))
        self.assertNotIn("MEMBER 11", " ".join(r.label for r in labels))
        # The engineer's, the translator's and HIL's outputs and the inputs nobody owns get no row.
        self.assertFalse({91, 92, 93, 94, 95} & set(tx))
        self.assertFalse(set(range(111, 133)) & set(rx))
        for r in labels:
            self.assertLessEqual(len(r.label), dante.LIMIT, r)
            self.assertTrue(r.label.isascii() and r.label.isprintable(), r)
            self.assertFalse(set(r.label) & set("=.@"), r)

    def test_a_name_is_spelled_in_ascii_capitals_and_cut_to_fit(self) -> None:
        # Zyxqw. Dqx@w=zy with diacritics, and a letter NFKD keeps whole (an omega).
        accented = chr(0x17D) + "yxqw." + chr(0x3A9) + "  " + chr(0x10E) + "qx@w=zy"
        roster = changed(ROSTER, ('name = "Member 2"', 'name = "Zyxqwvnop Qwxyzrtlmn Vlqxt"'),
                         ('name = "Member 6"', f'name = "{accented}"'),
                         ('name = "Member 7"', 'name = "@.="'))
        site = changed(SITE, ("tx = [73, 74]", "tx = [3, 4]"), ("rx = [102]", "rx = [2]"))
        with self.assertRaises(dante.SheetError) as e:
            rows(roster, site)
        self.assertIn("person 'member7'", str(e.exception))
        labels = rows(changed(roster, ('name = "@.="', 'name = "Member 7"')), site)
        tx, rx = by_channel(labels, "tx"), by_channel(labels, "rx")
        # Two digits at least; a NAME too long for 31 characters is cut and the row says so.
        self.assertEqual((tx[3].label, tx[3].shortened), ("ZYXQWVNOP QWXYZRTLMN inear L 03", True))
        self.assertEqual(len(tx[3].label), dante.LIMIT)
        self.assertEqual(dante.row(tx[3]), {"channel": 3, "label": "ZYXQWVNOP QWXYZRTLMN inear L 03", "set": "member2",
                                            "person": "member2", "shortened": True})
        self.assertNotIn("shortened", dante.row(tx[81]))
        self.assertEqual((rx[2].label, rx[2].shortened), ("02 ZYXQWVNOP QWXYZRTLMN VLQ mic", True))
        self.assertEqual(len(rx[2].label), dante.LIMIT)
        self.assertEqual((tx[81].label, tx[81].shortened), ("ZYXQW DQXWZY inear L 81", False))
        self.assertEqual(rx[107].label, "107 ZYXQW DQXWZY mic")

    def test_a_stereo_input_gets_l_and_r_and_the_type_word_is_folded_like_name(self) -> None:
        guitar = "Git" + chr(0xE1) + "RA"   # capitals and a diacritic: folded like NAME, then lower-cased
        roster = changed(ROSTER, ('mic10 = { name = "MEMBER9 mic", owner = "member9" }',
                                  'mic10 = { name = "MEMBER9 mic", owner = "member9" }\n'
                                  'keys = { name = "MEMBER9 keys", owner = "member9" }\n'
                                  'iemonly = { name = "IEMONLY" }'),
                         ('mic2 = { name = "MEMBER2 mic"', 'mic2 = { name = "MEMBER2 Vocals"'),
                         ('mic3 = { name = "MEMBER3 mic"', f'mic3 = {{ name = "MEMBER3 {guitar}"'),
                         ('mic4 = { name = "MEMBER4 mic"', 'mic4 = { name = "MEMBER4"'),
                         ('mic5 = { name = "MEMBER4 gtr"', 'mic5 = { name = "gtr"'),
                         ('mic6 = { name = "MEMBER5 mic"', 'mic6 = { name = "MEMBER5 backing vox"'),
                         ('mic7 = { name = "MEMBER6 mic"', 'mic7 = { name = "MEMBER6 microphone"'),
                         ('member1 = "member1"\nmember2 = "member2"', 'member2 = "member2"\nmember1 = "member1"'))
        labels = rows(roster)
        rx = by_channel(labels, "rx")
        self.assertEqual((rx[115].label, rx[116].label), ("115 MEMBER 9 keys L", "116 MEMBER 9 keys R"))
        # The last word folded (ASCII, lower case) when it is then a short word; no second word, or a
        # longer one: mic.
        self.assertEqual([rx[c].label for c in (102, 103, 104, 105, 106, 107)],
                         ["102 MEMBER 2 vocals", "103 MEMBER 3 gitara", "104 MEMBER 4 mic", "105 MEMBER 4 mic",
                          "106 MEMBER 5 vox", "107 MEMBER 6 mic"])
        # The rows go by channel whatever order the roster lists its sets and inputs in.
        self.assertEqual([r.channel for r in labels], sorted(r.channel for r in labels))
        self.assertEqual([r.kind for r in labels], ["tx"] * 18 + ["rx"] * 12)
        self.assertNotIn(117, rx)   # listed without an owner: no person names it

    def test_a_set_or_input_the_main_lineup_leaves_empty_names_its_single_wearer(self) -> None:
        """The coordinator's decision on #62: the main lineup's wearer first;
        else the one person who wears the set (owns the input) in the other
        lineups; else no row, the card keeps its label."""
        roster = changed(ROSTER, ('member6 = "member6"\n', ""), ('member7 = "member7"\n', ""),
                         ('member8 = "member8"\n', ""), ('member9 = "member9"\n', ""),
                         ('mic3 = { name = "MEMBER3 mic", owner = "member3" }\n', ""),
                         ('mic4 = { name = "MEMBER4 mic", owner = "member4" }\n', ""),
                         ('member4 = "member11"', 'member4 = "member11"\nmember9 = "member2"\nmember8 = "member4"\n'
                                                  'member7 = "member6"'),
                         ('mic4 = { name = "MEMBER11 mic", owner = "member11" }',
                          'mic4 = { name = "MEMBER11 mic", owner = "member11" }\n'
                          'mic3 = { name = "MEMBER5 gtr", owner = "member5" }'))
        roster += ('\n[[lineups]]\nid = "kids"\nname = "Kids band"\n\n[lineups.sets]\nmember8 = "member4"\n'
                   'member7 = "member5"\n\n[lineups.inputs]\nmic3 = { name = "MEMBER6 mic", owner = "member6" }\n')
        labels = rows(roster)
        tx, rx = by_channel(labels, "tx"), by_channel(labels, "rx")
        # A set only youth uses: its youth wearer, and the row says which lineup named it.
        self.assertEqual((tx[87].label, tx[87].person, tx[87].lineup), ("MEMBER 2 inear L 87", "member2", "youth"))
        self.assertEqual(dante.row(tx[88])["lineup"], "youth")
        # Worn by one person in two other lineups: that person, the first lineup naming them.
        self.assertEqual((tx[85].label, tx[85].lineup), ("MEMBER 4 inear L 85", "youth"))
        # A set nobody wears (member6's), and one different people wear (member7's): no row.
        self.assertFalse({81, 82, 83, 84} & set(tx))
        # An input only youth owns: its youth owner and youth's own label's type.
        self.assertEqual((rx[104].label, rx[104].person, rx[104].lineup), ("104 MEMBER 11 mic", "member11", "youth"))
        # An input different people own in the other lineups: no row.
        self.assertNotIn(103, rx)
        # The main lineup's own still come first, with no lineup named.
        self.assertEqual((tx[71].label, tx[71].lineup, rx[101].label), ("MEMBER 1 inear L 71", None, "101 MEMBER 1 mic"))
        self.assertNotIn("lineup", dante.row(tx[71]))
        # The main lineup's wearer wins over another lineup's: youth's member10 on set member1 names nothing.
        self.assertNotIn("MEMBER 10", " ".join(r.label for r in labels))

    def test_renamed_tx_channels_are_listed_for_resubscription(self) -> None:
        after = rows(changed(ROSTER, ('name = "Member 2"', 'name = "Member Two"'),
                             ('member5 = "member5"', 'member5 = "member10"'),
                             ('mic3 = { name = "MEMBER3 mic", owner = "member3" }',
                              'mic3 = { name = "MEMBER3 mic", owner = "member2" }'),
                             ('member4 = "member11"', 'member4 = "member9"')))
        sheet = dante.sheet(after, dante.tx_labels(rows()), "before")
        self.assertEqual(sheet["resubscribe"], [
            {"channel": 73, "label": "MEMBER TWO inear L 73", "before": "MEMBER 2 inear L 73"},
            {"channel": 74, "label": "MEMBER TWO inear R 74", "before": "MEMBER 2 inear R 74"},
            {"channel": 79, "label": "MEMBER 10 inear L 79", "before": "MEMBER 5 inear L 79"},
            {"channel": 80, "label": "MEMBER 10 inear R 80", "before": "MEMBER 5 inear R 80"},
        ])
        self.assertEqual(sheet["compared"], "before")
        # An RX rename keeps its subscription: on the sheet, never in the list.
        self.assertIn({"channel": 103, "label": "103 MEMBER TWO mic", "input": "mic3", "person": "member2"},
                      sheet["rx"])
        # The same roster renames nothing.
        self.assertEqual(dante.sheet(rows(), dante.tx_labels(rows()), "before")["resubscribe"], [])
        # Without the roster the card follows now, its labels are not known: every TX row is listed.
        unknown = dante.sheet(after, None, None)
        self.assertIsNone(unknown["compared"])
        self.assertEqual([(r["channel"], r["before"]) for r in unknown["resubscribe"]],
                         [(c, None) for c in range(71, 89)])

    def test_a_roster_or_site_the_labels_cannot_come_from_is_refused(self) -> None:
        cases = [
            (changed(ROSTER, ("main = true\n", "")), SITE, "exactly one main lineup is needed, it has 0"),
            (changed(ROSTER, ('name = "Youth band"', 'name = "Youth band"\nmain = true')), SITE, "it has 2"),
            (ROSTER, changed(SITE, ("rx = [101]", "rx = [101, 133, 134]")), "input 'mic1'"),
            (changed(ROSTER, ('member9 = "member9"', 'member9 = "member12"')), SITE, "person 'member12'"),
            (changed(ROSTER, ('member9 = "member9"', 'translator = "member9"')), SITE, "set 'translator'"),
            (changed(ROSTER, ('member9 = "member9"', 'member12 = "member9"')), SITE, "set 'member12'"),
            (changed(ROSTER, ('owner = "member9"', 'owner = "member12"')), SITE, "person 'member12'"),
            (changed(ROSTER, ("mic10 = {", "mic99 = {")), SITE, "input 'mic99'"),
            (ROSTER, changed(SITE, ("rx = [101]", 'rx = ["101"]')), "input 'mic1'"),
            (ROSTER, changed(SITE, ("rx = [101]", "rx = [256]")), "input 'mic1'"),   # Dante numbers 1 to 255
            (ROSTER, SITE.split("[engine]")[0], "[engine]"),
        ]
        for roster, site, words in cases:
            with self.assertRaises(dante.SheetError, msg=words) as e:
                rows(roster, site)
            self.assertIn(words, str(e.exception))
        self.assertIn(255, by_channel(rows(ROSTER, changed(SITE, ("rx = [101]", "rx = [255]"))), "rx"))


class DanteLabelsCommandTests(Base):
    def setUp(self) -> None:
        super().setUp()
        self.site_dir = self.tmp / "ops" / "site"
        self.site_dir.mkdir(parents=True)
        self.roster = self.site_dir / "roster.toml"
        shutil.copy(CONFIG / "test-roster.toml", self.roster)
        shutil.copy(CONFIG / "test-site.toml", self.site_dir / "site.toml")

    def test_the_sheet_comes_from_the_site_beside_the_roster_and_touches_nothing(self) -> None:
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"dante_labels": dante.sheet(rows(), None, None)}])
        self.assertEqual((self.pc.calls, self.pc.modules, self.pc.scps, self.gh.named("workflow")), ([], [], [], []))
        other = self.tmp / "elsewhere.toml"
        shutil.copy(CONFIG / "test-site.toml", other)
        self.roster.with_name("site.toml").unlink()
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster), "--site", str(other),
                                        "--before", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertEqual((docs[0]["dante_labels"]["resubscribe"], docs[0]["dante_labels"]["compared"]), ([], "before"))

    def test_an_unreadable_file_is_refused_naming_it(self) -> None:
        self.roster.with_name("site.toml").unlink()
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster))
        self.assertEqual((code, docs), (1, []))
        self.assertIn("site.toml", err)
        bad = self.tmp / "bad.toml"
        bad.write_text("[[people]\n", encoding="utf-8")
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster), "--site",
                                        str(CONFIG / "test-site.toml"), "--before", str(bad))
        self.assertEqual((code, docs), (1, []))
        self.assertIn("--before", err)
        self.assertIn("not TOML", err)


HOST, NAME = "192.0.2.10", "Zyxqw-Iem-Card"   # a documentation IP and an invented name (P6)


class FakeHelper:
    """Stands in for dante_card.py's subprocess (no card in CI): records each
    run's argv and stdin and answers as the helper does, from a card of 128
    TX and 128 RX channels that carry their default labels unless set. A
    write checks the flag path it is given before each label, as the helper
    does."""

    def __init__(self, test: Base) -> None:
        self.test = test
        self.calls: list[tuple[list[str], str]] = []
        self.card = {"tx": {str(n): str(n) for n in range(1, 129)}, "rx": {str(n): str(n) for n in range(1, 129)}}
        self.counts: dict[str, int] = {}               # a count the read reports instead of the labels'
        self.deaf: set[tuple[str, int]] = set()        # writes the card ignores
        self.garbled: dict[tuple[str, int], str] = {}  # writes that land as another label
        self.raise_at: tuple[str, int] | None = None   # the write whose command raises: the writes stop
        self.no_read_back = False
        self.refuse: str | None = None                 # every run fails with these words
        self.write_error: Exception | None = None      # a write run that fails or outlives its bound
        self.on_read = None
        self.after_write = None
        self.tamper = None                             # changes a write's answer

    def __call__(self, _ip, cmd: list[str], stdin: str) -> str:
        self.calls.append((list(cmd), stdin))
        if self.refuse is not None:
            raise ip.StepError(self.refuse)
        if cmd[3] == "read":
            if self.on_read is not None:
                self.on_read()
            return "netaudio noise\n" + json.dumps({"name": NAME, "rx_count": self.counts.get("rx", len(self.card["rx"])),
                                                    "tx_count": self.counts.get("tx", len(self.card["tx"])),
                                                    **self.card})
        if self.write_error is not None:
            raise self.write_error
        flag = Path(cmd[6])
        sent, failed, errors = [], None, []
        for c in json.loads(stdin):
            key = (c["type"], c["channel"])
            if flag.exists():
                errors.append(f"{flag} appeared: writing stopped")
                break
            if key == self.raise_at:
                failed = c
                errors.append(f"writing {c['type']} {c['channel']} failed (ConnectionRefusedError: synthetic)")
                break
            if key in self.garbled:
                self.card[c["type"]][str(c["channel"])] = self.garbled[key]
            elif key not in self.deaf:
                self.card[c["type"]][str(c["channel"])] = c["label"]
            sent.append(c)
            if self.after_write is not None:
                self.after_write()
        if self.no_read_back:
            errors.append("not read back: the card did not answer")
        back = not self.no_read_back
        mismatches = [{"type": c["type"], "channel": c["channel"], "want": c["label"],
                       "got": self.card[c["type"]][str(c["channel"])]}
                      for c in sent + ([failed] if failed else [])
                      if back and self.card[c["type"]][str(c["channel"])] != c["label"]]
        doc = {"sent": sent, "failed": failed, "error": "; ".join(errors) or None, "read_back": back,
               "mismatches": mismatches, "tx": dict(self.card["tx"]) if back else None,
               "rx": dict(self.card["rx"]) if back else None}
        if self.tamper is not None:
            self.tamper(doc)
        return json.dumps(doc)

    def verbs(self) -> list[str]:
        return [cmd[3] for cmd, _ in self.calls]

    def written(self) -> list[dict]:
        return [c for cmd, stdin in self.calls if cmd[3] == "write" for c in json.loads(stdin)]


class ApplyTests(Base):
    def setUp(self) -> None:
        super().setUp()
        site_dir = self.tmp / "ops" / "site"
        site_dir.mkdir(parents=True)
        self.roster = site_dir / "roster.toml"
        shutil.copy(CONFIG / "test-roster.toml", self.roster)
        shutil.copy(CONFIG / "test-site.toml", site_dir / "site.toml")
        self.python = self.tmp / "netaudio" / "bin" / "python"
        self.python.parent.mkdir(parents=True)
        self.python.write_text("", encoding="utf-8")
        self.python.chmod(0o755)
        self.env_text = ip.env_path().read_text(encoding="utf-8")
        ip.env_path().write_text(self.env_text + f"DANTE_IEM_HOST={HOST}\nDANTE_IEM_NAME={NAME}\n"
                                 f"NETAUDIO_PYTHON={self.python}\n", encoding="utf-8")
        self.fake = FakeHelper(self)
        patcher = mock.patch.object(dante, "helper", self.fake)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.rows = rows()

    def apply(self, *extra: str) -> tuple[int, list[dict], str]:
        return self.run_main("dante-labels", "--roster", str(self.roster), "--apply", *extra)

    def planned(self, rows: list[dante.Label]) -> list[dict]:
        """The report's entries for `rows`, each with the default label the fake card held before."""
        return [{"type": r.kind, "channel": r.channel, "label": r.label, "before": str(r.channel)} for r in rows]

    def tx(self, rows: list[dante.Label]) -> list[dict]:
        return [{"channel": r.channel, "label": r.label, "before": str(r.channel)} for r in rows if r.kind == "tx"]

    def test_apply_touches_only_the_iem_card(self) -> None:
        code, docs, err = self.apply()
        self.assertEqual(code, 0, err)
        # Two runs of the helper, a read and a write, each naming only the configured card; the write
        # also gets the EVENT-NOW flag's path, which it checks before each label.
        self.assertEqual(self.fake.verbs(), ["read", "write"])
        head = [str(self.python), "-I", str(dante.HELPER)]
        self.assertEqual([cmd for cmd, _ in self.fake.calls], [head + ["read", HOST, NAME],
                                                              head + ["write", HOST, NAME, str(ip.EVENT_NOW)]])
        # Only label changes, each one of the sheet's rows.
        written = self.fake.written()
        self.assertTrue(all(set(c) == {"type", "channel", "label"} for c in written))
        self.assertEqual(sorted((c["type"], c["channel"], c["label"]) for c in written),
                         sorted((r.kind, r.channel, r.label) for r in self.rows))
        out = docs[-1]["dante_apply"]
        self.assertEqual((out["changed"], out["unchanged"], out["confirmed"], out["unsent"], out["mismatches"],
                          out["error"]), (len(self.rows), 0, True, [], [], None))
        self.assertEqual(out["written"], self.planned(self.rows))
        self.assertEqual([r["channel"] for r in out["resubscribe"]], list(range(71, 89)))
        self.assertEqual(out["resubscribe"][0], {"channel": 71, "label": "MEMBER 1 inear L 71", "before": "71"})
        self.assertEqual((self.pc.calls, self.pc.modules, self.pc.scps, self.gh.named("workflow")), ([], [], [], []))

    def test_only_the_labels_that_differ_are_written(self) -> None:
        for r in self.rows[:4] + self.rows[-2:]:
            self.fake.card[r.kind][str(r.channel)] = r.label
        code, docs, err = self.apply()
        self.assertEqual(code, 0, err)
        written = {(c["type"], c["channel"]) for c in self.fake.written()}
        self.assertEqual(written, {(r.kind, r.channel) for r in self.rows[4:-2]})
        out = docs[-1]["dante_apply"]
        self.assertEqual((out["changed"], out["unchanged"]), (len(self.rows) - 6, 6))
        self.assertEqual(out["resubscribe"][0]["channel"], 75)   # 71-74 hold their labels already
        # The card holds every label now: nothing to change, no write.
        self.fake.calls.clear()
        code, docs, err = self.apply()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.fake.verbs(), ["read"])
        out = docs[-1]["dante_apply"]
        self.assertEqual((out["changed"], out["written"], out["resubscribe"], out["confirmed"]), (0, [], [], True))
        self.assertIn("nothing to change", out["note"])

    def test_resubscribe_lists_the_tx_labels_the_card_changed(self) -> None:
        """The safety review's M8: a TX label that did not land keeps its
        subscription, so it is not listed; one that landed as another label
        is listed with the label the card holds, marked."""
        self.fake.deaf.add(("tx", 71))
        self.fake.garbled[("tx", 72)] = "72 ZYXQW"
        code, docs, err = self.apply()
        self.assertEqual(code, 1, err)
        out = docs[-1]["dante_apply"]
        self.assertEqual(out["mismatches"],
                         [{"type": "tx", "channel": 71, "want": "MEMBER 1 inear L 71", "got": "71"},
                          {"type": "tx", "channel": 72, "want": "MEMBER 1 inear R 72", "got": "72 ZYXQW"}])
        self.assertEqual(out["resubscribe"][:2], [{"channel": 72, "label": "72 ZYXQW", "before": "72", "mismatch": True},
                                                  {"channel": 73, "label": "MEMBER 2 inear L 73", "before": "73"}])
        self.assertEqual([r["channel"] for r in out["resubscribe"]], list(range(72, 89)))
        self.assertEqual(out["written"], self.planned(self.rows[2:]))   # what the card holds as asked
        self.assertEqual((out["confirmed"], out["unsent"]), (True, []))

    def test_a_write_that_fails_part_way_reports_what_landed(self) -> None:
        """The safety review's I1: the helper reports what it sent before a
        write raised; what landed, what was never sent and the subscriptions
        to remake come out, exit 1."""
        self.fake.raise_at = ("tx", 73)
        code, docs, err = self.apply()
        self.assertEqual(code, 1, err)
        out = docs[-1]["dante_apply"]
        self.assertEqual((out["confirmed"], out["written"], out["unsent"]),
                         (True, self.planned(self.rows[:2]), self.planned(self.rows[3:])))
        self.assertEqual(out["mismatches"], [{"type": "tx", "channel": 73, "want": "MEMBER 2 inear L 73", "got": "73"}])
        self.assertEqual(out["resubscribe"], self.tx(self.rows[:2]))
        self.assertIn("tx 73", out["error"])

    def test_the_flag_during_the_writes_stops_them(self) -> None:
        """The safety review's M3: an "ide event" while the helper writes
        stops it before the next label; the rest is reported unsent."""
        self.fake.after_write = self.flag
        code, docs, err = self.apply()
        self.assertEqual(code, 1, err)
        out = docs[-1]["dante_apply"]
        self.assertEqual((out["confirmed"], out["written"], out["unsent"], out["resubscribe"]),
                         (True, self.planned(self.rows[:1]), self.planned(self.rows[1:]), self.tx(self.rows[:1])))
        self.assertIn("appeared", out["error"])

    def test_a_write_not_read_back_is_unconfirmed(self) -> None:
        self.fake.no_read_back = True
        code, docs, err = self.apply()
        self.assertEqual(code, 1, err)
        out = docs[-1]["dante_apply"]
        self.assertEqual((out["confirmed"], out["written"], out["unsent"], out["mismatches"], out["resubscribe"]),
                         (False, self.planned(self.rows), [], None, self.tx(self.rows)))
        self.assertIn("not read back", out["error"])

    def test_a_write_run_that_fails_or_outlives_its_bound_prints_the_plan_unconfirmed(self) -> None:
        """The safety review's I1 and M6: no answer from the write run, or one
        that is not a report of its writes; the planned labels and their
        subscriptions are printed unconfirmed, then it fails."""
        failures = [(ip.StepError("python failed (exit 1): Traceback: ConnectionRefusedError"), "ConnectionRefusedError"),
                    (ip.StillRunning(f"dante_card.py write still running after {dante.CARD_S:g} s here on this box "
                                     "(pid 4242)"), "here on this box (pid 4242)")]
        for error, words in failures:
            self.fake.write_error = error
            code, docs, err = self.apply()
            self.assertEqual(code, 1, words)
            self.assertIn(words, err)
            out = docs[-1]["dante_apply"]
            self.assertEqual((out["confirmed"], out["written"], out["unsent"], out["mismatches"], out["resubscribe"]),
                             (False, self.planned(self.rows), None, None, self.tx(self.rows)), words)
            self.assertIn(words, out["error"])
            self.fake.card = {"tx": {str(n): str(n) for n in range(1, 129)}, "rx": {str(n): str(n) for n in range(1, 129)}}
        self.fake.write_error = None
        for tamper in (lambda d: d.update(sent=d["sent"] + [{"type": "tx", "channel": 1, "label": "x"}]),
                       lambda d: d.update(sent=d["sent"][1:]), lambda d: d.update(read_back="yes"),
                       lambda d: d.pop("mismatches"), lambda d: d.update(error=3), lambda d: d.update(failed=[]),
                       lambda d: d.update(tx={"71": 71})):
            self.fake.tamper = tamper
            code, docs, err = self.apply()
            self.assertEqual(code, 1)
            self.assertIn("answered no report of its writes", err)
            self.assertEqual((docs[-1]["dante_apply"]["confirmed"], docs[-1]["dante_apply"]["written"]),
                             (False, self.planned(self.rows)))
            self.fake.card = {"tx": {str(n): str(n) for n in range(1, 129)}, "rx": {str(n): str(n) for n in range(1, 129)}}

    def test_another_device_at_the_host_writes_nothing(self) -> None:
        self.fake.refuse = ("python failed (exit 1): dante_card: the device answering at the configured host is not "
                            "the expected one: nothing was written")
        code, docs, err = self.apply()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("not the expected one", err)
        self.assertEqual(self.fake.verbs(), ["read"])

    def test_a_channel_the_card_lacks_writes_nothing(self) -> None:
        """The card's counts decide (the safety review's M5): a channel above
        the count, or one the read did not list, refuses before any write."""
        default = {str(n): str(n) for n in range(1, 129)}
        cards = [({str(n): str(n) for n in range(1, 65)}, None),              # 64 channels
                 ({**{str(n): str(n) for n in range(1, 64)}, "71": "71"}, None),   # 64 listed, 71 above the count
                 ({str(n): str(n) for n in range(1, 130) if n != 71}, None)]   # 128, 71 not listed
        for tx, count in cards:
            self.fake.card["tx"] = tx
            self.fake.counts = {} if count is None else {"tx": count}
            self.fake.calls.clear()
            code, docs, err = self.apply()
            self.assertEqual((code, docs), (1, []), sorted(tx, key=int)[-1])
            self.assertIn("the card has no tx channel 71 (nothing was written)", err)
            self.assertEqual(self.fake.verbs(), ["read"])
        # A read whose counts are not its labels' is no read.
        self.fake.card["tx"], self.fake.counts = default, {"tx": 129}
        code, docs, err = self.apply()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("dante_card.py gave no tx labels", err)

    def test_apply_refuses_while_event_now(self) -> None:
        """The plan's name: no label is written during an event. Dev time and
        locked (refused at the start), the flag read again right before the
        write (one that came during the card's read), and the helper checks
        it before each label (test_the_flag_during_the_writes_stops_them)."""
        spec = ip.COMMANDS["dante-labels"]
        self.assertEqual((spec.fn, spec.pc, spec.dev_time, spec.locked), (ip.cmd_dante_labels, False, True, True))
        self.flag()
        code, docs, err = self.apply()
        self.assertEqual((code, docs, self.fake.calls), (1, [], []))
        self.assertIn("runs only in dev time", err)
        ip.EVENT_NOW.unlink()
        self.fake.on_read = self.flag
        code, docs, err = self.apply()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("appeared: no label is written during an event (nothing was written)", err)
        self.assertEqual(self.fake.verbs(), ["read"])

    def test_apply_refuses_without_the_cards_env(self) -> None:
        full = ip.env_path().read_text(encoding="utf-8")
        for key in ("DANTE_IEM_HOST", "DANTE_IEM_NAME", "NETAUDIO_PYTHON"):
            ip.env_path().write_text("".join(line + "\n" for line in full.splitlines() if not line.startswith(key)),
                                     encoding="utf-8")
            code, docs, err = self.apply()
            self.assertEqual((code, docs), (1, []), key)
            self.assertIn(f"{key} missing in the private env", err)
        plain = self.tmp / "netaudio" / "bin" / "python-plain"
        plain.write_text("", encoding="utf-8")
        plain.chmod(0o644)
        bad = [("DANTE_IEM_HOST", host, "not a unicast IPv4 address")
               for host in ("iem-card.example", "224.0.0.251", "127.0.0.1", "0.0.0.0", "255.255.255.255", "2001:db8::10")]
        bad += [("NETAUDIO_PYTHON", python, "not the full path of an executable file")
                for python in ("python3", str(self.tmp / "none"), str(plain), str(self.python.parent))]
        for key, value, words in bad:
            ip.env_path().write_text(full.replace(f"{key}=", f"{key}={value}\n#"), encoding="utf-8")
            code, docs, err = self.apply()
            self.assertEqual((code, docs), (1, []), value)
            self.assertIn(words, err, value)
            self.assertNotIn(value, err)   # a value of the private env is never printed
        self.assertEqual(self.fake.calls, [])
        # Without the keys the sheet still prints, from --before or nothing.
        ip.env_path().write_text(self.env_text, encoding="utf-8")
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertIsNone(docs[-1]["dante_labels"]["compared"])
        self.assertEqual(self.fake.calls, [])

    def test_the_sheet_compares_with_the_card_and_falls_back_to_before(self) -> None:
        for r in self.rows[:2]:
            self.fake.card[r.kind][str(r.channel)] = r.label
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster), "--before", str(self.roster))
        self.assertEqual(code, 0, err)
        sheet = docs[-1]["dante_labels"]
        self.assertEqual(sheet["compared"], "card")   # the card read now wins over --before
        self.assertEqual([r["channel"] for r in sheet["resubscribe"]], list(range(73, 89)))
        self.assertEqual(self.fake.verbs(), ["read"])
        # The card cannot be read: a warning, then --before (nothing differs), or nothing known.
        self.fake.refuse = "python failed (exit 1): dante_card: the card did not answer"
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster), "--before", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertIn("WARNING: the card's labels could not be read", err)
        self.assertEqual((docs[-1]["dante_labels"]["compared"], docs[-1]["dante_labels"]["resubscribe"]),
                         ("before", []))
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertIn("every TX row is listed", err)
        self.assertIsNone(docs[-1]["dante_labels"]["compared"])


class HelperRunTests(Base):
    """The real `helper`: dante_card.py runs here, on the dev box, in its own
    process, bounded by CARD_S and never ended by force (the safety review's
    M4 and M6). A stand-in program takes the netaudio python's place."""

    def setUp(self) -> None:
        super().setUp()
        self.log = self.tmp / "standin.log"
        self.done = self.tmp / "standin.done"
        self.program = self.tmp / "netaudio" / "bin" / "python"
        self.program.parent.mkdir(parents=True)
        self.program.write_text(
            f"#!{sys.executable}\nimport json, sys, time\n"
            f"open({str(self.log)!r}, 'a').write(' '.join(sys.argv[3:]) + '\\n')\n"
            "data = sys.stdin.read()\nverb = sys.argv[3]\n"
            "print('netaudio noise', file=sys.stderr)\n"
            "if verb == 'refuse':\n    print('dante_card: refused here', file=sys.stderr)\n    sys.exit(1)\n"
            "if verb == 'write':\n    time.sleep(1.5)\n"
            f"print(json.dumps({{'verb': verb, 'stdin': data}}))\nopen({str(self.done)!r}, 'a').write('ended\\n')\n",
            encoding="utf-8")
        self.program.chmod(0o755)

    def cmd(self, verb: str, program: Path | None = None) -> list[str]:
        return [str(program or self.program), "-I", str(dante.HELPER), verb, HOST, NAME]

    def runs(self) -> list[str]:
        return self.log.read_text(encoding="utf-8").splitlines() if self.log.exists() else []

    def wait_for_end(self) -> None:
        """Until the recorded stand-in has ended by itself (never ended by force)."""
        pid = ip.read_json(ip.STATE_DIR / dante.RECORD, {})["pid"]
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            try:
                alive = str(dante.HELPER).encode() in Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")
            except (FileNotFoundError, ProcessLookupError):
                alive = False
            if not alive:
                break
            time.sleep(0.05)
        self.assertTrue(self.done.exists(), "the stand-in never ended")

    def test_a_run_answers_its_stdout_or_fails_with_its_words(self) -> None:
        out = dante.helper(ip, self.cmd("read"), "")
        self.assertEqual(json.loads(out), {"verb": "read", "stdin": ""})   # its stderr stays out of the answer
        with self.assertRaises(ip.StepError) as e:
            dante.helper(ip, self.cmd("refuse"), "")
        self.assertIn("(exit 1)", str(e.exception))
        self.assertIn("dante_card: refused here", str(e.exception))

    def test_a_python_that_cannot_start_is_a_step_error_without_its_path(self) -> None:
        hidden = self.tmp / "zyxqw-private"
        hidden.mkdir()
        plain = hidden / "python"
        plain.write_text("", encoding="utf-8")
        for program in (hidden / "none", plain):
            with self.assertRaises(ip.StepError) as e:
                dante.helper(ip, self.cmd("read", program), "")
            self.assertNotIn("zyxqw-private", str(e.exception))
            self.assertIn("could not be started", str(e.exception))
        self.assertEqual(self.runs(), [])

    def test_a_run_past_its_bound_is_left_to_end_and_no_second_run_overlaps_it(self) -> None:
        with mock.patch.object(dante, "CARD_S", 0.3):
            with self.assertRaises(ip.StillRunning) as e:
                dante.helper(ip, self.cmd("write"), '[{"type": "tx", "channel": 71, "label": "x"}]')
            self.assertIn("here on this box (pid ", str(e.exception))
            self.assertIn("never force-ended", str(e.exception))
            # While it runs, no other run of the helper starts.
            with self.assertRaises(ip.Refused) as e:
                dante.helper(ip, self.cmd("read"), "")
            self.assertIn("still runs on this box", str(e.exception))
            self.assertEqual(self.runs(), ["write 192.0.2.10 Zyxqw-Iem-Card"])
            self.wait_for_end()
            # It ended by itself, its answer kept on this box; the next run goes ahead.
            self.assertIn('"stdin": "[{', (ip.STATE_DIR / dante.OUT).read_text(encoding="utf-8"))
            self.assertEqual(json.loads(dante.helper(ip, self.cmd("read"), ""))["verb"], "read")
        # A recorded process that is not the helper (its pid reused) does not hold a run back.
        ip.write_json(ip.STATE_DIR / dante.RECORD, {"pid": os.getpid()})
        self.assertEqual(json.loads(dante.helper(ip, self.cmd("read"), ""))["verb"], "read")


if __name__ == "__main__":
    unittest.main()
