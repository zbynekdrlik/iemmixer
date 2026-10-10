"""Tests for scripts/iem-pc/iempc_sshshell.py: `iempc ssh-shell` (#15, the last
item of the elevated chain): Set-IemSshShell from the admin-only stage, a probe
of a fresh ssh session, then Confirm-IemSshShell; a failed probe never
confirms. They reuse iempc_test_support's fakes (FakePc stands in for ssh and scp,
FakeGh for GitHub); every value is synthetic."""
from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import iempc_sshshell as ss  # noqa: E402
from iempc_test_support import SHA, Base, ip, make_zip, sha256  # noqa: E402

HERE = Path(__file__).resolve().parent
MODULES = {"IemSshShell.psm1": b"synthetic IemSshShell.psm1", "tuning/IemTuningStore.psm1": b"synthetic IemTuningStore.psm1"}
CMD = "C:\\WINDOWS\\system32\\cmd.exe"
AT = "2026-10-08T12:10:00"
UPLOADED = "X:\\root\\bootstrap\\" + SHA + "\\"
# The probe's mark in a composed script (its body reads the session's parent process).
PROBE_MARK = "ParentProcessId"


def line(option: str, shell: str = CMD) -> str:
    """The parent process's command line as sshd builds it for a cmd shell:
    `"<shell>" <option> "<command>"`, the command iempc's own (ssh_cmd)."""
    return f'"{shell}" {option} "{ip.elevated_ps().REMOTE}"'


def ours() -> dict:
    return {"DefaultShell": {"kind": "String", "data": CMD},
            "DefaultShellCommandOption": {"kind": "String", "data": "/d /c"},
            "DefaultShellArguments": {"kind": "String", "data": "/d"}}


class ShellBase(Base):
    def setUp(self) -> None:
        super().setUp()
        self.gh.artifact = make_zip(self.tmp / "artifact-ssh" / f"iemmixer-{SHA}.zip", extra=MODULES)
        self.set_reply: object = {"state": "set", "key": ss.KEY, "values": ours(),
                                  "undo": {"task": ss.UNDO_TASK, "at": AT}}
        self.probe_reply: object = {"exe": CMD, "line": line("/d /c")}
        self.confirm_reply: object = {"state": "confirmed"}
        self.pc.texts[ss.SET] = lambda: self.answer(self.set_reply)
        self.pc.texts[ss.CONFIRM] = lambda: self.answer(self.confirm_reply)
        self.pc.texts[PROBE_MARK] = lambda: self.answer(self.probe_reply)
        self.failing: dict[str, str] = {}   # a script mark -> the ssh failure its call raises
        fake = self.pc.ssh_ps

        def ssh_ps(env, script, timeout, event):
            for mark, error in self.failing.items():
                if mark in script:
                    self.pc.modules.append((script, event))
                    raise ip.StepError(error)
            return fake(env, script, timeout, event)

        self.patch(ssh_ps=ssh_ps)

    @staticmethod
    def answer(r):
        """A scripted reply, or a callable's (one that does something first, e.g. writes the flag)."""
        return r() if callable(r) else r

    def scripts(self, mark: str) -> list[tuple[str, str]]:
        return [(s, e) for s, e in self.pc.modules if mark in s]


