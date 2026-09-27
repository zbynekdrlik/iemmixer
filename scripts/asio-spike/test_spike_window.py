"""Tests for scripts/asio-spike/spike_window.py (pure parts and the event
guard; ssh is the PC)."""
from __future__ import annotations

import hashlib
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import spike_window as sw  # noqa: E402

FULL = "\n".join(f"{k}=v" for k in sw.REQUIRED if k not in ("PC_BUFFER_ORIGINAL", "PC_NTRACK")) + "\nPC_BUFFER_ORIGINAL=64\nPC_NTRACK=9\n"


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

    def test_missing_file(self) -> None:
        with self.assertRaisesRegex(sw.StepError, "missing"):
            sw.load_env(Path(tempfile.mkdtemp()) / "absent.env")


class RequestTests(unittest.TestCase):
    def test_limits(self) -> None:
        sw.check_request("probe", None, 600, 0, 0, 5)
        sw.check_request("duplex", 32, 3600, 300, 8, 20)
        for bad in (("record", 32, 600, 0, 0, 5), ("duplex", 16, 600, 0, 0, 5), ("reopen", None, 600, 0, 0, 5),
                    ("duplex", 32, 0, 0, 0, 5), ("duplex", 32, 3601, 0, 0, 5), ("duplex", 32, 600, 301, 0, 5),
                    ("duplex", 32, 600, 0, 9, 5), ("reopen", 48, 600, 0, 0, 0), ("reopen", 48, 600, 0, 0, 21)):
            with self.assertRaises(sw.StepError, msg=str(bad)):
                sw.check_request(*bad)

    def test_timeouts(self) -> None:
        self.assertEqual([sw.run_timeout("probe", 600, 5), sw.run_timeout("duplex", 600, 5), sw.run_timeout("reopen", 600, 5)], [60, 660, 210])

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


class PreflightTests(unittest.TestCase):
    GOOD = {"pref": 64, "reaper": 1, "app": 1, "spike": 0, "holders": ["reaper.exe:6496"], "task": True, "files": 4}

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

    def test_segments_add_up(self) -> None:
        r = self.report()
        r["segments"].append({"telemetry": {"callbacks": 5, "missed": 1, "interval_us": {"p999": 900.0}}})
        r["segments"].append({"telemetry": None})
        v = sw.verdict(r)
        self.assertEqual((v["callbacks"], v["missed"], v["interval_p999_us"], v["stable"]), (1005, 1, 900.0, False))


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

    def test_the_flag_file_is_the_event_signal(self) -> None:
        self.assertFalse(sw.event_now())
        self.flag.touch()
        self.assertTrue(sw.event_now())


if __name__ == "__main__":
    unittest.main()
