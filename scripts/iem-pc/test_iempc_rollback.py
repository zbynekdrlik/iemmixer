"""Tests for scripts/iem-pc/iempc_rollback.py: `iempc rollback` (S8 lane 3,
#11), the lifecycle as the guard's status names it, the switch test's refusal
past the cutover, and the event path's owner signal (`iemmode event
--signal`) with its fall-back for an iemmode that does not know it. They reuse
iempc_test_support's fakes (FakePc stands in for ssh); every value is
synthetic."""
from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_rollback as rb  # noqa: E402
import iempc_switch as sw  # noqa: E402
import test_iempc_switch as tsw  # noqa: E402
from iempc_test_support import OK, SHA, Base, ip  # noqa: E402

GO = ("rollback",)
DRY = ("rollback", "--dry-run")
PROD = f"mode live; bundle {SHA}; prod since 1790000000: pin {SHA}, previous none"
DONE = f"rollback done: trial, event; {rb.ON_EXPORT}; the original project is kept as before-rollback-1790000100"


def reply(ok: bool, detail: str) -> tuple[int, str]:
    return (0 if ok else 1), json.dumps({"ok": ok, "mode": "event", "detail": detail, "alarms": []})


class LifecycleTests(unittest.TestCase):
    def test_the_guard_s_status_names_the_lifecycle(self) -> None:
        self.assertEqual(rb.lifecycle({"detail": f"mode event; bundle {SHA}"}), "trial")
        self.assertEqual(rb.lifecycle({"detail": PROD}), "prod")
        self.assertEqual(rb.lifecycle({"detail": "mode event; no bundle; rolling back to REAPER; rollback since 1 from "
                                                 f"the pin {SHA}: [Stop] done"}), "rolling_back")
        for bad in (None, "text", {}, {"detail": None}, {"detail": 7}):
            self.assertIsNone(rb.lifecycle(bad), bad)

    def test_the_phrases_are_the_guard_s(self) -> None:
        # The Rust texts these read (lifecycle::status, rollback::ON_EXPORT/ON_ORIGINAL).
        root = Path(__file__).resolve().parents[2] / "crates" / "iem-guard" / "src"
        lifecycle = (root / "lifecycle.rs").read_text(encoding="utf-8")
        rollback = (root / "rollback.rs").read_text(encoding="utf-8")
        self.assertIn('"prod since {}: pin {}, previous {}"', lifecycle)
        self.assertIn(f'Some("{rb.ROLLING_BACK}".to_owned())', lifecycle)
        self.assertIn(f'pub const ON_EXPORT: &str = "{rb.ON_EXPORT}";', rollback)
        self.assertIn(f'pub const ON_ORIGINAL: &str = "{rb.ON_ORIGINAL}";', rollback)


