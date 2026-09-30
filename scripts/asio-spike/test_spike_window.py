"""Tests for scripts/asio-spike/spike_window.py (pure parts and the event
guard; ssh is the PC)."""
from __future__ import annotations

import hashlib
import json
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import spike_window as sw  # noqa: E402

NUMERIC = {"PC_BUFFER_ORIGINAL": "64", "PC_NTRACK": "9", "PC_ACTIVITY_CHANNELS": "101-110,121-124"}
FULL = "\n".join(f"{k}=v" for k in sw.REQUIRED if k not in NUMERIC) + "\n" + "".join(f"{k}={v}\n" for k, v in NUMERIC.items())


def write(text: str, name: str = "asio-spike.env") -> Path:
    p = Path(tempfile.mkdtemp()) / name
    p.write_text(text, encoding="utf-8")
    return p


class EnvTests(unittest.TestCase):
    def test_complete_env_loads(self) -> None:
        env = sw.load_env(write(FULL.replace("PC_SSH=v", 'PC_SSH="u@h"') + "# comment\n"))
        self.assertEqual((env["PC_SSH"], env["PC_BUFFER_ORIGINAL"]), ("u@h", "64"))

    def test_missing_keys_are_named(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "missing PC_BUFFER_KEY"):
            sw.load_env(write(FULL.replace("PC_BUFFER_KEY=v\n", "")))

    def test_numbers_must_be_numbers(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "PC_BUFFER_ORIGINAL must be a whole number"):
            sw.load_env(write(FULL.replace("PC_BUFFER_ORIGINAL=64", "PC_BUFFER_ORIGINAL=sixty")))

    def test_the_guard_watches_the_configured_stage_inputs_or_explicitly_all(self) -> None:
        self.assertEqual(sw.load_env(write(FULL))["PC_ACTIVITY_CHANNELS"], "101-110,121-124")
        with self.assertRaisesRegex(sw.StepError, "missing PC_ACTIVITY_CHANNELS"):
            sw.load_env(write(FULL.replace("PC_ACTIVITY_CHANNELS=101-110,121-124\n", "")))
        self.assertEqual(sw.load_env(write(FULL.replace("=101-110,121-124", "=all")))["PC_ACTIVITY_CHANNELS"], "all")
        for bad in ("101-110, 121-124", "3;4", "0", "3-", "-3", "5-3", "All", "3,,4", "1-1025"):
            with self.assertRaisesRegex(sw.StepError, "PC_ACTIVITY_CHANNELS", msg=bad):
                sw.load_env(write(FULL.replace("=101-110,121-124", "=" + bad)))

    def test_missing_file(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "missing"):
            sw.load_env(Path(tempfile.mkdtemp()) / "absent.env")


class RequestTests(unittest.TestCase):
    def test_limits(self) -> None:
        sw.check_request("probe", None, 600, 0, 0, 5)
        sw.check_request("duplex", 32, 36000, 300, 8, 20)
        sw.check_request("hwlat", None, 30, 0, 0, 1, cpu=14, threshold_us=10)
        sw.check_request("duplex", 32, 600, 40, 4, 5, audio_cpus="14", stress_cpus="0,1,6-13")
        for bad in (("record", 32, 600, 0, 0, 5), ("duplex", 16, 600, 0, 0, 5), ("reopen", None, 600, 0, 0, 5),
                    ("duplex", 32, 0, 0, 0, 5), ("duplex", 32, 36001, 0, 0, 5), ("duplex", 32, 600, 301, 0, 5),
                    ("duplex", 32, 600, 0, 9, 5), ("reopen", 48, 600, 0, 0, 0), ("reopen", 48, 600, 0, 0, 21)):
            with self.assertRaises(sw.StepError, msg=str(bad)):
                sw.check_request(*bad)
        for kw in ({"cpu": None}, {"cpu": 64}, {"cpu": 3, "threshold_us": 0}, {"cpu": 3, "threshold_us": 1001}):
            with self.assertRaises(sw.StepError, msg=str(kw)):
                sw.check_request("hwlat", None, 30, 0, 0, 1, **kw)
        for bad in ("14;x", "a", "1,,2"):
            with self.assertRaises(sw.StepError, msg=bad):
                sw.check_request("duplex", 32, 600, 0, 0, 5, audio_cpus=bad)

    def test_stress_next_to_audio_cpus_names_its_own_cpus(self) -> None:
        # The spike refuses --stress with --audio-cpus but no --stress-cpus (the busy
        # threads would share the audio CPU, #32): refused here, before the PC.
        with self.assertRaisesRegex(sw.StepError, "--stress-cpus"):
            sw.check_request("duplex", 32, 600, 40, 4, 5, audio_cpus="14")
        sw.check_request("duplex", 32, 600, 40, 4, 5, audio_cpus="14", stress_cpus="0,1,6-13")
        sw.check_request("duplex", 32, 600, 40, 4, 5)                  # no audio CPU: the threads run anywhere
        sw.check_request("duplex", 32, 600, 40, 0, 5, audio_cpus="14")  # no stress

    def test_stress_and_audio_cpus_never_overlap(self) -> None:
        # Review m12, as the spike (lane F1): any overlap, with or without busy threads.
        for stress in (4, 0):
            with self.assertRaisesRegex(sw.StepError, r"overlap on processors \[13, 14\]", msg=str(stress)):
                sw.check_request("duplex", 32, 600, 40, stress, 5, audio_cpus="13-14", stress_cpus="6-13,14")
        sw.check_request("duplex", 32, 600, 40, 4, 5, audio_cpus="14", stress_cpus="6-13")

    def test_timeouts(self) -> None:
        self.assertEqual([sw.run_timeout("probe", 600, 5), sw.run_timeout("duplex", 600, 5), sw.run_timeout("reopen", 600, 5)], [60, 660, 210])
        self.assertEqual(sw.run_timeout("hwlat", 30, 1), 90)

    def test_the_request_carries_the_watched_inputs(self) -> None:
        env = {"PC_ASIO_DRIVER": "D", "PC_ASIO_MODULE": "M", "PC_ACTIVITY_CHANNELS": "101-110,121-124"}
        args = type("A", (), {"mode": "duplex", "frames": 64, "seconds": 20, "burn_us": 0, "stress": 0, "panic_at": 0, "cycles": 5})()
        self.assertEqual(sw.run_fields(env, args),
                         {"mode": "duplex", "driver": "D", "module": "M", "frames": 64, "seconds": 20, "burn_us": 0, "stress": 0,
                          "panic_at": 0, "cycles": 5, "cpu": -1, "threshold_us": 10, "audio_cpus": "", "stress_cpus": "",
                          "activity_channels": "101-110,121-124", "timeout": 80})
        args.mode, args.frames = "probe", None
        self.assertEqual((sw.run_fields(env, args)["frames"], sw.run_fields(env, args)["timeout"]), (0, 60))

    def test_request_hashtable_quotes_text_and_keeps_numbers(self) -> None:
        self.assertEqual(sw.ps_hashtable({"mode": "duplex", "driver": "It's a card", "frames": 32}),
                         "@{ mode = 'duplex'; driver = 'It''s a card'; frames = 32 }")
        self.assertEqual(sw.ps_hashtable({"flag": True}), "@{ flag = 'True' }")
        with self.assertRaises(sw.StepError):
            sw.ps_hashtable({"a; b": 1})


