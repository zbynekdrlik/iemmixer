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
from test_iempc import SHA, SHA2, Base, ip, make_zip, sha256  # noqa: E402
from test_iempc_tuning import MODULES, PROFILE  # noqa: E402
from test_latency_report import DPCISR_XPERF  # noqa: E402

XPERF = "X:\\wpt\\xperf.exe"
ENGINE = {"build": SHA, "frames": 32, "callbacks": 1000, "missed": 0, "resets": 0, "parked": False, "faulted": False,
          "pipe_private": True, "spawns": 1, "last_exit": None, "hil": []}
STOPPED = {"stopped": ["NT Kernel Logger", "IemMarkers"], "gone": [], "kept": [], "notes": [], "via": "logman", "tuning_error": None}
# The elevated tuning folder as the PC resolves it (the preflight's answer) and its modules.
TDIR = "C:\\ProgramData\\iemmixer\\tuning"
TUNING = TDIR + "\\IemTuning.psm1"
MEASURE = TDIR + "\\IemMeasure.psm1"
HT, HM = sha256(MODULES["tuning/IemTuning.psm1"]), sha256(MODULES["tuning/IemMeasure.psm1"])
IDLE = "(Get-Process -Id $PID).PriorityClass = 'Idle'"
STEP_NAMES = (("GetFolderPath('CommonApplicationData')", "preflight"), ("Start-IemTrace", "start"),
              ("Stop-IemTraceSessions", "stop"), ("'-merge'", "merge"), ("Invoke-IemDpcIsr", "dpcisr"))


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
        # The running engine's bundle, fetched on this box with its tuning modules.
        self.gh.artifact = make_zip(self.tmp / "artifact-tuning" / f"iemmixer-{SHA}.zip", extra=MODULES)
        self.fetched()
        self.statuses(status(engine=ENGINE), status(engine={**ENGINE, "callbacks": 4000, "missed": 2, "resets": 1}))
        self.answers: dict = {"GetFolderPath('CommonApplicationData')": {"dir": TDIR, "tuning": HT, "measure": HM,
                                                                          "profile": sha256(self.profile.read_bytes())},
                              "Start-IemTrace": {"dir": "x", "started": "2026-10-07T06:00:00Z"},
                              "Stop-IemTraceSessions": STOPPED, "'-merge'": None,
                              "Invoke-IemDpcIsr": "X:\\root\\traces\\run\\dpcisr.txt"}
        self.pc.module_result = self.answer
        # The trace runs no refresh, and its preflight names profile.json too: no
        # module script is answered as the refresh's profile check here.
        self.pc.texts = {}
        self.report = DPCISR_XPERF
        self.fetches: list[tuple[str, str, str]] = []
        ip.scp = self.scp
        ip.ssh_ps = self.ssh_ps

    def ssh_ps(self, env, script, timeout, event):
        """FakePc's answer; an answer {"pc_error": text} is the PC's own failure
        reply ({"ok": false, "error": text}), as module_script prints it."""
        out = self.pc.ssh_ps(env, script, timeout, event)
        doc = json.loads(out.splitlines()[-1])
        if isinstance(doc.get("r"), dict) and "pc_error" in doc["r"]:
            return json.dumps({"ok": False, "error": doc["r"]["pc_error"]}) + "\n"
        return out

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
        return [(next((n for t, n in STEP_NAMES if t in s), "other"), e) for s, e in self.pc.modules]

    def names(self) -> list[str]:
        return [n for n, _ in self.steps()]

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
        self.assertEqual(self.steps(), [("preflight", "abandon"), ("start", "finish"), ("stop", "ignore"),
                                        ("merge", "abandon"), ("dpcisr", "abandon")])
        [doc] = docs
        run = doc["run"]
        self.assertRegex(run, r"^base-test-\d{8}T\d{6}Z$")
        pc_run = f"X:\\root\\traces\\{run}"
        preflight, start, stop, merge, dpcisr = (s for s, _ in self.pc.modules)
        self.assertIn("(Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'iemmixer\\tuning')", preflight)
        load = f"{ip.hash_check(TUNING, HT)} ; {ip.hash_check(MEASURE, HM)} ; Import-Module '{MEASURE}' -Force ; "
        self.assertIn(f"try {{ {load}$r = & {{ Start-IemTrace -Xperf '{XPERF}' -Dir '{pc_run}' }}", start)
        self.assertIn(f"try {{ {ip.hash_check(MEASURE, HM)} ; Import-Module '{MEASURE}' -ArgumentList 'stop-only' -Force ; "
                      f"$r = & {{ Stop-IemTraceSessions -Dir '{pc_run}' -TimeoutSeconds 20 }}", stop)
        for step in (merge, dpcisr):
            self.assertIn(f"try {{ {IDLE} ; {load}$r = & {{ ", step)
        self.assertIn(f"Join-Path '{pc_run}' 'trace.etl'", merge)
        self.assertIn(f"Invoke-IemDpcIsr -Xperf '{XPERF}' -Dir '{pc_run}'", dpcisr)
        local = ip.state_dir() / "traces" / run / "dpcisr.txt"
        self.assertEqual(self.fetches, [(f"tester@pc.test:/X:/root/traces/{run}/dpcisr.txt", str(local), "abandon")])
        parsed = lr.parse_dpcisr(DPCISR_XPERF)
        self.assertEqual(doc, {
            "label": "base-test", "seconds": 1, "circular_mb": None, "run": run, "build": SHA, "frames": 32,
            "callbacks": 3000, "missed": 2, "resets": 1, "same_engine": True, "after_error": None,
            "after": {"mode": "dev", "parked": False, "faulted": False}, "profile_sha256": sha256(self.profile.read_bytes()),
            "watched": {"lps": [2, 3],
                        "dpc": {"carddrv.sys": {"2": 45000, "3": 0}, "gpudrv.sys": {"2": 0, "3": 2500}, "nicdrv.sys": {"2": 800, "3": 0}},
                        "isr": {"carddrv.sys": {"2": 9000, "3": 0}}},
            "top_modules": lr.top_modules(parsed), "findings": lr.budget_findings(parsed, [2, 3]), "report": str(local)})
        self.assertIn("dpc gpudrv.sys: up to 256 us on a watched CPU (budget 128)", doc["findings"])
        self.assertEqual(json.loads((local.parent / "summary.json").read_text(encoding="utf-8")), doc)

    def test_a_circular_trace_passes_its_size(self) -> None:
        code, docs, err = self.trace("--circular-mb", "512")
        self.assertEqual(code, 0, err)
        self.assertIn(" -CircularMB 512 }", self.pc.modules[1][0])
        self.assertEqual(docs[-1]["circular_mb"], 512)

    def test_a_long_trace_needs_a_circular_file(self) -> None:
        code, docs, err = self.run_main("trace", "--label", "soak", "--seconds", str(iempc_trace.MAX_LINEAR_S + 1))
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules), (1, [], [], []))
        self.assertIn("needs --circular-mb", err)

    def test_without_pc_xperf_it_is_refused_before_the_pc(self) -> None:
        envfile = ip.env_path()
        envfile.write_text(envfile.read_text(encoding="utf-8").replace(f"PC_XPERF={XPERF}\n", ""), encoding="utf-8")
        code, docs, err = self.trace()
        self.assertEqual((code, docs, self.pc.calls, self.pc.modules), (1, [], [], []))
        self.assertIn("PC_XPERF missing in the private env", err)

    def test_bad_arguments_are_refused_before_the_pc(self) -> None:
        for argv in (("--label", "Base_Test", "--seconds", "60"), ("--label", "ok", "--seconds", "0"),
                     ("--label", "ok", "--seconds", "86401", "--circular-mb", "512"),
                     ("--label", "ok", "--seconds", "60", "--circular-mb", "0"),
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

    def test_an_open_spike_window_refuses_the_trace(self) -> None:
        self.open_window()
        code, _, err = self.trace()
        self.assertEqual((code, self.pc.calls, self.pc.modules), (1, [], []))
        self.assertIn("'trace' waits until 'iempc handover-s1a'", err)

    def test_the_guard_must_be_settled_in_dev_with_a_playing_engine(self) -> None:
        for answer, why in ((status(mode="event", engine=ENGINE), "mode event"),
                            (status(engine=ENGINE, switching={"from": "event", "to": "dev"}), "a switch runs"),
                            (status(), "no engine runs"),
                            (status(engine={**ENGINE, "parked": True}), "parked"),
                            (status(engine={**ENGINE, "faulted": True}), "faulted"),
                            (status(engine={**ENGINE, "callbacks": True}), "callbacks"),
                            (status(engine={**ENGINE, "frames": "32"}), "frames"),
                            (status(engine={**ENGINE, "build": "local"}), "build"),
                            (status(code=4), "exit 4")):
            self.statuses(answer)
            self.pc.calls.clear()
            code, docs, err = self.trace()
            self.assertEqual((code, docs, self.pc.modules), (1, [], []), why)
            self.assertIn("no trace:", err, why)
            self.assertIn(why, err, why)
            self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")], why)

    def test_modules_that_are_not_the_running_bundle_s_are_refused_before_the_start(self) -> None:
        for found in ({"dir": TDIR, "tuning": HT, "measure": "0" * 64}, {"dir": TDIR, "tuning": None, "measure": None},
                      {"dir": "relative\\tuning", "tuning": HT, "measure": HM}, "ok"):
            self.answers["GetFolderPath('CommonApplicationData')"] = found
            self.pc.modules.clear()
            code, docs, err = self.trace()
            self.assertEqual((code, docs, self.names()), (1, [], ["preflight"]), found)
            self.assertIn("no trace:", err, found)
        self.assertIn(f"iempc tuning-install --sha {SHA}", err)

    def test_a_running_bundle_this_box_never_fetched_is_refused_before_the_pc_changes(self) -> None:
        self.statuses(status(engine={**ENGINE, "build": SHA2}))
        code, _, err = self.trace()
        self.assertEqual((code, self.pc.modules), (1, []))
        self.assertIn(f"bundle {SHA2} is not fetched", err)

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
        self.assertEqual(self.names(), ["preflight", "start", "stop", "merge", "dpcisr"])
        self.assertEqual((docs[-1]["callbacks"], docs[-1]["same_engine"], docs[-1]["after"]), (None, False, None))
        self.assertIn("connection reset", docs[-1]["after_error"])
        # A status that answers with an error is a failed read too, never a quiet restart.
        self.statuses(status(engine=ENGINE), status(code=4))
        code, docs, err = self.trace()
        self.assertEqual(code, 0, err)
        self.assertEqual((docs[-1]["after_error"], docs[-1]["same_engine"]), ("iemmode status exit 4", False))

    def test_the_after_read_names_an_engine_that_parked_during_the_trace(self) -> None:
        self.statuses(status(engine=ENGINE), status(engine={**ENGINE, "callbacks": 1500, "parked": True}))
        code, docs, err = self.trace()
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["after"], {"mode": "dev", "parked": True, "faulted": False})

    def test_a_pc_profile_other_than_the_local_one_is_refused_before_the_start(self) -> None:
        for found in ("0" * 64, None):
            self.answers["GetFolderPath('CommonApplicationData')"] = {"dir": TDIR, "tuning": HT, "measure": HM, "profile": found}
            self.pc.modules.clear()
            code, docs, err = self.trace()
            self.assertEqual((code, docs, self.names()), (1, [], ["preflight"]), found)
            self.assertIn("the watched processors would not be the PC's", err)
            self.assertIn(f"iempc tuning-install --sha {SHA}", err)


