"""Tests for `iempc event` on the guard's verdict (#10). They reuse
iempc_test_support's fakes (FakePc stands in for ssh); every value is synthetic.
A file of its own: test_iempc.py is over its size budget (#36)."""
from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from iempc_test_support import Base  # noqa: E402


class EventVerdictTests(Base):
    def test_an_event_switch_that_needs_the_owner_exits_non_zero(self) -> None:
        """#10 (2026-10-08): a switch to event whose REAPER handover failed ends
        `needs_owner`, never `done`; the guard answers ok false, iemmode exits 1,
        and so does iempc event, with the owner alarm (no --direct)."""
        reply = json.dumps({"ok": False, "mode": "event", "alarms": [],
                            "detail": "event: ended, needs the owner: ReaperHandover failed: REAPER "
                                      "does not run",
                            "last_switch": {"from": "dev", "to": "event", "ended_in": "event",
                                            "outcome": "needs_owner"}})
        self.pc.replies[("event",)] = (1, reply)
        code, docs, err = self.run_main("event")
        self.assertEqual((code, self.pc.calls), (1, [("iemmode.exe", ["event"], "ignore")]))
        self.assertEqual(docs[-1]["iemmode"], ["event"])
        self.assertIn("alarm the owner now", err)


if __name__ == "__main__":
    unittest.main()