class UndoPlanTests(unittest.TestCase):
    def state(self, **kw) -> dict:
        s = {"card": "reaper", "pref_original": 64, "pref_current": None, "pref_restored": False}
        s.update(kw)
        return s

    def test_nothing_to_undo_before_the_switch(self) -> None:
        self.assertEqual(sw.undo_plan(self.state(), spike_running=False), [])

    def test_a_running_spike_is_stopped_restored_and_reaper_comes_back(self) -> None:
        self.assertEqual(sw.undo_plan(self.state(card="free", pref_current=32), spike_running=True),
                         ["stop-spike", "restore-buffer", "bring-back"])

    def test_a_half_done_switch_still_brings_reaper_back(self) -> None:
        self.assertEqual(sw.undo_plan(self.state(card="switching"), spike_running=False), ["stop-spike", "bring-back"])

    def test_a_free_card_is_always_stopped_a_spike_may_be_starting(self) -> None:
        # The task may not have started the spike yet when "ide event" comes.
        self.assertEqual(sw.undo_plan(self.state(card="free"), spike_running=False), ["stop-spike", "bring-back"])
        self.assertEqual(sw.undo_plan(self.state(), spike_running=True), ["stop-spike"])

    def test_any_recorded_buffer_write_is_restored_with_read_back(self) -> None:
        # set-buffer 32 succeeded, set-buffer 64 failed: the state says 64, the registry may hold 32.
        self.assertEqual(sw.undo_plan(self.state(card="free", pref_current=64), False),
                         ["stop-spike", "restore-buffer", "bring-back"])
        self.assertEqual(sw.undo_plan(self.state(card="free", pref_current=32, pref_restored=True), False),
                         ["stop-spike", "restore-buffer", "bring-back"])
        self.assertTrue(sw.buffer_touched(self.state(pref_current=64)))
        self.assertFalse(sw.buffer_touched(self.state()))

    def test_trace_and_tuning_unwind_before_the_buffer_and_reaper(self) -> None:
        state = {"card": "free", "pref_current": 32, "trace": "C:\\t\\runs\\x", "tuning_mode": True, "fingerprint": "/b.json"}
        self.assertEqual(sw.undo_plan(state, spike_running=True),
                         ["stop-spike", "trace-stop", "tuning-exit", "restore-buffer", "bring-back", "fingerprint"])

    def test_no_fingerprint_without_bringing_reaper_back(self) -> None:
        state = {"card": "reaper", "pref_current": None, "tuning_mode": True, "fingerprint": "/b.json"}
        self.assertEqual(sw.undo_plan(state, spike_running=False), ["tuning-exit"])

    def test_a_prepared_reboot_is_card_away(self) -> None:
        # reboot-prepare left the card free as `rebooting` (#32 B3): "ide event" (or
        # to-event) stops a spike and brings REAPER back. Its clean unwind already
        # restored the buffer with read-back, and after the reboot REAPER may hold the
        # driver, so the buffer is not written again (the bring-back reads it).
        state = self.state(card="rebooting", pref_current=64, pref_restored=True)
        self.assertEqual(sw.undo_plan(state, spike_running=False), ["stop-spike", "bring-back"])
        state["fingerprint"] = "/b.json"
        self.assertEqual(sw.undo_plan(state, spike_running=True), ["stop-spike", "bring-back", "fingerprint"])
        # A buffer write not verified as restored is still restored, whatever the card.
        self.assertIn("restore-buffer", sw.undo_plan(self.state(card="rebooting", pref_current=32), False))

    def test_flags_recorded_before_the_action(self) -> None:
        # tuning_window records the flag first; a flag alone (the action may have failed half-way) still unwinds.
        self.assertIn("trace-stop", sw.undo_plan({"card": "free", "trace": "d"}, spike_running=False))
        self.assertIn("tuning-exit", sw.undo_plan({"card": "free", "tuning_mode": True}, spike_running=False))
        self.assertNotIn("trace-stop", sw.undo_plan({"card": "free", "trace": None}, spike_running=False))