class SequenceTests(ShellBase):
    """Set-IemSshShell, a fresh session's probe, then Confirm-IemSshShell."""

    def test_the_command_is_dev_time_locked_and_talks_to_the_pc(self) -> None:
        spec = ip.COMMANDS["ssh-shell"]
        self.assertEqual((spec.pc, spec.dev_time, spec.locked), (True, True, True))

    def test_set_then_a_fresh_probe_then_confirm(self) -> None:
        self.fetched()
        code, docs, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 0, err)
        marks = [next((m for m in (ss.SET, PROBE_MARK, ss.CONFIRM) if m in s), "other") for s, _ in self.pc.modules]
        self.assertEqual([m for m in marks if m != "other"], [ss.SET, PROBE_MARK, ss.CONFIRM])
        # A changing step lets a new flag finish it; the read-only probe is abandoned at once.
        self.assertEqual([e for _, e in self.scripts(ss.SET)] + [e for _, e in self.scripts(PROBE_MARK)]
                         + [e for _, e in self.scripts(ss.CONFIRM)], ["finish", "abandon", "finish"])
        self.assertEqual(docs[-1]["ssh_shell"], "confirmed")
        self.assertEqual((docs[-1]["set"], docs[-1]["confirm"], docs[-1]["shell"]), ("set", "confirmed", CMD))

    def test_the_modules_come_from_the_bundle_and_only_their_stage_copies_load(self) -> None:
        """IemSshShell.psm1 imports IemPc.psm1 and IemTuningStore.psm1 (its
        exact registry save and restore) from its own folder: both are staged
        first, each checked by the zip's sha256, and only the stage copy of
        the new module is imported (elevated_ps.staged)."""
        self.fetched()
        self.assertEqual(self.run_main("ssh-shell", "--sha", SHA)[0], 0)
        self.assertEqual(self.pc.scps, [
            (str(ip.bundle_dir(SHA) / "IemPc.psm1"), f"tester@pc.test:/X:/root/bootstrap/{SHA}/IemPc.psm1", "finish"),
            (str(ip.bundle_dir(SHA) / "tuning" / "IemTuningStore.psm1"),
             f"tester@pc.test:/X:/root/bootstrap/{SHA}/IemTuningStore.psm1", "finish"),
            (str(ip.bundle_dir(SHA) / "IemSshShell.psm1"), f"tester@pc.test:/X:/root/bootstrap/{SHA}/IemSshShell.psm1",
             "finish")])
        for mark in (ss.SET, ss.CONFIRM):
            script = self.scripts(mark)[0][0]
            pc = script.index(f"$iemB = [IO.File]::ReadAllBytes('{UPLOADED}IemPc.psm1')")
            store = script.index(f"$iemB = [IO.File]::ReadAllBytes('{UPLOADED}IemTuningStore.psm1')")
            new = script.index(f"$iemB = [IO.File]::ReadAllBytes('{UPLOADED}IemSshShell.psm1')")
            self.assertLess(pc, new, mark)
            self.assertLess(store, new, mark)
            self.assertIn(f"$iemH -cne '{sha256(b'synthetic IemPc.psm1')}'", script)
            self.assertIn(f"$iemH -cne '{sha256(MODULES['tuning/IemTuningStore.psm1'])}'", script)
            self.assertIn(f"$iemH -cne '{sha256(MODULES['IemSshShell.psm1'])}'", script)
            self.assertIn(f"Import-Module $iemMod -Force ; $r = & {{ {mark}", script[new:])
            self.assertNotIn(f"Import-Module '{UPLOADED}", script)   # never from the run folder (#15)
            self.assertEqual(script.count("Import-Module"), 1, mark)
        # Set copies the modules for its undo task only as the bytes this box checked (the stage is shared).
        sums = {"IemPc.psm1": sha256(b"synthetic IemPc.psm1"),
                "IemTuningStore.psm1": sha256(MODULES["tuning/IemTuningStore.psm1"]),
                "IemSshShell.psm1": sha256(MODULES["IemSshShell.psm1"])}
        self.assertIn("$r = & { Set-IemSshShell -ModuleSha256 @{ " + "; ".join(f"'{n}' = '{h}'" for n, h in sums.items())
                      + " } }", self.scripts(ss.SET)[0][0])
        self.assertIn("$r = & { Confirm-IemSshShell }", self.scripts(ss.CONFIRM)[0][0])
        probe = self.scripts(PROBE_MARK)[0][0]
        self.assertNotIn("Import-Module", probe)   # the probe runs as any session does: no module, no stage
        self.assertIn(ip.elevated_ps().PIN, probe)

    def test_values_already_ours_are_probed_and_nothing_is_confirmed(self) -> None:
        self.fetched()
        self.set_reply = {"state": "unchanged", "key": ss.KEY, "values": ours(), "undo": None}
        code, docs, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual(len(self.scripts(PROBE_MARK)), 1)
        self.assertEqual(self.scripts(ss.CONFIRM), [])
        self.assertEqual((docs[-1]["ssh_shell"], docs[-1]["confirm"]), ("unchanged", None))

    def test_a_rearmed_undo_is_probed_and_confirmed(self) -> None:
        self.fetched()
        self.set_reply = {"state": "rearmed", "key": ss.KEY, "values": ours(), "undo": {"task": ss.UNDO_TASK, "at": AT}}
        code, docs, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 0, err)
        self.assertEqual((docs[-1]["set"], docs[-1]["ssh_shell"]), ("rearmed", "confirmed"))


