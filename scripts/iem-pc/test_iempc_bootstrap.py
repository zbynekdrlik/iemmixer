"""Tests for scripts/iem-pc/iempc_bootstrap.py: bootstrap and handover-s1a
(split out of test_iempc.py, #36). They reuse iempc_test_support's fakes
(FakePc stands in for ssh and scp, FakeGh for GitHub); every value is
synthetic."""
from __future__ import annotations

import json
import sys
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from iempc_test_support import RUN, SHA, SHA2, Base, ip, make_zip, sha256  # noqa: E402


class BootstrapTests(Base):
    MODULE = f"X:\\root\\bootstrap\\{SHA}\\IemPc.psm1"

    def test_functions_and_their_parameters(self) -> None:
        self.assertEqual(ip.ps_params(["-Service", "svc name", "-WhatIf", "-20"]), " -Service 'svc name' -WhatIf '-20'")
        for bad in (["--sha", SHA], ['-Name', 'a"b']):
            with self.assertRaises(ip.Refused, msg=str(bad)):
                ip.ps_params(bad)
        self.assertEqual([ip.read_only_function(f) for f in ("Get-IemBootstrapState", "Test-IemServiceRight",
                                                               "Grant-IemServiceRight", "Register-IemTasks", "Get-Process")],
                         [True, True, False, False, False])

    def test_every_bootstrap_step_is_dev_time_and_locked(self) -> None:
        """Read-only functions too: each run makes a folder, copies the module
        and runs it elevated on the PC (design §6, P10)."""
        spec = ip.COMMANDS["bootstrap"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))

    def test_the_verified_module_is_shipped_and_imported_by_its_hash(self) -> None:
        self.fetched()
        self.pc.module_result = {"reaper": 1}
        code, docs, _ = self.run_main("bootstrap", "Grant-IemServiceRight", "-Service", "svc name")
        self.assertEqual(code, 0)
        self.assertEqual(self.pc.scps, [(str(ip.bundle_dir(SHA) / "IemPc.psm1"),
                                         f"tester@pc.test:/X:/root/bootstrap/{SHA}/IemPc.psm1", "finish")])
        script, mode = self.pc.modules[-1]
        self.assertEqual(mode, "finish")
        self.assertIn(f"$iemB = [IO.File]::ReadAllBytes('{self.MODULE}')", script)
        self.assertIn(f"$iemH -cne '{sha256(b'synthetic IemPc.psm1')}'", script)
        self.assertIn("Import-Module $iemMod -Force ; $r = & { Grant-IemServiceRight -Service 'svc name' }", script)
        self.assertNotIn(f"Import-Module '{self.MODULE}'", script)   # never from the run folder (#15)
        self.assertEqual(docs[-1], {"bootstrap": "Grant-IemServiceRight", "sha": SHA, "result": {"reaper": 1}})

    def test_a_read_only_step_is_refused_during_an_event(self) -> None:
        self.fetched()
        self.flag()
        for fn in ("Get-IemBootstrapState", "Get-IemPredecessorFacts", "Test-IemServiceRight"):
            code, docs, err = self.run_main("bootstrap", fn)
            self.assertEqual((code, docs), (1, []), fn)
            self.assertIn("runs only in dev time", err, fn)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))

    def test_a_new_flag_abandons_a_read_only_step(self) -> None:
        self.fetched()
        self.assertEqual(self.run_main("bootstrap", "Get-IemBootstrapState")[0], 0)
        self.assertEqual(({m for _, m in self.pc.modules}, {s[2] for s in self.pc.scps}), ({"abandon"}, {"abandon"}))

    def test_the_runner_token_reaches_the_pc_on_stdin_only(self) -> None:
        self.fetched()
        code, _, _ = self.run_main("bootstrap", "Register-IemRunner")
        self.assertEqual(code, 0)
        self.assertEqual(self.gh.named("api", "-X"), [["api", "-X", "POST", f"repos/{ip.OPS_REPO}/actions/runners/registration-token",
                                                       "--jq", ".token"]])
        script = self.pc.modules[-1][0]
        self.assertIn(f"try {{ $env:ACTIONS_RUNNER_INPUT_TOKEN = '{self.gh.runner_reply}' ; ", script)
        self.assertIn("finally { Remove-Item -Path 'Env:\\ACTIONS_RUNNER_INPUT_TOKEN' -ErrorAction SilentlyContinue }", script)
        self.assertFalse(any(self.gh.runner_reply in " ".join(s) for s in self.pc.scps))
        self.gh.runner_reply = "an unexpected form!"
        before = (len(self.pc.modules), len(self.pc.scps))
        self.assertEqual(self.run_main("bootstrap", "Register-IemRunner")[0], 1)
        self.assertEqual((len(self.pc.modules), len(self.pc.scps)), before)  # refused before the PC

    def test_unknown_functions_and_missing_bundles_are_refused(self) -> None:
        code, _, err = self.run_main("bootstrap", "Get-IemBootstrapState")
        self.assertEqual((code, self.pc.modules), (1, []))
        self.assertIn("no fetched bundle", err)
        self.fetched()
        for argv in (["bootstrap", "Invoke-Expression"], ["bootstrap", "Get-IemX", "--sha", SHA]):
            self.assertEqual(self.run_main(*argv)[0], 1, argv)
        self.assertEqual(self.pc.modules, [])

    def test_the_newest_fetched_bundle_is_the_default(self) -> None:
        self.fetched()
        self.gh.artifact = make_zip(self.tmp / "artifact2" / f"iemmixer-{SHA2}.zip", sha=SHA2, run=RUN + 1)
        self.gh.runs.append({"databaseId": RUN + 1, "headSha": SHA2, "event": "push", "headBranch": "dev", "conclusion": "success"})
        self.gh.jobs[RUN + 1] = self.gh.jobs[RUN]
        self.assertEqual(self.run_main("fetch-bundle", "--sha", SHA2)[0], 0)
        for older, newer in ((SHA, SHA2), (SHA2, SHA)):
            for sha, at in ((older, "2026-01-01T00:00:00+00:00"), (newer, "2026-09-27T12:00:00+00:00")):
                rec = ip.load_record(sha)
                rec["fetched_at"] = at
                ip.write_json(ip.bundle_dir(sha) / "fetch.json", rec)
            self.assertEqual(ip.latest_record_sha(), newer)


