"""Tests for scripts/iem-pc/iempc.py and iempc_core.py: the env, the waits,
the composed scripts, the replies, status, dev, the pre-emption of a waiting
command, the lock and gh. A fake ssh runner stands in for the PC and a fake gh
for GitHub; every value is synthetic, and the private env file is never read
(a temp file takes its place). The fakes and `Base` live in
iempc_test_support.py; the event path, the bundle path and bootstrap have
test files of their own (#36)."""
from __future__ import annotations

import fcntl
import json
import os
import re
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from iempc_test_support import ENV, MODULES, OK, REAL_GH, SHA, Base, ip  # noqa: E402


class EnvTests(Base):
    def write(self, text: str) -> Path:
        p = self.tmp / "other.env"
        p.write_text(text, encoding="utf-8")
        return p

    def test_a_complete_env_loads_and_the_bin_defaults_to_the_root(self) -> None:
        env = ip.load_env(self.write('# private\nPC_SSH="tester@pc.test"\nPC_ROOT=X:\\root\\\nPC_ROOT_SCP=/X:/root\n'))
        self.assertEqual((env["PC_SSH"], env["PC_BIN"]), ("tester@pc.test", "X:\\root\\bin"))
        env = ip.load_env(self.write("PC_SSH=t@h\nPC_ROOT=X:\\root\nPC_ROOT_SCP=/X:/root\nPC_BIN=Y:\\b\n"))
        self.assertEqual(env["PC_BIN"], "Y:\\b")

    def test_missing_keys_and_files_are_named(self) -> None:
        with self.assertRaisesRegex(ip.StepError, "missing PC_ROOT_SCP"):
            ip.load_env(self.write("PC_SSH=t@h\nPC_ROOT=X:\\root\n"))
        with self.assertRaisesRegex(ip.StepError, "not KEY=VALUE"):
            ip.load_env(self.write("PC_SSH\n"))
        with self.assertRaisesRegex(ip.StepError, "missing"):
            ip.load_env(self.tmp / "absent.env")

    def test_an_ssh_target_that_looks_like_an_option_is_refused(self) -> None:
        with self.assertRaisesRegex(ip.StepError, "not an option"):
            ip.load_env(self.write("PC_SSH=-oProxyCommand=x\nPC_ROOT=X:\\root\nPC_ROOT_SCP=/X:/root\n"))

    real_env_path = staticmethod(ip.env_path)  # Base replaces ip.env_path with a temp file

    def test_the_env_file_is_pc_env_or_the_private_default(self) -> None:
        with mock.patch.dict(os.environ, {"PC_ENV": "/elsewhere/pc.env"}):
            self.assertEqual(self.real_env_path(), Path("/elsewhere/pc.env"))
        with mock.patch.dict(os.environ, {}, clear=False):
            os.environ.pop("PC_ENV", None)
            self.assertEqual(self.real_env_path(), Path.home() / ".config/iemmixer/iem-pc.env")

    def test_no_site_values_in_the_module(self) -> None:
        repos = set()
        for module in MODULES:   # iempc.py and the modules it is split into (#36)
            text = Path(module.__file__).read_text(encoding="utf-8")
            self.assertIsNone(re.search(r"\b\d{1,3}(?:\.\d{1,3}){3}\b", text), ("an IPv4 address", module.__name__))
            self.assertIsNone(re.search(r"\b[A-Za-z]:\\", text), ("a Windows drive path", module.__name__))
            self.assertIsNone(re.search(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+", text), ("an ssh destination or email", module.__name__))
            self.assertIsNone(re.search(r"\b[a-z0-9-]+\.(?:lan|home|internal|corp)\b", text), ("a site host name", module.__name__))
            repos |= set(re.findall(r"zbynekdrlik/[\w-]+", text))
        self.assertEqual(repos, {ip.REPO, ip.OPS_REPO})


class GuardedTests(Base):
    """guarded() with local commands standing in for ssh."""

    def py(self, code: str) -> list[str]:
        return [sys.executable, "-c", code]

    def test_output_and_stdin_pass_through(self) -> None:
        out = ip.guarded(self.py("import sys, time; time.sleep(0.2); print(sys.stdin.read().upper())"), "hello\n", 10, "finish")
        self.assertEqual(out.strip(), "HELLO")

    def test_a_failing_command_raises(self) -> None:
        with self.assertRaisesRegex(ip.StepError, "exit 3"):
            ip.guarded(self.py("import sys; sys.exit(3)"), "", 10, "finish")

    def test_abandon_returns_within_a_poll(self) -> None:
        self.flag()
        t = time.monotonic()
        with self.assertRaises(ip.EventNow):
            ip.guarded(self.py("import time; time.sleep(5)"), "", 10, "abandon")
        self.assertLess(time.monotonic() - t, 2.0)

    def test_finish_completes_the_call_first(self) -> None:
        self.flag()
        t = time.monotonic()
        with self.assertRaises(ip.EventNow):
            ip.guarded(self.py("import time; time.sleep(0.5)"), "", 10, "finish")
        self.assertGreaterEqual(time.monotonic() - t, 0.5)

    def test_ignore_is_for_the_preemption_itself(self) -> None:
        self.flag()
        self.assertEqual(ip.guarded(self.py("print('ok')"), "", 10, "ignore").strip(), "ok")

    def test_a_call_past_its_bound_is_reported_never_force_ended(self) -> None:
        done = self.tmp / "bounded.done"
        with self.assertRaisesRegex(ip.StillRunning, "still running after 0.3 s .*never force-end"):
            ip.guarded(self.py(f"import time; time.sleep(1); open({str(done)!r}, 'w').close()"), "", 0.3, "finish")
        self.assertFalse(done.exists())
        deadline = time.monotonic() + 10
        while not done.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertTrue(done.exists(), "the call was left to end by itself")

    def test_a_flag_seen_during_a_finish_call_counts_even_when_gone_at_the_end(self) -> None:
        flag = str(ip.EVENT_NOW)
        code = (f"import pathlib, time; p = pathlib.Path({flag!r}); p.parent.mkdir(parents=True, exist_ok=True); "
                "p.write_text('x'); time.sleep(0.5); p.unlink(); time.sleep(0.3)")
        with self.assertRaises(ip.EventNow):
            ip.guarded(self.py(code), "", 10, "finish")
        self.assertFalse(ip.event_now())

    def test_only_a_flag_that_appears_after_the_start_preempts(self) -> None:
        args = type("A", (), {})()
        self.assertEqual([ip.Ctx(ENV, args, False).watch(True), ip.Ctx(ENV, args, False).watch(False)], ["abandon", "finish"])
        self.assertEqual([ip.Ctx(ENV, args, True).watch(True), ip.Ctx(ENV, args, True).watch(False)], ["ignore", "ignore"])


class ScriptTests(Base):
    # #15, the last lane, item 2: the elevated session loads modules only from Windows
    # PowerShell's own folders, pinned before its first command, and ssh starts
    # powershell.exe by its full path.
    PIN = ("$env:PSModulePath = [IO.Path]::Combine($PSHOME, 'Modules') + ';' + "
           "[IO.Path]::Combine([Environment]::GetFolderPath('ProgramFiles'), 'WindowsPowerShell\\Modules')")

    def test_every_script_pins_the_module_path_before_any_command(self) -> None:
        for script, eap in ((ip.native_script("x.exe", ["a"], ("CHECK",)), "Continue"),
                            (ip.module_script("Get-X", module="X:\\m.psm1", module_hex="cd" * 32, pre="P ; "), "Stop")):
            self.assertEqual(script.count("$env:PSModulePath"), 1, script)
            at = script.index(self.PIN)
            self.assertEqual(script[:at], f"$ErrorActionPreference = '{eap}'\n$ProgressPreference = 'SilentlyContinue'\n")
            self.assertTrue(script[at + len(self.PIN):].startswith(" ; try { "), script)

    def test_ssh_starts_windows_powershell_by_its_full_path(self) -> None:
        self.assertEqual(ip.ssh_cmd(ENV)[-1], "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe "
                                              "-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command -")

    def test_the_program_and_each_argument_are_quoted(self) -> None:
        s = ip.native_script("X:\\root\\bin\\iemmode.exe", ["install", "X:\\it's\\a.zip"], ("CHECK",))
        self.assertEqual(s.splitlines()[0], "$ErrorActionPreference = 'Continue'")
        self.assertIn("try { CHECK ; $x = 'X:\\root\\bin\\iemmode.exe' ; $a = @('install', 'X:\\it''s\\a.zip') ; ", s)
        self.assertTrue(s.endswith("ConvertTo-Json -InputObject $o -Compress"))
        self.assertIn("$a = @() ;", ip.native_script("x.exe", []))

    def test_every_powershell_single_quote_is_doubled(self) -> None:
        self.assertEqual(ip.ps_quote("a'b\u2019c\u2018d\u201ae\u201bf"), "'a''b\u2019\u2019c\u2018\u2018d\u201a\u201ae\u201b\u201bf'")

    def test_double_quotes_and_control_characters_never_reach_the_pc(self) -> None:
        for bad in ('a"b', "a\nb", "a\tb"):
            with self.assertRaises(ip.StepError, msg=bad):
                ip.ps_args([bad])

    def test_the_hash_check_throws_on_a_mismatch(self) -> None:
        check = ip.hash_check("X:\\z.zip", "ab" * 32)
        self.assertEqual(check, "if ((Get-FileHash -LiteralPath 'X:\\z.zip' -Algorithm SHA256 -ErrorAction Stop).Hash."
                                "ToLowerInvariant() -ne '" + "ab" * 32 + "') { throw ('sha256 mismatch: ' + 'X:\\z.zip') }")
        for bad in ("AB" * 32, "ab" * 31, "zz" * 32):
            with self.assertRaises(ip.StepError, msg=bad):
                ip.hash_check("X:\\z.zip", bad)

    def test_a_module_is_imported_only_from_its_admin_only_stage(self) -> None:
        """#15: the upload in the user's root is read once and checked; those
        bytes go into <elevated root>\\bootstrap-stage (admin-only, read back),
        are checked again there, and only that copy is imported."""
        hexd = "cd" * 32
        s = ip.module_script("Get-IemBootstrapState", module="X:\\run\\IemPc.psm1", module_hex=hexd)
        line = s.splitlines()[2]
        order = ["[IO.File]::ReadAllBytes('X:\\run\\IemPc.psm1')", f"$iemH -cne '{hexd}'",
                 "$iemRoot = (Join-Path ([Environment]::GetFolderPath('CommonApplicationData')) 'iemmixer')",
                 "$iemStage = Join-Path $iemRoot 'bootstrap-stage'", "& $iemDir $iemRoot", "& $iemDir $iemStage",
                 "$iemMod = Join-Path $iemStage 'IemPc.psm1'", "[IO.File]::Delete($iemMod)",
                 "[IO.File]::WriteAllBytes($iemMod, $iemB)", "& $iemOnly $iemMod",
                 f"(Get-FileHash -LiteralPath $iemMod -Algorithm SHA256).Hash.ToLowerInvariant() -cne '{hexd}'",
                 "Import-Module $iemMod -Force", "$r = & { Get-IemBootstrapState }"]
        at = 0   # each step after the one before (the keep check reads the copy back too, #15 review)
        for step in order:
            at = line.index(step, at)
        # The upload is read once (and named in the mismatch); nothing else reads or imports it.
        self.assertEqual(line.count("'X:\\run\\IemPc.psm1'"), 2)
        self.assertEqual(line.count("Import-Module"), 1)
        # A folder: created with its security in one step, owner checked, the DACL set again, read back.
        mk = line[line.index("$iemDir = {"):]
        mk = [mk.index(t) for t in ("SetOwner((New-Object System.Security.Principal.SecurityIdentifier 'S-1-5-32-544'))",
                                    "SetAccessRuleProtection($true, $false)", "[IO.Directory]::CreateDirectory($d, $s)",
                                    "& $iemOwn $d", "[IO.Directory]::SetAccessControl($d, $s)", "& $iemOnly $d")]
        self.assertEqual(mk, sorted(mk))
        # The CI runner's temp elevated root (Test-IemStage.ps1).
        self.assertIn("$iemRoot = 'Y:\\er' ; ", ip.module_script("B", module="X:\\m.psm1", module_hex=hexd, elevated_root="Y:\\er"))
        for bad in ("CD" * 32, "cd" * 31):
            with self.assertRaises(ip.StepError, msg=bad):
                ip.module_script("B", module="X:\\m.psm1", module_hex=bad)
        self.assertNotIn("finally", s)
        self.assertIn("} finally { F } ; ConvertTo-Json", ip.module_script("B", pre="P ; ", fin="F"))
        self.assertIn("try { P ; $r = & { B }", ip.module_script("B", pre="P ; ", fin="F"))

    def test_the_last_line_is_the_envelope(self) -> None:
        self.assertEqual(ip.last_json("noise\n{\"exit\": 0}\n\n"), {"exit": 0})
        self.assertEqual(ip.last_json("\ufeff{\"a\": 1}"), {"a": 1})
        for bad, words in (("", "no output"), ("noise", "not JSON"), ("[1]", "not a JSON object")):
            with self.assertRaisesRegex(ip.StepError, words):
                ip.last_json(bad)

    def test_a_program_that_did_not_start_is_an_error(self) -> None:
        self.patch(ssh_ps=lambda env, script, timeout, event: json.dumps({"exit": None, "out": None, "err": "not recognized"}))
        with self.assertRaisesRegex(ip.StepError, "iemmode.exe did not run on the PC: not recognized"):
            ip.iemmode(ENV, ["status"], 10, "abandon")
        self.patch(ssh_ps=lambda env, script, timeout, event: json.dumps({"exit": "zero", "out": "", "err": ""}))
        with self.assertRaisesRegex(ip.StepError, "non-numeric"):
            ip.iemmode(ENV, ["status"], 10, "abandon")

    def test_a_module_error_is_raised(self) -> None:
        self.patch(ssh_ps=lambda env, script, timeout, event: json.dumps({"ok": False, "error": "access denied"}))
        with self.assertRaisesRegex(ip.StepError, "PC step failed: access denied"):
            ip.run_module(ENV, "Get-IemBootstrapState", 10, "abandon")


class ReplyTests(Base):
    def test_iemmode_json_is_parsed_in_any_layout(self) -> None:
        doc = {"mode": "dev", "alarms": []}
        for text in (json.dumps(doc), json.dumps(doc, indent=2) + "\n", "\ufeff" + json.dumps(doc), "a log line\n" + json.dumps(doc)):
            self.assertEqual(ip.parse_reply(text), doc, text)

    def test_empty_or_non_object_replies_are_refused(self) -> None:
        for bad, words in (("  \n", "printed nothing"), ("[1, 2]", "not a JSON object"), ("mode: dev", "not JSON")):
            with self.assertRaisesRegex(ip.StepError, words):
                ip.parse_reply(bad)

    def test_owner_questions_are_the_unacknowledged_owner_alarms(self) -> None:
        # The guard's Alarm serializes `acked` (crates/iem-guard/src/alarms.rs).
        reply = {"alarms": [{"id": 1, "owner_question": True}, {"id": 2, "owner_question": True, "acked": True},
                            {"id": 3, "owner_question": False}, "text", {"id": 4}, {"id": 5, "owner_question": "yes"},
                            {"id": 6, "owner_question": True, "acked": False}]}
        self.assertEqual(ip.owner_alarms(reply), [{"id": 1, "owner_question": True},
                                                  {"id": 6, "owner_question": True, "acked": False}])
        self.assertEqual((ip.owner_alarms(None), ip.owner_alarms({"alarms": "x"}), ip.owner_alarms({})), ([], [], []))

    def test_an_owner_question_is_printed_for_the_agent(self) -> None:
        self.pc.replies[("status",)] = (0, json.dumps({"mode": "dev", "alarms": [{"id": 7, "owner_question": True}]}))
        code, docs, err = self.run_main("status")
        self.assertEqual(code, 0)
        self.assertIn('OWNER QUESTION (send the prepared question from the ops runbook): {"id": 7', err)

    def test_a_non_json_reply_is_kept_on_failure_and_refused_on_success(self) -> None:
        self.pc.replies[("status",)] = (1, "usage: iemmode ...")
        code, docs, _ = self.run_main("status")
        self.assertEqual((code, docs[-1]["reply"], docs[-1]["output"]), (1, None, "usage: iemmode ..."))
        self.pc.replies[("status",)] = (0, "usage: iemmode ...")
        code, docs, err = self.run_main("status")
        self.assertEqual((code, docs), (1, []))
        self.assertIn("not JSON", err)

    def test_the_pcs_stderr_is_reported_only_when_there_is_some(self) -> None:
        self.pc.replies[("status",)] = (0, OK, "a warning from the PC")
        code, docs, _ = self.run_main("status")
        self.assertEqual((code, docs[-1]["stderr"]), (0, "a warning from the PC"))
        self.pc.replies[("status",)] = (0, OK, "")
        self.assertNotIn("stderr", self.run_main("status")[1][-1])


class StatusTests(Base):
    def test_status_reports_the_guard_and_this_box(self) -> None:
        self.open_window()
        code, docs, _ = self.run_main("status")
        self.assertEqual(code, 0)
        self.assertEqual({k: docs[-1][k] for k in ("iemmode", "exit", "reply", "event_now", "spike_window_open", "dev_entry")},
                         {"iemmode": ["status"], "exit": 0, "reply": {"ok": True, "alarms": []}, "event_now": False,
                          "spike_window_open": True, "dev_entry": 0})
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "abandon")])

    def test_during_an_event_status_reports_this_box_only(self) -> None:
        self.flag()
        code, docs, _ = self.run_main("status")
        self.assertEqual((code, self.pc.calls, self.pc.modules), (0, [], []))
        self.assertEqual({k: docs[-1][k] for k in ("iemmode", "event_now", "spike_window_open", "dev_entry")},
                         {"iemmode": None, "event_now": True, "spike_window_open": False, "dev_entry": 0})
        self.assertIn("EVENT-NOW exists: no PC step during an event", docs[-1]["skipped"])

    def test_status_pc_asks_the_guard_during_an_event_and_passes_the_exit_through(self) -> None:
        self.flag()
        self.pc.replies[("status",)] = (4, OK)
        code, docs, _ = self.run_main("status", "--pc")
        self.assertEqual((code, docs[-1]["event_now"], "skipped" in docs[-1]), (4, True, False))
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["status"], "ignore")])

    def test_the_spike_window_state(self) -> None:
        self.assertFalse(ip.spike_window_open())
        self.open_window()
        self.assertTrue(ip.spike_window_open())
        self.open_window(closed=True)
        self.assertFalse(ip.spike_window_open())
        ip.SPIKE_STATE.write_text("{broken", encoding="utf-8")
        self.assertTrue(ip.spike_window_open())