class UnwindTests(unittest.TestCase):
    """unwind() with the PC calls recorded instead of sent."""

    def setUp(self) -> None:
        d = Path(tempfile.mkdtemp())
        self.saved = (sw.STATE, sw.ps)
        sw.STATE = d / "spike-window.json"
        self.calls: list[str] = []
        self.gone = True

        def fake_ps(env, body, timeout=300, event="finish"):
            self.calls.append(body)
            if body.startswith("(Stop-SpikeGracefully"):
                return self.gone
            return {"ok": body.split(" ", 1)[0]}

        sw.ps = fake_ps
        self.env = {"PC_ROOT": "R", "PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_REAPER_HTTP": "H",
                    "PC_REAPER_START_TASK_PATH": "P", "PC_REAPER_START_TASK": "T", "PC_NTRACK": "9",
                    "PC_METER_BRIDGE": "B", "PC_METER_ACTION": "A", "PC_METER_HEARTBEAT": "HB",
                    "PC_ASIO_MODULE": "M", "PC_APP_HTTP": "AH"}

    def tearDown(self) -> None:
        sw.STATE, sw.ps = self.saved

    def state(self, **kw) -> dict:
        s = {"id": "w", "card": "free", "pref_original": 64, "pref_current": 64, "pref_restored": False, "closed": False}
        s.update(kw)
        return s

    def test_the_buffer_is_restored_and_checked_again_before_reaper(self) -> None:
        state = self.state()
        done = sw.unwind(self.env, state, running=False)
        self.assertEqual([next(iter(d)) for d in done], ["stop-spike", "restore-buffer", "bring-back"])
        self.assertIn("-Value 64 -Original 64", self.calls[1])
        self.assertTrue(self.calls[2].startswith("Invoke-SpikeBringBack "))
        self.assertIn("-BufferKey 'K' -BufferName 'N' -Original 64", self.calls[2])
        self.assertEqual((state["card"], state["closed"], state["pref_restored"]), ("reaper", True, True))

    def test_a_text_value_is_written_back_as_it_was(self) -> None:
        sw.unwind(self.env, self.state(preflight={"kind": "String", "raw": "064"}), running=False)
        self.assertIn("-Raw '064'", self.calls[1])
        self.assertIn("-Raw '064'", self.calls[2])

    def test_a_spike_that_does_not_stop_keeps_reaper_away(self) -> None:
        self.gone = False
        state = self.state()
        with self.assertRaisesRegex(sw.StepError, "did not stop"):
            sw.unwind(self.env, state, running=False)
        self.assertFalse(any(c.startswith("Invoke-SpikeBringBack") for c in self.calls))
        self.assertFalse(state["closed"])

    def test_a_spike_not_confirmed_gone_never_gets_the_buffer_written(self) -> None:
        # The driver may still be open: Set-SpikeBufferPref there is what set-buffer
        # refuses (#32 B10). The unwind alarms the owner and stops, with or without
        # the bring-back (reboot-prepare), and the window stays open.
        self.gone = False
        alarms: list[str] = []
        saved = sw.alarm
        sw.alarm = alarms.append
        try:
            for bring_back in (True, False):
                state = self.state()
                with self.assertRaisesRegex(sw.StepError, "did not stop"):
                    sw.unwind(self.env, state, running=True, bring_back_reaper=bring_back)
                self.assertFalse(state["closed"])
                self.assertFalse(state["pref_restored"])
        finally:
            sw.alarm = saved
        self.assertFalse(any("Set-SpikeBufferPref" in c for c in self.calls))
        self.assertEqual(len(alarms), 2)
        self.assertIn("did not stop", alarms[0])

    def test_a_prepared_reboot_window_closes_only_with_reaper_back(self) -> None:
        state = self.state(card="rebooting", pref_restored=True, reboot={"prepared_at": "t"})
        done = sw.unwind(self.env, state, running=False)
        self.assertEqual([next(iter(d)) for d in done], ["stop-spike", "bring-back"])
        self.assertFalse(any("Set-SpikeBufferPref" in c for c in self.calls))
        self.assertEqual((state["card"], state["closed"]), ("reaper", True))