class HandoverTests(Base):
    def setUp(self) -> None:
        super().setUp()
        self.sw = ip.spike_module()
        saved = {n: getattr(self.sw, n) for n in ("STATE", "load_env", "ps")}
        self.addCleanup(lambda: [setattr(self.sw, n, v) for n, v in saved.items()])
        self.sw.STATE = ip.SPIKE_STATE
        self.sw.load_env = lambda path: {"PC_BUFFER_KEY": "K", "PC_BUFFER_NAME": "N", "PC_ASIO_MODULE": "testcard.dll"}
        self.checks = {"pref": 64, "holders": [], "spike": 0, "task": False}
        self.bodies: list[tuple[str, str]] = []

        def fake_ps(env, body, timeout=300, event="finish"):
            self.bodies.append((body, event))
            return dict(self.checks)

        self.sw.ps = fake_ps

    def state(self) -> dict:
        return json.loads(ip.SPIKE_STATE.read_text(encoding="utf-8"))

    def test_a_free_quiet_card_closes_the_window(self) -> None:
        self.open_window(pref_current=64)
        code, docs, _ = self.run_main("handover-s1a")
        self.assertEqual((code, self.state()["closed"], self.state()["handed_over"]["checks"]), (0, True, self.checks))
        body, mode = self.bodies[0]
        self.assertEqual(mode, "abandon")
        self.assertIn("Get-SpikeBufferPref -Key 'K' -Name 'N'", body)
        self.assertIn("Get-GoldenAsioHolders -Module 'testcard.dll'", body)
        self.assertIn("Get-ScheduledTask -TaskPath '\\iemmixer\\' -TaskName 'iemmixer-asio-spike'", body)
        self.assertEqual(docs[-1], {"handover-s1a": "w1", "closed": True, "checks": self.checks})

    def test_each_problem_keeps_the_window_open(self) -> None:
        self.open_window()
        for change, words in (({"pref": 32}, "reads 32, the original is 64"), ({"holders": ["testcard-host.exe:9"]}, "held"),
                              ({"spike": 1}, "a spike runs"), ({"task": True}, "the spike task runs")):
            self.checks = dict({"pref": 64, "holders": [], "spike": 0, "task": False}, **change)
            code, _, err = self.run_main("handover-s1a")
            self.assertEqual((code, self.state()["closed"]), (1, False), change)
            self.assertIn(words, err, change)

    def test_the_problems_on_both_sides(self) -> None:
        good = {"pref": 64, "holders": [], "spike": 0, "task": False}
        self.assertEqual(ip.handover_problems(good, 64), [])
        self.assertEqual(len(ip.handover_problems(good, 32)), 1)
        self.assertEqual(len(ip.handover_problems({"pref": 32, "holders": ["x:1"], "spike": 2, "task": True}, 64)), 4)

    def test_a_window_with_reaper_on_the_card_or_a_closed_one_is_not_handed_over(self) -> None:
        for kw, words in (({"card": "reaper"}, "not free"), ({"card": "switching"}, "not free"), ({"closed": True}, "already closed")):
            self.open_window(**kw)
            code, _, err = self.run_main("handover-s1a")
            self.assertEqual(code, 1, kw)
            self.assertIn(words, err, kw)
        self.assertEqual(self.bodies, [])

    def test_a_new_flag_during_the_checks_preempts_the_window(self) -> None:
        self.open_window()

        def flag_appears(env, body, timeout=300, event="finish"):
            self.flag()
            raise self.sw.EventNow()

        self.sw.ps = flag_appears
        code, _, _ = self.run_main("handover-s1a")
        self.assertEqual((code, self.spike_log.read_text(encoding="utf-8")), (ip.PREEMPTED, "preempt\n"))
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertFalse(self.state()["closed"])

    # F2 round 3, m5: the hand-over writes the state as saved when it closes it,
    # under the window lock, never the dict it read before its PC checks.
    def changing_meanwhile(self, change) -> None:
        def ps(env, body, timeout=300, event="finish"):
            st = self.state()
            change(st)
            ip.SPIKE_STATE.write_text(json.dumps(st), encoding="utf-8")   # another window process saved meanwhile
            return dict(self.checks)

        self.sw.ps = ps

    def test_a_change_saved_during_the_checks_is_kept(self) -> None:
        self.open_window(pref_current=64, runs=[])
        self.changing_meanwhile(lambda st: st["runs"].append({"request": "spike-1"}))
        code, _, err = self.run_main("handover-s1a")
        self.assertEqual(code, 0, err)
        st = self.state()
        self.assertEqual((st["closed"], st["runs"]), (True, [{"request": "spike-1"}]))
        self.assertIn("handed_over", st)

    def test_a_window_a_preempt_closed_during_the_checks_is_not_handed_over(self) -> None:
        self.open_window(pref_current=64)
        self.changing_meanwhile(lambda st: st.update(card="reaper", closed=True))
        code, _, err = self.run_main("handover-s1a")
        self.assertEqual(code, 1)
        self.assertIn("closed meanwhile", err)
        st = self.state()
        self.assertEqual((st["card"], st["closed"]), ("reaper", True))
        self.assertNotIn("handed_over", st)

    def test_a_pc_change_in_flight_keeps_the_window_open(self) -> None:
        # F2 round 3, MAJOR: a window step still changing the PC (a set-buffer write,
        # an enter) is no free card to hand over.
        self.open_window(pref_current=64, in_flight={"step": "set-buffer", "started": time.time(), "bound_s": 60})
        code, _, err = self.run_main("handover-s1a")
        self.assertEqual(code, 1)
        self.assertIn("in flight", err)
        self.assertFalse(self.state()["closed"])

    def test_a_card_taken_back_during_the_checks_keeps_the_window_open(self) -> None:
        self.open_window(pref_current=64)
        self.changing_meanwhile(lambda st: st.update(card="switching"))
        code, _, err = self.run_main("handover-s1a")
        self.assertEqual(code, 1)
        self.assertIn("not free", err)
        self.assertEqual((self.state()["card"], self.state()["closed"]), ("switching", False))


if __name__ == "__main__":
    unittest.main()
