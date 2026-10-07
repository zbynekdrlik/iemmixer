"""Tests for scripts/iem-pc/iempc_trace.py: `iempc trace` (#15), a kernel
DPC/ISR trace on the guard's engine. They reuse test_iempc's fakes (FakePc
stands in for ssh, a fake scp writes the report) and latency_report's
synthetic xperf fixture; every value is synthetic."""
from __future__ import annotations

import json
import signal
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "pc-tuning"))
import iempc_trace  # noqa: E402,F401  (the module under test; `iempc trace` runs it)
import iempc_tuning  # noqa: E402
import latency_report as lr  # noqa: E402
from test_iempc import SHA, Base, ip  # noqa: E402
from test_iempc_tuning import PROFILE  # noqa: E402
from test_latency_report import DPCISR_XPERF  # noqa: E402

XPERF = "X:\\wpt\\xperf.exe"
ENGINE = {"build": SHA, "frames": 32, "callbacks": 1000, "missed": 0, "resets": 0, "parked": False, "faulted": False,
          "pipe_private": True, "spawns": 1, "last_exit": None, "hil": []}
STOPPED = {"stopped": ["NT Kernel Logger", "IemMarkers"], "gone": [], "kept": [], "notes": [], "via": "logman", "tuning_error": None}
IMPORT = "Import-Module (Join-Path (Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'iemmixer\\tuning') 'IemMeasure.psm1')"


def status(mode: str = "dev", engine: dict | None = None, switching=None, code: int = 0) -> tuple[int, str]:
    doc = {"ok": code == 0, "mode": mode, "switching": switching, "alarms": [], "detail": f"mode {mode}; bundle {SHA}"}
    if engine is not None:
        doc["engine"] = engine
    return code, json.dumps(doc)


class TraceBase(Base):
    def setUp(self) -> None:
        super().setUp()
        envfile = ip.env_path()
        envfile.write_text(envfile.read_text(encoding="utf-8") + f"PC_XPERF={XPERF}\n", encoding="utf-8")
        self.profile = self.tmp / "pc-tuning.json"
        self.profile.write_text(json.dumps(PROFILE), encoding="utf-8")
        saved = iempc_tuning.PROFILE
        self.addCleanup(setattr, iempc_tuning, "PROFILE", saved)
        iempc_tuning.PROFILE = self.profile
        self.statuses(status(engine=ENGINE), status(engine={**ENGINE, "callbacks": 4000, "missed": 2, "resets": 1}))
        self.answers: dict = {"Start-IemTrace": {"dir": "x", "started": "2026-10-07T06:00:00Z"},
                              "Stop-IemTraceSessions": STOPPED, "'-merge'": None,
                              "Invoke-IemDpcIsr": "X:\\root\\traces\\run\\dpcisr.txt"}
        self.pc.module_result = self.answer
        self.report = DPCISR_XPERF
        self.fetches: list[tuple[str, str, str]] = []
        ip.scp = self.scp

    def statuses(self, *answers) -> None:
        """`iemmode status` gives these in turn, then the last one again; a callable is called."""
        queue = list(answers)

        def answer():
            a = queue.pop(0) if len(queue) > 1 else queue[0]
            return a() if callable(a) else a

        self.pc.replies[("status",)] = answer

    def answer(self):
        script = self.pc.modules[-1][0]
        for text, r in self.answers.items():
            if text in script:
                return r() if callable(r) else r
        raise AssertionError(f"unexpected module script {script[-300:]!r}")

    def scp(self, src: str, dst: str, event: str) -> None:
        self.fetches.append((src, dst, event))
        Path(dst).write_text(self.report, encoding="utf-8")

    def steps(self) -> list[tuple[str, str]]:
        """The PC module calls as (step, flag mode)."""
        names = (("Start-IemTrace", "start"), ("Stop-IemTraceSessions", "stop"), ("'-merge'", "merge"), ("Invoke-IemDpcIsr", "dpcisr"))
        return [(next((n for t, n in names if t in s), "other"), e) for s, e in self.pc.modules]

    def trace(self, *extra: str) -> tuple[int, list[dict], str]:
        return self.run_main("trace", "--label", "base-test", "--seconds", "1", *extra)