class WindowLockTests(unittest.TestCase):
    """Decision A of the #32 review: one dev-box lock serialises every
    read-modify-write of the window state, every bring-back and every unwind
    across processes (threads stand in for them: flock is per open file)."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (sw.STATE, sw.EVENT_NOW, sw.ps, sw.alarm, getattr(sw, "LOCK_WAIT_S", None))
        sw.STATE, sw.EVENT_NOW = self.dir / "spike-window.json", self.dir / "EVENT-NOW"
        self.alarms: list[str] = []
        sw.alarm = self.alarms.append
        self.bring_backs = 0
        self.env = {"PC_ROOT": "R", "PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_REAPER_HTTP": "H",
                    "PC_REAPER_START_TASK_PATH": "P", "PC_REAPER_START_TASK": "T", "PC_NTRACK": "9",
                    "PC_METER_BRIDGE": "B", "PC_METER_ACTION": "A", "PC_METER_HEARTBEAT": "HB",
                    "PC_ASIO_MODULE": "M", "PC_APP_HTTP": "AH"}

        def fake_ps(env, body, timeout=300, event="finish"):
            if "Get-Process -Name asio_spike" in body:
                return 0
            if "Stop-SpikeGracefully" in body:
                return True
            if body.startswith("Invoke-SpikeBringBack"):
                self.bring_backs += 1           # each one triggers REAPER's meter bridge
                threading.Event().wait(0.3)     # a bring-back takes a while: the other one arrives meanwhile
                return {"asio": "reaper"}
            return {"ok": True}

        sw.ps = fake_ps

    def tearDown(self) -> None:
        sw.STATE, sw.EVENT_NOW, sw.ps, sw.alarm, sw.LOCK_WAIT_S = self.saved

    def test_concurrent_updates_never_lose_or_break_the_state(self) -> None:
        sw.save_state({"id": "w", "n": 0, "closed": False})
        errors: list[Exception] = []

        def bump() -> None:
            for _ in range(40):
                try:
                    sw.update_state(change=lambda st: st.update(n=st["n"] + 1))
                except Exception as e:  # noqa: BLE001 — any failure is the finding
                    errors.append(e)

        threads = [threading.Thread(target=bump) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(errors, [])
        self.assertEqual(sw.load_state()["n"], 160)
        self.assertEqual([p.name for p in self.dir.glob("*.tmp")], [])

    def test_two_preempts_bring_reaper_back_once(self) -> None:
        # Two bring-backs at once would trigger the meter bridge twice: a REAPER dialog (#9).
        sw.save_state({"id": "w", "card": "free", "pref_original": 64, "pref_current": None, "pref_restored": False,
                       "runs": [], "closed": False})
        threads = [threading.Thread(target=sw.cmd_preempt, args=(self.env,)) for _ in range(2)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(self.bring_backs, 1)
        self.assertEqual((sw.load_state()["card"], sw.load_state()["closed"]), ("reaper", True))

    def test_a_lock_that_stays_taken_alarms_and_fails(self) -> None:
        sw.save_state({"id": "w", "closed": False})
        sw.LOCK_WAIT_S = 0.3
        taken, release = threading.Event(), threading.Event()

        def hold() -> None:
            with sw.window_lock():
                taken.set()
                release.wait(5)

        holder = threading.Thread(target=hold)
        holder.start()
        taken.wait(5)
        try:
            with self.assertRaisesRegex(sw.StepError, "lock"):
                sw.update_state({"x": 1})
        finally:
            release.set()
            holder.join()
        self.assertTrue(any("lock" in a for a in self.alarms))


class UnwindTuningTests(unittest.TestCase):
    """The S1c unwind branches (trace-stop, tuning-exit, fingerprint) with the
    PC calls recorded instead of sent (fake ps). Pins the design note §5.2
    safety invariant: a failed trace-stop or tuning-exit alarms the owner but
    REAPER still comes back, and reboot-prepare (bring_back_reaper=False) leaves
    the card free and the window open."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (sw.STATE, sw.ps)
        sw.STATE = self.dir / "spike-window.json"
        self.baseline = self.dir / "baseline.json"
        self.baseline.write_text(json.dumps({"plan.active": "reaper", "affinity": "x"}), encoding="utf-8")
        self.fail: set[str] = set()   # PowerShell verbs whose body should raise
        self.calls: list[str] = []
        self.gone = True

        def fake_ps(env, body, timeout=300, event="finish"):
            self.calls.append(body)
            for verb in self.fail:
                if verb in body:
                    raise sw.StepError(f"{verb} failed")
            if "Stop-SpikeGracefully" in body:
                return self.gone
            if "Get-IemReaperFingerprint" in body:
                return {"plan.active": "spike", "affinity": "x"}   # differs from the baseline → alarm
            return {"ok": True}

        sw.ps = fake_ps
        self.env = {"PC_ROOT": "R", "PC_TUNING_ROOT": "T", "PC_XPERF": "xperf.exe",
                    "PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_REAPER_HTTP": "H",
                    "PC_REAPER_START_TASK_PATH": "P", "PC_REAPER_START_TASK": "TK", "PC_NTRACK": "9",
                    "PC_METER_BRIDGE": "B", "PC_METER_ACTION": "A", "PC_METER_HEARTBEAT": "HB",
                    "PC_ASIO_MODULE": "M", "PC_APP_HTTP": "AH"}

    def tearDown(self) -> None:
        sw.STATE, sw.ps = self.saved

    def state(self, **kw) -> dict:
        s = {"id": "w", "card": "free", "pref_original": 64, "pref_current": 64, "pref_restored": False,
             "trace": "C:\\t\\runs\\x", "tuning_mode": True, "fingerprint": str(self.baseline), "closed": False}
        s.update(kw)
        return s

    def test_the_full_unwind_reverts_trace_and_tuning_before_the_buffer_and_reaper(self) -> None:
        state = self.state()
        done = sw.unwind(self.env, state, running=True)
        self.assertEqual([next(iter(d)) for d in done],
                         ["stop-spike", "trace-stop", "tuning-exit", "restore-buffer", "bring-back", "fingerprint"])
        self.assertEqual((state["trace"], state["tuning_mode"], state["card"], state["closed"]), (None, False, "reaper", True))
        # trace and tuning were reverted before restore-buffer and the bring-back
        idx = {next(iter(d)): i for i, d in enumerate(done)}
        self.assertLess(idx["trace-stop"], idx["restore-buffer"])
        self.assertLess(idx["tuning-exit"], idx["bring-back"])

    def test_a_failed_trace_or_tuning_exit_alarms_but_reaper_still_comes_back(self) -> None:
        self.fail = {"Stop-IemTrace", "Exit-IemTuningMode"}
        state = self.state()
        done = sw.unwind(self.env, state, running=False)
        steps = {next(iter(d)): list(d.values())[0] for d in done}
        self.assertIn("error", steps["trace-stop"])
        self.assertIn("error", steps["tuning-exit"])
        self.assertTrue(any(c.startswith("Invoke-SpikeBringBack") for c in self.calls))   # REAPER still comes back
        self.assertEqual((state["card"], state["closed"]), ("reaper", True))

    def test_a_spike_not_gone_still_stops_the_trace_and_the_mode_but_not_the_buffer(self) -> None:
        # Neither touches the driver; the buffer write and REAPER wait for the spike (#32 B10).
        self.gone = False
        state = self.state()
        with self.assertRaisesRegex(sw.StepError, "did not stop"):
            sw.unwind(self.env, state, running=True)
        self.assertTrue(any("Stop-IemTrace" in c for c in self.calls))
        self.assertTrue(any("Exit-IemTuningMode" in c for c in self.calls))
        self.assertFalse(any("Set-SpikeBufferPref" in c or c.startswith("Invoke-SpikeBringBack") for c in self.calls))
        self.assertEqual((state["trace"], state["tuning_mode"], state["pref_restored"], state["closed"]), (None, False, False, False))

    def test_a_fingerprint_that_differs_is_recorded(self) -> None:
        state = self.state()
        done = sw.unwind(self.env, state, running=False)
        fp = [d["fingerprint"] for d in done if "fingerprint" in d][0]
        self.assertEqual([e["key"] for e in fp], ["plan.active"])   # baseline reaper vs current spike

    def test_reboot_prepare_leaves_the_window_open_and_reaper_off(self) -> None:
        state = self.state()
        done = sw.unwind(self.env, state, running=False, bring_back_reaper=False)
        steps = [next(iter(d)) for d in done]
        self.assertNotIn("bring-back", steps)
        self.assertNotIn("fingerprint", steps)
        self.assertFalse(any(c.startswith("Invoke-SpikeBringBack") for c in self.calls))
        self.assertEqual((state["card"], state.get("closed")), ("free", False))


