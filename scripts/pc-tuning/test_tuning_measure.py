"""Tests for scripts/pc-tuning/tuning_window.py: measure and hwlat through the
real spike_window run loop, the PC faked at the ssh seam (FakePc). Split from
test_tuning_window.py, whose profile and env fixtures it shares."""
from __future__ import annotations

import argparse
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_latency_report as tlr  # noqa: E402  (the xperf fixtures)
import tuning_window as tw  # noqa: E402
from test_tuning_window import ENV, PROFILE, write  # noqa: E402  (fixtures only, never a TestCase)


class FakeClock:
    """spike_window's clock inside a test: each reading moves 5 s on, sleeps
    return at once (cmd_run polls the PC every 10 s of this clock)."""

    def __init__(self) -> None:
        self.t = 0.0

    def monotonic(self) -> float:
        self.t += 5
        return self.t

    def sleep(self, seconds: float) -> None:
        pass


def hwlat_report(cpu: int, **change) -> dict:
    h = {"cpu": cpu, "threshold_us": 10, "placed": [256 + cpu], "priority": "time-critical", "reads": 1000, "over": 2,
         "gaps_us": {"p50": 11.0, "p99": 15.0, "p999": 20.0, "max": 31.5}, "largest": [{"at_us": 5.0, "gap_us": 31.5}]}
    return {"outcome": "done", "hwlat": {**h, **change}}


DUPLEX = {"outcome": "done", "segments": [{"telemetry": {"callbacks": 1000, "late": 0, "missed": 0, "overruns": 0, "position_gaps": 0,
                                                         "callback_cpus": {"14": 1000}, "callback_thread": 4243}}]}
# The spike's exit code per outcome (examples/asio_spike/main.rs code_of; "done"/"stopped" 0).
EXIT_CODES = {"error": 1, "refused": 4, "band-activity": 5, "fault-caught": 6, "rate-changed": 7, "stop-hung": 8}
# Outcomes that end a run without a completed measurement.
NOT_MEASURED = (("refused", 4), ("band-activity", 5), ("fault-caught", 6), ("rate-changed", 7), ("stop-hung", 8))