class TraceTests(TraceBase):
    def test_trace_is_dev_time_and_one_at_a_time(self) -> None:
        spec = ip.COMMANDS["trace"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))
        self.flag()
        code, docs, err = self.trace()
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules), (1, [], [], []))
        self.assertIn("runs only in dev time", err)

    def test_a_trace_reads_the_engine_traces_stops_analyses_and_reports(self) -> None:
        code, docs, err = self.trace()
        self.assertEqual(code, 0, err)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")] * 2)
        self.assertEqual(self.steps(), [("start", "finish"), ("stop", "finish"), ("merge", "abandon"), ("dpcisr", "abandon")])
        [doc] = docs
        run = doc["run"]
        self.assertRegex(run, r"^base-test-\d{8}T\d{6}Z$")
        pc_run = f"X:\\root\\traces\\{run}"
        start, stop, merge, dpcisr = (s for s, _ in self.pc.modules)
        self.assertIn(f"$r = & {{ {IMPORT} -Force -Global ; Start-IemTrace -Xperf '{XPERF}' -Dir '{pc_run}' }}", start)
        self.assertIn(f"{IMPORT} -ArgumentList 'stop-only' -Force -Global ; Stop-IemTraceSessions -Dir '{pc_run}' -TimeoutSeconds 20", stop)
        for step in (merge, dpcisr):
            self.assertIn(f"$r = & {{ (Get-Process -Id $PID).PriorityClass = 'Idle' ; {IMPORT} -Force -Global ; ", step)
        self.assertIn(f"Join-Path '{pc_run}' 'trace.etl'", merge)
        self.assertIn(f"Invoke-IemDpcIsr -Xperf '{XPERF}' -Dir '{pc_run}'", dpcisr)
        local = ip.state_dir() / "traces" / run / "dpcisr.txt"
        self.assertEqual(self.fetches, [(f"tester@pc.test:/X:/root/traces/{run}/dpcisr.txt", str(local), "abandon")])
        parsed = lr.parse_dpcisr(DPCISR_XPERF)
        self.assertEqual(doc, {
            "label": "base-test", "seconds": 1, "circular_mb": None, "run": run, "build": SHA,
            "callbacks": 3000, "missed": 2, "resets": 1, "same_engine": True, "after_error": None,
            "watched": {"lps": [2, 3],
                        "dpc": {"carddrv.sys": {"2": 45000, "3": 0}, "gpudrv.sys": {"2": 0, "3": 2500}, "nicdrv.sys": {"2": 800, "3": 0}},
                        "isr": {"carddrv.sys": {"2": 9000, "3": 0}}},
            "top_modules": lr.top_modules(parsed), "findings": lr.budget_findings(parsed, [2, 3]), "report": str(local)})
        self.assertIn("dpc gpudrv.sys: up to 256 us on a watched CPU (budget 128)", doc["findings"])
        self.assertEqual(json.loads((local.parent / "summary.json").read_text(encoding="utf-8")), doc)

    def test_a_circular_trace_passes_its_size(self) -> None:
        code, docs, err = self.trace("--circular-mb", "512")
        self.assertEqual(code, 0, err)
        self.assertIn(" -CircularMB 512", self.pc.modules[0][0])
        self.assertEqual(docs[-1]["circular_mb"], 512)

    def test_without_pc_xperf_it_is_refused_before_the_pc(self) -> None:
        envfile = ip.env_path()
        envfile.write_text(envfile.read_text(encoding="utf-8").replace(f"PC_XPERF={XPERF}\n", ""), encoding="utf-8")
        code, docs, err = self.trace()
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules), (1, [], [], []))
        self.assertIn("PC_XPERF missing in the private env", err)

    def test_bad_arguments_are_refused_before_the_pc(self) -> None:
        for argv in (("--label", "Base_Test", "--seconds", "60"), ("--label", "ok", "--seconds", "0"),
                     ("--label", "ok", "--seconds", "86401"), ("--label", "ok", "--seconds", "60", "--circular-mb", "0"),
                     ("--label", "ok", "--seconds", "60", "--circular-mb", "16385")):
            code, docs, err = self.run_main("trace", *argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("trace:", err, argv)
        self.assertEqual((self.pc.calls, self.pc.modules), ([], []))

    def test_a_profile_tuning_rules_refuses_is_refused_before_the_pc(self) -> None:
        self.profile.write_text(json.dumps({**PROFILE, "layout": {"card": [2], "audio": [2]}}), encoding="utf-8")
        code, _, err = self.trace()
        self.assertEqual((code, self.pc.calls, self.pc.modules), (1, [], []))
        self.assertIn("processor 2 has two roles", err)

    def test_the_guard_must_be_settled_in_dev_with_an_engine(self) -> None:
        for answer, why in ((status(mode="event", engine=ENGINE), "mode event"),
                            (status(engine=ENGINE, switching={"from": "event", "to": "dev"}), "a switch runs"),
                            (status(), "no engine runs"),
                            (status(engine={**ENGINE, "callbacks": True}), "callbacks"),
                            (status(code=4), "exit 4")):
            self.statuses(answer)
            self.pc.calls.clear()
            code, docs, err = self.trace()
            self.assertEqual((code, docs, self.pc.modules), (1, [], []), why)
            self.assertIn("no trace:", err, why)
            self.assertIn(why, err, why)
            self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")], why)

    def test_a_restarted_engine_has_no_deltas(self) -> None:
        self.statuses(status(engine=ENGINE), status(engine={**ENGINE, "callbacks": 50, "spawns": 2}))
        code, docs, err = self.trace()
        self.assertEqual(code, 0, err)
        self.assertEqual({k: docs[-1][k] for k in ("callbacks", "missed", "resets", "same_engine")},
                         {"callbacks": None, "missed": None, "resets": None, "same_engine": False})
        self.statuses(status(engine=ENGINE), status())
        code, docs, err = self.trace()
        self.assertEqual((code, docs[-1]["callbacks"], docs[-1]["same_engine"]), (0, None, False), err)

    def test_an_after_read_that_fails_still_stops_and_analyses(self) -> None:
        def ssh_cut():
            raise ip.StepError("ssh: connection reset")

        self.statuses(status(engine=ENGINE), ssh_cut)
        code, docs, err = self.trace()
        self.assertEqual(code, 0, err)
        self.assertEqual([s for s, _ in self.steps()], ["start", "stop", "merge", "dpcisr"])
        self.assertEqual((docs[-1]["callbacks"], docs[-1]["same_engine"]), (None, False))
        self.assertIn("connection reset", docs[-1]["after_error"])


class TraceStopTests(TraceBase):
    """The kernel trace is never left behind: a flag, a signal or a failure
    after the start stops it at once (the stop-only import, no merge)."""

    def test_a_flag_during_the_wait_stops_the_trace_at_once_and_runs_the_event_path(self) -> None:
        with mock.patch.object(ip.time, "sleep", side_effect=lambda _s: self.flag()):
            code, docs, _ = self.trace()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.steps(), [("start", "finish"), ("stop", "ignore")])
        self.assertIn("-ArgumentList 'stop-only'", self.pc.modules[1][0])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon"), ("iemmode.exe", ["event"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual(self.fetches, [])

    def test_a_flag_during_the_analysis_gives_it_up_and_runs_the_event_path(self) -> None:
        self.answers["'-merge'"] = lambda: self.flag()
        code, _, _ = self.trace()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.steps(), [("start", "finish"), ("stop", "finish"), ("merge", "abandon")])
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))
        self.assertEqual(self.fetches, [])

    def test_a_signal_during_the_wait_stops_the_trace_and_ends_the_command(self) -> None:
        before = signal.getsignal(signal.SIGTERM)
        with mock.patch.object(ip.time, "sleep", side_effect=lambda _s: signal.raise_signal(signal.SIGTERM)):
            with self.assertRaises(SystemExit):
                self.trace()
        self.assertEqual(self.steps(), [("start", "finish"), ("stop", "ignore")])
        self.assertIs(signal.getsignal(signal.SIGTERM), before)

    def test_a_failed_start_still_runs_the_stop(self) -> None:
        def refused():
            raise ip.StepError("PC step failed: xperf at X:\\wpt\\xperf.exe is not signed by Microsoft")

        self.answers["Start-IemTrace"] = refused
        code, docs, err = self.trace()
        self.assertEqual((code, docs), (1, []))
        self.assertEqual(self.steps(), [("start", "finish"), ("stop", "ignore")])
        self.assertIn("not signed by Microsoft", err)

    def test_a_stop_that_is_not_confirmed_fails_and_analyses_nothing(self) -> None:
        self.answers["Stop-IemTraceSessions"] = {"stopped": [], "gone": [], "kept": ["NT Kernel Logger"], "notes": []}
        code, docs, err = self.trace()
        self.assertEqual((code, docs), (1, []))
        self.assertEqual([s for s, _ in self.steps()], ["start", "stop"])
        self.assertIn("the trace stop is not confirmed", err)

    def test_a_failed_stop_after_a_flag_is_reported_and_the_event_path_still_runs(self) -> None:
        def cut():
            raise ip.StepError("ssh: connection reset")

        self.answers["Stop-IemTraceSessions"] = cut
        with mock.patch.object(ip.time, "sleep", side_effect=lambda _s: self.flag()):
            code, _, err = self.trace()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertIn("WARNING: the kernel trace may still run", err)
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))

    def test_an_unreadable_report_fails_naming_the_saved_file(self) -> None:
        self.report = "not an xperf report\n"
        code, docs, err = self.trace()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("no DPC module read", err)
        self.assertIn("dpcisr.txt", err)


class ImportTests(unittest.TestCase):
    def test_iempc_loads_none_of_s1c_s_modules_until_a_tuning_command_runs(self) -> None:
        """`iempc event` must never depend on S1c's code (scripts/pc-tuning, asio-spike, golden)."""
        names = ("tuning_rules", "latency_report", "spike_window", "tuning_window", "golden_window")
        code = f"import sys; sys.path.insert(0, {str(HERE)!r}); import iempc; print([m for m in {names!r} if m in sys.modules])"
        out = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, check=True, timeout=60).stdout
        self.assertEqual(out.strip(), "[]")


if __name__ == "__main__":
    unittest.main()
