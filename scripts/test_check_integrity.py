"""Tests for scripts/check_integrity.py."""
from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

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

if __name__ == "__main__":
    unittest.main()