class FailureTests(ShellBase):
    """Nothing confirms a shell a fresh session did not prove; the undo task's
    restore is named, and no further ssh call is tried."""

    def assert_unconfirmed(self, err: str) -> None:
        self.assertEqual(self.scripts(ss.CONFIRM), [])
        self.assertLessEqual(len(self.scripts(PROBE_MARK)), 1)   # never a loop of probes
        self.assertIn(f"{ss.UNDO_TASK} restores the prior OpenSSH default shell at {AT}", err)

    def test_a_shell_without_d_never_confirms(self) -> None:
        self.fetched()
        self.probe_reply = {"exe": CMD, "line": line("/c")}
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("did not run with /d", err)
        self.assert_unconfirmed(err)

    def test_a_failed_probe_call_never_confirms_and_is_not_retried(self) -> None:
        self.fetched()
        self.failing[PROBE_MARK] = "ssh failed (exit 255): Connection closed by remote host"
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("Connection closed", err)
        self.assertEqual(len(self.scripts(PROBE_MARK)), 1)
        self.assert_unconfirmed(err)

    def test_a_probe_answer_that_is_not_a_shell_never_confirms(self) -> None:
        self.fetched()
        self.probe_reply = "ok"
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assert_unconfirmed(err)

    def test_a_failed_confirm_names_the_undo(self) -> None:
        self.fetched()
        self.failing[ss.CONFIRM] = "PC step failed: the values are not ours"
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("the values are not ours", err)
        self.assertIn(f"{ss.UNDO_TASK} restores the prior OpenSSH default shell at {AT}", err)

    def test_after_an_armed_set_confirm_must_have_confirmed(self) -> None:
        """`unchanged` from Confirm after Set armed the undo means it found
        neither the saved values nor the task: what became of the undo is
        unknown, so it is no success."""
        self.fetched()
        self.confirm_reply = {"state": "unchanged", "removed": []}
        code, docs, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertNotIn("ssh_shell", "".join(str(d) for d in docs))
        self.assertIn(f"restores the prior OpenSSH default shell at {AT}", err)

    def test_a_probe_of_another_shell_than_set_wrote_never_confirms(self) -> None:
        self.fetched()
        other = "D:\\Windows\\system32\\cmd.exe"
        self.probe_reply = {"exe": other, "line": line("/d /c", other)}
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assert_unconfirmed(err)

    def test_a_confirm_answer_that_is_not_confirmed_is_an_error(self) -> None:
        self.fetched()
        self.confirm_reply = {"state": "kept"}
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn(f"restores the prior OpenSSH default shell at {AT}", err)

    def test_a_failed_probe_with_nothing_armed_says_so(self) -> None:
        self.fetched()
        self.set_reply = {"state": "unchanged", "key": ss.KEY, "values": ours(), "undo": None}
        self.probe_reply = {"exe": CMD, "line": line("/c")}
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("no undo task is armed", err)
        self.assertNotIn("restores the prior", err)
        self.assertEqual(self.scripts(ss.CONFIRM), [])

    def test_a_failed_set_is_never_probed_and_names_the_undo_task(self) -> None:
        self.fetched()
        self.failing[ss.SET] = "PC step failed: HKLM:\\SOFTWARE\\OpenSSH may be changed by S-1-5-32-545"
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("Set-IemSshShell failed", err)
        self.assertIn(f"if it armed {ss.UNDO_TASK}", err)
        self.assertEqual((self.scripts(PROBE_MARK), self.scripts(ss.CONFIRM)), ([], []))

    def test_a_set_answer_without_a_state_or_undo_time_is_refused(self) -> None:
        self.fetched()
        no_values = {"state": "set", "key": ss.KEY, "undo": {"task": ss.UNDO_TASK, "at": AT}}
        no_shell = {"state": "unchanged", "key": ss.KEY, "undo": None, "values": {"DefaultShell": {"kind": "absent"}}}
        for bad in ({"state": "maybe"}, {"state": "set", "undo": None}, {"state": "set", "undo": {"at": ""}}, "ok",
                    no_values, no_shell):
            self.pc.modules.clear()
            self.set_reply = bad
            code, _, err = self.run_main("ssh-shell", "--sha", SHA)
            self.assertEqual(code, 1, bad)
            self.assertIn("Set-IemSshShell", err)
            self.assertEqual(self.scripts(PROBE_MARK), [], bad)

    def test_a_new_flag_during_set_lets_it_finish_then_runs_the_event_path_unprobed(self) -> None:
        self.fetched()

        def set_then_flag():
            self.flag()
            return {"state": "set", "key": ss.KEY, "values": ours(), "undo": {"task": ss.UNDO_TASK, "at": AT}}

        self.set_reply = set_then_flag
        code, docs, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([e for _, e in self.scripts(ss.SET)], ["finish"])
        self.assertEqual((self.scripts(PROBE_MARK), self.scripts(ss.CONFIRM)), ([], []))
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertIn(f"if it armed {ss.UNDO_TASK}", err)

    def test_a_new_flag_during_confirm_lets_it_finish_then_runs_the_event_path(self) -> None:
        self.fetched()

        def confirm_then_flag():
            self.flag()
            return {"state": "confirmed"}

        self.confirm_reply = confirm_then_flag
        code, docs, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual([e for _, e in self.scripts(ss.CONFIRM)], ["finish"])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        # Confirm may have finished on the PC: never called unconfirmed outright.
        self.assertIn("may have finished", err)
        self.assertIn(f"restores the prior OpenSSH default shell at {AT}", err)

    def test_a_new_flag_during_the_probe_runs_the_event_path_unconfirmed(self) -> None:
        self.fetched()

        def flag_then_answer():
            self.flag()
            return {"exe": CMD, "line": line("/d /c")}

        self.probe_reply = flag_then_answer
        code, docs, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, ip.PREEMPTED)
        self.assertEqual(self.scripts(ss.CONFIRM), [])
        self.assertEqual(self.pc.calls, [("iemmode.exe", ["event", "--signal"], "ignore")])
        self.assertEqual(docs[0], {"event": "ide event (flag file)", "action": "iempc event"})
        self.assertIn(f"restores the prior OpenSSH default shell at {AT}", err)


