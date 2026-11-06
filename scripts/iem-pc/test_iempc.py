"""Tests for scripts/iem-pc/iempc.py and iempc_core.py: the env, the waits, the
lock and gh. A fake gh stands in for GitHub; every value is synthetic, and the
private env file is never read (a temp file takes its place). The fakes and
`Base` live in iempc_test_support.py; the event path has a test file of its own
(#36)."""
from __future__ import annotations

import fcntl
import os
import re
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
from iempc_test_support import ENV, MODULES, REAL_GH, Base, ip  # noqa: E402


class EnvTests(Base):
    def write(self, text: str) -> Path:
        p = self.tmp / "other.env"
        p.write_text(text, encoding="utf-8")
        return p

    def test_an_env_file_loads(self) -> None:
        env = ip.load_env(self.write('# private\nGEN3_HOST="box.test"\nOTHER=x\n'))
        self.assertEqual(env, {"GEN3_HOST": "box.test", "OTHER": "x"})

    def test_a_bad_line_and_a_missing_file_are_named(self) -> None:
        with self.assertRaisesRegex(ip.StepError, "not KEY=VALUE"):
            ip.load_env(self.write("GEN3_HOST\n"))
        with self.assertRaisesRegex(ip.StepError, "missing"):
            ip.load_env(self.tmp / "absent.env")

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
        self.assertEqual(repos, {ip.REPO})


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


class LockTests(Base):
    def test_a_second_changing_command_is_refused_while_one_runs_but_event_never_waits(self) -> None:
        with open(ip.state_dir() / "iempc.lock", "a+", encoding="utf-8") as held:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            code, _, err = self.run_main("gen3-backup")
            self.assertEqual(code, 1)
            self.assertIn("another iempc command runs", err)
            self.assertEqual(self.run_main("event")[0], 0)


class PatchTests(Base):
    """#36: iempc is split into modules, and a patch reaches the code only in
    the one module that holds the name (iempc_test_support.Base.patch)."""

    def test_a_patch_reaches_the_owner_and_reads_live_through_iempc(self) -> None:
        self.patch(now_iso=lambda: "2026-10-10T12:00:00+02:00")
        self.assertEqual(ip.SPLIT[0].now_iso(), "2026-10-10T12:00:00+02:00")
        self.assertEqual(ip.now_iso(), "2026-10-10T12:00:00+02:00")

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


if __name__ == "__main__":
    unittest.main()