class TraceStopTests(TraceBase):
    """The kernel trace is never left behind unreported: a flag, a signal or a
    failure after the start stops it at once (the stop-only import, no merge)."""

    def test_a_flag_during_the_wait_stops_the_trace_at_once_and_runs_the_event_path(self) -> None:
        with mock.patch.object(ip.time, "sleep", side_effect=lambda _s: self.flag()):
            code, docs, _ = self.trace()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.steps(), [("preflight", "abandon"), ("start", "finish"), ("stop", "ignore")])
        self.assertIn("-ArgumentList 'stop-only'", self.pc.modules[2][0])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon"), ("iemmode.exe", ["event"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual(self.fetches, [])

    def test_a_flag_during_the_analysis_gives_it_up_and_runs_the_event_path(self) -> None:
        self.answers["'-merge'"] = lambda: self.flag()
        code, _, _ = self.trace()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.names(), ["preflight", "start", "stop", "merge"])
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))
        self.assertEqual(self.fetches, [])

    def test_a_signal_during_the_wait_stops_the_trace_and_ends_the_command(self) -> None:
        before = signal.getsignal(signal.SIGTERM)
        with mock.patch.object(ip.time, "sleep", side_effect=lambda _s: signal.raise_signal(signal.SIGTERM)):
            with self.assertRaises(SystemExit):
                self.trace()
        self.assertEqual(self.steps(), [("preflight", "abandon"), ("start", "finish"), ("stop", "ignore")])
        self.assertIs(signal.getsignal(signal.SIGTERM), before)

    def test_a_start_the_pc_refused_still_runs_the_stop(self) -> None:
        self.answers["Start-IemTrace"] = {"pc_error": "xperf at X:\\wpt\\xperf.exe is not signed by Microsoft"}
        code, docs, err = self.trace()
        self.assertEqual((code, docs), (1, []))
        self.assertEqual(self.steps(), [("preflight", "abandon"), ("start", "finish"), ("stop", "ignore")])
        self.assertIn("PC step failed: xperf at X:\\wpt\\xperf.exe is not signed by Microsoft", err)
        self.assertIn("the kernel trace was stopped", err)
        # The PC answered, so a stop that finds nothing proves that no trace ran.
        self.answers["Stop-IemTraceSessions"] = {"stopped": [], "gone": [], "kept": [], "notes": []}
        code, docs, err = self.trace()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("no kernel trace ran", err)

    def test_a_start_that_did_not_return_leaves_an_unconfirmed_stop(self) -> None:
        """A start whose reply was never read (it outlived its bound, its ssh
        session failed) may still begin its trace after a stop that found none
        or stopped one session only (#32 B5)."""
        def still_running():
            raise ip.StillRunning("ssh still running after 120 s (bounded on the PC; check 'iempc status', never force-end)")

        def ssh_failed():
            raise ip.StepError("ssh failed (exit 255): Connection reset by peer")

        for start in (still_running, ssh_failed):
            for stopped in ([], ["NT Kernel Logger"]):
                self.answers["Start-IemTrace"] = start
                self.answers["Stop-IemTraceSessions"] = {"stopped": stopped, "gone": [], "kept": [], "notes": []}
                self.pc.modules.clear()
                code, docs, err = self.trace()
                self.assertEqual(code, 1, (start, stopped))
                self.assertEqual(self.names(), ["preflight", "start", "stop"], (start, stopped))
                self.assertEqual(docs[0]["trace_stop"], "unconfirmed", (start, stopped))
                self.assertIn("WARNING: the kernel trace may still start or run", err)
                self.assertIn("Stop-IemTraceSessions -Dir", err)
                self.assertNotIn("the kernel trace was stopped", err)
        # Both sessions stopped: confirmed even after a start that did not return.
        self.answers["Stop-IemTraceSessions"] = STOPPED
        code, docs, err = self.trace()
        self.assertEqual((code, docs), (1, []))
        self.assertIn("the kernel trace was stopped", err)

    def test_a_flag_during_the_final_stop_still_checks_it_then_runs_the_event_path(self) -> None:
        def stop_then_flag(reply):
            self.flag()
            return reply

        self.answers["Stop-IemTraceSessions"] = lambda: stop_then_flag(STOPPED)
        code, docs, _ = self.trace()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.steps()[-1], ("stop", "ignore"))
        self.assertEqual((self.names()[-1], self.fetches), ("stop", []))
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        # A stop the PC did not confirm is named before the event path runs.
        self.answers["Stop-IemTraceSessions"] = lambda: stop_then_flag({"stopped": [], "gone": [], "kept": ["NT Kernel Logger"]})
        ip.EVENT_NOW.unlink()
        code, docs, err = self.trace()
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(docs[0]["trace_stop"], "failed")
        self.assertIn("the kernel trace may still run on the PC", err)
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))

    def test_a_stop_that_is_not_confirmed_fails_and_analyses_nothing(self) -> None:
        self.answers["Stop-IemTraceSessions"] = {"stopped": [], "gone": [], "kept": ["NT Kernel Logger"], "notes": []}
        code, docs, err = self.trace()
        self.assertEqual(code, 1)
        self.assertEqual(self.names(), ["preflight", "start", "stop"])
        self.assertEqual(docs[0]["trace_stop"], "failed")
        self.assertIn("the trace stop is not confirmed", err)
        self.assertIn("the kernel trace may still run on the PC", err)

    def test_a_final_stop_that_fails_names_the_trace_left_running(self) -> None:
        def cut():
            raise ip.StepError("ssh: connection reset")

        self.answers["Stop-IemTraceSessions"] = cut
        code, docs, err = self.trace()
        self.assertEqual(code, 1)
        self.assertEqual(docs[0]["trace_stop"], "failed")
        self.assertIn("connection reset", docs[0]["error"])
        self.assertIn("the kernel trace may still run on the PC", err)

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