class RefusalTests(ShellBase):
    def test_the_event_flag_refuses_it(self) -> None:
        self.fetched()
        self.flag()
        for argv in (("ssh-shell", "--sha", SHA), ("ssh-shell", "--sha", SHA, "--dry-run")):
            code, docs, err = self.run_main(*argv)
            self.assertEqual((code, docs), (1, []), argv)
            self.assertIn("runs only in dev time", err)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))

    def test_dry_run_prints_the_plan_and_touches_nothing(self) -> None:
        self.fetched()
        code, docs, err = self.run_main("ssh-shell", "--sha", SHA, "--dry-run")
        self.assertEqual(code, 0, err)
        self.assertEqual((self.pc.modules, self.pc.scps, self.pc.calls), ([], [], []))
        doc = docs[-1]
        self.assertEqual((doc["ssh_shell"], doc["sha"], doc["key"], doc["undo_task"], doc["undo_after_min"]),
                         ("dry-run", SHA, "HKLM:\\SOFTWARE\\OpenSSH", "\\iemmixer\\iemmixer-ssh-shell-undo", 10))
        self.assertEqual((doc["values"]["DefaultShellCommandOption"], doc["values"]["DefaultShellArguments"]), ("/d /c", "/d"))
        self.assertEqual(doc["modules"], ["IemPc.psm1", "tuning/IemTuningStore.psm1", "IemSshShell.psm1"])

    def test_a_bundle_without_the_module_is_refused_before_the_pc(self) -> None:
        self.gh.artifact = self.artifact   # iempc_test_support's zip: no IemSshShell.psm1, no tuning store
        self.fetched()
        for argv in (("ssh-shell", "--sha", SHA), ("ssh-shell", "--sha", SHA, "--dry-run")):
            code, _, err = self.run_main(*argv)
            self.assertEqual(code, 1, argv)
            self.assertIn("has no", err)
            self.assertIn("IemSshShell.psm1", err)
        self.assertEqual((self.pc.modules, self.pc.scps), ([], []))

    def test_a_bundle_without_the_tuning_store_is_refused_before_the_pc(self) -> None:
        self.gh.artifact = make_zip(self.tmp / "artifact-nostore" / f"iemmixer-{SHA}.zip",
                                    extra={"IemSshShell.psm1": MODULES["IemSshShell.psm1"]})
        self.fetched()
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual(code, 1)
        self.assertIn("has no tuning/IemTuningStore.psm1", err)
        self.assertEqual((self.pc.modules, self.pc.scps), ([], []))

    def test_an_unfetched_or_malformed_sha_is_refused(self) -> None:
        code, _, err = self.run_main("ssh-shell", "--sha", SHA)
        self.assertEqual((code, self.pc.modules), (1, []))
        self.assertIn("is not fetched", err)
        code, _, err = self.run_main("ssh-shell", "--sha", SHA.upper())
        self.assertEqual(code, 1)
        self.assertIn("not a full commit SHA", err)