class MainTests(unittest.TestCase):
    """main(): an error while the event flag exists still brings REAPER back."""

    def setUp(self) -> None:
        d = Path(tempfile.mkdtemp())
        self.flag = d / "EVENT-NOW"
        self.saved = (sw.EVENT_NOW, sw.load_env, sw.cmd_run, sw.cmd_new, sw.cmd_preempt)
        sw.EVENT_NOW = self.flag
        sw.load_env = lambda path: {}
        self.preempted = 0

        def fail(env, args):
            raise sw.StepError("ssh: connection reset")

        def preempt(env, args=None):
            self.preempted += 1

        sw.cmd_run, sw.cmd_new, sw.cmd_preempt = fail, fail, preempt

    def tearDown(self) -> None:
        sw.EVENT_NOW, sw.load_env, sw.cmd_run, sw.cmd_new, sw.cmd_preempt = self.saved

    def test_an_error_during_an_event_preempts(self) -> None:
        self.flag.touch()
        self.assertEqual(sw.main(["run", "--mode", "probe"]), 10)
        self.assertEqual(self.preempted, 1)

    def test_an_error_without_an_event_does_not(self) -> None:
        self.assertEqual(sw.main(["run", "--mode", "probe"]), 1)
        self.assertEqual(self.preempted, 0)

    def test_a_refused_new_window_touches_nothing(self) -> None:
        self.flag.touch()
        self.assertEqual(sw.main(["new", "--signal", "x"]), 1)
        self.assertEqual(self.preempted, 0)


