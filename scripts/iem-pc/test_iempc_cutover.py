"""Tests for scripts/iem-pc/iempc_cutover.py: `iempc cutover` (S8 lane 2,
#11): the guard's refusals first (`iemmode cutover --dry-run`), then
Install-IemCutover from the admin-only stage, then the guard's cutover. They
reuse iempc_test_support's fakes (FakePc stands in for ssh and scp, FakeGh for
GitHub); every value is synthetic."""
from __future__ import annotations

import json
import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_cutover as cut  # noqa: E402
from iempc_test_support import ENV, SHA, Base, ip, make_zip, sha256  # noqa: E402

HERE = Path(__file__).resolve().parent
MODULES = {"IemCutover.psm1": b"synthetic IemCutover.psm1", "tuning/IemTuningStore.psm1": b"synthetic IemTuningStore.psm1"}
UPLOADED = "X:\\root\\bootstrap\\" + SHA + "\\"
TASKS = "\\Pred\\appstart;\\Pred\\other"
RUN = "HKCU:\\Software\\Pred\\Run|app"
DRY = ("cutover", "--build", SHA, "--dry-run")
GO = ("cutover", "--build", SHA)


def installed() -> dict:
    return {"state": "installed", "task": cut.TASK, "dir": "X:\\elevated\\cutover", "tasks": ["\\Pred\\appstart", "\\Pred\\other"],
            "run": [RUN], "modules": {}}


class CutoverBase(Base):
    def setUp(self) -> None:
        super().setUp()
        self.gh.artifact = make_zip(self.tmp / "artifact-cut" / f"iemmixer-{SHA}.zip", extra=MODULES)
        self.write_env({"PC_AUTOSTART_TASKS": TASKS, "PC_AUTOSTART_RUN": RUN})
        self.install_reply: object = installed()
        self.pc.texts[cut.INSTALL] = lambda: self.install_reply() if callable(self.install_reply) else self.install_reply
        # The ops live run's verdicts on SHA (newest last by id); every other gh call is FakeGh's.
        self.live_runs: list[dict] = [{"id": 7, "name": cut.LIVE_CHECK, "status": "completed", "conclusion": "success"}]
        fake = self.gh

        def gh(args, timeout=ip.GH_S):
            if list(args[:2]) == ["api", f"repos/{ip.REPO}/commits/{SHA}/check-runs?check_name={cut.LIVE_CHECK}"]:
                return json.dumps({"total_count": len(self.live_runs), "check_runs": self.live_runs})
            return fake(args, timeout)

        self.patch(gh=gh)

    def write_env(self, extra: dict[str, str]) -> None:
        envfile = self.tmp / "iem-pc-cutover.env"
        envfile.write_text("".join(f"{k}={v}\n" for k, v in {**ENV, **extra}.items()), encoding="utf-8")
        self.patch(env_path=lambda: envfile)

    def scripts(self, mark: str) -> list[tuple[str, str]]:
        return [(s, e) for s, e in self.pc.modules if mark in s]

    def iemmode_calls(self) -> list[tuple[list[str], str]]:
        return [(args, event) for exe, args, event in self.pc.calls if exe == "iemmode.exe"]