class ProbeParseTests(unittest.TestCase):
    """parse_probe: the fresh session's parent process is System32's cmd.exe,
    started by sshd as `"<shell>" /d /c "<command>"`."""

    def test_a_cmd_shell_with_d_before_the_command_passes(self) -> None:
        got = ss.parse_probe(ip, {"exe": CMD, "line": line("/d /c")})
        self.assertEqual(got, {"shell": CMD, "line": line("/d /c")})
        # The path's case as the line spells it may differ from the process's.
        other = "C:\\Windows\\System32\\cmd.exe"
        self.assertEqual(ss.parse_probe(ip, {"exe": CMD, "line": line("/d /c", other)})["shell"], other)

    def test_anything_else_is_refused(self) -> None:
        refused = [
            {"exe": CMD, "line": line("/c")},                                    # sshd's own fallback: AutoRun runs
            {"exe": CMD, "line": f'"{CMD}" /c "x /d /c y"'},                     # /d only inside the command
            {"exe": CMD, "line": f'"{CMD}" /c /d /c "x"'},                       # anything between the shell and /d
            {"exe": CMD, "line": f'{CMD} /d /c "x"'},                            # not the quoted shell sshd writes
            {"exe": CMD, "line": f'"{CMD}" /d /c x'},                            # nor its quoted command
            {"exe": CMD, "line": ""},
            {"exe": "C:\\Tools\\cmd.exe", "line": line("/d /c", "C:\\Tools\\cmd.exe")},   # not System32's
            {"exe": "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
             "line": line("/d /c", "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")},
            {"exe": "C:\\Other\\system32\\cmd.exe", "line": line("/d /c")},       # the line names another program
            {"exe": CMD}, {"line": line("/d /c")}, {"exe": None, "line": line("/d /c")}, "ok", None, [CMD],
        ]
        for r in refused:
            with self.assertRaises(ip.StepError, msg=repr(r)):
                ss.parse_probe(ip, r)

    def test_the_shell_must_be_the_one_set_wrote(self) -> None:
        r = {"exe": CMD, "line": line("/d /c")}
        self.assertEqual(ss.parse_probe(ip, r, CMD.lower())["shell"], CMD)   # Windows paths: any case
        with self.assertRaisesRegex(ip.StepError, "not the DefaultShell"):
            ss.parse_probe(ip, r, "D:\\Windows\\system32\\cmd.exe")

    def test_the_refusal_names_what_was_read(self) -> None:
        with self.assertRaisesRegex(ip.StepError, re.escape(f'"{CMD}" /c')):
            ss.parse_probe(ip, {"exe": CMD, "line": line("/c")})