class RunTests(unittest.TestCase):
    """cmd_run end to end with the PC faked at the ssh seam (sw.ps, sw.scp)
    and the real window state in a temp dir."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (sw.STATE, sw.EVENT_NOW, sw.ps, sw.scp, sw.POLL_S)
        sw.STATE, sw.EVENT_NOW, sw.POLL_S = self.dir / "spike-window.json", self.dir / "EVENT-NOW", 0
        self.report: dict = {"outcome": "done", "segments": [{"telemetry": {"callbacks": 10, "missed": 0, "overruns": 0, "position_gaps": 0}}]}
        self.exit = 0

        def fake_ps(env, body, timeout=300, event="finish"):
            if "Write-GoldenRequest" in body:
                return "spike-1"
            if ".progress.json" in body:
                return {"status": {"state": "exited", "results": [{"exit": self.exit}]}, "progress": None}
            raise AssertionError(f"unexpected PC call: {body}")

        def fake_scp(src, dst):
            Path(dst).write_text(json.dumps(self.report) if dst.endswith(".report.json") else "", encoding="utf-8")

        sw.ps, sw.scp = fake_ps, fake_scp
        self.env = {"PC_ROOT": "R", "PC_ROOT_SCP": "/R", "PC_SSH": "u@pc", "PC_ASIO_DRIVER": "D", "PC_ASIO_MODULE": "M",
                    "PC_ACTIVITY_CHANNELS": "101-110", "RAW_DIR": str(self.dir / "raw")}
        sw.save_state({"id": "w", "card": "free", "preflight": {"pref": 64}, "pref_original": 64, "pref_current": 32,
                       "pref_restored": False, "runs": [], "closed": False})

    def tearDown(self) -> None:
        sw.STATE, sw.EVENT_NOW, sw.ps, sw.scp, sw.POLL_S = self.saved

    def args(self, **kw):
        base = {"mode": "duplex", "frames": 32, "seconds": 60, "burn_us": 0, "stress": 0, "panic_at": 0, "cycles": 1,
                "cpu": None, "threshold_us": 10, "audio_cpus": "", "stress_cpus": ""}
        return type("Args", (), dict(base, **kw))()

    def test_a_done_outcome_is_a_result(self) -> None:
        r = sw.cmd_run(self.env, self.args())
        self.assertEqual((r["exit"], r["verdict"]["stable"]), (0, True))

    def test_an_error_outcome_is_a_failed_step_not_a_result(self) -> None:
        # The spike ends with outcome "error", exit 1 when it could not set up what
        # was asked (a stress thread or the hwlat scanner not placed, #32): nothing
        # it reports measured the requested setup.
        self.report = {"outcome": "error", "error": "stress thread 1 not placed: synthetic", "segments": []}
        self.exit = 1
        with self.assertRaisesRegex(sw.StepError, "failed.*stress thread 1 not placed"):
            sw.cmd_run(self.env, self.args(stress=2, audio_cpus="14", stress_cpus="0,1"))
        self.assertEqual(sw.load_state()["runs"][-1]["verdict"]["outcome"], "error")   # recorded, not used


class PreflightTests(unittest.TestCase):
    GOOD = {"pref": 64, "reaper": 1, "app": 1, "spike": 0, "holders": ["reaper.exe:6496"], "task": True, "files": 6}

    def test_good_state_passes(self) -> None:
        self.assertEqual(sw.preflight_problems(dict(self.GOOD), 64), [])
        self.assertEqual(sw.preflight_problems(dict(self.GOOD, holders=None), 64), [])

    def test_each_problem_is_named(self) -> None:
        for change, words in (({"pref": 32}, "preferred buffer is 32"), ({"reaper": 0}, "REAPER is not running"),
                              ({"app": 0}, "predecessor app"), ({"spike": 1}, "spike already runs"),
                              ({"holders": ["asio_spike.exe:1"]}, "holders"), ({"task": False}, "not registered"),
                              ({"files": 3}, "3 verified")):
            problems = sw.preflight_problems(dict(self.GOOD, **change), 64)
            self.assertEqual(len(problems), 1, change)
            self.assertIn(words, problems[0])

    def test_a_dev_time_window_needs_reaper_gone_and_the_module_free(self) -> None:
        free = dict(self.GOOD, reaper=0, holders=[])
        self.assertEqual(sw.preflight_problems(free, 64, dev_time=True), [])
        self.assertEqual(sw.preflight_problems(dict(free, holders=None), 64, dev_time=True), [])
        for change, words in (({"reaper": 1}, "REAPER runs"), ({"holders": ["reaper.exe:1"]}, "holders"),
                              ({"pref": 32}, "preferred buffer is 32"), ({"app": 0}, "predecessor app"),
                              ({"files": 3}, "3 verified")):
            problems = sw.preflight_problems(dict(free, **change), 64, dev_time=True)
            self.assertEqual(len(problems), 1, change)
            self.assertIn(words, problems[0])


class DevTimeWindowTests(unittest.TestCase):
    """A window opened after REAPER was already saved and quit ("event skončil")."""

    def setUp(self) -> None:
        d = Path(tempfile.mkdtemp())
        self.saved = (sw.STATE, sw.EVENT_NOW, sw.ps)
        sw.STATE, sw.EVENT_NOW = d / "spike-window.json", d / "EVENT-NOW"
        self.calls: list[str] = []
        self.pc = {"pref": 64, "kind": "DWord", "raw": "64", "reaper": 0, "app": 1, "spike": 0, "holders": [],
                   "task": True, "files": 6}

        def fake_ps(env, body, timeout=300, event="finish"):
            self.calls.append(body)
            return dict(self.pc)

        sw.ps = fake_ps
        self.env = {"PC_BUFFER_ORIGINAL": "64", "PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_ASIO_MODULE": "M",
                    "PC_ROOT": "R", "PC_APP_PROCESS": "A"}

    def tearDown(self) -> None:
        sw.STATE, sw.EVENT_NOW, sw.ps = self.saved

    def args(self, **kw):
        return type("Args", (), dict({"signal": "owner, 13:07: event skončil", "dev_time": True, "frames": 32}, **kw))()

    def test_the_card_is_free_from_the_start_so_an_event_brings_reaper_back(self) -> None:
        sw.cmd_new(self.env, self.args())
        state = sw.load_state()
        self.assertEqual((state["card"], state["dev_time"]), ("free", True))
        self.assertEqual(sw.undo_plan(state, spike_running=False), ["stop-spike", "bring-back"])

    def test_an_event_state_window_still_starts_with_reaper(self) -> None:
        sw.cmd_new(self.env, self.args(dev_time=False))
        self.assertEqual(sw.load_state()["card"], "reaper")
        self.assertFalse(sw.load_state()["dev_time"])

    def test_preflight_records_the_free_card_and_refuses_after_a_buffer_write(self) -> None:
        sw.cmd_new(self.env, self.args())
        sw.cmd_preflight(self.env, self.args())
        state = sw.load_state()
        self.assertEqual((state["card"], state["preflight"]["kind"]), ("free", "DWord"))
        state["pref_current"] = 32
        sw.save_state(state)
        with self.assertRaisesRegex(sw.StepError, "preflight belongs before"):
            sw.cmd_preflight(self.env, self.args())

    def test_a_running_reaper_stops_a_dev_time_preflight(self) -> None:
        self.pc["reaper"] = 1
        sw.cmd_new(self.env, self.args())
        with self.assertRaisesRegex(sw.StepError, "REAPER runs"):
            sw.cmd_preflight(self.env, self.args())
        self.assertNotIn("preflight", sw.load_state())

    def test_no_buffer_write_or_run_before_the_preflight(self) -> None:
        sw.cmd_new(self.env, self.args())
        with self.assertRaisesRegex(sw.StepError, "run preflight first"):
            sw.cmd_set_buffer(self.env, self.args())
        run = self.args(mode="probe", seconds=60, burn_us=0, stress=0, cycles=1, panic_at=0)
        with self.assertRaisesRegex(sw.StepError, "run preflight first"):
            sw.cmd_run(self.env, run)
        self.assertEqual(self.calls, [])


class BundleTests(unittest.TestCase):
    def bundle(self) -> Path:
        d = Path(tempfile.mkdtemp())
        lines = []
        for name in sw.BUNDLE_FILES:
            (d / name).write_bytes(name.encode())
            lines.append(f"{hashlib.sha256(name.encode()).hexdigest()}  {name}")
        (d / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")
        return d

    def test_a_complete_bundle_verifies(self) -> None:
        self.assertEqual(sw.verify_bundle(self.bundle()), sorted(sw.BUNDLE_FILES))

    def test_a_changed_file_fails(self) -> None:
        d = self.bundle()
        (d / "asio_spike.exe").write_bytes(b"other")
        with self.assertRaisesRegex(sw.StepError, "asio_spike.exe"):
            sw.verify_bundle(d)

    def test_a_missing_or_extra_entry_fails(self) -> None:
        d = self.bundle()
        text = (d / "SHA256SUMS").read_text(encoding="utf-8")
        (d / "SHA256SUMS").write_text("\n".join(text.splitlines()[1:]), encoding="utf-8")
        with self.assertRaisesRegex(sw.StepError, "expected"):
            sw.verify_bundle(d)

    def test_paths_and_bad_lines_are_refused(self) -> None:
        for bad in ("0" * 64 + "  ../x.exe", "0" * 64 + " one-space.exe", "xyz  a.exe"):
            with self.assertRaises(sw.StepError, msg=bad):
                sw.parse_sums(bad)
        self.assertEqual(sw.parse_sums("\n" + "a" * 64 + "  a.exe\n"), {"a.exe": "a" * 64})

    def test_only_a_green_dev_push_run_of_that_sha(self) -> None:
        runs = [
            {"databaseId": 1, "headSha": "s", "event": "pull_request", "headBranch": "dev", "conclusion": "success"},
            {"databaseId": 2, "headSha": "s", "event": "push", "headBranch": "dev", "conclusion": "failure"},
            {"databaseId": 3, "headSha": "t", "event": "push", "headBranch": "dev", "conclusion": "success"},
            {"databaseId": 4, "headSha": "s", "event": "push", "headBranch": "main", "conclusion": "success"},
            {"databaseId": 5, "headSha": "s", "event": "push", "headBranch": "dev", "conclusion": "success"},
        ]
        self.assertEqual(sw.pick_run(runs, "s"), 5)
        with self.assertRaises(sw.StepError):
            sw.pick_run(runs[:4], "s")


class VerdictTests(unittest.TestCase):
    def report(self, outcome: str = "done", **tel) -> dict:
        t = {"callbacks": 1000, "late": 2, "missed": 0, "overruns": 0, "position_gaps": 0,
             "messages": {"resets": 0}, "interval_us": {"p999": 410.0}}
        t.update(tel)
        return {"outcome": outcome, "segments": [{"telemetry": t}]}

    def test_a_clean_run_is_stable(self) -> None:
        v = sw.verdict(self.report())
        self.assertEqual((v["stable"], v["callbacks"], v["late"], v["interval_p999_us"]), (True, 1000, 2, 410.0))

    def test_any_missed_overrun_gap_reset_or_early_end_is_unstable(self) -> None:
        for r in (self.report(missed=1), self.report(overruns=1), self.report(position_gaps=1),
                  self.report(messages={"resets": 1}), self.report(messages={"overloads": 1}),
                  self.report(messages={"buffer_size_changes": 1}), self.report("stopped"),
                  {"outcome": "done", "segments": []}):
            self.assertFalse(sw.verdict(r)["stable"], r)

    def test_the_hot_inputs_and_the_watched_ones_are_reported(self) -> None:
        r = self.report("band-activity")
        r["activity_channels"] = [101, 102]
        r["loudest_inputs"] = [{"channel": 125, "index": 124, "dbfs": -2.4}]
        v = sw.verdict(r)
        self.assertEqual((v["activity_channels"], v["loudest_inputs"], v["stable"]),
                         ([101, 102], [{"channel": 125, "index": 124, "dbfs": -2.4}], False))
        v = sw.verdict(self.report())
        self.assertEqual((v["activity_channels"], v["loudest_inputs"]), (None, []))

    def test_segments_add_up(self) -> None:
        r = self.report()
        r["segments"].append({"telemetry": {"callbacks": 5, "missed": 1, "interval_us": {"p999": 900.0}}})
        r["segments"].append({"telemetry": None})
        v = sw.verdict(r)
        self.assertEqual((v["callbacks"], v["missed"], v["interval_p999_us"], v["stable"]), (1005, 1, 900.0, False))


SLOW_COPY = "import sys, time; open(sys.argv[2], 'w').write('partial'); time.sleep(20)"
QUICK_COPY = "import sys; open(sys.argv[2], 'w').write('done')"
FAILED_COPY = "import sys; sys.stderr.write('Permission denied'); sys.exit(1)"


class ScpTests(unittest.TestCase):
    """scp with a local command standing in for the copy over ssh (review M3):
    an analysis download is abandoned at once on "ide event", and no copy
    outlives its bound; the local copy is interrupted, never ended harder."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.flag = self.dir / "EVENT-NOW"
        self.saved = (sw.EVENT_NOW, sw.POLL_S, getattr(sw, "SCP", None), getattr(sw, "SCP_BOUND_S", None))
        sw.EVENT_NOW, sw.POLL_S = self.flag, 0.1
        self.dst = self.dir / "near.txt"

    def tearDown(self) -> None:
        sw.EVENT_NOW, sw.POLL_S, sw.SCP, sw.SCP_BOUND_S = self.saved

    def copier(self, code: str) -> None:
        sw.SCP = (sys.executable, "-c", code)

    def test_an_event_interrupts_an_abandonable_copy_at_once(self) -> None:
        self.copier(SLOW_COPY)
        threading.Timer(0.5, self.flag.touch).start()
        t = time.monotonic()
        with self.assertRaises(sw.EventNow):
            sw.scp("u@host.invalid:/C:/t/near.txt", str(self.dst), event="abandon")
        self.assertLess(time.monotonic() - t, 5)
        self.assertFalse(self.dst.exists())                    # no partial file is left

    def test_the_copy_hears_ctrl_c_even_when_this_process_ignores_it(self) -> None:
        # Review round 3, m3: an ignored SIGINT is inherited by the child, and the
        # interrupt would then never end the copy; scp starts with SIGINT's default.
        self.copier(SLOW_COPY)
        saved = signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            with mock.patch.object(sw.subprocess, "Popen", wraps=subprocess.Popen) as popen:
                threading.Timer(0.5, self.flag.touch).start()
                t = time.monotonic()
                with self.assertRaises(sw.EventNow):
                    sw.scp("u@host.invalid:/C:/t/near.txt", str(self.dst), event="abandon")
                self.assertLess(time.monotonic() - t, 5)
            restore = popen.call_args.kwargs["preexec_fn"]
            restore()
            self.assertEqual(signal.getsignal(signal.SIGINT), signal.SIG_DFL)
        finally:
            signal.signal(signal.SIGINT, saved)

    def test_a_plain_copy_runs_to_its_end_whatever_the_flag(self) -> None:
        self.flag.touch()
        self.copier(QUICK_COPY)
        sw.scp("u@host.invalid:/x", str(self.dst))
        self.assertEqual(self.dst.read_text(encoding="utf-8"), "done")

    def test_a_failed_copy_raises(self) -> None:
        self.copier(FAILED_COPY)
        with self.assertRaisesRegex(sw.StepError, "Permission denied"):
            sw.scp("u@host.invalid:/x", str(self.dst))

    def test_a_copy_past_its_bound_is_interrupted(self) -> None:
        self.copier(SLOW_COPY)
        sw.SCP_BOUND_S = 0.5
        with self.assertRaisesRegex(sw.StepError, "interrupted"):
            sw.scp("u@host.invalid:/x", str(self.dst))
        self.assertFalse(self.dst.exists())


