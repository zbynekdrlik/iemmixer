"""Tests for scripts/iem-pc/iempc_dante.py: `iempc dante-labels` (#58 plan
Task 15, outcome B: the sheet). The roster and the site are the synthetic
config/test-roster.toml and config/test-site.toml (P6: synthetic people and
channel numbers); names with letters a label cannot hold are built from code
points, so this file stays ASCII."""
from __future__ import annotations

import json
import shutil
import sys
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
            (ROSTER, SITE.split("[engine]")[0], "[engine]"),
        ]
        for roster, site, words in cases:
            with self.assertRaises(dante.SheetError, msg=words) as e:
                rows(roster, site)
            self.assertIn(words, str(e.exception))


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
    TX and 128 RX channels that carry their default labels unless set."""

    def __init__(self, test: Base) -> None:
        self.test = test
        self.calls: list[tuple[list[str], str]] = []
        self.card = {"tx": {str(n): str(n) for n in range(1, 129)}, "rx": {str(n): str(n) for n in range(1, 129)}}
        self.deaf: set[tuple[str, int]] = set()
        self.refuse: str | None = None
        self.on_read = None
        self.count_off = 0   # a write answer that counts other than what it was given

    def __call__(self, _ip, cmd: list[str], stdin: str) -> str:
        self.calls.append((list(cmd), stdin))
        if self.refuse is not None:
            raise ip.StepError(self.refuse)
        if cmd[3] == "read":
            if self.on_read is not None:
                self.on_read()
            return "netaudio noise\n" + json.dumps({"name": NAME, "rx_count": len(self.card["rx"]),
                                                    "tx_count": len(self.card["tx"]), **self.card})
        changes = json.loads(stdin)
        for c in changes:
            if (c["type"], c["channel"]) not in self.deaf:
                self.card[c["type"]][str(c["channel"])] = c["label"]
        mismatches = [{"type": c["type"], "channel": c["channel"], "want": c["label"],
                       "got": self.card[c["type"]][str(c["channel"])]} for c in changes
                      if self.card[c["type"]][str(c["channel"])] != c["label"]]
        return json.dumps({"written": len(changes) + self.count_off, "mismatches": mismatches, **self.card})

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

    def test_apply_touches_only_the_iem_card(self) -> None:
        code, docs, err = self.apply()
        self.assertEqual(code, 0, err)
        # Two runs of the helper, a read and a write, each naming only the configured card.
        self.assertEqual(self.fake.verbs(), ["read", "write"])
        for cmd, _ in self.fake.calls:
            self.assertEqual(cmd, [str(self.python), "-I", str(dante.HELPER), cmd[3], HOST, NAME])
        # Only label changes, each one of the sheet's rows.
        written = self.fake.written()
        self.assertTrue(all(set(c) == {"type", "channel", "label"} for c in written))
        self.assertEqual(sorted((c["type"], c["channel"], c["label"]) for c in written),
                         sorted((r.kind, r.channel, r.label) for r in self.rows))
        out = docs[-1]["dante_apply"]
        self.assertEqual((out["changed"], out["unchanged"], out["mismatches"]), (len(self.rows), 0, []))
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
        self.assertEqual((docs[-1]["dante_apply"]["changed"], docs[-1]["dante_apply"]["resubscribe"]), (0, []))
        self.assertIn("nothing to change", docs[-1]["dante_apply"]["note"])

    def test_a_read_back_mismatch_exits_1(self) -> None:
        self.fake.deaf.add(("tx", 71))
        code, docs, err = self.apply()
        self.assertEqual(code, 1, err)
        self.assertEqual(docs[-1]["dante_apply"]["mismatches"],
                         [{"type": "tx", "channel": 71, "want": "MEMBER 1 inear L 71", "got": "71"}])
        # An answer that does not count what it was given is no answer either.
        self.fake.deaf.clear()
        self.fake.card["tx"]["72"] = "72"
        self.fake.count_off = -1
        code, docs, err = self.apply()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("answered no count of", err)

    def test_another_device_at_the_host_writes_nothing(self) -> None:
        self.fake.refuse = ("python failed (exit 1): dante_card: the device answering at the configured host is not "
                            "the expected one: nothing was written")
        code, docs, err = self.apply()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("not the expected one", err)
        self.assertEqual(self.fake.verbs(), ["read"])

    def test_a_channel_the_card_lacks_writes_nothing(self) -> None:
        self.fake.card["tx"] = {str(n): str(n) for n in range(1, 65)}
        code, docs, err = self.apply()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("the card has no tx channel 71 (nothing was written)", err)
        self.assertEqual(self.fake.verbs(), ["read"])

    def test_apply_refuses_while_event_now(self) -> None:
        """The plan's name: no label is written during an event. Dev time and
        locked (refused at the start), and the flag read again right before
        the write (one that came during the card's read)."""
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
        for key, bad, words in (("DANTE_IEM_HOST", "iem-card.example", "not an IP address"),
                                ("NETAUDIO_PYTHON", "python3", "not the full path of a file"),
                                ("NETAUDIO_PYTHON", str(self.tmp / "none"), "not the full path of a file")):
            ip.env_path().write_text(full.replace(f"{key}=", f"{key}={bad}\n#"), encoding="utf-8")
            code, docs, err = self.apply()
            self.assertEqual((code, docs), (1, []), bad)
            self.assertIn(words, err, bad)
            self.assertNotIn(bad, err)   # a value of the private env is never printed
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
        self.fake.refuse = "python failed (exit 1): dante_card: the card did not answer (TimeoutError)"
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster), "--before", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertIn("WARNING: the card's labels could not be read", err)
        self.assertEqual((docs[-1]["dante_labels"]["compared"], docs[-1]["dante_labels"]["resubscribe"]),
                         ("before", []))
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertIn("every TX row is listed", err)
        self.assertIsNone(docs[-1]["dante_labels"]["compared"])


if __name__ == "__main__":
    unittest.main()