class AgreementTests(unittest.TestCase):
    """The dev box and the PC module name the same values, task and key."""

    def test_the_module_writes_the_values_the_dev_box_probes_for(self) -> None:
        text = (HERE / "IemSshShell.psm1").read_text(encoding="ascii")
        self.assertIn(f"$script:CommandOption = '{ss.OPTION}'", text)
        self.assertIn(f"$script:ShellArguments = '{ss.ARGUMENTS}'", text)
        self.assertIn(f"$script:DefaultKey = '{ss.KEY}'", text)
        folder, name = ss.UNDO_TASK.rsplit("\\", 1)
        self.assertIn(f"$script:DefaultTaskFolder = '{folder}'", text)
        self.assertIn(f"$script:DefaultTaskName = '{name}'", text)
        self.assertIn(f"$script:UndoMinutes = {ss.UNDO_MIN}", text)
        self.assertEqual((ss.OPTION, ss.ARGUMENTS), ("/d /c", "/d"))
        # It loads IemPc.psm1 and IemTuningStore.psm1 from its own folder, so the stage gets both first.
        self.assertIn("Import-Module (Join-Path $PSScriptRoot 'IemPc.psm1')", text)
        self.assertIn("Import-Module (Join-Path $PSScriptRoot 'IemTuningStore.psm1')", text)
        self.assertEqual([name for _, name in ss.STAGE], ["IemPc.psm1", "IemTuningStore.psm1", "IemSshShell.psm1"])
        self.assertEqual([member for member, _ in ss.STAGE], ["IemPc.psm1", "tuning/IemTuningStore.psm1", "IemSshShell.psm1"])

    def test_a_key_s_rules_never_go_through_the_acl_cmdlets(self) -> None:
        """Windows PowerShell 5.1's Get-Acl and Set-Acl hand a -LiteralPath on as
        the provider's own path, which for a registry key ('HKEY_LOCAL_MACHINE\\...')
        they then cannot find (PowerShell #13107; CI run 37741639195: "Cannot
        find path ... because it does not exist" for a key that exists). The
        module and its self-test read a key's rules through the registry API;
        their Get-/Set-Acl calls are on files only."""
        for name in ("IemSshShell.psm1", "Test-IemSshShell.ps1"):
            for line in (HERE / name).read_text(encoding="ascii").splitlines():
                code = line.split("#", 1)[0]
                if re.search(r"\b(?:Get|Set)-Acl\b", code, re.IGNORECASE):
                    self.assertNotRegex(code, re.compile(r"\$key\b|HKLM:|HKEY_", re.IGNORECASE), f"{name}: {line.strip()}")

    def test_the_bundle_job_ships_the_module(self) -> None:
        ci = (HERE.parent.parent / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        job = ci[ci.index("\n  bundle:\n"):ci.index("\n  attest:\n")]
        self.assertRegex(job, r"Copy-Item -LiteralPath [^\n]*scripts/iem-pc/IemPc\.psm1[^\n]*scripts/iem-pc/IemSshShell\.psm1")

    def test_the_windows_job_runs_the_module_self_test(self) -> None:
        ci = (HERE.parent.parent / ".github" / "workflows" / "ci.yml").read_text(encoding="utf-8")
        job = ci[ci.index("\n  windows:\n"):ci.index("\n  bundle:\n")]
        self.assertIn("-File scripts/iem-pc/Test-IemSshShell.ps1", job)

    def test_no_site_values_in_the_module(self) -> None:
        text = Path(ss.__file__).read_text(encoding="utf-8")
        self.assertIsNone(re.search(r"\b\d{1,3}(?:\.\d{1,3}){3}\b", text), "an IPv4 address")
        self.assertIsNone(re.search(r"\b[A-Za-z]:\\", text), "a Windows drive path")
        self.assertIsNone(re.search(r"[\w.+-]+@[\w-]+(?:\.[\w-]+)+", text), "an ssh destination or email")


if __name__ == "__main__":
    unittest.main()