class RollbackTests(Base):
    def calls(self) -> list[tuple[list[str], str]]:
        return [(c[1], c[2]) for c in self.pc.calls]

    def test_the_command_is_dev_time_locked_and_talks_to_the_pc(self) -> None:
        spec = ip.COMMANDS["rollback"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))

    def test_the_guard_s_rollback_is_one_call_a_new_flag_abandons(self) -> None:
        self.pc.replies[GO] = reply(True, DONE)
        code, docs, err = self.run_main("rollback")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.calls(), [(list(GO), "abandon")])
        self.assertEqual((docs[-1]["rollback"], docs[-1]["reply"]["detail"]), ("done", DONE))

    def test_a_dry_run_asks_the_guard_only(self) -> None:
        self.pc.replies[DRY] = reply(True, f"dry run: rollback from the pin {SHA}: RollingBack saved; Stop")
        code, docs, err = self.run_main("rollback", "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.calls(), [(list(DRY), "abandon")])
        self.assertEqual(docs[-1]["rollback"], "dry-run")

    def test_a_refused_or_unfinished_rollback_fails_with_the_guard_s_words(self) -> None:
        for args, detail in ((DRY, "there is no cutover to roll back"), (GO, "rollback not finished: Autostarts: x")):
            self.pc.calls.clear()
            self.pc.replies[args] = reply(False, detail)
            code, docs, _ = self.run_main(*args)
            self.assertEqual((code, docs[-1]["rollback"], docs[-1]["reply"]["detail"]), (1, "failed", detail))

    def test_the_event_flag_and_an_open_window_refuse_it(self) -> None:
        self.open_window()
        code, _, err = self.run_main("rollback")
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertIn("'rollback' waits until 'iempc handover-s1a'", err)
        ip.SPIKE_STATE.unlink()
        self.flag()
        for argv in (("rollback",), ("rollback", "--dry-run")):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs, self.pc.calls), (1, [], []), argv)
            self.assertIn("runs only in dev time", err)

    def test_a_rollback_that_outlives_the_client_s_bound_says_it_goes_on(self) -> None:
        def slow():
            raise ip.StillRunning("iemmode.exe still running after 540 s")

        self.pc.replies[GO] = slow
        code, _, err = self.run_main("rollback")
        self.assertEqual(code, 1)
        self.assertIn("the guard's rollback goes on", err)

    def test_a_new_flag_during_the_rollback_runs_the_event_path(self) -> None:
        def flag_then_reply():
            self.flag()
            return reply(True, DONE)

        self.pc.replies[GO] = flag_then_reply
        code, docs, _ = self.run_main("rollback")
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.calls(), [(list(GO), "abandon"), (["event", "--signal"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})


class EventSignalTests(Base):
    OLD = (2, "", 'iemmode: unknown argument "--signal"\nusage: iemmode status')
    TRIAL = reply(True, f"mode event; bundle {SHA}")

    def test_ide_event_is_the_owner_s_signal(self) -> None:
        code, _, _ = self.run_main("event")
        self.assertEqual((code, [c[1] for c in self.pc.calls]), (0, [["event", "--signal"]]))

    def test_an_iemmode_that_does_not_know_the_signal_gets_the_plain_event_before_the_cutover(self) -> None:
        # An iemmode older than S8 lane 3 refuses the flag before any guard call (exit 2):
        # the guard's status says it is before the cutover, so the plain event is the same.
        self.pc.replies[("event", "--signal")] = self.OLD
        self.pc.replies[("status",)] = self.TRIAL
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual([c[1] for c in self.pc.calls], [["event", "--signal"], ["status"], ["event"]])
        self.assertEqual([d.get("iemmode") for d in docs[1:]], [["event", "--signal"], ["status"], ["event"]])

    def test_past_the_cutover_the_plain_event_is_never_sent(self) -> None:
        # An older iemmode with a prod guard: a plain event would be the button's rollback.
        for status in (reply(True, PROD), reply(True, "mode event; rolling back to REAPER"),
                       (0, json.dumps({"ok": True})), (1, "")):
            self.pc.calls.clear()
            self.pc.replies[("event", "--signal")] = self.OLD
            self.pc.replies[("status",)] = status
            code, _, err = self.run_main("event")
            self.assertEqual((code, [c[1] for c in self.pc.calls]), (1, [["event", "--signal"], ["status"]]), status)
            self.assertIn("a plain 'iemmode event' would be the rollback", err)
            self.assertIn("alarm the owner", err)

    def test_the_fall_back_keeps_the_direct_path(self) -> None:
        # The guard unreachable (its status too): the plain event, then --direct.
        self.pc.replies[("event", "--signal")] = self.OLD
        self.pc.replies[("status",)] = (4, OK)
        self.pc.replies[("event",)] = (4, OK)
        code, _, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual([c[1] for c in self.pc.calls],
                         [["event", "--signal"], ["status"], ["event"], ["event", "--direct"]])

    def test_another_usage_error_is_not_retried(self) -> None:
        self.pc.replies[("event", "--signal")] = (2, "", "iemmode: no command")
        code, _, _ = self.run_main("event")
        self.assertEqual((code, [c[1] for c in self.pc.calls]), (2, [["event", "--signal"]]))


class SwitchTestPastTheCutoverTests(unittest.TestCase):
    def test_the_switch_test_refuses_past_the_cutover(self) -> None:
        # In prod a plain `iemmode event` is the rollback: no event leg there.
        for detail in (PROD, f"mode dev; bundle {SHA}; rolling back to REAPER"):
            why = sw.switch_refusal(tsw.ready(detail=detail))
            self.assertEqual(why, "the PC is past the cutover: its event leg would be the rollback (a switch test is "
                                  "the trial's)", detail)
        self.assertIsNone(sw.switch_refusal(tsw.READY))


if __name__ == "__main__":
    unittest.main()
