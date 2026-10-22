"""Tests for scripts/iem-pc/iempc_dante.py: `iempc dante-labels` (#58 plan
Task 15, outcome B: the sheet). The roster and the site are the synthetic
config/test-roster.toml and config/test-site.toml (P6: synthetic people and
channel numbers); names with letters a label cannot hold are built from code
points, so this file stays ASCII."""
from __future__ import annotations

import shutil
import sys
import tomllib
import unittest
from pathlib import Path

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

    def test_renamed_tx_channels_are_listed_for_resubscription(self) -> None:
        after = rows(changed(ROSTER, ('name = "Member 2"', 'name = "Member Two"'),
                             ('member5 = "member5"', 'member5 = "member10"'),
                             ('mic3 = { name = "MEMBER3 mic", owner = "member3" }',
                              'mic3 = { name = "MEMBER3 mic", owner = "member2" }'),
                             ('member4 = "member11"', 'member4 = "member9"')))
        sheet = dante.sheet(after, rows())
        self.assertEqual(sheet["resubscribe"], [
            {"channel": 73, "label": "MEMBER TWO inear L 73", "before": "MEMBER 2 inear L 73"},
            {"channel": 74, "label": "MEMBER TWO inear R 74", "before": "MEMBER 2 inear R 74"},
            {"channel": 79, "label": "MEMBER 10 inear L 79", "before": "MEMBER 5 inear L 79"},
            {"channel": 80, "label": "MEMBER 10 inear R 80", "before": "MEMBER 5 inear R 80"},
        ])
        self.assertTrue(sheet["compared"])
        # An RX rename keeps its subscription: on the sheet, never in the list.
        self.assertIn({"channel": 103, "label": "103 MEMBER TWO mic", "input": "mic3", "person": "member2"},
                      sheet["rx"])
        # The same roster renames nothing.
        self.assertEqual(dante.sheet(rows(), rows())["resubscribe"], [])
        # Without the roster the card follows now, its labels are not known: every TX row is listed.
        unknown = dante.sheet(after, None)
        self.assertFalse(unknown["compared"])
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

    def test_apply_refuses_while_event_now(self) -> None:
        # The plan's name; outcome B (#62) built no --apply. The sheet is what the owner applies by
        # hand, so the command is dev time and locked: no sheet, and no rename, during an event.
        spec = ip.COMMANDS["dante-labels"]
        self.assertEqual((spec.fn, spec.pc, spec.dev_time, spec.locked), (ip.cmd_dante_labels, False, True, True))
        self.flag()
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster))
        self.assertEqual((code, docs), (1, []))
        self.assertIn("runs only in dev time", err)

    def test_the_sheet_comes_from_the_site_beside_the_roster_and_touches_nothing(self) -> None:
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"dante_labels": dante.sheet(rows(), None)}])
        self.assertEqual((self.pc.calls, self.pc.modules, self.pc.scps, self.gh.named("workflow")), ([], [], [], []))
        other = self.tmp / "elsewhere.toml"
        shutil.copy(CONFIG / "test-site.toml", other)
        self.roster.with_name("site.toml").unlink()
        code, docs, err = self.run_main("dante-labels", "--roster", str(self.roster), "--site", str(other),
                                        "--before", str(self.roster))
        self.assertEqual(code, 0, err)
        self.assertEqual((docs[0]["dante_labels"]["resubscribe"], docs[0]["dante_labels"]["compared"]), ([], True))

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


if __name__ == "__main__":
    unittest.main()