class PreemptionTests(Base):
    """"ide event" while another command waits: the flag file appears."""

    def test_a_new_flag_during_dev_abandons_the_client_and_runs_the_event_path(self) -> None:
        """The guard owns the switch and pre-empts it itself: the client is
        abandoned (a wait sees the flag within a poll, GuardedTests)."""
        self.pc.replies[("dev", "--build", SHA)] = lambda: (self.flag(), (0, OK))[1]
        code, docs, _ = self.run_main("dev", "--build", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["dev", "--build", SHA], "abandon"), ("iemmode.exe", ["event"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertEqual(ip.current_entry(), 0)

    def test_a_failed_step_after_a_new_flag_runs_the_event_path(self) -> None:
        def fail():
            self.flag()
            raise ip.StepError("ssh: connection reset")

        self.pc.replies[("rehearse-teardown",)] = fail
        code, docs, _ = self.run_main("rehearse-teardown")
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(docs[0]["event"], "ide event (flag file) after a failed step")
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["event"], "ignore"))

    def test_a_failed_step_without_the_flag_does_not(self) -> None:
        def fail():
            raise ip.StepError("ssh: connection reset")

        self.pc.replies[("rehearse-teardown",)] = fail
        code, docs, _ = self.run_main("rehearse-teardown")
        self.assertEqual((code, docs, len(self.pc.calls)), (1, [], 1))

    def test_a_read_only_call_started_during_an_event_is_not_preempted_again(self) -> None:
        self.flag()

        def fail():
            raise ip.StepError("ssh: timeout")

        self.pc.replies[("status",)] = fail
        code, docs, _ = self.run_main("status", "--pc")
        self.assertEqual((code, docs, len(self.pc.calls)), (1, [], 1))

    def test_a_dev_box_command_never_starts_the_event_path(self) -> None:
        self.gh.runs = []
        self.gh.on_list = self.flag
        code, _, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, self.pc.calls), (1, []))
        self.assertIn("no green push run", err)

    def test_a_failed_event_after_a_preemption_is_reported(self) -> None:
        self.pc.replies[("probe-task",)] = lambda: (self.flag(), (0, OK))[1]
        self.pc.replies[("event",)] = (1, OK)
        code, _, err = self.run_main("probe-task")
        self.assertEqual(code, 1)
        self.assertIn("alarm the owner now", err)


