"""Tests for the tuning window's PC changes (enter, exit, apply, undo) through
the real cmd_preempt, unwind and settle (F2 round 3 MAJOR and the review of
lane G2). Split from test_tuning_window.py, whose fixtures it imports (never
a TestCase, or discover would collect it twice); only sw.ps and the
owner-alarm printer are faked."""
from __future__ import annotations

import argparse
import signal
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tuning_window as tw  # noqa: E402
import pc_change as pcc  # noqa: E402  (scripts/asio-spike, on sys.path through tuning_window)
from test_tuning_window import ENV  # noqa: E402  (fixtures only)


class FakeClock:
    """`pc_change`'s `time`: `time()` moves only through `sleep()`."""

    def __init__(self, t: float) -> None:
        self.t = t

    def time(self) -> float:
        return self.t

    def sleep(self, seconds: float) -> None:
        self.t += seconds


class TuningChangeTests(unittest.TestCase):
    """F2 round 3, MAJOR, in the tuning window: enter, exit, apply and undo are
    PC changes with an intent (recorded under the lock before the call, cleared
    after it). They all write the tuning journal, so a preempt never runs its
    own tuning-exit while one is in flight (two journal writers at once lose
    entries); the late step runs the exit itself once its call is back, and an
    enter that lands after the window closed reverts its levers. Only sw.ps is
    faked; the real cmd_preempt, unwind and settle run."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm, tw.sw.POLL_S, pcc.SETTLE_S)
        tw.sw.STATE, tw.sw.EVENT_NOW = self.dir / "spike-window.json", self.dir / "EVENT-NOW"
        tw.sw.POLL_S, pcc.SETTLE_S = 0.05, 0.3
        self.alarms: list[str] = []
        tw.sw.alarm = self.alarms.append
        self.events: list[str] = []
        self.bodies: list[str] = []
        self.wait_for_close = ""        # a verb whose call returns only once a preempt closed the window
        self.refused = False            # that call then reports the PC's refusal on the stop file
        self.in_flight = threading.Event()

        def fake_ps(env, body, timeout=300, event="finish"):
            self.bodies.append(body)
            for verb in ("Enter-IemTuningMode", "Exit-IemTuningMode", "Invoke-IemTuningApply", "Undo-IemTuning"):
                if verb in body:
                    self.events.append(f"{verb}:start")
                    if verb == self.wait_for_close:
                        self.in_flight.set()
                        deadline = time.monotonic() + 5
                        while not tw.sw.load_state().get("closed"):
                            self.assertLess(time.monotonic(), deadline)
                            time.sleep(0.01)
                        if self.refused:
                            self.events.append(f"{verb}:refused")
                            raise tw.StepError(f"PC step failed: {tw.sw.STEP_REFUSED} (the spike stop file exists: synthetic)")
                    self.events.append(f"{verb}:end")
                    return [{"key": "plan:active", "action": "written", "error": None}]
            if body.endswith("@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count"):
                return 0   # alone, or after the preempt's stop file (#15)
            if "Stop-SpikeGracefully" in body:
                return True
            if "holders = @(Get-GoldenAsioHolders" in body:
                return {"reaper": 1, "holders": ["reaper.exe:42"]}
            return {"ok": True}   # the bring-back, the stop file's clean-up

        tw.sw.ps = fake_ps
        patch = mock.patch.object(tw.sw, "plain_ps", fake_ps, create=True)   # the preempt's first call (#15)
        patch.start()
        self.addCleanup(patch.stop)
        tw.sw.save_state({"id": "w", "card": "free", "preflight": {"pref": 64}, "pref_original": 64, "pref_current": None,
                          "pref_restored": False, "runs": [], "closed": False})
        self.env = dict(ENV)

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.alarm, tw.sw.POLL_S, pcc.SETTLE_S = self.saved

    def preempt_during(self, step) -> BaseException | None:
        out: dict = {}

        def run() -> None:
            try:
                step()
            except BaseException as e:  # noqa: BLE001 — the step's outcome is what the test reads
                out["error"] = e

        t = threading.Thread(target=run)
        t.start()
        self.assertTrue(self.in_flight.wait(5))
        (self.dir / "EVENT-NOW").touch()
        tw.sw.cmd_preempt(self.env)
        t.join(10)
        return out.get("error")

    def test_an_enter_that_lands_after_the_window_closed_reverts_its_levers(self) -> None:
        self.wait_for_close = "Enter-IemTuningMode"
        error = self.preempt_during(lambda: tw.cmd_enter(self.env, argparse.Namespace(only="plan,governor,placement", idle="default")))
        self.assertIsInstance(error, tw.sw.EventNow)
        # No exit while the enter wrote the journal; the late enter's own exit after it.
        self.assertEqual(self.events, ["Enter-IemTuningMode:start", "Enter-IemTuningMode:end",
                                       "Exit-IemTuningMode:start", "Exit-IemTuningMode:end"])
        st = tw.sw.load_state()
        self.assertEqual((st["closed"], st["tuning_mode"], st.get("in_flight")), (True, False, None))

    def test_an_apply_that_lands_after_the_window_closed_runs_the_deferred_exit_and_alarms(self) -> None:
        st = tw.sw.load_state()
        st["tuning_mode"] = True   # entered earlier in this window
        tw.sw.save_state(st)
        self.wait_for_close = "Invoke-IemTuningApply"
        error = self.preempt_during(lambda: tw.cmd_apply(self.env, argparse.Namespace(tier=2, only="")))
        self.assertIsInstance(error, tw.sw.EventNow)
        self.assertEqual(self.events, ["Invoke-IemTuningApply:start", "Invoke-IemTuningApply:end",
                                       "Exit-IemTuningMode:start", "Exit-IemTuningMode:end"])
        self.assertTrue(any("apply" in a and "pre-empted" in a for a in self.alarms), self.alarms)
        self.assertFalse(tw.sw.load_state()["tuning_mode"])

    # Review of lane G2, finding 8: a step the PC refused on the stop file changed
    # nothing, so its late handler neither alarms nor runs an exit it does not owe.
    def test_a_refused_apply_on_a_closed_window_alarms_nothing_and_exits_nothing(self) -> None:
        self.wait_for_close, self.refused = "Invoke-IemTuningApply", True
        error = self.preempt_during(lambda: tw.cmd_apply(self.env, argparse.Namespace(tier=2, only="")))
        self.assertIsInstance(error, tw.sw.EventNow)
        self.assertEqual(self.events, ["Invoke-IemTuningApply:start", "Invoke-IemTuningApply:refused"])
        self.assertFalse(any("apply" in a for a in self.alarms), self.alarms)

    def test_a_refused_enter_exits_only_a_mode_entered_before_it(self) -> None:
        self.wait_for_close, self.refused = "Enter-IemTuningMode", True
        args = argparse.Namespace(only="plan", idle="default")
        error = self.preempt_during(lambda: tw.cmd_enter(self.env, args))
        self.assertIsInstance(error, tw.sw.EventNow)
        self.assertEqual(self.events, ["Enter-IemTuningMode:start", "Enter-IemTuningMode:refused"])
        # Entered earlier in the window: the exit the preempt deferred is still owed.
        self.events.clear()
        self.in_flight.clear()
        tw.sw.save_state({"id": "w2", "card": "free", "preflight": {"pref": 64}, "pref_original": 64, "pref_current": None,
                          "pref_restored": False, "runs": [], "closed": False, "tuning_mode": True})
        (self.dir / "EVENT-NOW").unlink()
        error = self.preempt_during(lambda: tw.cmd_enter(self.env, args))
        self.assertEqual(self.events, ["Enter-IemTuningMode:start", "Enter-IemTuningMode:refused",
                                       "Exit-IemTuningMode:start", "Exit-IemTuningMode:end"])
        self.assertFalse(tw.sw.load_state()["tuning_mode"])

    def test_a_deferred_exit_whose_step_never_came_back_runs_after_the_settle(self) -> None:
        # Review of lane G2, finding 2 (MAJOR): the step's process died (a SIGTERM, a
        # Bash timeout) with its journal intent live; the preempt deferred its exit to
        # a late handler that never runs. Once the settle is over, the preempt exits
        # itself, alarms and clears the intent: no mode lever stays through the event.
        # On a fake clock that only the waits move: the intent is live when the
        # preempt reads it whatever this process's speed (a loaded run once took
        # longer than the 0.3 s bound before the preempt, so the intent read as
        # stale and the deferred-exit path never ran).
        clock = FakeClock(time.time())
        st = tw.sw.load_state()
        st.update(tuning_mode=True, in_flight={"step": "enter", "started": clock.time(), "bound_s": 0.3})
        tw.sw.save_state(st)
        (self.dir / "EVENT-NOW").touch()
        with mock.patch.object(pcc, "time", clock):
            tw.sw.cmd_preempt(self.env)
        self.assertEqual(self.events, ["Exit-IemTuningMode:start", "Exit-IemTuningMode:end"])
        st = tw.sw.load_state()
        self.assertEqual((st["closed"], st["tuning_mode"], st.get("in_flight")), (True, False, None))
        self.assertTrue(any("never came back" in a for a in self.alarms), self.alarms)

    def test_a_term_signal_ends_a_step_through_its_clean_up(self) -> None:
        # The other half of finding 2: SIGTERM and SIGHUP end a window command through
        # Python's exception path, so pc_change clears its intent (and runs a late
        # handler) instead of leaving it to the bound.
        saved = {s: signal.getsignal(s) for s in (signal.SIGTERM, signal.SIGHUP)}
        self.addCleanup(lambda: [signal.signal(s, h) for s, h in saved.items()])
        tw.sw.exit_on_signals()
        for s in (signal.SIGTERM, signal.SIGHUP):
            with self.assertRaises(SystemExit):
                signal.raise_signal(s)

    def test_every_tuning_change_refuses_on_the_pc_before_its_modules_load(self) -> None:
        # The stop file's check comes first: a step a preempt overtook compiles nothing
        # and changes nothing.
        tw.cmd_enter(self.env, argparse.Namespace(only="plan", idle="default"))
        tw.cmd_exit(self.env, argparse.Namespace())
        tw.cmd_apply(self.env, argparse.Namespace(tier=2, only=""))
        tw.cmd_undo(self.env, argparse.Namespace(tier=2, only=""))
        changes = [b for b in self.bodies if any(v in b for v in ("Enter-IemTuningMode", "Exit-IemTuningMode",
                                                                  "Invoke-IemTuningApply", "Undo-IemTuning"))]
        self.assertEqual(len(changes), 4)
        for body in changes:
            self.assertTrue(body.startswith(f"if (Test-Path -LiteralPath 'R\\queue\\stop') {{ throw '{tw.sw.STEP_REFUSED}"), body)
            self.assertLess(body.index("queue\\stop"), body.index("IemMeasure.psm1"))
        self.assertIsNone(tw.sw.load_state().get("in_flight"))


if __name__ == "__main__":
    unittest.main()