class GuardTests(unittest.TestCase):
    """guarded() with local commands standing in for ssh."""

    def setUp(self) -> None:
        self.flag = Path(tempfile.mkdtemp()) / "EVENT-NOW"
        self.saved = (sw.EVENT_NOW, sw.POLL_S)
        sw.EVENT_NOW, sw.POLL_S = self.flag, 0.1

    def tearDown(self) -> None:
        sw.EVENT_NOW, sw.POLL_S = self.saved

    def py(self, code: str) -> list[str]:
        return [sys.executable, "-c", code]

    def test_output_and_stdin_pass_through(self) -> None:
        out = sw.guarded(self.py("import sys, time; time.sleep(0.3); print(sys.stdin.read().upper())"), "hello\n", 10, "finish")
        self.assertEqual(out.strip(), "HELLO")

    def test_a_failing_command_raises(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "exit 3"):
            sw.guarded(self.py("import sys; sys.exit(3)"), "", 10, "finish")

    def test_abandon_returns_within_a_poll(self) -> None:
        self.flag.touch()
        t = time.monotonic()
        with self.assertRaises(sw.EventNow):
            sw.guarded(self.py("import time; time.sleep(5)"), "", 10, "abandon")
        self.assertLess(time.monotonic() - t, 2.0)

    def test_finish_completes_the_call_first(self) -> None:
        self.flag.touch()
        t = time.monotonic()
        with self.assertRaises(sw.EventNow):
            sw.guarded(self.py("import time; time.sleep(0.5)"), "", 10, "finish")
        self.assertGreaterEqual(time.monotonic() - t, 0.5)

    def test_ignore_is_for_the_preemption_itself(self) -> None:
        self.flag.touch()
        self.assertEqual(sw.guarded(self.py("print('ok')"), "", 10, "ignore").strip(), "ok")

    def test_a_call_past_its_bound_is_reported_not_killed(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "never kill"):
            sw.guarded(self.py("import time; time.sleep(3)"), "", 0.3, "finish")

    def test_a_lost_session_or_an_overdue_call_is_no_reply(self) -> None:
        # Review m9: sent, fate unknown — distinct from a reply that reports an error.
        with self.assertRaises(sw.NoReply):
            sw.guarded(self.py("import sys; sys.exit(255)"), "", 10, "finish")
        with self.assertRaises(sw.NoReply):
            sw.guarded(self.py("import time; time.sleep(3)"), "", 0.3, "finish")

    def test_a_connection_that_never_opened_is_a_plain_error(self) -> None:
        # Review round 3, m7: ssh exits 255 before any session existed — nothing was sent.
        for msg in ("ssh: connect to host pc port 22: Connection refused", "ssh: connect to host pc port 22: Connection timed out",
                    "ssh: Could not resolve hostname pc: Name or service not known", "u@pc: Permission denied (publickey).",
                    "Host key verification failed."):
            with self.assertRaises(sw.StepError, msg=msg) as cm:
                sw.guarded(self.py(f"import sys; sys.stderr.write({msg!r}); sys.exit(255)"), "", 10, "finish")
            self.assertNotIsInstance(cm.exception, sw.NoReply, msg)

    def test_a_session_that_ended_after_the_command_was_sent_is_no_reply(self) -> None:
        for msg in ("Connection to pc closed by remote host.", "client_loop: send disconnect: Broken pipe",
                    "Read from remote host pc: Connection reset by peer"):
            with self.assertRaises(sw.NoReply, msg=msg):
                sw.guarded(self.py(f"import sys; sys.stderr.write({msg!r}); sys.exit(255)"), "", 10, "finish")

    def test_the_flag_file_is_the_event_signal(self) -> None:
        self.assertFalse(sw.event_now())
        self.flag.touch()
        self.assertTrue(sw.event_now())