class FakePc:
    """The IEM PC at the ssh seam: `ps` answers sw.ps by the PowerShell verb
    and records (body, event); `scp` writes the file the PC would hold and
    records (name, event). Both keep sw.guarded's "ide event" contract: a
    call that is not event="ignore" raises EventNow when the flag exists once
    it has ended (the hooks may create it mid-call). Each spike run is
    running on its first status poll and exited on the next."""

    def __init__(self) -> None:
        self.calls: list[tuple[str, str]] = []
        self.copies: list[tuple[str, str]] = []
        self.reports: list[dict] = []          # one spike report per run, in order
        self.progress: dict = {"missed": 0, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        self.fail: set[str] = set()            # PowerShell verbs that fail on the PC
        self.dpcisr = tlr.DPCISR_XPERF         # what every dpcisr.txt holds
        self.runs = 0
        self.polls = 0
        self.on_call = None                    # a hook: (body) -> None, called before answering
        self.on_copy = None                    # a hook: (name) -> None, called during a copy
        # Stop-IemTraceSessions' reply: what it stopped, and no session kept.
        self.stop_reply = {"stopped": ["NT Kernel Logger", "IemMarkers"], "kept": [], "via": "logman"}

    def bodies(self, verb: str) -> list[str]:
        return [b for b, _ in self.calls if verb in b]

    def ps(self, env, body, timeout=300, event="finish"):
        answer = self.answer(body, event)
        if event != "ignore" and tw.sw.event_now():
            raise tw.sw.EventNow()
        return answer

    def answer(self, body, event):
        self.calls.append((body, event))
        if self.on_call:
            self.on_call(body)
        for verb in self.fail:
            if verb in body:
                raise tw.StepError(f"PC step failed: {verb} (synthetic)")
        if "Write-GoldenRequest" in body:
            self.runs += 1
            self.polls = 0
            return f"spike-{self.runs}"
        if ".progress.json" in body:
            self.polls += 1
            if self.polls == 1:
                return {"status": {"state": "running", "results": [{"pid": 4242}]}, "progress": self.progress}
            code = EXIT_CODES.get(self.reports[self.runs - 1]["outcome"], 0)
            return {"status": {"state": "exited", "results": [{"exit": code}]}, "progress": self.progress}
        if "Get-IemNow" in body:
            return "2026-01-01T00:00:00Z"
        if "Get-IemSystemEvents" in body:
            return []
        if "Win32_PerfRawData_PerfOS_Processor" in body or "Get-IemPollSample" in body:
            return {"at": "2026-01-01T00:00:10Z", "cpu": {"cpus": []}, "plan": "p", "governor": "Stopped",
                    "thread": {"base": 15, "current": 26}}
        if "Stop-IemTrace" in body:
            return self.stop_reply
        return {"ok": True}   # Start-IemTrace, Invoke-IemDpcIsr, Export-IemNearGlitch

    def scp(self, src: str, dst: str, event: str = "ignore") -> None:
        name = src.rsplit("/", 1)[-1]
        self.copies.append((name, event))
        if self.on_copy:
            self.on_copy(name)
        if event == "abandon" and tw.sw.event_now():
            raise tw.sw.EventNow()   # the copy is interrupted, nothing is written
        if name.endswith(".report.json"):
            text = json.dumps(self.reports[self.runs - 1])
        elif name.endswith(".stderr.txt"):
            text = ""
        elif name.endswith("dpcisr.txt"):
            text = self.dpcisr
        elif name.endswith("near.txt"):
            text = tlr.DUMPER
        else:
            raise AssertionError(f"unexpected copy: {src}")
        Path(dst).write_text(text, encoding="utf-8")


class WindowHarness(unittest.TestCase):
    """A dev-time window with a free card at buffer 32 and the real spike_window
    run loop; only the ssh seam (sw.ps, sw.scp) is faked and spike_window's
    clock runs fast."""

    def setUp(self) -> None:
        self.dir = Path(tempfile.mkdtemp())
        self.saved = (tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.scp, tw.sw.alarm, tw.PROFILE)
        tw.sw.STATE, tw.sw.EVENT_NOW = self.dir / "spike-window.json", self.dir / "EVENT-NOW"
        tw.PROFILE = write(PROFILE)
        self.pc = FakePc()
        tw.sw.ps, tw.sw.scp = self.pc.ps, self.pc.scp
        self.alarms: list[str] = []
        tw.sw.alarm = self.alarms.append
        clock = mock.patch.object(tw.sw, "time", FakeClock())
        clock.start()
        self.addCleanup(clock.stop)
        self.env = dict(ENV, PC_SSH="u@pc", PC_ROOT_SCP="/R", PC_TUNING_ROOT="C:\\t", PC_TUNING_ROOT_SCP="/C:/t",
                        PC_ASIO_DRIVER="D", PC_ACTIVITY_CHANNELS="101-110", RAW_DIR=str(self.dir / "raw"))
        tw.sw.save_state({"id": "w", "card": "free", "dev_time": True, "preflight": {"pref": 64}, "pref_original": 64,
                          "pref_current": 32, "pref_restored": False, "runs": [], "closed": False})

    def tearDown(self) -> None:
        tw.sw.STATE, tw.sw.EVENT_NOW, tw.sw.ps, tw.sw.scp, tw.sw.alarm, tw.PROFILE = self.saved

    def state(self) -> dict:
        return tw.sw.load_state()


class HwlatTests(WindowHarness):
    """hwlat per CPU through the real cmd_run (#32 C2)."""

    def args(self, lps: str = "2,14") -> argparse.Namespace:
        return argparse.Namespace(lps=lps, seconds=30, threshold_us=10)

    def rows(self) -> list[dict]:
        files = sorted((self.dir / "raw" / "pc-tuning" / "w").glob("hwlat-*.json"))
        self.assertEqual(len(files), 1)
        return json.loads(files[0].read_text(encoding="utf-8"))

    def test_every_cpu_is_measured_placed_and_raised(self) -> None:
        self.pc.reports = [hwlat_report(2), hwlat_report(14)]
        tw.cmd_hwlat(self.env, self.args())
        self.assertEqual([(r["cpu"], r["placed"], r["priority"], r["failed"]) for r in self.rows()],
                         [(2, [258], "time-critical", False), (14, [270], "time-critical", False)])

    def test_a_scanner_reported_unplaced_fails_the_step_and_keeps_what_was_measured(self) -> None:
        # An older spike wrote the placement error as text and still said "done".
        self.pc.reports = [hwlat_report(2), hwlat_report(14, placed="the CPU set could not be applied (synthetic)"), hwlat_report(15)]
        with self.assertRaisesRegex(tw.StepError, "CPU 14"):
            tw.cmd_hwlat(self.env, self.args("2,14,15"))
        self.assertEqual([(r["cpu"], r["failed"]) for r in self.rows()], [(2, False), (14, True)])
        self.assertEqual(self.pc.runs, 2)   # stops at the failed CPU

    def test_an_error_outcome_fails_the_step_and_keeps_what_was_measured(self) -> None:
        self.pc.reports = [hwlat_report(2), {"outcome": "error", "error": "hwlat: cpu 14 not raised (synthetic)",
                                             "hwlat": {"cpu": 14, "threshold_us": 10, "error": "cpu 14 not raised (synthetic)"}}]
        with self.assertRaisesRegex(tw.StepError, "not raised"):
            tw.cmd_hwlat(self.env, self.args())
        self.assertEqual([r["cpu"] for r in self.rows()], [2])

    def test_an_outcome_that_is_no_measurement_fails_the_step_and_is_recorded(self) -> None:
        # #32 follow-up: refused, fault-caught, rate-changed (and band activity, stop-hung)
        # end a run without a measurement: the step fails, the row names the outcome.
        for outcome, code in NOT_MEASURED:
            with self.subTest(outcome):
                for f in (self.dir / "raw" / "pc-tuning" / "w").glob("hwlat-*.json"):
                    f.unlink()
                self.pc.runs = 0
                self.pc.reports = [{"outcome": outcome, "error": f"synthetic {outcome}"}]
                with self.assertRaisesRegex(tw.StepError, f"outcome {outcome}"):
                    tw.cmd_hwlat(self.env, self.args("2"))
                self.assertEqual([(r["outcome"], r["failed"]) for r in self.rows()], [(outcome, True)])


class MeasureTests(WindowHarness):
    """measure through the real cmd_run and unwind, the PC faked at the ssh seam."""

    def setUp(self) -> None:
        super().setUp()
        self.pc.reports = [DUPLEX]

    def args(self, **kw) -> argparse.Namespace:
        base = {"label": "load-32", "frames": 32, "seconds": 60, "burn_us": 40, "stress": 0, "audio_cpus": "", "stress_cpus": "",
                "trace": "dpc", "circular_mb": 0}
        return argparse.Namespace(**{**base, **kw})

    def summary(self) -> dict:
        return json.loads(Path(self.state()["measurements"][-1]["summary"]).read_text(encoding="utf-8"))

    def record_trace(self, where: str = "C:\\t\\runs\\old-20260101T000000Z") -> None:
        st = self.state()
        st["trace"] = where
        tw.sw.save_state(st)

    def test_a_traced_run_is_summarised(self) -> None:
        tw.cmd_measure(self.env, self.args())
        s = self.summary()
        self.assertEqual((s["label"], s["verdict"]["stable"]), ("load-32", True))
        self.assertIn("isr nicdrv.sys: above 2048 us (a full period is 333)", s["findings"])
        self.assertIsNone(self.state()["trace"])

    # #32 follow-up: only a completed run ("done", or "stopped" by the stop file) is a measurement.
    def test_an_outcome_that_is_no_measurement_fails_the_step_and_is_recorded(self) -> None:
        tel = DUPLEX["segments"]
        self.pc.reports = [{"outcome": outcome, "segments": tel} for outcome, _ in NOT_MEASURED]
        for outcome, code in NOT_MEASURED:
            with self.subTest(outcome):
                with self.assertRaisesRegex(tw.StepError, f"{outcome}.*exit {code}"):
                    tw.cmd_measure(self.env, self.args(label=f"load-{code}"))
                row = self.state()["measurements"][-1]
                self.assertEqual((row["label"], row["outcome"], row["exit"], row["failed"]), (f"load-{code}", outcome, code, True))
                self.assertNotIn("summary", row)
                self.assertIsNone(self.state()["trace"])                 # stopped on the way out
        self.assertEqual(list((self.dir / "raw" / "pc-tuning" / "w").rglob("summary.json")), [])
        self.assertEqual(self.pc.bodies("Invoke-IemDpcIsr"), [])         # never analysed as a measurement

    def test_a_run_stopped_by_the_stop_file_is_still_a_measurement(self) -> None:
        self.pc.reports = [{**DUPLEX, "outcome": "stopped"}]
        tw.cmd_measure(self.env, self.args())
        self.assertEqual(self.summary()["verdict"]["outcome"], "stopped")

    # B5: a measure never leaves a kernel trace running behind it.
    def test_an_error_during_the_run_stops_the_trace_and_clears_it(self) -> None:
        self.pc.fail = {".progress.json"}
        with self.assertRaisesRegex(tw.StepError, "progress.json"):
            tw.cmd_measure(self.env, self.args())
        stops = self.pc.bodies("Stop-IemTrace")
        self.assertEqual(len(stops), 1)
        self.assertNotIn("-Merge", stops[0])      # quick: the raw files stay
        self.assertIsNone(self.state()["trace"])

    def test_a_failed_cleanup_keeps_the_trace_recorded_and_alarms(self) -> None:
        self.pc.fail = {".progress.json", "Stop-IemTrace"}
        with self.assertRaisesRegex(tw.StepError, "progress.json"):   # the run's error, not the cleanup's
            tw.cmd_measure(self.env, self.args())
        self.assertTrue(self.state()["trace"])     # trace-stop, or a preempt, retries it
        self.assertTrue(any("trace-stop" in a for a in self.alarms))

    def test_an_event_leaves_the_trace_to_the_preempt(self) -> None:
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if ".progress.json" in body else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args())
        self.assertEqual(self.pc.bodies("Stop-IemTrace"), [])   # no delay before REAPER comes back
        self.assertIn("trace-stop", tw.sw.undo_plan(self.state(), spike_running=False))

    def test_a_leftover_trace_refuses_measure_and_hwlat(self) -> None:
        self.record_trace()
        with self.assertRaisesRegex(tw.StepError, "trace-stop"):
            tw.cmd_measure(self.env, self.args())
        with self.assertRaisesRegex(tw.StepError, "trace-stop"):
            tw.cmd_hwlat(self.env, argparse.Namespace(lps="2", seconds=30, threshold_us=10))
        self.assertEqual(self.pc.calls, [])

    def test_trace_stop_stops_the_recorded_trace(self) -> None:
        self.record_trace()
        with mock.patch.object(tw.sw, "load_env", return_value=self.env):
            self.assertEqual(tw.main(["trace-stop"]), 0)
        stops = self.pc.bodies("Stop-IemTrace")
        self.assertEqual(len(stops), 1)
        self.assertIn("-Dir 'C:\\t\\runs\\old-20260101T000000Z'", stops[0])
        self.assertNotIn("-Merge", stops[0])
        self.assertIsNone(self.state()["trace"])

    def test_every_trace_stop_imports_only_the_stop(self) -> None:
        # #32 MINOR-1: a cut's stop, the final stop, a failed measure's cleanup,
        # trace-stop and the stop of a start the event overtook all send IemMeasure's
        # stop-only import and Stop-IemTraceSessions with the run folder, never the
        # tuning modules' import (an Add-Type compile on the PC being measured).
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(circular_mb=1024))             # a cut and the final stop
        self.pc.reports.append(DUPLEX)
        self.pc.fail = {".progress.json"}
        with self.assertRaisesRegex(tw.StepError, "progress.json"):
            tw.cmd_measure(self.env, self.args(label="load-err"))         # the cleanup's stop
        self.pc.fail = set()
        self.record_trace()
        tw.cmd_trace_stop(self.env, argparse.Namespace())                  # trace-stop
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if "Start-IemTrace" in body else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args(label="load-ev"))          # the overtaken start's stop
        stops = self.pc.bodies("Stop-IemTrace")
        self.assertEqual(len(stops), 5)
        for body in stops:
            self.assertIn("'bin\\IemMeasure.psm1') -ArgumentList 'stop-only'", body)
            self.assertRegex(body, r"Stop-IemTraceSessions -Dir 'C:\\t\\runs\\[a-z0-9-]+-?[0-9TZ]*' -TimeoutSeconds \d+")
            self.assertNotIn("IemTuning", body)
            self.assertNotIn("Stop-IemTrace -Xperf", body)

    def test_a_stop_that_is_not_confirmed_keeps_the_trace_recorded(self) -> None:
        # #32 MAJOR-1: only a stop reply with no kept session clears the recorded
        # trace; a kept kernel logger or a reply that is no stop result is a failure.
        for reply in ({"stopped": ["IemMarkers"], "kept": ["NT Kernel Logger"], "via": "logman"}, {"ok": True}):
            with self.subTest(reply=reply):
                self.record_trace()
                self.pc.stop_reply = reply
                with self.assertRaisesRegex(tw.StepError, "not confirmed"):
                    tw.cmd_trace_stop(self.env, argparse.Namespace())
                self.assertEqual(self.state()["trace"], "C:\\t\\runs\\old-20260101T000000Z")

    def test_a_failed_measure_whose_stop_is_not_confirmed_alarms(self) -> None:
        self.pc.fail = {".progress.json"}
        self.pc.stop_reply = {"stopped": ["IemMarkers"], "kept": ["NT Kernel Logger"], "via": "logman"}
        with self.assertRaisesRegex(tw.StepError, "progress.json"):   # the run's error, not the cleanup's
            tw.cmd_measure(self.env, self.args())
        self.assertTrue(self.state()["trace"])
        self.assertTrue(any("trace-stop" in a for a in self.alarms))

    def test_trace_stop_is_a_window_command_the_event_pre_empts(self) -> None:
        # Review m4: the stop completes (it changes the PC), then "ide event" wins (main pre-empts).
        self.record_trace()
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if "Stop-IemTrace" in body else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_trace_stop(self.env, argparse.Namespace())
        self.assertEqual([e for b, e in self.pc.calls if "Stop-IemTrace" in b], ["finish"])

    # Review m5: the error path never races a preempt over the window state.
    def test_abandon_trace_leaves_the_trace_to_the_preempt_once_the_flag_exists(self) -> None:
        self.record_trace()
        (self.dir / "EVENT-NOW").touch()
        tw.abandon_trace(self.env)
        self.assertEqual(self.pc.calls, [])
        self.assertTrue(self.state()["trace"])

    def test_abandon_trace_changes_only_the_trace_field(self) -> None:
        self.record_trace()

        def concurrent_preempt(body: str) -> None:
            if "Stop-IemTrace" in body:   # another process's preempt saves its own changes meanwhile
                st = tw.sw.load_state()
                st.update(card="reaper", closed=True, pref_restored=True)
                tw.sw.save_state(st)

        self.pc.on_call = concurrent_preempt
        tw.abandon_trace(self.env)
        st = self.state()
        self.assertEqual((st["trace"], st["card"], st["closed"], st["pref_restored"]), (None, "reaper", True, True))

    def test_trace_stop_without_a_recorded_trace_touches_nothing(self) -> None:
        tw.cmd_trace_stop(self.env, argparse.Namespace())
        self.assertEqual(self.pc.calls, [])

    # B6: "ide event" waits for the trace stop, never for the xperf analysis.
    def test_the_trace_stop_is_its_own_call_and_the_analysis_is_abandonable(self) -> None:
        seen: dict = {}

        def on_call(body: str) -> None:
            if "Invoke-IemDpcIsr" in body:
                seen["trace"] = self.state()["trace"]

        self.pc.on_call = on_call
        tw.cmd_measure(self.env, self.args())
        stops = [(b, e) for b, e in self.pc.calls if "Stop-IemTrace" in b]
        self.assertEqual(len(stops), 1)
        self.assertNotIn("-Merge", stops[0][0])                      # the merge belongs to the analysis (M1)
        self.assertNotIn("Invoke-IemDpcIsr", stops[0][0])
        self.assertEqual(stops[0][1], "finish")                      # the stop changes the PC: it completes
        self.assertEqual([e for b, e in self.pc.calls if "Invoke-IemDpcIsr" in b], ["abandon"])
        self.assertIsNone(seen["trace"])                             # recorded as stopped before the analysis

    # Review M1: "ide event" never waits for a merge, and no trace starts after it.
    def test_the_final_stop_does_not_merge_and_the_merge_is_abandonable(self) -> None:
        tw.cmd_measure(self.env, self.args())
        stops = [(b, e) for b, e in self.pc.calls if "Stop-IemTrace" in b]
        self.assertEqual([("-Merge" in b, e) for b, e in stops], [(False, "finish")])
        merges = [(b, e) for b, e in self.pc.calls if "'-merge'" in b]
        self.assertEqual([e for _, e in merges], ["abandon"])
        self.assertRegex(merges[0][0], r"'kernel\.etl', 'markers\.etl'.*'trace\.etl'")

    def test_a_cut_sets_the_raw_files_aside_and_merges_them_with_the_analysis(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(circular_mb=1024))
        cut_stops = [b for b in self.pc.bodies("Stop-IemTrace") if "cut-1" in b]
        self.assertEqual(len(cut_stops), 1)
        self.assertNotIn("-Merge", cut_stops[0])
        self.assertNotIn("Start-IemTrace", cut_stops[0])
        self.assertIn("'cut-1.kernel.etl'", cut_stops[0])
        analysis = " ; ".join(b for b, e in self.pc.calls if e == "abandon" and "'-merge'" in b)
        self.assertRegex(analysis, r"'cut-1\.kernel\.etl', 'cut-1\.markers\.etl'.*'cut-1\.etl'")

    # Review round 3, m5: a start the event overtook may end after a preempt's
    # trace-stop found no session — the measure stops it itself.
    def stop_after_start(self) -> list[str]:
        order = [b for b, _ in self.pc.calls if "Start-IemTrace" in b or "Stop-IemTrace" in b]
        return ["start" if "Start-IemTrace" in b else "stop" for b in order]

    def test_a_start_the_event_overtook_is_stopped_by_the_measure(self) -> None:
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if "Start-IemTrace" in body else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args())
        self.assertEqual(self.stop_after_start(), ["start", "stop"])
        self.assertNotIn("-Merge", self.pc.bodies("Stop-IemTrace")[0])

    def test_a_cut_restart_the_event_overtook_is_stopped_by_the_measure(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        starts = []

        def on_call(body: str) -> None:
            if "Start-IemTrace" in body:
                starts.append(body)
                if len(starts) == 2:
                    (self.dir / "EVENT-NOW").touch()   # during the cut's restart

        self.pc.on_call = on_call
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args(circular_mb=1024))
        self.assertEqual(self.stop_after_start(), ["start", "stop", "start", "stop"])

    def test_no_trace_starts_once_the_event_flag_exists(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}

        def on_call(body: str) -> None:
            if "Stop-IemTrace" in body and "cut-1" in body:
                (self.dir / "EVENT-NOW").touch()   # "ide event" during the cut's stop

        self.pc.on_call = on_call
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args(circular_mb=1024))
        self.assertEqual(len(self.pc.bodies("Start-IemTrace")), 1)   # the first start only
        self.assertTrue(self.state()["trace"])                       # left to the preempt's trace-stop

    # Review round 3, decision B: the analysis never competes with the event.
    ANALYSIS = ("'-merge'", "Invoke-IemDpcIsr", "Export-IemNearGlitch")

    def analysis_calls(self) -> list[tuple[str, str]]:
        return [(b, e) for b, e in self.pc.calls if any(v in b for v in self.ANALYSIS)]

    def test_each_analysis_step_is_its_own_idle_call_that_checks_the_stop_file(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        calls = self.analysis_calls()
        self.assertEqual(len(calls), 6)                                   # (trace + 1 cut) × (merge, dpcisr, near)
        for body, event in calls:
            self.assertEqual(sum(v in body for v in self.ANALYSIS), 1, body)
            self.assertEqual(event, "abandon")
            self.assertIn("(Get-Process -Id $PID).PriorityClass = 'Idle'", body)   # xperf inherits it
            # A preempt writes the spike's stop file first: newer than the analysis start → no start.
            self.assertRegex(body, r"queue\\stop'\) ; if \(\(Test-Path -LiteralPath \$s\) -and .*LastWriteTimeUtc -gt .*'2026-01-01T00:00:00Z'.*throw ")

    def test_the_analysis_guard_runs_before_the_tuning_modules_load(self) -> None:
        # F2 round 3, m6: importing IemMeasure loads IemTuning (an Add-Type compile).
        # A step the PC refuses at "ide event" must not compile first, and the import
        # itself runs at Idle priority.
        tw.cmd_measure(self.env, self.args(trace="diag"))
        calls = self.analysis_calls()
        self.assertEqual(len(calls), 3)
        for body, _ in calls:
            load = body.index("IemMeasure.psm1")
            self.assertLess(body.index("(Get-Process -Id $PID).PriorityClass = 'Idle'"), load, body)
            self.assertLess(body.index(f"throw '{tw.ANALYSIS_REFUSED}'"), load, body)

    def test_a_merged_trace_leaves_no_raw_files_behind(self) -> None:
        # Review round 3, m8: a soak's raw kernel/marker files (per cut) would double
        # the disk used. They are deleted only after `xperf -merge` succeeded: a failed
        # merge throws (sw.ps runs under $ErrorActionPreference='Stop') before the delete.
        # F2 round 3, m10: and only once the merged trace exists and is not empty
        # (an exit code of 0 alone is no proof the merge wrote it).
        for base, out in (("", "trace.etl"), ("cut-2.", "cut-2.etl")):
            body = tw.merge("'xperf.exe'", "'D'", base)
            self.assertRegex(body, r"\[void\]\(Invoke-IemXperf [^;]*'-merge'[^;]*\) ; ")
            self.assertTrue(body.endswith(" ; Remove-Item -LiteralPath $m"), body)
            check = re.search(r"if \(-not \(Test-Path -LiteralPath (\$\w+)\) -or \(Get-Item -LiteralPath \1\)\.Length -le 0\) "
                              r"\{ throw [^}]*\}", body)
            self.assertIsNotNone(check, body)
            self.assertIn(f"Join-Path 'D' '{out}'", body[:check.start()])
            self.assertLess(body.index("'-merge'"), check.start())
            self.assertLess(check.end(), body.index("Remove-Item"))

    def test_no_analysis_step_starts_once_the_flag_exists(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        self.pc.on_call = lambda body: (self.dir / "EVENT-NOW").touch() if "Invoke-IemDpcIsr" in body else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        self.assertEqual(len(self.analysis_calls()), 2)                  # the first merge, then the dpcisr during which it came

    def test_a_step_the_pc_refused_after_a_preempt_is_the_event(self) -> None:
        # The flag is not on the dev box yet, but a preempt already wrote the stop file.
        real = self.pc.answer

        def refusing(body, event):
            if "Invoke-IemDpcIsr" in body:
                self.pc.calls.append((body, event))
                raise tw.StepError("PC step failed: ide event: the analysis step did not start")
            return real(body, event)

        self.pc.answer = refusing
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args())
        self.assertNotIn("measurements", self.state())

    # Review M3: the analysis downloads and parses give way to "ide event".
    def analysis_copies(self) -> list[tuple[str, str]]:
        return [(n, e) for n, e in self.pc.copies if not n.endswith((".report.json", ".stderr.txt"))]

    def test_the_analysis_copies_are_abandonable(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        self.assertEqual([n for n, _ in self.analysis_copies()], ["dpcisr.txt", "near.txt", "cut-1.dpcisr.txt", "cut-1.near.txt"])
        self.assertEqual({e for _, e in self.analysis_copies()}, {"abandon"})

    def test_an_event_during_a_copy_stops_the_analysis_there(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        self.pc.on_copy = lambda name: (self.dir / "EVENT-NOW").touch() if name == "near.txt" else None
        with self.assertRaises(tw.sw.EventNow):
            tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        self.assertEqual([n for n, _ in self.analysis_copies()], ["dpcisr.txt", "near.txt"])
        self.assertNotIn("measurements", self.state())

    def test_the_near_dumps_are_parsed_from_their_files_with_the_event_check(self) -> None:
        seen: list = []
        real = tw.lr.near_glitch

        def spy(source, period_us, window_periods=2, check=None):
            seen.append((type(source).__name__, check))
            return real(source, period_us, window_periods, check)

        with mock.patch.object(tw.lr, "near_glitch", spy):
            tw.cmd_measure(self.env, self.args(trace="diag"))
        self.assertEqual([(kind, check is not None) for kind, check in seen], [("PosixPath", True)])

    # B7: a cut keeps the trace's options, and every cut gets its own near-glitch view.
    def test_a_diag_cut_restarts_with_context_switches_and_gets_its_near_glitch_view(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(trace="diag", circular_mb=1024))
        starts = [b.split("Start-IemTrace", 1)[1] for b in self.pc.bodies("Start-IemTrace")]
        self.assertEqual(len(starts), 2)                             # the start and the cut's restart
        for options in starts:
            self.assertIn("-CSwitch", options)
            self.assertIn("-CircularMB 1024", options)
        analysis = " ; ".join(self.pc.bodies("Export-IemNearGlitch"))
        self.assertRegex(analysis, r"Export-IemNearGlitch [^;]*-Name 'cut-1\.etl'")
        s = self.summary()
        self.assertEqual([c["cut"] for c in s["cuts"]], [1])
        self.assertEqual(s["cuts"][0]["near_glitch"][0]["kind"], "missed")
        self.assertIn("isr nicdrv.sys: above 2048 us (a full period is 333)", s["cuts"][0]["findings"])
        self.assertEqual(s["near_glitch"][0]["kind"], "missed")       # the final trace's view stays

    # B8 in the window: an unreadable dpcisr is a failed step with a clear message, not a traceback.
    def test_a_dpcisr_without_per_cpu_usage_fails_the_step(self) -> None:
        start = tlr.DPCISR_XPERF.index("     CPU 0 Usage,")
        self.pc.dpcisr = tlr.DPCISR_XPERF[:start] + tlr.DPCISR_XPERF[tlr.DPCISR_XPERF.index("\nTotal = 3261"):]
        with self.assertRaisesRegex(tw.StepError, "per-CPU usage.*dpcisr.txt"):
            tw.cmd_measure(self.env, self.args())
        self.assertIsNone(self.state()["trace"])                         # the trace was stopped before the analysis
        self.assertNotIn("measurements", self.state())

    # B11: the 10 s poll must not load the PC being measured with a C# compile or heavy WMI.
    def test_the_poll_is_light_and_still_samples_the_sentinels(self) -> None:
        tw.cmd_measure(self.env, self.args())
        polls = [(b, e) for b, e in self.pc.calls if "Win32_PerfRawData_PerfOS_Processor" in b or "Get-IemPollSample" in b]
        self.assertEqual(len(polls), 2)                                  # one per status poll
        for body, event in polls:
            self.assertNotIn("IemMeasure", body)          # importing the modules runs Add-Type (a C# compile) every time
            self.assertNotIn("Get-IemPollSample", body)
            self.assertNotIn("Win32_PerfFormattedData", body)            # the second, formatted WMI class is unused
            self.assertEqual(body.count("Get-CimInstance"), 1)
            self.assertIn("-Name 'gov'", body)                           # the governor from the local profile
            self.assertEqual(event, "abandon")
        self.assertIn("-Id 4242", polls[0][0])                           # the spike's pid from the running status
        self.assertIn("-eq 4243", polls[0][0])                           # and its callback thread from the progress
        self.assertNotIn("Get-Process", polls[1][0])                     # exited: no process to read
        self.assertEqual(self.summary()["callback"]["priority"], {"base_min": 15, "base_max": 15, "current_min": 26, "current_max": 26})

    # C1: the busy threads never share the audio CPU: the profile's housekeeping CPUs unless told otherwise.
    def test_stress_runs_on_the_housekeeping_cpus_unless_told_otherwise(self) -> None:
        self.pc.reports = [DUPLEX, DUPLEX]
        tw.cmd_measure(self.env, self.args(stress=4, audio_cpus="14"))
        tw.cmd_measure(self.env, self.args(label="load-32-b", stress=4, audio_cpus="14", stress_cpus="6-13"))
        first, second = self.pc.bodies("Write-GoldenRequest")
        self.assertIn("stress_cpus = '0,1,6,7,8,9,10,11,12,13'", first)
        self.assertIn("audio_cpus = '14'", first)
        self.assertIn("stress_cpus = '6-13'", second)

    def test_the_default_stress_cpus_leave_out_the_audio_cpus(self) -> None:
        # Review m12: an --audio-cpus inside the housekeeping layout is taken out of it.
        tw.cmd_measure(self.env, self.args(stress=4, audio_cpus="6,7"))
        self.assertIn("stress_cpus = '0,1,8,9,10,11,12,13'", self.pc.bodies("Write-GoldenRequest")[0])

    def test_a_dpc_trace_has_no_near_glitch_view(self) -> None:
        self.pc.progress = {"missed": 1, "overruns": 0, "position_gaps": 0, "callback_thread": 4243}
        tw.cmd_measure(self.env, self.args(circular_mb=1024))
        self.assertEqual(self.pc.bodies("Export-IemNearGlitch"), [])
        self.assertNotIn("-CSwitch", " ".join(self.pc.bodies("Start-IemTrace")))
        self.assertEqual(list(self.summary()["cuts"][0]), ["cut", "findings"])


if __name__ == "__main__":
    unittest.main()
