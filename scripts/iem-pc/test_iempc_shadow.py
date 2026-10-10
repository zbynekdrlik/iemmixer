"""Tests for scripts/iem-pc/iempc_shadow.py: `iempc shadow-report` (S8 lane
4, #11), the summary of the guard's shadow history (pure) and the command at
the subprocess boundary (iempc_test_support's FakePc stands in for ssh, a
fake scp writes the history). Every value is synthetic (P6)."""
from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_shadow as sh  # noqa: E402
from iempc_test_support import SHA, Base, ip  # noqa: E402

CLEAN = {"at": 1790000000000, "entry": "event→dev", "bundle": SHA, "import": "writes",
         "counts": {"tracks": 45, "sends": 268, "eqs": 44, "limiters": 10, "trims": 24},
         "site": [], "fit": 0, "state_from": "current", "state": [], "doubts": 0}


def diff(kind: str, ident: str, field: str) -> dict:
    return {"kind": kind, "id": ident, "field": field}


def line(**changes) -> str:
    doc = dict(CLEAN)
    doc.update(changes)
    return json.dumps(doc, ensure_ascii=False)


# A line of each shape the guard writes: clean, with differences, refused, an
# error, an unmappable project, nothing saved to compare with.
HISTORY = [
    line(),
    line(at=1790000000500, entry="event→live",
         state=[diff("mix", "member1", "volume"), diff("mix", "member2", "volume"),
                diff("level", "member1/mic1", "gain")]),
    line(at=1790000000100),
    line(at=1790000000200, entry="event→live", **{"import": "refuses_topology"},
         site=[diff("input", "mic1", "rx")], state=[diff("mix", "member1", "volume")]),
    json.dumps({"at": 1790000000300, "entry": "event→dev", "bundle": SHA, "error": "exit", "exit": 2,
                "why": "iem-migrate: zyxqwname.RPP: unreadable"}, ensure_ascii=False),
    json.dumps({"at": 1790000000400, "entry": "event→dev", "bundle": SHA, "import": "unmappable",
                "problems": 3}, ensure_ascii=False),
    line(at=1790000000050, state_from="none"),
    "",
    "not json",
    "[1, 2]",
]


class SummaryTests(unittest.TestCase):
    def test_counts_and_kinds_only(self) -> None:
        s = sh.summarise(HISTORY)
        self.assertEqual(s, {
            "entries": 7, "unreadable": 2,
            "by_entry": {"event→dev": 5, "event→live": 2},
            "clean": 2, "uncompared": 2,
            "imports": {"refuses_topology": 1, "unmappable": 1, "writes": 4},
            "errors": {"exit": 1},
            "with_differences": 2,
            "first": 1790000000000, "last": 1790000000500,
            "differences": {
                "site.input.rx": {"entries": 1, "total": 1},
                "state.level.gain": {"entries": 1, "total": 1},
                "state.mix.volume": {"entries": 2, "total": 3},
            },
        })
        text = json.dumps(s, ensure_ascii=False)
        for private in ("member1", "mic1", "zyxqwname", SHA):
            self.assertNotIn(private, text)

    def test_nothing_is_an_empty_summary(self) -> None:
        self.assertEqual(sh.summarise([]), {
            "entries": 0, "unreadable": 0, "by_entry": {}, "clean": 0, "uncompared": 0, "imports": {},
            "errors": {}, "with_differences": 0, "first": None, "last": None, "differences": {}})

    def test_clean_needs_a_write_no_difference_and_a_compared_state(self) -> None:
        for doc, clean in ((line(), True), (line(fit=1), False), (line(doubts=1), False),
                           (line(state_from="none"), False),
                           (line(state_from=None), False), (line(**{"import": "refuses_fit"}), False),
                           (line(site=[diff("mix", "member1", "tx")]), False),
                           (line(state=[diff("input", "mic1", "trim")]), False)):
            self.assertEqual(sh.summarise([doc])["clean"], int(clean), doc)

    def test_unexpected_text_never_reaches_the_output(self) -> None:
        odd = line(entry="zyxqwother", error="Zyxqw Name", at=True,
                   site=[diff("input", "mic1", "rx")])
        s = sh.summarise([odd, line(state=["member1", diff("Mix Member1", "m", "gain")])])
        self.assertEqual(s["by_entry"], {"?": 1, "event→dev": 1})
        self.assertEqual(s["errors"], {"?": 1})
        self.assertEqual((s["first"], s["last"]), (1790000000000, 1790000000000))
        self.assertEqual(s["differences"], {"state.?.?": {"entries": 1, "total": 1},
                                            "state.?.gain": {"entries": 1, "total": 1}})
        self.assertNotIn("Zyxqw", json.dumps(s))


class ShadowReportTests(Base):
    def setUp(self) -> None:
        super().setUp()
        self.history = "\n".join(HISTORY) + "\n"
        self.fetched: list[tuple[str, str, str]] = []

        def scp(src, dst, event):
            self.fetched.append((src, dst, event))
            Path(dst).write_text(self.history, encoding="utf-8")

        self.patch(scp=scp)

    def test_the_command_is_dev_time_read_only_and_talks_to_the_pc(self) -> None:
        spec = ip.COMMANDS["shadow-report"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, False))

    def test_it_reads_the_history_and_prints_the_summary(self) -> None:
        self.pc.texts["history.jsonl"] = True
        code, docs, err = self.run_main("shadow-report")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"shadow_report": sh.summarise(HISTORY)}])
        # One read-only look (Test-Path), one copy, both abandoned by a new flag.
        self.assertEqual(len(self.pc.modules), 1)
        script, event = self.pc.modules[0]
        self.assertIn("Test-Path -LiteralPath 'X:\\root\\shadow\\history.jsonl' -PathType Leaf", script)
        self.assertEqual(event, "abandon")
        self.assertEqual(len(self.fetched), 1)
        src, dst, event = self.fetched[0]
        self.assertEqual((src, event), ("tester@pc.test:/X:/root/shadow/history.jsonl", "abandon"))
        self.assertFalse(Path(dst).exists(), "the local copy was left behind")
        self.assertEqual(self.pc.calls, [], "no iemmode call")

    def test_no_history_yet_is_an_empty_summary(self) -> None:
        self.pc.texts["history.jsonl"] = False
        code, docs, err = self.run_main("shadow-report")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs, [{"shadow_report": sh.summarise([]), "history": "none"}])
        self.assertEqual(self.fetched, [])

    def test_a_copy_that_left_no_file_fails(self) -> None:
        self.pc.texts["history.jsonl"] = True
        self.patch(scp=lambda src, dst, event: None)
        code, docs, err = self.run_main("shadow-report")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("left no file", err)

    def test_the_event_flag_refuses_it(self) -> None:
        self.flag()
        code, docs, err = self.run_main("shadow-report")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("runs only in dev time", err)
        self.assertEqual((self.pc.modules, self.fetched), ([], []))


if __name__ == "__main__":
    unittest.main()
