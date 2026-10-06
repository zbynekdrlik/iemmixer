"""Tests for scripts/asio-spike/pc_change.py through spike_window's window
steps and preempt (F2 round 3 MAJOR, m1 and the review of lane G2): intents,
the settle, the stop file. Split from test_spike_window.py; only spike_window's
ssh seam (sw.ps) and owner-alarm printer are faked."""
from __future__ import annotations

import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import spike_window as sw  # noqa: E402  (first: pc_change reaches the state and the PC through it)
import pc_change as pcc  # noqa: E402


ENV = {"PC_ROOT": "R", "PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_REAPER_HTTP": "H", "PC_MAIN_PROJECT": "P.rpp",
       "PC_REAPER_START_TASK_PATH": "P", "PC_REAPER_START_TASK": "T", "PC_NTRACK": "9", "PC_METER_BRIDGE": "B",
       "PC_METER_ACTION": "A", "PC_METER_HEARTBEAT": "HB", "PC_ASIO_MODULE": "M", "PC_APP_HTTP": "AH",
       "PC_ASIO_DRIVER": "D", "PC_ACTIVITY_CHANNELS": "101-110"}


class FakeSpikePc:
    """The PC at sw.ps for the window steps and the preempt: REAPER runs and
    holds the card (`reaper`), a bring-back starts it, a hook sees each body
    before it is answered (another thread may act meanwhile)."""

    def __init__(self) -> None:
        self.reaper = True
        self.pref = 64
        self.gone = True                # the spike and its task are gone after the stop wait
        self.calls: list[str] = []
        self.bring_backs = 0
        self.on_call = None
        self.refuse_stop_file = False   # the PC's stop file exists: a guarded step does not start

    def ps(self, env, body, timeout=300, event="finish"):
        self.calls.append(body)
        if self.on_call:
            self.on_call(body)
        if self.refuse_stop_file and body.startswith("if (Test-Path -LiteralPath 'R\\queue\\stop')"):
            raise sw.StepError(f"PC step failed: {sw.STEP_REFUSED} (synthetic)")
        if body == "@(Get-Process -Name asio_spike -ErrorAction SilentlyContinue).Count":
            return 0
        if "Stop-SpikeGracefully" in body:
            return self.gone
        if "Invoke-SpikeBringBack" in body:
            self.bring_backs += 1
            self.reaper = True
            return {"asio": "reaper"}
        if "holders = @(Get-GoldenAsioHolders" in body:
            return {"reaper": int(self.reaper), "holders": ["reaper.exe:42"] if self.reaper else []}
        if "Test-SpikeTaskBusy" in body and "Remove-Item" in body:
            return "removed"
        if "Get-GoldenMeterSamples" in body:
            return []
        if "Invoke-GoldenSaveQuit" in body:
            self.reaper = False
            return {"saved": True, "quit": True}
        if "Set-SpikeBufferPref" in body:
            return {"before": 64, "after": 64}
        if "Get-SpikeBufferPref" in body:
            return {"value": self.pref, "kind": "DWord", "raw": str(self.pref)}
        if "Write-GoldenRequest" in body:
            return "spike-1"
        return {"ok": True}


