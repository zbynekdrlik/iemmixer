"""Tests for scripts/check_integrity.py."""
from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_integrity as ci  # noqa: E402

PINNED = "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n"


class IntegrityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())
        self.put("crates/a/src/lib.rs", "#[test]\nfn ok() {}\n")
        self.put("e2e/tests/a.spec.ts", 'test("ok", async () => {});\n')
        self.put(".github/workflows/ci.yml", "jobs:\n  a:\n    steps:\n" + PINNED)

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def put(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def test_clean_tree(self) -> None:
        self.assertEqual(ci.violations(self.root), [])

    def test_ignored_rust_test(self) -> None:
        self.put("crates/a/src/lib.rs", "#[test]\n#[ignore]\nfn skipped() {}\n")
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_skipped_or_focused_e2e(self) -> None:
        for body in ('test.skip("x", async () => {});', 'test.only("x", async () => {});',
                     'test.describe.skip("x", () => {});', 'test.fixme("x", async () => {});'):
            self.put("e2e/tests/a.spec.ts", body + "\n")
            self.assertEqual(len(ci.violations(self.root)), 1, body)

    def test_forbidden_workflow_constructs(self) -> None:
        for line in ("    continue-on-error: true\n", "    runs-on: [self-hosted, x]\n", "on: pull_request_target\n"):
            self.put(".github/workflows/ci.yml", "jobs:\n" + line + PINNED)
            self.assertEqual(len(ci.violations(self.root)), 1, line)

    def test_unpinned_action(self) -> None:
        self.put(".github/workflows/ci.yml", "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v7\n")
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_force_kill_command(self) -> None:
        self.put("scripts/stop.ps1", "taskkill /F /IM engine.exe\n")
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_force_kill_in_a_powershell_module_is_found(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "scripts" / "golden").mkdir(parents=True)
            (root / "scripts" / "golden" / "M.psm1").write_text("function X { Stop-Process -Id 1 }\n", encoding="utf-8")
            self.assertEqual(ci.violations(root), ["scripts/golden/M.psm1:1: force-kill command (program spec I8)"])

    def test_force_kill_words_in_comments_are_refused(self) -> None:
        # The S6 crates, the PC scripts and the workflows: prose included.
        cases = {
            "crates/iem-guard/src/win/app.rs": "/// never TerminateProcess the app\nfn f() {}\n",
            "crates/iem-win/src/process.rs": "// no taskkill here\nfn f() {}\n",
            "scripts/iem-pc/IemPc.psm1": "# Stop-Process is never used\nfunction X { }\n",
            "scripts/iem-pc/hil-v1.ps1": "<# the job is never ended with TerminateJobObject #>\n",
            ".github/workflows/ci.yml": "jobs:\n  a:\n    steps:\n" + PINNED + "      # taskkill\n",
        }
        for rel, text in cases.items():
            with tempfile.TemporaryDirectory() as d:
                root = Path(d)
                (root / rel).parent.mkdir(parents=True)
                (root / rel).write_text(text, encoding="utf-8")
                line = 5 if rel.endswith(".yml") else 1
                self.assertEqual(ci.violations(root), [f"{rel}:{line}: force-kill command (program spec I8)"], rel)

    def test_force_end_verbs_of_rust_powershell_and_jobs_are_refused(self) -> None:
        for body in ("child.kill()", "child.kill ()", "child.start_kill()", "cmd.kill_on_drop(true)",
                     "TerminateJobObject(job, 1)", "JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE", "$p.Kill()",
                     "proc.terminate()", "Invoke-CimMethod -InputObject $p -MethodName Terminate",
                     "Invoke-CimMethod $p -MethodName 'Terminate'", "shutdown.exe /f /r",
                     "nt::NtTerminateProcess(h, 0)", "Invoke-CimMethod -InputObject $p -Name Terminate",
                     "Invoke-WmiMethod -Path $w -Name Terminate", "Invoke-WmiMethod -Name 'terminate' -Path $w",
                     "wmic process where processid=1 call terminate", "wmic process where name='x' delete",
                     "tskill 1234", "pskill -t engine", "Get-Process x | ForEach-Object Kill",
                     "Get-Process x | % Kill", "Get-Process x | ForEach-Object -MemberName Kill"):
            self.put("crates/a/src/lib.rs", f"fn f() {{ {body}; }}\n")
            self.assertEqual(ci.violations(self.root), ["crates/a/src/lib.rs:1: force-kill command (program spec I8)"], body)

    def test_a_forced_restart_is_refused(self) -> None:
        # Microsoft, shutdown /t: "If the timeout period is greater than 0, the /f
        # parameter is implied." So a delay forces too, and /f counts anywhere (#32 B1).
        for body in ("shutdown.exe /r /t 60 /c 'x'", "shutdown /r /t 5", "shutdown -r -t 30", "shutdown /r /t:10",
                     "shutdown /r /f", "shutdown.exe /s /t 0 /f", "shutdown -r -f -t 0", "Restart-Computer -Force",
                     "Stop-Computer -ComputerName x -Force", "restart-computer -force"):
            self.put("scripts/iem-pc/x.ps1", body + "\n")
            self.assertEqual(len(ci.violations(self.root)), 1, body)

    def test_a_graceful_restart_passes(self) -> None:
        for body in ("& shutdown.exe /r /t 0 /d p:2:4 /c 'iemmixer: planned restart'", "shutdown /r /t 00", "shutdown /a",
                     "Restart-Computer", "shutdown_signal()", "handle.graceful_shutdown(Some(STOP_DRAIN))",
                     "runtime.shutdown_timeout(Duration::from_secs(1))", "a shutdown of the app took 5 s"):
            self.put("scripts/iem-pc/x.ps1", body + "\n")
            self.assertEqual(ci.violations(self.root), [], body)

    def test_every_spelling_of_a_forced_restart_is_refused(self) -> None:
        # Review m6: a shutdown invocation passes only with an explicit /t 0 and no
        # force flag, in any form; -Force abbreviations; the API force flags.
        for body in ("shutdown /r",                                  # default /t 30 implies /f
                     "& 'shutdown.exe' /r /t 60", 'shutdown.exe "/r" "/t" "60"',
                     'let argv = ["shutdown", "/r", "/f"];', 'Command::new("shutdown").args(["/r", "/f"]).status()',
                     "Start-Process shutdown.exe -ArgumentList '/r','/f'", "Start-Process -FilePath shutdown -ArgumentList '/r /t 60'",
                     "Restart-Computer -f", "Restart-Computer -Forc", "Stop-Computer -Force:$true",
                     "(Get-CimInstance Win32_OperatingSystem).Win32Shutdown(6)", "$os.Win32Shutdown(4)",
                     "Invoke-CimMethod -ClassName Win32_OperatingSystem -MethodName Win32Shutdown -Arguments @{ Flags = 5 }",
                     "$os.Win32ShutdownTracker(0, 'x', 0, 6)",
                     "ExitWindowsEx(EWX_REBOOT | EWX_FORCE, 0)", "ExitWindowsEx(EWX_REBOOT | EWX_FORCEIFHUNG, 0)",
                     "ExitWindowsEx(0x6, 0)", "InitiateShutdownW(null, null, 0, SHUTDOWN_RESTART | SHUTDOWN_FORCE_OTHERS, 0)"):
            self.put("scripts/iem-pc/x.ps1", body + "\n")
            self.assertEqual(len(ci.violations(self.root)), 1, body)

    def test_graceful_forms_and_other_commands_on_the_line_pass(self) -> None:
        # Review m7: only the command's own arguments count, never the next command's.
        for body in ("Start-Process shutdown.exe -ArgumentList '/r','/t','0'", 'Command::new("shutdown").args(["/r", "/t", "0"])',
                     "Restart-Computer -Wait -For PowerShell", "(Get-CimInstance Win32_OperatingSystem).Win32Shutdown(2)",
                     "ExitWindowsEx(EWX_REBOOT, SHTDN_REASON_FLAG_PLANNED)", "ExitWindowsEx(0x2, 0)",
                     'graceful shutdown ; [ -f "$pid" ]', "shutdown requested; ssh -t 5 host",
                     "Stop-Computer -ComputerName x ; Remove-Item x -Force", "Cmd::Shutdown => \"shutdown\",",
                     'reason: "shutdown".into(),', "self.shutdown(timeout=5)", "the shutdown message was sent"):
            self.put("scripts/iem-pc/x.ps1", body + "\n")
            self.assertEqual(ci.violations(self.root), [], body)

    def test_graceful_stops_and_ordinary_words_pass(self) -> None:
        for body in ('Command::new("kill").args(["-TERM", &pid])', "signal::kill(pid, Signal::SIGTERM)",
                     "self.killed = true", "skill(x)", "let force_ended = false",
                     "console::ctrl_break(pid)", "tasklist /m testcard.dll", "Invoke-CimMethod -MethodName Create",
                     "// the guard never force-ends a process", "a skill and a skills list", "the job Terminated",
                     "Invoke-CimMethod -Name GetOwner", "wmic os get caption", "delete the temp file",
                     "ForEach-Object Killed", "% Killer", "$exit = 'terminated'"):
            self.put("crates/a/src/lib.rs", f"fn f() {{ {body}; }}\n")
            self.assertEqual(ci.violations(self.root), [], body)

    def test_job_breakaway_only_in_iem_win(self) -> None:
        self.put("crates/iem-win/src/spawn.rs", "pub const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;\n")
        self.put("crates/iem-win/tests/ctrl_break.rs", "let f = flags & !CREATE_BREAKAWAY_FROM_JOB;\n")
        self.assertEqual(ci.violations(self.root), [])
        self.put("crates/iem-guard/src/win/procs.rs", "let f = CREATE_BREAKAWAY_FROM_JOB;\n")
        self.put("scripts/iem-pc/x.ps1", "$CREATE_BREAKAWAY_FROM_JOB = 0x01000000\n")
        self.put("crates/iem-winx/src/lib.rs", "// CREATE_BREAKAWAY_FROM_JOB\n")
        self.assertEqual(ci.violations(self.root), [
            "crates/iem-guard/src/win/procs.rs:1: job breakaway outside iem-win (S6 design note §5.1)",
            "crates/iem-winx/src/lib.rs:1: job breakaway outside iem-win (S6 design note §5.1)",
            "scripts/iem-pc/x.ps1:1: job breakaway outside iem-win (S6 design note §5.1)",
        ])

    def test_a_pin_needs_its_version_comment(self) -> None:
        sha = "3d3c42e5aac5ba805825da76410c181273ba90b1"
        for line, ok in ((f"      - uses: actions/checkout@{sha} # v7.0.1\n", True),
                         (f"      - uses: actions/checkout@{sha}  #v7\n", True),
                         (f"      - uses: actions/checkout@{sha}\n", False),
                         (f"      - uses: actions/checkout@{sha} # latest\n", False),
                         (f"        uses: actions/checkout@{sha} # v7.0.1 pinned\n", False),
                         ("      - uses: ./.github/actions/local\n", True)):
            self.put(".github/workflows/ci.yml", "jobs:\n  a:\n    steps:\n" + line)
            want = [] if ok else [f".github/workflows/ci.yml:4: pinned action without its version comment (# vX.Y.Z): actions/checkout@{sha}"]
            self.assertEqual(ci.violations(self.root), want, line)

    def test_the_s6_crates_and_scripts_are_scanned(self) -> None:
        self.put("crates/iem-guard/src/daemon.rs", "#[test]\n#" + "[ignore]\nfn skipped() {}\n")
        self.put("scripts/iem-pc/stop.cmd", "taskkill /im iem-engine.exe\n")
        self.assertEqual(ci.violations(self.root), [
            "crates/iem-guard/src/daemon.rs:2: #" + "[ignore] test",
            "scripts/iem-pc/stop.cmd:1: force-kill command (program spec I8)",
        ])

    def test_goldens_over_twenty_megabytes_fail(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "goldens" / "s1b").mkdir(parents=True)
            (root / "goldens" / "s1b" / "x.f64").write_bytes(b"\0" * (20 * 1024 * 1024 + 1))
            self.assertIn("goldens/: 20971521 bytes, over the 20 MB budget (spec §3.5)", ci.violations(root))

    def test_asio_setting_calls_are_refused(self) -> None:
        for call in ("driver.set_sample_rate(48000.0)", "d.set_clock_source(1)", "self.driver.open_control_panel()",
                     "Driver::set_sample_rate(&d, 48000.0)", "azo::Driver::set_clock_source(&d, 1)",
                     "d.as_raw().control_panel()", "d.as_raw().set_sample_rate(48000.0)",
                     "d.future::<SetIoFormat>(&mut p)", "d.as_raw().future(sel, opt)"):
            self.put("crates/a/src/lib.rs", f"fn f() {{ {call}; }}\n")
            self.assertEqual(len(ci.violations(self.root)), 1, call)
        self.put("crates/a/src/lib.rs", "// the host never calls set_sample_rate\nfn f() {}\n")
        self.assertEqual(ci.violations(self.root), [])
        self.put("crates/a/src/lib.rs", "fn f() { let futures = 1; let _ = futures; driver.sample_position(); }\n")
        self.assertEqual(ci.violations(self.root), [])

class BundleSyncTests(unittest.TestCase):
    """spike_window.BUNDLE_FILES equals the asio-spike Bundle step's Copy-Item
    list (#32 E5): a drift makes fetch-bundle reject every artifact on the dev
    box while CI stays green."""

    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp())

    def tearDown(self) -> None:
        shutil.rmtree(self.root)

    def put(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def bundle(self, files: str, copy: str) -> None:
        self.put("scripts/asio-spike/spike_window.py", f"import os\nBUNDLE_FILES = ({files})\n")
        self.put(".github/workflows/ci.yml",
                 "jobs:\n  asio-spike:\n    steps:\n" + PINNED
                 + "      - name: Bundle (spike, PC scripts, SHA256SUMS)\n        run: |\n"
                 + f"          Copy-Item -LiteralPath {copy} -Destination $b\n"
                 + "  bundle:\n    steps:\n      - name: Bundle\n        run: Copy-Item -LiteralPath other.exe -Destination $b\n")

    def test_a_matching_bundle_passes(self) -> None:
        self.bundle('"A.psm1", "b.exe"', "target/release/examples/b.exe, scripts/x/A.psm1")
        self.assertEqual(ci.violations(self.root), [])

    def test_a_drift_either_way_is_refused(self) -> None:
        for files, missing in (('"A.psm1", "b.exe", "C.psm1"', "C.psm1"), ('"A.psm1"', "b.exe")):
            self.bundle(files, "target/release/examples/b.exe, scripts/x/A.psm1")
            found = ci.violations(self.root)
            self.assertEqual(len(found), 1, files)
            self.assertTrue(found[0].startswith(".github/workflows/ci.yml:7: "), found[0])   # the Copy-Item line
            self.assertIn("BUNDLE_FILES", found[0])
            self.assertIn(missing, found[0])

    def test_a_missing_bundle_step_is_refused(self) -> None:
        self.put("scripts/asio-spike/spike_window.py", 'BUNDLE_FILES = ("A.psm1",)\n')
        self.put(".github/workflows/ci.yml", "jobs:\n  a:\n    steps:\n" + PINNED)
        self.assertEqual(len(ci.violations(self.root)), 1)

    def test_the_repository_is_in_sync(self) -> None:
        self.assertEqual(ci.bundle_violations(ci.ROOT), [])

    def test_the_real_repository_needs_its_spike_window(self) -> None:
        # Review m8: a missing spike_window.py must not silence the check on the real tree.
        self.assertEqual(ci.bundle_violations(self.root), [])            # a fixture tree without it
        self.assertEqual(len(ci.bundle_violations(self.root, required=True)), 1)
        with mock.patch.object(ci, "ROOT", self.root):                  # main() scans the real root: required
            self.assertEqual(ci.main(), 1)


if __name__ == "__main__":
    unittest.main()