class DevTests(Base):
    def test_every_successful_dev_opens_a_new_entry(self) -> None:
        code, docs, _ = self.run_main("dev", "--build", SHA)
        self.assertEqual((code, docs[-1]["dev_entry"], ip.current_entry()), (0, 1, 1))
        self.assertEqual(self.run_main("dev")[1][-1]["dev_entry"], 2)
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["dev"], "abandon"))
        self.assertEqual(self.pc.timeouts, [ip.SWITCH_S, ip.SWITCH_S])

    def test_an_open_spike_window_refuses_dev_and_the_rehearsal(self) -> None:
        self.open_window()
        for argv in (["dev", "--build", SHA], ["dev", "--dry-run"], ["rehearse-teardown"]):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("'iempc handover-s1a' has handed the card over", err, argv)
        self.assertEqual(self.pc.calls, [])
        self.open_window(closed=True)
        self.assertEqual((self.run_main("dev", "--build", SHA)[0], self.run_main("rehearse-teardown")[0]), (0, 0))
        self.assertEqual([c[1][0] for c in self.pc.calls], ["dev", "rehearse-teardown"])

    def test_a_refused_dev_or_a_dry_run_opens_none(self) -> None:
        self.pc.replies[("dev", "--build", SHA)] = (1, json.dumps({"error": "precheck refused"}))
        code, docs, _ = self.run_main("dev", "--build", SHA)
        self.assertEqual((code, "dev_entry" in docs[-1], ip.current_entry()), (1, False, 0))
        code, docs, _ = self.run_main("dev", "--build", SHA, "--dry-run")
        self.assertEqual((code, "dev_entry" in docs[-1], ip.current_entry()), (0, False, 0))
        self.assertEqual(self.pc.calls[-1], ("iemmode.exe", ["dev", "--build", SHA, "--dry-run"], "abandon"))
        self.assertEqual(self.pc.timeouts[-1], ip.STATUS_S)

    def test_a_bad_build_is_refused_before_the_pc(self) -> None:
        for bad in ("1234", SHA.upper(), SHA + "0"):
            self.assertEqual(self.run_main("dev", "--build", bad)[0], 1, bad)
        self.assertEqual(self.pc.calls, [])

    def test_rehearse_teardown_and_probe_task_are_plain_guard_requests(self) -> None:
        self.assertEqual(self.run_main("rehearse-teardown")[0], 0)
        self.pc.replies[("probe-task",)] = (1, json.dumps({"error": "refused"}))
        self.assertEqual(self.run_main("probe-task")[0], 1)
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["rehearse-teardown"], "abandon"), ("iemmode.exe", ["probe-task"], "finish")])