class PsReplyTests(unittest.TestCase):
    """sw.ps over a faked sw.guarded (the ssh process): a reply that reports an
    error is a plain StepError, no reply is NoReply (review m9)."""

    def setUp(self) -> None:
        self.saved = sw.guarded
        self.out = ""
        sw.guarded = lambda cmd, stdin, timeout, event: self.out

    def tearDown(self) -> None:
        sw.guarded = self.saved

    def test_an_error_reply_is_no_lost_reply(self) -> None:
        self.out = json.dumps({"ok": False, "error": "Access is denied"}) + "\n"
        with self.assertRaisesRegex(sw.StepError, "Access is denied") as cm:
            sw.ps({"PC_ROOT": "R", "PC_SSH": "u@h"}, "x")
        self.assertNotIsInstance(cm.exception, sw.NoReply)

    def test_the_script_sent_is_ps_script(self) -> None:
        # One builder for what reaches the PC, so CI can run the same text (review m11).
        sent: list[str] = []
        sw.guarded = lambda cmd, stdin, timeout, event: sent.append(stdin) or json.dumps({"ok": True, "r": 1})
        self.assertEqual(sw.ps({"PC_ROOT": "R", "PC_SSH": "u@h"}, "Get-X"), 1)
        self.assertEqual(sent, [sw.ps_script("R", "Get-X") + "\n"])

    def test_no_or_a_cut_reply_is_no_reply(self) -> None:
        for out in ("", "\n", '{"ok": tr\n'):
            self.out = out
            with self.assertRaises(sw.NoReply, msg=repr(out)):
                sw.ps({"PC_ROOT": "R", "PC_SSH": "u@h"}, "x")


if __name__ == "__main__":
    unittest.main()