class PcChangeTests(unittest.TestCase):
    """F2 round 3, MAJOR: a step that changes the PC records its intent (step,
    started, bound) under the window lock BEFORE its ssh call and clears it
    after, on error too; the lock is never held across the call. A preempt
    that finds a change in flight brings REAPER back, then watches, bounded,
    that REAPER runs and holds the card, and brings it back again when the
    late step took it down. m1: a start refuses while the spike's stop file
    exists, and only the unwind that closes the window removes it."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (sw.STATE, sw.EVENT_NOW, sw.ps, sw.alarm, sw.POLL_S, pcc.SETTLE_S)
        sw.STATE, sw.EVENT_NOW = self.dir / "spike-window.json", self.dir / "EVENT-NOW"
        sw.POLL_S, pcc.SETTLE_S = 0.05, 0.3
        self.alarms: list[str] = []
        sw.alarm = self.alarms.append
        self.pc = FakeSpikePc()
        sw.ps = self.pc.ps

    def tearDown(self) -> None:
        sw.STATE, sw.EVENT_NOW, sw.ps, sw.alarm, sw.POLL_S, pcc.SETTLE_S = self.saved

    def window(self, **kw) -> None:
        sw.save_state({"id": "w", "card": "free", "preflight": {"pref": 64}, "pref_original": 64, "pref_current": None,
                       "pref_restored": False, "runs": [], "closed": False, **kw})

    def lock_is_free(self) -> bool:
        got: list[bool] = []

        def probe() -> None:
            try:
                with sw.window_lock():
                    got.append(True)
            except sw.StepError:
                got.append(False)

        saved = sw.LOCK_WAIT_S
        sw.LOCK_WAIT_S = 0.3
        try:
            t = threading.Thread(target=probe)
            t.start()
            t.join()
        finally:
            sw.LOCK_WAIT_S = saved
        return got[0]

    def wait_closed(self) -> None:
        deadline = time.monotonic() + 5
        while not sw.load_state().get("closed"):
            self.assertLess(time.monotonic(), deadline, "the preempt never closed the window")
            time.sleep(0.01)

    def run_in_thread(self, fn) -> tuple[threading.Thread, dict]:
        out: dict = {}

        def run() -> None:
            try:
                out["result"] = fn()
            except BaseException as e:  # noqa: BLE001 — the step's own outcome is what the test reads
                out["error"] = e

        t = threading.Thread(target=run)
        t.start()
        return t, out

    def test_the_intent_is_recorded_before_the_call_and_cleared_after_it_without_the_lock_held(self) -> None:
        self.window(card="reaper")
        seen: list[tuple] = []

        def during(body: str) -> None:
            if "Invoke-GoldenSaveQuit" in body:
                st = sw.load_state()
                seen.append((st["card"], (st.get("in_flight") or {}).get("step"), (st.get("in_flight") or {}).get("bound_s"),
                             self.lock_is_free()))

        self.pc.on_call = during
        sw.cmd_to_dev(ENV, None)
        self.assertEqual(seen, [("switching", "to-dev", sw.SAVE_QUIT_S, True)])
        st = sw.load_state()
        self.assertEqual((st["card"], st.get("in_flight")), ("free", None))

    def test_a_failed_call_clears_its_intent_and_keeps_what_it_recorded_before(self) -> None:
        self.window()
        real = self.pc.ps
        during: list = []

        def failing(env, body, timeout=300, event="finish"):
            if "Set-SpikeBufferPref" in body:
                during.append((sw.load_state().get("in_flight") or {}).get("step"))
                raise sw.NoReply("PC command failed (exit 255): Connection reset")
            return real(env, body, timeout, event)

        sw.ps = failing
        with self.assertRaises(sw.NoReply):
            sw.cmd_set_buffer(ENV, type("A", (), {"frames": 32})())
        self.assertEqual(during, ["set-buffer"])                                     # recorded during the call
        st = sw.load_state()
        self.assertEqual((st.get("in_flight"), st["pref_current"]), (None, 32))   # a preempt still restores the buffer

    def test_a_second_pc_change_waits_for_the_one_in_flight(self) -> None:
        self.window(in_flight={"step": "enter", "started": time.time(), "bound_s": 240})
        with self.assertRaisesRegex(sw.StepError, "in flight"):
            sw.cmd_set_buffer(ENV, type("A", (), {"frames": 32})())
        self.assertFalse(any("Set-SpikeBufferPref" in c for c in self.pc.calls))
        # An intent past its bound is stale (its process ended without clearing it).
        self.window(in_flight={"step": "enter", "started": time.time() - 3600, "bound_s": 240})
        sw.cmd_set_buffer(ENV, type("A", (), {"frames": 32})())
        self.assertTrue(any("Set-SpikeBufferPref" in c for c in self.pc.calls))

    def test_every_pc_change_starts_on_the_pc_only_without_the_stop_file(self) -> None:
        # m1 and the late step: a body sent before "ide event" changes nothing once a
        # preempt wrote the stop file; the start body never removes it.
        self.window(card="reaper")
        sw.cmd_to_dev(ENV, None)
        sw.cmd_set_buffer(ENV, type("A", (), {"frames": 32})())
        st = sw.load_state()
        st["pref_current"] = 32
        sw.save_state(st)
        args = type("A", (), {"mode": "probe", "frames": None, "seconds": 60, "burn_us": 0, "stress": 0, "panic_at": 0,
                              "cycles": 1, "cpu": None, "threshold_us": 10, "audio_cpus": "", "stress_cpus": ""})()

        def event_at_the_first_poll(body: str) -> None:
            if ".progress.json" in body:
                raise sw.EventNow()

        self.pc.on_call = event_at_the_first_poll
        with self.assertRaises(sw.EventNow):
            sw.cmd_run(ENV, args)
        guarded = [c for c in self.pc.calls if any(v in c for v in ("Invoke-GoldenSaveQuit", "Set-SpikeBufferPref -Key",
                                                                     "Write-GoldenRequest"))]
        self.assertEqual(len(guarded), 3)
        for body in guarded:
            self.assertTrue(body.startswith(f"if (Test-Path -LiteralPath 'R\\queue\\stop') {{ throw '{sw.STEP_REFUSED}"), body)
            self.assertNotIn("Remove-Item", body)

    def test_a_step_the_pc_refused_on_the_stop_file_is_the_event(self) -> None:
        self.window()
        self.pc.refuse_stop_file = True
        with self.assertRaisesRegex(sw.StepError, "stop file"):            # no flag: an unwind did not clean it up
            sw.cmd_set_buffer(ENV, type("A", (), {"frames": 32})())
        self.window()
        # A preempt wrote it; the dev box sees the flag once the call is back.
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if "Set-SpikeBufferPref" in body else None
        with self.assertRaises(sw.EventNow):
            sw.cmd_set_buffer(ENV, type("A", (), {"frames": 32})())
        self.assertIsNone(sw.load_state().get("in_flight"))

    def test_a_step_the_pc_refused_takes_back_what_it_recorded_before_its_call(self) -> None:
        # Review of lane G2, finding 3: nothing ran on the PC, so the fields the step
        # recorded with its intent go back (a "switching" card, a pref_current a
        # later unwind or reboot-prepare would act on).
        self.pc.refuse_stop_file = True
        self.window(card="reaper")
        with self.assertRaisesRegex(sw.StepError, "stop file"):
            sw.cmd_to_dev(ENV, None)
        self.assertEqual((sw.load_state()["card"], sw.load_state().get("in_flight")), ("reaper", None))
        self.window(pref_current=48, pref_restored=False)
        with self.assertRaisesRegex(sw.StepError, "stop file"):
            sw.cmd_set_buffer(ENV, type("A", (), {"frames": 32})())
        st = sw.load_state()
        self.assertEqual((st["pref_current"], st["pref_restored"], st.get("in_flight")), (48, False, None))

    def test_a_save_and_quit_that_lands_after_the_bring_back_is_settled(self) -> None:
        # The MAJOR: "ide event" during to-dev's save and quit. The preempt finds
        # REAPER still up, brings it back (checks only) and closes the window; the
        # quit lands after that. The settle watch sees REAPER gone and brings it
        # back again, so REAPER holds the card for the event.
        self.window(card="reaper")
        in_flight = threading.Event()
        real = self.pc.ps

        def save_quit(env, body, timeout=300, event="finish"):
            if "Invoke-GoldenSaveQuit" in body:
                in_flight.set()
                self.wait_closed()                 # the preempt ran meanwhile
            return real(env, body, timeout, event)

        sw.ps = save_quit
        step, out = self.run_in_thread(lambda: sw.cmd_to_dev(ENV, None))
        self.assertTrue(in_flight.wait(5))
        (self.dir / "EVENT-NOW").touch()
        sw.cmd_preempt(ENV)
        step.join(10)
        self.assertTrue(self.pc.reaper, "REAPER stayed down after the late quit")
        self.assertEqual(self.pc.bring_backs, 2)
        self.assertIsInstance(out.get("error"), sw.EventNow)
        st = sw.load_state()
        self.assertEqual((st["card"], st["closed"], st.get("in_flight")), ("reaper", True, None))
        self.assertTrue(any("brought back again" in a for a in self.alarms), self.alarms)
        # Review of lane G2, finding 1: the late quit runs while the settle still
        # watches; telling the owner to run the event path again would start the
        # guard's bring-back next to the settle's.
        self.assertFalse(any("event path again" in a for a in self.alarms), self.alarms)
        self.assertNotIn("settling", st)

    def test_the_settle_is_recorded_while_it_runs(self) -> None:
        # So another process's preempt (and iempc event) can wait for it.
        self.window(in_flight={"step": "to-dev", "started": time.time(), "bound_s": 0.2})
        seen: list = []
        self.pc.on_call = lambda body: seen.append(sw.load_state().get("settling")) if "holders = @(Get-GoldenAsioHolders" in body else None
        (self.dir / "EVENT-NOW").touch()
        sw.cmd_preempt(ENV)
        self.assertTrue(seen and all(s and s["step"] == "to-dev" and s["until"] > time.time() - 5 for s in seen), seen)
        self.assertNotIn("settling", sw.load_state())

    def test_a_preempt_that_finds_the_window_closed_waits_for_a_live_settle(self) -> None:
        # Review of lane G2, finding 1: a window process's own preempt closed the
        # window and settles without the lock; iempc's preempt must not return (and
        # let the guard's bring-back start) until that settle is over.
        self.window(card="reaper", closed=True, settling={"step": "to-dev", "until": time.time() + 5})

        def settle_ends() -> None:
            time.sleep(0.4)
            sw.update_state(change=lambda st: st.pop("settling", None))

        t = threading.Thread(target=settle_ends)
        start = time.monotonic()
        t.start()
        sw.cmd_preempt(ENV)
        waited = time.monotonic() - start
        t.join()
        self.assertGreaterEqual(waited, 0.35)
        self.assertLess(waited, 3)
        self.assertEqual(self.pc.calls, [])
        # A settle whose process died (its bound is over) holds nothing up.
        self.window(card="reaper", closed=True, settling={"step": "to-dev", "until": time.time() - 1})
        start = time.monotonic()
        sw.cmd_preempt(ENV)
        self.assertLess(time.monotonic() - start, 0.3)

    def test_the_settle_watch_is_bounded_by_the_intent(self) -> None:
        # A step whose process never clears its intent: the watch still ends, at
        # the latest SETTLE_S after the intent's own bound.
        self.window(in_flight={"step": "set-buffer", "started": time.time(), "bound_s": 0.5})
        t = time.monotonic()
        (self.dir / "EVENT-NOW").touch()
        sw.cmd_preempt(ENV)
        self.assertLess(time.monotonic() - t, 0.5 + pcc.SETTLE_S + 2)
        self.assertGreater(sum("holders = @(Get-GoldenAsioHolders" in c for c in self.pc.calls), 1)
        self.assertEqual(self.pc.bring_backs, 1)

    # Review of lane G2, finding 6: the watch costs an ssh session and a PowerShell
    # start per poll on the PC during the event, so REAPER is read only after a step
    # that can take it off the card; a journal step is waited out from the state alone.
    def test_a_step_that_cannot_take_reaper_down_is_waited_out_without_pc_reads(self) -> None:
        self.window(in_flight={"step": "apply", "started": time.time(), "bound_s": 0.2})
        (self.dir / "EVENT-NOW").touch()
        t = time.monotonic()
        sw.cmd_preempt(ENV)
        self.assertGreaterEqual(time.monotonic() - t, 0.2)          # waited for the step all the same
        self.assertFalse(any("holders = @(Get-GoldenAsioHolders" in c for c in self.pc.calls))

    def test_the_watch_bound_is_never_more_than_the_steps_own_bound_from_now(self) -> None:
        # A wall clock set back must not stretch the watch: the remaining bound is
        # taken between 0 and bound_s.
        self.window(in_flight={"step": "to-dev", "started": time.time() + 2, "bound_s": 0.2})
        (self.dir / "EVENT-NOW").touch()
        t = time.monotonic()
        sw.cmd_preempt(ENV)
        self.assertLess(time.monotonic() - t, 0.2 + pcc.SETTLE_S + 0.8)

    # Review of lane G2, finding 5: one failed read must not end the watch, and the
    # stop file's clean-up runs however the watch ends.
    def test_a_failed_reaper_read_during_the_settle_is_an_alarm_and_the_watch_goes_on(self) -> None:
        self.window(in_flight={"step": "to-dev", "started": time.time(), "bound_s": 0.2})
        real, reads = self.pc.ps, []

        def flaky(env, body, timeout=300, event="finish"):
            if "holders = @(Get-GoldenAsioHolders" in body:
                reads.append(body)
                if len(reads) == 1:
                    raise sw.NoReply("PC command failed (exit 255): Connection reset")
            return real(env, body, timeout, event)

        sw.ps = flaky
        (self.dir / "EVENT-NOW").touch()
        sw.cmd_preempt(ENV)
        self.assertGreater(len(reads), 1)
        self.assertTrue(any("could not be read" in a for a in self.alarms), self.alarms)
        self.assertTrue(any("Test-SpikeTaskBusy" in c and "Remove-Item" in c for c in self.pc.calls))

    def test_a_settle_that_never_saw_reaper_on_the_card_fails_after_the_clean_up(self) -> None:
        self.window(in_flight={"step": "to-dev", "started": time.time(), "bound_s": 0.2})
        real = self.pc.ps

        def unreadable(env, body, timeout=300, event="finish"):
            if "holders = @(Get-GoldenAsioHolders" in body:
                raise sw.NoReply("PC command failed (exit 255): Connection reset")
            return real(env, body, timeout, event)

        sw.ps = unreadable
        (self.dir / "EVENT-NOW").touch()
        with self.assertRaisesRegex(sw.StepError, "REAPER"):
            sw.cmd_preempt(ENV)   # exit 1: iempc event then lets the guard bring REAPER back
        self.assertTrue(any("Test-SpikeTaskBusy" in c and "Remove-Item" in c for c in self.pc.calls))
        self.assertNotIn("settling", sw.load_state())

    def test_only_the_closing_unwind_removes_the_stop_file(self) -> None:
        self.window()
        sw.cmd_preempt(ENV)
        cleanup = [i for i, c in enumerate(self.pc.calls) if "Test-SpikeTaskBusy" in c and "Remove-Item" in c]
        bring_back = [i for i, c in enumerate(self.pc.calls) if "Invoke-SpikeBringBack" in c]
        self.assertEqual(len(cleanup), 1)
        self.assertLess(bring_back[0], cleanup[0])
        self.assertIn("'R\\queue\\stop'", self.pc.calls[cleanup[0]])
        # An unwind that does not close the window keeps it (a spike may still see it).
        self.pc.calls.clear()
        self.window()
        sw.unwind(ENV, sw.load_state(), running=False, bring_back_reaper=False)
        self.assertFalse(any("Remove-Item" in c for c in self.pc.calls))
        self.pc.calls.clear()
        self.pc.gone = False
        self.window()
        with self.assertRaisesRegex(sw.StepError, "did not stop"):
            sw.cmd_preempt(ENV)
        self.assertFalse(any("Remove-Item" in c for c in self.pc.calls))


if __name__ == "__main__":
    unittest.main()