class LockTests(Base):
    def test_a_second_changing_command_is_refused_while_one_runs_but_event_never_waits(self) -> None:
        with open(ip.state_dir() / "iempc.lock", "a+", encoding="utf-8") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            for argv in (["probe-task"], ["bootstrap", "Get-IemBootstrapState"]):
                code, _, err = self.run_main(*argv)
                self.assertEqual((code, self.pc.calls, self.pc.modules), (1, [], []), argv)
                self.assertIn("another iempc command runs", err, argv)
            self.assertEqual(self.run_main("status")[0], 0)
            self.assertEqual(self.run_main("event")[0], 0)
        ip.EVENT_NOW.unlink()  # "event skončil"
        self.assertEqual(self.run_main("probe-task")[0], 0)
        self.assertEqual([c[1] for c in self.pc.calls], [["status"], ["event"], ["probe-task"]])


class PatchTests(Base):
    """#36: iempc is split into modules, and a patch reaches the code only in
    the one module that holds the name (iempc_test_support.Base.patch)."""

    def test_a_patch_reaches_the_owner_and_reads_live_through_iempc(self) -> None:
        self.patch(now_iso=lambda: "2026-10-10T12:00:00+02:00")
        self.assertEqual(ip.SPLIT[0].now_iso(), "2026-10-10T12:00:00+02:00")
        self.assertEqual(ip.now_iso(), "2026-10-10T12:00:00+02:00")
        self.assertEqual(ip.next_entry(None), 1)
        self.assertEqual(ip.read_json(ip.state_dir() / "entry.json", {})["at"], "2026-10-10T12:00:00+02:00")

    def test_a_name_set_on_iempc_itself_fails_at_once(self) -> None:
        # Such a name would shadow the moved module's binding, which the code reads.
        with self.assertRaisesRegex(AssertionError, "Base.patch"):
            ip.EVENT_NOW = self.tmp / "elsewhere"
        with self.assertRaisesRegex(AssertionError, "Base.patch"):
            with mock.patch.object(ip, "ensure_flag", side_effect=AssertionError("never reached")):
                pass
        with self.assertRaisesRegex(AssertionError, "Base.patch"):
            del ip.event_now

    def test_a_name_held_by_two_modules_is_refused(self) -> None:
        module = ip.SPLIT[-1]
        module.now_iso = ip.now_iso   # a copy: a patch of the owner would never reach it
        self.addCleanup(delattr, module, "now_iso")
        with self.assertRaisesRegex(AssertionError, "now_iso is held by"):
            self.patch(now_iso=lambda: "x")


