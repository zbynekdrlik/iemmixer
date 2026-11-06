"""Tests for scripts/iem-pc/iempc_event.py: `iempc event`, the flag and nothing
else (#86): no ssh to the box, whatever the state folder holds. Every value is
synthetic."""
from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from iempc_test_support import Base, ip  # noqa: E402

LEFT_ALONE = {"event": "the flag only: the box keeps serving the band, the PC is left alone"}


class FlagTests(Base):
    def test_every_dev_time_command_is_refused_with_the_flag(self) -> None:
        self.flag()
        for argv in (["gen3-backup"], ["gen3-drift"], ["gen3-gate", "hil"]):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("runs only in dev time", err, argv)
        self.assertEqual(self.gh.calls, [])

    def test_event_goes_through_with_the_flag(self) -> None:
        self.flag()
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, docs), (0, [LEFT_ALONE]))

    def test_event_writes_the_flag_and_calls_nothing(self) -> None:
        with mock.patch("subprocess.Popen", side_effect=AssertionError("no process: no ssh to the box or the PC")):
            code, docs, _ = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual(docs, [{"flag": str(ip.EVENT_NOW), "written": True}, LEFT_ALONE])
        self.assertRegex(ip.EVENT_NOW.read_text(encoding="utf-8"), r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d[+-]\d\d:\d\d\n$")

    def test_no_side_record_changes_the_event(self) -> None:
        # The old record may still lie in the state folder: nothing reads it.
        ip.state_dir()
        (ip.STATE_DIR / "gen3-side.json").write_text('{"side": 2}', encoding="utf-8")
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, docs[-1]), (0, LEFT_ALONE))

    def test_a_dry_run_writes_no_flag(self) -> None:
        code, docs, _ = self.run_main("event", "--dry-run")
        self.assertEqual((code, docs, ip.event_now()), (0, [LEFT_ALONE], False))

    def test_an_existing_flag_is_kept(self) -> None:
        self.flag()
        self.assertFalse(ip.ensure_flag())
        self.assertEqual(ip.EVENT_NOW.read_text(encoding="utf-8"), "2026-09-27T20:00:00+02:00\n")

    def test_a_flag_that_cannot_be_written_is_a_warning(self) -> None:
        blocker = self.tmp / "not-a-folder"
        blocker.write_text("a file where the flag's folder should be", encoding="utf-8")
        self.patch(EVENT_NOW=blocker / "EVENT-NOW")
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual({k: docs[0][k] for k in ("flag", "written")}, {"flag": str(ip.EVENT_NOW), "written": False})
        self.assertIn("File exists", docs[0]["error"])
        self.assertIn("was not written", err)


if __name__ == "__main__":
    unittest.main()
