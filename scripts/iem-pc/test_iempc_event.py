"""Tests for scripts/iem-pc/iempc_event.py: `iempc event`, the flag, the spike
preempt, the one budget, and its end on the guard's verdict (#10; split out of
test_iempc.py, #36). They reuse iempc_test_support's fakes (FakePc stands in
for ssh); every value is synthetic."""
from __future__ import annotations

import json
import sys
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from iempc_test_support import OK, SHA, Base, FakeClock, ip  # noqa: E402


class FlagTests(Base):
    def test_the_flag_refuses_dev_and_touches_nothing(self) -> None:
        self.flag()
        code, docs, err = self.run_main("dev", "--build", SHA)
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules, self.gh.calls), (1, [], [], [], []))
        self.assertIn("runs only in dev time", err)

    def test_every_dev_time_command_is_refused_with_the_flag(self) -> None:
        self.fetched()
        self.flag()
        before = len(self.gh.calls)
        for argv in (["install", "--sha", SHA], ["install", "--sha", SHA, "--first"], ["dispatch-hil", "--sha", SHA],
                     ["rehearse-teardown"], ["probe-task"], ["handover-s1a"], ["bootstrap", "Register-IemTasks"],
                     ["bootstrap", "Get-IemBootstrapState"], ["bootstrap", "Get-IemTunnelOrigin"],
                     ["bootstrap", "Test-IemServiceRight"], ["dev", "--dry-run"]):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("runs only in dev time", err, argv)
        self.assertEqual((self.pc.calls, self.pc.modules, self.pc.scps, len(self.gh.calls)), ([], [], [], before))

    def test_event_goes_through_with_the_flag(self) -> None:
        self.flag()
        code, docs, _ = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertNotIn("flag", docs[0])

    def test_event_writes_the_flag_before_anything_else(self) -> None:
        seen = []
        self.pc.replies[("event", "--signal")] = lambda: (seen.append(ip.event_now()), (0, OK))[1]
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, seen, docs[0]), (0, [True], {"flag": str(ip.EVENT_NOW), "written": True}))
        self.assertRegex(ip.EVENT_NOW.read_text(encoding="utf-8"), r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d[+-]\d\d:\d\d\n$")

    def test_an_existing_flag_is_kept(self) -> None:
        self.flag()
        self.assertFalse(ip.ensure_flag())
        self.assertEqual(ip.EVENT_NOW.read_text(encoding="utf-8"), "2026-09-27T20:00:00+02:00\n")

    def test_a_flag_that_cannot_be_written_never_stops_the_event_path(self) -> None:
        blocker = self.tmp / "not-a-folder"
        blocker.write_text("a file where the flag's folder should be", encoding="utf-8")
        self.patch(EVENT_NOW=blocker / "EVENT-NOW")
        self.open_window()
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual({k: docs[0][k] for k in ("flag", "written")}, {"flag": str(ip.EVENT_NOW), "written": False})
        self.assertIn("File exists", docs[0]["error"])
        self.assertEqual(self.spike_log.read_text(encoding="utf-8"), "preempt\n")
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertIn("was not written", err)
        self.assertNotIn("alarm the owner", err)


class EventTests(Base):
    def test_an_open_spike_window_is_preempted_before_iemmode(self) -> None:
        self.open_window()
        seen = []
        self.pc.replies[("event", "--signal")] = lambda: (seen.append(self.spike_log.is_file()), (0, OK))[1]
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, seen, self.spike_log.read_text(encoding="utf-8")), (0, [True], "preempt\n"))
        self.assertEqual(docs[1], {"spike_preempt": {"ok": True, "output": "preempted\n"}})

    def test_a_closed_or_absent_window_is_left_alone(self) -> None:
        for state in (None, {"closed": True}):
            if state is not None:
                self.open_window(**state)
            self.assertEqual(self.run_main("event")[0], 0)
        self.assertFalse(self.spike_log.exists())
        self.assertEqual(len(self.pc.calls), 2)

    def test_a_failed_spike_preempt_still_runs_iemmode_event(self) -> None:
        self.open_window()
        self.patch(SPIKE=self.write_spike(3))
        code, docs, err = self.run_main("event")
        self.assertEqual((code, docs[1]["spike_preempt"]["ok"], "running" in docs[1]["spike_preempt"]), (0, False, False))
        self.assertIn("exit 3", docs[1]["spike_preempt"]["error"])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertIn("spike preempt", err)

    # F2 round 3, m2 and decision 2: after a FAILED spike preempt the window is closed
    # under spike_window's lock first, so a window preempt another process still has
    # queued finds it closed and starts no second bring-back next to the guard's.
    def test_a_failed_spike_preempt_closes_the_window_before_iemmode_event(self) -> None:
        self.open_window()
        self.patch(SPIKE=self.write_spike(3))
        seen: list[dict] = []
        self.pc.replies[("event", "--signal")] = lambda: (seen.append(json.loads(ip.SPIKE_STATE.read_text(encoding="utf-8"))), (0, OK))[1]
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertEqual(len(seen), 1)
        self.assertTrue(seen[0]["closed"])
        self.assertIn("failed spike preempt", seen[0]["closed_by"]["by"])
        self.assertIn("exit 3", seen[0]["closed_by"]["error"])
        self.assertEqual(seen[0]["card"], "free")   # nothing else is claimed: the guard brings REAPER back
        self.assertIn({"spike_window": "closed", "after": "a failed spike preempt"}, docs)

    def test_a_change_still_in_flight_after_a_failed_preempt_is_named(self) -> None:
        # Review of lane G2, finding 5: no settle watches it on this path; the step's own
        # late handler and the guard take it from here, and the agent hears which step.
        intent = {"step": "set-buffer", "started": time.time(), "bound_s": 60}
        self.open_window(in_flight=intent)
        self.patch(SPIKE=self.write_spike(3))
        code, docs, err = self.run_main("event")
        self.assertEqual(code, 0, err)
        self.assertIn({"spike_window": "closed", "after": "a failed spike preempt", "in_flight": intent}, docs)
        self.assertIn("set-buffer is still in flight", err)

    def test_a_closed_window_whose_preempt_still_settles_gets_the_spike_preempt(self) -> None:
        # Review of lane G2, finding 1: a window process's own preempt closed the window
        # and still watches REAPER (settle, without the lock); `spike_window.py preempt`
        # waits for that watch, so iemmode event never runs next to its bring-back.
        self.open_window(card="reaper", closed=True, settling={"step": "to-dev", "until": time.time() + 60})
        seen = []
        self.pc.replies[("event", "--signal")] = lambda: (seen.append(self.spike_log.is_file()), (0, OK))[1]
        code, docs, _ = self.run_main("event")
        self.assertEqual((code, seen, self.spike_log.read_text(encoding="utf-8")), (0, [True], "preempt\n"))
        # A watch past its bound (its process died) is no window to pre-empt.
        self.spike_log.unlink()
        self.open_window(card="reaper", closed=True, settling={"step": "to-dev", "until": time.time() - 1})
        self.assertEqual(self.run_main("event")[0], 0)
        self.assertFalse(self.spike_log.exists())

    def test_no_iemmode_call_while_the_window_lock_stays_taken(self) -> None:
        # The lock wait fits the event budget: a window process that holds it (a
        # bring-back of its own?) is never raced by the guard's.
        self.open_window()
        self.patch(SPIKE=self.write_spike(3), EVENT_BUDGET_S=2.0, SWITCH_MIN_S=1.0)
        sw = ip.spike_module()
        saved = sw.STATE
        self.addCleanup(setattr, sw, "STATE", saved)
        sw.STATE = ip.SPIKE_STATE
        taken, release = threading.Event(), threading.Event()

        def hold() -> None:
            with sw.window_lock():
                taken.set()
                release.wait(10)

        holder = threading.Thread(target=hold)
        holder.start()
        taken.wait(5)
        try:
            code, _, err = self.run_main("event")
        finally:
            release.set()
            holder.join()
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertIn("window lock", err)
        self.assertIn("alarm the owner now", err)
        self.assertFalse(json.loads(ip.SPIKE_STATE.read_text(encoding="utf-8"))["closed"])

    def test_the_event_path_has_one_budget_that_fits_a_bash_call(self) -> None:
        self.assertLessEqual(ip.EVENT_BUDGET_S, 540)  # a Bash call ends at 10 min; the plan's waits stay within 9
        self.assertLessEqual(ip.SPIKE_SHARE_S + ip.SWITCH_MIN_S, ip.EVENT_BUDGET_S)
        self.pc.replies[("event", "--signal")] = (4, OK)
        self.assertEqual(self.run_main("event")[0], 0)
        first, direct = self.pc.timeouts
        self.assertLessEqual(first, ip.EVENT_BUDGET_S)
        self.assertGreater(first, ip.EVENT_BUDGET_S - 10)
        self.assertLess(direct, first)

    def test_iemmode_event_gets_what_the_spike_preempt_left(self) -> None:
        self.open_window()
        self.patch(SPIKE=self.write_spike(0, delay=0.4))
        self.assertEqual(self.run_main("event")[0], 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertLessEqual(self.pc.timeouts[0], ip.EVENT_BUDGET_S - 0.4)
        self.assertGreater(self.pc.timeouts[0], ip.EVENT_BUDGET_S - 10)

    def test_no_iemmode_call_while_the_spike_preempt_outlives_its_share(self) -> None:
        self.open_window()
        self.patch(SPIKE=self.write_spike(0, delay=3), SPIKE_SHARE_S=0.3)
        t = time.monotonic()
        code, docs, err = self.run_main("event")
        spike_still_runs = not self.spike_done.exists()
        self.assertTrue(spike_still_runs)
        self.assertLess(time.monotonic() - t, 2.0)
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertEqual((docs[1]["spike_preempt"]["ok"], docs[1]["spike_preempt"]["running"]), (False, True))
        self.assertIn("preempt still runs after its 0.3 s share", err)
        self.assertIn("alarm the owner now", err)
        self.wait_for_spike_end()
        self.assertEqual(self.pc.calls, [])

    def test_an_iemmode_call_never_starts_with_less_than_its_minimum(self) -> None:
        # On a fake clock that only the replies move: the branch never depends on
        # this process's own speed (a loaded run once ate the margin).
        clock = FakeClock()
        self.patch(EVENT_BUDGET_S=1.0, SWITCH_MIN_S=0.5)
        self.pc.replies[("event", "--signal")] = lambda: (clock.sleep(0.625), (4, OK))[1]
        with self.patched(event_clock=clock.now):
            code, _, err = self.run_main("event")
        self.assertEqual((code, [c[1] for c in self.pc.calls]), (1, [["event", "--signal"]]))
        self.assertIn("less than the 0.5 s an iemmode call gets: run 'iempc event' again", err)
        self.assertIn("alarm the owner now", err)
        self.pc.calls.clear()
        # Exactly the minimum left is enough: 1.0 - 0.5 = 0.5.
        self.pc.replies[("event", "--signal")] = lambda: (clock.sleep(0.5), (4, OK))[1]
        with self.patched(event_clock=clock.now):
            self.assertEqual(self.run_main("event")[0], 0)
        self.assertEqual([c[1] for c in self.pc.calls], [["event", "--signal"], ["event", "--signal", "--direct"]])

    def test_an_unreachable_guard_falls_back_to_direct(self) -> None:
        self.pc.replies[("event", "--signal")] = (4, json.dumps({"error": "guard unreachable"}))
        code, docs, _ = self.run_main("event")
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore"), ("iemmode.exe", ["event", "--signal", "--direct"], "ignore")])
        self.assertEqual([d.get("iemmode") for d in docs[1:]], [["event", "--signal"], ["event", "--signal", "--direct"]])

    def test_only_exit_4_falls_back(self) -> None:
        for exit_code in (1, 2, 3, 5, 70):
            self.pc.calls.clear()
            self.pc.replies[("event", "--signal")] = (exit_code, OK)
            code, _, err = self.run_main("event")
            self.assertEqual((code, self.pc.calls), (exit_code, [("iemmode.exe", ["event", "--signal"], "ignore")]), exit_code)
            self.assertIn("the event path did not complete", err)

    def test_a_failed_direct_event_is_an_owner_alarm(self) -> None:
        self.pc.replies[("event", "--signal")] = (4, OK)
        self.pc.replies[("event", "--signal", "--direct")] = (1, json.dumps({"error": "a guard runs; use the pipe"}))
        code, _, err = self.run_main("event")
        self.assertEqual((code, len(self.pc.calls)), (1, 2))
        self.assertIn("alarm the owner now", err)

    def test_an_unreachable_pc_is_an_owner_alarm(self) -> None:
        def down():
            raise ip.StepError("ssh failed (exit 255): no route")

        self.pc.replies[("event", "--signal")] = down
        code, _, err = self.run_main("event")
        self.assertEqual(code, 1)
        self.assertIn("no route", err)
        self.assertIn("alarm the owner now", err)

    def test_a_dry_run_writes_no_flag_and_preempts_nothing(self) -> None:
        self.open_window()
        self.pc.replies[("event", "--dry-run", "--signal")] = (4, OK)
        code, docs, err = self.run_main("event", "--dry-run")
        self.assertEqual((code, ip.event_now(), self.spike_log.exists()), (0, False, False))
        self.assertEqual(docs[0], {"spike_window": "open", "plan": "spike_window.py preempt"})
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--dry-run", "--signal"], "ignore"),
                                         ("iemmode.exe", ["event", "--dry-run", "--signal", "--direct"], "ignore")])
        self.assertNotIn("alarm the owner", err)

    def test_a_failed_dry_run_is_no_owner_alarm(self) -> None:
        def down():
            raise ip.StepError("ssh failed (exit 255): no route")

        self.pc.replies[("event", "--dry-run", "--signal")] = down
        code, _, err = self.run_main("event", "--dry-run")
        self.assertEqual((code, ip.event_now()), (1, False))
        self.assertIn("no route", err)
        self.assertNotIn("alarm the owner", err)



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
        self.pc.replies[("event", "--signal")] = (1, reply)
        code, docs, err = self.run_main("event")
        self.assertEqual((code, self.pc.calls), (1, [("iemmode.exe", ["event", "--signal"], "ignore")]))
        self.assertEqual(docs[-1]["iemmode"], ["event", "--signal"])
        self.assertIn("alarm the owner now", err)


if __name__ == "__main__":
    unittest.main()