class GhTests(Base):
    """The real gh wrapper, with a stand-in `gh` program first on PATH: the P5
    gate rests on its exit-code check."""

    def test_a_successful_call_returns_its_output(self) -> None:
        self.gh_program("sys.stderr.write('unknown command in a warning'); sys.stdout.write('args: ' + ' '.join(sys.argv[1:]))")
        self.assertEqual(REAL_GH(["run", "list", "--limit", "1"]), "args: run list --limit 1")

    def test_a_failed_call_raises_with_its_exit_and_stderr(self) -> None:
        self.gh_program("sys.stderr.write('HTTP 404: Not Found\\n'); sys.exit(1)")
        with self.assertRaises(ip.StepError) as cm:
            REAL_GH(["workflow", "run", "hil.yml"])
        self.assertEqual(str(cm.exception), "gh workflow run failed (exit 1): HTTP 404: Not Found")

    def test_a_gh_without_attestation_gets_the_hint(self) -> None:
        self.gh_program("sys.stderr.write('unknown command \"attestation\" for \"gh\"'); sys.exit(1)")
        with self.assertRaises(ip.StepError) as cm:
            REAL_GH(["attestation", "verify", "x.zip"])
        self.assertEqual(str(cm.exception), "gh attestation verify failed (exit 1) (this gh has no 'attestation' command: "
                                            "install gh >= 2.49): unknown command \"attestation\" for \"gh\"")

    def test_a_missing_gh_is_named(self) -> None:
        empty = self.tmp / "empty-bin"
        empty.mkdir()
        self.set_path(str(empty))
        with self.assertRaisesRegex(ip.StepError, "^gh is not installed on this box$"):
            REAL_GH(["run", "list"])

    def test_a_gh_that_does_not_answer_is_bounded(self) -> None:
        self.gh_program("time.sleep(5)")
        t = time.monotonic()
        with self.assertRaisesRegex(ip.StepError, "^gh run download: no answer within 0.3 s$"):
            REAL_GH(["run", "download", "1"], timeout=0.3)
        self.assertLess(time.monotonic() - t, 3)

    def test_a_failed_attestation_through_the_real_gh_keeps_nothing(self) -> None:
        self.gh_program("sys.stderr.write('no attestation matched the bundle'); sys.exit(1)")
        self.route_to_real_gh("attestation", "verify")
        code, docs, err = self.run_main("fetch-bundle", "--sha", SHA)
        self.assertEqual((code, docs), (1, []))
        self.assertIn("gh attestation verify failed (exit 1): no attestation matched the bundle", err)
        self.assertIsNone(ip.load_record(SHA))
        self.assertEqual(list((ip.STATE_DIR / "bundles").iterdir()), [])
        self.assertEqual(len(self.gh.named("run", "download")), 1)


if __name__ == "__main__":
    unittest.main()