class SequenceTests(CutoverBase):
    def test_the_command_is_dev_time_locked_and_talks_to_the_pc(self) -> None:
        spec = ip.COMMANDS["cutover"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))

    def test_the_guard_s_dry_run_then_the_install_then_the_guard_s_cutover(self) -> None:
        self.fetched()
        code, docs, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, 0, err)
        # The guard's refusals and the cutover are the guard's switch: a new flag abandons this client.
        self.assertEqual(self.iemmode_calls(), [(list(DRY), "abandon"), (list(GO), "abandon")])
        # The install changes the PC: a new flag lets it finish.
        self.assertEqual([e for _, e in self.scripts(cut.INSTALL)], ["finish"])
        self.assertEqual(docs[-1]["cutover"], "done")
        self.assertEqual((docs[-1]["sha"], docs[-1]["install"]["state"], docs[-1]["exit"]), (SHA, "installed", 0))

    def test_the_install_gets_the_modules_as_checked_and_the_env_s_autostarts(self) -> None:
        self.fetched()
        self.assertEqual(self.run_main("cutover", "--sha", SHA)[0], 0)
        self.assertEqual(self.pc.scps, [
            (str(ip.bundle_dir(SHA) / "IemPc.psm1"), f"tester@pc.test:/X:/root/bootstrap/{SHA}/IemPc.psm1", "finish"),
            (str(ip.bundle_dir(SHA) / "tuning" / "IemTuningStore.psm1"),
             f"tester@pc.test:/X:/root/bootstrap/{SHA}/IemTuningStore.psm1", "finish"),
            (str(ip.bundle_dir(SHA) / "IemCutover.psm1"), f"tester@pc.test:/X:/root/bootstrap/{SHA}/IemCutover.psm1",
             "finish")])
        script = self.scripts(cut.INSTALL)[0][0]
        pc = script.index(f"$iemB = [IO.File]::ReadAllBytes('{UPLOADED}IemPc.psm1')")
        store = script.index(f"$iemB = [IO.File]::ReadAllBytes('{UPLOADED}IemTuningStore.psm1')")
        new = script.index(f"$iemB = [IO.File]::ReadAllBytes('{UPLOADED}IemCutover.psm1')")
        self.assertLess(pc, new)
        self.assertLess(store, new)
        self.assertNotIn(f"Import-Module '{UPLOADED}", script)   # never from the run folder (#15)
        self.assertEqual(script.count("Import-Module"), 1)
        sums = {"IemPc.psm1": sha256(b"synthetic IemPc.psm1"),
                "IemTuningStore.psm1": sha256(MODULES["tuning/IemTuningStore.psm1"]),
                "IemCutover.psm1": sha256(MODULES["IemCutover.psm1"])}
        want = ("$r = & { Install-IemCutover -ModuleSha256 @{ " + "; ".join(f"'{n}' = '{h}'" for n, h in sums.items())
                + " } -Root 'X:\\root' -Tasks @('\\Pred\\appstart', '\\Pred\\other') "
                  "-RunValues @('HKCU:\\Software\\Pred\\Run|app') }")
        self.assertIn(want, script)

    def test_dry_run_asks_the_guard_only(self) -> None:
        self.fetched()
        code, docs, err = self.run_main("cutover", "--sha", SHA, "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertEqual(self.iemmode_calls(), [(list(DRY), "abandon")])
        self.assertEqual((self.pc.modules, self.pc.scps), ([], []))
        self.assertEqual((docs[-1]["cutover"], docs[-1]["tasks"], docs[-1]["run"]),
                         ("dry-run", ["\\Pred\\appstart", "\\Pred\\other"], [RUN]))
        self.assertEqual(docs[-1]["live"], {"id": 7, "conclusion": "success"})


class RefusalTests(CutoverBase):
    def test_a_guard_refusal_changes_nothing(self) -> None:
        self.fetched()
        why = f"the guard is in event: the cutover runs from dev or a live trial on {SHA}"
        self.pc.replies[DRY] = (1, json.dumps({"ok": False, "detail": why, "alarms": []}))
        code, docs, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertEqual(self.iemmode_calls(), [(list(DRY), "abandon")])
        self.assertEqual((self.pc.modules, self.pc.scps), ([], []))
        self.assertEqual((docs[-1]["cutover"], docs[-1]["reply"]["detail"]), ("refused", why))

    def test_the_event_flag_refuses_it(self) -> None:
        self.fetched()
        self.flag()
        for argv in (("cutover", "--sha", SHA), ("cutover", "--sha", SHA, "--dry-run")):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("runs only in dev time", err)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))

    def test_an_open_spike_window_refuses_it(self) -> None:
        self.fetched()
        self.open_window()
        code, _, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))

    def test_without_the_predecessor_s_autostarts_nothing_runs(self) -> None:
        self.fetched()
        self.write_env({})
        code, _, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("the private env names no autostart of the predecessor", err)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))

    def test_an_autostart_not_in_the_module_s_form_is_refused_before_the_pc(self) -> None:
        self.fetched()
        for extra in ({"PC_AUTOSTART_TASKS": "Pred\\appstart"}, {"PC_AUTOSTART_TASKS": "\\Pred\\x\\"},
                      {"PC_AUTOSTART_RUN": "HKU:\\x\\Run|app"}, {"PC_AUTOSTART_RUN": "HKCU:\\Software\\Run"},
                      {"PC_AUTOSTART_TASKS": "\\Pred\\a\"b"}):
            self.write_env(extra)
            code, _, err = self.run_main("cutover", "--sha", SHA)
            self.assertEqual(code, 1, extra)
            self.assertIn("the private env's autostarts are refused", err)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))

    def test_a_bundle_without_the_module_is_refused_before_the_pc(self) -> None:
        self.gh.artifact = make_zip(self.tmp / "artifact-old" / f"iemmixer-{SHA}.zip",
                                    extra={"tuning/IemTuningStore.psm1": b"x"})
        self.fetched()
        code, _, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("has no IemCutover.psm1", err)
        self.assertEqual((self.pc.modules, self.pc.calls), ([], []))


class LiveGateTests(CutoverBase):
    """Design section 3.2: the build's newest live/iem-pc must be green, before
    anything reaches the PC (the guard records only HIL)."""

    def run_refused(self, why: str) -> None:
        self.fetched()
        for argv in (("cutover", "--sha", SHA), ("cutover", "--sha", SHA, "--dry-run")):
            code, _, err = self.run_main(*argv)
            self.assertEqual(code, 1, argv)
            self.assertIn(why, err)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))

    def test_no_live_result_refuses_it(self) -> None:
        self.live_runs = [{"id": 9, "name": "hil/iem-pc", "status": "completed", "conclusion": "success"}]
        self.run_refused(f"{SHA} has no {cut.LIVE_CHECK} result")

    def test_a_red_or_unfinished_newest_live_result_refuses_it(self) -> None:
        for newest in ({"status": "completed", "conclusion": "failure"}, {"status": "in_progress", "conclusion": None}):
            with self.subTest(newest=newest):
                self.live_runs = [{"id": 7, "name": cut.LIVE_CHECK, "status": "completed", "conclusion": "success"},
                                  {"id": 8, "name": cut.LIVE_CHECK, **newest}]
                self.run_refused(f"the newest {cut.LIVE_CHECK} of {SHA} is {newest['status']}")

    def test_an_older_red_result_before_a_green_one_passes(self) -> None:
        self.fetched()
        self.live_runs = [{"id": 8, "name": cut.LIVE_CHECK, "status": "completed", "conclusion": "success"},
                          {"id": 3, "name": cut.LIVE_CHECK, "status": "completed", "conclusion": "failure"}]
        code, docs, err = self.run_main("cutover", "--sha", SHA, "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertEqual(docs[-1]["live"], {"id": 8, "conclusion": "success"})


class FailureTests(CutoverBase):
    def test_a_failed_install_never_runs_the_guard_s_cutover(self) -> None:
        self.fetched()
        self.pc.texts[cut.INSTALL] = "nothing like an answer"
        code, _, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("the guard's cutover did not run", err)
        self.assertEqual(self.iemmode_calls(), [(list(DRY), "abandon")])

    def test_a_cutover_that_outlives_the_client_s_bound_says_it_goes_on(self) -> None:
        self.fetched()

        def slow():
            raise ip.StillRunning("iemmode.exe still running after 540 s")

        self.pc.replies[GO] = slow
        code, _, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("the guard's cutover goes on: it ends in prod or unwinds to trial and event by itself", err)

    def test_a_failed_cutover_says_so_with_the_guard_s_reply(self) -> None:
        self.fetched()
        why = f"cutover of {SHA} failed at Checks: refused; unwound to trial and event"
        self.pc.replies[GO] = (1, json.dumps({"ok": False, "detail": why, "alarms": []}))
        code, docs, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertEqual((docs[-1]["cutover"], docs[-1]["reply"]["detail"]), ("failed", why))

    def test_a_new_flag_during_the_install_lets_it_finish_then_runs_the_event_path(self) -> None:
        self.fetched()

        def install_then_flag():
            self.flag()
            return installed()

        self.install_reply = install_then_flag
        code, docs, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([e for _, e in self.scripts(cut.INSTALL)], ["finish"])
        self.assertEqual(self.iemmode_calls(), [(list(DRY), "abandon"), (["event"], "ignore")])

    def test_a_new_flag_during_the_guard_s_cutover_runs_the_event_path(self) -> None:
        self.fetched()

        def flag_then_reply():
            self.flag()
            return (0, json.dumps({"ok": True, "detail": "cutover done", "alarms": []}))

        self.pc.replies[GO] = flag_then_reply
        code, docs, err = self.run_main("cutover", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.iemmode_calls(), [(list(DRY), "abandon"), (list(GO), "abandon"), (["event"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})


class BodyTests(unittest.TestCase):
    def test_every_value_is_quoted_and_a_quote_doubled(self) -> None:
        body = cut.install_body(ip, {"IemCutover.psm1": "a" * 64}, "X:\\root", ["\\Pred\\it's"],
                                ["HKCU:\\Software\\O'Brien\\Run|app"])
        self.assertEqual(body, f"Install-IemCutover -ModuleSha256 @{{ 'IemCutover.psm1' = '{'a' * 64}' }} "
                               "-Root 'X:\\root' -Tasks @('\\Pred\\it''s') -RunValues @('HKCU:\\Software\\O''Brien\\Run|app')")
        self.assertIn("-Tasks @() -RunValues @('x')", cut.install_body(ip, {}, "X:\\root", [], ["x"]))
        for bad in ({"a b": "a" * 64}, {"IemCutover.psm1": "A" * 64}, {"IemCutover.psm1": "a" * 63}):
            with self.assertRaises(ValueError):
                cut.install_body(ip, bad, "X:\\root", [], [])

    def test_the_install_answer_must_name_the_cutover_task(self) -> None:
        self.assertEqual(cut.check_install(ip, installed()), installed())
        for r in ({"state": "installed", "task": "\\iemmixer\\other"}, {"state": "refused"}, "ok", None):
            with self.assertRaises(ip.StepError, msg=repr(r)):
                cut.check_install(ip, r)


class AgreementTests(unittest.TestCase):
    """The dev box and the PC module name the same task, verbs and forms."""

    def test_the_module_and_the_dev_box_agree(self) -> None:
        text = (HERE / "IemCutover.psm1").read_text(encoding="ascii")
        folder, name = cut.TASK.rsplit("\\", 1)
        self.assertIn(f"$script:DefaultFolder = '{folder}'", text)
        self.assertIn(f"$script:TaskName = '{name}'", text)
        self.assertIn("Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1')", text)
        self.assertIn("Import-Module (Join-Path $PSScriptRoot 'IemTuningStore.psm1')", text)
        self.assertEqual([n for _, n in cut.STAGE], ["IemPc.psm1", "IemTuningStore.psm1", "IemCutover.psm1"])
        # The guard's names (crates/iem-guard/src/cutover.rs): the task, its kind, the verbs, the export.
        rust = (HERE.parent.parent / "crates" / "iem-guard" / "src" / "cutover.rs").read_text(encoding="utf-8")
        self.assertIn('pub const TASK: &str = r"' + cut.TASK + '";', rust)
        self.assertIn('pub const KIND: &str = "cutover";', rust)
        self.assertIn("'cutover.result.json'", text)
        self.assertIn("Read-IemTaskRequest -Root $Root -Kind 'cutover'", text)
        for verb in ("autostarts-off", "autostarts-on", "logon-on", "logon-off"):
            self.assertIn(f'"{verb}"', rust)
            self.assertIn(f"'{verb}'", text)
        self.assertIn('pub const EXPORT_PREFIX: &str = "autostarts-";', rust)
        self.assertIn("$script:ExportPattern = '^autostarts-[0-9]{1,20}$'", text)
        # The env's forms are the module's.
        self.assertIn("$script:TaskPathPattern = '^" + cut.TASK_PATH.pattern + "$'", text)
        self.assertIn("$script:RunPattern = '^" + cut.RUN_VALUE.pattern + "$'", text)

    def test_the_bundle_job_ships_the_module_and_the_windows_job_tests_it(self) -> None:
        ci = (HERE.parent.parent / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        bundle = ci[ci.index("\n  bundle:\n"):ci.index("\n  attest:\n")]
        self.assertRegex(bundle, r"Copy-Item -LiteralPath [^\n]*scripts/iem-pc/IemPc\.psm1[^\n]*scripts/iem-pc/IemCutover\.psm1")
        windows = ci[ci.index("\n  windows:\n"):ci.index("\n  bundle:\n")]
        self.assertIn("-File scripts/iem-pc/Test-IemCutover.ps1", windows)

    def test_no_site_values_in_the_module(self) -> None:
        for path in (Path(cut.__file__), HERE / "IemCutover.psm1"):
            text = path.read_text(encoding="utf-8")
            self.assertIsNone(re.search(r"\b\d{1,3}(?:\.\d{1,3}){3}\b", text), f"{path.name}: an IPv4 address")
            self.assertIsNone(re.search(r"\b[A-Za-z]:\\", text), f"{path.name}: a Windows drive path")
            self.assertIsNone(re.search(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+", text), f"{path.name}: an ssh destination or email")


if __name__ == "__main__":
    unittest.main()
